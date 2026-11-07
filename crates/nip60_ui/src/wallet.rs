use std::rc::Rc;

use ::nip60::{HistoryEntry, WalletEvent, WalletRegistry};
use common::TimestampExt;
use gpui::prelude::FluentBuilder;
use gpui::{
    AnyElement, App, AppContext, ClickEvent, Context, Entity, EventEmitter, FocusHandle, Focusable,
    IntoElement, ParentElement, Render, SharedString, Styled, Subscription, Window, div, px,
    uniform_list,
};
use gpui_kit::assets::IconName;
use gpui_kit::base::StyledExt as _;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::clipboard::Clipboard;
use gpui_kit::component::dock::{Panel, PanelEvent};
use gpui_kit::component::input::{Input, InputState};
use gpui_kit::component::notification::Notification;
use gpui_kit::component::separator::Separator;
use gpui_kit::component::{ActiveTheme, Icon, Sizable, WindowExt, h_flex, v_flex};
use nostr_sdk::prelude::*;
use smallvec::SmallVec;

pub fn init(window: &mut Window, cx: &mut App) -> Entity<WalletPanel> {
    cx.new(|cx| WalletPanel::new(window, cx))
}

type SubmitFn = Rc<dyn Fn(&ClickEvent, &mut Window, &mut App) -> bool>;

pub struct WalletPanel {
    name: SharedString,
    focus_handle: FocusHandle,
    /// The token produced by the last send, shown for copying
    sent_token: Option<SharedString>,
    _subscriptions: SmallVec<[Subscription; 1]>,
}

impl WalletPanel {
    fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let wallet = WalletRegistry::global(cx);
        let mut subscriptions = SmallVec::new();
        subscriptions.push(
            cx.subscribe_in(
                &wallet,
                window,
                |this, _wallet, event, window, cx| match event {
                    WalletEvent::Updated => cx.notify(),
                    WalletEvent::Error(error) => {
                        window.push_notification(Notification::error(error.clone()), cx);
                    }
                    WalletEvent::TokenCreated(token) => {
                        this.sent_token = Some(token.clone().into());
                        cx.notify();
                    }
                },
            ),
        );

        Self {
            name: "Wallet".into(),
            focus_handle: cx.focus_handle(),
            sent_token: None,
            _subscriptions: subscriptions,
        }
    }

    fn render_balance(&self, cx: &Context<Self>) -> impl IntoElement {
        let balance = WalletRegistry::global(cx).read(cx).balance();

        v_flex()
            .items_center()
            .gap_1()
            .child(
                div()
                    .text_xl()
                    .child(SharedString::from(balance.to_string())),
            )
            .child(
                div()
                    .text_xs()
                    .text_color(cx.theme().muted_foreground)
                    .child("sats"),
            )
    }

    fn render_actions(&self) -> impl IntoElement {
        h_flex()
            .justify_center()
            .gap_2()
            .child(
                Button::new("send")
                    .label("Send")
                    .primary()
                    .on_click(|_, window, cx| open_send_dialog(window, cx)),
            )
            .child(
                Button::new("receive")
                    .label("Receive")
                    .on_click(|_, window, cx| open_receive_dialog(window, cx)),
            )
    }

    fn render_sent_token(&self, cx: &Context<Self>) -> impl IntoElement {
        v_flex()
            .gap_1p5()
            .w_full()
            .child(
                div()
                    .text_xs()
                    .font_semibold()
                    .text_color(cx.theme().muted_foreground)
                    .child("Sent token:"),
            )
            .when_some(self.sent_token.as_ref(), |this, token| {
                this.child(
                    h_flex()
                        .gap_1()
                        .w_full()
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .truncate()
                                .text_color(cx.theme().muted_foreground)
                                .child(token.clone()),
                        )
                        .child(
                            Clipboard::new("copy-token")
                                .accessibility_label("Copy cashu token")
                                .value_fn({
                                    let token = token.clone();
                                    move |_, _| SharedString::from(token.to_string())
                                }),
                        ),
                )
            })
    }

    fn render_history(
        &mut self,
        history: Vec<HistoryEntry>,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        if history.is_empty() {
            return div()
                .text_xs()
                .text_color(cx.theme().muted_foreground)
                .child("No transactions yet.")
                .into_any_element();
        }

        uniform_list(
            "wallet-history",
            history.len(),
            cx.processor(move |this, range, _window, cx| {
                this.render_history_rows(range, &history, cx)
            }),
        )
        .flex_1()
        .min_h_0()
        .into_any_element()
    }

    fn render_history_rows(
        &self,
        range: std::ops::Range<usize>,
        history: &[HistoryEntry],
        cx: &Context<Self>,
    ) -> Vec<AnyElement> {
        range
            .map(|ix| {
                self.render_history_entry(&history[ix], cx)
                    .into_any_element()
            })
            .collect()
    }

    fn render_history_entry(&self, entry: &HistoryEntry, cx: &Context<Self>) -> impl IntoElement {
        let (label, color) = match entry.direction {
            TransactionDirection::In => ("Received", cx.theme().success),
            TransactionDirection::Out => ("Sent", cx.theme().danger),
        };

        h_flex()
            .justify_between()
            .w_full()
            .py_0p5()
            .text_xs()
            .child(
                h_flex()
                    .gap_2()
                    .child(div().text_color(color).child(label))
                    .child(SharedString::from(format!("{} sats", entry.amount))),
            )
            .child(
                div()
                    .text_color(cx.theme().muted_foreground)
                    .child(entry.created_at.to_human_time()),
            )
    }
}

/// Ask for an amount and send ecash.
fn open_send_dialog(window: &mut Window, cx: &mut App) {
    let registry = WalletRegistry::global(cx);
    let receiver_input = cx.new(|cx| InputState::new(window, cx).placeholder("npub… (optional)"));
    let amount_input = cx.new(|cx| InputState::new(window, cx).placeholder("Amount (sats)"));

    let send: SubmitFn = {
        let receiver_input = receiver_input.clone();
        let amount_input = amount_input.clone();

        Rc::new(move |_, window, cx| {
            let Some(amount) = parse_amount(&amount_input, cx) else {
                window.push_notification(Notification::error("Enter an amount in sats"), cx);
                return false;
            };

            let receiver = receiver_input.read(cx).value().trim().to_string();

            let recipient = match receiver.as_str() {
                "" => None,
                value => match PublicKey::parse(value) {
                    Ok(recipient) => Some(recipient),
                    Err(_) => {
                        window.push_notification(
                            Notification::error("Receiver must be an npub or a public key"),
                            cx,
                        );
                        return false;
                    }
                },
            };

            match recipient {
                Some(recipient) => registry.update(cx, |registry, cx| {
                    registry.send_nutzap(recipient, amount, None, None, cx);
                }),
                None => registry.update(cx, |registry, cx| {
                    registry.send_token(amount, cx);
                }),
            }

            true
        })
    };

    window.open_dialog(cx, move |this, _window, _cx| {
        this.width(px(380.))
            .title("Send eCash")
            .on_ok({
                let send = send.clone();
                move |event, window, cx| send(event, window, cx)
            })
            .child(
                v_flex()
                    .gap_3()
                    .child(
                        v_flex()
                            .gap_1p5()
                            .child(
                                div()
                                    .text_xs()
                                    .font_semibold()
                                    .child("To (npub, optional):"),
                            )
                            .child(Input::new(&receiver_input).cleanable(true)),
                    )
                    .child(Input::new(&amount_input).cleanable(true))
                    .child(
                        h_flex()
                            .gap_1()
                            .justify_end()
                            .child(
                                Button::new("cancel")
                                    .label("Cancel")
                                    .ghost()
                                    .on_click(|_, window, cx| window.close_dialog(cx)),
                            )
                            .child(Button::new("send").label("Send").primary().on_click({
                                let send = send.clone();
                                move |event, window, cx| {
                                    if send(event, window, cx) {
                                        window.close_dialog(cx);
                                    }
                                }
                            })),
                    ),
            )
    });
}

/// Create a lightning invoice or redeem a pasted cashu token
fn open_receive_dialog(window: &mut Window, cx: &mut App) {
    let amount_input = cx.new(|cx| InputState::new(window, cx).placeholder("Amount (sats)"));
    let token_input = cx.new(|cx| InputState::new(window, cx).placeholder("cashu…"));

    let create_invoice: SubmitFn = {
        let input = amount_input.clone();
        Rc::new(move |_, window, cx| {
            let Some(amount) = parse_amount(&input, cx) else {
                window.push_notification(Notification::error("Enter an amount in sats"), cx);
                return false;
            };

            WalletRegistry::global(cx).update(cx, |registry, cx| {
                registry.create_mint_quote(amount, cx);
            });

            true
        })
    };

    let redeem: SubmitFn = {
        let input = token_input.clone();
        Rc::new(move |_, window, cx| {
            let token = input.read(cx).value().trim().to_string();
            if token.is_empty() {
                window.push_notification(Notification::error("Paste a cashu token"), cx);
                return false;
            }

            WalletRegistry::global(cx).update(cx, |registry, cx| {
                registry.receive_token(token, cx);
            });

            true
        })
    };

    window.open_dialog(cx, move |this, _window, _cx| {
        this.width(px(420.))
            .title("Receive")
            .on_ok({
                let token_input = token_input.clone();
                let create_invoice = create_invoice.clone();
                let redeem = redeem.clone();
                move |_event, window, cx| {
                    if !token_input.read(cx).value().trim().is_empty() {
                        redeem(_event, window, cx)
                    } else {
                        create_invoice(_event, window, cx)
                    }
                }
            })
            .child(
                v_flex()
                    .gap_3()
                    .child(
                        v_flex()
                            .gap_1p5()
                            .child(div().text_xs().font_semibold().child("Lightning invoice:"))
                            .child(
                                h_flex()
                                    .gap_1()
                                    .w_full()
                                    .child(Input::new(&amount_input).cleanable(true))
                                    .child(Button::new("invoice").label("Invoice").on_click({
                                        let create_invoice = create_invoice.clone();
                                        move |event, window, cx| {
                                            if create_invoice(event, window, cx) {
                                                window.close_dialog(cx);
                                            }
                                        }
                                    })),
                            ),
                    )
                    .child(Separator::horizontal())
                    .child(
                        v_flex()
                            .gap_1p5()
                            .child(div().text_xs().font_semibold().child("Cashu token:"))
                            .child(
                                h_flex()
                                    .gap_1()
                                    .w_full()
                                    .child(Input::new(&token_input).cleanable(true))
                                    .child(Button::new("redeem").label("Redeem").on_click({
                                        let redeem = redeem.clone();
                                        move |event, window, cx| {
                                            if redeem(event, window, cx) {
                                                window.close_dialog(cx);
                                            }
                                        }
                                    })),
                            ),
                    ),
            )
    });
}

fn parse_amount(input: &Entity<InputState>, cx: &App) -> Option<u64> {
    match input.read(cx).value().trim().parse::<u64>() {
        Ok(amount) if amount > 0 => Some(amount),
        _ => None,
    }
}

impl gpui_kit::component::dock::BasePanel for WalletPanel {
    fn panel_name(&self) -> &'static str {
        "Wallet"
    }
}

impl Panel for WalletPanel {
    fn title(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        h_flex()
            .gap_1p5()
            .child(
                Icon::new(IconName::Wallet)
                    .small()
                    .text_color(cx.theme().muted_foreground),
            )
            .child(self.name.clone())
            .into_any_element()
    }
}

impl EventEmitter<PanelEvent> for WalletPanel {}

impl Focusable for WalletPanel {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Render for WalletPanel {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let history = WalletRegistry::global(cx).read(cx).history().to_vec();

        v_flex()
            .size_full()
            .p_3()
            .gap_3()
            .text_xs()
            .child(self.render_balance(cx))
            .child(self.render_actions())
            .when_some(self.sent_token.as_ref(), |this, _token| {
                this.child(self.render_sent_token(cx))
            })
            .child(Separator::horizontal())
            .child(
                div()
                    .text_xs()
                    .font_semibold()
                    .text_color(cx.theme().muted_foreground)
                    .child("History:"),
            )
            .child(self.render_history(history, cx))
    }
}
