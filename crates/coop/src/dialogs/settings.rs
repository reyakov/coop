use gpui::http_client::Url;
use gpui::{
    div, px, App, AppContext, Context, Entity, IntoElement, ParentElement, Render, SharedString,
    Styled, Window,
};
use settings::{AppSettings, AuthMode};
use theme::ActiveTheme;
use ui::button::{Button, ButtonVariants};
use ui::group_box::{GroupBox, GroupBoxVariants};
use ui::input::{InputState, TextInput};
use ui::menu::{DropdownMenu, PopupMenuItem};
use ui::notification::Notification;
use ui::switch::Switch;
use ui::{h_flex, v_flex, IconName, Sizable, WindowExtension};

pub fn init(window: &mut Window, cx: &mut App) -> Entity<Preferences> {
    cx.new(|cx| Preferences::new(window, cx))
}

pub struct Preferences {
    file_input: Entity<InputState>,
}

impl Preferences {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let server = AppSettings::get_file_server(cx);
        let file_input = cx.new(|cx| {
            InputState::new(window, cx)
                .default_value(server.to_string())
                .placeholder("https://myblossom.com")
        });

        Self { file_input }
    }

    fn update_file_server(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let value = self.file_input.read(cx).value();

        match Url::parse(&value) {
            Ok(url) => {
                AppSettings::update_file_server(url, cx);
            }
            Err(e) => {
                window.push_notification(Notification::error(e.to_string()), cx);
            }
        }
    }
}

impl Render for Preferences {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        const SCREENING: &str =
            "When opening a request, a popup will appear to help you identify the sender.";
        const AVATAR: &str =
            "Hide all avatar pictures to improve performance and protect your privacy.";
        const AUTH: &str = "Authentication before send events for some relays.";

        let screening = AppSettings::get_screening(cx);
        let hide_avatar = AppSettings::get_hide_avatar(cx);
        let auth_mode = AppSettings::get_auth_mode(cx);

        v_flex()
            .gap_4()
            .child(
                GroupBox::new()
                    .id("general")
                    .title("General")
                    .fill()
                    .child(
                        Switch::new("screening")
                            .label("Screening")
                            .description(SCREENING)
                            .checked(screening)
                            .on_click(move |_, _window, cx| {
                                AppSettings::update_screening(!screening, cx);
                            }),
                    )
                    .child(
                        Switch::new("avatar")
                            .label("Hide user avatar")
                            .description(AVATAR)
                            .checked(hide_avatar)
                            .on_click(move |_, _window, cx| {
                                AppSettings::update_hide_avatar(!hide_avatar, cx);
                            }),
                    )
                    .child(
                        h_flex()
                            .gap_3()
                            .justify_between()
                            .child(
                                v_flex()
                                    .child(
                                        div()
                                            .text_sm()
                                            .child(SharedString::from("Relay authentication")),
                                    )
                                    .child(
                                        div()
                                            .text_xs()
                                            .text_color(cx.theme().text_muted)
                                            .child(SharedString::from(AUTH)),
                                    ),
                            )
                            .child(
                                Button::new("auth")
                                    .label(auth_mode.to_string())
                                    .ghost_alt()
                                    .small()
                                    .dropdown_menu(|this, _window, _cx| {
                                        this.min_w(px(256.))
                                            .item(
                                                PopupMenuItem::new("Auto authentication").on_click(
                                                    |_ev, _window, cx| {
                                                        AppSettings::update_auth_mode(
                                                            AuthMode::Auto,
                                                            cx,
                                                        );
                                                    },
                                                ),
                                            )
                                            .item(PopupMenuItem::new("Ask every time").on_click(
                                                |_ev, _window, cx| {
                                                    AppSettings::update_auth_mode(
                                                        AuthMode::Manual,
                                                        cx,
                                                    );
                                                },
                                            ))
                                    }),
                            ),
                    ),
            )
            .child(
                GroupBox::new()
                    .id("media")
                    .title("Media Upload Service")
                    .fill()
                    .child(
                        v_flex()
                            .gap_0p5()
                            .child(
                                h_flex()
                                    .gap_1()
                                    .child(TextInput::new(&self.file_input).text_xs().small())
                                    .child(
                                        Button::new("update-file-server")
                                            .icon(IconName::Check)
                                            .ghost()
                                            .size_8()
                                            .on_click(cx.listener(move |this, _ev, window, cx| {
                                                this.update_file_server(window, cx)
                                            })),
                                    ),
                            )
                            .child(
                                div()
                                    .text_size(px(10.))
                                    .italic()
                                    .text_color(cx.theme().text_placeholder)
                                    .child(SharedString::from("Only support blossom service")),
                            ),
                    ),
            )
    }
}
