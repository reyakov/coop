# NIP-60: cashu wallet — implementation plan

Spec: https://github.com/nostr-protocol/nips/blob/master/60.md
CDK: https://github.com/cashubtc/cdk (crates `cdk`, `cdk-redb`, `cdk-nostr`, from git main)

NIP-60 stores a user's cashu wallet state on relays: the wallet config
(kind `17375`), its unspent proofs (kind `7375`), and an optional spending
history (kind `7376`). All content is NIP-44 encrypted to self. This plan
implements it inside coop using the CDK for all cashu/mint operations and
the app's existing nostr client/signer for events.

## Analysis

| Kind   | Replaceable | Content (NIP-44 to self)                        | Purpose                          |
| ------ | ----------- | ----------------------------------------------- | -------------------------------- |
| 17375  | yes         `[[privkey, hex], [mint, url], ...]`            | wallet config: P2PK key + mints  |
| 7375   | no          `{mint, unit, proofs[], del[]}`                 | unspent proofs (per mint)        |
| 7376   | no          `[[direction], [amount], [unit], [e, ...]]`     | spend/receive history (optional) |
| 7374   | no          `quote-id` + `mint`/`expiration` tags           | pending mint quote (optional)    |
| 10019  | yes         empty, `relay` tags                             | wallet relay list                |

Key mechanics:

- Wallet state transitions are rollover: spending part of a token event
  publishes a new `7375` with the unspent proofs + `del: [old-event-id]`,
  then NIP-09 deletes the old event (the delete MUST carry `["k", "7375"]`).
- The `privkey` in `17375` is a wallet-only key (never the nostr identity
  key). It exists to unlock P2PK-locked ecash, i.e. redeeming NIP-61
  nutzaps.
- NIP-60 itself does not move money to/from others. Receiving ecash from
  peers is NIP-61 (nutzaps): gift-wrapped token events P2PK-locked to the
  wallet pubkey. Redemption is `receive` + a `7376` with an `e` tag
  marked `redeemed` (this one tag stays unencrypted).
- History (7376) and quote events (7374) are optional; the spec recommends
  local state for quotes. We publish 7376, skip 7374 (cdk keeps quotes in
  its local DB).

## How it fits coop

Follows the existing crate patterns exactly; no new architecture.

- One protocol crate: `crates/nip60`, mirroring `nip4e`/`nip29` — a
  registry entity set as GPUI global by `init(cx)`, `EventEmitter` for
  UI updates, `SmallVec<Task>` + `SmallVec<Subscription>` fields, flume
  signal channel for background results, reset/reload on
  `NostrRegistry::signer_changed` (see `crates/nip29/src/lib.rs`).
- Desktop-only: crate-level `#![cfg(not(target_arch = "wasm32"))]` like
  `crates/auto_update/src/lib.rs`. Wallet UI is only wired on desktop.
- Tokio bridge: all cdk calls run through `gpui_tokio::Tokio::spawn(cx, ...)`
  (runtime is already initialized by `state::init`). The `cdk-nostr`
  inbox listener also runs on that runtime.
- Signer-agnostic encryption: the signer may be a bunker/extension, so we
  never use `nostr::nips::nip60` prepare helpers (they need an owned
  `SecretKey`).
  We build payloads as plain JSON, encrypt with
  `signer.nip44_encrypt(self_pubkey, json)` and sign with
  `EventBuilder::new(kind, content).finalize_async(signer)` — the same
  pattern as `crates/nip4e/src/storage.rs`. Parsing is decrypt-then-parse:
  `TokenEvent` derives `Deserialize`; `17375` content is a trivial
  `Vec<Vec<String>>` parse.
- Existing facts we build on:
  - rust-nostr (git, already a dependency) ships `nostr::nip60` with the
    event data types (`WalletEvent`, `TokenEvent`, `SpendingHistory`,
    `QuoteEvent`, `CashuProof`) and kinds
    (`Kind::CashuWallet` = 17375, `CashuWalletUnspentProof` = 7375,
    `CashuWalletSpendingHistory` = 7376, `CashuWalletQuote` = 7374).
    It is behind the `nip60` feature, which coop does not enable yet.
  - `state::NostrRegistry` gives the `Client` (LMDB cache + bootstrap
    relays + gossip) and the `UniversalSigner`.
  - Local file storage precedent: `NostrLmdb::open(config_dir().join("nostr"))`
    in `crates/state/src/lib.rs`; cdk's sqlite store goes to
    `config_dir().join("cashu")`.

## Dependencies

Workspace `Cargo.toml` (final state after the phase 0 spike):

```toml
nostr = { git = "https://github.com/rust-nostr/nostr", features = [ "nip59", "nip49", "nip44", "nip60", "nip98" ] }

# Cashu (NIP-60). From git: the published 0.18.x line targets nostr 0.44 and
# cannot unify with the git rust-nostr used here, while main targets 0.45.x
cdk = { git = "https://github.com/cashubtc/cdk", default-features = false, features = [ "wallet" ] }
cdk-redb = { git = "https://github.com/cashubtc/cdk", default-features = false, features = [ "wallet" ] }
cdk-nostr = { git = "https://github.com/cashubtc/cdk", default-features = false }

[patch.crates-io]
nostr = { git = "https://github.com/rust-nostr/nostr" }
nostr-sdk = { git = "https://github.com/rust-nostr/nostr" }
```

Spike findings (all resolved and verified with `cargo check`):

- The cdk family hard-pins `cdk-common` with `=` per release, so all crates
  must come from one release train. crates.io 0.18.3 pins nostr 0.44 and
  cannot coexist with coop's git nostr 0.45.x; the git main line pins
  nostr ^0.45.3, which the `[patch.crates-io]` block redirects onto the
  same git rust-nostr checkout. The lockfile ends with exactly one `nostr`
  and one `nostr-sdk`, proven by compile-time type-identity checks between
  `nostr` and `cdk_nostr::nostr`.
- Storage is `cdk-redb` (`WalletRedbDatabase::new(&Path)`, pure Rust), not
  `cdk-sqlite`: published cdk-sqlite pins rusqlite ^0.31, whose
  `libsqlite3-sys` clashes at the `links` level with the `nostr-sqlite`
  wasm dependency of the `state` crate.
- rust-nostr 0.45.x has no root re-exports: the module is
  `nostr::nips::nip60`, and cdk-nostr re-exports the SDK as
  `cdk_nostr::nostr` / `cdk_nostr::nostr_sdk`.
- The `nip60` crate exists (`crates/nip60`) with a compile-only spike in
  `lib.rs` and a no-op `init(cx)` wired into `desktop/src/main.rs`; both
  are replaced in phase 1.

## Design

### Key material

- On first `setup(mint_url)` (explicit user action, no auto-creation):
  generate `SecretKey::generate()` as the wallet key, publish `17375`
  containing it + the mint list (encrypted to self, signed by the user).
- The cdk seed is derived deterministically from the wallet key
  (`HKDF-SHA256(wallet_privkey, info = "coop cashu seed")` -> `[u8; 64]`).
  No extra local storage, and a fresh device recovers the full wallet from
  the relay state alone. Tradeoff: the derivation is coop-specific (NIP-60
  does not standardize seed derivation), so it does not restore into other
  apps — acceptable, see open questions.
- Proof-level secrets/counters live in cdk's sqlite store; NUT-13 restore
  from mints works because the seed is deterministic.

### Local state

- cdk redb store at `config_dir().join("cashu")` (one `WalletDatabase`
  shared by all mint wallets).
- `WalletRegistry` holds: parsed `17375` (`WalletEvent` + event id), the
  active `7375` events per mint (event id + `TokenEvent`), cdk `Wallet`
  per mint (`CurrencyUnit::Sat` only in v1), and the subscription handle.

### Relay sync

- One subscription: `authors: [me], kinds: [17375, 7375, 7376]` plus
  kind `5` with `k: 7375`, on the existing client (bootstrap relays).
  Notifications flow through the nip29-style flume channel.
- Published relays: the same client/relays coop already uses. Publishing a
  `10019` wallet relay list with those relays is a one-liner for interop;
  reading `10019` from other clients is skipped in v1 (coop always uses
  its own relay set).

### Operations

Each op runs on tokio and ends with the same `sync` tail:
read unspent proofs from cdk -> publish one `7375` per changed mint
(rolling up previous event ids into `del`) -> NIP-09 delete old events with
a `k:7375` tag -> publish `7376` history -> update registry state and emit.

- `setup(mint_url)`: create wallet key + seed, publish `17375`, build cdk
  `Wallet`, open subscription.
- `create_mint_quote(amount)`: `wallet.mint_quote` -> return invoice to UI;
  poll / subscribe quote state, then `wallet.mint(quote_id)` -> sync.
  On load, `mint_unissued_quotes` claims quotes paid elsewhere.
- `melt(invoice)`: `wallet.melt` -> sync (change proofs roll over).
- `send_token(amount)`: `wallet.send` -> return token string for the user
  to share -> sync. (No in-chat token delivery in v1.)
- `receive_token(token)`: `wallet.receive` -> sync.
- Redeem nutzap (NIP-61): verify the gift-wrapped proofs are P2PK-locked to
  the wallet pubkey, then `wallet.receive_proofs(proofs, ReceiveOptions {
  p2pk_signing_keys: [wallet_key] })` -> `7376` with unencrypted
  `e <nutzap-id> "" redeemed` tag -> sync.
- Remote `7375` from another device: import its unspent proofs into the
  cdk store as the new local truth (cdk proof insertion + state check via
  `check_proofs_spent`); superseded `7375`s reconcile through `del`/NIP-09.
- On every load: `recover_incomplete_sagas` and `mint_unissued_quotes`
  (required by cdk to clear interrupted operations), then re-derive the
  unspent set from `7375` minus deletes.

### Crate layout (3 files, kept small on purpose)

- `lib.rs` — `init`, `WalletRegistry`, `WalletEvent` (`Updated`, `Error`),
  subscription/notification handling, `reset` on signer change.
- `keys.rs` — wallet key/seed derivation, `17375` build/parse via signer.
- `ops.rs` — cdk operations + the shared `sync` tail (rollover, deletes,
  history), NIP-61 redemption.

Wiring: `nip60::init(cx)` in `desktop/src/main.rs` after `nip4e::init(cx)`;
`nip60 = { path = "../crates/nip60" }` added to `desktop/Cargo.toml` (and
the UI crate when Phase 3 lands).

## Phases

### Phase 0 — dependency spike (done)

Outcome recorded in the Dependencies section: cdk family on git main,
`[patch.crates-io]` unification, cdk-redb storage, `nip60` crate skeleton
wired into desktop. `cargo check -p nip60` and the full desktop check pass.

The remaining spike code in `crates/nip60/src/lib.rs` is deleted in
phase 1.

### Phase 1 — bootstrap + load + balance (done)

Implemented in `crates/nip60` (`lib.rs`, `keys.rs`):

- `WalletRegistry` follows the nip29/nip4e pattern: global entity, signer
  reset, flume notification listener, task + subscription SmallVecs.
- `keys.rs`: wallet key generation, HKDF seed derivation, `17375`
  encrypt/parse through the signer (works with bunkers), unit tests.
- Load: subscribe + query cache for 17375/7375/7376 and kind 5 `k:7375`,
  decrypt via signer, prune deleted (`k:7375` deletes) and superseded
  (`del` chains) token events, expose `has_wallet()`/`balance()`.
- cdk: redb store per wallet pubkey under `config_dir()/cashu`, one `Wallet`
  per mint (`sat`), unspent proofs imported/diffed against the store on
  every sync, `recover_incomplete_sagas` + `mint_unissued_quotes` fire on
  first store open.
- `setup(mint)` creates the wallet (first mint) or adds a mint, publishing
  `17375` and saving it to the local cache.

Deferred to later phases: token rollover publishing (phase 2), remote
proof validation at the mint (phase 2 reconciliation), replacing proofs
reserved by local in-flight operations (phase 2 guard).

### Phase 2 — moving money (done)

Implemented in `crates/nip60/src/ops.rs` plus registry wiring:

- All store mutations go through a serialized job queue
  (`schedule_job`): one job at a time on the background executor,
  snapshots taken at schedule time, one queued job slot. This keeps ops
  and relay-driven imports from interleaving on the store, which a loose
  sync tail would not guarantee.
- Jobs: `Reconcile` (claim paid quotes via `mint_unissued_quotes`, then
  converge each mint — publish the store's unspent set when it diverges
  from the relay view, and import relay-only proofs into the store),
  `ClaimQuote`, `Melt`, `SendToken`, `Receive`.
- The sync tail (`sync_token_events`) publishes a fresh `7375` with the
  store's unspent proofs, NIP-09-deletes the previous token events with
  the required `k: 7375` tag, and publishes a `7376` history event with
  `created`/`destroyed` markers.
- Registry API: `create_mint_quote(amount)` (invoice stored in
  `mint_quotes()`, UI polls `claim_mint_quote(id)`), `melt(invoice)`,
  `send_token(amount)` (token delivered via `WalletEvent::TokenCreated`),
  `receive_token(token)` (mint resolved from the token).
- Claim-on-load moved from a fire-and-forget task into `Reconcile`:
  the fire-and-forget variant raced the import pass, which could wipe
  freshly claimed proofs from the store before they were published.
- History amounts: mint/receive record what was received; melt records
  invoice amount + fee (`FinalizedMelt::total_amount`); send records the
  sent amount.

Deferred: NUT-20/quote events (`7374`, per spec local state is fine),
sending tokens in-chat (v1 copies the token string).

### Phase 3 — NIP-61 redemption (cdk-nostr)

1. `cdk_nostr::NostrInbox` on the wallet key + relays, started in
   `Tokio::spawn`, stopped on reset; rumors parsed into proofs + mint.
2. Redemption path with `p2pk_signing_keys`, `redeemed` history tag.

### Phase 4 — UI

- Settings entry for the mint URL (`settings` crate pattern).
- Minimal wallet surface in the workspace (balance, receive invoice,
  paste-token receive, send-token, history list) subscribing to
  `WalletEvent`. Follows `nip29_ui`'s split if it grows beyond a panel.

## Tests

- `keys.rs` round-trips: encrypt/decrypt `17375` payloads against the
  rust-nostr reference types (mirrors `nostr::nip60` unit tests).
- Rollover logic: given prior token events + a new unspent set, assert the
  produced `7375`/delete/`7376` trio (pure functions, no network).
- Integration (optional, behind `#[ignore]`): testnut mint mint -> melt
  cycle against cdk's memory store.

## Open questions / tradeoffs

- Seed derivation is coop-specific; cross-app portability of proof
  restores is unsolved by NIP-60 generally. Revisit if a derivation
  convention lands in the NIP or cdk.
- v1 is single-unit (`sat`) and effectively single-mint in the UI, even
  though the event format and registry support many mints.
- Concurrent spends from two devices double-spend; recovery is the spec's
  Appendix-1 validation (check proofs at the mint, roll over survivors) —
  implemented as reconciliation on sync, not real-time locking.
- Import replaces the unspent proof set wholesale; the serialized job
  queue closes the in-flight-operation window, and mint-side double-spend
  rejection remains the backstop.
- Tracking cdk via git main is a moving target; the lockfile pins a rev
  and must be bumped deliberately (same policy as the rust-nostr git dep).
  Watch for the next cdk release line built against nostr 0.45.x — moving
  back to crates.io then removes the moving-target concern.
- Web target gets no wallet in v1: cdk store backends (redb, sqlite) need
  a real filesystem and do not build on wasm32; the crate compiles to
  nothing there (auto_update pattern). A wasm store would need a cdk
  `WalletDatabase` over IndexedDB — explicitly out of scope.
- `7374` quote events are skipped (local cdk state per the spec); pending
  quotes are not visible to other devices until claimed.
