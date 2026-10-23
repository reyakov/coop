use gpui::http_client::Url;
use gpui::{
    App, AppContext, Context, Entity, IntoElement, ParentElement, PathPromptOptions, Render,
    SharedString, Styled, Window, div,
};
use gpui_kit::assets::IconName;
use gpui_kit::base::StyledExt as _;
use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::group_box::{GroupBox, GroupBoxVariants};
use gpui_kit::component::input::{Input, InputState};
use gpui_kit::component::menu::{DropdownMenu, PopupMenuItem};
use gpui_kit::component::notification::Notification;
use gpui_kit::component::switch::Switch;
use gpui_kit::component::{ActiveTheme, Disableable, Sizable, WindowExt, h_flex, v_flex};
use settings::{AppSettings, AppearanceMode};
use ui::settings_row;

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

    /// Update the file server (blossom) URL
    fn update_file_server(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let value = self.file_input.read(cx).value();

        match Url::parse(&value) {
            Ok(url) => {
                AppSettings::update_file_server(url, cx);
            }
            Err(e) => {
                window.push_notification(Notification::error(e.to_string()).autohide(false), cx);
            }
        }
    }

    /// Set the appearance mode (system, light or dark)
    fn set_appearance_mode(mode: AppearanceMode, cx: &mut App) {
        AppSettings::update_appearance_mode(mode, cx);
        AppSettings::global(cx).update(cx, |settings, cx| {
            settings.apply_theme(cx);
        });
    }

    /// Ask the user for an image to show behind chat and group panels
    fn choose_background(cx: &mut App) {
        let task = cx.prompt_for_paths(PathPromptOptions {
            files: true,
            directories: false,
            multiple: false,
            prompt: None,
        });

        cx.spawn(async move |cx| {
            let Ok(Ok(Some(mut paths))) = task.await else {
                return;
            };

            let Some(path) = paths.pop() else {
                return;
            };

            cx.update(|cx| {
                AppSettings::update_panel_background_path(Some(path), cx);
                cx.refresh_windows();
            });
        })
        .detach();
    }
}

impl Render for Preferences {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        const SCREENING: &str = "Show an screening dialog to verify the unknown sender.";
        const WOT: &str = "Treat people followed by your contacts as known senders.";
        const AVATAR: &str = "Hide all avatar pictures to improve performance.";
        const MODE: &str = "Follow the OS, or always use light or dark.";
        const NIP4E: &str = "Use a dedicated key to encrypt and decrypt messages.";
        const BACKGROUND: &str = "Show an image behind chat and group panels.";

        let screening = AppSettings::get_screening(cx);
        let wot_auto_trust = AppSettings::get_wot_auto_trust(cx);
        let hide_avatar = AppSettings::get_hide_avatar(cx);
        let nip4e = AppSettings::get_nip4e(cx);
        let appearance_mode = AppSettings::get_appearance_mode(cx);
        let panel_background = AppSettings::get_panel_background(cx);
        let panel_background_path = AppSettings::get_panel_background_path(cx);

        let background_label = match &panel_background_path {
            Some(path) => path.display().to_string(),
            None => String::from("Default Background"),
        };

        v_flex()
            .gap_4()
            .child(
                GroupBox::new()
                    .id("general")
                    .title("General")
                    .fill()
                    .child(settings_row(
                        "Screening",
                        SCREENING,
                        Switch::new("screening").checked(screening).on_click(
                            move |_, _window, cx| {
                                AppSettings::update_screening(!screening, cx);
                            },
                        ),
                        cx,
                    ))
                    .child(settings_row(
                        "Web of Trust",
                        WOT,
                        Switch::new("wot-auto-trust")
                            .checked(wot_auto_trust)
                            .on_click(move |_, _window, cx| {
                                AppSettings::update_wot_auto_trust(!wot_auto_trust, cx);
                            }),
                        cx,
                    ))
                    .child(settings_row(
                        "Hide user avatar",
                        AVATAR,
                        Switch::new("avatar").checked(hide_avatar).on_click(
                            move |_, _window, cx| {
                                AppSettings::update_hide_avatar(!hide_avatar, cx);
                            },
                        ),
                        cx,
                    )),
            )
            .child(
                GroupBox::new()
                    .id("appearance")
                    .title("Appearance")
                    .fill()
                    .child(
                        h_flex()
                            .gap_3()
                            .justify_between()
                            .child(
                                v_flex()
                                    .child(div().text_xs().child(SharedString::from("Mode")))
                                    .child(
                                        div()
                                            .text_xs()
                                            .text_color(cx.theme().muted_foreground)
                                            .child(SharedString::from(MODE)),
                                    ),
                            )
                            .child(
                                Button::new("appearance-mode")
                                    .label(appearance_mode.label())
                                    .ghost()
                                    .small()
                                    .dropdown_menu(|this, _window, _cx| {
                                        AppearanceMode::ALL.iter().fold(this, |menu, mode| {
                                            menu.item(PopupMenuItem::new(mode.label()).on_click(
                                                move |_, _window, cx| {
                                                    Self::set_appearance_mode(*mode, cx);
                                                },
                                            ))
                                        })
                                    }),
                            ),
                    ),
            )
            .child(
                GroupBox::new()
                    .id("background")
                    .title("Background")
                    .fill()
                    .child(settings_row(
                        "Panel background",
                        BACKGROUND,
                        Switch::new("panel-background")
                            .checked(panel_background)
                            .on_click(move |_, _window, cx| {
                                AppSettings::update_panel_background(!panel_background, cx);
                                cx.refresh_windows();
                            }),
                        cx,
                    ))
                    .child(
                        h_flex()
                            .gap_3()
                            .justify_between()
                            .child(
                                v_flex()
                                    .flex_1()
                                    .min_w_0()
                                    .child(div().text_xs().child(SharedString::from("Image")))
                                    .child(
                                        div()
                                            .text_xs()
                                            .text_color(cx.theme().muted_foreground)
                                            .truncate()
                                            .child(SharedString::from(background_label)),
                                    ),
                            )
                            .child(
                                h_flex()
                                    .flex_shrink_0()
                                    .gap_1()
                                    .child(
                                        Button::new("choose-background")
                                            .label("Choose image")
                                            .ghost()
                                            .small()
                                            .disabled(!panel_background)
                                            .on_click(|_, _window, cx| {
                                                Self::choose_background(cx);
                                            }),
                                    )
                                    .child(
                                        Button::new("reset-background")
                                            .label("Reset")
                                            .ghost()
                                            .small()
                                            .disabled(
                                                !panel_background
                                                    || panel_background_path.is_none(),
                                            )
                                            .on_click(|_, _window, cx| {
                                                AppSettings::update_panel_background_path(None, cx);
                                                cx.refresh_windows();
                                            }),
                                    ),
                            ),
                    ),
            )
            .child(
                GroupBox::new()
                    .id("experiments")
                    .title("Experiments")
                    .fill()
                    .child(settings_row(
                        "Decoupling Encryption Key",
                        NIP4E,
                        Switch::new("nip4e")
                            .checked(nip4e)
                            .on_click(move |_, _window, cx| {
                                AppSettings::update_nip4e(!nip4e, cx);
                            }),
                        cx,
                    )),
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
                                    .child(Input::new(&self.file_input).small())
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
                                    .text_xs()
                                    .font_medium()
                                    .italic()
                                    .text_color(cx.theme().muted_foreground)
                                    .child(SharedString::from("Only support blossom service")),
                            ),
                    ),
            )
    }
}
