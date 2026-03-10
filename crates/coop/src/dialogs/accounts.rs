use anyhow::Error;
use gpui::prelude::FluentBuilder;
use gpui::{
    App, AppContext, Context, Entity, InteractiveElement, IntoElement, ParentElement, Render,
    SharedString, StatefulInteractiveElement, Styled, Subscription, Task, Window, div, px,
};
use nostr_sdk::prelude::*;
use person::PersonRegistry;
use state::{NostrRegistry, StateEvent};
use theme::ActiveTheme;
use ui::avatar::Avatar;
use ui::button::{Button, ButtonVariants};
use ui::indicator::Indicator;
use ui::{Disableable, Icon, IconName, Sizable, WindowExtension, h_flex, v_flex};

use crate::dialogs::connect::ConnectSigner;
use crate::dialogs::import::ImportKey;

pub fn init(window: &mut Window, cx: &mut App) -> Entity<AccountSelector> {
    cx.new(|cx| AccountSelector::new(window, cx))
}

/// Account selector
pub struct AccountSelector {
    /// Public key currently being chosen for login
    logging_in: Entity<Option<PublicKey>>,

    /// The error message displayed when an error occurs.
    error: Entity<Option<SharedString>>,

    /// Async tasks
    tasks: Vec<Task<Result<(), Error>>>,

    /// Subscription to the signer events
    _subscription: Option<Subscription>,
}

impl AccountSelector {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let logging_in = cx.new(|_| None);
        let error = cx.new(|_| None);

        // Subscribe to the signer events
        let nostr = NostrRegistry::global(cx);
        let subscription = cx.subscribe_in(&nostr, window, |this, _state, event, window, cx| {
            match event {
                StateEvent::SignerSet => {
                    window.close_all_modals(cx);
                    window.refresh();
                }
                StateEvent::Error(e) => {
                    this.set_error(e.to_string(), cx);
                }
                _ => {}
            };
        });

        Self {
            logging_in,
            error,
            tasks: vec![],
            _subscription: Some(subscription),
        }
    }

    fn logging_in(&self, public_key: &PublicKey, cx: &App) -> bool {
        self.logging_in.read(cx) == &Some(*public_key)
    }

    fn set_logging_in(&mut self, public_key: PublicKey, cx: &mut Context<Self>) {
        self.logging_in.update(cx, |this, cx| {
            *this = Some(public_key);
            cx.notify();
        });
    }

    fn set_error<T>(&mut self, error: T, cx: &mut Context<Self>)
    where
        T: Into<SharedString>,
    {
        self.error.update(cx, |this, cx| {
            *this = Some(error.into());
            cx.notify();
        });

        self.logging_in.update(cx, |this, cx| {
            *this = None;
            cx.notify();
        })
    }

    fn login(&mut self, public_key: PublicKey, window: &mut Window, cx: &mut Context<Self>) {
        let nostr = NostrRegistry::global(cx);
        let task = nostr.read(cx).get_signer(&public_key, cx);

        // Mark the public key as being logged in
        self.set_logging_in(public_key, cx);

        self.tasks.push(cx.spawn_in(window, async move |this, cx| {
            match task.await {
                Ok(signer) => {
                    nostr.update(cx, |this, cx| {
                        this.set_signer(signer, cx);
                    });
                }
                Err(e) => {
                    this.update(cx, |this, cx| {
                        this.set_error(e.to_string(), cx);
                    })?;
                }
            };
            Ok(())
        }));
    }

    fn remove(&mut self, public_key: PublicKey, cx: &mut Context<Self>) {
        let nostr = NostrRegistry::global(cx);

        nostr.update(cx, |this, cx| {
            this.remove_signer(&public_key, cx);
        });
    }

    fn open_import(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let import = cx.new(|cx| ImportKey::new(window, cx));

        window.open_modal(cx, move |this, _window, _cx| {
            this.width(px(460.))
                .title("Import a Secret Key or Bunker Connection")
                .show_close(true)
                .pb_2()
                .child(import.clone())
        });
    }

    fn open_connect(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let connect = cx.new(|cx| ConnectSigner::new(window, cx));

        window.open_modal(cx, move |this, _window, _cx| {
            this.width(px(460.))
                .title("Scan QR Code to Connect")
                .show_close(true)
                .pb_2()
                .child(connect.clone())
        });
    }
}

impl Render for AccountSelector {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let persons = PersonRegistry::global(cx);
        let nostr = NostrRegistry::global(cx);
        let npubs = nostr.read(cx).npubs();
        let loading = self.logging_in.read(cx).is_some();

        v_flex()
            .size_full()
            .gap_2()
            .when_some(self.error.read(cx).as_ref(), |this, error| {
                this.child(
                    div()
                        .italic()
                        .text_xs()
                        .text_center()
                        .text_color(cx.theme().danger_active)
                        .child(error.clone()),
                )
            })
            .children({
                let mut items = vec![];

                for (ix, public_key) in npubs.read(cx).iter().enumerate() {
                    let profile = persons.read(cx).get(public_key, cx);
                    let logging_in = self.logging_in(public_key, cx);

                    items.push(
                        h_flex()
                            .id(ix)
                            .group("")
                            .px_2()
                            .h_10()
                            .justify_between()
                            .w_full()
                            .rounded(cx.theme().radius)
                            .bg(cx.theme().ghost_element_background)
                            .hover(|this| this.bg(cx.theme().ghost_element_hover))
                            .child(
                                h_flex()
                                    .gap_2()
                                    .child(Avatar::new(profile.avatar()).small())
                                    .child(div().text_sm().child(profile.name())),
                            )
                            .when(logging_in, |this| this.child(Indicator::new().small()))
                            .when(!logging_in, |this| {
                                this.child(
                                    h_flex()
                                        .gap_1()
                                        .invisible()
                                        .group_hover("", |this| this.visible())
                                        .child(
                                            Button::new(format!("del-{ix}"))
                                                .icon(IconName::Close)
                                                .ghost()
                                                .small()
                                                .disabled(logging_in)
                                                .on_click(cx.listener({
                                                    let public_key = *public_key;
                                                    move |this, _ev, _window, cx| {
                                                        cx.stop_propagation();
                                                        this.remove(public_key, cx);
                                                    }
                                                })),
                                        ),
                                )
                            })
                            .when(!logging_in, |this| {
                                let public_key = *public_key;
                                this.on_click(cx.listener(move |this, _ev, window, cx| {
                                    this.login(public_key, window, cx);
                                }))
                            }),
                    );
                }

                items
            })
            .child(div().w_full().h_px().bg(cx.theme().border_variant))
            .child(
                h_flex()
                    .gap_1()
                    .justify_end()
                    .w_full()
                    .child(
                        Button::new("input")
                            .icon(Icon::new(IconName::Usb))
                            .label("Import")
                            .ghost()
                            .small()
                            .disabled(loading)
                            .on_click(cx.listener(move |this, _ev, window, cx| {
                                this.open_import(window, cx);
                            })),
                    )
                    .child(
                        Button::new("qr")
                            .icon(Icon::new(IconName::Scan))
                            .label("Scan QR to connect")
                            .ghost()
                            .small()
                            .disabled(loading)
                            .on_click(cx.listener(move |this, _ev, window, cx| {
                                this.open_connect(window, cx);
                            })),
                    ),
            )
    }
}
