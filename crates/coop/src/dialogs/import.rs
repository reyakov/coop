use std::time::Duration;

use anyhow::{anyhow, Error};
use gpui::prelude::FluentBuilder;
use gpui::{
    div, AppContext, Context, Entity, IntoElement, ParentElement, Render, SharedString, Styled,
    Subscription, Task, Window,
};
use nostr_connect::prelude::*;
use smallvec::{smallvec, SmallVec};
use state::{CoopAuthUrlHandler, NostrRegistry, SignerEvent};
use theme::ActiveTheme;
use ui::button::{Button, ButtonVariants};
use ui::input::{InputEvent, InputState, TextInput};
use ui::{v_flex, Disableable};

#[derive(Debug)]
pub struct ImportKey {
    /// Secret key input
    key_input: Entity<InputState>,

    /// Password input (if required)
    pass_input: Entity<InputState>,

    /// Error message
    error: Entity<Option<SharedString>>,

    /// Countdown timer for nostr connect
    countdown: Entity<Option<u64>>,

    /// Whether the user is currently loading
    loading: bool,

    /// Async tasks
    tasks: Vec<Task<Result<(), Error>>>,

    /// Event subscriptions
    _subscriptions: SmallVec<[Subscription; 2]>,
}

impl ImportKey {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let nostr = NostrRegistry::global(cx);
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

        subscriptions.push(
            // Subscribe to the nostr signer event
            cx.subscribe_in(&nostr, window, |this, _state, event, _window, cx| {
                if let SignerEvent::Error(e) = event {
                    this.set_error(e, cx);
                }
            }),
        );

        Self {
            key_input,
            pass_input,
            error,
            countdown,
            loading: false,
            tasks: vec![],
            _subscriptions: subscriptions,
        }
    }

    fn login(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.loading {
            return;
        };
        // Prevent duplicate login requests
        self.set_loading(true, cx);

        let value = self.key_input.read(cx).value();
        let password = self.pass_input.read(cx).value();

        if value.starts_with("bunker://") {
            self.bunker(&value, window, cx);
            return;
        }

        if value.starts_with("ncryptsec1") {
            self.ncryptsec(value, password, window, cx);
            return;
        }

        if let Ok(secret) = SecretKey::parse(&value) {
            let keys = Keys::new(secret);
            let nostr = NostrRegistry::global(cx);

            // Update the signer
            nostr.update(cx, |this, cx| {
                this.add_key_signer(&keys, cx);
            });
        } else {
            self.set_error("Invalid key", cx);
        }
    }

    fn bunker(&mut self, content: &str, window: &mut Window, cx: &mut Context<Self>) {
        let Ok(uri) = NostrConnectUri::parse(content) else {
            self.set_error("Bunker is not valid", cx);
            return;
        };

        let nostr = NostrRegistry::global(cx);
        let app_keys = nostr.read(cx).app_keys.clone();
        let timeout = Duration::from_secs(30);

        // Construct the nostr connect signer
        let mut signer = NostrConnect::new(uri, app_keys.clone(), timeout, None).unwrap();

        // Handle auth url with the default browser
        signer.auth_url_handler(CoopAuthUrlHandler);

        // Set signer in the background
        nostr.update(cx, |this, cx| {
            this.add_nip46_signer(&signer, cx);
        });

        // Start countdown
        self.tasks.push(cx.spawn_in(window, async move |this, cx| {
            for i in (0..=30).rev() {
                if i == 0 {
                    this.update(cx, |this, cx| {
                        this.set_countdown(None, cx);
                    })?;
                } else {
                    this.update(cx, |this, cx| {
                        this.set_countdown(Some(i), cx);
                    })?;
                }
                cx.background_executor().timer(Duration::from_secs(1)).await;
            }
            Ok(())
        }));
    }

    fn ncryptsec<S>(&mut self, content: S, pwd: S, window: &mut Window, cx: &mut Context<Self>)
    where
        S: Into<String>,
    {
        let nostr = NostrRegistry::global(cx);
        let content: String = content.into();
        let password: String = pwd.into();

        if password.is_empty() {
            self.set_error("Password is required", cx);
            return;
        }

        let Ok(enc) = EncryptedSecretKey::from_bech32(&content) else {
            self.set_error("Secret Key is invalid", cx);
            return;
        };

        // Decrypt in the background to ensure it doesn't block the UI
        let task = cx.background_spawn(async move {
            if let Ok(content) = enc.decrypt(&password) {
                Ok(Keys::new(content))
            } else {
                Err(anyhow!("Invalid password"))
            }
        });

        self.tasks.push(cx.spawn_in(window, async move |this, cx| {
            match task.await {
                Ok(keys) => {
                    nostr.update(cx, |this, cx| {
                        this.add_key_signer(&keys, cx);
                    });
                }
                Err(e) => {
                    this.update(cx, |this, cx| {
                        this.set_error(e.to_string(), cx);
                    })?;
                }
            }

            Ok(())
        }));
    }

    fn set_error<S>(&mut self, message: S, cx: &mut Context<Self>)
    where
        S: Into<SharedString>,
    {
        // Reset the log in state
        self.set_loading(false, cx);

        // Reset the countdown
        self.set_countdown(None, cx);

        // Update error message
        self.error.update(cx, |this, cx| {
            *this = Some(message.into());
            cx.notify();
        });

        // Clear the error message after 3 secs
        self.tasks.push(cx.spawn(async move |this, cx| {
            cx.background_executor().timer(Duration::from_secs(3)).await;

            this.update(cx, |this, cx| {
                this.error.update(cx, |this, cx| {
                    *this = None;
                    cx.notify();
                });
            })?;

            Ok(())
        }));
    }

    fn set_loading(&mut self, status: bool, cx: &mut Context<Self>) {
        self.loading = status;
        cx.notify();
    }

    fn set_countdown(&mut self, i: Option<u64>, cx: &mut Context<Self>) {
        self.countdown.update(cx, |this, cx| {
            *this = i;
            cx.notify();
        });
    }
}

impl Render for ImportKey {
    fn render(&mut self, _window: &mut gpui::Window, cx: &mut Context<Self>) -> impl IntoElement {
        v_flex()
            .size_full()
            .p_4()
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
                    .loading(self.loading)
                    .disabled(self.loading)
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
                        .text_color(cx.theme().danger_active)
                        .child(error.clone()),
                )
            })
    }
}
