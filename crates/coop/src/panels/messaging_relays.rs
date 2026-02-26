use std::collections::HashSet;
use std::time::Duration;

use anyhow::{anyhow, Context as AnyhowContext, Error};
use gpui::prelude::FluentBuilder;
use gpui::{
    div, rems, AnyElement, App, AppContext, Context, Entity, EventEmitter, FocusHandle, Focusable,
    InteractiveElement, IntoElement, ParentElement, Render, SharedString, Styled, Subscription,
    Task, TextAlign, Window,
};
use nostr_sdk::prelude::*;
use smallvec::{smallvec, SmallVec};
use state::{NostrRegistry, TIMEOUT};
use theme::ActiveTheme;
use ui::button::{Button, ButtonVariants};
use ui::dock_area::panel::{Panel, PanelEvent};
use ui::input::{InputEvent, InputState, TextInput};
use ui::{divider, h_flex, v_flex, Disableable, IconName, Sizable, StyledExt, WindowExtension};

const MSG: &str = "Messaging Relays are relays that hosted all your messages. \
                   Other users will find your relays and send messages to it.";

pub fn init(window: &mut Window, cx: &mut App) -> Entity<MessagingRelayPanel> {
    cx.new(|cx| MessagingRelayPanel::new(window, cx))
}

#[derive(Debug)]
pub struct MessagingRelayPanel {
    name: SharedString,
    focus_handle: FocusHandle,

    /// Relay URL input
    input: Entity<InputState>,

    /// Whether the panel is updating
    updating: bool,

    /// Error message
    error: Option<SharedString>,

    /// All relays
    relays: HashSet<RelayUrl>,

    /// Event subscriptions
    _subscriptions: SmallVec<[Subscription; 1]>,

    /// Background tasks
    tasks: Vec<Task<Result<(), Error>>>,
}

impl MessagingRelayPanel {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let input = cx.new(|cx| InputState::new(window, cx).placeholder("wss://example.com"));
        let mut subscriptions = smallvec![];

        subscriptions.push(
            // Subscribe to user's input events
            cx.subscribe_in(&input, window, move |this, _input, event, window, cx| {
                if let InputEvent::PressEnter { .. } = event {
                    this.add(window, cx);
                }
            }),
        );

        // Run at the end of current cycle
        cx.defer_in(window, |this, window, cx| {
            this.load(window, cx);
        });

        Self {
            name: "Update Messaging Relays".into(),
            focus_handle: cx.focus_handle(),
            input,
            updating: false,
            relays: HashSet::new(),
            error: None,
            _subscriptions: subscriptions,
            tasks: vec![],
        }
    }

    fn load(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let nostr = NostrRegistry::global(cx);
        let client = nostr.read(cx).client();

        let task: Task<Result<Vec<RelayUrl>, Error>> = cx.background_spawn(async move {
            let signer = client.signer().context("Signer not found")?;
            let public_key = signer.get_public_key().await?;

            let filter = Filter::new()
                .kind(Kind::InboxRelays)
                .author(public_key)
                .limit(1);

            if let Some(event) = client.database().query(filter).await?.first_owned() {
                Ok(nip17::extract_owned_relay_list(event).collect())
            } else {
                Err(anyhow!("Not found."))
            }
        });

        self.tasks.push(cx.spawn_in(window, async move |this, cx| {
            let relays = task.await?;

            // Update state
            this.update(cx, |this, cx| {
                this.relays.extend(relays);
                cx.notify();
            })?;

            Ok(())
        }));
    }

    fn add(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let value = self.input.read(cx).value().to_string();

        if !value.starts_with("ws") {
            self.set_error("Relay URl is invalid", window, cx);
            return;
        }

        if let Ok(url) = RelayUrl::parse(&value) {
            if self.relays.insert(url) {
                self.input.update(cx, |this, cx| {
                    this.set_value("", window, cx);
                });
                cx.notify();
            }
        } else {
            self.set_error("Relay URl is invalid", window, cx);
        }
    }

    fn remove(&mut self, url: &RelayUrl, cx: &mut Context<Self>) {
        self.relays.remove(url);
        cx.notify();
    }

    fn set_error<E>(&mut self, error: E, window: &mut Window, cx: &mut Context<Self>)
    where
        E: Into<SharedString>,
    {
        self.error = Some(error.into());
        cx.notify();

        self.tasks.push(cx.spawn_in(window, async move |this, cx| {
            cx.background_executor().timer(Duration::from_secs(2)).await;

            // Clear the error message after a delay
            this.update(cx, |this, cx| {
                this.error = None;
                cx.notify();
            })?;

            Ok(())
        }));
    }

    fn set_updating(&mut self, updating: bool, cx: &mut Context<Self>) {
        self.updating = updating;
        cx.notify();
    }

    pub fn set_relays(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.relays.is_empty() {
            self.set_error("You need to add at least 1 relay", window, cx);
            return;
        };

        let nostr = NostrRegistry::global(cx);
        let client = nostr.read(cx).client();

        // Construct event tags
        let tags: Vec<Tag> = self
            .relays
            .iter()
            .map(|relay| Tag::relay(relay.clone()))
            .collect();

        // Set updating state
        self.set_updating(true, cx);

        let task: Task<Result<(), Error>> = cx.background_spawn(async move {
            // Construct nip17 event builder
            let builder = EventBuilder::new(Kind::InboxRelays, "").tags(tags);
            let event = client.sign_event_builder(builder).await?;

            // Set messaging relays
            client
                .send_event(&event)
                .to_nip65()
                .ok_timeout(Duration::from_secs(TIMEOUT))
                .await?;

            Ok(())
        });

        self.tasks.push(cx.spawn_in(window, async move |this, cx| {
            match task.await {
                Ok(_) => {
                    this.update_in(cx, |this, window, cx| {
                        this.set_updating(false, cx);
                        this.load(window, cx);

                        window.push_notification("Update successful", cx);
                    })?;
                }
                Err(e) => {
                    this.update_in(cx, |this, window, cx| {
                        this.set_updating(false, cx);
                        this.set_error(e.to_string(), window, cx);
                    })?;
                }
            };

            Ok(())
        }));
    }

    fn render_list_items(&mut self, cx: &mut Context<Self>) -> Vec<impl IntoElement> {
        let mut items = Vec::new();

        for url in self.relays.iter() {
            items.push(
                h_flex()
                    .id(SharedString::from(url.to_string()))
                    .group("")
                    .flex_1()
                    .w_full()
                    .h_8()
                    .px_2()
                    .justify_between()
                    .rounded(cx.theme().radius)
                    .bg(cx.theme().secondary_background)
                    .text_color(cx.theme().secondary_foreground)
                    .child(div().text_sm().child(SharedString::from(url.to_string())))
                    .child(
                        Button::new("remove_{ix}")
                            .icon(IconName::Close)
                            .xsmall()
                            .ghost()
                            .invisible()
                            .group_hover("", |this| this.visible())
                            .on_click({
                                let url = url.to_owned();
                                cx.listener(move |this, _ev, _window, cx| {
                                    this.remove(&url, cx);
                                })
                            }),
                    ),
            )
        }

        items
    }

    fn render_empty(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        h_flex()
            .h_20()
            .justify_center()
            .border_2()
            .border_dashed()
            .border_color(cx.theme().border)
            .rounded(cx.theme().radius_lg)
            .text_sm()
            .text_align(TextAlign::Center)
            .child(SharedString::from("Please add some relays."))
    }
}

impl Panel for MessagingRelayPanel {
    fn panel_id(&self) -> SharedString {
        self.name.clone()
    }

    fn title(&self, _cx: &App) -> AnyElement {
        self.name.clone().into_any_element()
    }
}

impl EventEmitter<PanelEvent> for MessagingRelayPanel {}

impl Focusable for MessagingRelayPanel {
    fn focus_handle(&self, _: &App) -> gpui::FocusHandle {
        self.focus_handle.clone()
    }
}

impl Render for MessagingRelayPanel {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        v_flex()
            .p_3()
            .gap_3()
            .w_full()
            .child(
                div()
                    .text_xs()
                    .text_color(cx.theme().text_muted)
                    .child(SharedString::from(MSG)),
            )
            .child(divider(cx))
            .child(
                v_flex()
                    .gap_2()
                    .flex_1()
                    .w_full()
                    .text_sm()
                    .child(
                        div()
                            .text_xs()
                            .font_semibold()
                            .text_color(cx.theme().text_muted)
                            .child(SharedString::from("Relays:")),
                    )
                    .child(
                        v_flex()
                            .gap_1()
                            .child(
                                h_flex()
                                    .gap_1()
                                    .w_full()
                                    .child(
                                        TextInput::new(&self.input)
                                            .small()
                                            .bordered(false)
                                            .cleanable(),
                                    )
                                    .child(
                                        Button::new("add")
                                            .icon(IconName::Plus)
                                            .tooltip("Add relay")
                                            .ghost()
                                            .size(rems(2.))
                                            .on_click(cx.listener(move |this, _, window, cx| {
                                                this.add(window, cx);
                                            })),
                                    ),
                            )
                            .when_some(self.error.as_ref(), |this, error| {
                                this.child(
                                    div()
                                        .italic()
                                        .text_xs()
                                        .text_color(cx.theme().danger_foreground)
                                        .child(error.clone()),
                                )
                            }),
                    )
                    .map(|this| {
                        if self.relays.is_empty() {
                            this.child(self.render_empty(window, cx))
                        } else {
                            this.child(
                                v_flex()
                                    .gap_1()
                                    .flex_1()
                                    .w_full()
                                    .children(self.render_list_items(cx)),
                            )
                        }
                    })
                    .child(
                        Button::new("submit")
                            .icon(IconName::CheckCircle)
                            .label("Update")
                            .primary()
                            .small()
                            .font_semibold()
                            .loading(self.updating)
                            .disabled(self.updating)
                            .on_click(cx.listener(move |this, _ev, window, cx| {
                                this.set_relays(window, cx);
                            })),
                    ),
            )
    }
}
