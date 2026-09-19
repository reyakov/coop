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
| 1 | kind `33302`, addressable | `cord02::list::KIND_COMMUNITY_LIST = 13302` (retired) |
| 2 | one event per fragment, `d` = index, `frags` declared | no `frags`, single event, `d` unused, `load_list` `.limit(1)` |
| 3 | 32-byte values unpadded base64url at any depth | hex: `JoinMaterial.owner`/`control_root` (`PublicKey`/`String`), `CommunityId` serde, `ChannelGrant.key` |
| 4 | `seed` omitted when equal to `current`; embedded snapshot omits `community_id`; `seed`'s cosmetic fields rewritten from `current` | both snapshots always serialized verbatim; `community_id` always present |
| 5 | fetch from relays | local database only — **fixed in Phase C** |
| 6 | materialize `CommunityState` from join material | no such path; only `CommunityState::from_genesis` |
| 7 | publish the List on create/join (read-modify-write) | `build_list_event` is referenced only by tests and docs |
| 8 | private channel keys ride in join material | `ChannelKeyRef` has no key field |

Divergences 1–4 meant that even if the fetch existed, coop could neither read
what accordion wrote nor write something accordion could read. **Phases A, B and
C are done**, so 1–6 are resolved; 7 and 8 remain (8 only in that private planes
are still not subscribed).

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
`fragment_index(event)` reads the index, which keeps `sync.rs` untouched until
Phase C. `MAX_MEMBERSHIPS = 50` is kept for now as a stopgap (see risks): §8 has
no membership limit, and Phase D's fragmentation is what removes the cap.

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

Tests (no network): a fragment in the database with **no** state document
materializes a community and writes one; a tombstone at `u64::MAX` drops a held
membership; a two-fragment List with only fragment 0 delivered still yields its
membership; a held membership the List never mentions is kept alongside the
discovered one; `refresh` keeps `heads`/`banned`/`dissolved` and both control
planes while taking the List's keys; and the `concord/list` id is not read as a
community subscription. Fragment events are built with `build_list_event` from a
§8 JSON payload, so the test exercises the real decrypt-and-merge path without a
relay.

### Phase D — publish

`crates/community/src/sync.rs`, `crates/concord/src/store.rs`

1. `create` appends to the List and publishes the fragment read-modify-write per
   §8, targeting the metadata's relays.
2. `create` publishes the genesis wraps to those relays. Today it only
   `client.database().save_event(wrap)`s, so a created community is invisible to
   every other account.
3. Leave uses a tombstone; a repack requires the complete List and is a
   non-goal until memberships outgrow one fragment.

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
  NIP-44 plaintext, which understates that by roughly a third, so the count cap is
  kept as a conservative stopgap until Phase D measures the built event and
  fragments on write.
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
- **`store::save_state` signs with a per-process random key.** Harmless while it
  stays local, but it means the state document can never be published or
  compared; if a future phase wants it on the wire, it needs the account signer.
