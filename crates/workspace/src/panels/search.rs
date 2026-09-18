use gpui::{
    AnyElement, App, AppContext, Context, Entity, EventEmitter, FocusHandle, Focusable,
    IntoElement, ParentElement, Render, SharedString, Styled, Window,
};
use theme::ActiveTheme;
use ui::dock::{Panel, PanelEvent};
use ui::{Icon, IconName, Sizable, h_flex};

pub fn init(window: &mut Window, cx: &mut App) -> Entity<SearchPanel> {
    cx.new(|cx| SearchPanel::new(window, cx))
}

pub struct SearchPanel {
    name: SharedString,
    focus_handle: FocusHandle,
}

impl SearchPanel {
    fn new(_window: &mut Window, cx: &mut App) -> Self {
        Self {
            name: "Search".into(),
            focus_handle: cx.focus_handle(),
        }
    }
}

impl Panel for SearchPanel {
    fn panel_id(&self) -> SharedString {
        self.name.clone()
    }

    fn title(&self, cx: &App) -> AnyElement {
        h_flex()
            .gap_1p5()
            .child(
                Icon::new(IconName::Search)
                    .small()
                    .text_color(cx.theme().icon_muted),
            )
            .child(self.name.clone())
            .into_any_element()
    }
}

impl EventEmitter<PanelEvent> for SearchPanel {}

impl Focusable for SearchPanel {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Render for SearchPanel {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        h_flex()
            .size_full()
            .justify_center()
            .text_sm()
            .text_color(cx.theme().text_muted)
            .child(self.name.clone())
    }
}
