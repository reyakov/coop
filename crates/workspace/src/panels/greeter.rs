use gpui::{
    AnyElement, App, AppContext, Context, Entity, EventEmitter, FocusHandle, Focusable,
    IntoElement, ParentElement, Render, SharedString, Styled, Window, div, px, svg,
};
use state::NostrRegistry;
use theme::{ActiveTheme, TextStyle, Typeset as _};
use ui::button::{Button, ButtonVariants};
use ui::dock::{Panel, PanelEvent};
use ui::{Icon, IconName, Sizable, StyledExt, h_flex, v_flex};

use crate::dialogs::profile;

pub fn init(window: &mut Window, cx: &mut App) -> Entity<GreeterPanel> {
    cx.new(|cx| GreeterPanel::new(window, cx))
}

pub struct GreeterPanel {
    name: SharedString,
    focus_handle: FocusHandle,
}

impl GreeterPanel {
    fn new(_window: &mut Window, cx: &mut App) -> Self {
        Self {
            name: "Onboarding".into(),
            focus_handle: cx.focus_handle(),
        }
    }

    fn open_profile(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let nostr = NostrRegistry::global(cx);

        if let Some(public_key) = nostr.read(cx).current_user() {
            profile::open(public_key, window, cx);
        }
    }
}

impl Panel for GreeterPanel {
    fn panel_id(&self) -> SharedString {
        self.name.clone()
    }

    fn title(&self, cx: &App) -> AnyElement {
        h_flex()
            .gap_1()
            .text_style(TextStyle::Caption)
            .child(
                svg()
                    .path("brand/coop.svg")
                    .size_4()
                    .text_color(cx.theme().text_muted),
            )
            .child("Welcome")
            .into_any_element()
    }
}

impl EventEmitter<PanelEvent> for GreeterPanel {}

impl Focusable for GreeterPanel {
    fn focus_handle(&self, _: &App) -> gpui::FocusHandle {
        self.focus_handle.clone()
    }
}

impl Render for GreeterPanel {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        const TITLE: &str = "Welcome to Coop!";
        const DESCRIPTION: &str = "Chat Freely, Stay Private on Nostr.";

        h_flex()
            .size_full()
            .items_center()
            .justify_center()
            .p_2()
            .child(
                v_flex()
                    .h_full()
                    .w_112()
                    .gap_6()
                    .items_center()
                    .justify_center()
                    .child(
                        h_flex()
                            .mb_4()
                            .gap(px(theme::SPACE))
                            .w_full()
                            .child(
                                svg()
                                    .path("brand/coop.svg")
                                    .size_12()
                                    .text_color(cx.theme().text_faint),
                            )
                            .child(
                                v_flex()
                                    .child(
                                        div()
                                            .font_semibold()
                                            .text_color(cx.theme().text)
                                            .child(SharedString::from(TITLE)),
                                    )
                                    .child(
                                        div()
                                            .text_style(TextStyle::Caption)
                                            .text_color(cx.theme().text_muted)
                                            .child(SharedString::from(DESCRIPTION)),
                                    ),
                            ),
                    )
                    .child(
                        v_flex()
                            .gap(px(theme::SPACE))
                            .w_full()
                            .child(
                                h_flex()
                                    .gap(px(theme::SPACE))
                                    .w_full()
                                    .text_style(TextStyle::Caption)
                                    .font_semibold()
                                    .text_color(cx.theme().text_muted)
                                    .child(SharedString::from("Get Started"))
                                    .child(div().flex_1().h_px().bg(cx.theme().border)),
                            )
                            .child(
                                v_flex()
                                    .gap(px(theme::SPACE))
                                    .w_full()
                                    .justify_start()
                                    .items_start()
                                    .child(
                                        Button::new("profile")
                                            .icon(Icon::new(IconName::Profile))
                                            .label("Update profile")
                                            .ghost()
                                            .small()
                                            .on_click(cx.listener(move |this, _, window, cx| {
                                                this.open_profile(window, cx)
                                            })),
                                    ),
                            ),
                    ),
            )
    }
}
