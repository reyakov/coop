use std::ops::Range;
use std::rc::Rc;

use ::nip60::{HistoryEntry, MintQuoteEntry, WalletEvent, WalletRegistry};
use common::TimestampExt;
use gpui::prelude::FluentBuilder;
use gpui::{
    AnyElement, App, AppContext, ClickEvent, Context, Entity, EventEmitter, FocusHandle, Focusable,
    InteractiveElement as _, IntoElement, ParentElement, Render, SharedString,
    StatefulInteractiveElement as _, Styled, Subscription, Task, Window, div, px, relative,
    uniform_list,
};
use gpui_kit::assets::IconName;
use gpui_kit::base::StyledExt as _;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::clipboard::Clipboard;
use gpui_kit::component::dock::{Panel, PanelEvent};
use gpui_kit::component::input::{Input, InputEvent, InputState, NumberInput};
use gpui_kit::component::notification::Notification;
use gpui_kit::component::separator::Separator;
use gpui_kit::component::{
    ActiveTheme, Disableable as _, Icon, Sizable, WindowExt, h_flex, v_flex,
};
use nostr_sdk::prelude::*;
use person::Directory;
use settings::AppSettings;
use smallvec::SmallVec;
use state::NostrRegistry;
use ui::Avatar;

pub fn init(window: &mut Window, cx: &mut App) -> Entity<WalletPanel> {
    cx.new(|cx| WalletPanel::new(window, cx))
}

type SubmitFn = Rc<dyn Fn(&ClickEvent, &mut Window, &mut App) -> bool>;

pub struct WalletPanel {
    name: SharedString,
    focus_handle: FocusHandle,
    _subscriptions: SmallVec<[Subscription; 1]>,
}

impl WalletPanel {
    fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let wallet = WalletRegistry::global(cx);
        let mut subscriptions = SmallVec::new();
        subscriptions.push(cx.subscribe_in(
            &wallet,
            window,
            |_this, _wallet, event, window, cx| match event {
                WalletEvent::Updated => cx.notify(),
                WalletEvent::Error(error) => {
                    window.push_notification(Notification::error(error.clone()), cx);
                }
            },
        ));

        Self {
            name: "Wallet".into(),
            focus_handle: cx.focus_handle(),
            _subscriptions: subscriptions,
        }
    }

    fn render_balance(&self, cx: &Context<Self>) -> impl IntoElement {
        let balance = WalletRegistry::global(cx).read(cx).balance();

        h_flex()
            .justify_center()
            .items_baseline()
            .gap_2()
            .child(
                div()
                    .text_3xl()
                    .line_height(relative(1.0))
                    .child(SharedString::from(balance.to_string())),
            )
            .child(
                div()
                    .italic()
                    .line_height(relative(1.0))
                    .text_color(cx.theme().muted_foreground)
                    .child("sats"),
            )
    }

    fn render_actions(&self) -> impl IntoElement {
        h_flex().w_full().justify_center().child(
            h_flex()
                .w_80()
                .gap_2()
                .child(
                    Button::new("send")
                        .label("Send")
                        .secondary()
                        .rounded_full()
                        .flex_1()
                        .on_click(|_, window, cx| open_send_dialog(window, cx)),
                )
                .child(
                    Button::new("receive")
                        .label("Receive")
                        .secondary()
                        .rounded_full()
                        .flex_1()
                        .on_click(|_, window, cx| open_receive_dialog(window, cx)),
                ),
        )
    }

    fn render_quotes(&self, quotes: Vec<MintQuoteEntry>, cx: &Context<Self>) -> Option<AnyElement> {
        if quotes.is_empty() {
            return None;
        }

        Some(
            v_flex()
                .gap_2()
                .child(
                    div()
                        .text_xs()
                        .font_semibold()
                        .text_color(cx.theme().muted_foreground)
                        .child("Pending invoices"),
                )
                .children(
                    quotes
                        .iter()
                        .map(|quote| self.render_quote_entry(quote, cx)),
                )
                .into_any_element(),
        )
    }

    fn render_quote_entry(&self, quote: &MintQuoteEntry, cx: &Context<Self>) -> impl IntoElement {
        let wallet = WalletRegistry::global(cx);
        let quote_id = quote.quote_id.clone();

        v_flex()
            .gap_1()
            .w_full()
            .child(
                h_flex()
                    .justify_between()
                    .gap_2()
                    .child(
                        h_flex()
                            .gap_2()
                            .min_w_0()
                            .child(
                                div()
                                    .flex_shrink_0()
                                    .text_color(cx.theme().success)
                                    .child(SharedString::from(format!("{} sats", quote.amount))),
                            )
                            .child(
                                div()
                                    .truncate()
                                    .text_color(cx.theme().muted_foreground)
                                    .child(SharedString::from(quote.mint.to_string())),
                            ),
                    )
                    .child(
                        h_flex()
                            .flex_shrink_0()
                            .gap_1()
                            .child(
                                Button::new(format!("mint-quote-check-{quote_id}"))
                                    .label("Check")
                                    .tooltip("Check payment and claim")
                                    .ghost()
                                    .small()
                                    .on_click({
                                        let wallet = wallet.clone();
                                        let quote_id = quote_id.clone();
                                        move |_, _, cx| {
                                            wallet.update(cx, |registry, cx| {
                                                registry.claim_mint_quote(quote_id.clone(), cx)
                                            });
                                        }
                                    }),
                            )
                            .child(
                                Clipboard::new(format!("mint-quote-copy-{quote_id}"))
                                    .value(quote.request.clone())
                                    .tooltip("Copy invoice")
                                    .small(),
                            )
                            .child(
                                Button::new(format!("mint-quote-dismiss-{quote_id}"))
                                    .icon(IconName::Close)
                                    .tooltip("Dismiss")
                                    .ghost()
                                    .small()
                                    .on_click({
                                        let wallet = wallet.clone();
                                        move |_, _, cx| {
                                            wallet.update(cx, |registry, cx| {
                                                registry.dismiss_mint_quote(quote_id.clone(), cx)
                                            });
                                        }
                                    }),
                            ),
                    ),
            )
            .child(
                div()
                    .overflow_hidden()
                    .whitespace_nowrap()
                    .text_ellipsis_middle()
                    .text_color(cx.theme().muted_foreground)
                    .child(SharedString::from(quote.request.clone())),
            )
    }

    fn render_history(
        &mut self,
        history: Vec<HistoryEntry>,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        if history.is_empty() {
            return v_flex()
                .items_center()
                .justify_center()
                .h_32()
                .w_full()
                .text_xs()
                .text_color(cx.theme().muted_foreground)
                .border_1()
                .border_dashed()
                .border_color(cx.theme().border)
                .rounded(cx.theme().radius_lg)
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

/// Pick a contact and an amount, then send a NIP-61 nutzap
fn open_send_dialog(window: &mut Window, cx: &mut App) {
    let view = cx.new(|cx| SendDialog::new(window, cx));

    window.open_dialog(cx, {
        let view = view.clone();
        move |this, _window, _cx| this.width(px(420.)).title("Send eCash").child(view.clone())
    });
}

struct SendDialog {
    /// Search input, filters contacts or accepts an npub / NIP-05 address
    input: Entity<InputState>,
    amount_input: Entity<InputState>,
    /// Current user's contacts
    contacts: Option<Vec<PublicKey>>,
    /// Person directory for contact names and avatars
    directory: Entity<Directory>,
    /// The contact chosen to receive the nutzap
    selected: Option<PublicKey>,
    /// Whether a NIP-05 address is being resolved
    resolving: bool,
    error: Option<SharedString>,
    tasks: Vec<Task<Result<(), anyhow::Error>>>,
    _subscriptions: Vec<Subscription>,
}

impl SendDialog {
    fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let input = cx.new(|cx| {
            InputState::new(window, cx).placeholder("Search or enter npub or user@address.me")
        });
        let amount_input = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder("Amount (sats)")
                .step(1.)
                .min(1.)
        });

        let subscriptions = vec![
            cx.subscribe_in(&input, window, |this, _input, event, window, cx| {
                if let InputEvent::PressEnter { .. } = event {
                    this.submit(window, cx);
                }
            }),
            cx.subscribe_in(&amount_input, window, |this, _input, event, window, cx| {
                if let InputEvent::PressEnter { .. } = event {
                    this.send(window, cx);
                }
            }),
        ];

        // Load the contacts once the view is mounted
        cx.defer_in(window, |this, window, cx| {
            this.load_contacts(window, cx);
        });

        Self {
            input,
            amount_input,
            contacts: None,
            directory: Directory::new(cx),
            selected: None,
            resolving: false,
            error: None,
            tasks: Vec::new(),
            _subscriptions: subscriptions,
        }
    }

    /// Load the signed-in user's contact list.
    fn load_contacts(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.contacts.is_some() {
            return;
        }

        let nostr = NostrRegistry::global(cx);
        let client = nostr.read(cx).client();

        let Some(current_user) = nostr.read(cx).current_user() else {
            return;
        };

        let task: Task<Result<Vec<PublicKey>, anyhow::Error>> = cx.background_spawn(async move {
            let filter = Filter::new()
                .author(current_user)
                .kind(Kind::ContactList)
                .limit(1);

            let contacts: Vec<PublicKey> = client
                .database()
                .query(filter)
                .await?
                .into_iter()
                .next()
                .map(|event| event.tags.public_keys().collect())
                .unwrap_or_default();

            Ok(contacts)
        });

        self.tasks.push(cx.spawn_in(window, async move |this, cx| {
            let contacts = task.await;

            this.update_in(cx, |this, _window, cx| {
                match contacts {
                    Ok(contacts) => {
                        this.directory.update(cx, |directory, cx| {
                            directory.track(contacts.iter().copied(), cx)
                        });
                        this.contacts = Some(contacts);
                    }
                    Err(error) => this.set_error(error.to_string(), cx),
                }
                cx.notify();
            })?;

            Ok(())
        }));
    }

    /// The contacts matching the current input, or every contact when empty.
    fn filtered_contacts(&self, cx: &Context<Self>) -> Vec<PublicKey> {
        let Some(contacts) = self.contacts.as_ref() else {
            return Vec::new();
        };

        let query = self.input.read(cx).value().trim().to_lowercase();

        if query.is_empty() {
            return contacts.clone();
        }

        contacts
            .iter()
            .filter(|public_key| {
                if public_key.to_hex().to_lowercase().contains(&query) {
                    return true;
                }

                let name = self.directory.read(cx).person(public_key, cx).name();
                name.to_lowercase().contains(&query)
            })
            .cloned()
            .collect()
    }

    /// Resolve the input into a contact selection.
    fn submit(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.resolving {
            return;
        }

        let query = self.input.read(cx).value().trim().to_string();

        if query.is_empty() {
            return;
        }

        // A raw public key or an npub can be selected directly.
        if let Ok(public_key) = PublicKey::parse(&query) {
            self.select(public_key, cx);
            self.clear_input(window, cx);
            return;
        }

        // A NIP-05 address is resolved before being selected.
        if let Ok(address) = Nip05Address::parse(&query) {
            let nostr = NostrRegistry::global(cx);
            let task = nostr.read(cx).query_address(address, cx);

            self.set_resolving(true, window, cx);

            self.tasks.push(cx.spawn_in(window, async move |this, cx| {
                let result = task.await;

                this.update_in(cx, |this, window, cx| {
                    this.set_resolving(false, window, cx);

                    match result {
                        Ok(public_key) => {
                            this.select(public_key, cx);
                            this.clear_input(window, cx);
                        }
                        Err(error) => this.set_error(error.to_string(), cx),
                    }
                })?;

                Ok(())
            }));

            return;
        }

        // Fall back to a local search by contact name.
        let matches = self.filtered_contacts(cx);
        match matches.len() {
            0 => self.set_error("Enter a valid npub or NIP-05 address", cx),
            1 => {
                let public_key = matches[0];
                self.select(public_key, cx);
                self.clear_input(window, cx);
            }
            _ => self.set_error("Multiple contacts match, pick one from the list", cx),
        }
    }

    /// Select a single recipient for the nutzap.
    fn select(&mut self, public_key: PublicKey, cx: &mut Context<Self>) {
        let nostr = NostrRegistry::global(cx);

        if nostr.read(cx).current_user() == Some(public_key) {
            self.set_error("You cannot send to yourself", cx);
            return;
        }

        self.directory
            .update(cx, |directory, cx| directory.track([public_key], cx));
        self.selected = Some(public_key);
        self.error = None;
        cx.notify();
    }

    /// Toggle a contact's selection.
    fn toggle_selection(&mut self, public_key: &PublicKey, cx: &mut Context<Self>) {
        if self.selected == Some(*public_key) {
            self.selected = None;
            cx.notify();
        } else {
            self.select(*public_key, cx);
        }
    }

    /// Validate the selection and amount, then schedule the nutzap.
    fn send(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> bool {
        let Some(recipient) = self.selected else {
            self.set_error("Select a contact to send to", cx);
            return false;
        };

        let Some(amount) = parse_amount(&self.amount_input, cx) else {
            self.set_error("Enter an amount in sats", cx);
            return false;
        };

        WalletRegistry::global(cx).update(cx, |registry, cx| {
            registry.send_nutzap(recipient, amount, None, None, cx);
        });

        true
    }

    fn clear_input(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.input.update(cx, |this, cx| {
            this.set_value("", window, cx);
        });
    }

    /// Toggle the input's loading spinner while a NIP-05 address is resolved.
    fn set_resolving(&mut self, resolving: bool, window: &mut Window, cx: &mut Context<Self>) {
        self.input.update(cx, |this, cx| {
            this.set_loading(resolving, window, cx);
        });
        self.resolving = resolving;
        cx.notify();
    }

    fn set_error(&mut self, message: impl Into<SharedString>, cx: &mut Context<Self>) {
        self.error = Some(message.into());
        cx.notify();
    }

    fn render_contacts(
        &self,
        range: Range<usize>,
        contacts: &[PublicKey],
        cx: &mut Context<Self>,
    ) -> Vec<AnyElement> {
        contacts
            .get(range)
            .into_iter()
            .flatten()
            .map(|public_key| {
                let profile = self.directory.read(cx).person(public_key, cx);
                let public_key = *public_key;
                let selected = self.selected == Some(public_key);
                let id = SharedString::from(format!("contact-{}", public_key.to_hex()));

                h_flex()
                    .id(id)
                    .w_full()
                    .h_8()
                    .px_2()
                    .gap_2()
                    .rounded(cx.theme().radius)
                    .text_sm()
                    .cursor_pointer()
                    .hover(|this| this.bg(cx.theme().list_hover))
                    .when(selected, |this| this.bg(cx.theme().list_active))
                    .when(!AppSettings::get_hide_avatar(cx), |this| {
                        this.child(
                            Avatar::new(profile.avatar())
                                .seed(profile.avatar_seed())
                                .small(),
                        )
                    })
                    .child(div().min_w_0().truncate().child(profile.name()))
                    .on_click(cx.listener(move |this, _event, _window, cx| {
                        this.toggle_selection(&public_key, cx);
                    }))
                    .into_any_element()
            })
            .collect()
    }

    fn render_selected(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let Some(public_key) = self.selected else {
            return div().into_any_element();
        };

        let profile = self.directory.read(cx).person(&public_key, cx);
        let id = SharedString::from(format!("selected-{}", public_key.to_hex()));

        h_flex()
            .gap_1()
            .child(
                h_flex()
                    .id(id)
                    .gap_1()
                    .p_0p5()
                    .rounded_full()
                    .bg(cx.theme().secondary)
                    .hover(|this| this.bg(cx.theme().primary))
                    .child(
                        Avatar::new(profile.avatar())
                            .seed(profile.avatar_seed())
                            .xsmall(),
                    )
                    .child(
                        div()
                            .max_w_32()
                            .truncate()
                            .text_xs()
                            .line_height(relative(1.0))
                            .child(profile.name()),
                    )
                    .on_click(cx.listener(move |this, _event, _window, cx| {
                        this.selected = None;
                        cx.notify();
                    })),
            )
            .into_any_element()
    }
}

impl Render for SendDialog {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        const NOTICE: &str = "Recipients need a NIP-60 wallet on a mint you both use.";

        let contacts = Rc::new(self.filtered_contacts(cx));
        let contact_count = contacts.len();

        v_flex()
            .gap_3()
            .child(NumberInput::new(&self.amount_input))
            .child(
                h_flex()
                    .gap_1()
                    .w_full()
                    .child(Input::new(&self.input).flex_1().cleanable(true))
                    .child(
                        Button::new("add")
                            .icon(IconName::Plus)
                            .tooltip("Add person")
                            .ghost()
                            .large()
                            .disabled(self.resolving)
                            .on_click(cx.listener(|this, _event, window, cx| {
                                this.submit(window, cx);
                            })),
                    ),
            )
            .when(self.selected.is_some(), |this| {
                this.child(self.render_selected(cx))
            })
            .child(
                uniform_list(
                    "send-contacts",
                    contact_count,
                    cx.processor(move |this, range, _window, cx| {
                        this.render_contacts(range, contacts.as_slice(), cx)
                    }),
                )
                .h_64(),
            )
            .map(|this| {
                if let Some(error) = self.error.clone() {
                    this.child(div().text_xs().text_color(cx.theme().danger).child(error))
                } else {
                    this.child(
                        div()
                            .text_xs()
                            .text_color(cx.theme().muted_foreground)
                            .child(NOTICE),
                    )
                }
            })
            .child(
                h_flex()
                    .gap_1()
                    .justify_end()
                    .child(
                        Button::new("cancel")
                            .label("Cancel")
                            .secondary()
                            .on_click(|_, window, cx| window.close_dialog(cx)),
                    )
                    .child(
                        Button::new("send")
                            .label("Send")
                            .primary()
                            .on_click(cx.listener(move |this, _event, window, cx| {
                                if this.send(window, cx) {
                                    window.close_dialog(cx);
                                }
                            })),
                    ),
            )
    }
}

/// Create a lightning invoice or redeem a pasted cashu token
fn open_receive_dialog(window: &mut Window, cx: &mut App) {
    let amount_input = cx.new(|cx| InputState::new(window, cx).placeholder("Amount (sats)"));
    let token_input = cx.new(|cx| InputState::new(window, cx).placeholder("cashu…"));

    let create_invoice: SubmitFn = {
        let wallet = WalletRegistry::global(cx);
        let input = amount_input.clone();
        Rc::new(move |_, window, cx| {
            let Some(amount) = parse_amount(&input, cx) else {
                window.push_notification(Notification::error("Enter an amount in sats"), cx);
                return false;
            };

            wallet.update(cx, |registry, cx| {
                registry.create_mint_quote(amount, cx);
            });

            true
        })
    };

    let redeem: SubmitFn = {
        let wallet = WalletRegistry::global(cx);
        let input = token_input.clone();
        Rc::new(move |_, window, cx| {
            let token = input.read(cx).value().trim().to_string();

            if token.is_empty() {
                window.push_notification(Notification::error("Paste a cashu token"), cx);
                return false;
            }

            wallet.update(cx, |registry, cx| {
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
                                    .child(
                                        Button::new("invoice")
                                            .label("Invoice")
                                            .secondary()
                                            .w_24()
                                            .on_click({
                                                let create_invoice = create_invoice.clone();
                                                move |event, window, cx| {
                                                    if create_invoice(event, window, cx) {
                                                        window.close_dialog(cx);
                                                    }
                                                }
                                            }),
                                    ),
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
                                    .child(
                                        Button::new("redeem")
                                            .label("Redeem")
                                            .secondary()
                                            .w_24()
                                            .on_click({
                                                let redeem = redeem.clone();
                                                move |event, window, cx| {
                                                    if redeem(event, window, cx) {
                                                        window.close_dialog(cx);
                                                    }
                                                }
                                            }),
                                    ),
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
            .child(ui::experimental_tag(cx))
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
        let wallet = WalletRegistry::global(cx);
        let history = wallet.read(cx).history().to_vec();
        let quotes = wallet.read(cx).mint_quotes().to_vec();

        v_flex()
            .size_full()
            .p_3()
            .gap_4()
            .text_xs()
            .child(self.render_balance(cx))
            .child(self.render_actions())
            .children(self.render_quotes(quotes, cx))
            .child(
                v_flex()
                    .gap_2()
                    .child(
                        div()
                            .text_xs()
                            .font_semibold()
                            .text_color(cx.theme().muted_foreground)
                            .child("History"),
                    )
                    .child(self.render_history(history, cx)),
            )
    }
}
