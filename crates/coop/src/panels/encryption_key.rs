use gpui::{
    AnyElement, App, AppContext, Context, Entity, EventEmitter, FocusHandle, Focusable,
    IntoElement, Render, SharedString, Styled, Window,
};
use ui::dock_area::panel::{Panel, PanelEvent};
use ui::v_flex;

pub fn init(window: &mut Window, cx: &mut App) -> Entity<EncryptionPanel> {
    cx.new(|cx| EncryptionPanel::new(window, cx))
}

#[derive(Debug)]
pub struct EncryptionPanel {
    name: SharedString,
    focus_handle: FocusHandle,
}

impl EncryptionPanel {
    fn new(_window: &mut Window, cx: &mut Context<Self>) -> Self {
        Self {
            name: "Encryption".into(),
            focus_handle: cx.focus_handle(),
        }
    }
}

impl Panel for EncryptionPanel {
    fn panel_id(&self) -> SharedString {
        self.name.clone()
    }

    fn title(&self, _cx: &App) -> AnyElement {
        self.name.clone().into_any_element()
    }
}

impl EventEmitter<PanelEvent> for EncryptionPanel {}

impl Focusable for EncryptionPanel {
    fn focus_handle(&self, _: &App) -> gpui::FocusHandle {
        self.focus_handle.clone()
    }
}

impl Render for EncryptionPanel {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        v_flex()
            .size_full()
            .items_center()
            .justify_center()
            .p_2()
            .gap_10()
    }
}
