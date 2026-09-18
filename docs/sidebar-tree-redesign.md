# Sidebar tree redesign

Status: steps 1-5 implemented. Search now lives in `panels/search.rs`; the
sidebar renders the nav rail and the flattened tree. Remaining: step 6 (pin UI),
step 7 (community rows are already rendered from dummy data, tracked by the
`TODO(concord)`), optional step 8 (persistence), step 9 (cleanup of the step-6
dead code).

Scope: `crates/workspace/src/sidebar` (`mod.rs`, `entry.rs`, new `tree.rs`),
new panel shells in `crates/workspace/src/panels/`, and the `Command` wiring in
`crates/workspace/src/lib.rs`. New icons in `crates/ui/src/icon.rs` and
`assets/icons/`.

Related: `docs/concord-usage.md` — Community is placeholder data until a
`ConcordRegistry` exists (that document describes the backend shape; none of it
is wired up yet).

## 1. Goal

Replace the segmented filter (Inbox / Requests) plus flat room list with a
collapsible tree, and give the sidebar a nav rail whose items open dock panels.

The sidebar body is always the tree. Search is not part of it: the find input,
results, and contacts move to a Search panel (relocation lands here, step 5;
the panel is owned separately afterwards).

Target layout:

```text
┌ sidebar ───────────────────────────────┐
│ [avatar] user menu                     │  render_user (unchanged)
│                                        │
│  Inbox                                 │  opens Inbox panel
│  Browse                                │  opens Browse panel
│  Search                                │  opens Search panel
│                                        │
│  ▾ Pinned                     (2)      │  hidden when empty
│     ○ alice                            │
│     ○ team chat                        │
│  ▸ Requests                            │  collapsed by default
│  ▾ Community                           │
│     ○ Coop Contributors                │  1-3 dummy entries
│     ○ Nostr Design                     │
│  ▾ Messages                            │
│     ○ bob                              │  RoomKind::Ongoing
└────────────────────────────────────────┘
```

## 2. Current state

| Piece | Where |
| --- | --- |
| Sidebar view, search, filters, room list | `crates/workspace/src/sidebar/mod.rs` |
| Room row element | `crates/workspace/src/sidebar/entry.rs` (`RoomEntry`) |
| Room kinds and lookup | `crates/chat/src/lib.rs` (`ChatRegistry::rooms/count/room`), `crates/chat/src/room.rs` (`RoomKind::{Request, Ongoing}`) |
| User row, dropdown menu | `Sidebar::render_user` (keep as is) |
| Dock panels | `crates/workspace/src/panels/` (`greeter`, `profile`, `contact_list`, ...) |
| Panel opening + commands | `crates/workspace/src/lib.rs` (`Command`, `Workspace::on_command`, `add_panel_to_dock`) |
| Panel dedupe/focus | `crates/ui/src/dock/mod.rs` (`add_panel` finds by `panel_id` and moves/focuses) |
| Buttons, icons, tooltips | `crates/ui/src/button.rs`, `crates/ui/src/icon.rs` |
| Split button / dropdown primitives | `crates/ui/src/menu/` (`DropdownMenu`, `PopupMenu`, `PopupMenuItem`) |
| App settings persistence | `crates/settings/src/lib.rs` (`Settings`, `setting_accessors!`) |

Behavior to preserve:

- `RoomEntry` click emits `ChatEvent::OpenRoom` through
  `ChatRegistry::emit_room`, and shows the screening modal for non-ongoing
  rooms (`entry.rs`).
- `ChatEvent::Ping` sets `new_requests = true`, drawn as a dot on Requests.
- The dock-facing `Panel`/`Focusable` impls on `Sidebar` stay untouched.
- Search behavior is preserved by moving it, not rewriting it (§7).

## 3. Decisions and assumptions

| # | Question | Decision |
| --- | --- | --- |
| 1 | Nav items | `Inbox` / `Browse` / `Search` dispatch new commands (`Command::{ShowInbox, ShowBrowse, ShowSearch}`); `Workspace::on_command` opens each panel with `DockPlacement::Center`. `ui::dock::add_panel` already focuses an open panel by `panel_id` instead of duplicating it. |
| 2 | Search in the sidebar | No find input, no results/contacts sections. The existing search/select implementation moves to `panels/search.rs` as a mechanical relocation (§7, step 5). |
| 3 | Panels in this change | `Inbox` and `Browse` render empty bodies for now (tab title only); `Search` gets its body from the search relocation in step 5. Real Inbox/Browse content is follow-up work. |
| 4 | Pin storage | UI-local `Vec<u64>` of room ids, in memory first; persistence is step 8 (optional). |
| 5 | Pinned rooms in Messages | Kept in both places; `Pinned` is a shortcut, not a move. |
| 6 | Row height | Uniform `h_8` (32px) for every tree row, including `RoomEntry` (currently `h_9`). Required by `uniform_list`, which measures only the first row. |
| 7 | Community data | 1-3 hardcoded `CommunityEntry` values with a `TODO(concord)` pointing at `docs/concord-usage.md`. |
| 8 | Requests default | Collapsed; the folder row still shows the unread dot. Expanding clears `new_requests`. |

## 4. State model

`Sidebar` keeps only what the tree needs.

```rust
/// Collapsible tree sections; declaration order is render order.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
enum TreeSection {
    Pins,
    Requests,
    Community,
    Messages,
}
```

New fields:

```rust
expanded: BTreeSet<TreeSection>,
pinned_rooms: Vec<u64>, // room ids in pin order
```

Defaults: `expanded = {Community, Messages}` (Requests intentionally absent;
Pins only matters when non-empty and starts expanded).

New methods:

```rust
fn toggle_section(&mut self, section: TreeSection, cx: &mut Context<Self>);
fn is_expanded(&self, section: TreeSection) -> bool;
fn pin_room(&mut self, room_id: u64, cx: &mut Context<Self>);
fn unpin_room(&mut self, room_id: u64, cx: &mut Context<Self>);
fn is_pinned(&self, room_id: u64) -> bool;
fn tree_rows(&self, cx: &App) -> Vec<SidebarRow>; // see §5
```

`toggle_section(Requests)` clears `new_requests`.

Removed from `Sidebar` (all search-related, carried to the Search panel in
step 5): `filter: Entity<RoomKind>`, `current_filter`, `set_filter`,
`show_find_panel`, `find_input`, `find_debouncer`, `finding`, `find_focused`,
`find_results`, `find_task`, `has_search`, `contact_list`, `selected_pkeys`,
and methods `get_contact_list`, `set_contact_list`, `debounced_search`,
`search`, `set_results`, `set_finding`, `set_input_focus`, `reset`, `select`,
`is_selected`, `get_selected`, `create_room`, `render_results`,
`render_contacts`. Nothing is deleted from the codebase: step 5 moves it into
the Search panel.

## 5. Row model and flattening

The tree body is one `uniform_list`, so all rows must be the same height
(`h_8`) and the flattened order must be precomputed per frame.

New file `crates/workspace/src/sidebar/tree.rs`:

```rust
/// One rendered tree row, in flattened order.
enum SidebarRow {
    Section { section: TreeSection, count: usize },
    Room { room: Entity<Room>, depth: u8, pinned: bool },
    Community { entry: &'static CommunityEntry, depth: u8 },
    Hint { text: SharedString, depth: u8 },
}

struct CommunityEntry {
    name: &'static str,
    // Rendered as a 20px circle with the first letter; no backend yet.
}

fn dummy_communities() -> &'static [CommunityEntry]; // TODO(concord)

/// Folder/file row. One element for section headers, community rows and hints.
#[derive(IntoElement)]
struct TreeRow { /* id, depth, caret, icon, avatar, label, count, dot, selected, on_click */ }
```

`Sidebar::tree_rows`:

```rust
// Pins (only when a pinned id resolves to a live room), in pin order
// Requests -> chat.rooms(&RoomKind::Request, cx)
// Community -> dummy_communities()
// Messages -> chat.rooms(&RoomKind::Ongoing, cx)
//
// Each section emits Section first, then children when expanded,
// then a Hint row when expanded and empty.
```

Render integration:

```rust
let rows = Rc::new(self.tree_rows(cx));

uniform_list("sidebar-tree", rows.len(), cx.processor(move |this, range, _window, cx| {
    this.render_rows(range, &rows, cx)
}))
.track_scroll(&self.scroll_handle)
```

`render_rows` matches on `SidebarRow` and builds either a `TreeRow` (sections,
communities, hints) or a `RoomEntry` (rooms). Element ids: room rows use the
flattened index (`RoomEntry::new(ix)`); non-room rows use
`ElementId::NamedInteger("tree-row".into(), ix as u64)`.

Why a flattened list instead of `gpui_base::Tree` / `VirtualList`: sections are
few and fixed, rows are heterogeneous, and the crate already uses
`uniform_list` + `Scrollbar::vertical`. The base `Tree`/`VirtualList`
primitives remain available if variable row heights are ever needed.

## 6. Rendering spec

### 6.1 Nav rail

Three full-width `Button`s, `ghost_alt`, `small`, dispatching actions:

```rust
Button::new("nav-inbox")
    .icon(IconName::Inbox)
    .label("Inbox")
    .w_full()
    .justify_start()
    .on_click(|_ev, _window, cx| {
        cx.dispatch_action(&Command::ShowInbox);
    })
```

Icons: `Inbox`, `Compass` (new, Browse), `Search`. `Command` is already
imported in `sidebar/mod.rs`, and dispatching actions from a button listener is
the existing `greeter.rs` pattern.

Nav rows carry no `selected` state: the sidebar does not know which panel the
dock is showing. Highlighting the active destination is a follow-up if the dock
API exposes it.

### 6.2 Section (folder) row

- `h_8`, `pl`/`pr` matching the list padding, `rounded(theme.radius)`, full
  width, hover `ghost_element_hover`.
- Caret: `CaretDown` when expanded, `CaretRight` when collapsed.
- Icon 16px (`small()`), `text_muted`.
- Label: `text_xs`, `font_semibold`, `text_muted`; `flex_1`.
- Trailing: count (`text_xs`, `text_placeholder`) for Requests/Pins, and the
  unread dot (`size_1().rounded_full().bg(theme.cursor)`) when
  `new_requests && section == Requests`.
- Click toggles the section.

### 6.3 File rows

- Rooms reuse `RoomEntry` with two additions: `.depth(u8)` (left padding
  `px(6. + depth * 14.)`) and an optional `.trailing(AnyElement)` slot for the
  hover ellipsis; height becomes `h_8`.
- Community rows use `TreeRow` with a 20px `element_background` circle and the
  first letter, `text_sm` label.
- Indent guide (optional polish): 1px `border_variant` vertical line at the
  child indent, drawn by the child row.

### 6.4 Fixed chrome

`render_user` unchanged. The loading pill stays positioned as today. The
"Create DM" floating button and the screening flow move with search to the
Search panel. Only the tree body scrolls.

## 7. Search leaves the sidebar (hidden, not removed)

Search is now a panel, not a sidebar mode:

- `Command::ShowSearch` opens `panels/search.rs` in the dock center.
- The current implementation moves there intact — same input, debounce
  (`DebouncedDelay` + `FIND_DELAY`), `NostrRegistry::search`, contact list,
  multi-select, create-DM flow, and the `RoomEntry` selection/screening
  behavior. The move is mechanical; no search logic is rewritten or dropped.
- The sidebar renders no input and no results/contacts sections, and keeps no
  copy of the state (a dormant copy would be dead code).
- The Search panel is a separate workstream from the tree: it owns the module
  after the relocation and evolves independently.

## 8. Pin folder

- Pin state: `pinned_rooms: Vec<u64>` in `Sidebar`, order = pin order.
- UI: hover ellipsis (`IconName::Ellipsis`, `ghost_alt`, `xsmall`, `compact`)
  on each room row, opening a `DropdownMenu` with `Pin` / `Unpin`
  (`PopupMenuItem::new(...).on_click(...)`). Verify the trigger click does not
  also fire the row's `emit_room` click; if it does, `cx.stop_propagation()`
  in the menu trigger's `on_click`. (There is no right-click menu pattern in
  the codebase yet; a context menu is a follow-up.)
- `Pinned` folder is hidden when no pinned room resolves to a live room;
  otherwise expanded by default, showing pinned rooms in pin order.
- A pinned room remains listed under `Messages`.

## 9. Requests

- Folder always rendered, collapsed by default (`expanded` does not contain
  `Requests`).
- Count badge = `chat.count(&RoomKind::Request, cx)`.
- Expanding the folder clears `new_requests`.
- Children are the same `RoomKind::Request` rooms the old Requests filter
  showed, with the same `RoomEntry` screening behavior.

## 10. Community (dummy data)

- `dummy_communities()` returns 2 entries for now (`Coop Contributors`,
  `Nostr Design`) so the folder has content; 1-3 is the range the sketch asks
  for.
- Rows do not navigate anywhere yet; clicking is a no-op. Add
  `// TODO(concord): replace with ConcordRegistry communities, see docs/concord-usage.md`.
- Folder expanded by default.

## 11. Messages

- `RoomKind::Ongoing` rooms, using the existing `render_list_items` logic
  (display name/avatar/member pubkey/kind/created_at, `emit_room` on click).
- Expanded by default.
- When empty and expanded, show a `Hint` row ("No conversations yet") instead
  of the current large dashed card; the card is removed.

## 12. Implementation steps

Steps 1-4 are additive and compile on their own. Step 5 is one atomic change
set: the search relocation and the sidebar render rewrite depend on each other,
because removing the search fields breaks the old render and rewriting the
render orphans the search code. Helpers added in earlier steps may warn as
unused until step 5 consumes them. Run the checks in §15 after each step.

- [x] **Step 1 — icons.** Add `assets/icons/folder.svg`, `compass.svg`,
  `message.svg` (24x24 viewBox, `stroke="currentColor"`, `stroke-width="1.5"`,
  matching existing files); add `Folder`, `Compass`, `Message` variants to
  `IconName` and its `path()` match in `crates/ui/src/icon.rs`.
- [x] **Step 2 — tree primitives.** Add `crates/workspace/src/sidebar/tree.rs`
  with `TreeSection`, `SidebarRow`, `CommunityEntry`, `dummy_communities()`,
  and the `TreeRow` element; declare `mod tree;` in `sidebar/mod.rs`.
- [x] **Step 3 — `RoomEntry`.** Add `.depth(u8)` and `.trailing(AnyElement)`;
  change `h_9` to `h_8`.
- [x] **Step 4 — panel openers.** Add `Command::{ShowInbox, ShowBrowse,
  ShowSearch}` and `panels/{inbox,browse,search}.rs` shells (`init`, `Panel`,
  `Focusable`, `EventEmitter<PanelEvent>`, empty `Render`, following
  `greeter.rs`); register them in `panels/mod.rs`; handle the commands in
  `Workspace::on_command` with `add_panel_to_dock(..., DockPlacement::Center, ...)`.
  All three render empty bodies for now; the Search body is filled in step 5.
- [x] **Step 5 — relocation + render rewrite (atomic, separate workstream
  handoff).** Move the search/select implementation out of `Sidebar` into
  `panels/search.rs` (inventory in §7), wiring the input, results, contacts,
  selection, and create-DM button exactly as they are today; at the same time
  rewrite the sidebar render (nav rail dispatching the three commands, flattened
  tree list, scrollbar, `render_user`, loading pill), add
  `expanded`/`pinned_rooms`/`tree_rows`, and delete `filter`, `current_filter`,
  `set_filter`, and the sidebar's search state. The search workstream owns the
  relocated module afterwards. Done: `SearchPanel` owns the input, debounce,
  results, contacts, selection and create-DM flow; `Sidebar` owns
  `expanded`/`pinned_rooms` and flattens the four sections into one
  `uniform_list("sidebar-tree")`. `has_search`, `find_focused`, `set_input_focus`
  were dropped because they only existed to switch the sidebar between the room
  list and the search view.
- [ ] **Step 6 — pin UI.** Build the per-row ellipsis dropdown, wire
  `pin_room`/`unpin_room`.
- [ ] **Step 7 — community section.** Render dummy entries and hint; add the
  `TODO(concord)` marker. The flattening and rendering landed with step 5
  (`SidebarRow::Community` -> `TreeRow`, dummy data from `dummy_communities()`),
  so this step is effectively complete once the names in §10 are confirmed.
- [ ] **Step 8 (optional) — persistence.** Add
  `#[serde(default)] pinned_rooms: Vec<u64>` (and optionally
  `expanded_sections: Vec<String>`) to `settings::Settings`, register accessors
  in `setting_accessors!`, and load/save from `Sidebar`. The `#[serde(default)]`
  attribute is required: `Settings` has no defaults today, so a new field
  without it breaks parsing of existing `.settings` files.
- [ ] **Step 9 — cleanup.** `cargo fmt`, remove dead imports/helpers, run
  clippy.

## 13. Files touched

| File | Change |
| --- | --- |
| `crates/workspace/src/sidebar/mod.rs` | State, flattening, render rewrite; search code moves out |
| `crates/workspace/src/sidebar/tree.rs` | New: sections, rows, `TreeRow`, dummy data |
| `crates/workspace/src/sidebar/entry.rs` | `depth`, `trailing`, height |
| `crates/workspace/src/panels/{inbox,browse,search}.rs` | New panel modules |
| `crates/workspace/src/panels/mod.rs` | Module registration |
| `crates/workspace/src/lib.rs` | `Command` variants + `on_command` arms |
| `crates/ui/src/icon.rs` | New icon variants |
| `assets/icons/{folder,compass,message}.svg` | New assets |
| `crates/settings/src/lib.rs` | Optional step 8 only |

## 14. Edge cases

- **Uniform height.** `uniform_list` measures the first row and reuses that
  height; every row must be `h_8`. If a section row ever needs a different
  height, switch to `gpui_base::VirtualList` instead of mixing.
- **Panel dedupe.** Clicking a nav item whose panel is already open focuses and
  moves it (`ui::dock::add_panel` looks up `panel_id`); no duplicate tabs.
- **Stale pins.** A pinned id whose room is gone is skipped at flatten time
  (and pruned on the next pin/unpin).
- **Empty sections.** Expanded + empty renders a `Hint` row; collapsed sections
  render nothing.
- **Logged out.** `NostrRegistry::current_user()` is `None`: `render_user`
  keeps its import-identity prompt; sections resolve to empty and show hints.
- **Loading.** Sections may be empty; the loading pill stays.
- **New requests while collapsed.** The dot shows on the collapsed Requests
  folder; expanding clears it.
- **Image cache.** Keep `retain_all("sidebar")` on the root.
- **Element ids.** Flattened index for room rows, `NamedInteger` for others, so
  expansion/collapse does not smuggle state between rows.

## 15. Validation

- `cargo fmt --check` (workspace `rustfmt.toml`).
- `cargo check -p workspace` and `cargo clippy -p workspace --all-targets`.
- Manual QA checklist:
  - Inbox/Browse/Search each open their panel; clicking the same nav item again
    focuses the existing panel instead of duplicating it;
  - the sidebar has no search input and no results/contacts sections;
  - the Search panel keeps the old behavior (debounced search, contacts,
    multi-select, create DM, `Enter` to search);
  - each folder toggles and keeps its state across re-renders and room updates;
  - Requests starts collapsed; the dot appears on `ChatEvent::Ping` and clears
    when expanded;
  - pin/unpin from the row menu updates the Pinned folder without opening the
    room; clicking a pinned row opens it;
  - Messages lists ongoing rooms and still opens the screening modal for
    non-ongoing rooms;
  - empty states at 0 ongoing and 0 requests.
- There is no GPUI test infrastructure in the repo (no `#[gpui::test]`
  anywhere), so tests are limited to pure helpers (`TreeSection` defaults, pin
  ordering) if they are extracted as free functions; `cargo check` plus the
  manual checklist is the baseline.

## 16. Open questions

1. **Persistence.** Persist pins and folder state, or keep them session-local?
2. **Row density.** `h_8` vs the current `h_9`; `SIDEBAR_WIDTH` stays 240px for
   now, one indent level fits.
3. **Community entries.** Preferred dummy names/branding before the real
   registry lands.

## 17. Out of scope / follow-ups

- Inbox/Browse panel content (empty bodies in this change).
- Search panel development beyond the relocation; any search UI changes happen
  in that workstream.
- Real Concord integration (`ConcordRegistry`, subscriptions, member lists) —
  tracked in `docs/concord-usage.md`.
- Drag-to-reorder pins, pin folders/groups beyond the single `Pinned` folder.
- Unread counts per room, nav-item active highlighting.
- Variable-height rows or nested subfolders.
