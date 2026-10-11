# NIP-29 alignment: coop vs nostord

Comparison of our NIP-29 implementation (`crates/nip29`, `crates/nip29_ui`,
`crates/workspace`) against [nostord](https://github.com/nostrord/nostrord)
(`main` @ `bfd933e`), checked against
[NIP-29](https://github.com/nostr-protocol/nips/blob/master/29.md).

The two clients are architecturally similar: relay-hosted groups keyed by
`(relay, id)`, relay-generated `3900x` state events, user moderation events
`9000-9022`, and a NIP-51 `kind:10009` group list. They made different
trade-offs; each has real gaps the other closed.

## TL;DR

- We follow the spec more literally where nostord deviates: `previous`
  timeline refs, membership-from-9000/9001 (the spec's "implementation
  quirks" section), and fork/migration detection via admins' `kind:10009`.
  nostord implements none of those three.
- nostord is ahead on product surface: invite code listing/revocation, full
  LiveKit AV (token endpoint + `kind:39004`), threads UI, subgroup hierarchy
  management, member rosters as a membership source.
- Spec bugs in ours worth fixing: no `39002`/`39001` fallback for
  membership. (Since this comparison: `duplicate:` join rejection, 9008
  delete-group and reaction aggregation were implemented.)

## 1. Protocol compliance vs NIP-29

| Spec item | coop | nostord |
| --- | --- | --- |
| `h` tag on all user→group events | yes, every builder (`crates/nip29/src/protocol.rs`) | yes |
| Moderation kinds 9000/9001/9002/9005/9007/9009 | yes, all | yes, all |
| **9008 delete-group** | yes, plus confirm dialog | yes |
| **9010 update-pin-list + 39005** | send + parse + UI | stub ("disabled until the pinning backend exists") |
| `previous` tag (last 50 seen, excl. self, ≥3 × 8 chars) | `TimelineWindow` (`crates/nip29/src/protocol.rs`) | not built, not validated |
| Membership = latest of own 9000/9001 | `Membership::from_events` | derived from 39001/39002 rosters + local markers instead |
| Join request `code` tag / `naddr1…?invite=` | `GroupReference::parse` | yes, plus legacy formats (`wss://host'id`, `?relay=&group=`) |
| `duplicate:` 9021 rejection = already member | treated as success (`mark_member`) | matched by wording, treated as success |
| 39000 metadata: all flags + `supported_kinds` + `parent`/`child` | yes, plus banner and content-JSON fallback | yes, minus banner; tags only |
| 39001/39002/39003 state | parsed, admins used for actions | richer use (roster as membership source) |
| **39004 livekit participants + token endpoint (NIP-98 kind 27235)** | flag parsed/editable only | full flow incl. `/.well-known` support probe |
| Kind 10009 list | NIP-44 private section, public-wins, opaque-carry | plus foreign-tag preservation and relay-consensus freshness |
| Fork/migration detection (admins' 10009) | `watch_forks` + "may have moved" banner | none |
| Metadata author check (relay-signed 3900x) | parsed from any author | same |

Both clients ignore "clients should also check [`previous` refs] to keep
relays honest" — fine, it is a SHOULD. NIP-42 auth: we configure
`SignerAuthenticator` on the SDK client (`crates/state/src/lib.rs`), nostord
waits for AUTH explicitly before REQs; both work on auth-gating relays.

## 2. Where coop is ahead (keep these)

1. **Fork/migration detection.** The spec says clients SHOULD (MUST when the
   primary relay is down) watch admins' `kind:10009`.
   `GroupsRegistry::watch_forks` / `evaluate_forks` does this on a 15-minute
   cycle with a "may have moved" banner and a rejoin button. nostord has
   nothing here. Known limitation (see `docs/TODO.md`): the fork-detection
   subscription is unscoped, so NIP-29 group relays refuse it by policy; it
   currently works via generic and gossip relays only.
2. **Membership per the spec quirk.** Latest own 9000/9001 decides, with
   `Refused` / `Removed` states nostord lacks entirely.
3. **Pinning.** Full 39005/9010 round-trip, pin chips, admin toggle.
4. **Spec-shaped `previous` refs** and a metadata parser that tolerates
   content-JSON variants.
5. **Discovery primitives.** Contact-list `10009` sweep plus per-relay
   browse (`Discovery`, `GroupsRegistry::browse`). nostord deliberately has
   no relay browsing (join by address only). The browse core exists even
   though the UI only wires contact-based discovery.

## 3. Gaps, prioritized

### P1 — spec correctness / interop

1. ~~**`duplicate:` join rejection**~~ **done.** A `duplicate:`-prefixed
   publish failure on 9021 now marks the group membership as `Member`
   (`GroupsRegistry::join_key` → `Group::mark_member`).
2. ~~**9008 delete-group**~~ **done.** `GroupId::delete_group` builder,
   `Group::delete_group`, `GroupsRegistry::delete_group` (forgets the group
   on accept), and an admin-only "Delete group" button with a confirm
   dialog in `nip29_ui::actions`.
3. ~~**Reactions render as message bubbles.**~~ **done.** Kind-7 events no
   longer become rows: `Group::absorb` routes them to `push_reaction`, which
   aggregates them per target (`Group::reactions`); `nip29_ui` renders a
   grouped reaction bar under the target message. Deleting an event drops
   its reactions.

### P2 — client-parity features

4. **Invite management.** We create a 9009 code and copy the link, but never
   subscribe to 9009, so there is no list of active codes and no revoke
   (nostord: active codes = 9009 minus 9005 revocations, per-code copy and
   revoke). Natural fit: add 9009 to the group's state subscription and an
   "Invites" section in the details dialog.
5. **Membership fallback to rosters.** We only derive membership from
   9000/9001. If a relay prunes moderation history (or the user was added
   without us seeing the 9000), membership shows `Unknown` while 39002 lists
   us. Consider nostord's "external add" adoption: own pubkey in 39001/39002
   → treat as member (or invite card), without weakening the 9000/9001
   override.
6. **LiveKit.** The flag is parsed and editable, but joining an AV group does
   nothing. At minimum: the `/.well-known/nip29/livekit` probe (204) to gate
   the toggle, and the token flow
   (`/.well-known/nip29/livekit/<id>` with NIP-98 kind 27235 plus a 39004
   participant subscription) if AV is on the roadmap.
7. **Subgroup management UI.** `parent` is set at creation (`CreateChannel`),
   and `edit_metadata` already carries `parent`/`child` tags, but the edit
   dialog cannot reparent or reorder children (the spec's 9002 child-list
   replace semantics). nostord has a full Hierarchy tab (move under / make
   root / reorder). Nothing reads NIP-11 `nip29.subgroups` to gate the UI.
8. **No reply/thread UI.** `send_message` accepts `replies` but
   `GroupPanel::send` always passes empty; kind 11/1111 render inline only.
   nostord has reply chips, a threads pane, and NIP-22 tag nesting.

### P3 — polish

9. **10009 foreign tags.** We rebuild only `group`/`r` tags on publish
   (`GroupList::public_tags`), dropping anything other clients stored in the
   event; nostord preserves unknown tags verbatim. Cheap fix, avoids
   clobbering.
10. **No `previous` validation on incoming events** (spec SHOULD) and no
    client-side late-publication guard — mostly relay-side concerns.
11. **Self-demotion guard.** Member roster UI lives in the sidebar
    (`crates/workspace/src/sidebar/group_view.rs`: promote/demote/remove) at
    parity with nostord minus roles beyond `admin` (both clients only assign
    one role). nostord additionally blocks self-demotion/self-removal —
    worth copying to avoid locking yourself out.

## 4. Suggested order of work

1. ~~Fix `duplicate:`-prefix handling on 9021~~ done.
2. ~~Exclude/aggregate kind-7 reactions in group rows~~ done.
3. ~~Add 9008 delete-group + confirm dialog~~ done.
4. Subscribe 9009 → invite list + revoke via 9005 (medium).
5. 39002/39001 membership fallback (medium).
6. Subgroup reparent/reorder in the edit dialog (medium, uses the existing
   9002 path).
7. LiveKit probe + token flow (larger; only if AV is in scope).
