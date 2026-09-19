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
2. **Discovery is: fetch my `33302` from relays → materialize a community from
   `current` join material → subscribe to its planes → fold.** The fold produces
   the authoritative state; the List only supplies the keys to start.

## Divergences (coop vs spec)

| # | Spec | coop today |
| --- | --- | --- |
| 1 | kind `33302`, addressable | `cord02::list::KIND_COMMUNITY_LIST = 13302` (retired) |
| 2 | one event per fragment, `d` = index, `frags` declared | no `frags`, single event, `d` unused, `load_list` `.limit(1)` |
| 3 | 32-byte values unpadded base64url at any depth | hex: `JoinMaterial.owner`/`control_root` (`PublicKey`/`String`), `CommunityId` serde, `ChannelGrant.key` |
| 4 | `seed` omitted when equal to `current`; embedded snapshot omits `community_id`; `seed`'s cosmetic fields rewritten from `current` | both snapshots always serialized verbatim; `community_id` always present |
| 5 | fetch from relays | local database only |
| 6 | materialize `CommunityState` from join material | no such path; only `CommunityState::from_genesis` |
| 7 | publish the List on create/join (read-modify-write) | `build_list_event` is referenced only by tests and docs |
| 8 | private channel keys ride in join material | `ChannelKeyRef` has no key field |

Divergences 1–4 meant that even if the fetch existed, coop could neither read
what accordion wrote nor write something accordion could read. **Phases A and B
are done**, so 1–4 and 6 are resolved; 5, 7 and 8 remain (8 only in that private
planes are still not subscribed).

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

### Phase C — fetch the List from relays, then load

`crates/community/src/sync.rs`

1. `load` becomes:
   - resolve where to ask: the account's NIP-65 write relays (kind `10002`) plus
     the pool's connected relays. If only the app's bootstrap relays are queried,
     a List published by another client (e.g. accordion on `relay.damus.io` /
     `nos.lol`) will simply not be found.
   - `client.fetch_events(Filter::new().kind(33302).author(self_pk))` — one
     filter returns every fragment. Fetched events are persisted by the client
     (`nostr-sdk/src/relay/inner.rs:1291`), so the database read stays valid.
   - merge fragments → `CommunityList`.
   - for each entry whose `is_live(&id)`: if a state document exists, keep its
     `heads` (the fold's authority) and refresh relays/keys from `current`;
     otherwise `from_join_material(..)`.
   - `store::save_state` each result so the next `load` is warm.
2. `load_list` keeps reading `client.database()` — after the fetch it is
   populated. It must stop using `.limit(1)`.
3. Drop the `states.retain(..)` shape: the List is now the *source* of states,
   not just a filter over local ones. A local state whose membership is
   tombstoned is still dropped, but a List entry with no local state now
   produces one.

Tests (no network, `nostr-memory`): a `33302` fragment written by the account is
discovered with **no** state document present; a tombstoned id is dropped; a
missing fragment leaves the rest usable. A `nostr_sdk::local_relay::LocalRelay`
(in-process relay, public in this pinned revision) can drive the real
fetch/subscribe path end to end.

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
- C is provable with `nostr-memory` + `LocalRelay`, so it needs no network.
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
- **Relay selection for the fetch is the difference between finding the account's
  List and not.** NIP-65 write relays + pool, or a user-visible relay setting?
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
