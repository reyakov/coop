use std::sync::Arc;

use gpui::prelude::FluentBuilder;
use gpui::{
    App, AppContext, Axis, Context, DismissEvent, Entity, EventEmitter, FocusHandle, Focusable,
    IntoElement, ParentElement, Pixels, Render, SharedString, Styled, Subscription, WeakEntity,
    Window,
};
use smallvec::SmallVec;
use theme::{ActiveTheme, CLIENT_SIDE_DECORATION_ROUNDING};

use super::{DockArea, PanelEvent};
use crate::dock_area::panel::{Panel, PanelView};
use crate::dock_area::tab_panel::TabPanel;
use crate::resizable::{
    resizable_panel, ResizablePanelEvent, ResizablePanelGroup, ResizablePanelState, ResizableState,
    PANEL_MIN_SIZE,
};
use crate::{h_flex, AxisExt as _, Placement};

pub struct StackPanel {
    pub(super) parent: Option<WeakEntity<StackPanel>>,
    pub(super) axis: Axis,
    focus_handle: FocusHandle,
    pub(crate) panels: SmallVec<[Arc<dyn PanelView>; 2]>,
    state: Entity<ResizableState>,
    _subscriptions: Vec<Subscription>,
}

impl Panel for StackPanel {
    fn panel_id(&self) -> SharedString {
        "StackPanel".into()
    }

    fn title(&self, _cx: &App) -> gpui::AnyElement {
        "StackPanel".into_any_element()
    }
}

impl StackPanel {
    pub fn new(axis: Axis, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let state = cx.new(|_| ResizableState::default());

        // Bubble up the resize event.
        let subscriptions =
            vec![
                cx.subscribe_in(&state, window, |_, _, _: &ResizablePanelEvent, _, cx| {
                    cx.emit(PanelEvent::LayoutChanged)
                }),
            ];

        Self {
            axis,
            parent: None,
            focus_handle: cx.focus_handle(),
            panels: SmallVec::new(),
            state,
            _subscriptions: subscriptions,
        }
    }

    /// The first level of the stack panel is root, will not have a parent.
    fn is_root(&self) -> bool {
        self.parent.is_none()
    }

    /// Return true if self or parent only have last panel.
    pub fn is_last_panel(&self, cx: &App) -> bool {
        if self.panels.len() > 1 {
            return false;
        }

        if let Some(parent) = &self.parent {
            if let Some(parent) = parent.upgrade() {
                return parent.read(cx).is_last_panel(cx);
            }
        }

        true
    }

    pub fn panels_len(&self) -> usize {
        self.panels.len()
    }

    /// Return the index of the panel.
    pub fn index_of_panel(&self, panel: Arc<dyn PanelView>) -> Option<usize> {
        self.panels.iter().position(|p| p == &panel)
    }

    /// Add a panel at the end of the stack.
    pub fn add_panel(
        &mut self,
        panel: Arc<dyn PanelView>,
        size: Option<Pixels>,
        dock_area: WeakEntity<DockArea>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.insert_panel(panel, self.panels.len(), size, dock_area, window, cx);
    }

    pub fn add_panel_at(
        &mut self,
        panel: Arc<dyn PanelView>,
        placement: Placement,
        size: Option<Pixels>,
        dock_area: WeakEntity<DockArea>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.insert_panel_at(
            panel,
            self.panels_len(),
            placement,
            size,
            dock_area,
            window,
            cx,
        );
    }

    #[allow(clippy::too_many_arguments)]
    pub fn insert_panel_at(
        &mut self,
        panel: Arc<dyn PanelView>,
        ix: usize,
        placement: Placement,
        size: Option<Pixels>,
        dock_area: WeakEntity<DockArea>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match placement {
            Placement::Top | Placement::Left => {
                self.insert_panel_before(panel, ix, size, dock_area, window, cx)
            }
            Placement::Right | Placement::Bottom => {
                self.insert_panel_after(panel, ix, size, dock_area, window, cx)
            }
        }
    }

    /// Insert a panel at the index.
    pub fn insert_panel_before(
        &mut self,
        panel: Arc<dyn PanelView>,
        ix: usize,
        size: Option<Pixels>,
        dock_area: WeakEntity<DockArea>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.insert_panel(panel, ix, size, dock_area, window, cx);
    }

    /// Insert a panel after the index.
    pub fn insert_panel_after(
        &mut self,
        panel: Arc<dyn PanelView>,
        ix: usize,
        size: Option<Pixels>,
        dock_area: WeakEntity<DockArea>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.insert_panel(panel, ix + 1, size, dock_area, window, cx);
    }

    fn insert_panel(
        &mut self,
        panel: Arc<dyn PanelView>,
        ix: usize,
        size: Option<Pixels>,
        dock_area: WeakEntity<DockArea>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        // If the panel is already in the stack, return.
        if self.index_of_panel(panel.clone()).is_some() {
            return;
        }

        let view = cx.entity().clone();

        window.defer(cx, {
            let panel = panel.clone();

            move |window, cx| {
                // If the panel is a TabPanel, set its parent to this.
                if let Ok(tab_panel) = panel.view().downcast::<TabPanel>() {
                    tab_panel.update(cx, |tab_panel, _| tab_panel.set_parent(view.downgrade()));
                } else if let Ok(stack_panel) = panel.view().downcast::<Self>() {
                    stack_panel.update(cx, |stack_panel, _| {
                        stack_panel.parent = Some(view.downgrade())
                    });
                }

                // Subscribe to the panel's layout change event.
                _ = dock_area.update(cx, |this, cx| {
                    if let Ok(tab_panel) = panel.view().downcast::<TabPanel>() {
                        this.subscribe_panel(&tab_panel, window, cx);
                    } else if let Ok(stack_panel) = panel.view().downcast::<Self>() {
                        this.subscribe_panel(&stack_panel, window, cx);
                    }
                });
            }
        });

        let ix = if ix > self.panels.len() {
            self.panels.len()
        } else {
            ix
        };

        // Get avg size of all panels to insert new panel, if size is None.
        let size = match size {
            Some(size) => size,
            None => {
                let state = self.state.read(cx);
                (state.container_size() / (state.sizes().len() + 1) as f32).max(PANEL_MIN_SIZE)
            }
        };

        // Insert panel
        self.panels.insert(ix, panel.clone());

        // Update resizable state
        self.state.update(cx, |state, cx| {
            state.insert_panel(Some(size), Some(ix), cx);
        });

        cx.emit(PanelEvent::LayoutChanged);
        cx.notify();
    }

    /// Remove panel from the stack.
    ///
    /// If `ix` is not found, do nothing.
    pub fn remove_panel(
        &mut self,
        panel: Arc<dyn PanelView>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(ix) = self.index_of_panel(panel.clone()) else {
            return;
        };

        self.panels.remove(ix);
        self.state.update(cx, |state, cx| {
            state.remove_panel(ix, cx);
        });

        cx.emit(PanelEvent::LayoutChanged);

        self.remove_self_if_empty(window, cx);
    }

    /// Replace the old panel with the new panel at same index.
    pub fn replace_panel(
        &mut self,
        old_panel: Arc<dyn PanelView>,
        new_panel: Entity<StackPanel>,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(ix) = self.index_of_panel(old_panel.clone()) {
            self.panels[ix] = Arc::new(new_panel.clone());
            self.state.update(cx, |state, cx| {
                state.replace_panel(ix, ResizablePanelState::default(), cx);
            });
            cx.emit(PanelEvent::LayoutChanged);
        }
    }

    /// If children is empty, remove self from parent view.
    pub fn remove_self_if_empty(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.is_root() {
            return;
        }

        if !self.panels.is_empty() {
            return;
        }

        let view = cx.entity().clone();
        if let Some(parent) = self.parent.as_ref() {
            _ = parent.update(cx, |parent, cx| {
                parent.remove_panel(Arc::new(view.clone()), window, cx);
            });
        }

        cx.emit(PanelEvent::LayoutChanged);
        cx.notify();
    }

    /// Find the first top left in the stack.
    pub fn left_top_tab_panel(&self, check_parent: bool, cx: &App) -> Option<Entity<TabPanel>> {
        if check_parent {
            if let Some(parent) = self.parent.as_ref().and_then(|parent| parent.upgrade()) {
                if let Some(panel) = parent.read(cx).left_top_tab_panel(true, cx) {
                    return Some(panel);
                }
            }
        }

        let first_panel = self.panels.first();
        if let Some(view) = first_panel {
            if let Ok(tab_panel) = view.view().downcast::<TabPanel>() {
                Some(tab_panel)
            } else if let Ok(stack_panel) = view.view().downcast::<StackPanel>() {
                stack_panel.read(cx).left_top_tab_panel(false, cx)
            } else {
                None
            }
        } else {
            None
        }
    }

    /// Find the first top right in the stack.
    pub fn right_top_tab_panel(&self, check_parent: bool, cx: &App) -> Option<Entity<TabPanel>> {
        if check_parent {
            if let Some(parent) = self.parent.as_ref().and_then(|parent| parent.upgrade()) {
                if let Some(panel) = parent.read(cx).right_top_tab_panel(true, cx) {
                    return Some(panel);
                }
            }
        }

        let panel = if self.axis.is_vertical() {
            self.panels.first()
        } else {
            self.panels.last()
        };

        if let Some(view) = panel {
            if let Ok(tab_panel) = view.view().downcast::<TabPanel>() {
                Some(tab_panel)
            } else if let Ok(stack_panel) = view.view().downcast::<StackPanel>() {
                stack_panel.read(cx).right_top_tab_panel(false, cx)
            } else {
                None
            }
        } else {
            None
        }
    }

    /// Remove all panels from the stack.
    pub fn remove_all_panels(&mut self, _: &mut Window, cx: &mut Context<Self>) {
        self.panels.clear();
        self.state.update(cx, |state, cx| {
            state.clear();
            cx.notify();
        });
    }

    /// Change the axis of the stack panel.
    pub fn set_axis(&mut self, axis: Axis, _: &mut Window, cx: &mut Context<Self>) {
        self.axis = axis;
        cx.notify();
    }
}

impl Focusable for StackPanel {
    fn focus_handle(&self, _cx: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl EventEmitter<PanelEvent> for StackPanel {}
impl EventEmitter<DismissEvent> for StackPanel {}

impl Render for StackPanel {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        h_flex()
            .size_full()
            .overflow_hidden()
            .bg(cx.theme().panel_background)
            .when(cx.theme().platform.is_linux(), |this| {
                this.rounded_br(CLIENT_SIDE_DECORATION_ROUNDING)
            })
            .child(
                ResizablePanelGroup::new("stack-panel-group")
                    .with_state(&self.state)
                    .axis(self.axis)
                    .children(self.panels.clone().into_iter().map(|panel| {
                        resizable_panel()
                            .child(panel.view())
                            .visible(panel.visible(cx))
                    })),
            )
    }
}
