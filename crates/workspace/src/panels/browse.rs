use gpui::{
    AnyElement, App, AppContext, Context, Entity, EventEmitter, FocusHandle, Focusable,
    IntoElement, ParentElement, Render, SharedString, Styled, Window,
};
use theme::ActiveTheme;
use ui::dock::{Panel, PanelEvent};
use ui::{Icon, IconName, Sizable, h_flex};

pub fn init(window: &mut Window, cx: &mut App) -> Entity<BrowsePanel> {
    cx.new(|cx| BrowsePanel::new(window, cx))
}

pub struct BrowsePanel {
    name: SharedString,
    focus_handle: FocusHandle,
}

impl BrowsePanel {
    fn new(_window: &mut Window, cx: &mut App) -> Self {
        Self {
            name: "Browse".into(),
            focus_handle: cx.focus_handle(),
        }
    }
}

impl Panel for BrowsePanel {
    fn panel_id(&self) -> SharedString {
        self.name.clone()
    }

    fn title(&self, cx: &App) -> AnyElement {
        h_flex()
            .gap_1p5()
            .child(
                Icon::new(IconName::Compass)
                    .small()
                    .text_color(cx.theme().icon_muted),
            )
            .child(self.name.clone())
            .into_any_element()
    }
}

impl EventEmitter<PanelEvent> for BrowsePanel {}

impl Focusable for BrowsePanel {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Render for BrowsePanel {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        h_flex()
            .size_full()
            .justify_center()
            .text_sm()
            .text_color(cx.theme().text_muted)
            .child(self.name.clone())
    }
}
