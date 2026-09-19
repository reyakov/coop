# Using the Concord backend

`crates/concord` is a protocol crate: derivations, envelopes, folds and the local
state document. It has no GPUI dependency and owns no strings a user reads — the
UI layer decides every rendering. This document is the map from a UI action to
the calls it makes.

A community is addressed by a `community_id` (never on the wire) plus three
secrets: `community_root` (read access — holding it *is* membership),
`control_root` (write access to the Control Plane, held by staff), and per-Channel
keys for private channels. Authority is a roster of owner-rooted signed grants,
folded independently by every client.

## Modules

Files follow the CORD documents. The frozen derivations of Appendix A and the id
vocabulary are shared substrate — every document calls them — so they live outside
`cords` in `utils::derive` (re-exported as `derive`) and `types`.

| Module | Owns |
| --- | --- |
| `cord01` | Private Streams: the seal/wrap envelope and the NIP-44 helpers |
| `cord02` | Communities: identity, epochs, metadata, the Control Plane fold and writer |
| `cord02::guestbook` | Joins, leaves, kicks, snapshots, the member list |
| `cord02::list` | The Community List (a member's own memberships, across devices) |
| `cord03` | Channels: Channel metadata and the Chat Plane |
| `cord04` | Roles: chained editions, parse/hash/fold, the roster, permissions, the banlist |
| `cord04::pins` | Pin Lists, and the key disclosure a keyless reader verifies |
| `cord05` | Invite bundles, links, the Direct Invite, the Invite List |
| `cord06` | Key rotations, refounding, compaction, dissolution |
| `derive` | Every frozen HKDF derivation and coordinate |
| `store` | Local rumor cache, the community state document, relay paging |

`CommunityId`, `ChannelId`, `RoleId`, `Epoch` and `Extra` (crate-internal) come from
the private `types` module and are re-exported at the crate root.

CORD-07 (audio/video) is unimplemented. CORD-08's timer has no file of its own: it
lives in the metadata it reads (`cord02`) and the fold it filters (`cord03`).

Read `CommunityId` as "this community", `ChannelId` as "this channel", `Epoch` as
"which key generation". Nothing else in the API needs internal state.

## Creating a community

```rust
use concord::cord02::{self, CommunityMetadata};
use concord::store::{self, CommunityState, save_state};

let metadata = CommunityMetadata { name: "Room".into(), ..Default::default() };
let minted = cord02::genesis(&owner_keys, &metadata, now_secs).await?;

// minted.identity    — community_id, owner, owner_salt (verify() recomputes it)
// minted.wraps       — the two owner-signed genesis editions, already sealed
// minted.channel_id  — the #general channel
for wrap in &minted.wraps {
    client.send_event(wrap).to(&relays).await?;
}
```

The owner then needs the folded state, which is also what every member does on
join:

```rust
use concord::derive::{control_group_key, control_signer_group_key};
use concord::cord04::ParsedEdition;

let read = control_group_key(&minted.community_root, &minted.identity.community_id, Epoch(0))?;
let signer = control_signer_group_key(&minted.control_root, &minted.identity.community_id, Epoch(0))?;
let editions: Vec<ParsedEdition> = minted
    .wraps
    .iter()
    .map(|wrap| cord02::open_edition(wrap, &read, &signer.pk(), true))
    .collect::<Result<_, _>>()?;

let mut state = CommunityState::from_genesis(&minted, &editions, added_at_ms)?;
save_state(&client, &state).await?;
```

Put the community's relay list into `state.relays` and add those relays to the
client explicitly — coop's client is a gossip client with no background refresh.

## Joining

An invite link resolves to a bundle:

```rust
use concord::cord05::{self, BundleState, invite_bundle_key};

let link = cord05::parse_link(url)?;      // link_signer, token, bootstrap_relays, naddr

// The crate does no I/O: fetch the naddr from the fragment's relays, then:
let invite = match cord05::parse_bundle_event(&event, &link.link_signer, &invite_bundle_key(&link.token))? {
    BundleState::Live(invite) => invite,      // validate() already ran
    BundleState::Revoked => return Ok(None),  // a tombstone at the coordinate
};
```

A Direct Invite arrives as a NIP-59 gift wrap addressed to the member:

```rust
let (inviter, invite) = cord05::unwrap_direct_invite(&wrap, &my_keys).await?;
```

Either way the invite carries `community_id`, `owner`, `owner_salt`,
`community_root`, `root_epoch`, `control_pk`, the granted `channels`
(`ChannelGrant { id, key, epoch, name }`) and the relay set. `invite.expired(now_ms)`
is a preview rule: a past expiry still renders, but the join is refused.

Then publish a join so the member list sees the member before any backfill:

```rust
use concord::derive::guestbook_group_key;
use concord::cord02::guestbook;

let guestbook = guestbook_group_key(&invite.community_root, &invite.community_id, invite.root_epoch)?;
let rumor = cord02::guestbook::build_join(my_pk, Some((creator_npub, label)), now_ms);
let (wrap, _) = cord02::guestbook::seal_rumor(&rumor, &guestbook, &my_keys).await?;
client.send_event(&wrap).to(&relays).await?;
```

## Reading the Control Plane

```rust
use concord::cord02::{self, ControlFold};

let editions: Vec<ParsedEdition> = wraps
    .iter()
    .filter_map(|wrap| cord02::open_edition(wrap, &read, &control_pk, true).ok())
    .collect();

let control: ControlFold =
    cord02::fold_control(&owner, &community_id, &editions, &state.floors(), &state.banned);
state.apply_fold(&control);
```

`ControlFold` is everything the community UI needs:

| Field | Use |
| --- | --- |
| `community` | name, description, icon, banner, `message_expiration` |
| `channels` | the channel list; `deleted: true` means drop it |
| `roles` | `role()`, `roles_of()`, `effective_permissions()`, `is_authorized()`, `is_staff()` |
| `banned` | the banlist |
| `registries` / `is_public()` | each invite creator's live link signers |
| `pins` / `pin_content(id, channel)` | Pin List content per channel |
| `floors` | the committed heads the next fold is judged against |
| `gapped` | a chain hole: refetch the Control Plane before trusting what is missing |

`None` on `community` or a channel means "this client saw no authorized edition",
never "the value is gone" — keep what the state already holds rather than walking
the community backwards. Feed `state.floors()` and `state.banned` into the next
fold; they are its memory.

## Sending a message

```rust
use concord::cord03::{self, build_message};
use concord::derive::channel_group_key;

let plane = channel_group_key(&community_root, &channel, epoch)?;   // public channel
let rumor = build_message(my_pk, &channel, epoch, text, None, at_ms, timer);
let (wrap, wrap_key) = cord03::seal_rumor(&rumor, &plane, &my_keys, false).await?;
client.send_event(&wrap).to(&relays).await?;
```

- `epoch` is the channel's current epoch (`state.channels` carries it). A private
  channel derives from its own key instead of `community_root`.
- `timer` is `control.community.message_expiration`; pass `None` when it is off.
  The builder attaches the NIP-40 tag and `seal_rumor` mirrors it onto the wrap,
  so relays drop the ciphertext too.
- `ephemeral: true` picks kind `21059` for typing indicators. Keep the returned
  wrap key if the message may be deleted later — a kind-5 delete needs it.
- That `send_event` is the whole publish path; there is no optimistic echo. Feed
  the wrap through the same ingest path the subscription uses so send-then-read
  never waits on a relay round trip.

`build_edit`, `build_reaction` and `build_delete` are the same shape, each a rumor
about an existing `EventId` rather than a mutation.

## Reading a channel

```rust
use concord::cord03::{self, fold, plane_keys};

let planes = plane_keys(&held, &channel)?;            // &[(Epoch, secret)]
let mut rumors = Vec::new();

for wrap in &wraps {
    let Some((epoch, group)) = planes.iter().find(|(_, group)| group.pk() == wrap.pubkey) else {
        continue;
    };
    let Ok((opened, rumor)) = cord03::open(wrap, group, &channel, *epoch) else {
        continue;
    };
    store::cache_rumor(&client, &channel, &opened).await?;
    rumors.push(rumor);
}

let messages = fold(&rumors, Timestamp::now(), |actor, citation, author| {
    citation_ok(&owner, &community_id, actor, citation, &control.roles.floors)
        && control.roles.can_act_on_member(actor, &owner, author, Permissions::MANAGE_MESSAGES)
});
```

`ChatMessage` carries `content`, `at_ms`, `edited_at`, `deleted`, `reactions`,
`reply_to` and `thread_root` already resolved. The fold drops expired rumors; a
`deleted` row is still returned so the timeline keeps its shape.

Relay history pages through the local cache:

```rust
let page = store::backfill(client, &channel, &held, until, 50).await?;
let cached = store::query_rumors(&client, &channel, None, 50).await?;
```

`backfill` walks newest-first across every held epoch, caches what it opens, and
stops on a short page. `query_rumors` is the read path when the group keys are
gone. Run `store::purge_expired(client, &channel, now)` on the same cadence as
any other local sweep — the timer is cooperative, so the local store is the
artifact that has to forget.

`ChatAction::TimerNotice { seconds }` is a policy notice, not a message: render it
as an inline row only when its author passes
`control.roles.is_authorized(&author, &owner, Permissions::MANAGE_METADATA)`.

## Membership

```rust
let states = cord02::guestbook::coalesce(&rumors, now_ms, Some(&refounder_pk), |actor, target, citation| {
    citation_ok(&owner, &community_id, actor, citation, &control.roles.floors)
        && control.roles.can_act_on_member(actor, &owner, target, Permissions::KICK)
});
let members = cord02::guestbook::complete_memberlist(&states, &observed, &granted, &control.banned, &BTreeMap::new());
```

- `observed` is npub → ms for every author this client has seen publish anything
  usable, which is what makes a member visible before their Join arrives. Only
  count it forward.
- `granted` is every npub the roster ranks; they are members with no Guestbook
  entry at all.
- `banned_at` is empty today, so a ban is terminal in the fold. Fill it when the
  banlist head's timestamp is plumbed through.
- Removal is three separate actions, composed by the caller: strip the grant
  (immediate and cheap), then the kick directive, then — for a ban — the rotation
  that actually enforces it.

## Moderation writes

Every Control Plane write goes through one writer and one edition shape:

```rust
let writer = ControlWriter { author: my_pk, read: read.clone(), signer: signer.clone() };
let head = control.floors.get(entity).cloned();

let (wrap, new_head) = writer.set_community_metadata(
    &my_keys, &community_id, &metadata, head.as_ref(), citation, now_secs).await?;
```

`citation` is the `vac` the actor acts under — `None` only for the owner. Build it
from the folded Grant that ranks them (`AuthorityCitation { entity, version, hash }`)
and pass the head from the current fold, so the chain cannot silently fork.

Wrappers: `set_community_metadata`, `set_channel_metadata`, `set_role`,
`set_grant`, `set_banlist`, `set_registry`, `set_pin_list`, plus raw `publish`.
A ban is a `set_banlist` followed by a base rekey; a kick is a `set_grant` with an
empty `role_ids` followed by `cord02::guestbook::build_kick`.

## Pins

```rust
use concord::cord04::pins;

let entry = cord04::pins::build_entry(&opened_message, &plane, &channel)?;
let head_content = control.pin_content(&community_id, &channel).unwrap_or("");
let read = cord04::pins::read_list(head_content, |epoch| channel_group_key(&root, &channel, epoch).ok());
let content = cord04::pins::publishable(&read, channel_is_private, &plane, epoch)?;
let (wrap, _) = writer.set_pin_list(
    &my_keys, &community_id, &channel, &content, head, citation, now_secs).await?;
```

Reading is verification: `read_list` decodes either content form (public, or
sealed under the channel key at the named epoch), and
`cord04::pins::verify_entry(entry, &channel)` returns a `VerifiedPin` with the proven
author, words and time — no history and no old keys needed. `read.sealed` means
the list is sealed under an epoch this client never held: show it as unavailable,
and never write from it (`publishable` refuses). `cord04::pins::killed_by(&pin, &delete)`
answers whether a folded kind-5 erases an entry.

## Invites

```rust
use concord::derive::{invite_bundle_key, TOKEN_LEN};
use concord::cord05::{self, InviteEntry, InviteTombstone};

let token: [u8; TOKEN_LEN] = /* 16 bytes from any CSPRNG */;
let bundle_key = invite_bundle_key(&token);
let link_signer = Keys::generate();
let bundle = cord05::build_bundle_event(&link_signer, &invite, &bundle_key)?;
let url = cord05::build_invite_url(BASE, &link_signer.public_key(), &token, &relays)?;
```

A link is a coordinate plus a fragment: the naddr fetches the bundle, the token
unlocks it, and the fragment names the relays to fetch from.
`cord05::stock_relays()` is what a fragment with no relays of its own means.

The `link_signer` secret is what lets the creator refresh or retire the link, so
keep it against the token in the member's own Invite List — a local document
encrypted to self, exactly like the Community List:

```rust
let mut list = cord05::parse_invite_list(&my_keys, &event).await?;
list.entries.push(InviteEntry {
    token: HEXLOWER.encode(&token),
    signer_sk: link_signer.secret_key().to_secret_hex(),
    community_id: invite.community_id,
    url,
    label: None,
    created_at: now_ms,
    expires_at: None,
    extra: Default::default(),
});
let event = cord05::build_invite_list(&my_keys, &list).await?;      // kind 13303

// Retiring is a tombstone, never a deletion: it beats a stale copy terminally.
list.tombstones.push(InviteTombstone {
    token: HEXLOWER.encode(&token),
    community_id: invite.community_id,
    extra: Default::default(),
});
```

`merge_invite_lists` merges two devices' copies, `is_live(&token_hex)` answers
whether a link still stands, and `fits()` is the write gate.

## Rekeys, refounding and dissolution

A rotation is authority plus delivery: `rekey_authorized(&control.roles, &owner, &me, permission, &removed)`
gates it, `plan_refounding(epoch)` mints the new pair, and `build_rekey_chunks`
seals one blob per remaining member:

```rust
use concord::derive::epoch_key_commitment;
use concord::cord06::{self, RekeyScope};

let scope = RekeyScope::Channel(channel_id);                    // or RekeyScope::Base
let plan = cord06::plan_refounding(Epoch(epoch + 1))?;

// A base rotation delivers the new control-plane keys beside the root; a channel
// rotation delivers only that channel's fresh key.
let new_key = plan.new_root;
let (control_pk, control_root) = match scope {
    RekeyScope::Base => {
        let pk = plan.signer(&community_id)?.pk().to_bytes();
        (Some(pk), is_staff.then_some(&plan.new_control_root))
    }
    RekeyScope::Channel(_) => (None, None),
};

let mut blobs = Vec::with_capacity(members.len());

for member in &members {
    blobs.push(
        cord06::build_blob(
            &my_keys, member, scope, plan.epoch, &new_key, control_pk.as_ref(), control_root,
        )
        .await?,
    );
}

let rekey_group = cord06::rekey_group(scope, &community_root, &community_id, plan.epoch)?;
let wraps = cord06::build_rekey_chunks(
    &my_keys,
    &rekey_group,
    scope,
    plan.epoch,
    Epoch(epoch),
    &epoch_key_commitment(Epoch(epoch), &community_root),
    &blobs,
    citation,
    false,
    now_secs,
)
.await?;
```

On the receiving side, `cord06::parse_rekey_chunk(&opened)` per wrap, then
`collect_rotations(&chunks)`, then `am_i_removed(&rotation, &me)` — which is
`None` until every chunk is held, because an incomplete set is never a removal. A
member finds their delivery with `find_my_blobs` / `open_blob`, and adopts the key
only if the plaintext binds to the scope and epoch they expect and its `prevcommit`
matches the key they already hold. Two concurrent rotations settle on `fork_winner`.

The blob plaintext is a fixed-width binary record, but a signer's NIP-44 is
text-only, so `build_blob` carries it base64-encoded inside the envelope.
`open_blob` mirrors that, so the record layout and the `locator` are unchanged.

Dissolution is owner-only and terminal:

```rust
let rumor = cord06::dissolved_tombstone_rumor(owner_pk, &community_id, now_secs);
let wrap = cord06::seal_dissolved(&rumor, &community_id, &my_keys, now_secs).await?;

// A receiver seals the community read-only on sight.
if cord06::verify_dissolved(&wrap, &identity) {
    state.dissolved = true;
}
```

## The Community List

A member's own memberships, synced across their devices:

```rust
use concord::cord02::list;

let material = cord02::list::join_material(&invite, staff.then_some(&control_root));
let mut mine = cord02::list::parse_list_event(&my_keys, &event).await?;
mine = cord02::list::merge(mine, cord02::list::CommunityList {
    entries: vec![cord02::list::CommunityListEntry { community_id, seed: material.clone(), current: material, added_at: now_ms, extra: Default::default() }],
    ..Default::default()
});
let event = cord02::list::build_list_event(&my_keys, &mine).await?;      // kind 13302, NIP-44 to self
```

`is_live(&id)` answers joined-versus-left: a tombstone is terminal until a
strictly newer join outruns it. `fits()` is the write gate — 50 memberships and
the NIP-44 size cap, both protocol constants.

## GPUI integration

`crates/concord` stays GPUI-free; the registry and sync engine live in
`crates/community`. That layer adds a registry global and one
entity per community, and moves every decrypt, verification, fold and I/O off
the foreground thread.

### Entities

Same shape as `ChatRegistry`:

```rust
pub fn init(cx: &mut App) {
    CommunityRegistry::set_global(cx.new(CommunityRegistry::new), cx);
}

impl CommunityRegistry {
    pub fn global(cx: &App) -> Entity<Self> {
        cx.global::<GlobalCommunityRegistry>().0.clone()
    }
}
```

Call it after `cord03::init` in `desktop/src/main.rs` and `web/src/lib.rs`, and
subscribe to `NostrRegistry` for `SignerChanged` so the communities reset with
the account.

- `CommunityRegistry` holds `communities: Vec<Entity<Community>>`, an index by
  `CommunityId`, and `tasks: SmallVec<[Task<Result<()>>; 2]>`.
- `Community` owns one `CommunityState`, the last `ControlFold`, the member list
  and the channel list. Views render `Entity<Community>`; no protocol state
  lives in a view.
- `CommunityState::apply_fold` is one assignment: run it in the task that
  produced the fold and send only the result to the foreground.
- Emit an event on every fold so dependents re-read.

### Foreground and background

A background task never touches an entity. It sends results through a bounded
`flume` channel that a foreground `cx.spawn` drains with `this.update(...)`.

```rust
let (signal_tx, signal_rx) = flume::bounded::<Signal>(256);
let client = client.clone();

// Background: open, verify, fold — no entities.
self.ingress = Some(cx.background_spawn(async move {
    for wrap in &wraps {
        let Some(plane) = planes.iter().find(|plane| plane.group.pk() == wrap.pubkey) else {
            continue;
        };
        let (opened, rumor) = cord03::open(wrap, &plane.group, &plane.channel, plane.epoch)?;
        store::cache_rumor(&client, &plane.channel, &opened).await?;
        signal_tx.send_async(Signal::Chat { channel: plane.channel, rumor }).await?;
    }
    Ok(())
}));

// Foreground: the only place entities change.
self.consumer = Some(cx.spawn(async move |this, cx| {
    while let Ok(signal) = signal_rx.recv_async().await {
        this.update(cx, |this, cx| this.apply(signal, cx))?;
    }
    Ok(())
}));
```

- Every store function takes the `&Client` and reaches the database through
  `client.database()`, so clone the `Client` into the background task.
- Keep long-lived tasks in fields — dropping a `Task` cancels it. Assign `None`
  to an `Option<Task<_>>` before respawning it; a signer change replaces both
  the listener and the consumer.
- `cx.spawn` when the work updates an entity after awaiting, and
  `cx.background_spawn` when it only produces a value. A query the foreground awaits can be returned straight out: `fn messages(&self, cx: &App) -> Task<Result<Vec<ChatMessage>, Error>>`.
- Do the first load in `cx.defer_in(window, ...)` so `init` returns before the
  first relay request.
- NIP-46 signing is async: call `signer.get_public_key_async()` /
  `sign_event_async` inside the background task. Every account-key writer takes
  any signer (`Keys` or the app's `UniversalSigner`) and is `async`, so `await`
  it there rather than requiring device keys.

### Subscriptions

A wrap is addressed to a plane, so the plane's public key is the routing key and
one `Filter` per held plane is enough:

```rust
let filter = Filter::new()
    .kinds([Kind::from(KIND_WRAP), Kind::from(KIND_WRAP_EPHEMERAL)])
    .pubkey(plane.group.pk())
    .since(joined_at);
client.subscribe(filter).with_id(sub_id).await?;
```

- `pubkeys([...])` carries every plane of a community on one subscription. Call
  `subscribe` again with the new address whenever a join, a channel add or a
  rekey fold changes it.
- Route inbound events by `subscription_id` from `RelayMessage::Event`, never by
  kind.
- Watch one epoch ahead: while holding `root_N`, subscribe to
  `base_rekey_group_key(&root_N, &community_id, Epoch(N + 1))` and to
  `channel_rekey_group_key(&root_N, &channel, Epoch(N + 1))` for each private
  channel. A second epoch ahead is not derivable until the new root arrives.

### Tests

`cx.background_executor().timer(..)` for delays, never `smol::Timer`, or
`run_until_parked()` finds nothing left to run. Push a wrap into the channel and
`run_until_parked()` to drive the foreground consumer.

## Not wired up yet

- **`crates/concord` stays protocol-only; the registry lives in
  `crates/community`.** `concord` has no subscriptions, no `init`, and no
  `Entity<Community>`; `community::CommunityRegistry` owns one `Entity<Community>`
  per state document, subscribes when a community's plane set changes, and
  re-folds on an inbound wrap. Nothing observes `CommunityEvent` yet, and
  `CommunityRegistry::create` persists the genesis locally without publishing it
  to the metadata's relays.
- **Account-key writers take any signer, not `&Keys`.** `genesis`,
  `ControlWriter`, the guestbook and chat `seal_rumor`s, the `list` builders, and
  the `cord05` invite writers (`build_direct_invite` / `unwrap_direct_invite`,
  `build_invite_list` / `parse_invite_list`) and `cord06` blob writers
  (`build_blob` / `open_blob`) are `async` and generic over the SDK's
  `AsyncGetPublicKey` / `AsyncSignEvent` / `AsyncNip44` traits, so a `Keys` and an
  app `UniversalSigner` both work. The NIP-59 paths (`build_direct_invite`,
  `unwrap_direct_invite`) stay `Sized` because the SDK's gift-wrap helpers are.
  Group-key and locally-held-secret writers (`cord01` wrap functions,
  `cord05::build_bundle_event`, `store`) still take the raw key material they
  genuinely need.
- **`crates/chat/src/lib.rs::handle_notifications` treats every kind 1059 event as
  a NIP-59 gift wrap for the current user.** Concord wraps are kind 1059 too, so
  that handler must route by subscription id before any concord subscription goes
  live, or every stream wrap lands in the DM trash and raises a toast.
- **No plane key can be persisted yet.** `CommunityState` has nowhere to keep a
  key a rotation delivered and `ChannelKeyRef` carries no key of its own, so a
  client can verify a rotation and still lose it on restart — history under a
  prior root or a prior channel epoch is unreadable until that schema change
  lands.
