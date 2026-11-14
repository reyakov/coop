# Coop Stability Audit — Pre-Release

Date: 2026-11-12 · Revision audited: `8fa79aef` (master) · Scope: entire workspace (~35k lines, 18 crates + `desktop` + `web`)

> **Remediation status (2026-11-12, later the same day):** all ten P0 items from §8 have been
> implemented and validated (workspace clippy clean, `cargo fmt --check` clean, 35 tests passing).
> See §9 for the per-fix log. The P1 list (committed as `fd802a88` base) has also been processed —
> see §10. P2 hygiene is done too (S1/S2 excluded by request) — see §11.

## 1. Scope and Method

Every crate was audited file-by-file against the project conventions (`AGENTS.md`), Rust best
practices (ownership, error handling, async, API design, anti-patterns), and GPUI lifecycle rules
(entity updates, task/subscription ownership, `ElementId` stability, render purity). Severe claims
were verified against the vendored dependency sources (`gpui-pre 0.3.12`, `gpui-base 0.9.1`,
`gpui-component 0.9.1`, rust-nostr 0.45.6, `gpui-updater-pre 0.2.0`), and the highest-severity
findings were independently re-verified by hand.

Tooling results: `cargo clippy --workspace --all-targets` — **clean, zero warnings**.
`cargo fmt --all --check` — **clean**. `cargo check -p workspace` and `-p person` — pass.

## 2. Executive Summary

**Verdict: not ready for stable release yet.** The codebase is well-structured — clean clippy,
disciplined GPUI entity/subscription hygiene in most crates, good hostile-input handling in the
crypto-adjacent code, and no lock-across-await anywhere. But there are **3 Critical** and
**~27 High** findings that must be addressed first, concentrated in three areas:

1. **Money and key safety** (`nip60`, `state`, `browser-signer-proxy`): a one-event wallet-wipe
   vector, a startup crash loop, a plaintext client key in web `localStorage`, and an
   unauthenticated localhost signing API.
2. **Silent failure paths**: errors from wallet ops, message loads, relay connects, settings
   persistence, and backup/keyring reads are swallowed; several failure states strand data or
   wedge the UI (stuck `loading_more`, permanently-blacklisted profile fetches, dictation dying
   after 30 s).
3. **Wrong results visible to users**: reaction double-counting, multi-line messages flattened to
   one line, contact-list updates that erase relay hints and petnames, stale group state from
   out-of-order relay delivery, duplicate tabs.

### Must fix before stable (P0)

| # | Severity | Location | Issue |
|---|----------|----------|-------|
| 1 | Critical | `nip60/src/lib.rs:420,552` | Spoofed kind:5 deletion from **any** author wipes wallet proofs (operator precedence bug + missing author check in `absorb_deletion`) |
| 2 | Critical | `state/src/lib.rs:117-122` | LMDB open `block_on` + `.expect` → unrecoverable startup crash loop on any DB error |
| 3 | Critical | `state/src/lib.rs:447-449` | Plaintext NIP-46 client secret key persisted to `localStorage` on web |
| 4 | High | `browser-signer-proxy/src/lib.rs:443-571` | Unauthenticated localhost signing API with `Access-Control-Allow-Origin: *`; plaintext theft, request suppression, response forgery |
| 5 | High | `nip60/src/ops.rs:455-488` | Reconcile marks relay proofs `Unspent` unconditionally → resurrects locally-spent proofs (double-spend risk) |
| 6 | High | `nip60/src/ops.rs:381-417` | Publish failure after store commit strands sent/received proofs; no retry path |
| 7 | High | `state/src/file.rs:418-443` | Decrypted-file cache keyed by filename only → user can be served another attachment's contents |
| 8 | High | `chat_ui/src/lib.rs:605-611` | Own reactions counted twice (backup gift-wrap re-delivery, no dedup) |
| 9 | High | `common/src/media_extractor.rs:52-58` | Whitespace collapse destroys line breaks in every message |
| 10 | High | `workspace/src/panels/contact_list.rs:187-191` | Contact-list Update silently strips NIP-02 relay hints and petnames from the published event |
| 11 | High | `nip29/src/lib.rs:541-555` + `group.rs:696-728` | No `created_at` guards: stale relay data resurrects left groups; group state applied last-write-wins |
| 12 | High | `settings/src/lib.rs:403-404,423-438` | Failed settings load silently resets and overwrites the user's file; saves discard errors and race |
| 13 | High | `speech/src/parakeet.rs:134` | Dictation worker exits after 30 s idle → longer dictations silently lost |
| 14 | High | `browser-signer-proxy/src/lib.rs:339-366` | Timed-out signing requests leak plaintext in memory forever |
| 15 | High | `dock/mod.rs:50-73` + `sidebar/rows.rs:54-57` | Tab dedup is dead code (duplicate tabs); "Ignore" screening action dispatches to a handler that can never receive it |
| 16 | High | `nip60_ui/src/wallet.rs:615-726` + `nip60/src/lib.rs:147-185` | Lightning invoice flow broken end-to-end (quotes never rendered or claimable) |
| 17 | High | `state/src/lib.rs:553-554` | Proxy credential write silently discarded (`save.await.ok()`) |
| 18 | High | `person/src/lib.rs:310-333` | One 2 s metadata timeout permanently blacklists those profiles until restart |
| 19 | High | `chat/src/lib.rs:398-403` | Failed-to-decrypt messages are trashed with no UI surface anywhere |
| 20 | High | `chat_ui/src/lib.rs:325-334`, `workspace/src/dialogs/profile.rs:227-235`, `workspace/src/panels/backup.rs:59-76` | Silent failure cluster: room loads, profile URL saves, keyring reads |

The full finding list follows, crate by crate.

## 3. Systemic Patterns (cross-cutting)

**S1 — Task handles accumulate unboundedly (~20 containers, ~48 declaration sites).**
`tasks: Vec<Task<_>>` / `SmallVec<[Task<_>; N]>` fields are pushed on every operation and never
drained; finished `Task` handles held in a container are not reaped by GPUI. Worst offenders:
`nip60/src/lib.rs:79` (a handle per relay event, whole session), `nip4e/src/lib.rs:90` (runs on
every settings change), `chat_ui/src/lib.rs:110`, `nip29/src/lib.rs:90`, `nip29_ui/src/lib.rs:337`,
`person/src/directory.rs:110`, `state/src/lib.rs:95`, plus every workspace dialog. The codebase
already contains the correct patterns: single `Option<Task<_>>` replacement
(`screening.rs`, `greeter.rs`, `debounced_delay.rs`) and reset-on-teardown (`nip29::reset`).
Fix convention-wide: replace-per-spawn for one-shots, `detach_and_log_err` for true fire-and-forget.

**S2 — Silent error discards (~156 `let _ =` / `.ok()` sites outside tests).**
Project rule forbids `let _ =` on fallible ops; `AGENTS.md` requires async errors to reach the UI.
Beyond the P0s above, the recurring case is `_ = this.update(cx, …)` — mostly benign
(entity dropped at shutdown) but should be `.log_err()` for visibility. Files with the most sites:
`dock/tab_panel.rs` (6), `nip60/src/lib.rs` (10), `nip4e/src/lib.rs` (8).

**S3 — `chat_ui` ↔ `nip29_ui` message-panel duplication.**
`ListState` sync + splice bookkeeping (including divergent `ix + 1` vs `ix + 2` offsets),
`rendered_texts_by_id` cache (both share the stale-mention bug), speech reveal + `RevealText`
fallback timer + `end_events` loop, `Speaker` wiring, reaction grouping, `MessageRow` assembly are
near-verbatim copies — and they have already diverged: `nip29_ui` splices at the prepend position
while `chat_ui` always appends (bug 5.7.4). Extract a shared message-panel controller in `ui`.

**S4 — Dialog error-banner + auto-clear timer duplicated four times** with drifted timings
(`restore.rs` 3 s, `import.rs` 3 s, `relays.rs` 2 s, `contact_list.rs` 2 s). One shared
`error: Entity<Option<SharedString>>` helper removes the drift.

**S5 — `DefaultHasher` used for network-persisted IDs.**
`common::EventExt::uniq_id` (`event.rs:12-18`) is hashed into published `r` tags by
`chat/src/lib.rs:1031-1041`; `DefaultHasher`'s algorithm is not stable across Rust versions, so a
toolchain bump silently changes every published room id. Also hand-rolled in three places
(`nip29/protocol.rs:282-286`, `nip29/utils.rs:27-32`). Replace with a stable hash (SHA-256 of
sorted hex pubkeys) in one shared helper.

**S6 — Index-based element ids.** Rows keyed by list index (`sidebar/rows.rs:174+`,
`contact_list.rs:238`, `greeter.rs:226+`, `nip29_ui` reply previews `message.rs:359`) reassign to
different data after mutations; benign today because handlers capture data, but any future
stateful element (hover/focus) in a row will mis-attach. Prefer domain ids (pubkey hex, event id,
room id) as `group_view.rs` already does.

## 4. Findings by Crate

Severity: **[C]** Critical · **[H]** High · **[M]** Medium · **[L]** Low

### 4.1 `state` — central Nostr registry, signers, persistence, files

- **[C] `lib.rs:117-122` — Startup LMDB open: `block_on` + `.expect` = crash loop.** The
  `block_on` itself is acceptable (runs once before the window opens; `NostrLmdb::open` schedules
  nothing back onto the GPUI executor — add a comment saying so). The real bug: a corrupted or
  unwritable data dir makes **every** launch panic with no dialog and no recovery except manually
  deleting data. The web path already does it right (`web_database.rs:30-33` falls back to memory
  with a logged error). Fix: mirror that on desktop — memory DB fallback + `StateEvent::Error` so
  the UI can tell the user local history is unavailable.
- **[C] `lib.rs:447-449` (+ `credentials.rs:29-38`, `common/web_store.rs:21-25`) — Plaintext
  bunker client key in web `localStorage`.** `import_bunker` embeds a fresh `Keys::generate()` into
  the bunker URI and stores the credential unencrypted on wasm. XSS/malicious extension/shared
  machine = signature capability through the bunker (bunkers commonly auto-approve registered
  clients). Desktop stores it in the OS keyring, which is fine. Fix: on wasm, passphrase-encrypt
  (NIP-49) like `import_nsec` does, or keep the session memory-only.
- **[H] `lib.rs:553-554` — Proxy credential write silently discarded.**
  `cx.background_spawn(async move { save.await.ok() }).detach();` — keyring failure = user silently
  dropped to login next launch. Log + emit `StateEvent::error`.
- **[H] `file.rs:418-443` — Decrypted-file cache collides by filename.** Cache key is
  `sha256(name)` and the `metadata().is_err()` guard skips the write, so two attachments named
  `invoice.pdf` from different chats resolve to the **first** file's path. Fix: include the
  attachment sha256/URL in the key, or always overwrite via the atomic-rename path.
- **[M] `lib.rs:373-386` — Web `ncryptsec` stored with `KeySecurity::Medium` (2¹⁶ PBKDF2).**
  Encrypted, but offline guessing at 2¹⁶ defeats human passphrases. Raise to `Strong` and/or move
  to IndexedDB + session-only keys. Acceptable if documented as a tradeoff.
- **[M] `lib.rs:564-589` — Stale proxy monitor can log out a newer session.** The monitor's guard
  is only `current_user.is_some()`; if the user disconnects the proxy and signs in with nsec/bunker
  before the next 5 s tick, the monitor swaps the **active** signer for `Keys::generate()`. Fix:
  capture the proxy pubkey at spawn and only act if it's still the active signer.
- **[M] `lib.rs:95` — `tasks` vec grows forever** (see S1; `connect_proxy` alone pushes four).
- **[L] `lib.rs:714` `to_bech32().expect("infallible")`; `signer.rs` ×7
  `.expect("RwLock poisoned")`.** Practically unreachable, but a poisoned signer lock is the worst
  failure mode for a signer — prefer `unwrap_or_else(|e| e.into_inner())`.
- **[L] `lib.rs:93` — `passphrase_required` written, never read** (UI learns via
  `StateEvent::PassphraseRequired`). Delete or expose.
- **[L] `file.rs:381-384` — `DownloadedFile::save` (wasm) has no callers.** Remove or wire up.
- **[L] `lib.rs:647-656` — Dead search-relay guard.** Vendored nostr-sdk: `Client::relay()` returns
  `Result<Option<_>>`, so `is_ok()` is always true and the "Failed to add search relay" branch is
  unreachable. Test `is_ok_and(|r| r.is_some())` or drop the check.
- **[L] `file.rs:221-258` — No size limit on pick/upload** (`MAX_FILE_SIZE` enforced only on
  download); a multi-GB pick is read fully into memory. Check size before reading.
- **[L] `file.rs:427-441` — Plaintext decrypted files in `temp_dir()/coop-files`, default
  permissions, never purged.** Consider 0600 + best-effort purge on exit. Also `file.rs:150`
  builds `file://{path}` by string concat (breaks on `#`/`?`/`%`).
- **[L] `file.rs:291` — Blossom rejection classified by `message.contains("415")`** — match the
  HTTP status instead. `file.rs:172` — 16-byte GCM nonce; 12 bytes is the norm.
- **[L] `lib.rs:280-289` — `restore_session` cfg branches compile to empty bodies on wrong
  target**, silently emitting no event. `debug_assert!` or fall through to `NoSigner`.
- **[L] `file.rs:446-451` + `web_file.rs:164-174` — Web blob URLs never revoked** when replaced;
  every decrypted file pins its bytes for the page lifetime.
- **[L] `lib.rs:683-687` — Search results can contain duplicate pubkeys.** Dedupe.

Positives: device key stored only NIP-44-encrypted to self in the local DB; bunker credentials
verified before persisting; `UniversalSigner` clones the inner `Arc` out of the lock before
awaiting (no locks across `.await`); downloads size-capped and hash-verified; `Keys`' vendored
`Debug` hides secrets.

### 4.2 `nip60` — NIP-60 Cashu wallet engine

- **[C] `lib.rs:420` + `lib.rs:552-572` — Spoofed kind:5 deletion can wipe the wallet.**
  `let wallet_event = ours && is_cashu || is_token_deletion(&event);` parses as
  `(ours && is_cashu) || is_token_deletion(&event)` — deletion events from **any** author are
  forwarded, and `absorb_deletion` never checks `event.pubkey == me`. Kind 7375 event ids are
  public on relays; one attacker event removes our tokens, marks them stale, and the Reconcile
  deletes the real proofs from the store (restore-only recovery). Fix:
  `wallet_event = ours && (is_cashu || is_token_deletion(&event))` + author check inside
  `absorb_deletion` for defense in depth.
- **[H] `ops.rs:455-488` (with `proof_info` `ops.rs:601-617`) — Reconcile resurrects spent
  proofs.** Every relay-token proof is added to the store hardcoded `State::Unspent`, ignoring the
  proof's existing store state. After any failed-publish op, reconcile re-adds proofs this device
  itself marked spent/reserved (incl. offline sends the recipient also holds) → bogus balance,
  real double-spend race. Fix: skip `y`s already in the store with non-Unspent state; verify
  unknown proofs against the mint (NUT-08) before adding.
- **[H] `ops.rs:381-417` + `lib.rs:836-843` — Publish failure after store commit strands funds.**
  If `send_event` fails, the confirmed `TokenUpdate` is dropped: for `SendToken` the token string
  is lost (proofs spent locally, recipient never gets it); for `Receive` proofs sit unpublished and
  a retry errors as duplicate. Reconcile only republishes when `mint_unissued_quotes() != 0`
  (`ops.rs:429-432`), so it doesn't heal this. Fix: retain the confirmed update on error; force
  republish whenever the store's unspent set differs from relay state.
- **[H] `ops.rs:420-453` — Quote-minting publish can `del` events whose proofs aren't converged
  yet.** `previous_ids` is the schedule-time relay snapshot; if another device's token was absorbed
  but not yet converged, the published event `del`s the id while omitting its proofs — those proofs
  get pruned everywhere while remaining spendable at the mint. Fix: converge before publish, or
  only `del` when the published proof set fully covers the id.
- **[H] `lib.rs:616,651` + `lib.rs:902-912` — Failed nutzap redemption stuck until restart.**
  `redeeming.insert` happens before scheduling; the `Err` branch never removes the id and
  `absorb_nutzap` skips anything in `redeeming`, and nothing requeues to `pending_nutzaps`. Fix:
  remove + requeue with bounded retries on error.
- **[H] `lib.rs:79` — `tasks` grows by a handle per relay event** (see S1).
- **[H] `lib.rs:147-149,172-185` — `mint_quotes()`/`claim_mint_quote()` dead → created invoices
  are never displayable or claimable.** `Outcome::QuoteCreated` pushes into `self.quotes` and
  nothing ever reads it (workspace-wide grep). Quotes also accumulate forever. Fix with the invoice finding in 4.3 below.
- **[M] `lib.rs:188-215` — `melt()` and `send_token()` unreachable; `WalletEvent::TokenCreated`
  emitted only from the dead path** while `workspace/src/lib.rs:226-236` promises "copy it from
  the wallet panel". Wire into UI or remove; fix the notification text regardless.
- **[M] `lib.rs:285-370` — Concurrent `setup()` can fork the wallet key** (only guarded by
  `loading`); absorption tie-breaks with `created_at >=` on second-granularity timestamps, and
  stores are keyed by wallet pubkey — the losing key orphans its funds. Fix: `setup_in_progress`
  guard; deterministic tie-break (event id).
- **[M] `lib.rs:430-432` — `processed_events.clear()` at 1000 entries** re-delivers a burst;
  evict oldest instead.
- **[M] `lib.rs:615-661` — Nutzap `unit` tag never validated**; a `usd` nutzap passes local
  validation. Require `unit == "sat"` in `Nutzap::from_event`.
- **[L] `lib.rs:301-303,699-701` — Silent no-ops**: `setup()` when mint already listed;
  `ensure_wallets` on unparseable mint URLs forever. Surface feedback.
- **[L] `ops.rs:107` — ClaimQuote history records `quote.amount.unwrap_or_default()`** (0-sat
  inflow when mint omits amount); use the summed proof amount.
- **[L] `Cargo.toml:23` — `tokio` dep unused** (only `gpui_tokio::Tokio` is referenced). Remove.

Positives: store-before-publish enforced via cdk prepare/confirm sagas; all relay-derived content
author-checked before decryption; `keys.rs` secret handling clean (no Debug on `WalletData`, no
secrets in logs, HKDF salt pinned by test); single-flight job queue; `WalletEvent::Error` reaches
notifications even with the panel closed.

### 4.3 `nip60_ui` — wallet panel + dialogs

- **[H] `wallet.rs:615-726` — Invoice button creates an invoice the user can never see or pay.**
  After `create_mint_quote` the bolt11 string lands in `WalletRegistry::quotes`, which nothing
  renders. Receive-via-lightning is broken end-to-end, silently. Fix: render pending quotes
  (request + copy + check/claim) in the dialog/panel, refreshed on `WalletEvent::Updated`.
- **[M] `wallet.rs:765` — `render()` clones the whole history `Vec` every frame** (history is
  unbounded). Snapshot into `Rc` only on `WalletEvent::Updated`. Same for `filtered_contacts`
  (`wallet.rs:542`) — cache on input change.
- **[L] `wallet.rs:206,284,356` — `SendDialog::tasks` accumulates** (see S1).
- **[L] `wallet.rs:262-264` — `load_contacts` gives up silently pre-login**; retry on
  `StateEvent::SignerChanged` (sidebar already does this).
- **[L] `wallet.rs:728-733` vs `lib.rs:28-34` — amount parsing duplicated.** Share `parse_amount`.

Positives: `cx.notify()` on all mutating paths; per-key hex ElementIds; stored subscriptions;
history list windowed via `uniform_list`.

### 4.4 `browser-signer-proxy` — NIP-46 localhost bridge

- **[H] `lib.rs:443-571` — Unauthenticated API + `ACAO: *`.** Every response sends
  `Access-Control-Allow-Origin: *` (verified at :556, :588, :607, :631); no `Host`/`Origin` checks.
  Consequences: (1) `GET /api/pending` returns full `sign_event`/`nip44_encrypt` params (message
  plaintext) readable by any web page in Firefox-family browsers and by DNS rebinding anywhere;
  (2) `:516-517` clears the queue on read → third party can steal requests so the real extension
  page never sees them; (3) `POST /api/response` accepts any `{id, result}` → forged
  `get_public_key` substitutes an attacker pubkey as "the user". Fix: drop CORS entirely (the
  served page is same-origin), reject non-`127.0.0.1:7400` Host, gate `/api/*` on a per-session
  random token embedded in the served page.
- **[H] `lib.rs:339-366` — Timed-out requests leak forever.** The `RequestData` and oneshot sender
  stay in the maps after the 30 s timeout — plaintext retained for the session, re-served to
  whoever polls next, map grows unboundedly. Remove entries on timeout (self-removing guard or
  id-returning `request`).
- **[M] `lib.rs:374-380` — `_sign_event` verifies the signature but not the event.** Nothing checks
  the returned event matches the requested `UnsignedEvent` (pubkey/content/tags/kind/created_at);
  a compromised signer page can substitute content. Compare before accepting.
- **[M] `lib.rs:538` — Decrypted plaintext debug-logged.** `tracing::debug!("Received response …
  {message:?}")` leaks `nip44_decrypt` output and event content the moment anyone runs with
  `RUST_LOG=debug`. Log id + method only.
- **[M] `lib.rs:447,465-480,523` — Unbounded reads/allocations**: header line grows unbounded;
  `vec![0u8; content_length]` trusts `Content-Length` (4 GiB reservation from one POST); no
  connection timeout. Cap header block (16 KiB), cap body (1 MiB), add idle timeout.
- **[L] `lib.rs:650-678` — `race_timeout` reports channel-closed as timeout.** Distinguish.
- **[L] `lib.rs:574-638` — Three near-identical response writers**; one function with a header
  options struct (also makes the CORS removal enforceable in one place).
- **[L] `lib.rs:546,563,593,612,636` — bare `let _ =` on writes**; centralize the
  best-effort-write rationale in the helper.

Positives: loopback-only bind, graceful shutdown tied to handle lifetime, per-request timeouts,
UUID ids, bounded channels.

### 4.5 `nip4e` — per-device keys

- **[M] `lib.rs:1036,774-860` — Approval button wedged + double-clickable.** `loading.set(true)`
  without `cx.notify()` keeps the old button enabled (double-click sends the secret in two 4455
  events); on failure `loading` is never reset and the notification isn't removed. Notify after
  setting, reset on error, rebuild the notification on both outcomes.
- **[M] `lib.rs:403-405` — Rotation skips archiving silently, then overwrites the key.** If the DB
  read/decrypt fails transiently, `archive_key` is skipped but `set_keys` still overwrites → old
  chats permanently undecryptable. Make archive failure abort rotation (or log loudly).
- **[M] `lib.rs:175-206` — Relay listener can die silently.** `tx.send_async(...).await?` aborts on
  full bounded(100) channel or dead consumer; the task's `Err` is never inspected. All 4e event
  processing stops until the next signer change. Log and handle backpressure.
- **[M] `lib.rs:90` — `tasks` grows on every settings change** (`get_announcement` runs per
  settings mutation) — see S1.
- **[M] `lib.rs:177,184` — `processed_events` inserts **every** relay event** (insert precedes the
  kind filter) and grows unbounded. Filter first; bound the set.
- **[L] `lib.rs:303-309` — Desktop key backup file written 0644** (subject to umask); use 0600.
- **[L] `lib.rs:178` — `current_user` snapshotted per listener**; events between signer change and
  `get_public_key_async()` resolving are dropped, and a stale snapshot can briefly forward one
  account's events to the next handler. Re-check per event.
- **[L] `storage.rs:29-32` — Malformed stored `created_at` silently resets the rotation
  interval.** Log the fallback.
- **[L] Zeroization gaps** (`app_keys`, decrypted secrets in `decrypt_approval`/`approve`): Low —
  the nostr stack itself doesn't zeroize; note as defense-in-depth.
- **[L] `lib.rs:141,246,426,458,845,854,894,1041` — `.ok()` discards** (see S2); `lib.rs:479`
  single-letter `n`.
- **[L] `lib.rs:136,241,888,914` — `cx.windows().first()` targets an arbitrary window** (matters
  only if multi-window ever lands).

Positives: secret never published/logged; transfer encrypted to a fresh one-time keypair;
approvals re-validated against the live announcement; stale requests dropped by age; bounded
archive; account switch wipes pending state.

### 4.6 `chat` — chat registry + rooms

- **[H] `lib.rs:331-338` + `message.rs:203-220` — Failed-to-decrypt messages silently trashed;
  nothing renders `trash`.** The `trash: Entity<BTreeSet<FailedMessage>>` is written, cleared, and
  reprocessed, but has **no reader or renderer workspace-wide**. A user whose incoming message
  can't be decrypted gets zero feedback — core E2EE UX gap. Emit a `ChatEvent` or render an
  indicator.
- **[M] `lib.rs:284,327,620-635,726,733,854,892` — `RwLock` `.read().unwrap()`/`.write().unwrap()`
  poisoning crash loop.** `search.write()` runs on a background thread; one panic while holding it
  turns every later foreground read into a panic. These maps are reconstructible heuristics — use
  the tolerant `poisoned.into_inner()` pattern the codebase already has
  (`person/src/directory.rs:226-233`).
- **[M] `lib.rs:779-780` — Off-by-one index stored in `room_map`** (`rooms.len()` after push).
  Currently unreachable (batch rooms come from an id-keyed set) but any future reader panics at
  `self.rooms[index]`. Store `len() - 1`, or drop the never-usefully-read index bookkeeping.
- **[M] `lib.rs:440-443,454-461,825-833` — Discarded subscribe result + `unwrap_or_default()`.**
  `_ = client.subscribe(...)` (rule violation); a DB error becomes "not found" → false
  "Messaging Relays not found" retry prompt, and a contacts DB error classifies peer rooms as
  `Request` instead of `Ongoing` (wrong trust classification). Distinguish errors from empty.
- **[M] `lib.rs:110,199,232-358,362-414` — Stored task results never inspected** (see S1/S2); a
  dead notification listener silently stops message delivery — at least `detach_and_log_err`.
- **[L] `room.rs:637-646` — `deliver<S>` generic has one implementation**; inline it.
- **[L] `lib.rs:867` — `max_by_key().unwrap()`** relies on an invariant three lines up;
  `let Some(...) else { continue }` documents it.
- **[L] `lib.rs:332-338` — Empty-tags rumor is both trashed and delivered** (appears as failure
  entry *and* rendered message until reprocess). Pick one path.
- **[L] `room.rs:350` — `self.members[0]` safe only via a cross-crate invariant**
  (`extract_public_keys` always appends the author). Add `debug_assert!` or a comment.
- **[L] `Cargo.toml:28-29` — `smol` unused.** Remove.
- **[L] `lib.rs:182-186` — `let _ = entity.update(...)`** in deferred init; `.log_err()`.

Positives: signal batching bounds per-update work; wasm yield loop; gift-wrap dedup; seal
verification; `WeakEntity` room handles; `ChatEvent::Error`/`Loaded`/`Ping` genuinely surfaced.

### 4.7 `chat_ui` — chat panel

- **[H] `lib.rs:605-611` — Your own reactions count twice.** `insert_reaction` pushes
  unconditionally (keyed only by target id); with `backup: true` (default) the reaction comes back
  as your own backup gift wrap → inserted again → "👍 2". Text messages are dedup'd by
  `binary_search`; reactions aren't. The dormant `RoomEvent::Reload` path double-counts too.
  Track seen reaction event ids.
- **[H] `lib.rs:325-334` (+ `272-277`) — Room message-load and connect failures swallowed.** The
  stored task's `Result` is never inspected: user sees an empty room with no error; relay
  subscription failures invisible. Push a notification on error.
- **[M] `lib.rs:557-581` — Bulk load is O(n²) on the foreground thread.** Per-event
  binary-search + index rebuild + splice inside one `update`; multi-second stall on big rooms.
  Add a bulk variant (sort once, rebuild index once, splice once).
- **[M] `lib.rs:569-570` — Insert position and `ListState::splice` disagree.** Messages are
  binary-inserted by `created_at` but the list always appends at `len..len`; mid-`Vec` arrivals
  shift rows while the list state learns "appended one" → stale height caches, scroll jumps.
  `nip29_ui/src/lib.rs:516` does it right: `splice(pos + 1..pos + 1, 1)`.
- **[M] `lib.rs:1183-1198` — `rendered_texts_by_id` caches mention names forever** (same defect as
  `nip29_ui` 4.8). A message rendered before the profile loaded shows `@npub1…` all session.
  Key the cache on directory generation or drop the resolved-text cache.
- **[M] `lib.rs:50-51,395-398` — `\p{Emoji}` matches plain digits: typing "2" while replying to
  exactly one message sends a "2" reaction instead of a message.** Require
  `[\p{Emoji_Presentation}\p{Extended_Pictographic}\u{200D}\u{FE0F}\u{20E3}]`.
- **[M] `lib.rs:110` — `tasks: Vec<Task>` grows without bound** (see S1).
- **[M] `lib.rs:547-549,618,623` — `reports_by_id: Arc<RwLock<...>>` nothing else shares.** Plain
  `BTreeMap` field; removes the poisoning risk for zero benefit.
- **[L] `lib.rs:499` — `rumor.id.expect("rumor must have an id")`** — invariant holds today, but
  degrade + notify instead of panic per house style (also `to_bech32().unwrap()` at 652, 1072).
- **[L] `lib.rs:362-370,797-802` — `.ok()` discards `room.update`** (subject change, retry action).
- **[L] `lib.rs:722-727` — `encrypted` snapshotted before the file-picker await**; typing while the
  picker is open classifies the file by stale input state. Re-read after the await.
- **[L] `lib.rs:1170-1178` — `load_file` invoked from the render path** (guarded, but render should
  be pure — move to a subscription).
- **[L] `lib.rs:147-221` — Summary comments** ("Define input state", …) violate the comment rule.

### 4.8 `nip29` — NIP-29 registry + protocol

- **[H] `lib.rs:541-555` (merge), `582-607` (absorb_list) — Stale group-list events resurrect left
  groups and get republished.** The kind 10004 list is absorbed without comparing `created_at` and
  `merge` never removes entries absent from older snapshots; a relay still holding the pre-leave
  version re-adds the left group, `ensure()` re-creates subscription, and the next
  `refresh_entry`/`publish_list` republishes the left group to all write relays (privacy angle).
  Track the last accepted list event's `created_at`; ignore older.
- **[H] `group.rs:696-728` (absorb_state) — Group state applied last-write-wins, no ordering
  guard.** Out-of-order relay delivery (newest-first is common) leaves the *oldest*
  metadata/members/admins/roles/pins displayed indefinitely. Cache hydration is safe (sorted
  ascending at `group.rs:507`); the live path is not. Keep `created_at` per state kind (the
  `forks` map at `lib.rs:669-675` shows the pattern).
- **[M] `lib.rs:173-174,189` — Join published optimistically; refusal classified by relay error
  prose** (`reason.starts_with("duplicate")`); a refused join stays in the public list. Drop the
  entry on refusal; detect duplicates via a stable marker.
- **[M] `lib.rs:173,825-834` — Rejoin flips a private list entry to public** (`join_key` hardcodes
  `private: false`, `upsert` replaces the whole entry). Preserve the existing flag.
- **[M] `group.rs:418-448` + `lib.rs:383-397,1146-1159` — Failed page load leaves `loading_more`
  stuck true forever** (subscribe error path emits no signal; guard then permanently blocks
  pagination for the session). Route a synthetic settle on error.
- **[M] `lib.rs:308-330` + `discovery.rs:253,267,274,281` — Network failures silently produce
  "empty" results** (`and_connect().await.ok()`, discarded sync errors): browse shows zero
  candidates, discovery silently stale. Surface `GroupsEvent::Error`.
- **[M] `discovery.rs:240-252,264-267` — Empty contact list → `Filter::authors(∅)` serializes as
  absent → the sync reconciles **every user's** list, and the pool-wide sweep (kinds
  `[GroupMetadata, GroupMembers]`, no ids/limit) pulls all NIP-29 state from all relays.**
  Early-return when `authors.is_empty()`; constrain the sweep.
- **[M] `group.rs:457-511` + `lib.rs:906-941,1042-1071` — Every live event triggers a full
  unlimited `load_envelopes` reload** (re-query + JSON-decode + sort of entire group history per
  message) and every event is cached forever. Use the single-event absorb path live; cap/prune the
  cache.
- **[M] `lib.rs:90` — `tasks: SmallVec<[Task; 2]>` accumulates per incoming event** (see S1).
- **[L] `protocol.rs:389-399,534,650,964` — One malformed optional tag fails the whole parse**
  (one bad `supported_kinds` value blanks a group's metadata display). Skip + log instead.
- **[L] `protocol.rs:24` — Custom kind 9010 not in rust-nostr 0.45.6's NIP-29 range**; confirm
  against target relay implementations before stable.
- **[L] `group.rs:602-611` — `previous` mismatch only logs a warning**; an empty `previous` tag
  counts as fully verified (`protocol.rs:1046-1056`). Align comment with behavior or escalate.
- **[L] `protocol.rs:995-1016` — `TimelineWindow.seen` grows unbounded** (u32/event, never pruned).
- **[L] `lib.rs:582-607,661-677` — No client-side author re-check on list/fork subscriptions.**
  Mitigated by `verify_subscriptions(true)` + `ban_relay_on_mismatch(true)`; note the residual
  NIP-29 trust model rests on relay admission policy.
- **[L] `lib.rs:860-904` — Concurrent `publish_list` can complete out of order** (each snapshots at
  call time; older snapshot can win). Serialize publishes.
- **[L] `lib.rs:1225-1230` — Negentropy/foreign-EOSE cross-talk** fires spurious `Synced` signals
  from unrelated subscriptions.
- **[L] `utils.rs:24,31,35` — Subscription ids truncate the digest to 32 bits**; collision would
  cross-wire routes. Negligible at this cardinality; use the full digest anyway.
- **[L] `lib.rs:1198-1200` — `processed` dedup wholesale-cleared at 10k**; evict oldest instead.
- **[L] `lib.rs:723-736` — `previews` grow unbounded per session** (entity + subscription each).
- **[L] `group.rs:197-202` — `Group::message(id)` O(n) scan** called per reply preview in render
  paths → O(n²) timelines. Build an id→row map.
- **[L] `Cargo.toml:14` — `person` dep unused.** Remove.
- **[L] `lib.rs:1208-1229` — `.ok()` on signal sends** (benign; log or comment per rule).
- **[L] `lib.rs:358,949,975` — Inline global-handle chains** (rule: bind to a local).

Positives: zero reachable panics; tag parsing consistently `get`/`skip`/`first`;
`saturating_sub` timestamp math; tasks/subscriptions stored; `WeakEntity` spawns; membership
derivation order-independent; strong publish-error propagation.

### 4.9 `nip29_ui` — group panel + dialogs

- **[H] `lib.rs:1053-1067` — `rendered_texts_by_id` bakes mention names in permanently.**
  Computed once per `EventId`; when the profile arrives later the body keeps `@npub1ab…` (the
  header recovers via re-render, the body doesn't). Key on directory generation or don't cache the
  resolved text (only the markdown source needs caching).
- **[M] `lib.rs:337` — `tasks` accumulates** from every send/upload/react/delete/pin (see S1).
- **[M] `lib.rs:1068` — Content `TextView` keyed by row index.** Prepending shifts every index →
  every visible `TextViewState` re-parses markdown in one frame ("Load earlier messages" jank).
  Use the message id as element id.
- **[M] `message.rs:359` + `lib.rs:1068` — Sibling ElementId collision: reply previews use bare
  reply index as sibling of the row-index TextView**; when `replies.count > row_index` both sides
  share `…/[N]/[N]`. Latent today, breaks silently when element types coincide. Namespaced ids.
- **[L] `message.rs:192,227,238` — `let _ = panel.update_in(...)`** where sibling call sites log
  (`log::warn!` at 110-160). Mirror the logged pattern.
- **[L] `lib.rs:630` — File-picker failure silently swallowed** (task result never inspected).
- **[L] `lib.rs:436-445` — Utterance-end loop clears the reveal unconditionally**; interrupted
  utterances still fire `on_utterance_end` on many backends, cancelling B's reveal while B's audio
  plays. Guard with the session id like `message.rs:144` does.
- **[L] `message.rs:97,130,168-180` — Per-frame per-row allocations** (`preview()` computed twice
  per row per frame, custom-emoji `Vec` rebuilt + cloned into the submenu closure every render —
  and gpui-pre redraws the window on every keystroke). Compute once per render.
- **[L] `lib.rs:677,956` — Inline global chains** (rule).
- **[L] `lib.rs:407-419` — Subscribe + observe both run `sync` + `track_participants`** (events
  and notifies fire together) → double O(rows) scan per change.
- **[L] `message.rs:322` — Empty role list renders "is now "** (trailing space, no roles). Emit
  "now a regular member" for the empty case.

### 4.10 `common` — shared utilities

- **[H] `media_extractor.rs:52-58` — Every message body has all whitespace collapsed, destroying
  line breaks.** `cleanup_text`'s `\s+ → " "` runs on the entire remaining text unconditionally,
  so multi-line messages render as one line in both chat and groups (via `chat/message.rs:99-103`).
  Normalize only around removed URLs; never touch `\n` in untouched regions.
- **[M] `media_extractor.rs:15-18,56,62-65` — Regexes compiled per message** (the cache struct's
  own entry point defeats it). `static LazyLock<Regex>`.
- **[M] `parser.rs:240,254,260` — `NostrParserIter::next` recurses per token; depth is
  attacker-controlled** (~9 bytes/token of crafted `nostr:` URIs). Desktop's 8 MB stack survives;
  the wasm32 web build can crash the tab. Rewrite as a loop.
- **[M] `event.rs:12-18` — `uniq_id` uses `DefaultHasher` and is persisted to published events**
  (see S5).
- **[M] `paths.rs:7,14,23,31` — `expect()` on `dirs::download_dir()` etc.** — legitimately `None`
  on Linux without XDG user-dirs → panic on export (`workspace/src/lib.rs:405`). Return `Option`
  and handle.
- **[L] `display.rs:18,20,39` — `as i64` on relay-controlled timestamps** (contained by
  `LocalResult`); fallback magic strings `"9999"`/`"1m"`.
- **[L] `event.rs:12-53` — `Event`/`UnsignedEvent` impls duplicate the same logic with drift**
  (one sorts, one doesn't). Factor into one helper — same helper fixes S5.
- **[L] `debounced_delay.rs:33` — `_ = channel.send(())`** explicit silent discard (harmless;
  satisfy the rule with a comment + `if let Err`).

Positives: `web_store.rs` error handling exemplary; parser char-boundary-safe throughout;
`DebouncedDelay` cancellation correct; time formatting degrades gracefully.

### 4.11 `person` — profiles + web of trust

- **[H] `lib.rs:310-333` + `204-245` — One failed metadata batch permanently blacklists those
  pubkeys.** `in_flight` removal happens only on metadata arrival; `get_metadata` uses
  `TIMEOUT = 2 s` (tight for bootstrap relays + EOSE) and on failure keys stay in `in_flight`
  forever → those people show truncated-pubkey placeholders until restart. Return failed keys on
  error and release them; raise the request timeout independently of the batching window.
- **[M] `lib.rs:175` — Malformed metadata wipes a good profile.**
  `Metadata::from_json(...).unwrap_or_default()` replaces previously-loaded data with a default
  `Person`; content is attacker-influenceable (anyone can publish metadata for any key). Skip on
  parse failure.
- **[M] `directory.rs:110` (+ `lib.rs:255-262`) — Task handles accumulate per `track`** (see S1).
- **[M] `directory.rs:161-172` — `person()` performs side effects (lock write + network fetch) from
  a render-time getter.** Deduped via `pending`, but order-dependent render behavior; consider an
  explicit `ensure_tracked` from update paths.
- **[L] `wot.rs:100,343` — DB errors silently become empty results**; add `log::warn`.
- **[L] `wot.rs:150-153` — `let _ = this.update(...)`** (commented, periodic; noted for the rule).
- **[L] `person.rs:23-47` — `PartialEq/Eq/PartialOrd/Ord/Hash` for `Person` unused
  workspace-wide**; the `Ord` impl clones `Metadata` twice per comparison. Delete.
- **[L] `person.rs:66-68,124-138` — `metadata()` clones per call; `name()` calls it twice** per
  invocation → 2+ clones per avatar/name row per frame. Borrow or cache.
- **[L] `person.rs:166-177` — `shorten_pubkey` doc example doesn't match output; panics if
  `len ≥ 63`** (callers use 4/8 today). Note: the `let Ok(...) =` pattern compiles only because
  `ToBech32::Err = Infallible` — worth a comment since it looks like a compile error.

Positives: WoT builder caps hostile publishers; poisoning-tolerant lock helpers; bounded dispatch
channel; `limit(1)` newest-event lookups verified correct against nostr-lmdb ordering.

### 4.12 `speech` — dictation + TTS

- **[H] `parakeet.rs:134` — Inference worker exits after 30 s idle, permanently disabling
  dictation.** `recv_timeout(30s)` kills the thread; the `OnceLock` sender survives, so after
  that every `send` fails — and `send_or_log` (`:200-204`) logs at **debug** and drops. A
  `MAX_SECONDS = 120` dictation records fine, then `finish()`'s `Transcribe` goes into the void;
  gpui-kit's 180 s `stop_timeout` then force-ends with an **empty** transcript. Net: dictations
  longer than 30 s silently lost after up to a 3-minute hang. Fix: block on `recv()` (an `Unload`
  command already exists for explicit retirement), and make `finish()` surface send failure via
  `sink.error`.
- **[M] `parakeet.rs:63-68` — Audio past 120 s silently discarded** mid-sentence with no
  indication. Surface once or stop capture at the cap.
- **[M] `parakeet.rs:119-128` — Worker-spawn failure leaves a permanently dead channel** (receiver
  dropped, sender cached in the `OnceLock` forever). Don't cache on failure.
- **[M] `speaker.rs:36-53` — `Tts::default()` constructed lazily inside `speak()` invoked from
  render paths**; first-use engine init can take tens-to-hundreds of ms on the UI thread
  mid-render. Initialize eagerly at panel creation or on the background executor.
- **[L] `model.rs:82-97` — `status()` does filesystem I/O per call** while the settings dialog
  polls every 500 ms on the foreground. Cache status in the state mutex.
- **[L] `model.rs:183` — `let _unused = fs::remove_file(...)`**; log per rule.
- **[L] `web.rs:23-25` — `is_available` hardcodes `true`**; mic button renders enabled in browsers
  without `SpeechRecognition`. Reflect actual support.
- **[L] `web.rs:200,218` — `let _ignored` abort/stop** — commented, best-effort; fine.

Positives: `catch_unwind` isolation around model load/transcribe; staged download with SHA-256 +
size cap + atomic rename; dedicated threads for inference/download; all other fallible paths
reach `sink.error`.

### 4.13 `settings` — persistence + themes

- **[H] `lib.rs:403-404` + `311-314` — Failed load silently resets settings and destroys the saved
  file.** `unwrap_or(Settings::default())` swallows I/O and JSON errors; the observer then
  immediately writes defaults over the user's file (losing `trusted_relays`, pins, wallet prefs)
  with no log. Log, keep defaults in memory, but don't persist until a real user change.
- **[H] `lib.rs:423-438` — Saves discard errors and race each other.** `.await.ok()` hides
  disk-full/permission failures; every change detaches a new concurrent write to the same path —
  last-completion-wins can overwrite newer snapshots and interleaved truncate/write can corrupt
  the file (font-size input ticks each trigger a save). Single writer slot + temp-file + rename +
  `log_err`.
- **[L] `lib.rs:425` — Serialization failure silently skips saving**; log.
- **[L] `lib.rs:596-639,671-683` — `apply_persisted_theme` re-reads and re-parses the settings
  file four times.** Parse once.
- **[L] `lib.rs:211,408` — `Url::parse(DEFAULT_FILE_SERVER).unwrap()`**; `OnceLock` or a reasoned
  `expect`.

Positives: accessor macros keep 20 settings consistent; serde default/alias migration fields with
tests covering old formats; wasm path handled.

### 4.14 `workspace` — core + dock + sidebar

- **[H] `dock/mod.rs:50-73` + `lib.rs:309-320` — Tab dedup is unreachable; repeated opens stack
  duplicate tabs.** `find_panel` matches on `PanelId`, which is always `PanelId::from(entity_id())`
  of a panel created in the same statement — so it can never match (verified against gpui-base
  0.9.1 `add_panel_inner`, which inserts without dedup). Clicking the same chat row twice opens
  two live panels for one room; ShowInbox/Browse/Contacts/Backup multiply tabs. Key panels by
  domain identity (singleton entities per panel name, or room/group key). The ~55 lines of
  `find_panel`/`move_panel` are dead until then.
- **[H] `sidebar/rows.rs:54-57` — "Ignore" dispatches `ClosePanel` where it can never be handled.**
  The only handler is on the tab-group frame; the alert dialog holds focus at that moment, and
  dispatch resolves along the focus node's ancestor path — the dock tree isn't on it. Clicking
  "Ignore" is a silent no-op for a destructive-intent action. Close the room via the dock handle
  already in scope, or restore focus first.
- **[H] `sidebar/header.rs:38,236-268` — Updater buttons render stale status.** Nothing observes
  the `AutoUpdater` entity (gpui-pre has no read-tracking); a staged update can sit invisible on
  an idle window indefinitely. Add `cx.observe(&updater, …)`.
- **[M] `sidebar/mod.rs:192-206` + `search.rs:70-117` — Full message-index scan on the foreground
  thread for every keystroke.** Only the *global* search is debounced; local search synchronously
  scans every indexed message, sorts the entire match set, then truncates. Debounce + background
  spawn + partial top-`limit` select.
- **[M] `sidebar/list.rs:41,178-248` + `group_view.rs:88-136` — Row model rebuilt on every sidebar
  render** (clone + partition + pin-tag scans ×visible rows; sidebar re-renders on 500 ms ticks,
  keystrokes, group updates). Cache the row model in state, refresh on registry events.
- **[M] `sidebar/search.rs:152-155` — Global search failure renders as "No profiles found".**
  Surface the error state.
- **[M] `lib.rs:410-421` — Save-dialog failure silently cancels the backup** (`Err(_) => return`):
  indistinguishable from user cancel; user believes the key was backed up. Split arms + notify.
- **[M] `sidebar/mod.rs:139-142` — Whole-sidebar re-render on every `GroupsEvent::Updated`**
  (acknowledged TODO; compounds the row-rebuild cost). Gate on visibility.
- **[L] `dock/skin.rs:25,35-37,55` — `toggle_button_visible: Cell<bool>` is write-once** (set true
  in ctor, never elsewhere) → dead state + dead branch at `tab_panel.rs:212`. Delete.
- **[L] `sidebar/rows.rs:174-374` — Index-based row ids** (see S6).
- **[L] `dock/tab_panel.rs` ×6, `skin.rs:262`, `sidebar/search.rs:164,178`, `lib.rs:641` — `let _ =
`/`.ok()` on fallible updates** (see S2); `lib.rs:641` is pre-destructive-reset — make it visible.
- **[L] `rows.rs:220-222,346-351`, `list.rs:53-55` — Inline global chains** (rule).
- **[L] `lib.rs:84`, `sidebar/mod.rs:99`, `sidebar/inbox.rs:68,118` — `tasks` accumulate** (see S1).
- **[L] `lib.rs:54` — Typo: `DeviceNotifcation`.**

Positives: careful `WeakEntity` hygiene; virtualized lists via `uniform_list` + `Rc` snapshots;
`InvalidPanel` preserves unknown panels across load/save; `theme_name_at` maps the palette's
original `IndexPath` correctly; `cargo check -p workspace` passes.

### 4.15 `workspace` — dialogs + panels

- **[H] `panels/contact_list.rs:187-191` — "Update" republishes the contact list with relay hints
  and petnames stripped.** `load()` keeps only `tags.public_keys()`; `update()` rebuilds every
  entry as `Contact::new(*public_key)` (`relay_url: None, alias: None`) while the UI reports
  success. Silent NIP-02 data loss on relays. Carry the original `Contact`s through.
- **[H] `dialogs/new_chat.rs:60-64` — Contact list doesn't re-filter while typing.** No
  `InputEvent::Change` handling; the input renders its own state entity so typing doesn't notify
  `NewChat`; results stay stale until an unrelated click/Enter. `cx.notify()` on Change. Same
  pattern, cosmetic: `profile.rs:320-326`.
- **[H] `panels/backup.rs:59-76` — Keyring read failures silently swallowed**; backup panel just
  shows empty inputs. Log + surface.
- **[H] `dialogs/profile.rs:227-235` — Invalid avatar/website URLs silently dropped on save** while
  "Profile updated successfully" shows. Validate both up front; inline error.
- **[M] `dialogs/screening.rs:52-55` — `on_release_in` calls `window.close_all_dialogs(cx)`**,
  nuking unrelated stacked dialogs (including the mutual-contacts dialog this view opens). Close
  just the hosting dialog / cancel tasks only.
- **[M] `dialogs/screening.rs:126-168` — DB-fallback "mutual contacts" has different semantics than
  the WoT list** (followers vs. your-contacts-who-follow). Intersect or relabel.
- **[M] `dialogs/screening.rs:211-232` — NIP-05 network errors render as "address does not
  match"** (`unwrap_or(false)` asserts infrastructure failure as fact about the person). Tri-state.
- **[M] `dialogs/screening.rs:250-282` — Report submit gives no failure feedback.**
- **[M] `dialogs/import.rs:129-185` — Enter bypasses the loading guard** (no in-method guard; all
  three `PressEnter` subscriptions call `login()` directly) → concurrent double import. Same class:
  `contact_list.rs:176-228`, `relays.rs:377,521`.
- **[M] `dialogs/import.rs:31-40` — Unlock dialog is inescapable** (no cancel/logout path for a
  forgotten passphrase). Add a "start over" escape hatch for stable.
- **[M] `panels/browse.rs:484-493,316-348` — Card list recomputed per render *and* per visible
  row** (O(n) clone + registry reads ×(rows+1) per frame during scroll). Compute once per render
  (`new_chat.rs`'s `Rc<Vec>` pattern).
- **[M] `panels/greeter.rs:127-137` — Every refresh fetches the full message history of every room
  to take `.last()`.** Query latest-per-room instead.
- **[M] `dialogs/new_chat.rs:138-160,388` — Filter allocates per contact per keystroke-scale
  render** (`to_hex().to_lowercase()` per contact). Cache lowercase names / filter once per change.
- **[M] `dialogs/settings.rs:124-148` — Speech-model errors can be consumed before the status
  change check** and dropped. Check `error` before the equality early-return.
- **[M] `new_chat.rs:282-315` vs `profile.rs:274-313` — Duplicated room-open logic with divergent
  dedup.** Unify on find-or-create.
- **[L] `panels/contact_list.rs:238,259` + `greeter.rs:226-402` — HashSet/index-based element ids**
  (see S6).
- **[L] `panels/contact_list.rs:288` — Copy bug: contacts panel says "Please add some relays."**
- **[L] `dialogs/new_group.rs:392` — `let _ = this.update(...)`** (see S2; `relays.rs:623-629` is
  the correct logged pattern).
- **[L] `panels/backup.rs:66,70` — `.unwrap()` on bech32** (infallible today; still banned pattern).
- **[L] `dialogs/restore.rs:52-62`, `contact_list.rs:130`, `relays.rs:352-355,497-500` — Untrimmed
  paste input fails parse** ("trailing newline = invalid key"); `relays` has a weak
  `starts_with("ws")` check.
- **[L] `dialogs/relays.rs:124-140,360-366` — Relay metadata can never be reset; duplicate add is a
  silent no-op** that can't update metadata.
- **[L] `dialogs/settings.rs:517-543` — Toggles compute `!rendered_value` instead of the switch's
  `checked`**; two fast clicks collapse into one. Match `onboarding.rs:398-399`.
- **[L] Dialog `tasks` fields never pruned** (`import.rs:63`, `restore.rs:24`, `relays.rs:45,57`,
  `contact_list.rs:52`, `profile.rs:55`, `new_chat.rs:50`, `new_group.rs:94`) — see S1;
  `screening.rs`/`greeter.rs` show the right patterns.
- **[L] `new_chat.rs:93-95`, `profile.rs:278-280`, `screening.rs:90-92` — Signed-out paths return
  silently**; no retry when login completes later.
- **[L] Dialogs can be stacked repeatedly** — no `has_active_dialog` checks in any `open()`.
- **[L] `new_group.rs:137-140` — `GroupsEvent::Created { warning, .. }` discards the non-fatal
  warning.**

Positives: `limit(1)` newest-event pattern verified correct on both backends; entities created
before `open_dialog` per gpui-kit contract; `new_group.rs` has a solid error lifecycle; heavy work
consistently on background; no unused deps in the crate.

### 4.16 `ui` — shared components

- **[M] `avatar.rs:128-186` — `PixelAvatar` regenerates the full pattern every frame** (FNV-1a +
  xorshift 64-cell grid + ~50 child divs per avatar; ~30 visible avatars, redrawn on every
  keystroke) — the biggest hot-path cost in the main view. Memoize the deterministic pattern
  (seed → `[u8; 64]` cache); hoist `FALLBACK_SEED` to a `&'static str` path.
- **[L] `text.rs:11-38` — `InlineReplacement` is `pub` but unreachable externally**; same for
  `RUN_WINDOW_SECS` (`message.rs:28`). `pub(crate)` or document as extension points.
- **[L] `message.rs:766` — `RevealText::new` is the only public constructor without a doc line.**
- **[L] `message.rs:775-796` — `RevealText` re-splits text into a `Vec<SharedString>` every
  animation frame**; split once in `new`.
- **[L] `text.rs:89-95` — Non-web, non-nevent Nostr links are silently dead** (`npub`/`nprofile`
  clicks do nothing). Route to a profile view or drop the link markup.
- **[L] `Cargo.toml:15,19` — `web-time` and `uuid` unused.** Remove.

Positives: consistent `RenderOnce` + `Styled` builder pattern; `#[must_use]` builders; documented
public items; char-boundary-safe slicing; truncating casts provably bounded.

### 4.17 `auto_update`

- **[M] `lib.rs:92` — Checksum-only verification; no signing key configured.** SHA256SUMS comes
  from the same host as the artifact — protects against corruption, not a compromised release
  host/account. The engine already supports minisign/Ed25519; publish a sidecar signature and pin
  the public key before stable.
- **Verified non-issue:** the `futures::executor::block_on` in `source.rs:152,170` runs on the GPUI
  **background** executor (verified through `gpui-updater-pre 0.2.0`'s `Updater::check` →
  `engine.check()` → `fetch_latest` call chain) — no UI freeze. Severity Low: it parks one
  background thread up to ~60 s during a check. Optional tidy-up only.
- **[L] `source.rs:156-163,180-183` — Asset URLs not scheme-checked**; assert `https://`.
- **[L] `lib.rs:119-131,145-147` — A failed check shows for 5 s then reports "Up to date"**
  (`error_revert` → `Idle`, and `Idle` counts as up-to-date). Distinct "check failed" state.
- **[L] `lib.rs:103,128` — `.ok()` on entity updates** (see S2).
- **[L] `lib.rs:134-139` — Auto-download/stage ~2 min after startup without a prompt**; fine on
  unmetered, worth a setting.

Positives: strict `latest > current` gate (no downgrades); drafts/prereleases skipped; 30 s
timeouts; defensive checksum parsing; good tests.

### 4.18 `gpui_tokio`

- **[L] `lib.rs:14` — `.expect("Failed to initialize Tokio")`**; per the no-panic rule, `init`
  returning `Result` would be more in keeping (realistically unreachable).
- **[L] `lib.rs:38` — `pub struct Tokio {}`** namespace struct; `pub struct Tokio;` or free
  functions.

Positives: the `defer`-guard cancellation pattern is correct in both completion and drop cases;
`shutdown_background()` avoids blocking exit; `spawn_result` maps `JoinError` into `anyhow`.
No deadlock risk across the two runtimes (verified: nothing waits on GPUI from tokio or holds a
lock across the boundary).

### 4.19 `desktop` / `web` / `assets` entries

- **[M] `desktop/src/main.rs:20-26,153-170` — Secret key accepted as a CLI argument.** `--sec
  nsec1…` puts the master key in the process list (`ps`, `/proc`, shell history); the parse-error
  path echoes the raw (near-)key to stderr. Read from stdin/env; print error kind only, never the
  input.
- **[L] `desktop/src/main.rs:131-150` — Seven startup unwraps on embedded assets.** Deterministic,
  but one corrupt font aborts launch; skip bad fonts with a warning and fail only if zero loaded.
  `open_window().expect` is fair (genuinely unrecoverable). The startup `block_on` for fonts
  (main.rs:135) is intentional and acceptable — noted so it isn't confused with the anti-pattern.
- **[L] `web/src/lib.rs:122` — `expect("Failed to open window…")` aborts the wasm module** (blank
  page + console error; `panic = "abort"`). A DOM-level error would be kinder.
- **[L] `web/src/lib.rs:43` — `.ok()` on console-log init** (justified; noted for completeness).
- **[L] `desktop/Cargo.toml:37` + `web/Cargo.toml:13` — `ui` dep unused in both entries.** Remove
  unless it exists to force feature unification (then comment why).
- **[L] `assets/Cargo.toml:9` — `anyhow` unused** (code uses `gpui::Result`). Remove.

Positives: `web` opens IndexedDB before `run()`, panic hook + both logging stacks via `OnceLock`;
`assets` ships the theme-schema test.

## 5. Tooling Validation

| Check | Result |
|---|---|
| `cargo clippy --workspace --all-targets` | clean — zero warnings |
| `cargo fmt --all --check` | clean |
| `cargo check -p workspace`, `-p person` | pass |
| `let _ =` / `.ok()` sites (non-test) | ~156 — mostly entity-update-expected-error cases; see S2 |
| `tasks:` container declarations | ~48 sites across 20 owners — see S1 |
| Startup `block_on` sites | 3 (`state` DB open, `desktop` fonts — both acceptable-at-startup; `auto_update` — background thread, non-issue) |

## 6. Verified Non-Issues (checked, do not re-flag)

- `auto_update/source.rs` `block_on`: background executor only (verified through the
  `gpui-updater-pre 0.2.0` call chain) — no UI freeze.
- `nip29/src/protocol.rs:1072` `block_on`: test-only.
- No locks held across `.await` anywhere in the workspace (all audited crates).
- `person.rs:170` `let Ok(pubkey) = to_bech32()` — irrefutable (`Err = Infallible`).
- `stored_contact_list`'s `limit(1)` + `.next()` — correct: nostr-lmdb orders by `created_at`
  descending.
- `nip60`/`nip60_ui` have zero non-test `unwrap`/`expect`/`let _ =`; indexing is guarded; no
  truncating casts.
- `nip29_ui`'s `sync()` splice math traced against `ListState` semantics — correct (it's `chat_ui`
'  that has the mismatch).
- gpui-pre's entity read-tracking re-renders panels on `Directory`/`SpeechState` notifications —
  the missing-`cx.notify()` suspicion in speech handlers is fine; the stale-cache bugs above are
  real, the observation isn't.
- `nip60::init` startup ordering is sound for normal flows; the CLI-key path fires `SignerChanged`
  before wallet subscription exists (app-wide init-order caveat, affects all registries equally).
- `nip60` melt double-spend paths are latent (dead API), not live.

## 7. What's Done Well

- **Clippy-clean, fmt-clean workspace** with no reachable panics in `nip29`, `nip60`, `ui`,
  `nip29_ui`, `person` production paths; hostile-input discipline in the WoT builder, speech
  downloads (SHA-256 + size cap + atomic rename), and auto-update (fail-closed checksums, no
  downgrades).
- **`verify_subscriptions(true)` + `ban_relay_on_mismatch(true)`** in `state` is the single most
  valuable client-side trust boundary, and it's on.
- **Entity/subscription hygiene is strong overall**: subscriptions stored in `_subscriptions`,
  `WeakEntity` used for room/group/dock back-references, no strong entity cycles found anywhere,
  no dropped-subscription bugs.
- **Secrets discipline**: no secret is ever logged or published in `nip4e`/`nip60`/`state` desktop
  paths (verified against every log call); `Keys`' vendored `Debug` hides secrets.
- **Errors generally do reach the UI** where it matters most (group writes via `util::report`,
  wallet events, speech errors, import/restore flows) — the gaps are enumerated above.

## 8. Recommended Remediation Order

**P0 — before any release candidate (blocks release):**
1. nip60 deletion spoofing (4.2 #1) — one-line precedence fix + author check.
2. browser-signer-proxy auth/CORS/token + timeout cleanup (4.4 #1-2).
3. state startup DB fallback (4.1 #1) and web localStorage encryption (4.1 #2).
4. nip60 reconcile/republish money-safety trio (4.2 #2-4) + stuck `redeeming` (4.2 #5).
5. state file-cache collision (4.1 #4) — wrong-file disclosure.
6. chat_ui reaction double-count (4.7 #1) and common whitespace collapse (4.10 #1).
7. contact-list Update data loss (4.15 #1).
8. speech parakeet worker lifetime (4.12 #1).
9. settings load/save durability (4.13 #1-2).
10. nip29 `created_at` guards (4.8 #1-2).

**P1 — fix during the release cycle:**
Dock panel identity + "Ignore" dispatch (4.14 #1-2); updater observation (4.14 #3); invoice flow
or removal (4.3 #1); person `in_flight` release (4.11 #1); chat trash UI (4.6 #1); chat_ui load
errors + splice + quadratic insert (4.7 #2-4); profile URL validation, backup keyring surfacing,
new_chat re-filter, screening dialog cluster (4.15); parser recursion for wasm (4.10 #3);
`DefaultHasher` → stable hash before more room ids ship (S5); nip4e approval button + rotation
archive + listener death (4.5 #1-3); proxy plaintext leak + event-substitution check (4.4 #3-4).

**P2 — hygiene, in one mechanical sweep:**
S1 (`tasks` containers → replace-per-spawn / `detach_and_log_err`), S2 (`let _ =`/`.ok()` →
`.log_err()`), S6 (domain-keyed element ids), unused deps (`chat`: smol; `nip29`: person;
`nip60`: tokio; `ui`: web-time, uuid; `assets`: anyhow; entries: ui), dead code
(`nip60` melt/send/quotes decision, `state::passphrase_required`, `DownloadedFile::save`,
`dock::find_panel` fix-or-delete, `skin.rs` write-once flag, `Person` trait impls), avatar
memoization, sidebar row-model caching, rename `DeviceNotifcation`, summary-comment cleanup.

---

*Method note: every crate's sources were read in full by a dedicated audit pass; GPUI/fork API
claims were verified against the vendored `gpui-pre 0.3.12`, `gpui-base 0.9.1`,
`gpui-component 0.9.1`, and rust-nostr sources; the Critical and top-High findings were
independently re-verified by hand against the working tree at `8fa79aef`. No files were modified
during the audit.*

## 9. P0 Remediation Log (2026-11-12)

All ten P0 items fixed. Validation after the full batch: `cargo clippy --workspace --all-targets`
zero warnings, `cargo fmt --all --check` clean, `cargo test --workspace` 35 passed / 0 failed.
Diff: 15 files, +816/−273.

1. **nip60 deletion spoofing** — `nip60/src/lib.rs`: precedence fixed to
   `ours && (is_cashu || is_token_deletion(..))`; `absorb`/`absorb_deletion` now take `me` and
   reject foreign-authored kind 5 events (defense in depth; the live notification gate was the
   only protection before, and `absorb_deletion` had no author check). The DB load path was
   already author-filtered and is unchanged.
2. **browser-signer-proxy hardening** — per-session UUID token generated at startup, embedded in
   the served page (`inject_session_token`), required on `/api/*` (403 otherwise); exact `Host`
   match (DNS-rebinding defense); all CORS headers and the OPTIONS preflight removed;
   `cancel_request` removes timed-out requests from both maps so plaintext is dropped;
   centralized best-effort `write_response`. `proxy.js` authenticates all three fetches. Token
   never appears in the URL the app opens.
3. **state startup crash loop** — `NostrLmdb::open` failure now logs and falls back to
   `MemoryDatabase::unbounded()` (nostr-memory added to the desktop deps), emitting
   `StateEvent::error` via the existing deferred path; the `block_on` stays (safe: the LMDB open
   schedules nothing back onto the GPUI executor — now commented).
4. **state web localStorage key** — `import_bunker` on wasm32 no longer persists the bunker
   credential (session is memory-only; desktop keyring unchanged); `restore_session` degrades to
   `NoSigner` when nothing is stored (verified). **Known behavior change: web users re-enter their
   bunker URI after each reload.**
5. **state file-cache collision** — cache filename now derived from the decrypted content's
   sha256 instead of the filename; the metadata guard is gone and every store goes through
   temp-write + atomic rename.
6. **nip60 reconcile money-safety** — `ops.rs::reconcile` rewritten: relay tokens are converged
   into the store BEFORE any publish (closes the `del`-before-converge race); relay proofs already
   tracked locally (unspent or in-flight Pending/Reserved/PendingSpent) are never re-added, and
   unseen proofs are checked against the mint (NUT-08 `check_proofs_spent`, best-effort) before
   being accepted as Unspent; when a prior job errored after mutating the store
   (`publish_failed` flag), reconcile republishes the store's unspent set instead of trusting
   relay state — healing stranded sends/receives; without the flag the relay-trust removal path
   (cross-device spend convergence) is preserved. A failing Reconcile does not reschedule itself
   (no infinite loop).
7. **nip60 stuck nutzap redemptions** — `apply_outcome` now receives the failed job; on
   `RedeemNutzap` error the id is removed from `redeeming` and requeued up to
   `MAX_REDEEM_ATTEMPTS = 3` (cleared on success, reset on signer change), so transient mint
   failures no longer wedge redemptions until restart.
8. **Reactions double-count** — `chat_ui::insert_reaction` replaces per (author, target) per
   NIP-25, making the backup gift-wrap echo idempotent (verified: optimistic rumor and the
   re-delivered seal carry the same event id) and history reloads non-duplicating.
9. **Whitespace collapse** — `common/media_extractor.rs` rewritten: whitespace is tidied only at
   URL-removal boundaries; line breaks in untouched regions survive; regex hoisted to
   `LazyLock`; 9 unit tests added (multi-line preservation, URL-only messages, digit/emoji edges).
10. **Contact-list data loss** — `contact_list.rs` now parses the stored event into full
    `nostr::nips::nip02::Contact` values (relay hint + alias preserved through load → add/remove
    → republish); new contacts still get `Contact::new`.
11. **Speech worker lifetime** — `parakeet.rs`: the `OnceLock<Sender>` became a resettable
    `Mutex<Option<Sender>>`; a dead worker (30 s idle exit) is detected via `is_disconnected`, the
    slot cleared, and a replacement thread spawned lazily on the next command; spawn failure no
    longer caches a dead sender; failures surface through `sink.error` / returned `Err` instead
    of a debug-level swallow (one retry covers the liveness-check/send race).
12. **Settings durability** — a failed load logs and keeps defaults in memory without
    persisting (`suppress_next_save`, consumed by the next real change); fresh-install NotFound is
    no longer conflated with read/parse failures; saves are serialized through a single
    `save_task` slot (chained, FIFO order) with temp-file + atomic rename; serialization and
    write failures are logged.
13. **nip29 ordering guards** — shared `is_newer_event` (strict `(created_at, id)` total order):
    `Group::absorb_state` keeps a per-kind stamp (written only after a successful apply, so a
    malformed newer event can't poison the guard) and rejects stale replays while ascending cache
    hydration still applies; the registry's `list_stamp` seeds from the cached list event at
    startup, is checked in `merge` before applying, updated by `publish_list` (closing the
    leave→resurrect window), and reset on signer change. 3 new tests across protocol/group.
14. **nip60_ui invoice flow** — *not* in this batch: it is P1 (the audit lists it as High but the
    P0 list ranks it 16th via `nip60_ui/wallet.rs`); `mint_quotes()`/`claim_mint_quote()` remain
    dead until the P1 UI work lands. No other P0 code paths depend on it.

Behavior changes to be aware of when testing:
- Web: bunker credentials are no longer persisted (re-connect after each reload).
- Desktop: a corrupted/unreadable local database now starts with an empty in-memory history and
  shows an error event instead of crash-looping; relays re-sync as before.
- nip60: reconcile now performs one NUT-08 mint round-trip when unseen relay proofs appear.
- Reactions render per NIP-25 replace semantics (one reaction per author per target).
- Messages keep their line breaks; only URL-adjacent whitespace is tidied.

## 10. P1 Remediation Log (2026-11-12)

All eleven P1 items from §8 fixed on top of the P0 commit (`fd802a88`). Validation after the
full batch: `cargo clippy --workspace --all-targets` zero warnings, `cargo fmt --all --check`
clean, `cargo test --workspace` 38 passed / 0 failed. Diff: 25 files, +1072/−247.

1. **Dock panel dedup made real** (`workspace/src/dock/mod.rs`, `lib.rs`, `skin.rs`):
   `PanelKey` — `Named(panel_name)` for singletons, `Room(room_id)` for chat panels — recovered
   from docked panels via `PanelHandle::of` + downcasts; `add_panel` now focuses the existing
   tab (reopening a collapsed dock first) instead of stacking a duplicate; group panels dedupe
   through `Workspace.group_panels: HashMap<GroupKey, PanelId>` (group identity is not readable
   off a `GroupPanel`), with stale ids pruned via `area.panel(id)` validation. The dead
   `find_panel`/`move_panel`/`InsertTarget` machinery is deleted, along with `skin.rs`'s
   write-once `toggle_button_visible`. Known residual: the panel opened by the new-group dialog
   (`dialogs/new_group.rs`, still plain `add_panel_to`) isn't registered — P2.
2. **"Ignore" screening action** (`sidebar/rows.rs`, `dock/mod.rs`): closes the request room's
   tab via `dock::close_room_panel` (find by room key → `DockArea::remove_panel`) instead of
   dispatching `ClosePanel` into a focus path that can never reach its handler.
3. **Updater observation** (`sidebar/mod.rs`, `sidebar/header.rs`, `auto_update/src/lib.rs`):
   `AutoUpdater` now exposes `UpdateDisplay` (`up_to_date`, `staged`, `status` text) via
   `display()`; the Sidebar mirrors it into a small `Entity<Option<UpdateDisplay>>`, updating it
   only when a rendered fact actually changes, and the header renders from that snapshot (the
   restart click fetches the global entity at click time). Observing the whole updater and
   notifying the entire cached sidebar was rejected as too blunt: download-progress ticks would
   have invalidated the panel on every byte.
4. **Wallet invoice flow** (`nip60_ui/wallet.rs`, `nip60/src/lib.rs`): pending mint quotes render
   in the panel (amount, mint, bolt11 with middle-ellipsis, copy via the house `Clipboard`
   component, check/claim wired to the existing `Job::ClaimQuote` path, per-quote dismiss via the
   new `dismiss_mint_quote`); claim results refresh through the existing `WalletEvent::Updated`;
   an unpaid check emits "Invoice is not paid yet" (gated on `ClaimQuote` — flagged behavior
   change). Receive-via-lightning is functional end-to-end.
5. **Person fetch blacklisting** (`person/src/lib.rs`): `in_flight` is now shared
   (`Arc<RwLock<HashSet<_>>>`) with the batching task; all three duplicated `get_metadata` error
   arms factored into `flush_batch`, which releases the whole batch for retry on failure and
   logs. A 2 s timeout now costs a retry, not a session-long placeholder.
6. **Chat trash surfaced** (`chat/src/lib.rs`): the signal consumer counts *freshly* trashed
   messages (BTreeSet insert dedupes reprocess attempts) and emits one batched
   `ChatEvent::Error` ("N messages could not be decrypted"), which the workspace already renders
   as an error notification.
7. **chat_ui list fixes** (`chat_ui/src/lib.rs`): room connect/load failures now push persistent
   error notifications (`notify_failure`, `.autohide(false)`) instead of silently empty rooms;
   message inserts splice at the real row (`pos + 1`, welcome row accounted for — append case is
   byte-identical to the old behavior); bulk loads are O(n log n) sort-merge with a single splice
   and one index rebuild, preserving the old first-arrival-wins timestamp dedup exactly.
8. **Dialog fixes** (`workspace/src/dialogs/{profile,new_chat,screening}.rs`, `panels/backup.rs`):
   profile validates avatar/website URLs before publishing (inline error banner; empty still
   means "keep saved value"); backup surfaces keyring/decode failures in-panel and its bech32
   `unwrap`s are gone; new_chat re-filters on `InputEvent::Change` (and profile's avatar preview
   tracks typing); screening no longer closes all stacked dialogs on release, NIP-05 verification
   is tri-state (pending/verified/mismatched/failed — network failure no longer asserts "does not
   match"), and report submission shows failures.
9. **Parser recursion** (`common/src/parser.rs`): `NostrParserIter::next` is a loop; the
   pending-match fields are gone (provably passthrough); 50,000 consecutive malformed `nostr:`
   URIs now cost loop iterations, not stack frames (regression tests added) — wasm tab-crash
   vector closed.
10. **Stable room ids** (`common/src/event.rs`, `nip29`): `DefaultHasher` replaced by SHA-256
    (`common::stable_id` 128-bit hex, `stable_id_u64` for u64-keyed consumers); `Event` and
    `UnsignedEvent` `uniq_id` impls factored into one `pubkey_set_uniq_id` helper; nip29's
    `GroupKey::uniq_id` and `browse_subscription` unified (verified session-local). **Room id
    values change** (new hash domain) — follow-up fix after first run: `Room::get_messages`
    filters cached history by the persisted `r` tag, so pre-change rows were unreachable
    (empty message lists); `query_chat_rooms` now compares each row's `r` tag against the
    freshly computed id during its startup scan and re-tags stale rows, restoring history.
11. **nip4e lifecycle** (`nip4e/src/lib.rs`): approval has an `approving` re-entry guard (the
    device secret can no longer be double-sent), toast lifecycle is correct on both outcomes
    (success clears only the matching request; failure rebuilds the toast with a fresh enabled
    button); rotation aborts before overwrite when load/archive fails (surfaces via
    `DeviceEvent::error`); the relay listener logs dropped events and its own death instead of
    stopping silently.
12. **Proxy hardening round 2** (`browser-signer-proxy/src/lib.rs`): signed events are checked
    for correspondence (`event.compute_id()` vs returned id — proves field equality, covers
    stale ids), closing the event-substitution hole; the response log carries id + ok/error only,
    never values; request lines/headers are capped (16 KiB → 400), `Content-Length` capped
    (1 MiB → 413), invalid UTF-8 → 400, and connections are dropped after 30 s (slowloris
    bound).

### P1 behavior changes to be aware of when testing

- Opening an already-open room/group/panel focuses the existing tab; the "Ignore" action closes
  the request room's tab.
- Wallet panel shows a "Pending invoices" section; unpaid checks report "Invoice is not paid
  yet"; quotes are dismissable.
- Room groupings regenerate under a new id scheme — first launch after this change rebuilds
  rooms from cached rumors (history is kept; grouping self-heals).
- Profile saves with an invalid URL now stop with an inline error instead of silently dropping
  the field; NIP-05 shows "could not be verified" on network failure instead of "does not
  match"; undecryptable messages and room load/connect failures produce visible notifications.
- First message-send/receive after the room-id change republishes `r` tags under the new ids.

## 11. P2 Remediation Log (2026-11-12)

P2 hygiene processed with S1 (task-handle containers) and S2 (silent error discards) excluded by
request. Validation after the full batch: `cargo clippy --workspace --all-targets` zero warnings,
`cargo fmt --all --check` clean, `cargo test --workspace` 38 passed / 0 failed.

1. **Domain-keyed element ids (S6)** — sidebar rows use `Room.id` / group `cache_tag` / pubkey
   hex / compound message-hit digests instead of list indexes (`rows.rs`, `nav.rs` untouched);
   contact-list rows and remove buttons key on pubkey hex; greeter sections key on
   `request-/response-/follow-up-/reply-{room_id}`; nip29_ui content `TextView`, `MessageRow`,
   and `ReplyPreview` key on event-id hex with a `reply-` namespace (traced into gpui-base:
   `TextViewState`'s key is the whole element-id stack, so the row container id had to change
   too — this also kills the prepend-triggered markdown re-parse jank). Uniqueness arguments per
   list are in the work log; `chat_ui`'s identical index-keyed row pattern remains as a noted
   follow-up (audit S3).
2. **Unused dependencies removed** (each grep-verified before removal): `smol` (chat), `person`
   (nip29), `tokio` (nip60 — `gpui_tokio` keeps a superset), `ui` (desktop + web — plain path
   deps, no feature unification needed; ui stays in the graph via chat_ui/nip29_ui/nip4e/
   nip60_ui/workspace), `anyhow` (assets), `web-time` + `uuid` (ui). Cargo.lock rewritten by
   cargo only.
3. **Dead code removed** (each grep-verified): `state::passphrase_required` field + writes (UI
   uses `StateEvent::PassphraseRequired`); wasm-only `DownloadedFile::save`; `Person`'s unused
   `PartialEq/Eq/PartialOrd/Ord/Hash` impls (+ their imports); nip60's unreachable
   `melt()`/`send_token()` API with `Job::Melt`/`Job::SendToken`, `Outcome::Melted`/`Sent`,
   `WalletEvent::TokenCreated`, its emission, and workspace's misleading "copy it from the wallet
   panel" notification arm (plus the matching no-op arm in nip60_ui's exhaustive match).
   `dock::find_panel` dead machinery and `skin.rs`'s write-once flag were already removed in P1.
4. **Avatar memoization** (`ui/src/avatar.rs`): `AvatarPattern { hash, grid: [u8; 64] }` cached
   per seed in a `LazyLock<Mutex<HashMap<String, AvatarPattern>>>`; repeat renders skip FNV-1a +
   xorshift and only rebuild the divs; theme colors stay computed at paint time so output is
   bit-identical. The per-frame fallback-seed `String` is gone (`SharedString::new_static`).
5. **Sidebar row-model caching** (`sidebar/mod.rs`, `list.rs`): rows cached as
   `Rc<Vec<SidebarRow>>` behind two invalidation inputs — a `rows_dirty` flag set by chat/groups
   events and two new entity observes (catching `track_room`/`sort`/`reset`, which notify the
   registries without emitting events), and a cheap `RowsSignature` snapshot (catching room-kind
   promotion Request→Ongoing, which notifies only the Room entity, and pin/unpin, which mutates
   `AppSettings`' inner entity and notifies nothing the sidebar observes — both verified real
   hazards a flag-only design would have missed). Progress ticks, typing, and hover are verified
   cache-neutral; `find_input` verified as not a `rows_for` input. `group_view` member rendering
   left uncached deliberately: `GroupEvent::Updated` is over-emitted and not provably complete,
   so no safe cache exists without a membership-version counter.
6. **Rename** `DeviceNotifcation` → `DeviceNotification` (workspace/src/lib.rs, 5 sites). Note:
   nip4e defines its own correctly-spelled marker type, so the two crates' notification ids were
   already distinct; unifying them (so removals cross-dismiss) is a deliberate behavior change
   left for later.
7. **ui API tightening + RevealText**: `InlineReplacement`, `RenderedText::new`, and
   `RUN_WINDOW_SECS` → `pub(crate)` (verified unused externally); `RevealText::new` doc added;
   word-split moved from every animation frame into `new`.
8. **Summary-comment cleanup**: the five cited chat_ui "Define …" comments removed; all
   why-comments (including the O(1)-lookup one) kept. Remaining candidates flagged
   (`// Get room id and name`, `// Define attachments and replies_to entities`) left untouched
   per scope.

### P2 notes

- nip60's send/melt removal means there is currently no UI to melt to a lightning invoice or
  export a token string; both were unreachable before. Re-add together with their UI in a future
  feature change.
- Element ids: two known residuals documented (inbox.rs contact p-tag dedupe;
  `HashSet<Contact>` equality includes relay/alias, so the same pubkey with a different relay
  could render twice in the contact picker).
- S1/S2 (task-handle containers, `.ok()`/`let _ =` sweeps): reviewed and **declined** — judged not
  worth the churn; the affected sites are benign (entity-dropped-at-shutdown cases) and the
  containers are bounded in practice.
