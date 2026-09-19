# Concord backend audit and simplification plan

Audit of `crates/concord`, triggered by `CommunityRegistry` never reaching
`subscribe`: `sync::load` found zero community state documents. Tracing that
surfaced two separate things: the app only uses a fraction of the crate, and the
crate's writers take a concrete `nostr::Keys`, which the app's signer can never
produce.

Sizes: ~10,500 lines total — ~6,750 production, ~3,750 tests.

## Decisions taken

- **D1 — Keep the unwired protocol surface.** `cord05`/`cord06`/`pins`/paging
  stay in the tree for future use. No mass deletion. (Findings are recorded in
  §4 for reference only.)
- **D2 — Replace `&Keys` with a signer boundary** for account-key operations.
  Verified feasible against the pinned SDK; design in §2.

---

## 1. `&Keys` cannot be replaced by a public key — but it can be replaced by a signer

The original question was whether functions like `genesis` only need
`signer.get_public_key_async()`. They do not: they sign.

- `cord02::genesis` (`cords/cord02/mod.rs:117`) → `seal_edition` (`:711`) →
  `build_seal` (`cord01.rs:217`), which signs the seal (`.finalize(author)`,
  `cord01.rs:226`), and `wrap_seal_with` (`:247`), which signs the wrap.
- Self-addressed documents use NIP-44 to self: `seal_to_self`
  (`cord01.rs:201`) derives a conversation key from `keys.secret_key()`.

A public key can produce neither a Schnorr signature nor an ECDH key, so
"public-key-only" is impossible. The real defect is the **concrete type**: the
app holds `state::UniversalSigner` (async, possibly NIP-46), and a `nostr::Keys`
can never be conjured from it. `docs/concord-usage.md:535-536` already records
this as a deliberate migration pass.

### What the pinned SDK actually provides

Pinned rev `b230cec` (`nostr` 0.45.4 / `nostr-sdk` 0.45.2):

- There is **no `NostrSigner` trait in this revision.** The async signer surface
  is three traits, all in the `nostr` crate:
  - `AsyncGetPublicKey` — `nostr/src/key/public_key.rs:39`
  - `AsyncSignEvent` — `nostr/src/event/mod.rs:366`
  - `AsyncNip44` — `nostr/src/nips/nip44/traits.rs:30`
- `Keys` implements all three (`nostr/src/key/mod.rs:298,309,342`), so tests and
  local key holders keep working.
- `UniversalSigner` already implements all three with
  `Error = UniversalSignerError` (`crates/state/src/signer.rs:148-191`).
- SDK helpers accept them:
  - `EventBuilder::finalize_async` — `S: AsyncGetPublicKey + AsyncSignEvent + ?Sized`
    (`nostr/src/event/builder.rs:171-193`)
  - `GiftWrapBuilder::finalize_async` — `S: AsyncGetPublicKey + AsyncSignEvent + AsyncNip44`
    (`nostr/src/nips/nip59.rs:334-355`)
  - `UnwrappedGift::from_gift_wrap_async` — `T: AsyncNip44` (`nip59.rs:84-90`)

So the answer is yes: pass a signer. `UniversalSigner` works as-is.

### Per-function bounds, not a bundle

Each function should request only the capabilities it uses. The SDK itself is
designed this way (`UnsignedEvent::finalize_async` takes only `AsyncSignEvent`,
`EventBuilder::finalize_async` takes `AsyncGetPublicKey + AsyncSignEvent`,
NIP-59 takes all three).

| Operation | Bounds |
| --- | --- |
| Sign a seal/edition/rekey wrap, author already known | `AsyncSignEvent` |
| Build an event where the author comes from the signer | `AsyncGetPublicKey + AsyncSignEvent` |
| To-self documents (Community List, Invite List) | `AsyncGetPublicKey + AsyncNip44`, plus `AsyncSignEvent` when the document is itself an event |
| Decrypt-only (`parse_list_event`, `unwrap_direct_invite`) | `AsyncNip44` |
| Rekey blob encrypt (`build_blob`) | `AsyncGetPublicKey + AsyncNip44` (no signing) |
| Rekey blob open (`open_blob`) | `AsyncNip44` |
| Direct invite build (`GiftWrapBuilder`) | all three |

Use generics (`S: AsyncSignEvent + ?Sized`), never `&dyn`: the traits carry
associated `Error` types, so `dyn AsyncSignEvent` would force the concrete error
at every call site (`dyn AsyncSignEvent<Error = UniversalSignerError>`),
defeating the abstraction. The SDK uses generics throughout for this reason.

Do **not** define a supertrait bundle
`trait Signer: AsyncGetPublicKey + AsyncSignEvent + AsyncNip44 {}`: all three
supertraits declare an associated `Error`, so `Self::Error` becomes ambiguous,
and the bundle forces NIP-44 onto purely-signing callers (and vice versa).

Inside concord, replace `builder.finalize(&keys)` with
`builder.finalize_async(signer).await`. `finalize_async` fetches the signer's
public key and uses it as the event author, exactly as `finalize` did, so the
bytes are unchanged for every caller that passes a matching signer.

We deliberately **do not** pre-check the author against `rumor.pubkey` inside
`build_seal`. The seal's author is the signer's own public key, matching the old
`finalize` semantics; a signer that does not match the rumor is still caught by
`open_wrap_at` as `AuthorMismatch` (`cord01.rs:328`). Pre-checking would also
make it impossible to construct the hostile seals the cord suite relies on as
test vectors (`cord01.rs` `hostile_wraps_are_dropped_in_order`).

If the repeated `<S as ...>::Error: Error + Send + Sync + 'static` bounds
become too noisy, the only stable-Rust way to shorten them is an owned
error-erased trait (as the app already does with
`crates/state/src/signer.rs:64-138`). That trades precision for brevity; keep
per-function bounds unless the noise proves unmanageable.

### What must NOT go through the signer

- **Group-key NIP-44.** `cord01::{seal_bytes, open_bytes, wrap_seal,
  wrap_seal_with, rewrap_seal}` encrypt under a `ConversationKey` derived from
  HKDF group secrets. `AsyncNip44` can only ECDH against a public key, so group
  encryption stays on `ConversationKey` / `GroupKey::keys()`.
- **Wrap signatures.** Wraps are signed by the derived group signer key
  (`GroupKey::keys()`), not the account.
- **Locally held raw secrets.** `cord05::{build_bundle_event, build_revocation}`
  take a generated `link_signer` whose secret the app stores as
  `signer_sk` (`docs/concord-usage.md:306-321`). `&Keys` is correct there; the
  app has the secret itself.
- **Local database artifacts.** `store::{cache_rumor, save_state}` sign with the
  internal random `LOCAL_KEYS` (`store.rs:18`). No user signer involved.

### Call-site inventory

Account-key sites to migrate:

| Site | Today | After | Bounds |
| --- | --- | --- | --- |
| `cord02::genesis` (`cord02/mod.rs:117`) | `owner: &Keys` | `owner: &S` | `AsyncGetPublicKey + AsyncSignEvent` |
| `ControlWriter::{publish, set_*}` (`cord02/mod.rs:214-425`) | `keys: &Keys` | `keys: &S` | `AsyncGetPublicKey + AsyncSignEvent` |
| `seal_edition` (`cord02/mod.rs:711`, internal) | `owner: &Keys` | `owner: &S` | `AsyncGetPublicKey + AsyncSignEvent` |
| `cord01::build_seal` (`cord01.rs:217`) | `author: &Keys` | `author: &S` | `AsyncGetPublicKey + AsyncSignEvent` |
| `cord01::{seal_to_self, open_to_self}` (`:201,209`) | `keys: &Keys` | `&S`, async | `AsyncGetPublicKey + AsyncNip44` |
| `guestbook::seal_rumor` (`guestbook.rs:186`) | `author: &Keys` | `author: &S` | `AsyncGetPublicKey + AsyncSignEvent` |
| `cord03::seal_rumor` (`cord03.rs:295`) | `author: &Keys` | `author: &S` | `AsyncGetPublicKey + AsyncSignEvent` |
| `list::build_list_event` (`list.rs:186`) | `keys: &Keys` | `keys: &S` | all three |
| `list::parse_list_event` (`list.rs:197`) | `keys: &Keys` | `keys: &S` | `AsyncGetPublicKey + AsyncNip44` |
| `cord05::{build_direct_invite, unwrap_direct_invite}` (`:451,478`) | `inviter`/`recipient: &Keys` | **done** | build: all three (`Sized`); unwrap: `AsyncNip44` (`Sized`) |
| `cord05::{build_invite_list, parse_invite_list}` (`:593,604`) | `keys: &Keys` | **done** | build: all three; parse: `AsyncGetPublicKey + AsyncNip44` |
| `cord06::build_blob` (`:302`) | `rotator: &Keys` | **done** | `AsyncGetPublicKey + AsyncNip44` |
| `cord06::open_blob` (`:319`) | `recipient: &Keys` | **done** | `AsyncNip44` |
| `cord06::{build_rekey_chunks, seal_dissolved}` (`:602,737`) | actor `&Keys` | **done** | `AsyncGetPublicKey + AsyncSignEvent` |

Leave unchanged: `cord05::{build_bundle_event, build_revocation}`, all
`cord01` wrap functions, `GroupKey::keys()`, `store::LOCAL_KEYS`.

### Async ripple and tests

Every migrated function becomes `async`. `smol` is already a dev-dependency of
concord (`crates/concord/Cargo.toml:21-23`), so affected `#[test]`s become
`smol::block_on(...)` wrappers. The app's call sites are already async
background tasks.

### Known constraints

- `GiftWrapBuilder::finalize_async` and `UnwrappedGift::from_gift_wrap_async`
  are generic over `S: Sized` (no `?Sized`), so those functions must stay
  generic, never `&dyn`.
- Every converted `S::Error` must be `Error + Send + Sync + 'static` for the
  SDK helpers' `Error::other` (`nostr/src/error.rs:100-105`) and for
  `anyhow`; `Keys::AsyncGetPublicKey::Error = Infallible`,
  `Keys::AsyncSignEvent::Error = nostr::Error`, `UniversalSignerError`
  (`crates/state/src/signer.rs:10-32`) all qualify.
- `AsyncGetPublicKey` is worth requiring alongside `AsyncSignEvent` wherever the
  author is embedded in the payload: `sign_event` signs the id of the given
  unsigned event without rewriting its pubkey, so a mismatched signer is only
  caught later by signature verification.

---

## 2. Migration plan

### Phase 1 — replace `&Keys` with per-function signer bounds (no behavior change) — DONE

1. No new module: change the signatures listed in the inventory table to
   generics over the SDK traits (`S: AsyncGetPublicKey + AsyncSignEvent`,
   `S: AsyncSignEvent`, `S: AsyncGetPublicKey + AsyncNip44`, or `S: AsyncNip44`).
2. Migrate the live path only: `cord01::build_seal`, `cord01::{seal_to_self,
   open_to_self}`, `seal_edition`, `genesis`, `ControlWriter`, `guestbook::
   seal_rumor`, `cord03::seal_rumor`, `list::{build,parse}_list_event`.
3. Update `docs/concord-usage.md` examples to take a signer.
4. Update concord tests to `smol::block_on`; `&Keys` keeps working because it
   implements all three traits.

Validation: `cargo test -p concord` — 46 passed, 0 failed. The one behavior
change from the plan sketch is the dropped up-front author check in §1.
`cord01::{seal_to_self, open_to_self}` now take `&str` and return `String`
(NIP-44 is UTF-8 text), so the `list` and invite-list callers read the plaintext
with `serde_json::from_str`.

**Unplanned but forced:** `cord01::{build_seal, seal_to_self, open_to_self}` are
shared helpers, so the unwired callers had to be migrated in the same pass to
keep the crate compiling: `cord05::{build_invite_list, parse_invite_list}` and
`cord06::{build_rekey_chunks, seal_dissolved}` (Phase 3's mechanical part).
`cord05::{build_direct_invite, unwrap_direct_invite}` and
`cord06::{build_blob, open_blob}` were untouched by Phase 1 — they use the NIP-59
and group-key paths, not the migrated helpers — and were migrated in Phase 3.

### Phase 2 — app uses the signer — DONE

1. `sync::create(client, signer, metadata)` (`crates/community/src/sync.rs`) runs
   `cord02::genesis`, opens the genesis editions, persists the state with
   `store::save_state`, and also stores the genesis wraps so the control plane
   folds locally. It is generic over `S: AsyncGetPublicKey + AsyncSignEvent + ?Sized`
   (the bounds `genesis` needs and no more, per D2); the app passes its
   `UniversalSigner`, so no secret material is exposed and NIP-46 accounts work
   too.
   `CommunityRegistry::create(metadata, cx)` (`crates/community/src/lib.rs`) is
   the GPUI wrapper: it refuses when no account is signed in, otherwise runs the
   task off-thread and refreshes tracking, so `sync::load` now returns one state
   and `subscribe` finally fires.
2. The app-side reimplementation of `list::parse_list_event`
   (`crates/community/src/sync.rs:154-156`) is deleted; `load_list` calls the
   real `cord02::list::parse_list_event`.
3. Relays in the metadata are persisted but the genesis is **not** published yet;
   `create` is local-only. Wiring genesis/broadcast through the relay pool is the
   next app step, not part of this phase.

Validation: `cargo test -p community` (1 passed), `cargo test -p concord`
(46 passed), `cargo clippy -p community --all-targets`, `cargo fmt -p community
--check`, and `cargo check --workspace --all-targets` are all clean.

**Deviation from the plan sketch:** the planned "drive `CommunityRegistry`" test
is instead a `sync`-layer test, `sync::tests::
creating_a_community_persists_a_state_that_subscribes_and_folds`. A GPUI-level
test cannot construct a `NostrRegistry` — it opens LMDB at `config_dir()` and
connects bootstrap relays in `NostrRegistry::new`, which is private and not
injectable — so the test drives a `Client` on an in-memory database
(`nostr-memory`, already a dev-dependency) directly. It asserts the whole
contract the registry depends on: `create` persists a state `load` returns, the
subscription filter addresses the genesis wraps, `fold` yields the created
community, and an inbound control edit folds over it.

### Phase 3 — migrate the remaining unwired writers — DONE

`cord05::{build_direct_invite, unwrap_direct_invite}` and
`cord06::{build_blob, open_blob}` now take a signer. The NIP-59 pair keeps a
`Sized` `S` (`AsyncGetPublicKey + AsyncSignEvent + AsyncNip44` to build,
`AsyncNip44` to unwrap) because the SDK's `GiftWrapBuilder::finalize_async` and
`UnwrappedGift::from_gift_wrap_async` are `Sized`-bounded. The blob pair is
`AsyncGetPublicKey + AsyncNip44` to build and `AsyncNip44` to open, with `?Sized`.

The blobs forced one behavior change, because a signer's NIP-44 is text-only
(`nip44_encrypt_async(public_key, &str)`) while the blob plaintext is a
fixed-width binary record. `build_blob` now carries that record base64-encoded
inside the NIP-44 envelope and `open_blob` decodes it again. The record layout,
the `locator`, and the envelope are unchanged; only the bytes inside the envelope
differ. There are no golden vectors for blobs and no producer or consumer other
than these two functions, so the round-trip stays self-consistent; cord06 remains
unwired and persists nothing.

Validation: `cargo test -p concord` — 46 passed, 0 failed (the 80-blob
`a_full_send_chunk_stays_within_a_relay_event` size assertion still holds under
the base64 record). `cargo clippy -p concord --all-targets` and
`cargo fmt -p concord --check` are clean.

### Phase 4 — duplication and hygiene (independent, low risk) — DONE

1. DONE — `store::load_states(client)` added (with a direct `store` test), the
   app-side state-document scan in `sync::load` is gone.
2. DONE — `store::STATE_PREFIX` is public; the app-side `concord/` literals are
   gone, and subscription ids reuse the exported prefix.
3. DONE — the shared rumor tag readers and error live in a new `cords::rumor`
   module (`RumorError`, `tag`, `required`, `value`, `pubkey`,
   `optional_citation`), re-exported as `cord03::ChatError` and
   `cord02::guestbook::GuestbookError`. `cord06` keeps its own narrower
   `RekeyError`, which the plan scoped out.
4. RETAINED — none of the "never-varied parameters" were removed. Each is
   load-bearing for a flow the fold or a writer already implements (D1):
   - `complete_memberlist`'s `banned_at` is read by the fold and is exercised
     with a non-empty map by `join_leave_kick_and_snapshot_converge_to_one_memberlist`;
     `docs/concord-usage.md` already promises to fill it once the banlist head's
     timestamp is plumbed through.
   - `cache_rumor -> Result<bool>` is read by `backfill` to drop expired rumors.
   - `coalesce`'s `snapshot_authority` gates which snapshots apply; passing
     `None` today is a policy, not a dead parameter.
   - `seal_rumor(ephemeral)` and the `until` cursors on `backfill`/`query_rumors`
     select protocol modes and paging.
5. DONE — tightened `cord04` visibility: `edition_hash`, `fold`, `FoldResult`,
   `bootstrap_head`, `parse_banlist`, `Role::parse` and `Grant::parse` are no
   longer `pub`. `HeadSelection` stays `pub` because the public `fold_head`
   returns it.
6. DONE — doc drift fixed: the store takes `&Client` throughout (including
   `load_state`/`load_states`/`query_rumors`, not just the writers), `backfill`
   arity, `set_pin_list`'s missing `.await`, the GPUI `init` signature and
   registry names, and the "Not wired up yet" registry bullet.

---

## 3. Retained-by-decision surface (reference only)

Per D1 these stay, but they should be understood as unwired, not live:

| Module | Approx. prod LOC | App use |
| --- | --- | --- |
| `cord06` rotation/refounding/dissolution | ~850 | none |
| `cord05` invites/links/direct/list | ~650 | none (types only, via unused `list::join_material`) |
| `cord04::pins` | ~550 | none |
| `cord03` write path + `fold` + `plane_keys` | ~340 | only `open` / `expiration_of` |
| guestbook / list write paths | ~240 | `open`, `coalesce`, `complete_memberlist`, `is_live` |
| `store` paging / purge / query / load_state(s) | ~180 | `cache_rumor`, `save_state`, `load_states` |

Truly unreferenced even by tests (safe candidates, but kept per D1):
`CommunityInvite::expired`, `GroupKey::pk_hex`, `From<[u8; 32]>` impls,
`CommunityRoles::{roles, is_empty}`.

---

## 4. Non-goals

- No mass deletion of unwired modules (D1).
- No changes to frozen HKDF derivations, locators, golden vectors, or `cord01`
  envelope semantics. The one exception Phase 3 forced is the blob plaintext
  encoding (base64 inside the envelope, see Phase 3); the blob record layout and
  `locator` are untouched.
- No group-key encryption through the signer.
- Tests move only alongside the code they cover.

## 5. Validation

- `cargo test -p concord` after each phase; `cargo test --workspace` before
  landing.
- Phase 1 is behavior-preserving: the existing cord test suite is the oracle.
- Phase 2 adds the app-level test: seed a `CommunityState` via
  `store::save_state`, drive `CommunityRegistry`, assert a subscription is made
  and an inbound wrap folds into the community.
- Phase 4: `cargo test -p concord -p community` (47 + 1 passed),
  `cargo clippy -p concord -p community --all-targets`, and
  `cargo fmt -p concord -p community --check` are all clean.

## 6. Immediate unblock

Option 2 (the clean path, using `UniversalSigner`) landed in Phase 2. Option 1
(exposing the local `Keys` from `crates/state/src/lib.rs:254`) is obsolete.
