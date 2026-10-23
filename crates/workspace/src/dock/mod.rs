use std::sync::Arc;

use gpui::{App, Pixels, WeakEntity, Window, px};
use gpui_kit::component::dock::{
    DockArea, DockPlacement, InsertTarget, NodeId, PaneRef, PanelHandle, PanelId,
};

mod invalid_panel;
mod skin;
mod tab_panel;
mod window_controls;

pub use skin::CoopDockSkin;

/// The fixed height of the tab bar, which doubles as the window title bar.
pub const TAB_BAR_HEIGHT: Pixels = px(44.);

/// The panel in any region of `area whose id is `key.
fn find_panel(area: &DockArea, key: PanelId, _cx: &App) -> Option<(PanelId, NodeId, usize)> {
    let placements = [
        DockPlacement::Center,
        DockPlacement::Left,
        DockPlacement::Right,
        DockPlacement::Bottom,
    ];

    for placement in placements {
        let Some(tree) = area.layout(placement) else {
            continue;
        };

        for id in tree.panels() {
            if id != key {
                continue;
            }

            let node = tree.find_panel_node(id)?;
            let PaneRef::Tabs { panels, .. } = tree.find_node(node)?.kind() else {
                continue;
            };
            let ix = panels.iter().position(|panel| *panel == key)?;
            return Some((id, node, ix));
        }
    }

    None
}

/// Add `panel to the area, or move it to the end of its existing tab group.
pub fn add_panel(
    area: &mut DockArea,
    panel: PanelHandle,
    placement: DockPlacement,
    window: &mut Window,
    cx: &mut gpui::Context<DockArea>,
) {
    let key = panel.panel_id(cx);
    if let Some((id, node, ix)) = find_panel(area, key, cx) {
        area.move_panel(
            id,
            InsertTarget::Tabs {
                node,
                ix: Some(ix),
                activate: true,
            },
            window,
            cx,
        );
        return;
    }

    area.add_panel_view(Arc::new(panel), placement, None, window, cx);
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
