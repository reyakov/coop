use ::nip60::{HistoryEntry, MintQuoteEntry, WalletEvent, WalletRegistry};
use common::TimestampExt;
use gpui::prelude::FluentBuilder;
use gpui::{
    App, AppContext, Context, Entity, EventEmitter, FocusHandle, Focusable, IntoElement,
    ParentElement, Render, SharedString, Styled, Subscription, Window, div,
};
use gpui_kit::assets::IconName;
use gpui_kit::base::StyledExt as _;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::clipboard::Clipboard;
use gpui_kit::component::dock::{Panel, PanelEvent};
use gpui_kit::component::input::{Input, InputState};
use gpui_kit::component::notification::Notification;
use gpui_kit::component::scroll::ScrollableElement as _;
use gpui_kit::component::separator::Separator;
use gpui_kit::component::{ActiveTheme, Icon, Sizable, WindowExt, h_flex, v_flex};
use nostr_sdk::prelude::*;
use smallvec::SmallVec;
pub fn init(window: &mut Window, cx: &mut App) -> Entity<WalletPanel> {
    cx.new(|cx| WalletPanel::new(window, cx))
}

pub struct WalletPanel {
    name: SharedString,
    focus_handle: FocusHandle,

    /// Mint URL input for the setup flow
    mint_input: Entity<InputState>,
    /// Amount input for sending a cashu token
    send_input: Entity<InputState>,
    /// Amount input for the funding invoice
    invoice_input: Entity<InputState>,
    /// Pasted token input for receiving ecash
    token_input: Entity<InputState>,
    /// The token produced by the last send, shown for copying
    sent_token: Option<SharedString>,

    _subscriptions: SmallVec<[Subscription; 1]>,
}

impl WalletPanel {
    fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let mint_input = cx.new(|cx| InputState::new(window, cx).placeholder("https://mint…"));
        let send_input = cx.new(|cx| InputState::new(window, cx).placeholder("Amount (sats)"));
        let invoice_input = cx.new(|cx| InputState::new(window, cx).placeholder("Amount (sats)"));
        let token_input = cx.new(|cx| InputState::new(window, cx).placeholder("cashu…"));

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
            mint_input,
            send_input,
            invoice_input,
            token_input,
            sent_token: None,
            _subscriptions: subscriptions,
        }
    }

    fn parse_amount(&self, input: &Entity<InputState>, cx: &Context<Self>) -> Option<u64> {
        match input.read(cx).value().trim().parse::<u64>() {
            Ok(amount) if amount > 0 => Some(amount),
            _ => None,
        }
    }

    fn setup(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let mint = self.mint_input.read(cx).value().trim().to_string();
        if mint.is_empty() {
            window.push_notification(Notification::error("Enter a mint URL"), cx);
            return;
        }

        WalletRegistry::global(cx).update(cx, |registry, cx| {
            registry.setup(mint, cx);
        });
    }

    fn send_token(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(amount) = self.parse_amount(&self.send_input, cx) else {
            window.push_notification(Notification::error("Enter an amount in sats"), cx);
            return;
        };

        WalletRegistry::global(cx).update(cx, |registry, cx| {
            registry.send_token(amount, cx);
        });
    }

    fn create_invoice(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(amount) = self.parse_amount(&self.invoice_input, cx) else {
            window.push_notification(Notification::error("Enter an amount in sats"), cx);
            return;
        };

        WalletRegistry::global(cx).update(cx, |registry, cx| {
            registry.create_mint_quote(amount, cx);
        });
    }

    fn claim_quote(&mut self, quote_id: String, cx: &mut Context<Self>) {
        WalletRegistry::global(cx).update(cx, |registry, cx| {
            registry.claim_mint_quote(quote_id, cx);
        });
    }

    fn receive_token(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let token = self.token_input.read(cx).value().trim().to_string();
        if token.is_empty() {
            window.push_notification(Notification::error("Paste a cashu token"), cx);
            return;
        }

        WalletRegistry::global(cx).update(cx, |registry, cx| {
            registry.receive_token(token, cx);
        });
    }

    fn render_balance(&self, cx: &Context<Self>) -> impl IntoElement {
        let balance = WalletRegistry::global(cx).read(cx).balance();

        h_flex()
            .gap_2()
            .items_end()
            .child(
                div()
                    .text_2xl()
                    .child(SharedString::from(balance.to_string())),
            )
            .child(
                div()
                    .text_xs()
                    .text_color(cx.theme().muted_foreground)
                    .child("sats"),
            )
    }

    fn render_setup(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let has_wallet = WalletRegistry::global(cx).read(cx).has_wallet();

        v_flex()
            .gap_1p5()
            .w_full()
            .child(
                div()
                    .text_xs()
                    .font_semibold()
                    .text_color(cx.theme().muted_foreground)
                    .child(SharedString::from(if has_wallet {
                        "Add a mint:"
                    } else {
                        "Set up your wallet with a mint:"
                    })),
            )
            .child(
                h_flex()
                    .gap_1()
                    .w_full()
                    .child(Input::new(&self.mint_input).cleanable(true))
                    .child(
                        Button::new("setup")
                            .label(if has_wallet { "Add" } else { "Set up" })
                            .primary()
                            .small()
                            .on_click(cx.listener(move |this, _ev, window, cx| {
                                this.setup(window, cx);
                            })),
                    ),
            )
    }

    fn render_send(&self, cx: &Context<Self>) -> impl IntoElement {
        v_flex()
            .gap_1p5()
            .w_full()
            .child(
                div()
                    .text_xs()
                    .font_semibold()
                    .text_color(cx.theme().muted_foreground)
                    .child("Send ecash:"),
            )
            .child(
                h_flex()
                    .gap_1()
                    .w_full()
                    .child(Input::new(&self.send_input).cleanable(true))
                    .child(
                        Button::new("send")
                            .label("Send")
                            .small()
                            .on_click(cx.listener(move |this, _ev, window, cx| {
                                this.send_token(window, cx);
                            })),
                    ),
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

    fn render_receive(&self, cx: &Context<Self>) -> impl IntoElement {
        let quotes = WalletRegistry::global(cx).read(cx).mint_quotes().to_vec();

        let mut section =
            v_flex()
                .gap_1p5()
                .w_full()
                .child(
                    div()
                        .text_xs()
                        .font_semibold()
                        .text_color(cx.theme().muted_foreground)
                        .child("Receive:"),
                )
                .child(
                    h_flex()
                        .gap_1()
                        .w_full()
                        .child(Input::new(&self.invoice_input).cleanable(true))
                        .child(Button::new("invoice").label("Invoice").small().on_click(
                            cx.listener(move |this, _ev, window, cx| {
                                this.create_invoice(window, cx);
                            }),
                        )),
                );

        for quote in quotes {
            section = section.child(self.render_quote(&quote, cx));
        }

        section.child(
            h_flex()
                .gap_1()
                .w_full()
                .child(Input::new(&self.token_input).cleanable(true))
                .child(
                    Button::new("receive")
                        .label("Redeem")
                        .small()
                        .on_click(cx.listener(move |this, _ev, window, cx| {
                            this.receive_token(window, cx);
                        })),
                ),
        )
    }

    fn render_quote(&self, quote: &MintQuoteEntry, cx: &Context<Self>) -> impl IntoElement {
        h_flex()
            .gap_1()
            .w_full()
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .text_color(cx.theme().muted_foreground)
                    .child(SharedString::from(quote.request.clone())),
            )
            .child(
                Clipboard::new(SharedString::from(format!("copy_{}", quote.quote_id)))
                    .accessibility_label("Copy lightning invoice")
                    .value_fn({
                        let request = SharedString::from(quote.request.clone());
                        move |_, _| request.clone()
                    }),
            )
            .child(
                Button::new(SharedString::from(format!("claim_{}", quote.quote_id)))
                    .label("Claim")
                    .small()
                    .on_click({
                        let quote_id = quote.quote_id.clone();
                        cx.listener(move |this, _ev, _window, cx| {
                            this.claim_quote(quote_id.clone(), cx);
                        })
                    }),
            )
    }

    fn render_history(&self, cx: &Context<Self>) -> impl IntoElement {
        let history = WalletRegistry::global(cx).read(cx).history().to_vec();

        let mut section = v_flex().gap_1().w_full().child(
            div()
                .text_xs()
                .font_semibold()
                .text_color(cx.theme().muted_foreground)
                .child("History:"),
        );

        if history.is_empty() {
            section = section.child(
                div()
                    .text_xs()
                    .text_color(cx.theme().muted_foreground)
                    .child("No transactions yet."),
            );
        }

        for entry in history {
            section = section.child(self.render_history_entry(&entry, cx));
        }

        section
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
        let has_wallet = WalletRegistry::global(cx).read(cx).has_wallet();

        v_flex()
            .p_3()
            .gap_3()
            .w_full()
            .overflow_y_scrollbar()
            .text_xs()
            .when(!has_wallet, |this| this.child(self.render_setup(cx)))
            .when(has_wallet, |this| {
                this.child(self.render_balance(cx))
                    .child(Separator::horizontal())
                    .child(self.render_send(cx))
                    .child(self.render_receive(cx))
                    .child(Separator::horizontal())
                    .child(self.render_history(cx))
                    .child(self.render_setup(cx))
            })
    }
}
