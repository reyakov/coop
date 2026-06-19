use std::time::Duration;

use anyhow::{Error, anyhow};
use gpui::prelude::FluentBuilder;
use gpui::{
    AppContext, Context, Entity, IntoElement, ParentElement, Render, SharedString, Styled,
    Subscription, Task, Window, div,
};
use nostr_connect::prelude::*;
use state::NostrRegistry;
use theme::ActiveTheme;
use ui::button::{Button, ButtonVariants};
use ui::input::{Input, InputEvent, InputState};
use ui::{Disableable, WindowExtension, v_flex};

#[derive(Debug)]
pub struct ImportIdentity {
    /// Secret key input
    key_input: Entity<InputState>,

    /// Password input (if required)
    pass_input: Entity<InputState>,

    /// Error message
    error: Entity<Option<SharedString>>,

    /// Whether the user is currently loading
    loading: bool,

    /// Async tasks
    tasks: Vec<Task<Result<(), Error>>>,

    /// Input subscription
    _subscription: Option<Subscription>,
}

impl ImportIdentity {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let key_input = cx.new(|cx| InputState::new(window, cx).masked(true));
        let pass_input = cx.new(|cx| InputState::new(window, cx).masked(true));
        let error = cx.new(|_| None);

        let input_subscription =
            cx.subscribe_in(&key_input, window, |this, _input, event, window, cx| {
                if let InputEvent::PressEnter { .. } = event {
                    this.login(window, cx);
                };
            });

        Self {
            key_input,
            pass_input,
            error,
            loading: false,
            tasks: vec![],
            _subscription: Some(input_subscription),
        }
    }

    fn login(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let value = self.key_input.read(cx).value();
        let password = self.pass_input.read(cx).value();

        if value.starts_with("ncryptsec1") {
            self.ncryptsec(value, password, window, cx);
            return;
        }

        if let Ok(secret) = SecretKey::parse(&value) {
            let keys = Keys::new(secret);
            let nostr = NostrRegistry::global(cx);

            // Update the signer
            nostr.update(cx, |this, cx| {
                this.set_signer(keys, cx);
            });
            window.close_modal(cx);
        } else {
            self.set_error("Invalid key", cx);
        }
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
                    nostr.update_in(cx, |this, window, cx| {
                        this.set_signer(keys, cx);
                        window.close_modal(cx);
                    })?;
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
}

impl Render for ImportIdentity {
    fn render(&mut self, _window: &mut gpui::Window, cx: &mut Context<Self>) -> impl IntoElement {
        const MSG: &str = "Coop isn't stored your identity secret in local device. Everything will be reset on the next login.";

        v_flex()
            .size_full()
            .gap_2()
            .text_sm()
            .child(
                v_flex()
                    .gap_1()
                    .text_sm()
                    .text_color(cx.theme().text_muted)
                    .child("nsec or ncryptsec://")
                    .child(Input::new(&self.key_input)),
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
                            .child(Input::new(&self.pass_input)),
                    )
                },
            )
            .child(div().text_xs().text_color(cx.theme().text_muted).child(MSG))
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
            .when_some(self.error.read(cx).as_ref(), |this, error| {
                this.child(
                    div()
                        .text_xs()
                        .text_center()
                        .text_color(cx.theme().text_danger)
                        .child(error.clone()),
                )
            })
    }
}
