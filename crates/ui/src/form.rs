use gpui::{App, Div, IntoElement, ParentElement, SharedString, Styled, div};
use gpui_kit::component::{ActiveTheme, h_flex, v_flex};

/// A settings row: the label and description on the left, the control on the right.
pub fn settings_row(
    label: impl Into<SharedString>,
    description: impl Into<SharedString>,
    control: impl IntoElement,
    cx: &App,
) -> Div {
    h_flex()
        .gap_3()
        .items_center()
        .justify_between()
        .child(
            v_flex()
                .flex_1()
                .min_w_0()
                .child(div().text_sm().child(label.into()))
                .child(
                    div()
                        .text_xs()
                        .text_color(cx.theme().muted_foreground)
                        .child(description.into()),
                ),
        )
        .child(control)
}
