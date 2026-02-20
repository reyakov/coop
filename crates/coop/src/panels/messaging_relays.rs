use std::collections::HashSet;
use std::time::Duration;

use anyhow::{anyhow, Context as AnyhowContext, Error};
use gpui::prelude::FluentBuilder;
use gpui::{
    div, relative, uniform_list, AnyElement, App, AppContext, Context, Entity, EventEmitter,
    FocusHandle, Focusable, InteractiveElement, IntoElement, ParentElement, Render, SharedString,
    Styled, Subscription, Task, TextAlign, UniformList, Window,
};
use nostr_sdk::prelude::*;
use smallvec::{smallvec, SmallVec};
use state::NostrRegistry;
use theme::ActiveTheme;
use ui::button::{Button, ButtonVariants};
use ui::dock_area::panel::{Panel, PanelEvent};
use ui::input::{InputEvent, InputState, TextInput};
use ui::{divider, h_flex, v_flex, IconName, Sizable, StyledExt};

pub fn init(window: &mut Window, cx: &mut App) -> Entity<MessagingRelayPanel> {
    cx.new(|cx| MessagingRelayPanel::new(window, cx))
}

#[derive(Debug)]
pub struct MessagingRelayPanel {
    name: SharedString,
    focus_handle: FocusHandle,

    /// Relay URL input
    input: Entity<InputState>,

    /// Error message
    error: Option<SharedString>,

    // All relays
    relays: HashSet<RelayUrl>,

    // Event subscriptions
    _subscriptions: SmallVec<[Subscription; 1]>,

    // Background tasks
    _tasks: SmallVec<[Task<()>; 1]>,
}

impl MessagingRelayPanel {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let input = cx.new(|cx| InputState::new(window, cx).placeholder("wss://example.com"));
        let nostr = NostrRegistry::global(cx);
        let client = nostr.read(cx).client();

        let mut subscriptions = smallvec![];
        let mut tasks = smallvec![];

        tasks.push(
            // Load user's relays in the local database
            cx.spawn_in(window, async move |this, cx| {
                let result = cx
                    .background_spawn(async move { Self::load(&client).await })
                    .await;

                if let Ok(relays) = result {
                    this.update(cx, |this, cx| {
                        this.relays.extend(relays);
                        cx.notify();
                    })
                    .ok();
                }
            }),
        );

        subscriptions.push(
            // Subscribe to user's input events
            cx.subscribe_in(&input, window, move |this, _input, event, window, cx| {
                if let InputEvent::PressEnter { .. } = event {
                    this.add(window, cx);
                }
            }),
        );

        Self {
            name: "Update Messaging Relays".into(),
            focus_handle: cx.focus_handle(),
            input,
            relays: HashSet::new(),
            error: None,
            _subscriptions: subscriptions,
            _tasks: tasks,
        }
    }

    async fn load(client: &Client) -> Result<Vec<RelayUrl>, Error> {
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
    }

    fn add(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let value = self.input.read(cx).value().to_string();

        if !value.starts_with("ws") {
            self.set_error("Relay URl is invalid", window, cx);
            return;
        }

        if let Ok(url) = RelayUrl::parse(&value) {
            if !self.relays.insert(url) {
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

        cx.spawn_in(window, async move |this, cx| {
            cx.background_executor().timer(Duration::from_secs(2)).await;
            // Clear the error message after a delay
            this.update(cx, |this, cx| {
                this.error = None;
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    pub fn set_relays(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.relays.is_empty() {
            self.set_error("You need to add at least 1 relay", window, cx);
            return;
        };

        let nostr = NostrRegistry::global(cx);
        let client = nostr.read(cx).client();

        let tags: Vec<Tag> = self
            .relays
            .iter()
            .map(|relay| Tag::relay(relay.clone()))
            .collect();

        let task: Task<Result<(), Error>> = cx.background_spawn(async move {
            // Construct nip17 event builder
            let builder = EventBuilder::new(Kind::InboxRelays, "").tags(tags);
            let event = client.sign_event_builder(builder).await?;

            // Set messaging relays
            client.send_event(&event).to_nip65().await?;

            Ok(())
        });

        cx.spawn_in(window, async move |this, cx| {
            match task.await {
                Ok(_) => {
                    // TODO
                }
                Err(e) => {
                    this.update_in(cx, |this, window, cx| {
                        this.set_error(e.to_string(), window, cx);
                    })
                    .ok();
                }
            };
        })
        .detach();
    }

    fn render_list(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> UniformList {
        let relays = self.relays.clone();
        let total = relays.len();

        uniform_list(
            "relays",
            total,
            cx.processor(move |_v, range, _window, cx| {
                let mut items = Vec::new();

                for ix in range {
                    let Some(url) = relays.iter().nth(ix) else {
                        continue;
                    };

                    items.push(
                        div()
                            .id(SharedString::from(url.to_string()))
                            .group("")
                            .w_full()
                            .h_9()
                            .py_0p5()
                            .child(
                                h_flex()
                                    .px_2()
                                    .flex()
                                    .justify_between()
                                    .rounded(cx.theme().radius)
                                    .bg(cx.theme().elevated_surface_background)
                                    .child(
                                        div().text_sm().child(SharedString::from(url.to_string())),
                                    )
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
                            ),
                    )
                }

                items
            }),
        )
        .h_full()
    }

    fn render_empty(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        h_flex()
            .mt_2()
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
                    .child(SharedString::from("Update Messaging Relays")),
            )
            .child(
                v_flex()
                    .w_112()
                    .gap_2()
                    .text_sm()
                    .child(
                        v_flex()
                            .gap_1p5()
                            .child(
                                h_flex()
                                    .gap_1()
                                    .w_full()
                                    .child(TextInput::new(&self.input).small())
                                    .child(
                                        Button::new("add")
                                            .icon(IconName::Plus)
                                            .label("Add")
                                            .ghost()
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
                        if !self.relays.is_empty() {
                            this.child(self.render_list(window, cx))
                        } else {
                            this.child(self.render_empty(window, cx))
                        }
                    })
                    .child(divider(cx))
                    .child(
                        Button::new("submit")
                            .label("Update")
                            .primary()
                            .on_click(cx.listener(move |this, _ev, window, cx| {
                                this.set_relays(window, cx);
                            })),
                    ),
            )
    }
}
