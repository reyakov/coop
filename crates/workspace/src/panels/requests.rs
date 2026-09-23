use gpui::{
    AnyElement, App, AppContext, Context, Entity, EventEmitter, FocusHandle, Focusable,
    IntoElement, ParentElement, Render, SharedString, Styled, Window,
};
use theme::ActiveTheme;
use ui::dock::{Panel, PanelEvent};
use ui::{Icon, IconName, Sizable, h_flex};

pub fn init(window: &mut Window, cx: &mut App) -> Entity<RequestsPanel> {
    cx.new(|cx| RequestsPanel::new(window, cx))
}

pub struct RequestsPanel {
    name: SharedString,
    focus_handle: FocusHandle,
}

impl RequestsPanel {
    fn new(_window: &mut Window, cx: &mut App) -> Self {
        Self {
            name: "Requests".into(),
            focus_handle: cx.focus_handle(),
        }
    }
}

impl Panel for RequestsPanel {
    fn panel_id(&self) -> SharedString {
        self.name.clone()
    }

    fn title(&self, cx: &App) -> AnyElement {
        h_flex()
            .gap_1p5()
            .child(
                Icon::new(IconName::Invite)
                    .small()
                    .text_color(cx.theme().icon_muted),
            )
            .child(self.name.clone())
            .into_any_element()
    }
}

impl EventEmitter<PanelEvent> for RequestsPanel {}

impl Focusable for RequestsPanel {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Render for RequestsPanel {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        h_flex()
            .size_full()
            .justify_center()
            .text_sm()
            .text_color(cx.theme().text_muted)
            .child(self.name.clone())
    }
}
