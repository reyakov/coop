# Concord discovery: why no community ever reaches `subscribe`

Audit + fix plan. Read alongside `docs/concord-usage.md` and
`docs/concord-simplification-plan.md`.

## Symptom

`crates/community/src/lib.rs::subscribe` is never called, so no wrap is ever
subscribed to and the sidebar stays empty. `community load: 0 state document(s)
found` is the only clue.

## Root cause

`CommunityRegistry::load` only ever reads the **local database**. Nothing in the
discovery path touches a relay.

```
community::init
  └─ SignerChanged → load
       └─ sync::load
            ├─ store::load_states(client)   → client.database().query(..)   // local only
            └─ load_list(client, ..)        → client.database().query(..)   // local only, .limit(1)
       → track([])
       → sync_subscriptions: `for community in self.communities` runs zero times
       → subscribe never called
       → no relay is ever queried
       → the database never fills
       → load stays empty forever
```

The loop is self-reinforcing: the local database is populated *by* the
subscriptions that the empty load prevents. That is why an account which belongs
to several communities in another client still shows nothing — a fresh install
has no `concord/*` state document, and coop has no way to ask for one.

Confirmed by inspection:

| Location | What it does |
| --- | --- |
| `crates/community/src/lib.rs:167-191` | `load` → `sync::load`, then `track(states)` |
| `crates/community/src/sync.rs:125-137` | `load` = `store::load_states` + `load_list` |
| `crates/concord/src/store.rs:314-341` | `load_states` queries `client.database()` only |
| `crates/community/src/sync.rs:139-153` | `load_list` queries `client.database()` only, `.limit(1)` |
| `crates/community/src/lib.rs:233-276` | `sync_subscriptions` skips everything when `communities` is empty |

`subscribe` itself is correct. Do not debug it.

## What the protocol actually says

Read from the spec (`concord-protocol/concord`, the submodule referenced by
accordion.chat): `02.md` §8 and `examples.md` §6.2.

A member's memberships live in the **Community List**, on relays:

- **Kind `33302`**, addressable, NIP-44-encrypted to self, signed by the
  member's real key, one event per **fragment** with `d` = the fragment index in
  decimal (`"0"`, `"1"`, …). `13302` is explicitly **retired** ("the
  single-event Community List, superseded by `33302` once it outgrew one event —
  a replaceable kind cannot fragment", `02.md:314`).
- Every 32-byte value at **any depth** is unpadded base64url, not hex. This is
  section-scoped: CORD-05 invite fields stay hex (`examples.md` §6.3).
- Join material is the membership subset — `owner, owner_salt, community_root,
  root_epoch, control_pk, channels, relays, name`, plus `control_root` when
  held. It is the *only* durable home of a member's keys.
- The two snapshots solve opposite problems: `seed` is the earliest epoch held
  (backfill anchor), `current` the latest ("so a fresh device reconstructs the
  Community instantly with no epoch-by-epoch walk"). `seed` is omitted when
  equal to `current`; embedded snapshots omit `community_id` (inherited).
- A client holds the complete List when it holds a fragment at every index below
  `frags`; it unions fragments and merges, so a partial read is safe.

Two consequences for coop:

1. **The state document is a coop invention.** `store::{save_state, load_state,
   load_states}` write kind `30078` with `d = concord/<id>`, signed by a
   per-process `LOCAL_KEYS`, and never leave the machine. No equivalent exists
   anywhere in the spec. It is a local cache and must never be treated as the
   discovery source.
2. **Discovery is: subscribe to my `33302` → materialize a community from
   `current` join material → subscribe to its planes → fold.** The fold produces
   the authoritative state; the List only supplies the keys to start.

## Divergences (coop vs spec)

| # | Spec | coop today |
| --- | --- | --- |
| 1 | kind `33302`, addressable | was `cord02::list::KIND_COMMUNITY_LIST = 13302` (retired) — **fixed in Phase A** |
| 2 | one event per fragment, `d` = index, `frags` declared | was no `frags`, single event, `d` unused, `load_list` `.limit(1)` — **fixed in Phase A** |
| 3 | 32-byte values unpadded base64url at any depth | was hex for `JoinMaterial.owner`/`control_root`, `CommunityId` serde, `ChannelGrant.key` — **fixed in Phase A** |
| 4 | `seed` omitted when equal to `current`; embedded snapshot omits `community_id`; `seed`'s cosmetic fields rewritten from `current` | was both snapshots emitted verbatim, `community_id` always present — **fixed in Phase A** |
| 5 | fetch from relays | local database only — **fixed in Phase C** |
| 6 | materialize `CommunityState` from join material | was no such path; only `CommunityState::from_genesis` — **fixed in Phase B** |
| 7 | publish the List on create/join (read-modify-write) | `build_list_event` is referenced only by tests and docs — **fixed in Phase D** |
| 8 | private channel keys ride in join material | `ChannelKeyRef` has a key field, but private planes are still not subscribed |

Divergences 1–7 are resolved. 8 remains, in the narrow sense that `planes()`
still skips private channels rather than deriving their addresses from the
granted key.

## Plan

Ordered so each phase is independently reviewable and testable. Nothing here
touches the frozen HKDF derivations or `cord01` envelope semantics.

### Phase A — make the List interoperable (pure, no I/O) — DONE

`crates/concord/src/cords/cord02/list.rs`

1. `KIND_COMMUNITY_LIST` → `33302`; add `frags: u64` to `CommunityList` and
   `is_complete(&self, frags) -> bool`.
2. Add a base64url codec for the §8 value set and apply it to every 32-byte
   field at every depth. Because `JoinMaterial` currently types `owner` and
   `control_pk` as `PublicKey` (nostr's hex serde), this needs either wire
   newtypes or `serialize_with`/`deserialize_with` helpers. Keep it local to the
   List: `cord05` stays hex.
3. Implement the two §8 MUSTs: omit `community_id` on an embedded snapshot,
   omit `seed` when it byte-equals `current`, and rewrite `seed`'s cosmetic
   fields (`name`, `relays`, each channel's `name`) from `current` on every
   serialization.
4. `build_list_event`/`parse_list_event` take the fragment index and emit/read
   the `d` tag.

Tests: round-trip the `examples.md` §6.2 payload verbatim; `merge` convergence
for two devices and mixed-age fragments; `frags` disagreement resolves to the
larger value; a repack does not shed unknown fields.

**As built.** The §8 rules live behind private wire structs (`WireList`,
`WireEntry`, `WireSnapshot`, `WireChannel`), so a writer re-encodes on every
serialization while the public types keep their internal hex/`PublicKey`
spellings and `cord05` stays hex. Three deviations from the sketch above:

- `is_complete` takes the set of fragment indices a client holds, not a count:
  a count is wrong when the indices are sparse.
- The reader tolerates non-zero base64url trailing bits. The spec's own §6.2
example has five such values, so a strict decoder rejects the worked example;
  the writer still emits the canonical spelling.
- The third omission MUST was implemented too: an entry whose `added_at` does
  not outrun its tombstone is not written. It is a serialization rule exactly
  like the other two, so it belongs here rather than in Phase D.

`parse_list_event` validates the `d` tag but returns just the `CommunityList`;
`fragment_index(event)` reads the index, which kept `sync.rs` untouched until
Phase C. `MAX_MEMBERSHIPS = 50` is kept as a stopgap (see risks): §8 has no
membership limit, and removing the cap needs write-time fragmentation.

### Phase B — materialize a community from join material (pure) — DONE

`crates/concord/src/store.rs`, `crates/concord/src/cords/cord02/list.rs`

1. `CommunityState::from_join_material(material: &JoinMaterial, added_at_ms:
   u64) -> Result<Self>`: identity/owner/salt/root/root_epoch from the material;
   `control_pks = { root_epoch → control_pk }`; `relays` parsed; `channels` from
   the grants; `control_root` when present; `heads` empty (the first control
   fold fills them); `banned` empty; `dissolved` false.
2. Carry the private channel key: add `key: Option<[u8; 32]>` to
   `ChannelKeyRef` (or a parallel map) so a grant's `key` has a home. Without
   this, a private channel is silently read-only-until-rekey.

Tests: a material with and without `control_root`; a private grant's key
survives; `from_join_material` then `planes()` yields the control `control_pk`
plus the guestbook and public channels, i.e. a subscription filter that
addresses real planes.

**As built.** `from_join_material` does not verify `community_id` against
`owner`/`owner_salt`: the List is signed by the member's own key and encrypted
to self, and the invite path already validates that binding in
`CommunityInvite::validate`. `private` on a materialized channel is simply
`key.is_some()` — the spec's `channels` carry only the Private Channel keys a
member was granted, so a grant with no key is a public channel. Nothing else
changed: `from_genesis` and `apply_fold` construct every channel with
`key: None`, and `planes()` still skips private channels, whose address derives
from the granted key rather than the `community_root`. Carrying the key is what
makes subscribing to them possible later; it is not needed to fix discovery.

Two tests. In `concord`, `from_join_material` (with and without `control_root`,
a granted key surviving, a public grant staying keyless). In `community`,
`planes()` plus `subscription_filter` over a state built field-by-field (control
+ guestbook + public channel addressed, private skipped) — `JoinMaterial` and
`ChannelGrant` cannot be constructed from `community` because their `extra`
field's type is crate-private, so the materialization and the plane derivation
are each proved where they live.

### Phase C — the List drives `load` — DONE

`crates/community/src/sync.rs`, `crates/community/src/lib.rs`

1. `subscribe_list(client, self_pk)` subscribes to `Kind::Custom(33302)`
   `author(self_pk)` under a dedicated `concord/list` subscription id, using
   `ReqTarget::auto`. With gossip enabled, `auto` breaks the filter down by
   author, so it queries the account's NIP-65 write relays and adds/connects
   them itself — bootstrap relays alone would miss a List published elsewhere.
2. `CommunityRegistry` calls `subscribe_list` once per signer (signer change and
   the initial defer). It is deliberately **not** called from `load`:
   re-subscribing on every List event would re-deliver the List and loop. `reset`
   does not unsubscribe it either — `subscribe_list` replaces the subscription
   itself, and a `reset`-issued unsubscribe could race the replacement and cancel
   discovery.
3. The notification listener routes a `concord/list` event to a new `Signal::List`,
   whose consumer re-runs `load`. Community planes keep using `Signal::Event(id)`.
4. `load_list` reads every `33302` event by `self_pk` from the database, keeps the
   newest event per fragment index, decrypts and `merge`s them. `.limit(1)` is gone.
   An incomplete List is read normally — a missing fragment is news not yet heard.
5. `load` unions two sources: every live List entry (materialized with
   `from_join_material`, or refreshed if a state document already exists) and every
   held local state the List does not mention. A held membership is dropped only
   when a tombstone outranks its `added_at_ms`; absence from the List is never a
   fact. Each list-derived state is `save_state`d, so the next `load` is warm.
6. `refresh(held, fresh)` keeps the fold's authority (`heads`, `banned`,
   `dissolved`) and the control planes it learned, and takes the List's identity,
   relays, and channel keys. Channels are merged by id rather than replaced, so a
   public channel the fold discovered is not shed by a List snapshot that predates
   it.

**As built, deviating from the sketch above.** The plan called for
`client.fetch_events(..)`; the SDK's own recommendation is to keep the request
path on a subscription and read the database. This is safer than it sounds: a
relay's event is persisted at `nostr-sdk/src/relay/inner.rs:1291` **before** the
notification is emitted, so a subscription plus a database read loses nothing and
needs no explicit save. The subscription is set up with `ReqTarget::auto` rather
than a hand-built NIP-65 relay map, because gossip already resolves the author's
write relays and connects them on demand.

Tests (no network, in `crates/community/src/sync.rs`): a membership the List
carries materializes a community even though no state document was ever written
for it, and discovery writes the document so the next load is warm; a held
membership the List never mentions is kept alongside the one it does; a tombstone
outranks a held membership and drops it; and the `concord/list` id is not read as
a community subscription. Fragment events are built with `store::list_entry` +
`CommunityList::joined` + `build_list_event` and saved straight into a memory
database, so the tests exercise the real seal/parse/merge path without a relay.

### Phase D — publish — DONE

`crates/community/src/sync.rs`, `crates/concord/src/store.rs`,
`crates/concord/src/cords/cord02/list.rs`

1. `create` mints the genesis, folds it into a state, and saves that state locally
   as before, then announces the community: the genesis wraps to its relay set,
   and the membership to the account's own List. Both publishes are best-effort —
   a relay that is down is a warning, not a failed create.
2. The List write is a read-modify-write over the copy already held (§8). `create`
   reads the newest held fragment, unions its own entry in with
   `CommunityList::joined`, builds fragment 0, and publishes it. Publishing saves
   it locally as a side effect of `send_event`, before any relay is resolved, so
   the fragment survives a relay that is down and no explicit database write is
   needed.
3. The fragment's `created_at` is `max(now, previous + 1)`, so an addressable
   relay can never quietly keep the copy the write meant to replace.

**As built, deviating from the sketch above.** Three decisions the sketch did not
cover:

- The List goes to the account's **NIP-65 write relays** (`.to_nip65()`), not the
  community's metadata relays. The List is the member's own document, and it is
  the same relay set `subscribe_list` resolves for its `author` filter — the two
  halves must agree or a write can land where nothing reads. The genesis wraps,
  which belong to the community and not the member, do go to the metadata relays.
- The entry is built by a new `store::list_entry(state, name)`. `JoinMaterial`'
  `extra` field is crate-private, so the community crate cannot build one; `name`
  is passed in because the state does not carry it — the name lives in the Control
  fold, and a created community has it in the metadata.
- A List that already spans more than one fragment is **left alone**: placing a
  new membership needs a repack (which fragment does it belong in?), and §8 allows
  a repack only against the complete List. `load` keeps a membership the List
  never mentions, so the community is still tracked locally; the remote write is
  deferred with a warning rather than performed wrongly.

Tests: `create` records a membership the List round-trips, and a second create
unions into the same document instead of replacing it.

### Phase E — verify live

`RUST_LOG=info cargo run -p coop`, sign in with the accordion account that
already belongs to communities. Expect `community {id}: subscribing to ..` and
rows in the sidebar. This is the first time the path can be exercised at all.

## Validation per phase

- `cargo test -p concord` (A, B), `cargo test -p community` (B, C, D).
- `cargo clippy --workspace --all-targets`, `cargo fmt --all -- --check`.
- A is provable against the spec's worked example, so it needs no relay.
- C is provable with `nostr-memory`: fragments are built with `build_list_event`
  and saved as the subscription would have, then `load` reads them. No relay,
  no `LocalRelay`.
- E is the only step that needs real relays.

## Risks and open decisions

- **Base64url is case-significant and coop's ids are hex everywhere else.**
  Confine the codec to `cord02::list`; any normalisation that case-folds will
  silently corrupt §8 values. **Resolved in Phase A**: the codec is private to
  `list.rs` and never case-folds.
- **`MAX_MEMBERSHIPS = 50` is not in the spec.** §8 has no membership limit; its
  only bound is the 65,536-byte *encoded event*. `fits()` still measures the
  NIP-44 plaintext, which understates that by roughly a third. Phase D kept the
  count cap and added a guard: a List that already spans more than one fragment is
  not appended to, because placing a new membership needs a repack. So a member
  with more than one fragment gets no remote write until fragmentation lands; the
  community stays local and visible.
- **Relay selection is the difference between finding the account's List and
  not.** Resolved in Phase C by `ReqTarget::auto`, whose gossip path resolves the
  filter's author to their NIP-65 write relays and connects them. A List
  published only to relays with no NIP-65 entry is still unreachable; that is a
  user-visible relay setting if it ever bites.
- **Private channels stay unsubscribed until `planes()` derives their address
  from the granted key** (Phase B gave `ChannelKeyRef` a home for it, but the
  discovery fix does not need it). Public discovery works regardless.
- **Two writers, one key.** Once coop publishes `33302`, an account used from
  both accordion and coop has both clients writing the List. §8's
  read-modify-write is what keeps that from losing memberships — it is not
  optional.
- **A create racing the first list sync can publish over an unseen List.**
  `record_membership` unions into what the local database holds, and on a fresh
  sign-in that is empty until the `concord/list` subscription has delivered. A
  create in that window writes a one-entry fragment 0, and an addressable relay
  then replaces the account's fuller List with it. The window is the ordinary
  sign-in-to-create interval, so it is small but not zero. The honest fix is to
  treat the List write as part of the sync loop — republish `list ∪ local
  memberships` whenever the subscription settles — rather than doing it inside
  `create`; an EOSE flag is not enough on its own, because an account with no
  NIP-65 relays never reaches EOSE and would then never write at all.
- **`store::save_state` signs with a per-process random key.** Harmless while it
  stays local, but it means the state document can never be published or
  compared; if a future phase wants it on the wire, it needs the account signer.
- **The deployed reference client still writes the retired kind `13302`.** The
  spec this plan implements (`concord-protocol/concord` `main`) moved the List to
  `33302` in PR #18, merged **2026-08-15**. The `applesauce` `concord` branch that
  accordion.chat builds against still declares `13302`, single-event, capped at 50
  memberships, at its head of **2026-08-05**; accordion's pin predates even that
  (`0.0.0-concord-20260804145327`). So an account whose memberships were written
  by that build stores them under a kind coop deliberately does not read, and will
  show an empty sidebar until the client is updated to the fragmented kind. This
  is not a bug in the discovery path — Phases C and D are correct against the
  current spec — but it is the first thing to check if a live sign-in still shows
  nothing. Supporting `13302` alongside `33302` is a deliberate non-goal until the
  reference client moves.
