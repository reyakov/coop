use gpui::{App, Div, IntoElement, ParentElement, SharedString, Styled, div};
use gpui_kit::base::StyledExt as _;
use gpui_kit::component::{ActiveTheme, h_flex, v_flex};

/// A settings row: the label and description on the left, the control on the right.
pub fn settings_row(
    label: impl Into<SharedString>,
    description: impl Into<SharedString>,
    control: impl IntoElement,
    cx: &App,
) -> Div {
    h_flex()
        .w_full()
        .gap_4()
        .items_center()
        .justify_between()
        .child(
            v_flex()
                .flex_1()
                .min_w_0()
                .child(div().text_sm().font_semibold().child(label.into()))
                .child(
                    div()
                        .text_xs()
                        .text_color(cx.theme().muted_foreground)
                        .child(description.into()),
                ),
        )
        .child(
            h_flex()
                .w_40()
                .flex_shrink_0()
                .justify_end()
                .items_center()
                .child(control),
        )
}

/// A full-width settings block with the label and description in one header.
pub fn settings_block(
    label: impl Into<SharedString>,
    description: impl Into<SharedString>,
    control: impl IntoElement,
    cx: &App,
) -> impl IntoElement {
    v_flex()
        .w_full()
        .gap_3()
        .child(
            v_flex()
                .w_full()
                .child(div().text_sm().font_semibold().child(label.into()))
                .child(
                    div()
                        .text_xs()
                        .text_color(cx.theme().muted_foreground)
                        .truncate()
                        .child(description.into()),
                ),
        )
        .child(control)
}
