use std::any::Any;
use std::sync::Arc;

use gpui::{
    AnyElement, AnyView, App, Element, Entity, EventEmitter, FocusHandle, Focusable, Render,
    SharedString, Window,
};
use gpui_base::dock::{PanelId, PanelState};

use crate::button::Button;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PanelEvent {}

pub trait Panel: EventEmitter<PanelEvent> + Render + Focusable {
    /// The name of the panel used to serialize, deserialize and identify the panel.
    fn panel_id(&self) -> SharedString;

    /// The title of the panel
    fn title(&self, _cx: &App) -> AnyElement {
        SharedString::from("Unnamed").into_any()
    }

    /// Whether the panel can be closed, default is `true`.
    fn closable(&self, _cx: &App) -> bool {
        true
    }

    /// Whether the panel is zoomable, default is `true`.
    fn zoomable(&self, _cx: &App) -> bool {
        true
    }

    /// Return false to hide panel, true to show panel, default is `true`.
    fn visible(&self, _cx: &App) -> bool {
        true
    }

    /// Set active state of the panel.
    fn set_active(&self, _active: bool, _window: &mut Window, _cx: &mut App) {}

    /// Set zoomed state of the panel.
    fn set_zoomed(&self, _zoomed: bool, _cx: &mut App) {}

    /// Toolbar buttons shown at the right of the title bar.
    fn toolbar_buttons(&self, _window: &Window, _cx: &App) -> Vec<Button> {
        vec![]
    }
}

pub(crate) trait PanelView: 'static + Send + Sync {
    fn panel_id(&self, cx: &App) -> SharedString;
    fn title(&self, cx: &App) -> AnyElement;
    fn closable(&self, cx: &App) -> bool;
    fn zoomable(&self, cx: &App) -> bool;
    fn visible(&self, cx: &App) -> bool;
    fn set_active(&self, active: bool, window: &mut Window, cx: &mut App);
    fn set_zoomed(&self, zoomed: bool, cx: &mut App);
    fn view(&self) -> AnyView;
    fn focus_handle(&self, cx: &App) -> FocusHandle;
}

impl<T: Panel> PanelView for Entity<T> {
    fn panel_id(&self, cx: &App) -> SharedString {
        self.read(cx).panel_id()
    }

    fn title(&self, cx: &App) -> AnyElement {
        self.read(cx).title(cx)
    }

    fn closable(&self, cx: &App) -> bool {
        self.read(cx).closable(cx)
    }

    fn zoomable(&self, cx: &App) -> bool {
        self.read(cx).zoomable(cx)
    }

    fn visible(&self, cx: &App) -> bool {
        self.read(cx).visible(cx)
    }

    fn set_active(&self, active: bool, window: &mut Window, cx: &mut App) {
        self.update(cx, |this, cx| {
            this.set_active(active, window, cx);
        })
    }

    fn set_zoomed(&self, zoomed: bool, cx: &mut App) {
        self.update(cx, |this, cx| {
            this.set_zoomed(zoomed, cx);
        })
    }

    fn view(&self) -> AnyView {
        self.clone().into()
    }

    fn focus_handle(&self, cx: &App) -> FocusHandle {
        self.read(cx).focus_handle(cx)
    }
}

#[derive(Clone)]
pub struct PanelHandle {
    id: PanelId,
    panel: Arc<dyn PanelView>,
}

impl PanelHandle {
    pub fn new<P: Panel>(panel: Entity<P>) -> Self {
        Self {
            id: PanelId::from(panel.entity_id()),
            panel: Arc::new(panel),
        }
    }

    /// Downcast a base handle back to the coop handle.
    pub(crate) fn of(panel: &Arc<dyn gpui_base::dock::PanelView>) -> Option<&Self> {
        panel.as_any().downcast_ref::<Self>()
    }

    pub(crate) fn panel(&self) -> &Arc<dyn PanelView> {
        &self.panel
    }
}

impl gpui_base::dock::PanelView for PanelHandle {
    fn panel_name(&self, _: &App) -> &'static str {
        "CoopPanel"
    }

    fn panel_id(&self, _: &App) -> PanelId {
        self.id
    }

    fn closable(&self, cx: &App) -> bool {
        self.panel.closable(cx)
    }

    fn zoomable(&self, cx: &App) -> bool {
        self.panel.zoomable(cx)
    }

    fn visible(&self, cx: &App) -> bool {
        self.panel.visible(cx)
    }

    fn set_active(&self, active: bool, window: &mut Window, cx: &mut App) {
        self.panel.set_active(active, window, cx);
    }

    fn set_zoomed(&self, zoomed: bool, _: &mut Window, cx: &mut App) {
        self.panel.set_zoomed(zoomed, cx);
    }

    fn on_added_to(
        &self,
        _group: gpui::WeakEntity<gpui_base::dock::TabGroup>,
        _: &mut Window,
        _: &mut App,
    ) {
    }

    fn on_removed(&self, _: &mut Window, _: &mut App) {}

    fn view(&self) -> AnyView {
        self.panel.view()
    }

    fn focus_handle(&self, cx: &App) -> FocusHandle {
        self.panel.focus_handle(cx)
    }

    fn dump(&self, cx: &App) -> PanelState {
        PanelState::new(self.panel_name(cx))
    }

    fn as_any(&self) -> &dyn Any {
        self
    }
}
