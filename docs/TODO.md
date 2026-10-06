# TODO

Open items from the search and lazy-loading work. Each entry notes the known
tradeoff and where the fix would land.

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
