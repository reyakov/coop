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
- nostord is ahead on product surface: full LiveKit AV (token endpoint +
  `kind:39004`) and a threads pane. Invite code management, subgroup
  reparent/reorder and reply chips landed here since the comparison.
- Spec bugs in ours worth fixing: none open. The `duplicate:` join
  rejection, 9008 delete-group, reaction aggregation and the `39002`/`39001`
  membership fallback were implemented.

## 1. Protocol compliance vs NIP-29

| Spec item | coop | nostord |
| --- | --- | --- |
| `h` tag on all user→group events | yes, every builder (`crates/nip29/src/protocol.rs`) | yes |
| Moderation kinds 9000/9001/9002/9005/9007/9009 | yes, all | yes, all |
| **9008 delete-group** | yes, plus confirm dialog | yes |
| **9010 update-pin-list + 39005** | send + parse + UI | stub ("disabled until the pinning backend exists") |
| `previous` tag (last 50 seen, excl. self, ≥3 × 8 chars) | `TimelineWindow` (`crates/nip29/src/protocol.rs`) | not built, not validated |
| Membership = latest of own 9000/9001 | `Membership::from_events`, rosters resolve `Unknown` as fallback | derived from 39001/39002 rosters + local markers instead |
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

4. ~~**Invite management.**~~ **done.** The group subscription now includes a
   moderation filter (`9009` + `9005`, `GroupId::moderation_filter`),
   `Group::invites()` exposes active codes (revocations arrive as 9005
   deletions and via the publisher's own `Sent` path), and the invite dialog
   lists codes with per-code copy-link and revoke.
5. ~~**Membership fallback to rosters.**~~ **done.** `Group::
   adopt_roster_membership`: while the latest own 9000/9001 leaves membership
   `Unknown`, being listed in 39001/39002 counts as `Member`. `Pending`,
   `Refused` and `Removed` are never overridden.
6. **LiveKit — partially done.** The edit dialog probes
   `/.well-known/nip29/livekit` (expecting 204) and gates the toggle with a
   description when the relay doesn't announce support. Remaining: the token
   flow (`/.well-known/nip29/livekit/<id>` with NIP-98 kind 27235) and a
   `kind:39004` participant subscription — both only useful once an AV
   client is on the roadmap.
7. ~~**Subgroup management UI.**~~ **done.** The edit dialog has a Parent
   field (empty = root; reparenting) and a children list with Up/Down
   reordering (the spec's 9002 child-list replace semantics). Self-parent and
   parent-is-child are rejected client-side; NIP-11 `nip29.subgroups` is not
   read yet.
8. **Reply UI — partially done.** The composer can target a reply
   (context menu → Reply, chip with cancel), sends kind 11 with `e` tags, and
   messages render a reply-context line (author + first line) resolved from
   loaded rows. Remaining: a dedicated threads pane (nostord has one).

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
4. ~~Subscribe 9009 → invite list + revoke via 9005~~ done.
5. ~~39002/39001 membership fallback~~ done.
6. ~~Subgroup reparent/reorder in the edit dialog~~ done.
7. LiveKit probe (done, gates the toggle) + token flow (larger; only if AV is
   in scope).
8. Reply sending + chips (done); a dedicated threads pane remains.
