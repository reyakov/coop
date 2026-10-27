#[cfg(not(target_arch = "wasm32"))]
use std::time::Duration;

use chat_ui::ToggleDictation;
#[cfg(not(target_arch = "wasm32"))]
use gpui::PathPromptOptions;
#[cfg(not(target_arch = "wasm32"))]
use gpui::Task;
use gpui::http_client::Url;
use gpui::{
    AnyElement, App, AppContext, Context, Entity, IntoElement, ParentElement, Render, Styled,
    Subscription, Window, div,
};
use gpui_kit::assets::IconName;
use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::input::{Input, InputEvent, InputState, NumberInput};
use gpui_kit::component::kbd::Kbd;
use gpui_kit::component::menu::{DropdownMenu, PopupMenuItem};
use gpui_kit::component::notification::Notification;
#[cfg(not(target_arch = "wasm32"))]
use gpui_kit::component::progress::Progress;
use gpui_kit::component::separator::Separator;
use gpui_kit::component::switch::Switch;
use gpui_kit::component::{Disableable, Sizable, WindowExt, h_flex, v_flex};
use settings::{AppSettings, AppearanceMode};
#[cfg(not(target_arch = "wasm32"))]
use speech::model::{self as speech_model};
use ui::{settings_block, settings_row};

/// Bounds for the theme number inputs
const FONT_SIZE_RANGE: (f32, f32) = (10., 32.);
const RADIUS_RANGE: (f32, f32) = (0., 24.);

pub fn init(window: &mut Window, cx: &mut App) -> Entity<Preferences> {
    cx.new(|cx| Preferences::new(window, cx))
}

pub struct Preferences {
    file_input: Entity<InputState>,
    font_size_input: Entity<InputState>,
    radius_input: Entity<InputState>,
    radius_lg_input: Entity<InputState>,
    #[cfg(not(target_arch = "wasm32"))]
    _speech_poll: Task<()>,
    /// Keeps the control subscriptions alive while the dialog is open
    _subscriptions: Vec<Subscription>,
}

impl Preferences {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let server = AppSettings::get_file_server(cx);
        let file_input = cx.new(|cx| {
            InputState::new(window, cx)
                .default_value(server.to_string())
                .placeholder("https://myblossom.com")
        });

        let theme = AppSettings::get_theme(cx);
        let font_size_input = cx.new(|cx| {
            InputState::new(window, cx)
                .default_value(theme.font_size.to_string())
                .step(1.)
                .min(FONT_SIZE_RANGE.0 as f64)
                .max(FONT_SIZE_RANGE.1 as f64)
        });
        let radius_input = cx.new(|cx| {
            InputState::new(window, cx)
                .default_value(
                    theme
                        .radius
                        .map(|value| value.to_string())
                        .unwrap_or_default(),
                )
                .placeholder("System")
                .step(1.)
                .min(RADIUS_RANGE.0 as f64)
                .max(RADIUS_RANGE.1 as f64)
        });
        let radius_lg_input = cx.new(|cx| {
            InputState::new(window, cx)
                .default_value(
                    theme
                        .radius_lg
                        .map(|value| value.to_string())
                        .unwrap_or_default(),
                )
                .placeholder("System")
                .step(1.)
                .min(RADIUS_RANGE.0 as f64)
                .max(RADIUS_RANGE.1 as f64)
        });

        let subscriptions = vec![
            Self::wire_number_input(&font_size_input, FONT_SIZE_RANGE, cx, |value, cx| {
                if let Some(font_size) = value {
                    AppSettings::update_theme(|theme| theme.font_size = font_size, cx);
                    AppSettings::apply_theme_style(cx);
                }
            }),
            Self::wire_number_input(&radius_input, RADIUS_RANGE, cx, |value, cx| {
                AppSettings::update_theme(|theme| theme.radius = value, cx);
                AppSettings::apply_theme_style(cx);
            }),
            Self::wire_number_input(&radius_lg_input, RADIUS_RANGE, cx, |value, cx| {
                AppSettings::update_theme(|theme| theme.radius_lg = value, cx);
                AppSettings::apply_theme_style(cx);
            }),
        ];

        #[cfg(not(target_arch = "wasm32"))]
        let _speech_poll = cx.spawn_in(window, async move |this, cx| {
            let timer = Duration::from_millis(500);
            let mut last = speech_model::status();

            loop {
                cx.background_executor().timer(timer).await;

                let status = speech_model::status();
                let error = speech_model::take_error();

                if status == last {
                    continue;
                }

                last = status;

                this.update_in(cx, |_, window, cx| {
                    if let Some(error) = error {
                        window.push_notification(Notification::error(error).autohide(false), cx);
                    }
                    cx.notify();
                })
                .ok();
            }
        });

        Self {
            file_input,
            font_size_input,
            radius_input,
            radius_lg_input,
            #[cfg(not(target_arch = "wasm32"))]
            _speech_poll,
            _subscriptions: subscriptions,
        }
    }

    /// Persist a theme number as it changes; an empty value stores `None`
    fn wire_number_input(
        state: &Entity<InputState>,
        range: (f32, f32),
        cx: &mut Context<Self>,
        persist: impl Fn(Option<f32>, &mut App) + 'static,
    ) -> Subscription {
        let (min, max) = range;
        let state = state.clone();

        cx.subscribe(&state, move |_, state, event: &InputEvent, cx| {
            if !matches!(event, InputEvent::Change) {
                return;
            }

            let value = state
                .read(cx)
                .value()
                .trim()
                .parse::<f32>()
                .ok()
                .map(|value| value.clamp(min, max));
            persist(value, cx);
        })
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
    fn choose_background(
        #[cfg_attr(not(target_arch = "wasm32"), allow(unused_variables))] window: &mut Window,
        cx: &mut App,
    ) {
        #[cfg(target_arch = "wasm32")]
        {
            let note = Notification::warning("Background images are not supported on web yet.");
            window.push_notification(note, cx);
        }

        #[cfg(not(target_arch = "wasm32"))]
        {
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

    fn render_speech(
        &mut self,
        #[cfg_attr(target_arch = "wasm32", allow(unused_variables))] cx: &mut Context<Self>,
    ) -> AnyElement {
        #[cfg(target_arch = "wasm32")]
        {
            return div().into_any_element();
        }

        #[cfg(not(target_arch = "wasm32"))]
        {
            let size = speech_model::total_size() / 1_000_000;
            let status = speech_model::status();

            let (description, control) = match &status {
                speech_model::Status::NotInstalled => (
                    format!("{size} MB download"),
                    Button::new("download-model")
                        .label("Download")
                        .secondary()
                        .small()
                        .on_click(|_, _window, cx| {
                            speech_model::start_download();
                            cx.refresh_windows();
                        })
                        .into_any_element(),
                ),
                speech_model::Status::Downloading { downloaded, total } => (
                    format!(
                        "Downloading · {} of {} MB",
                        downloaded / 1_000_000,
                        total / 1_000_000
                    ),
                    Button::new("cancel-download")
                        .label("Cancel")
                        .ghost()
                        .small()
                        .on_click(|_, _window, cx| {
                            speech_model::cancel_download();
                            cx.refresh_windows();
                        })
                        .into_any_element(),
                ),
                speech_model::Status::Installed => (
                    format!("Parakeet v3 · {size} MB"),
                    Button::new("remove-model")
                        .label("Remove")
                        .secondary()
                        .small()
                        .on_click(|_, _window, cx| {
                            speech_model::delete();
                            cx.refresh_windows();
                        })
                        .into_any_element(),
                ),
            };

            let mut block = v_flex().w_full().gap_3().child(settings_row(
                "Speech model",
                description,
                control,
                cx,
            ));

            if let speech_model::Status::Downloading { downloaded, total } = &status {
                let (downloaded, total) = (*downloaded, *total);

                block = block.child(
                    Progress::new("speech-model-download")
                        .value(downloaded as f32 / total.max(1) as f32 * 100.)
                        .small(),
                );
            }

            block.into_any_element()
        }
    }

    /// Shows the keystroke actually bound to the dictation action at startup
    fn render_dictation_shortcut(
        &self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let control = match Kbd::global_binding_for_action(&ToggleDictation, window) {
            Some(kbd) => kbd.into_any_element(),
            None => div().into_any_element(),
        };

        settings_row("Shortcut", "Press to start or stop dictation", control, cx)
    }
}

impl Render for Preferences {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        const SCREENING: &str = "Show an screening dialog to verify the unknown sender.";
        const WOT: &str = "Treat people followed by your contacts as known senders.";
        const AVATAR: &str = "Hide all avatar pictures to improve performance.";
        const APPEARANCE: &str =
            "Choose whether the app follows the system theme or uses a light/dark theme.";
        const FONT_SIZE: &str = "The base font size for the interface.";
        const RADIUS: &str = "Corner radius for UI elements. Empty follows the OS theme.";
        const RADIUS_LG: &str =
            "Corner radius for large elements like dialogs. Empty follows the OS theme.";
        const FOCUS_RING: &str = "Draw a ring around focused controls.";
        const NIP4E: &str = "Use a dedicated key to encrypt and decrypt messages.";
        const BACKGROUND: &str = "Show an image behind chat and group panels.";
        const MEDIA: &str = "Server for media attachments. Only blossom services are supported.";

        let screening = AppSettings::get_screening(cx);
        let wot_auto_trust = AppSettings::get_wot_auto_trust(cx);
        let hide_avatar = AppSettings::get_hide_avatar(cx);
        let nip4e = AppSettings::get_nip4e(cx);
        let appearance_mode = AppSettings::get_appearance_mode(cx);
        let theme = AppSettings::get_theme(cx);
        let panel_background = AppSettings::get_panel_background(cx);
        let panel_background_path = AppSettings::get_panel_background_path(cx);

        let background_label = match &panel_background_path {
            Some(path) => path.display().to_string(),
            None => String::from("Default Background"),
        };

        v_flex()
            .mt_2()
            .gap_4()
            .w_full()
            .child(
                v_flex()
                    .w_full()
                    .gap_3()
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
            .child(Separator::horizontal())
            .child(settings_row(
                "Appearance",
                APPEARANCE,
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
                cx,
            ))
            .child(Separator::horizontal())
            .child(
                v_flex()
                    .w_full()
                    .gap_3()
                    .child(settings_row(
                        "Font Size",
                        FONT_SIZE,
                        NumberInput::new(&self.font_size_input).w_24(),
                        cx,
                    ))
                    .child(settings_row(
                        "Corner Radius",
                        RADIUS,
                        NumberInput::new(&self.radius_input).w_24(),
                        cx,
                    ))
                    .child(settings_row(
                        "Large Corner Radius",
                        RADIUS_LG,
                        NumberInput::new(&self.radius_lg_input).w_24(),
                        cx,
                    ))
                    .child(settings_row(
                        "Focus Ring",
                        FOCUS_RING,
                        Switch::new("focus-ring")
                            .checked(theme.focus_ring)
                            .on_click(move |_, _window, cx| {
                                AppSettings::update_theme(
                                    |theme| theme.focus_ring = !theme.focus_ring,
                                    cx,
                                );
                                AppSettings::apply_theme_style(cx);
                            }),
                        cx,
                    )),
            )
            .child(Separator::horizontal())
            .child(
                v_flex()
                    .w_full()
                    .gap_3()
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
                    .child(settings_block(
                        "Image",
                        background_label,
                        h_flex()
                            .justify_end()
                            .gap_1()
                            .child(
                                Button::new("choose-background")
                                    .label("Choose image")
                                    .ghost()
                                    .small()
                                    .disabled(!panel_background)
                                    .on_click(|_, window, cx| {
                                        Self::choose_background(window, cx);
                                    }),
                            )
                            .child(
                                Button::new("reset-background")
                                    .label("Reset")
                                    .ghost()
                                    .small()
                                    .disabled(!panel_background || panel_background_path.is_none())
                                    .on_click(|_, _window, cx| {
                                        AppSettings::update_panel_background_path(None, cx);
                                        cx.refresh_windows();
                                    }),
                            ),
                        cx,
                    )),
            )
            .child(Separator::horizontal())
            .child(settings_row(
                "Decoupling Encryption Key",
                NIP4E,
                Switch::new("nip4e")
                    .checked(nip4e)
                    .on_click(move |_, _window, cx| {
                        AppSettings::update_nip4e(!nip4e, cx);
                    }),
                cx,
            ))
            .child(Separator::horizontal())
            .child(self.render_speech(cx))
            .child(self.render_dictation_shortcut(window, cx))
            .child(Separator::horizontal())
            .child(settings_block(
                "Media Upload Service",
                MEDIA,
                h_flex()
                    .gap_1()
                    .child(div().flex_1().child(Input::new(&self.file_input)))
                    .child(
                        Button::new("update-file-server")
                            .icon(IconName::Check)
                            .ghost()
                            .size_8()
                            .on_click(cx.listener(move |this, _ev, window, cx| {
                                this.update_file_server(window, cx)
                            })),
                    ),
                cx,
            ))
    }
}
