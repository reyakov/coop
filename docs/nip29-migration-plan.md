# NIP-29 migration plan

`crates/nip29` is complete and reviewed; `docs/nip29-usage.md` is its API contract. This
plan replaces the Concord stack — `crates/concord`, `crates/community` and
`crates/community_ui` — with it, and lays out the UI surfaces the backend was built for.

Every phase ends with the app building and running. The last phase deletes the old crates,
so nothing is removed until the replacement is in place.

## What is being replaced

| Today | Replacement |
| --- | --- |
| `crates/concord` (`cord01`–`cord06`, `state`, `derive`) | deleted — the relay owns identity, membership, roles and history |
| `crates/community` (`CommunityRegistry`, `Community`, `sync`, `cache`, `rekey`, `history`) | `crates/nip29` (`GroupsRegistry`, `Group`) — already built |
| `crates/community_ui` (`CommunityPanel`) | new `crates/nip29_ui` (`GroupPanel`) |
| Sidebar `Communities` tab + two-level community view (channels / admins / members) | one-level `Groups` tab; admins and members move into a details dialog |
| `dialogs/new_community.rs` | `dialogs/new_group.rs` and a new `dialogs/join_group.rs` |
| `panels/browse.rs` stub | a working browse panel over `GroupsRegistry::browse` |
| `settings.recent_communities` (dead, nothing reads it) | removed |

## Capability disposition

| Concord capability | On NIP-29 |
| --- | --- |
| End-to-end encrypted channels | Dropped. NIP-29 groups are relay-readable; `private` only means members-only read. |
| Channels inside a community | Dropped. A group is one timeline; subgroups are the only hierarchy. |
| Roles and control plane | Relay owns them (`39001`, `39003`); the client reads and displays, never derives authority. |
| Rekey, rotation, refounding, dissolution | Dropped with `cord06`; leaving and deleting are relay operations. |
| Invites | `9009` `create-invite` + the `?invite=` suffix on a group `naddr`. |
| Pins | `9010` → relay mirrors `39005`; the backend is ready. |
| Message deletion | `9005`; the local eviction lands in Phase 0. |
| Reactions | Kind `7` rows arrive as ordinary timeline rows; grouping them under the target is a later enhancement. |
| Members list | `39002` when the relay publishes it; may be absent or partial by design. |
| Community list | `10009`, owned entirely by the backend. |
| Multi-relay communities | One relay per group; migration and forks surface through `Group::elsewhere()`. |
| Attachments | Any kind the relay supports; v1 renders media URLs already parsed by `chat::Message`. |

## Rules the implementation keeps

Carried over from the backend review; they apply to every phase.

- New code goes into `crates/nip29` and `crates/nip29_ui`. Never create a `groups` crate.
- Implementation-block style: methods on the type they belong to, `self` first.
- Read model is subscribe → pump → envelope cache → hydrate. Never `fetch_events`.
- Short-lived subscriptions close on EOSE (`SubscribeAutoCloseOptions`), long-lived ones use a
  prefix the pump recognises; dedup events by id before they mutate state.
- I/O runs on `cx.background_spawn`, called from a foreground `cx.spawn` task; tasks are
  stored on the entity so they cancel with it.
- No comments, no docs, no tests inside the crates. `docs/` is the only place for prose.
- No premature optimization: scan the small vectors, don't build index maps.
- No `unwrap`, no `let _ =` on fallible calls; propagate with `?` and surface errors to the UI
  (relay messages verbatim).
- Views read entities; protocol state never lives in a view. A panel re-reads on the event.
- UI follows the existing components (`ui::button`, `ui::input`, `ui::dock`, `ui::message`,
  `ui::markdown`, `theme` tokens). Copy is sentence case; a dialog names its object
  (`Leave "Pizza Lovers"?` with a `Leave` button).
- `Monitor` stays unadopted; the fork watch already works without relay status.

---

## Phase 0 — `9005` deletes reach local state

Today a `9005` stops the relay serving the event, but the envelope cache and the timeline keep
it. Everything else about deletion is done (the write, the `OK false` path).

Files: `crates/nip29/src/protocol.rs`, `crates/nip29/src/group.rs`, `crates/nip29/src/lib.rs`.

1. `TimelineWindow::remove(&mut self, id: EventId)` — drop the reference so a `previous` tag
   never points at a deleted event.
2. `Group::forget(&mut self, id: EventId, cx: &mut Context<Self>)` (crate-private) —
   `messages.retain`, `index.remove`, `window.remove`; when something was removed, emit
   `GroupEvent::Updated` and `cx.notify()`. `hydrate` stays additive, so a later reload cannot
   resurrect the row once its envelope is gone.
3. The registry's ingestion of our accepted delete: in `ensure`'s `GroupEvent::Sent` arm, when
   `event.kind == Kind::Custom(9005)`, take the target from `event.tags.event_ids().next()`,
   delete its envelope (`Kind::ApplicationSpecificData` + `Tag::identifier(target)` +
   `r = key.cache_tag()`) on the background executor, wait for that to finish, then
   `group.forget(target)` and the usual `reload(key, Some(event))`. The deletion must complete
   before the reload reads the envelopes, or the reload would see the target still cached.

Remote deletes (another admin's `9005` arriving live) are out of scope; the standing REQ does
not carry them, and a relay that no longer serves the event cannot be asked for it.

Validation: `cargo clippy -p nip29 --all-targets`, `cargo fmt -p nip29`,
`cargo check -p coop -p coop_web`. Manually: as an admin, delete a message and confirm the row
disappears and stays gone after a restart.

---

## Phase 1 — `crates/nip29_ui`: the group panel

A new crate, modeled on `community_ui`, that renders one `Entity<Group>`.

Files: `crates/nip29_ui/Cargo.toml`, `crates/nip29_ui/src/lib.rs`,
`crates/nip29_ui/src/message.rs`; one line in `crates/chat_ui/src/lib.rs`.

```toml
# Cargo.toml
[dependencies]
nip29 = { path = "../nip29" }
chat = { path = "../chat" }
chat_ui = { path = "../chat_ui" }
state = { path = "../state" }
ui = { path = "../ui" }
theme = { path = "../theme" }
common = { path = "../common" }
person = { path = "../person" }
settings = { path = "../settings" }

gpui.workspace = true
nostr-sdk.workspace = true
anyhow.workspace = true
smallvec.workspace = true
```

Make `chat_ui`'s text helper shared by flipping `mod text;` to `pub mod text;`. It is the only
change to an existing UI crate in this phase.

```rust
pub fn init(group: Entity<Group>, window: &mut Window, cx: &mut App) -> Entity<GroupPanel>;

pub struct GroupPanel {
    id: SharedString,                                     // "group-<cache_tag>"
    focus_handle: FocusHandle,
    group: WeakEntity<Group>,
    list_state: ListState,
    rendered_texts_by_id: BTreeMap<EventId, RenderedText>,
    input: Entity<TextareaState>,
    tasks: SmallVec<[Task<Result<()>>; 4]>,
    _subscriptions: SmallVec<[Subscription; 2]>,
}
```

The panel keeps **no copy of the timeline**. Rows are `group.read(cx).messages()`; the only
local cache is parsed markdown keyed by event id. Deleting a message, a page arriving, or a
live tail all reduce to "the entity changed, re-read it" — no merge, no index.

Rows: `0` welcome, `1` load-older, then the messages.

- Welcome — `WelcomeMessage` with the group avatar; `Welcome to {name}` and
  `This is the start of {name}.`
- Load older — visible when `group.oldest().is_some()`, disabled while
  `group.loading_more()`, click → `GroupsRegistry::global(cx).update(|registry, cx| registry.load_more(&key, cx))`.
- Message — `MessageRow` with avatar, author (`PersonRegistry`), `created_at.to_human_time()`,
  and `chat_ui::text::rendered_text(&message.content, &message.mentions, &persons, true, cx)`
  cached in `rendered_texts_by_id`. Show the author on a run start (same author, gap under a
  few minutes) like `community_ui` does. Render `message.media` as chat does: one image
  full-width, several in a wrapped row.

Events: subscribe to the group and drive everything from it.

| `GroupEvent` | Panel |
| --- | --- |
| `Incoming` / `Updated` | sync `list_state` (`reset(item_count)` when the count changed) and `cx.notify()` |
| `Eose` | `cx.notify()` — `group.loading()` is now false |
| `Refused(reason)` | show the notice |
| `Sent` | ignore; the registry reloads through the same cache path |

Composer, gated exactly on the NIP's flags:

```rust
let metadata = group.metadata();
let supports_text = metadata.is_none_or(|m| m.supports(Kind::ChatMessage));
let writable = metadata.is_none_or(|m| !m.restricted) || matches!(group.membership(), Membership::Member);
let can_send = supports_text && writable;
```

Send mirrors `community_ui::send`: read `input.value()`, clear the textarea, spawn
`group.send_message(&text, vec![], cx)` and turn an `Err` into an
`autohide(false)` notification — that string is the relay's refusal, verbatim. The panel does
not reload anything; `GroupEvent::Sent` already routes the accepted event through the registry.

Notice above the composer (and centered when there are no messages):

| State | Notice | Action |
| --- | --- | --- |
| `group.refused()` | Relay message | none |
| not a member (`Unknown` / `Removed` / `Refused { reason }`) | `Request to join`, preceded by the refusal reason when there is one | `registry.join_key(key, None, cx)` |
| any of the above while `metadata.closed` | `This group is closed; an invite link is required.` | disabled |
| `Membership::Pending { .. }` | `Waiting for the relay to approve.` | disabled |
| `!supports_text` | `This group doesn't support text messages.` | disabled |

`Panel` impl: `panel_id` is `group-{cache_tag}` so a group opens once; `title` is the avatar
plus the name; `toolbar_buttons` starts empty (Phase 4 fills it).

Validation: `cargo clippy -p nip29_ui --all-targets`, `cargo fmt -p nip29_ui`,
`cargo check -p coop -p coop_web`. The crate is not reachable from the UI until Phase 2, so
review is by code and compile.

---

## Phase 2 — the sidebar and its dialogs

The `Communities` tab becomes one level: a list of groups, a `New Group` dialog and a
`Join Group` dialog. The two-level community view (channels, admins, members, the back
button) is removed; with no channels it has nothing to show, and details move to Phase 4's
dialog.

Files: `crates/workspace/src/sidebar/mod.rs`, `crates/workspace/src/sidebar/tab.rs`,
`crates/workspace/src/lib.rs`, `crates/workspace/src/dialogs/mod.rs`,
`crates/workspace/src/dialogs/new_group.rs` (replaces `new_community.rs`),
`crates/workspace/src/dialogs/join_group.rs`, `crates/workspace/Cargo.toml`.

Add `nip29` and `nip29_ui` to the workspace crate.

`tab.rs`: `Communities` → `Groups`, label and list title `Groups`, list id `sidebar-groups`.
`lib.rs`: `Command::NewCommunity` → `Command::NewGroup`; add `Command::JoinGroup`; both arms
open the dialogs below.

`mod.rs`:

- `SidebarRow::Community { community }` → `SidebarRow::Group { group: Entity<Group> }`.
- Delete `CommunitySection`, `CommunityRow`, `community_scroll`, `channels_open`,
  `admins_open`, `members_open`, `Sidebar::community`, `reset_community`,
  `render_community`, `render_community_rows`, `section_row`, `channel_row`, `member_row`,
  and the back button in `render_user`. `render` loses its community branch.
- `rows_for(Groups)` reads `GroupsRegistry::global(cx).read(cx).groups()` in registry order
  (the `10009` order, then joins).
- The group row: name from `metadata.name()`, falling back to `key.id().as_str()`; avatar from
  `metadata.picture`, seeded with `key.cache_tag()`; suffix for the membership state —
  `Requested` (`Pending`), `Refused`, `Removed`, and `Moved` when `elsewhere().is_some()`;
  nothing for a plain member. Click opens
  `PanelHandle::new(nip29_ui::init(group.clone(), window, cx))` in `DockPlacement::Center`.
- Subscriptions: replace the `CommunityRegistry` subscription with `GroupsRegistry` —
  `GroupsEvent::Updated` → `cx.notify()`, `GroupsEvent::Error(error)` → notification.
- `render_tabs`'s Groups section: `New Group` (`Command::NewGroup`), `Join Group`
  (`Command::JoinGroup`), `Browse` (`Command::ShowBrowse`, still the Phase 3 stub).
- Remove `dialogs/new_community.rs` and its `pub mod` line — nothing dispatches it any more.

The two dialogs, both modeled on `new_community.rs`:

**New group.** Three inputs — name, group id, relay — with ok/validation:

```rust
let id = GroupId::new(if raw_id.is_empty() { slug(&name) } else { raw_id })?;
let relay = RelayUrl::parse(relay_input)?;
registry.create(GroupKey::new(relay, id), GroupMetadata { name: Some(name), ..Default::default() }, cx);
```

A parse failure keeps the dialog open and pushes a notification. The relay's answer arrives
asynchronously as `GroupsEvent::Error(reason)` — the sidebar already shows it. There is no
default relay; the account has no "groups relay" setting yet, so the field starts empty.

**Join.** One naddr input:

```rust
let reference = GroupReference::parse(input)?;   // accepts naddr1…?invite=CODE
GroupsRegistry::global(cx).update(cx, |registry, cx| registry.join(reference, cx));
```

Validation: `cargo check -p workspace`, `cargo clippy -p workspace --all-targets`,
`cargo fmt -p workspace`, then run the app: the tab lists whatever the account's `10009`
holds, no Concord screen is reachable from it, and a pasted naddr joins and opens.

---

## Phase 3 — browse panel

`panels/browse.rs` stops being a stub. `init` takes the dock (`WeakEntity<DockArea>`) the way
`search::init` does, so a joined group can be opened directly.

- Header: relay `Input` + a `Go` button; Enter submits. `RelayUrl::parse` → `registry.browse(relay, cx)`.
- Body: `registry.browsed(&relay)` — a spinner while `Browse::loading`, then rows of
  `GroupCandidate`: avatar, name (or id), `id@relay`, and `Join`
  (`registry.join_key(candidate.key, None, cx)`, then open the panel). Rows for keys already
  in `registry.groups()` show `Joined` instead.
- Subscribes to `GroupsEvent::Updated` and re-renders. Empty state: `No groups found.`;
  before a relay is entered: `Enter a relay to browse its groups.`

Validation: `cargo check -p workspace`; manually browse a public NIP-29 relay and join from a
result, and create a group on a relay that accepts `9007`.

---

## Phase 4 — details, delete, pins, leave

Read-mostly group management, the surfaces the sidebar gave up in Phase 2 plus the two writes
that need Phase 0.

- **Group details** — a dialog opened from the panel's header row: name, picture, banner,
  about, flags (`private`, `restricted`, `hidden`, `closed`, `livekit`, `parent`, `children`),
  admins with their role labels (`Group::admins().entries()`), the roles the relay advertises
  (`Group::roles()`), members when the relay publishes `39002` (`Group::members()`), and pins
  (`Group::pins()`). Read-only in this phase.
- **Delete a message** — admin-only entry in the message row's context menu (current user in
  `admins()`) → `group.delete_event(id, cx)`; Phase 0 makes the row disappear.
- **Pin / unpin** — the same menu: `group.update_pins(pins, cx)` with the current list plus or
  minus `Pin::Event(id)`; show `Group::pins()` as a strip above the list.
- **Leave** — header `Leave…` opens a confirmation naming the group (`Leave "Pizza Lovers"?`);
  on confirm `registry.leave(&key, cx)` and the panel closes (dispatch `ClosePanel`, as the
  screening dialog does).
- **Fork / migration banner** — when `group.elsewhere()` is `Some(moved)`, a banner:
  `This group may have moved to {relay}.` with a `Join` button calling
  `registry.join_key(moved, None, cx)`. Never auto-join: the NIP asks the client to offer.

Validation: same commands; manually delete a message, pin and unpin, leave and re-join.

---

## Phase 5 — admin writes

The remaining moderation surface, all through the calls the backend already exposes.

- **Edit metadata** — a dialog opened from the panel header's `Manage` menu, started from
  `group.metadata().cloned().unwrap_or_default()`: name, about, picture, the flags. `children`
  is preserved as-is — `edit_metadata` re-emits the full list, and a `9002` that drops one is
  rejected by the relay.
- **Invite** — `create_invite(code, cx)` with an input or generated code, then
  `metadata.naddr(relay, Some(code))` copied to the clipboard (`naddr` needs parsed metadata;
  a group whose relay publishes no `39000` cannot mint a link).
- **Users and roles** — add by `npub` → `put_user(pubkey, roles, cx)`, remove →
  `remove_user(pubkey, cx)`; role choices come from `Group::roles()`. The relay is the
  authority for both the roles it supports and who may call these.
- **React** — a `React` submenu in the same row menu sends `send_reaction(target, emoji, cx)`;
  a kind `7` row appears inline (that is the current backend behaviour). Unlike the items above
  it is not admin-only, so the menu now exists for every member.

Validation: same commands; manually exercise each write against a relay that grants the
account admin, and confirm the refusal path shows the relay's message.

---

## Phase 6 — delete the Concord stack

Nothing in the app may reference Concord before this phase starts.

1. Delete `crates/community`, `crates/concord`, `crates/community_ui`.
2. `crates/workspace/Cargo.toml`: drop `community` and `community_ui`.
   `desktop/Cargo.toml` and `web/Cargo.toml`: drop `community`; `nip29` stays.
3. `desktop/src/main.rs` and `web/src/lib.rs`: drop `community::init(cx)` and its comment.
4. `crates/settings/src/lib.rs`: drop `recent_communities` and `record_recent_community` —
   nothing reads them (verified), and the concept no longer exists.
5. `docs/nip29-usage.md`: replace the "Not wired up yet" section. Remaining known gaps then
   are: LiveKit's response field names (a guess until checked against a real relay), the AV
   room itself, local handling of remote `9005`s, the subgroup tree, and reaction grouping.
6. Confirm no `concord`/`community` string remains outside `docs/` history, and run a full
   `cargo check -p coop -p coop_web` plus `cargo clippy --workspace --all-targets`.

---

## Later (not part of the migration)

- **LiveKit** — `Group::livekit_supported` and `Group::livekit_session` are ready; check
  `metadata.livekit`, then build the AV room UI. Verify the token response fields first.
- **Subgroups** — `parent`/`children` are parsed; assemble the tree in the sidebar when the
  relay advertises `nip29.subgroups`.
- **Replies and threads** — `send_message` already takes reply ids and `chat::Message` reads
  them back; the panel needs a reply affordance and a quoted row.
- **Remote deletions** — fold a `9005` seen live into `Group::forget`, the same eviction as
  Phase 0.
- **Recent groups** — if a local recency list is wanted, store group keys, not the removed
  `recent_communities` strings.

## Validation checklist (every phase)

```sh
cargo clippy -p <touched-crates> --all-targets
cargo fmt -p <touched-crates>          # check the exit code, not the output
cargo check -p coop -p coop_web
```

Plus the manual smoke for the phase, run in the app. The wasm target is not build-verified in
this repo; `coop_web` check is the proxy.

## Decisions taken

- The tab is renamed `Groups`; the vocabulary is the NIP's.
- The sidebar keeps one level; admins and members move to the details dialog.
- The panel reads `Group` directly and keeps no timeline copy.
- Mentions are rendered (shared `chat_ui::text::rendered_text`); replies and attachments are
  not offered for sending in v1.
- `Join` never happens implicitly — not from `10009` alone, and not from `elsewhere()`.
- The old crates are deleted only at the end, so any phase can be stopped and the app still
  runs.
- The dock renders no per-panel toolbar — `Panel::toolbar_buttons` is defined but never called —
  so the group's `Details` and `Leave` actions live in a header row inside the panel.
- The fork banner's button joins the target group rather than opening it: the panel holds no
  dock handle and the plan's call is `join_key` alone; the joined group then appears in the
  sidebar.
- The admin writes sit behind a `Manage` menu in the panel header, shown only while the current
  user is in `Group::admins()`; the details dialog stays read-only. The message row menu is now
  built for every member — `React` is not moderation — and only its `Pin`/`Delete` items are
  gated on being an admin.
