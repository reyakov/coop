use std::cell::{Cell, RefCell};
use std::ops::Deref as _;
use std::rc::Rc;
use std::sync::Arc;

use gpui::prelude::FluentBuilder as _;
use gpui::{
    Anchor, AnyElement, AnyView, App, AppContext as _, Bounds, Context, Div, Empty, Entity,
    GlobalElementId, InspectorElementId, InteractiveElement as _, IntoElement, LayoutId,
    MouseButton, MouseMoveEvent, MouseUpEvent, ParentElement as _, Pixels, Render, ScrollHandle,
    SharedString, Stateful, StatefulInteractiveElement as _, Style, StyleRefinement, Styled as _,
    WeakEntity, Window, actions, div, px, rems,
};
pub use gpui_base::dock::{DockArea, DockLayout, DockPlacement};
use gpui_base::dock::{
    DockAreaRenderer, DockContext, DragPanel, DropIndicator, InsertTarget, NodeId, PaneNode,
    PaneRef, PanelId, TabGroupContext, TabGroupRenderer, TileContext, TilesRenderer,
};
use gpui_base::{Placement, ResizeHandleContext, Side};
use theme::{ActiveTheme, TABBAR_HEIGHT};

use crate::button::{Button, ButtonVariants as _};
use crate::menu::DropdownMenu as _;
use crate::resizable::{resize_handle, resize_handle_appearance};
use crate::tab::Tab;
use crate::tab::tab_bar::TabBar;
use crate::title_bar::{TRAFFIC_LIGHT_PADDING, title_bar_drag_handlers, window_controls};
use crate::{IconName, Selectable, Sizable, StyledExt, h_flex, v_flex};

mod panel;
pub use panel::*;

actions!(dock, [ToggleZoom, ClosePanel]);

pub type TitleBarRenderer = fn(&mut Window, &mut App) -> AnyElement;

#[derive(Default)]
pub struct TitleBarChrome {
    trailing: Cell<Option<TitleBarRenderer>>,
}

impl TitleBarChrome {
    pub fn set_trailing(&self, renderer: TitleBarRenderer) {
        self.trailing.set(Some(renderer));
    }

    fn trailing(&self, window: &mut Window, cx: &mut App) -> Option<AnyElement> {
        self.trailing.get().map(|render| render(window, cx))
    }
}

pub fn dock_area(
    id: impl Into<SharedString>,
    window: &mut Window,
    cx: &mut App,
) -> (Entity<DockArea>, Rc<TitleBarChrome>) {
    let chrome = Rc::new(TitleBarChrome::default());
    let shared = Rc::new(SkinShared {
        area: RefCell::new(None),
        resizing: Cell::new(None),
        chrome: chrome.clone(),
    });
    let area = cx.new(|cx| {
        DockArea::new(id, None, window, cx).with_renderer(Rc::new(DockSkin {
            shared: shared.clone(),
        }))
    });

    *shared.area.borrow_mut() = Some(area.downgrade());
    (area, chrome)
}

pub fn add_panel(
    area: &mut DockArea,
    panel: PanelHandle,
    placement: DockPlacement,
    window: &mut Window,
    cx: &mut Context<DockArea>,
) {
    let key = panel.panel().panel_id(cx);
    if let Some((id, node, ix)) = find_panel(area, &key, cx) {
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

/// The panel in any region of `area` whose logical id is `key`.
fn find_panel(area: &DockArea, key: &SharedString, cx: &App) -> Option<(PanelId, NodeId, usize)> {
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
            let matches = area
                .panel(id)
                .and_then(PanelHandle::of)
                .is_some_and(|handle| handle.panel().panel_id(cx) == *key);
            if !matches {
                continue;
            }

            let Some(node) = tree.find_panel_node(id) else {
                continue;
            };
            let Some(PaneRef::Tabs { panels, .. }) = tree.find_node(node).map(PaneNode::kind)
            else {
                continue;
            };
            let Some(ix) = panels.iter().position(|candidate| *candidate == id) else {
                continue;
            };

            return Some((id, node, ix));
        }
    }

    None
}

pub fn focus_tab_panel(area: &DockArea, window: &mut Window, cx: &mut App) {
    let Some(tree) = area.layout(DockPlacement::Center) else {
        return;
    };
    let Some(node) = left_top_group(tree.root()) else {
        return;
    };
    let Some(PaneRef::Tabs { panels, active_ix }) = tree.find_node(node).map(PaneNode::kind) else {
        return;
    };

    let displayed = match panels.get(active_ix) {
        Some(panel) if area.panel(*panel).is_some_and(|panel| panel.visible(cx)) => Some(*panel),
        _ => panels
            .iter()
            .copied()
            .find(|id| area.panel(*id).is_some_and(|panel| panel.visible(cx))),
    };

    let Some(panel) = displayed.and_then(|id| area.panel(id).cloned()) else {
        return;
    };

    let focus_handle = panel.focus_handle(cx);
    window.focus(&focus_handle, cx);
}

fn left_top_group(node: &PaneNode) -> Option<NodeId> {
    match node.kind() {
        PaneRef::Tabs { .. } => Some(node.id()),
        PaneRef::Split { children, .. } => children.first().and_then(left_top_group),
        PaneRef::Tiles { .. } => None,
    }
}

fn right_top_group(node: &PaneNode) -> Option<NodeId> {
    match node.kind() {
        PaneRef::Tabs { .. } => Some(node.id()),
        PaneRef::Split { axis, children, .. } => {
            let child = if axis == gpui::Axis::Vertical {
                children.first()
            } else {
                children.last()
            };
            child.and_then(right_top_group)
        }
        PaneRef::Tiles { .. } => None,
    }
}

#[derive(Default)]
struct SkinShared {
    area: RefCell<Option<WeakEntity<DockArea>>>,
    resizing: Cell<Option<DockPlacement>>,
    chrome: Rc<TitleBarChrome>,
}

impl SkinShared {
    fn area(&self) -> Option<Entity<DockArea>> {
        self.area.borrow().as_ref().and_then(|area| area.upgrade())
    }
}

struct DockSkin {
    shared: Rc<SkinShared>,
}

impl DockSkin {
    fn resize_handle(&self, dock: &DockContext) -> impl IntoElement {
        let placement = dock.placement();
        let shared = self.shared.clone();

        let id = match placement {
            DockPlacement::Left => "dock-resize-handle-left",
            DockPlacement::Right => "dock-resize-handle-right",
            DockPlacement::Bottom => "dock-resize-handle-bottom",
            DockPlacement::Center => "dock-resize-handle-center",
        };

        resize_handle(id, placement.axis())
            .placement(if placement.is_left() {
                Side::Left
            } else {
                Side::Right
            })
            .with_appearance(resize_handle_appearance())
            .on_drag(DockResizeHandle, move |info, _, _, cx| {
                cx.stop_propagation();
                shared.resizing.set(Some(placement));
                cx.new(|_| info.deref().clone())
            })
    }
}

impl DockAreaRenderer for DockSkin {
    fn render_split_handle(
        &self,
        handle: &ResizeHandleContext,
        window: &mut Window,
        cx: &mut App,
    ) -> Option<AnyElement> {
        resize_handle_appearance()(handle, window, cx)
    }

    fn render_dock(
        &self,
        dock: &DockContext,
        content: AnyElement,
        _: &mut Window,
        _: &mut App,
    ) -> AnyElement {
        div()
            .flex()
            .size_full()
            .relative()
            .child(content)
            .child(self.resize_handle(dock))
            .child(DockResizeTracker {
                dock: dock.clone(),
                shared: self.shared.clone(),
            })
            .into_any_element()
    }

    fn tab_group_renderer(&self) -> Rc<dyn TabGroupRenderer> {
        Rc::new(TabGroupSkin::new(self.shared.clone()))
    }

    fn tiles_renderer(&self) -> Rc<dyn TilesRenderer> {
        Rc::new(NoTiles)
    }
}

struct NoTiles;

impl TilesRenderer for NoTiles {
    fn render_drag_bar(&self, _: &TileContext, _: &mut Window, _: &mut App) -> AnyElement {
        Empty.into_any_element()
    }
}

/// The payload a dock's resize handle drags; the handle is the affordance.
#[derive(Clone)]
struct DockResizeHandle;

impl Render for DockResizeHandle {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        Empty
    }
}

/// Turns the window's mouse stream into dock resizing.
struct DockResizeTracker {
    dock: DockContext,
    shared: Rc<SkinShared>,
}

impl IntoElement for DockResizeTracker {
    type Element = Self;

    fn into_element(self) -> Self::Element {
        self
    }
}

impl gpui::Element for DockResizeTracker {
    type PrepaintState = ();
    type RequestLayoutState = ();

    fn id(&self) -> Option<gpui::ElementId> {
        None
    }

    fn source_location(&self) -> Option<&'static std::panic::Location<'static>> {
        None
    }

    fn request_layout(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        window: &mut Window,
        cx: &mut App,
    ) -> (LayoutId, Self::RequestLayoutState) {
        (window.request_layout(Style::default(), None, cx), ())
    }

    fn prepaint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        _: Bounds<Pixels>,
        _: &mut Self::RequestLayoutState,
        _: &mut Window,
        _: &mut App,
    ) {
    }

    fn paint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        _: Bounds<Pixels>,
        _: &mut Self::RequestLayoutState,
        _: &mut Self::PrepaintState,
        window: &mut Window,
        _: &mut App,
    ) {
        let placement = self.dock.placement();

        window.on_mouse_event({
            let dock = self.dock.clone();
            let shared = self.shared.clone();
            move |event: &MouseMoveEvent, phase, window, cx| {
                if !phase.bubble() || shared.resizing.get() != Some(placement) {
                    return;
                }

                let open = shared
                    .area()
                    .is_some_and(|area| area.read(cx).is_dock_open(placement));

                if !open {
                    dock.toggle(window, cx);
                }

                dock.resize_to(event.position, window, cx);
            }
        });

        window.on_mouse_event({
            let shared = self.shared.clone();
            move |_: &MouseUpEvent, phase, _, _| {
                if !phase.bubble() || shared.resizing.get() != Some(placement) {
                    return;
                }

                shared.resizing.set(None);
            }
        });
    }
}

struct DragPreview {
    panel: Arc<dyn gpui_base::dock::PanelView>,
}

impl Render for DragPreview {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        h_flex()
            .id("drag-panel")
            .cursor_grab()
            .p_2()
            .min_w_24()
            .justify_center()
            .overflow_hidden()
            .whitespace_nowrap()
            .rounded(cx.theme().radius)
            .text_sm()
            .text_color(cx.theme().text)
            .text_ellipsis()
            .when(cx.theme().shadow, |this| this.shadow_xs())
            .bg(cx.theme().background)
            .child(panel_title(&self.panel, cx))
    }
}

struct TabGroupSkin {
    shared: Rc<SkinShared>,
    scroll_handle: ScrollHandle,
    last_active_ix: Cell<Option<usize>>,
}

impl TabGroupSkin {
    fn new(shared: Rc<SkinShared>) -> Self {
        Self {
            shared,
            scroll_handle: ScrollHandle::new(),
            last_active_ix: Cell::new(None),
        }
    }

    fn is_title_bar_group(&self, group: &TabGroupContext, cx: &App) -> bool {
        let Some(area) = self.shared.area() else {
            return false;
        };

        area.read(cx)
            .layout(DockPlacement::Center)
            .and_then(|tree| left_top_group(tree.root()))
            == Some(group.node())
    }

    /// Whether this group is the left dock's root with a single panel.
    ///
    /// Such a group draws no tab bar, so its panel owns the window's top-left
    /// corner — including the space the macOS traffic lights overlay.
    fn is_plain_left_group(&self, group: &TabGroupContext, cx: &App) -> bool {
        let Some(area) = self.shared.area() else {
            return false;
        };
        let area = area.read(cx);

        area.layout(DockPlacement::Left)
            .map(|tree| tree.root().id())
            == Some(group.node())
            && group.panels().len() == 1
    }

    /// Whether this group is the topmost-left group on screen, which sits under
    /// the native macOS traffic lights. The left dock's group is leftmost while
    /// it is open and holds a panel; the center's is leftmost otherwise.
    fn is_leftmost_top_group(&self, group: &TabGroupContext, cx: &App) -> bool {
        let Some(area) = self.shared.area() else {
            return false;
        };
        let area = area.read(cx);

        let left_open =
            area.is_dock_open(DockPlacement::Left) && !area.is_empty(DockPlacement::Left, cx);
        let tree = if left_open {
            area.layout(DockPlacement::Left)
        } else {
            area.layout(DockPlacement::Center)
        };

        tree.and_then(|tree| left_top_group(tree.root())) == Some(group.node())
    }

    fn render_toolbar(
        &self,
        group: &TabGroupContext,
        window: &mut Window,
        cx: &mut App,
    ) -> impl IntoElement {
        let zoomed = group.is_zoomed();
        let closable = group.is_closable();
        let zoomable = group.active_panel().is_some_and(|panel| panel.zoomable(cx));

        let zoom_label = if zoomed { "Zoom Out" } else { "Zoom In" };

        let buttons = group
            .active_panel()
            .and_then(PanelHandle::of)
            .map(|handle| handle.panel().toolbar_buttons(window, cx))
            .unwrap_or_default();

        let menu_panel = group
            .active_panel()
            .and_then(PanelHandle::of)
            .map(|handle| handle.panel().clone());

        h_flex()
            .p_0p5()
            .gap_1p5()
            .occlude()
            .rounded_full()
            .children(buttons.into_iter().map(|button| button.small().ghost()))
            .when(zoomed, |this| {
                this.child(
                    Button::new("zoom")
                        .icon(IconName::Zoom)
                        .small()
                        .ghost()
                        .tooltip("Zoom Out")
                        .on_click({
                            let group = TabGroupContext::clone(group);
                            move |_, window, cx| group.toggle_zoom(window, cx)
                        }),
                )
            })
            .child(
                Button::new("menu")
                    .icon(IconName::Ellipsis)
                    .small()
                    .ghost()
                    .dropdown_menu({
                        move |menu, _, cx| {
                            let menu = match menu_panel.clone() {
                                Some(panel) => panel.popup_menu(menu, cx),
                                None => menu,
                            };

                            menu.when(zoomable, |this| {
                                this.separator().menu(zoom_label, Box::new(ToggleZoom))
                            })
                            .when(closable, |this| {
                                this.separator().menu("Close", Box::new(ClosePanel))
                            })
                        }
                    })
                    .anchor(Anchor::TopRight),
            )
    }

    fn render_title(
        &self,
        group: &TabGroupContext,
        ix: usize,
        window: &mut Window,
        cx: &mut App,
    ) -> AnyElement {
        let panel = group.panels()[ix].clone();
        let left_button = self.dock_toggle_button(DockPlacement::Left, group, cx);
        let bottom_button = self.dock_toggle_button(DockPlacement::Bottom, group, cx);
        let right_button = self.dock_toggle_button(DockPlacement::Right, group, cx);
        let has_leading = left_button.is_some() || bottom_button.is_some();
        let drag = tab_drag(group, ix, cx);
        let is_title_bar = self.is_title_bar_group(group, cx);
        let needs_traffic_light_padding =
            cfg!(target_os = "macos") && self.is_leftmost_top_group(group, cx);
        let trailing_chrome = is_title_bar
            .then(|| self.shared.chrome.trailing(window, cx))
            .flatten();

        let bar = h_flex()
            .id("tab-title-bar")
            .justify_between()
            .items_center()
            .line_height(rems(1.0))
            .h(TABBAR_HEIGHT)
            .bg(cx.theme().panel_background)
            .when(left_button.is_some(), |this| this.pl_2())
            .when(right_button.is_some(), |this| this.pr_2())
            .when(has_leading, |this| {
                this.child(
                    h_flex()
                        .flex_shrink_0()
                        .mr_1()
                        .gap_1()
                        .children(left_button)
                        .children(bottom_button),
                )
            })
            .when(needs_traffic_light_padding, |this| {
                this.pl(px(TRAFFIC_LIGHT_PADDING))
            })
            .child(
                div()
                    .id("tab")
                    .flex_initial()
                    .min_w_0()
                    .px_2()
                    .overflow_hidden()
                    .whitespace_nowrap()
                    .child(
                        div()
                            .w_full()
                            .text_ellipsis()
                            .text_sm()
                            .child(panel_title(&panel, cx)),
                    )
                    .when_some(drag, |this, drag| {
                        this.on_drag(drag, {
                            let panel = panel.clone();
                            move |drag, offset, _, cx| {
                                cx.stop_propagation();
                                drag.set_drag_offset(offset);
                                cx.new(|_| DragPreview {
                                    panel: panel.clone(),
                                })
                            }
                        })
                    }),
            )
            .child({
                let space = div().id("tab-title-space").flex_1().h_full();
                if is_title_bar {
                    title_bar_drag_handlers(space, window, cx).into_any_element()
                } else {
                    space.into_any_element()
                }
            })
            .child(
                h_flex()
                    .flex_shrink_0()
                    .ml_1()
                    .gap_1()
                    .child(self.render_toolbar(group, window, cx))
                    .children(right_button),
            )
            .when_some(trailing_chrome, |this, chrome| this.child(chrome));

        if is_title_bar {
            h_flex()
                .h(TABBAR_HEIGHT)
                .bg(cx.theme().panel_background)
                .child(bar.flex_1())
                .child(window_controls())
                .into_any_element()
        } else {
            bar.into_any_element()
        }
    }

    fn render_tabs(
        &self,
        group: &TabGroupContext,
        visible: &[usize],
        window: &mut Window,
        cx: &mut App,
    ) -> AnyElement {
        let left_button = self.dock_toggle_button(DockPlacement::Left, group, cx);
        let bottom_button = self.dock_toggle_button(DockPlacement::Bottom, group, cx);
        let right_button = self.dock_toggle_button(DockPlacement::Right, group, cx);
        let has_leading = left_button.is_some() || bottom_button.is_some();
        let collapsed = group.is_collapsed();
        let droppable = group.is_droppable();
        let tabs_count = group.panels().len();
        let displayed = group.active_panel().map(|panel| panel.panel_id(cx));
        let displayed_ix = displayed.and_then(|displayed| {
            group
                .panels()
                .iter()
                .position(|panel| panel.panel_id(cx) == displayed)
        });
        let is_title_bar = self.is_title_bar_group(group, cx);
        let needs_traffic_light_padding =
            cfg!(target_os = "macos") && self.is_leftmost_top_group(group, cx);
        let trailing_chrome = is_title_bar
            .then(|| self.shared.chrome.trailing(window, cx))
            .flatten();
        let empty_space = div()
            .id("tab-bar-empty-space")
            .h_full()
            .flex_grow_1()
            .min_w_16()
            .when(droppable, |this| {
                this.drag_over::<DragPanel>(|this, _, _, cx| this.bg(cx.theme().surface_background))
                    .on_drop({
                        let group = TabGroupContext::clone(group);
                        move |drag: &DragPanel, window, cx| {
                            let ix = (drag.source() == group.node()).then(|| tabs_count - 1);
                            group.drop_panel(drag.clone(), ix, false, window, cx);
                        }
                    })
            });
        let empty_space = if is_title_bar {
            title_bar_drag_handlers(empty_space, window, cx).into_any_element()
        } else {
            empty_space.into_any_element()
        };

        let bar = TabBar::new("tab-bar")
            .track_scroll(&self.scroll_handle)
            .h(TABBAR_HEIGHT)
            .bg(cx.theme().panel_background)
            .when(needs_traffic_light_padding, |this| {
                this.pl(px(TRAFFIC_LIGHT_PADDING))
            })
            .when(is_title_bar || has_leading, |this| {
                this.prefix(
                    h_flex()
                        .items_center()
                        .top_0()
                        .right(-px(1.))
                        .pl_0p5()
                        .pr_1()
                        .children(left_button)
                        .children(bottom_button),
                )
            })
            .children(visible.iter().map(|ix| {
                let ix = *ix;
                let panel = group.panels()[ix].clone();
                let drag = tab_drag(group, ix, cx);

                Tab::new()
                    .ix(ix)
                    .tab_bar_prefix(has_leading)
                    .child(panel_title(&panel, cx))
                    .selected(!collapsed && displayed_ix == Some(ix))
                    .disabled(collapsed)
                    .suffix(
                        Button::new("close")
                            .icon(IconName::Close)
                            .tooltip("Close panel")
                            .ghost()
                            .xsmall()
                            .on_click({
                                let group = TabGroupContext::clone(group);
                                let panel = panel.clone();
                                move |_, window, cx| {
                                    group.close(panel.panel_id(cx), window, cx);
                                }
                            }),
                    )
                    .on_click({
                        let group = TabGroupContext::clone(group);
                        move |_, window, cx| group.select_tab(ix, window, cx)
                    })
                    .when(!collapsed, |this| {
                        this.on_mouse_down(MouseButton::Middle, {
                            let group = TabGroupContext::clone(group);
                            let panel = panel.clone();
                            move |_, window, cx| {
                                group.close(panel.panel_id(cx), window, cx);
                            }
                        })
                        .when_some(drag, |this, drag| {
                            this.on_drag(drag, {
                                let panel = panel.clone();
                                move |drag, offset, _, cx| {
                                    cx.stop_propagation();
                                    drag.set_drag_offset(offset);
                                    cx.new(|_| DragPreview {
                                        panel: panel.clone(),
                                    })
                                }
                            })
                        })
                        .when(droppable, |this| {
                            this.drag_over::<DragPanel>(|this, _, _, cx| {
                                this.rounded_l_none()
                                    .border_l_2()
                                    .border_r_0()
                                    .border_color(cx.theme().border)
                            })
                            .on_drop({
                                let group = TabGroupContext::clone(group);
                                move |drag: &DragPanel, window, cx| {
                                    group.drop_panel(drag.clone(), Some(ix), true, window, cx);
                                }
                            })
                        })
                    })
            }))
            .last_empty_space(empty_space)
            .when(!collapsed, |this| {
                this.suffix(
                    h_flex()
                        .items_center()
                        .top_0()
                        .right_0()
                        .h_full()
                        .px_0p5()
                        .gap_1()
                        .child(self.render_toolbar(group, window, cx))
                        .children(right_button)
                        .children(trailing_chrome),
                )
            });

        if is_title_bar {
            h_flex()
                .h(TABBAR_HEIGHT)
                .w_full()
                .bg(cx.theme().panel_background)
                .child(bar.flex_1())
                .child(window_controls())
                .into_any_element()
        } else {
            bar.into_any_element()
        }
    }

    fn dock_toggle_button(
        &self,
        placement: DockPlacement,
        group: &TabGroupContext,
        cx: &mut App,
    ) -> Option<Button> {
        if group.is_zoomed() {
            return None;
        }

        let area = self.shared.area()?;
        let is_open = {
            let area = area.read(cx);

            if !area.is_dock_collapsible(placement) {
                return None;
            }

            let designated = match placement {
                DockPlacement::Left => area
                    .layout(DockPlacement::Center)
                    .and_then(|tree| left_top_group(tree.root())),
                DockPlacement::Right => area
                    .layout(DockPlacement::Center)
                    .and_then(|tree| right_top_group(tree.root())),
                DockPlacement::Bottom => area
                    .layout(DockPlacement::Bottom)
                    .and_then(|tree| left_top_group(tree.root())),
                DockPlacement::Center => None,
            };

            if designated != Some(group.node()) {
                return None;
            }

            area.is_dock_open(placement)
        };

        let icon = match (placement, is_open) {
            (DockPlacement::Left, true) => IconName::PanelLeft,
            (DockPlacement::Left, false) => IconName::PanelLeftOpen,
            (DockPlacement::Right, true) => IconName::PanelRight,
            (DockPlacement::Right, false) => IconName::PanelRightOpen,
            (DockPlacement::Bottom, true) => IconName::PanelBottom,
            (DockPlacement::Bottom, false) => IconName::PanelBottomOpen,
            (DockPlacement::Center, _) => return None,
        };

        Some(
            Button::new(SharedString::from(format!("toggle-dock:{:?}", placement)))
                .icon(icon)
                .small()
                .ghost()
                .tab_stop(false)
                .tooltip(if is_open { "Collapse" } else { "Expand" })
                .on_click(move |_, window, cx| {
                    area.update(cx, |area, cx| area.toggle_dock(placement, window, cx));
                }),
        )
    }
}

impl TabGroupRenderer for TabGroupSkin {
    fn frame(&self, group: &TabGroupContext, _: &mut Window, _cx: &mut App) -> Stateful<Div> {
        div().id("tab-panel").when(!group.is_collapsed(), |this| {
            this.on_action({
                let group = TabGroupContext::clone(group);
                move |_: &ToggleZoom, window, cx| group.toggle_zoom(window, cx)
            })
            .on_action({
                let group = TabGroupContext::clone(group);
                move |_: &ClosePanel, window, cx| {
                    let Some(panel) = group.active_panel() else {
                        return;
                    };
                    let panel = panel.panel_id(cx);
                    group.close(panel, window, cx);
                }
            })
        })
    }

    fn render_tab_bar(
        &self,
        group: &TabGroupContext,
        window: &mut Window,
        cx: &mut App,
    ) -> AnyElement {
        // The left dock's only panel draws bare, so its content can own the
        // window's top-left corner instead of a tab bar doing so.
        if self.is_plain_left_group(group, cx) {
            return Empty.into_any_element();
        }

        let visible: Vec<usize> = group
            .panels()
            .iter()
            .enumerate()
            .filter(|(_, panel)| panel.visible(cx))
            .map(|(ix, _)| ix)
            .collect();

        let active_ix = group.active_ix();
        if self.last_active_ix.replace(Some(active_ix)) != Some(active_ix)
            && let Some(visible_ix) = visible.iter().position(|ix| *ix == active_ix)
        {
            self.scroll_handle.scroll_to_item(visible_ix);
        }

        match visible.as_slice() {
            [] => Empty.into_any_element(),
            // One panel in a group that is not asking for tabs gets the title
            // instead of a tab bar.
            [ix] => self.render_title(group, *ix, window, cx),
            _ => self.render_tabs(group, visible.as_slice(), window, cx),
        }
    }

    fn render_active_panel(
        &self,
        panel: AnyView,
        group: &TabGroupContext,
        _: &mut Window,
        cx: &mut App,
    ) -> AnyElement {
        if group.is_collapsed() {
            return Empty.into_any_element();
        }

        v_flex()
            .id("tab-content")
            .group("")
            .overflow_hidden()
            .flex_1()
            .child(
                div()
                    .size_full()
                    .bg(cx.theme().panel_background)
                    .overflow_hidden()
                    .child(panel.cached(StyleRefinement::default().v_flex().size_full())),
            )
            .into_any_element()
    }

    fn render_drop_indicator(
        &self,
        indicator: DropIndicator,
        _: &mut Window,
        cx: &mut App,
    ) -> Option<AnyElement> {
        let size = indicator.bounds().size;
        let fraction = 0.35;

        let (left, top, width, height) = match indicator.placement() {
            Some(Placement::Left) => (px(0.), px(0.), size.width * fraction, size.height),
            Some(Placement::Right) => (
                size.width * (1. - fraction),
                px(0.),
                size.width * fraction,
                size.height,
            ),
            Some(Placement::Top) => (px(0.), px(0.), size.width, size.height * fraction),
            Some(Placement::Bottom) => (
                px(0.),
                size.height * (1. - fraction),
                size.width,
                size.height * fraction,
            ),
            None => (px(0.), px(0.), size.width, size.height),
        };

        Some(
            div()
                .absolute()
                .left(left)
                .top(top)
                .w(width)
                .h(height)
                .rounded(cx.theme().radius_lg)
                .border_1()
                .border_color(cx.theme().element_disabled)
                .bg(cx.theme().drop_target_background)
                .into_any_element(),
        )
    }
}

fn tab_drag(group: &TabGroupContext, ix: usize, cx: &App) -> Option<DragPanel> {
    group
        .is_draggable()
        .then(|| group.drag_panel(ix, cx))
        .flatten()
}

fn panel_title(panel: &Arc<dyn gpui_base::dock::PanelView>, cx: &App) -> AnyElement {
    match PanelHandle::of(panel) {
        Some(handle) => handle.panel().title(cx),
        None => SharedString::from(panel.panel_name(cx)).into_any_element(),
    }
}
