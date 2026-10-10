use gpui::prelude::FluentBuilder;
use gpui::{
    App, AppContext, Context, Entity, IntoElement, ParentElement, Render, SharedString, Styled,
    Window, px,
};
use gpui_base::Tooltip as BaseTooltip;
use theme::{ActiveTheme, TextStyle, Typeset as _, control_radius};

pub struct Tooltip {
    text: SharedString,
}

impl Tooltip {
    pub fn new(text: impl Into<SharedString>, _window: &mut Window, cx: &mut App) -> Entity<Self> {
        cx.new(|_| Self { text: text.into() })
    }
}

impl Render for Tooltip {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        BaseTooltip::new("tooltip")
            .font_family(cx.theme().font_sans.clone())
            .m_3()
            .p_1p5()
            .border_1()
            .border_color(cx.theme().border)
            .bg(cx.theme().surface_overlay)
            .when(cx.theme().shadow, |this| this.shadow_sm())
            .rounded(px(control_radius()))
            .text_style(TextStyle::Caption)
            .text_color(cx.theme().text)
            .child(self.text.clone())
    }
}
