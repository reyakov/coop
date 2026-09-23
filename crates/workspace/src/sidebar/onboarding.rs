use gpui::{App, InteractiveElement, IntoElement, ParentElement, Styled, Window, div, svg};
use theme::{ActiveTheme, TABBAR_HEIGHT};
use ui::button::{Button, ButtonVariants};
use ui::{StyledExt, h_flex, title_bar_drag_handlers, v_flex};

use crate::dialogs::import;

const TITLE: &str = "Welcome to Coop!";
const DESCRIPTION: &str = "Chat Freely, Stay Private on Nostr.";

pub(super) fn render(window: &mut Window, cx: &mut App) -> impl IntoElement {
    v_flex()
        .size_full()
        .relative()
        .bg(cx.theme().surface_background)
        .child(title_bar_drag_handlers(
            div()
                .id("onboarding-drag")
                .absolute()
                .top_0()
                .left_0()
                .h(TABBAR_HEIGHT)
                .w_full(),
            window,
            cx,
        ))
        .child(
            v_flex()
                .size_full()
                .justify_end()
                .gap_4()
                .p_4()
                .child(
                    h_flex()
                        .gap_2()
                        .child(
                            svg()
                                .path("brand/coop.svg")
                                .size_8()
                                .text_color(cx.theme().icon_muted),
                        )
                        .child(
                            v_flex().child(div().font_semibold().child(TITLE)).child(
                                div()
                                    .text_xs()
                                    .text_color(cx.theme().text_muted)
                                    .child(DESCRIPTION),
                            ),
                        ),
                )
                .child(
                    v_flex()
                        .gap_2()
                        .w_full()
                        .child(
                            Button::new("join-now")
                                .label("Join now")
                                .primary()
                                .font_semibold()
                                .h_8()
                                .w_full(),
                        )
                        .child(
                            Button::new("import-identity")
                                .label("Import identity")
                                .secondary()
                                .font_semibold()
                                .h_8()
                                .w_full()
                                .on_click(|_event, window, cx| import::open(window, cx)),
                        ),
                ),
        )
}
