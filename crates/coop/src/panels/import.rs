use std::time::Duration;

use anyhow::anyhow;
use gpui::prelude::FluentBuilder;
use gpui::{
    div, relative, AnyElement, App, AppContext, Context, Entity, EventEmitter, FocusHandle,
    Focusable, IntoElement, ParentElement, Render, SharedString, Styled, Subscription, Window,
};
use nostr_connect::prelude::*;
use smallvec::{smallvec, SmallVec};
use state::{CoopAuthUrlHandler, NostrRegistry};
use theme::ActiveTheme;
use ui::button::{Button, ButtonVariants};
use ui::dock_area::panel::{Panel, PanelEvent};
use ui::dock_area::ClosePanel;
use ui::input::{InputEvent, InputState, TextInput};
use ui::notification::Notification;
use ui::{v_flex, Disableable, StyledExt, WindowExtension};

pub fn init(window: &mut Window, cx: &mut App) -> Entity<ImportPanel> {
    cx.new(|cx| ImportPanel::new(window, cx))
}

#[derive(Debug)]
pub struct ImportPanel {
    name: SharedString,
    focus_handle: FocusHandle,

    /// Secret key input
    key_input: Entity<InputState>,

    /// Password input (if required)
    pass_input: Entity<InputState>,

    /// Error message
    error: Entity<Option<SharedString>>,

    /// Countdown timer for nostr connect
    countdown: Entity<Option<u64>>,

    /// Whether the user is currently logging in
    logging_in: bool,

    /// Event subscriptions
    _subscriptions: SmallVec<[Subscription; 1]>,
}

impl ImportPanel {
    fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let key_input = cx.new(|cx| InputState::new(window, cx).masked(true));
        let pass_input = cx.new(|cx| InputState::new(window, cx).masked(true));

        let error = cx.new(|_| None);
        let countdown = cx.new(|_| None);

        let mut subscriptions = smallvec![];

        subscriptions.push(
            // Subscribe to key input events and process login when the user presses enter
            cx.subscribe_in(&key_input, window, |this, _input, event, window, cx| {
                if let InputEvent::PressEnter { .. } = event {
                    this.login(window, cx);
                };
            }),
        );

        Self {
            key_input,
            pass_input,
            error,
            countdown,
            name: "Import".into(),
            focus_handle: cx.focus_handle(),
            logging_in: false,
            _subscriptions: subscriptions,
        }
    }

    fn login(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.logging_in {
            return;
        };
        // Prevent duplicate login requests
        self.set_logging_in(true, cx);

        let value = self.key_input.read(cx).value();
        let password = self.pass_input.read(cx).value();

        if value.starts_with("bunker://") {
            self.login_with_bunker(&value, window, cx);
            return;
        }

        if value.starts_with("ncryptsec1") {
            self.login_with_password(&value, &password, window, cx);
            return;
        }

        if let Ok(secret) = SecretKey::parse(&value) {
            let keys = Keys::new(secret);
            let nostr = NostrRegistry::global(cx);
            // Update the signer
            nostr.update(cx, |this, cx| {
                this.set_signer(keys, true, cx);
            });
            // Close the current panel after setting the signer
            window.dispatch_action(Box::new(ClosePanel), cx);
        } else {
            self.set_error("Invalid", cx);
        }
    }

    fn login_with_bunker(&mut self, content: &str, window: &mut Window, cx: &mut Context<Self>) {
        let Ok(uri) = NostrConnectUri::parse(content) else {
            self.set_error("Bunker is not valid", cx);
            return;
        };

        let nostr = NostrRegistry::global(cx);
        let weak_state = nostr.downgrade();

        let app_keys = nostr.read(cx).app_keys();
        let timeout = Duration::from_secs(30);
        let mut signer = NostrConnect::new(uri, app_keys.clone(), timeout, None).unwrap();

        // Handle auth url with the default browser
        signer.auth_url_handler(CoopAuthUrlHandler);

        // Start countdown
        cx.spawn_in(window, async move |this, cx| {
            for i in (0..=30).rev() {
                if i == 0 {
                    this.update(cx, |this, cx| {
                        this.set_countdown(None, cx);
                    })
                    .ok();
                } else {
                    this.update(cx, |this, cx| {
                        this.set_countdown(Some(i), cx);
                    })
                    .ok();
                }
                cx.background_executor().timer(Duration::from_secs(1)).await;
            }
        })
        .detach();

        // Handle connection
        cx.spawn_in(window, async move |_this, cx| {
            let result = signer.bunker_uri().await;

            weak_state
                .update_in(cx, |this, window, cx| {
                    match result {
                        Ok(uri) => {
                            this.persist_bunker(uri, cx);
                            this.set_signer(signer, true, cx);
                            // Close the current panel after setting the signer
                            window.dispatch_action(Box::new(ClosePanel), cx);
                        }
                        Err(e) => {
                            window.push_notification(Notification::error(e.to_string()), cx);
                        }
                    };
                })
                .ok();
        })
        .detach();
    }

    pub fn login_with_password(
        &mut self,
        content: &str,
        pwd: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if pwd.is_empty() {
            self.set_error("Password is required", cx);
            return;
        }

        let Ok(enc) = EncryptedSecretKey::from_bech32(content) else {
            self.set_error("Secret Key is invalid", cx);
            return;
        };

        let password = pwd.to_owned();

        // Decrypt in the background to ensure it doesn't block the UI
        let task = cx.background_spawn(async move {
            if let Ok(content) = enc.decrypt(&password) {
                Ok(Keys::new(content))
            } else {
                Err(anyhow!("Invalid password"))
            }
        });

        cx.spawn_in(window, async move |this, cx| {
            let result = task.await;

            this.update_in(cx, |this, window, cx| {
                match result {
                    Ok(keys) => {
                        let nostr = NostrRegistry::global(cx);
                        // Update the signer
                        nostr.update(cx, |this, cx| {
                            this.set_signer(keys, true, cx);
                        });
                        // Close the current panel after setting the signer
                        window.dispatch_action(Box::new(ClosePanel), cx);
                    }
                    Err(e) => {
                        this.set_error(e.to_string(), cx);
                    }
                };
            })
            .ok();
        })
        .detach();
    }

    fn set_error<S>(&mut self, message: S, cx: &mut Context<Self>)
    where
        S: Into<SharedString>,
    {
        // Reset the log in state
        self.set_logging_in(false, cx);

        // Reset the countdown
        self.set_countdown(None, cx);

        // Update error message
        self.error.update(cx, |this, cx| {
            *this = Some(message.into());
            cx.notify();
        });

        // Clear the error message after 3 secs
        cx.spawn(async move |this, cx| {
            cx.background_executor().timer(Duration::from_secs(3)).await;

            this.update(cx, |this, cx| {
                this.error.update(cx, |this, cx| {
                    *this = None;
                    cx.notify();
                });
            })
            .ok();
        })
        .detach();
    }

    fn set_logging_in(&mut self, status: bool, cx: &mut Context<Self>) {
        self.logging_in = status;
        cx.notify();
    }

    fn set_countdown(&mut self, i: Option<u64>, cx: &mut Context<Self>) {
        self.countdown.update(cx, |this, cx| {
            *this = i;
            cx.notify();
        });
    }
}

impl Panel for ImportPanel {
    fn panel_id(&self) -> SharedString {
        self.name.clone()
    }

    fn title(&self, _cx: &App) -> AnyElement {
        self.name.clone().into_any_element()
    }
}

impl EventEmitter<PanelEvent> for ImportPanel {}

impl Focusable for ImportPanel {
    fn focus_handle(&self, _: &App) -> gpui::FocusHandle {
        self.focus_handle.clone()
    }
}

impl Render for ImportPanel {
    fn render(&mut self, _window: &mut gpui::Window, cx: &mut Context<Self>) -> impl IntoElement {
        const SECRET_WARN: &str = "* Coop doesn't store your secret key. \
            It will be cleared when you close the app. \
            To persist your identity, please connect via Nostr Connect.";

        v_flex()
            .size_full()
            .items_center()
            .justify_center()
            .p_2()
            .gap_10()
            .child(
                div()
                    .text_center()
                    .font_semibold()
                    .line_height(relative(1.25))
                    .child(SharedString::from("Import a Secret Key or Bunker")),
            )
            .child(
                v_flex()
                    .w_112()
                    .gap_2()
                    .text_sm()
                    .child(
                        v_flex()
                            .gap_1()
                            .text_sm()
                            .text_color(cx.theme().text_muted)
                            .child("nsec or bunker://")
                            .child(TextInput::new(&self.key_input)),
                    )
                    .when(
                        self.key_input.read(cx).value().starts_with("ncryptsec1"),
                        |this| {
                            this.child(
                                v_flex()
                                    .gap_1()
                                    .text_sm()
                                    .text_color(cx.theme().text_muted)
                                    .child("Password:")
                                    .child(TextInput::new(&self.pass_input)),
                            )
                        },
                    )
                    .child(
                        Button::new("login")
                            .label("Continue")
                            .primary()
                            .loading(self.logging_in)
                            .disabled(self.logging_in)
                            .on_click(cx.listener(move |this, _, window, cx| {
                                this.login(window, cx);
                            })),
                    )
                    .when_some(self.countdown.read(cx).as_ref(), |this, i| {
                        this.child(
                            div()
                                .text_xs()
                                .text_center()
                                .text_color(cx.theme().text_muted)
                                .child(SharedString::from(format!(
                                    "Approve connection request from your signer in {} seconds",
                                    i
                                ))),
                        )
                    })
                    .when_some(self.error.read(cx).as_ref(), |this, error| {
                        this.child(
                            div()
                                .text_xs()
                                .text_center()
                                .text_color(cx.theme().danger_foreground)
                                .child(error.clone()),
                        )
                    })
                    .child(
                        div()
                            .mt_2()
                            .italic()
                            .text_xs()
                            .text_color(cx.theme().text_muted)
                            .child(SharedString::from(SECRET_WARN)),
                    ),
            )
    }
}
