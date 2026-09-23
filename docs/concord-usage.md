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
| `state` | The community state document and its pure folds — no I/O |

`CommunityId`, `ChannelId`, `RoleId`, `Epoch` and `Extra` (crate-internal) come from
the private `types` module and are re-exported at the crate root.

Nothing in `concord` touches a relay or a database. The local rumor cache, the
state documents and the relay paging walk live one crate up, in
`community::cache` and `community::history`, and are what a client actually calls
(read on below).

CORD-07 (audio/video) is unimplemented. CORD-08's timer has no file of its own: it
lives in the metadata it reads (`cord02`) and the fold it filters (`cord03`).

Read `CommunityId` as "this community", `ChannelId` as "this channel", `Epoch` as
"which key generation". Nothing else in the API needs internal state.

## Creating a community

```rust
use concord::cord02::{self, CommunityMetadata};
use concord::state::CommunityState;
use community::cache::save_state;

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

Creating is not finished until the membership is announced. The two writes are
independent and both best-effort: the genesis wraps go to the community's
relays, and the membership goes to the account's own Community List (below), so a
new device — or another client — can find the community without an invite.
`crates/community`'s `sync::create` performs both.

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

- `epoch` is the **root** epoch for a public channel: its plane is
  `channel_group_key(community_root, channel, root_epoch)`, so a Refounding moves
  it along with the root. A private channel passes its own current channel epoch
  (`state.channels` carries it) and derives from its own key instead of
  `community_root`.
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
use community::cache;

let planes = plane_keys(&held, &channel)?;            // &[(Epoch, secret)]
let mut rumors = Vec::new();

for wrap in &wraps {
    let Some((epoch, group)) = planes.iter().find(|(_, group)| group.pk() == wrap.pubkey) else {
        continue;
    };
    let Ok((opened, rumor)) = cord03::open(wrap, group, &channel, *epoch) else {
        continue;
    };
    cache::cache_rumor(&client, &channel, &opened).await?;
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
use community::history::{self, PageRegistry, Window};

// Every key the client still holds for the channel, newest epoch first: the
// state's current key (`ChannelKeyRef::key` at `epoch`) plus every `priors`
// entry a rotation stepped off. A public channel derives one per held root.
let held: Vec<concord::state::HeldKey> = state.held_keys(&channel);

// The same registry `CommunityRegistry` shares with its notification pump: it is
// what carries each page's EOSE and CLOSED back to the round waiting on it.
let page = history::page(
    client,
    &pages,
    &channel,
    &held,
    &relays,
    Window::opening(cursor),
    20,
    50,
)
.await?;
let cached = cache::query_rumors(&client, &channel, None, 50, Some(&cord03::ROW_KINDS)).await?;
```

A key's `retired_at` (a `Timestamp`, set when a rotation supersedes it) is a read
cutoff: a wrap at that epoch with a later `created_at` is refused, so a retired
epoch is history and never a live plane an ejected holder can keep writing into.

A relay is only ever *asked*: `history::page` installs **one REQ per page** over
`ReqTarget::manual` for the community's own relays (which is what makes the client
verify a wrap, deduplicate it and persist it), waits for every relay to settle,
and then reads the page back out of the local database — no wrap is ever consumed
straight off the wire, and `fetch_events` is not used anywhere. The wrap lands in
the client's shared event store through that ordinary ingest path; `cache_rumor`
then puts the decrypted rumor beside it, and only the rumor is ever served to a
reader.

EOSE and CLOSED are **not** watched inside the page. The registry's notification
pump is the only consumer of `client.notifications()`; it delivers one
`PageReport` per settling relay into the `PageRegistry` under the page's
subscription id, and the page waits on that channel with `PAGE_TIMEOUT`. The id
is opaque (`concord-history-<n>`) because a NIP-01 id is capped at 64 characters,
and the registry is what maps it back to the waiting round.

A page ends on EOSE. A CLOSED ends it as unanswered, *except* NIP-42's
`auth-required`: the SDK re-issues that REQ under the same subscription id once
the handshake completes, so the page waits for the resubscribed answer rather
than writing off a relay that only wanted to authenticate.

`history::page` walks newest-first across every held epoch, caches what it opens, and
reports what it saw: `oldest`/`newest` feed the caller's `ChannelCursor`,
`exhausted` is earned only by a short page *after* history was seen, and an
all-empty answer sets `failed` so a later round re-asks instead of sealing the
channel at "no more history". `unreadable` counts the wraps the page reached that
no held key could open — sealed past the cutoff their key's rotation set, or bound
to another channel — because those are history the reader is missing, not history
that is not there. Page down with `Window::older_than(seen.oldest)`,
open a channel with `Window::opening(cursor)` (wide cold, `newest - 60s` warm),
and read the region between two cursors with `Window::between(..)`. A `Window`
carries `Timestamp`s, so its bounds go straight into a NIP-01 filter with no unit
conversion; only a reader's millisecond `at_ms` narrows to a second at the query.
`query_rumors` is the read path when the group keys are gone; pass `kinds` to
budget rows apart from the events that only decorate them. `cache::wrapper_index`
reads the cached rows back keyed by the wrap they came from, which is what lets
`sync::fold` observe author and message times without re-opening a wrap it already
cached, and `cache::purge_expired(client, &channel, now)` runs at the top of every
round — the timer is cooperative, so the local store is the artifact that has to
forget.

`sync::fold` counts the same thing over the whole store, per channel, in
`Snapshot.unreadable`. `sync_round` sums a round's pages into
`Progress.unreadable`, and `Community` keeps both per channel: `progress(channel)`
is the last completed round, `unreadable(channel)` is the count, and
`missing_key(channel)`, `channel_removed_at(channel)`, `removed_at()` and
`stranded()` are the rest of the honest read surface a panel needs to tell an
empty room from one it cannot read. `Community::due(channel)` is the other half —
whether a round is worth asking for yet (a round for `Older` and an explicit retry
never ask it), and `Community::tick`, driven by `CommunityRegistry` once per
`community::MIN_ROUND_INTERVAL`, re-folds and re-rounds the active channel of a
community whose last round is older than `STALE_AFTER`.

`ChatAction::TimerNotice { seconds }` is a policy notice, not a message: render it
as an inline row only when its author passes
`control.roles.is_authorized(&author, &owner, Permissions::MANAGE_METADATA)`.

## Membership

```rust
let states = cord02::guestbook::coalesce(&rumors, now_ms, &refounders, |actor, target, citation| {
    citation_ok(&owner, &community_id, actor, citation, &control.roles.floors)
        && control.roles.can_act_on_member(actor, &owner, target, Permissions::KICK)
});
let members = cord02::guestbook::complete_memberlist(&states, &observed, &granted, &control.banned, &BTreeMap::new());
```

- `refounders` is the set of npubs whose rotations minted an epoch this client
  verified (`CommunityState.refounders`) — a snapshot chunk is honored only from
  one of them, and an empty set honors none. A rotation that delivered this
  client no key still names its minter, so the set is filled by any base rotation
  whose continuity verifies against a root held (`rekey::walk`); a List entry's
  `refounder`, read by `list::JoinMaterial::refounder`, is the other source.
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
gates it, `plan_rotation(scope, epoch)` mints what it delivers, and
`build_rekey_chunks` seals one blob per remaining member:

```rust
use concord::derive::epoch_key_commitment;
use concord::cord06::{self, RekeyScope, RotationPlan};

let scope = RekeyScope::Channel(channel_id);                    // or RekeyScope::Base
let plan = cord06::plan_rotation(scope, Epoch(epoch + 1))?;
let new_key = plan.new_key();

// A base rotation delivers the new control-plane keys beside the root; a channel
// rotation delivers only that channel's fresh key.
let (control_pk, control_root) = match &plan {
    RotationPlan::Base(refounding) => {
        let pk = refounding.signer(&community_id)?.pk().to_bytes();
        (Some(pk), is_staff.then_some(&refounding.new_control_root))
    }
    RotationPlan::Channel { .. } => (None, None),
};

let mut blobs = Vec::with_capacity(members.len());

for member in &members {
    blobs.push(
        cord06::build_blob(
            &my_keys,
            member,
            scope,
            plan.epoch(),
            &new_key,
            control_pk.as_ref(),
            control_root,
        )
        .await?,
    );
}

let rekey_group = cord06::rekey_group(scope, &community_root, &community_id, plan.epoch())?;
let wraps = cord06::build_rekey_chunks(
    &my_keys,
    &rekey_group,
    scope,
    plan.epoch(),
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

`community::rekey::adopt` is that receiver, run against the local database: it
walks a scope forward one epoch at a time off the key actually held (never
waiving a gap, never adopting a fork), keeps each stepped-off key as a prior
with the rotation's publish time as its read cutoff, and reports a removal or a
strand when a complete rotation carries no blob for the member.

`community::rekey::rotate` is the sender, run against the held state: it resolves
the epoch and key the scope is stepping off, refuses a rotation that would skip or
cut off its own rotator, delivers one blob per recipient (`Rewrite.recipients`,
which the caller names because a private channel's audience is not in the local
state), marks the rotation severed when it excludes somebody, and — for a base
scope — carries the settled Control Plane heads onto the new epoch's groups with
`cord06::compact`. `Community::rotate` checks `Rewrite::authorized` first, the same
`rekey_authorized` under the same permissions `adopt` applies, publishes to the
community's relays, and then adopts what it published through `rekey::adopt`
rather than by construction: a rotation this client wrote and one it received take
one path.

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

let entry = concord::state::list_entry(&state, &metadata.name);  // state → §8 material
let held = list::parse_list_event(&my_keys, &event).await?;       // validates the d tag
let mine = held.joined(entry);                                    // community_id-keyed union
let event = list::build_list_event(&my_keys, &mine, 0, now_secs).await?;  // kind 33302, d = 0
client.send_event(&event).to_nip65().await?;                      // account's own write relays
```

The publish needs no separate database write: `send_event` persists the event
locally *before* it resolves targets, so the fragment is available to the
`concord/list` read path even if every relay is unreachable.

`join_material(&invite, control_root)` makes the §8 material from a CORD-05
invite; `concord::state::list_entry(state, name)` makes it from a `CommunityState`, and is
what a write path uses after a create, join or rename. `joined` and `tombstoned`
are the two mutations: both are `community_id`-keyed unions, so neither an append
nor a leave can lose a membership the other writer has.

The entry is signed by the member's real key and sealed to that same key
(`seal_to_self`), so only the member's devices read it — a stranger's
`parse_list_event` fails rather than returning a partial list. Publishing goes to
the member's **NIP-65 write relays**, the same set the `concord/list`
subscription resolves for its `author` filter.

Kind `33302` is **addressable and fragmented**: one event per fragment, its `d`
tag the fragment index in decimal. `frags` in the payload declares how many the
List has, and `is_complete(held_indices)` answers whether the client has a
fragment at every index below it. `merge` resolves a `frags` disagreement to the
larger value. (`13302`, the single-event List, is retired by the spec — a
replaceable kind cannot fragment.)

A join material may also carry two hex extensions this crate does not write but
does read, because they are the only place a device that never held a rotation can
learn who minted an epoch: `refounder` names the npub whose Refounding minted the
entry's `root_epoch`, and `held_roots` is the retained prior epochs
(`[{epoch, key, refounder?, control_pk?, retired_at?}]`). `JoinMaterial::refounder()`
and `JoinMaterial::held_roots()` decode both; unknown fields are round-tripped
regardless, so a document that carries them keeps them.

The payload's 32-byte values are **unpadded base64url at every depth**, which is
section-scoped to §8: CORD-05 invites stay hex. The writer re-encodes them on
every serialization, so its output is always the canonical 43-character spelling;
the reader also accepts non-zero trailing bits, because the spec's own worked
example contains them and no reader can tell a mis-encoded named field from a
correct one. The codec is `utils::base64url` and the wire structs behind the
List's `Serialize`/`Deserialize` are the only callers, so no other encoding path
is touched.

Three write-time rules are folded into serialization, so an in-memory document
and its wire form differ:

- an embedded snapshot omits `community_id` and inherits the entry's;
- `seed` is omitted when it equals `current`, and its cosmetic fields (`name`,
  `relays`, each channel's `name`) are overwritten from `current` first, so a
  rename collapses the snapshots instead of forking them;
- an entry whose `added_at` does not outrun its tombstone is omitted — the
  tombstone alone carries the state.

`is_live(&id)` answers joined-versus-left: a tombstone is terminal until a
strictly newer join outruns it. `fits()` is the write gate: 50 memberships and
the NIP-44 plaintext cap. The 50 is a stopgap inherited from the retired
single-event design — §8 has **no membership limit**, its only bound is the
65,536-byte encoded event, and the real fix is to start a new fragment on write.
Until that lands, an append onto a List that already spans more than one fragment
is refused rather than performed against a partial read, because placing a new
membership needs a repack. The community stays local (`load` keeps a membership
the List never mentions) and the write is deferred with a warning.

Discovery is a **subscription, not a fetch**: subscribe with
`Filter::new().kind(Kind::Custom(KIND_COMMUNITY_LIST)).author(my_pk)` and read the
fragments back out of `client.database()`. The client persists a relay's event
before it notifies, so a subscription plus a database read loses nothing and
needs no explicit save. Parse each event with `parse_list_event`, keep the newest
per `fragment_index`, and `merge` them — reading an incomplete List is safe, since
a missing fragment is only news not yet heard.

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
        cache::cache_rumor(&client, &plane.channel, &opened).await?;
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

- Every cache function takes the `&Client` and reaches the database through
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
- Watch one epoch ahead for the base, and a **window** of channel epochs under
  every held root for private channels: `base_rekey_group_key(&root_N, &id,
  Epoch(N + 1))`, plus `channel_rekey_group_key(&root, &channel, Epoch(channel_epoch
  + ahead))` for `ahead in 1..=8` and each root in `state.roots()`. A Refounding
  seals its channel rekeys under the root current when it was minted, so a member
  who adopted the base rotation first must still ask under the prior root; the
  window is what lets a member who missed a rotation catch up, or learn they were
  cut. This is `community::rekey::watches`, installed as a second standing REQ
  whose id resolves back to its community through `rekey::WatchRegistry` (the id
  cannot carry a 64-hex community id within the NIP-01 length cap).
- `community::rekey::adopt` then reads those wraps **from the database** and walks
  each scope forward one epoch at a time: a complete, authorized rotation whose
  `prevcommit` extends the key actually held hands over the next key (raced
  rotations settle on the lowest); a gap is fetched, never waived; a fork is never
  adopted. An addressed-but-unverifiable rotation is neither adopted nor read as a
  removal. Only a complete rotation at/after the join, from a rotator who outranks
  the member, with no blob for them, is a removal; one that predates the join is a
  strand — a stale invite landed the member on a superseded epoch.

### Tests

`cx.background_executor().timer(..)` for delays, never `smol::Timer`, or
`run_until_parked()` finds nothing left to run. Push a wrap into the channel and
`run_until_parked()` to drive the foreground consumer.

## Not wired up yet

- **`crates/concord` stays protocol-only; the registry lives in
  `crates/community`.** `concord` has no subscriptions, no `init`, and no
  `Entity<Community>`; `community::CommunityRegistry` owns one `Entity<Community>`
  per state document, subscribes when a community's plane set changes, and
  re-folds on an inbound wrap. The sidebar subscribes to the registry, surfaces
  `CommunityEvent::Error` as a window notification, and its "New community" row
  in the Communities tab dispatches `Command::NewCommunity`, whose name prompt
  calls `CommunityRegistry::create`. `create` persists the
  genesis locally, publishes the wraps to the community's relays, and records the
  membership in the account's Community List — all best-effort, so a relay that is
  down warns without losing the community. Discovery
  subscribes to the account's CORD-02 Community List (`33302`) under the
  `concord/list` subscription id and reads the fragments back out of
  `client.database()` — the SDK persists a relay's event before notifying, so the
  read is always current. A `concord/list` notification re-runs `load`, which
  materializes a community from each live List entry (`from_join_material`) and
  keeps any state document the List does not mention, so a fresh install — or one
  signing in as an account that joined elsewhere — finds its communities. See
  `docs/concord-community-discovery-plan.md` (including its note on the retired
  `13302` the current reference client still writes).
- **Account-key writers take any signer, not `&Keys`.** `genesis`,
  `ControlWriter`, the guestbook and chat `seal_rumor`s, the `list` builders, and
  the `cord05` invite writers (`build_direct_invite` / `unwrap_direct_invite`,
  `build_invite_list` / `parse_invite_list`) and `cord06` blob writers
  (`build_blob` / `open_blob`) are `async` and generic over the SDK's
  `AsyncGetPublicKey` / `AsyncSignEvent` / `AsyncNip44` traits, so a `Keys` and an
  app `UniversalSigner` both work. The NIP-59 paths (`build_direct_invite`,
  `unwrap_direct_invite`) stay `Sized` because the SDK's gift-wrap helpers are.
  Group-key and locally-held-secret writers (`cord01` wrap functions,
  `cord05::build_bundle_event`, `community::cache`) still take the raw key material they
  genuinely need.
- **`crates/chat/src/lib.rs::handle_notifications` treats every kind 1059 event as
  a NIP-59 gift wrap for the current user.** Concord wraps are kind 1059 too, so
  that handler must route by subscription id before any concord subscription goes
  live, or every stream wrap lands in the DM trash and raises a toast.
- **Rotation-delivered plane keys are persisted, and history spans them.**
  `CommunityState.held_roots` keeps every root a rotation stepped off (with the
  retired root's Control signer and the publish time that retires it), and each
  `ChannelKeyRef.priors` keeps every channel key a rotation stepped off. The read
  side derives planes from all of them, and a retired key's `retired_at` is a hard
  read cutoff at both `history::page` and `sync::fold`. The state
  `Community::removed_at`/`Community::stranded`/`channel_removed_at` carries is
  rendered as a notice **and** enforced at write time: `Community::send` refuses
  when `channel_secret` is `None` (a removal, a strand, a channel cut, or a key we
  never held), because a wrap sealed under a superseded root would reach nobody who
  rotated. A rotation this client publishes itself (`Community::rotate`) is adopted
  through the same `rekey::adopt` an arriving one goes through, so the held state
  and the wire cannot diverge.
