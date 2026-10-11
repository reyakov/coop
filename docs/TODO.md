# TODO

Open items. Each entry notes the known tradeoff and where the fix would land.

## nip29

- [ ] **Incremental event handling for open groups.** Live events for an open
  (active) group still trigger a full envelope reload and re-hydrate of the
  whole group on every event (`GroupsRegistry::reload` in
  `crates/nip29/src/lib.rs`). Closed groups already absorb events in O(1);
  open groups could use the same incremental path, keeping the full reload
  only as an occasional self-healing rebuild.
- [ ] **Fetch admins' group lists from NIP-29 relays.** The fork-detection
  query (`GroupsRegistry::sync_forks`) is unscoped, so group relays refuse it
  by policy (`must have 'h', 'e' or 'a' tag`). It still works via generic and
  gossip relays, but the deeper fix is a per-relay scoped subscription using
  an `a` tag with `10009:<admin-pk>` coordinates. The exact coordinate format
  each relay accepts needs to be tested first.
- [ ] **LiveKit AV.** The `/.well-known/nip29/livekit` probe gates the edit
  toggle, but the token flow (`/.well-known/nip29/livekit/<id>` with NIP-98
  kind 27235) and a `kind:39004` participant subscription are missing — both
  only matter once an AV client is on the roadmap.
- [ ] **Threads pane.** Replies to kind 11 messages send and render
  (composer target + reply-context line), but there is no dedicated threads
  view like nostord's.
- [ ] **NIP-11 `nip29.subgroups` gating.** The subgroup UI (parent edit,
  child reorder) is always shown; relays that don't support subgroups reject
  the 9002. Reading the relay information document would gate it.
- [ ] **Invite sync window.** The moderation subscription replays the last
  100 moderation events, so invites older than that window appear in the
  list only once they enter the local envelope cache.
- [ ] **`previous` mismatches are advisory.** Incoming events whose
  `previous` references weren't seen from the relay are logged
  (`Group::absorb`) but still rendered: partial history makes dropping
  false-positive-prone. Late-publication rejection remains relay-side, per
  the spec.
- [ ] **Download stall watchdog.** The auto-updater's streamed download
  (`auto_update::download`) relies on the HTTP client's defaults; a
  mid-transfer stall hangs the progress loop instead of erroring out.
