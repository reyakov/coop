use gpui::http_client::Url;
use gpui::{
    App, AppContext, Context, Entity, IntoElement, ParentElement, Render, SharedString, Styled,
    Window, div,
};
use settings::AppSettings;
use theme::{ActiveTheme, AppExt as _, AppearanceMode, TextStyle, Typeset as _};
use ui::button::{Button, ButtonVariants};
use ui::group_box::{GroupBox, GroupBoxVariants};
use ui::input::{Input, InputState};
use ui::menu::{DropdownMenu, PopupMenuItem};
use ui::notification::Notification;
use ui::switch::Switch;
use ui::{IconName, Sizable, WindowExtension, h_flex, v_flex};

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
        cx.set_appearance_mode(mode);
    }
}

impl Render for Preferences {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        const SCREENING: &str = "Show an screening dialog to verify the unknown sender.";
        const AVATAR: &str = "Hide all avatar pictures to improve performance.";
        const MODE: &str = "Follow the OS, or always use light or dark.";
        const NIP4E: &str = "Use a dedicated key to encrypt and decrypt messages.";

        let screening = AppSettings::get_screening(cx);
        let hide_avatar = AppSettings::get_hide_avatar(cx);
        let nip4e = AppSettings::get_nip4e(cx);
        let appearance_mode = AppSettings::get_appearance_mode(cx);

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
                    ),
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
                                    .child(
                                        div()
                                            .text_style(TextStyle::Callout)
                                            .child(SharedString::from("Mode")),
                                    )
                                    .child(
                                        div()
                                            .text_style(TextStyle::Caption)
                                            .text_color(cx.theme().text_muted)
                                            .child(SharedString::from(MODE)),
                                    ),
                            )
                            .child(
                                Button::new("appearance-mode")
                                    .label(appearance_mode.label())
                                    .ghost_alt()
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
                    .id("experiments")
                    .title("Experiments")
                    .fill()
                    .child(
                        Switch::new("nip4e")
                            .label("Decoupling Encryption Key")
                            .description(NIP4E)
                            .checked(nip4e)
                            .on_click(move |_, _window, cx| {
                                AppSettings::update_nip4e(!nip4e, cx);
                            }),
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
                                    .text_style(TextStyle::Caption2)
                                    .italic()
                                    .text_color(cx.theme().text_faint)
                                    .child(SharedString::from("Only support blossom service")),
                            ),
                    ),
            )
    }
}
