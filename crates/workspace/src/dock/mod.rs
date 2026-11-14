use std::sync::Arc;

use chat_ui::ChatPanel;
use gpui::{App, Entity, Pixels, SharedString, WeakEntity, Window, px};
use gpui_base::dock::PanelView;
use gpui_kit::component::dock::{DockArea, DockPlacement, PanelHandle, PanelId};
use nip29::GroupKey;
use nip29_ui::GroupPanel;

mod invalid_panel;
mod skin;
mod tab_panel;
mod window_controls;

pub use skin::CoopDockSkin;

/// The fixed height of the tab bar, which doubles as the window title bar.
pub const TAB_BAR_HEIGHT: Pixels = px(44.);
/// Dock placements.
const DOCK_PLACEMENTS: [DockPlacement; 4] = [
    DockPlacement::Center,
    DockPlacement::Left,
    DockPlacement::Right,
    DockPlacement::Bottom,
];

/// What makes a panel the same panel across opens.
#[derive(Clone, PartialEq, Eq)]
enum PanelKey {
    /// A panel of which there should be one, keyed by its `panel_name`.
    Named(&'static str),
    /// A chat panel, keyed by the id of the room it displays.
    Room(SharedString),
}

/// The key a panel handle carries, or `None` when it cannot be read off the panel.
fn panel_key(panel: &PanelHandle, cx: &App) -> Option<PanelKey> {
    let view = panel.panel();
    if let Some(chat) = view.as_any().downcast_ref::<Entity<ChatPanel>>() {
        return Some(PanelKey::Room(chat.read(cx).room_id().clone()));
    }
    if view.as_any().downcast_ref::<Entity<GroupPanel>>().is_some() {
        return None;
    }
    Some(PanelKey::Named(view.panel_name(cx)))
}

/// The key of a panel docked in `area`, recovered through the handle base holds.
fn docked_panel_key(view: &Arc<dyn PanelView>, cx: &App) -> Option<PanelKey> {
    let handle = PanelHandle::of(view)?;
    panel_key(handle, cx)
}

/// The docked panel whose key is `key`, and the dock it sits in.
fn find_panel(area: &DockArea, key: &PanelKey, cx: &App) -> Option<(PanelId, DockPlacement)> {
    for placement in DOCK_PLACEMENTS {
        let Some(tree) = area.layout(placement) else {
            continue;
        };

        for id in tree.panels() {
            let Some(view) = area.panel(id) else {
                continue;
            };
            if docked_panel_key(view, cx).is_some_and(|found| &found == key) {
                return Some((id, placement));
            }
        }
    }

    None
}

/// Bring the docked panel `id` on screen.
pub fn focus_panel(
    area: &mut DockArea,
    id: PanelId,
    window: &mut Window,
    cx: &mut gpui::Context<DockArea>,
) {
    let Some(placement) = DOCK_PLACEMENTS.into_iter().find(|placement| {
        area.layout(*placement)
            .is_some_and(|tree| tree.panels().any(|panel| panel == id))
    }) else {
        return;
    };

    // The center region has no open state, and `toggle_dock` would ignore it.
    if placement != DockPlacement::Center && !area.is_dock_open(placement) {
        area.toggle_dock(placement, window, cx);
    }
    area.select_panel(id, window, cx);
}

/// Add `panel` to the area, or focus the panel already showing the same thing.
pub fn add_panel(
    area: &mut DockArea,
    panel: PanelHandle,
    placement: DockPlacement,
    window: &mut Window,
    cx: &mut gpui::Context<DockArea>,
) {
    let Some(key) = panel_key(&panel, cx) else {
        area.add_panel_view(Arc::new(panel), placement, None, window, cx);
        return;
    };

    if let Some((id, _)) = find_panel(area, &key, cx) {
        focus_panel(area, id, window, cx);
        return;
    }

    area.add_panel_view(Arc::new(panel), placement, None, window, cx);
}

/// Close the docked chat panel showing `room_id`, if there is one.
pub fn close_room_panel(
    area: &mut DockArea,
    room_id: &SharedString,
    window: &mut Window,
    cx: &mut gpui::Context<DockArea>,
) {
    let key = PanelKey::Room(room_id.clone());

    let Some((id, _)) = find_panel(area, &key, cx) else {
        return;
    };

    let Some(view) = area.panel(id) else {
        return;
    };

    let Some(chat) = view.as_any().downcast_ref::<Entity<ChatPanel>>() else {
        return;
    };

    let chat = chat.clone();
    area.remove_panel(chat, window, cx);
}

/// Add a panel to a dock reached through a weak handle.
pub fn add_panel_to(
    dock: &WeakEntity<DockArea>,
    panel: PanelHandle,
    placement: DockPlacement,
    window: &mut Window,
    cx: &mut App,
) {
    if let Err(error) = dock.update(cx, |area, cx| {
        add_panel(area, panel, placement, window, cx);
    }) {
        log::warn!("Failed to add panel to dock: {error}");
    }
}

/// Add a group panel to a dock, or focus the panel already showing that group.
///
/// Group panels hide their group, so the workspace records which panel it
/// docked per key; without that record, reopening a group stacks another tab.
pub fn add_group_panel_to(
    dock: &WeakEntity<DockArea>,
    group_key: GroupKey,
    panel: PanelHandle,
    placement: DockPlacement,
    window: &mut Window,
    cx: &mut App,
) {
    let Some(root) = window.root::<crate::Workspace>().flatten() else {
        add_panel_to(dock, panel, placement, window, cx);
        return;
    };

    root.update(cx, |this, cx| {
        this.add_group_panel_to_dock(group_key, panel, placement, window, cx);
    });
}
