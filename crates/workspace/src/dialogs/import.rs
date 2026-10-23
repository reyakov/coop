use anyhow::Error;
use gpui::prelude::FluentBuilder;
use gpui::{
    App, AppContext, Context, Entity, IntoElement, ParentElement, Render, SharedString, Styled,
    Subscription, Task, Window, div, px, relative,
};
use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::component::{ActiveTheme, Disableable, WindowExt, v_flex};
use instant::Duration;
use nostr_connect::prelude::*;
use state::NostrRegistry;

pub fn open(window: &mut Window, cx: &mut App) {
    let import = cx.new(|cx| ImportIdentity::new(window, cx));

    window.open_dialog(cx, move |this, _window, _cx| {
        this.width(px(450.))
            .close_button(false)
            .overlay_closable(false)
            .keyboard(false)
            .title("Continue with your Nostr Identity")
            .child(import.clone())
    });
}

/// Open a passphrase-only dialog to unlock the stored encrypted key
pub fn open_unlock(window: &mut Window, cx: &mut App) {
    let import = cx.new(|cx| ImportIdentity::new_unlock(window, cx));

    window.open_dialog(cx, move |this, _window, _cx| {
        this.width(px(450.))
            .close_button(false)
            .overlay_closable(false)
            .keyboard(false)
            .title("Unlock your identity")
            .child(import.clone())
    });
}

#[derive(Debug, PartialEq, Eq)]
enum Mode {
    /// Import a credential (nsec, ncryptsec or bunker)
    Import,
    /// Unlock the stored encrypted key with its passphrase
    Unlock,
}

#[derive(Debug)]
pub struct ImportIdentity {
    mode: Mode,
    /// Secret key input
    key_input: Entity<InputState>,
    /// Passphrase input
    pass_input: Entity<InputState>,
    /// Repeated passphrase input when encrypting a plaintext key
    repass_input: Entity<InputState>,
    /// Error message
    error: Entity<Option<SharedString>>,
    /// Whether the user is currently loading
    loading: bool,
    tasks: Vec<Task<Result<(), Error>>>,
    _subscriptions: Vec<Subscription>,
}

impl ImportIdentity {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        Self::build(Mode::Import, window, cx)
    }

    pub fn new_unlock(window: &mut Window, cx: &mut Context<Self>) -> Self {
        Self::build(Mode::Unlock, window, cx)
    }

    fn build(mode: Mode, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let key_input = cx.new(|cx| InputState::new(window, cx).placeholder("secret or bunker://"));
        let pass_input = cx.new(|cx| InputState::new(window, cx).masked(true));
        let repass_input = cx.new(|cx| InputState::new(window, cx).masked(true));
        let error = cx.new(|_| None);

        let subscriptions = vec![
            cx.subscribe_in(&key_input, window, |this, _input, event, _window, cx| {
                if let InputEvent::PressEnter { .. } = event {
                    this.login(cx);
                };
            }),
            cx.subscribe_in(&pass_input, window, |this, _input, event, _window, cx| {
                if let InputEvent::PressEnter { .. } = event {
                    this.login(cx);
                };
            }),
            cx.subscribe_in(&repass_input, window, |this, _input, event, _window, cx| {
                if let InputEvent::PressEnter { .. } = event {
                    this.login(cx);
                };
            }),
        ];

        Self {
            mode,
            key_input,
            pass_input,
            repass_input,
            error,
            loading: false,
            tasks: vec![],
            _subscriptions: subscriptions,
        }
    }

    fn login(&mut self, cx: &mut Context<Self>) {
        let password = self.pass_input.read(cx).value().to_string();
        let nostr = NostrRegistry::global(cx);

        if self.mode == Mode::Unlock {
            if password.is_empty() {
                self.set_error("Passphrase is required", cx);
                return;
            }

            self.set_loading(true, cx);
            let task = nostr.update(cx, |this, cx| this.restore_with_passphrase(&password, cx));
            self.finish(task, cx);
            return;
        }

        let value = self.key_input.read(cx).value().trim().to_string();
        self.set_loading(true, cx);

        if value.starts_with("bunker://") {
            let task = nostr.update(cx, |this, cx| this.import_bunker(&value, cx));
            self.finish(task, cx);
            return;
        }

        if value.starts_with("ncryptsec1") {
            if password.is_empty() {
                self.set_error("Passphrase is required", cx);
                return;
            }

            let task = nostr.update(cx, |this, cx| this.import_ncryptsec(&value, &password, cx));
            self.finish(task, cx);
            return;
        }

        // A plaintext key is encrypted with the passphrase before it is stored
        if value.starts_with("nsec1") || SecretKey::parse(&value).is_ok() {
            if password.is_empty() {
                self.set_error("Passphrase is required", cx);
                return;
            }

            let repass = self.repass_input.read(cx).value().to_string();

            if password != repass {
                self.set_error("Passphrases do not match", cx);
                return;
            }

            let task = nostr.update(cx, |this, cx| this.import_nsec(&value, &password, cx));
            self.finish(task, cx);
            return;
        }

        self.set_error("Enter an nsec, ncryptsec or bunker://", cx);
    }

    /// Run the import task, surfacing errors while success closes the dialog
    /// through the `SignerChanged` event
    fn finish(&mut self, task: Task<Result<PublicKey, Error>>, cx: &mut Context<Self>) {
        self.tasks.push(cx.spawn(async move |this, cx| {
            if let Err(e) = task.await {
                this.update(cx, |this, cx| {
                    this.set_error(e.to_string(), cx);
                })?;
            }

            Ok(())
        }));
    }

    #[cfg(not(target_arch = "wasm32"))]
    fn proxy(&mut self, cx: &mut Context<Self>) {
        let nostr = NostrRegistry::global(cx);
        nostr.update(cx, |this, cx| {
            this.connect_proxy(cx);
        });
    }

    fn set_loading(&mut self, status: bool, cx: &mut Context<Self>) {
        self.loading = status;
        cx.notify();
    }

    fn set_error<S>(&mut self, message: S, cx: &mut Context<Self>)
    where
        S: Into<SharedString>,
    {
        self.set_loading(false, cx);

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
        const BUNKER_WARN: &str = "Nostr Connect will usually take more time to get all your messages. Please keep your session open until you see all your messages.";
        const KEY_WARN: &str = "Coop encrypts your key with your passphrase and stores it on this device. You only need the passphrase to sign in next time.";

        let is_wasm = cfg!(target_arch = "wasm32");
        let is_unlock = self.mode == Mode::Unlock;

        let value = self.key_input.read(cx).value().trim().to_string();
        let require_password = value.starts_with("ncryptsec1");
        let require_encrypt = value.starts_with("nsec1");
        let key_warning = require_password || require_encrypt;
        let bunker_warning = value.starts_with("bunker://");

        v_flex()
            .size_full()
            .gap_4()
            .text_xs()
            .child(
                v_flex()
                    .gap_2()
                    .when(!is_unlock, |this| {
                        this.child(
                            v_flex()
                                .gap_1()
                                .text_color(cx.theme().muted_foreground)
                                .child("Use secret key or bunker connection")
                                .child(Input::new(&self.key_input)),
                        )
                    })
                    .when(is_unlock || require_password, |this| {
                        this.child(
                            v_flex()
                                .gap_1()
                                .text_color(cx.theme().muted_foreground)
                                .child(if is_unlock {
                                    "Passphrase to unlock your identity:"
                                } else {
                                    "Decrypt Password:"
                                })
                                .child(Input::new(&self.pass_input)),
                        )
                    })
                    .when(require_encrypt, |this| {
                        this.child(
                            v_flex()
                                .gap_1()
                                .text_color(cx.theme().muted_foreground)
                                .child("Passphrase to encrypt your key on this device:")
                                .child(Input::new(&self.pass_input)),
                        )
                        .child(
                            v_flex()
                                .gap_1()
                                .text_color(cx.theme().muted_foreground)
                                .child("Repeat Passphrase:")
                                .child(Input::new(&self.repass_input)),
                        )
                    })
                    .when(bunker_warning, |this| {
                        this.child(
                            div()
                                .p_2()
                                .rounded(cx.theme().radius_lg)
                                .bg(cx.theme().warning.opacity(0.15))
                                .text_xs()
                                .text_color(cx.theme().warning)
                                .line_height(relative(1.2))
                                .child(div().child(BUNKER_WARN)),
                        )
                    })
                    .when(key_warning, |this| {
                        this.child(
                            div()
                                .p_2()
                                .rounded(cx.theme().radius_lg)
                                .bg(cx.theme().warning.opacity(0.15))
                                .text_xs()
                                .text_color(cx.theme().warning)
                                .line_height(relative(1.2))
                                .child(div().child(KEY_WARN)),
                        )
                    }),
            )
            .child(
                Button::new("login")
                    .label(if is_unlock { "Unlock" } else { "Continue" })
                    .primary()
                    .loading(self.loading)
                    .disabled(self.loading)
                    .on_click(cx.listener(move |this, _ev, _window, cx| {
                        this.login(cx);
                    })),
            )
            .when(!is_wasm && !is_unlock, |this| {
                this.child(
                    Button::new("proxy")
                        .label("Connect via Web Extension (Experimental)")
                        .ghost()
                        .disabled(self.loading)
                        .on_click(cx.listener(move |this, _ev, _window, cx| {
                            this.proxy(cx);
                        })),
                )
            })
            .when_some(self.error.read(cx).as_ref(), |this, error| {
                this.child(
                    div()
                        .text_xs()
                        .text_center()
                        .text_color(cx.theme().danger)
                        .child(error.clone()),
                )
            })
    }
}
