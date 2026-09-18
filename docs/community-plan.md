# The community registry — implementation plan

`crates/community` is the GPUI layer over `crates/concord`: a global registry, one
entity per community, and one notification stream that keeps both fed from the
local database. `crates/concord` stays GPUI-free; this crate is the only place
where entities, tasks and subscriptions meet.

The shape follows `crates/chat`: a registry global created in `init`, entities
that own protocol state, a background notification listener, and a foreground
consumer that is the only writer of entity state.

## Scope

In:

- discovery of the current account's communities from the local database;
- one `Entity<Community>` owning `CommunityState`, the last `ControlFold`, the
  folded member list and the channel list;
- subscriptions to every held plane (Control, Guestbook, public Channels);
- notification-driven refresh: open and cache wraps in the background, fold them
  there too, and apply only the result on the foreground;
- re-subscription when a fold moves a plane address or a relay list changes.

Out (see "Known gaps"): rekeys, private-channel keys, sends, moderation,
invites, chat timelines, and NIP-46 writers.

## Crate layout

| File | Owns |
| --- | --- |
| `community.rs` | The `Community` entity, `CommunityEvent`, refresh coalescing |
| `lib.rs` | `init`, `CommunityRegistry`, the signal channel, subscription sync |
| `sync.rs` | Planes, the REQ filter, loading from the database, the fold |

## Entities

`CommunityRegistry` mirrors `ChatRegistry`:

```rust
pub struct CommunityRegistry {
    communities: Vec<Entity<Community>>,
    index: HashMap<CommunityId, Entity<Community>>,
    synced: HashMap<CommunityId, SubscriptionKey>,
    signal_tx: flume::Sender<Signal>,
    signal_rx: flume::Receiver<Signal>,
    tasks: SmallVec<[Task<Result<(), Error>>; 2]>,
    notification_listener: Option<Task<Result<(), Error>>>,
    signal_consumer: Option<Task<Result<(), Error>>>,
    _subscriptions: SmallVec<[Subscription; 2]>,
}
```

```rust
pub enum CommunityEvent {
    Updated(CommunityId),
    Error(String),
}
```

- `Updated` is emitted by a `Community` after a fold is applied; the registry
  emits only `Error`. Views subscribe where they render.
- `Community` holds `state: CommunityState`, `control: ControlFold`,
  `members: BTreeSet<PublicKey>`, a `dirty` flag and one in-flight
  `refresh_task`. Public reads: `id()`, `state()`, `control()`, `members()`,
  `channels()`.
- The registry is not an emitter of `Updated`: a view holds
  `Entity<Community>` and observes that.

## Loading

The registry loads when the signer changes and once at startup through
`cx.defer_in`.

- State documents are discovered by scanning the local database for
  `Kind::ApplicationSpecificData` events whose `d` tag is `concord/<hex>`,
  newest per community. This is the bootstrap path: `cord02::genesis` +
  `store::save_state` (the creation flow in `concord-usage.md`) writes no list
  entry, so a list-only load would show nothing until joining exists.
- The Community List (`kind 13302`, NIP-44 to self) is read from the database
  without `fetch_events`; decryption goes through `UniversalSigner::nip44_decrypt_async`,
  so NIP-46 signers work. When a list exists it is authoritative for liveness:
  a state document whose id is not live (no entry, or a newer tombstone) is
  dropped. With no list event, every state document loads.
- `cord02::list::parse_list_event` takes `&Keys`, which the UI layer does not
  hold, so the list is decrypted with the signer and parsed as
  `CommunityList` directly. Nothing in the list is rewritten here.

Creation and join flows persist a `CommunityState` themselves and call
`CommunityRegistry::reload`; the registry grows no writer APIs it cannot
correctly support.

## Subscriptions

A community's planes are derived from its state; the wrap's *author* is the
routing key.

| Plane | Read key | Wrap author (`Filter::authors`) |
| --- | --- | --- |
| Control, each held epoch | `control_group_key(root, id, epoch)` | `state.control_pks[epoch]` (the signer pk) |
| Guestbook | `guestbook_group_key(root, id, root_epoch)` | the group's own pk |
| Channel (public only) | `channel_group_key(root, channel, channel.epoch)` | the group's own pk |

One caveat on the snippet in `concord-usage.md`: it uses
`Filter::new().pubkey(plane.pk())`, but in this nostr-sdk `Filter::pubkey` adds a
`#p` tag constraint, and a Concord wrap's `p` tag is a random ephemeral key
(`cord01::wrap_seal_with`). The filter must be `.authors(...)`, matching the
event author that `open_wrap_at` already checks.

- One subscription id per community: `SubscriptionId::new("concord/<hex>")`.
  Routing back from a notification is a prefix strip and a hex parse.
- `Community` exposes a cheap `SubscriptionKey` (control pks, channels + epochs
  + privacy, relays). When a fold changes it, the registry re-subscribes:
  `unsubscribe` then `subscribe` with the same id.
- Community relays are added to the client explicitly
  (`client.add_relay(..).and_connect()`), per `concord-usage.md`.

## Notification stream

`client.notifications()` is one stream for the whole app; the community listener
takes the first-seen variant and routes by subscription id, never by kind:

```rust
while let Some(notification) = notifications.next().await {
    let ClientNotification::Event { subscription_id, event, .. } = notification else {
        continue;
    };
    if event.kind != Kind::from(KIND_WRAP) {
        continue;
    }
    let Some(id) = sync::community_of(&subscription_id) else {
        continue;
    };
    tx.send_async(Signal::Event(id)).await?;
}
```

- `ClientNotification::Event` fires only the first time an event is seen; the
  relay has already saved it to the local database before notifying
  (`nostr-sdk` relay inner), so a signal only needs the community id and the
  fold reads the wrap back from the database. This is also what makes restart
  work: a backlog already in the database produces no notification, so
  `Community::refresh` runs once when the community is tracked.
- `KIND_WRAP_EPHEMERAL` (21059, typing) is not subscribed: ephemeral events are
  never persisted, so the database-read path cannot see them. Nothing in the
  registry consumes typing today.
- The channel is `flume::bounded(256)`; the consumer is a foreground `cx.spawn`
  that updates entities, as in `concord-usage.md`.

The chat registry must route gift wraps by subscription id before community
subscriptions go live, or every stream wrap lands in the DM trash:

```rust
RelayMessage::Event { subscription_id, event } => {
    if event.kind == Kind::GiftWrap
        && subscription_id.as_ref() != sub_id1.as_str()
        && subscription_id.as_ref() != sub_id2.as_str()
    {
        continue;
    }
    // ..
}
```

The `InboxRelays` handling in the same loop stays unscoped: it arrives on a
short-lived subscription with a generated id.

## The fold

One background function, `sync::fold(database, state) -> Snapshot`, does all
crypto, verification, I/O and folding. `Snapshot` carries the applied
`CommunityState`, the `ControlFold` and the member set; the foreground only
assigns.

1. Derive the held planes.
2. For each plane, query the database for `KIND_WRAP` events authored by the
   plane address and open them:
   - Control: `cord02::open_edition(wrap, read, address, true)` → `ParsedEdition`;
   - Guestbook: `cord02::guestbook::open(wrap, group)` → `GuestbookRumor`;
   - Channel: `cord03::open(wrap, group, channel, epoch)` then
     `store::cache_rumor` — the chat read path is already database-backed.
   Collect `observed: PublicKey -> ms` from every author that opened.
3. `cord02::fold_control(owner, id, &editions, &state.floors(), &state.banned)`,
   then `state.apply_fold` and `store::save_state`, all in this task. If no
   edition opened at all, the fold is not applied: an empty fold would erase the
   committed floors the next fold is judged against.
4. `cord02::guestbook::coalesce` with the roster-backed `can_kick`
   (`citation_ok` + `can_act_on_member(.., Permissions::KICK)`), then
   `complete_memberlist` with `observed`, the roster's grants, `control.banned`
   and an empty `banned_at`. The owner is inserted explicitly — the roster does
   not mint an implicit grant for them.

## Foreground and background

- Every entity touch and every fold application happens on the foreground.
  Background tasks only read the database and return values.
- `Community::refresh` coalesces: if a fold is in flight it sets `dirty`, and
  the completion applies the snapshot, clears the task, then runs one more fold
  if dirtied. A burst of backlog events produces at most two folds.
- `refresh_task: Option<Task<_>>` is dropped on reset, which cancels it.
- The registry observes each community (`cx.observe`) and re-syncs
  subscriptions when a fold changed a plane or relay set; sync is a key
  comparison, so ordinary notifies are a no-op.
- Errors from load/subscribe/fold reach the UI as `CommunityEvent::Error`; a
  task whose result is never read must not be the only error path.

## Integration

- `community::init(window, cx)` in `desktop/src/main.rs` and `web/src/lib.rs`
  after `chat::init`.
- Chat's notification routing fix above.
- `concord::store::{save_state, load_state}` gain `+ ?Sized` on `D`: the
  integration path passes `&dyn NostrDatabase` (the doc's advice cannot compile
  against the current bound). `cache_rumor`, `query_rumors`, `purge_expired`
  and `backfill` already take `&dyn`.

## Tests

Pure `#[test]` with `MemoryDatabase` and `smol::block_on`, like `concord`'s
store tests; no GPUI test context (no registry test exists in this repo, and
`state::init` owns the global client).

1. genesis folds into metadata, the general channel, and an owner-only member
   list, and the folded state is persisted.
2. a member's join becomes a member and a later leave removes them.
3. the subscription filter asks for every held plane by wrap author, and
   `community_of(subscription_id(id)) == Some(id)`.
4. loading: state documents load without a list; a Community List entry keeps a
   community and a newer tombstone hides it.

## Known gaps

- **Rekeys are not adopted.** `CommunityState` cannot hold a second root or a
  channel key, so the rekey planes (one epoch ahead) are not subscribed. A
  community stays on the plane set its state can derive.
- **Private channels are skipped**, not guessed: no key is held for them yet.
- The fold re-opens every wrap on every refresh. `ClientNotification::Event`
  plus refresh coalescing keep it bounded, and the database read path stays
  simple; incremental caches are a follow-up.
- `list.is_live` filtering is only as fresh as the last list event; the
  registry never writes list or state documents for the user's account.

## Phases

Phases are sequential: each one lands compiling code and has an exit check. Nothing
in a later phase is started before the earlier one is green, so the crate is never
in a half-wired state.

### Phase 0 — Decisions

Five calls to confirm before writing code. Defaults in brackets.

1. **v1 planes** [Control + Guestbook + public Channels]. Rekeys and private
   channels are out of scope, not stubs.
2. **Discovery and liveness** [scan the local DB for `concord/<hex>` state
   documents; read the Community List when present and use `is_live` to drop
   tombstones; never republish the list].
3. **Wiring** [`community::init` after `chat::init` in `desktop` and `web`].
4. **Chat routing fix** [route kind 1059 by subscription id in
   `chat::handle_notifications`; leave `InboxRelays` unscoped].
5. **`concord::store` bound** [add `+ ?Sized` to `save_state`/`load_state` so
   `&dyn NostrDatabase` compiles; the doc's snippet does not compile today].
6. **Tests** [pure `#[test]` + `smol::block_on` + `MemoryDatabase`; no GPUI test
   context].

Exit: all six confirmed. Any that change rewrite the affected phase below.

### Phase 1 — Crate skeleton

- Create `crates/community/Cargo.toml` and `src/{lib,community,sync}.rs` stubs.
- Deps: `concord`, `state`, `gpui`, `nostr-sdk`, `anyhow`, `flume`, `log`,
  `serde_json`, `smallvec`. Dev: `nostr-memory`, `smol`.
- The workspace already globs `crates/*`, so no root manifest edit.

Exit: `cargo check -p community` passes with the empty modules.

### Phase 2 — `sync.rs`

Pure, GPUI-free plumbing: `Plane`/`PlaneKind`, `planes(&CommunityState)`,
`subscription_filter` (using `.authors(...)`, not `.pubkey(...)`), the
`SubscriptionId`/`community_of` round-trip, `load`, and `fold -> Snapshot`.

Exit: unit tests 3 and 4 pass; no GPUI types in the file.

### Phase 3 — `community.rs`

`Community` entity (`state`, `control`, `members`, `dirty`, in-flight
`refresh_task`), `CommunityEvent::{Updated, Error}`, and coalesced refresh that
spawns the fold on `background_spawn` and applies the snapshot on the foreground.

Exit: tests 1 and 2 pass; entity compiles against a `TestAppContext`-free test.

### Phase 4 — `lib.rs`

`init` plus `CommunityRegistry`: bounded `flume(256)` signal channel, the
notification listener task, the foreground consumer, `SubscriptionKey` re-sync on
observe, and `reset`/`reload` on `StateEvent::SignerChanged`.

Exit: registry starts and stops cleanly under `cargo check`; listener routes by
subscription id only.

### Phase 5 — Cross-crate fixes

- `concord::store::{save_state, load_state}` gain `+ ?Sized`.
- Chat gift-wrap routing fix.

Exit: `cargo check -p concord -p chat` passes; no behaviour change for DM-only
clients.

### Phase 6 — App wiring

Call `community::init(window, cx)` after `chat::init` in `desktop/src/main.rs`
and `web/src/lib.rs`.

Exit: `cargo check -p coop` (or the app targets) passes.

### Phase 7 — Validation

Run the four tests plus `cargo check` and `cargo test` for `community`, then the
workspace.

Exit: all green, or failing lines reported with root cause.

### Phase 8 — Doc finalization

Reconcile this document with what actually landed (scope, gaps, test names) and
remove the draft's speculative sections that were cut.

Exit: the doc matches the code.
