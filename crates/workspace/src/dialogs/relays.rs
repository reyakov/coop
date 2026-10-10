use std::collections::HashSet;

use anyhow::{Error, anyhow};
use gpui::prelude::FluentBuilder;
use gpui::{
    App, AppContext, ClickEvent, Context, Div, Entity, InteractiveElement, IntoElement,
    ParentElement, Render, SharedString, Stateful, Styled, Subscription, Task, TextAlign,
    WeakEntity, Window, div, px, rems,
};
use instant::Duration;
use nostr_sdk::prelude::*;
use smallvec::{SmallVec, smallvec};
use state::NostrRegistry;
use theme::{ActiveTheme, TextStyle, Typeset as _, button_radius, panel_radius};
use ui::button::{Button, ButtonVariants, IconPosition};
use ui::input::{Input, InputEvent, InputState};
use ui::menu::{DropdownMenu, PopupMenuItem};
use ui::scroll::ScrollableElement;
use ui::tab::Tab;
use ui::tab::tab_bar::TabBar;
use ui::{Disableable, IconName, Sizable, StyledExt, WindowExtension, h_flex, v_flex};

const GOSSIP_MSG: &str = "Gossip Relays are the relays where you publish all your events and discover events from others.";
const INBOX_MSG: &str = "Messaging Relays host your private messages. Others find these relays and send you messages there.";

pub fn open(window: &mut Window, cx: &mut App) {
    let view = cx.new(|cx| RelayManager::new(window, cx));

    window.open_dialog(cx, move |this, _window, _cx| {
        this.title("Relays").child(view.clone())
    });
}

/// State of the NIP-65 relay list (gossip relays).
struct GossipRelays {
    /// Relay URL input
    input: Entity<InputState>,
    /// Metadata assigned to relays added afterwards
    metadata: Option<RelayMetadata>,
    /// All relays
    relays: HashSet<(RelayUrl, Option<RelayMetadata>)>,
    /// Whether the relay list is updating
    updating: bool,
    error: Option<SharedString>,
    tasks: Vec<Task<Result<(), Error>>>,
}

/// State of the NIP-17 inbox relays (messaging relays).
struct InboxRelays {
    /// Relay URL input
    input: Entity<InputState>,
    /// All relays
    relays: HashSet<RelayUrl>,
    /// Whether the relays are updating
    updating: bool,
    error: Option<SharedString>,
    tasks: Vec<Task<Result<(), Error>>>,
}

pub struct RelayManager {
    /// Index of the active tab, `0` for gossip relays and `1` for inbox relays
    active_tab: usize,
    gossip: GossipRelays,
    inbox: InboxRelays,
    _subscriptions: SmallVec<[Subscription; 2]>,
}

impl RelayManager {
    fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let gossip = GossipRelays::new(window, cx);
        let inbox = InboxRelays::new(window, cx);

        let mut subscriptions = smallvec![];

        subscriptions.push(
            // Add a relay when the user presses enter in the gossip input
            cx.subscribe_in(&gossip.input, window, |this, _input, event, window, cx| {
                if let InputEvent::PressEnter { .. } = event {
                    this.gossip.add(window, cx);
                }
            }),
        );

        subscriptions.push(
            // Add a relay when the user presses enter in the inbox input
            cx.subscribe_in(&inbox.input, window, |this, _input, event, window, cx| {
                if let InputEvent::PressEnter { .. } = event {
                    this.inbox.add(window, cx);
                }
            }),
        );

        // Run at the end of current cycle
        cx.defer_in(window, |this, window, cx| {
            this.gossip.load(window, cx);
            this.inbox.load(window, cx);
        });

        Self {
            active_tab: 0,
            gossip,
            inbox,
            _subscriptions: subscriptions,
        }
    }

    fn render_gossip(&mut self, cx: &mut Context<Self>) -> impl IntoElement {
        let manager = cx.entity().downgrade();

        v_flex()
            .gap(px(theme::SPACE))
            .child(
                div()
                    .text_style(TextStyle::Caption)
                    .text_color(cx.theme().text_muted)
                    .child(SharedString::from(GOSSIP_MSG)),
            )
            .child(
                h_flex()
                    .gap_1()
                    .w_full()
                    .child(Input::new(&self.gossip.input).small().cleanable(true))
                    .child(
                        Button::new("metadata")
                            .map(|this| {
                                if let Some(metadata) = self.gossip.metadata {
                                    this.label(metadata.to_string())
                                } else {
                                    this.label("R & W")
                                }
                            })
                            .tooltip("Relay metadata")
                            .ghost()
                            .h(rems(2.))
                            .text_style(TextStyle::Caption)
                            .dropdown_menu(move |menu, _window, _cx| {
                                menu.item(metadata_item(&manager, RelayMetadata::Read))
                                    .item(metadata_item(&manager, RelayMetadata::Write))
                            }),
                    )
                    .child(
                        Button::new("add-gossip")
                            .icon(IconName::Plus)
                            .tooltip("Add relay")
                            .ghost()
                            .size(rems(2.))
                            .on_click(cx.listener(move |this, _, window, cx| {
                                this.gossip.add(window, cx);
                            })),
                    ),
            )
            .when_some(self.gossip.error.as_ref(), |this, error| {
                this.child(
                    div()
                        .italic()
                        .text_style(TextStyle::Caption)
                        .text_color(cx.theme().danger)
                        .child(error.clone()),
                )
            })
            .map(|this| {
                if self.gossip.relays.is_empty() {
                    this.child(empty_relays(cx))
                } else {
                    this.child(
                        v_flex()
                            .gap_1()
                            .w_full()
                            .max_h(rems(16.))
                            .overflow_y_scrollbar()
                            .children(self.gossip_rows(cx)),
                    )
                }
            })
    }

    fn gossip_rows(&self, cx: &mut Context<Self>) -> Vec<Stateful<Div>> {
        let mut rows = Vec::new();

        for (relay, metadata) in self.gossip.relays.iter() {
            let url = relay.to_owned();
            let badge = match metadata {
                Some(metadata) => SharedString::from(metadata.to_string()),
                None => SharedString::from("Read and Write"),
            };

            rows.push(relay_row(
                url.clone(),
                Some(badge),
                cx.listener(move |this, _event, _window, cx| this.gossip.remove(&url, cx)),
                cx,
            ));
        }

        rows
    }

    fn render_inbox(&mut self, cx: &mut Context<Self>) -> impl IntoElement {
        v_flex()
            .gap(px(theme::SPACE))
            .child(
                div()
                    .text_style(TextStyle::Caption)
                    .text_color(cx.theme().text_muted)
                    .child(SharedString::from(INBOX_MSG)),
            )
            .child(
                h_flex()
                    .gap_1()
                    .w_full()
                    .child(Input::new(&self.inbox.input).small().cleanable(true))
                    .child(
                        Button::new("add-inbox")
                            .icon(IconName::Plus)
                            .tooltip("Add relay")
                            .ghost()
                            .size(rems(2.))
                            .on_click(cx.listener(move |this, _, window, cx| {
                                this.inbox.add(window, cx);
                            })),
                    ),
            )
            .when_some(self.inbox.error.as_ref(), |this, error| {
                this.child(
                    div()
                        .italic()
                        .text_style(TextStyle::Caption)
                        .text_color(cx.theme().danger)
                        .child(error.clone()),
                )
            })
            .map(|this| {
                if self.inbox.relays.is_empty() {
                    this.child(empty_relays(cx))
                } else {
                    this.child(
                        v_flex()
                            .gap_1()
                            .w_full()
                            .max_h(rems(16.))
                            .overflow_y_scrollbar()
                            .children(self.inbox_rows(cx)),
                    )
                }
            })
    }

    fn inbox_rows(&self, cx: &mut Context<Self>) -> Vec<Stateful<Div>> {
        let mut rows = Vec::new();

        for relay in self.inbox.relays.iter() {
            let url = relay.to_owned();

            rows.push(relay_row(
                url.clone(),
                None,
                cx.listener(move |this, _event, _window, cx| this.inbox.remove(&url, cx)),
                cx,
            ));
        }

        rows
    }

    fn render_tab_bar(&self, cx: &mut Context<Self>) -> impl IntoElement {
        TabBar::new("relays-tabs")
            .segmented(true)
            .selected_index(self.active_tab)
            .on_click(cx.listener(|this, ix: &usize, _window, cx| {
                this.active_tab = *ix;
                cx.notify();
            }))
            .child(Tab::new().label("Gossip Relays"))
            .child(Tab::new().label("Messaging Relays"))
    }

    fn render_actions(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let updating = if self.active_tab == 0 {
            self.gossip.updating
        } else {
            self.inbox.updating
        };

        h_flex().gap_1().justify_end().child(
            Button::new("update")
                .icon(IconName::ArrowRight)
                .icon_position(IconPosition::End)
                .label("Update")
                .primary()
                .font_semibold()
                .loading(updating)
                .disabled(updating)
                .on_click(cx.listener(move |this, _ev, window, cx| {
                    if this.active_tab == 0 {
                        this.gossip.publish(window, cx);
                    } else {
                        this.inbox.publish(window, cx);
                    }
                })),
        )
    }
}

impl GossipRelays {
    fn new(window: &mut Window, cx: &mut Context<RelayManager>) -> Self {
        let input = cx.new(|cx| InputState::new(window, cx).placeholder("wss://example.com"));

        Self {
            input,
            metadata: None,
            relays: HashSet::new(),
            updating: false,
            error: None,
            tasks: vec![],
        }
    }

    #[allow(clippy::type_complexity)]
    fn load(&mut self, window: &mut Window, cx: &mut Context<RelayManager>) {
        let nostr = NostrRegistry::global(cx);
        let client = nostr.read(cx).client();

        let Some(public_key) = nostr.read(cx).current_user() else {
            return;
        };

        let task: Task<Result<Vec<(RelayUrl, Option<RelayMetadata>)>, Error>> = cx
            .background_spawn(async move {
                let filter = Filter::new()
                    .kind(Kind::RelayList)
                    .author(public_key)
                    .limit(1);

                if let Some(event) = client.database().query(filter).await?.into_iter().next() {
                    Ok(nip65::extract_relay_list(&event).collect())
                } else {
                    Err(anyhow!("Not found."))
                }
            });

        self.tasks.push(cx.spawn_in(window, async move |this, cx| {
            let relays = task.await?;

            this.update(cx, |this, cx| {
                this.gossip.relays.extend(relays);
                cx.notify();
            })?;

            Ok(())
        }));
    }

    fn add(&mut self, window: &mut Window, cx: &mut Context<RelayManager>) {
        let value = self.input.read(cx).value().to_string();
        let metadata = self.metadata;

        if !value.starts_with("ws") {
            self.set_error("Relay URl is invalid", window, cx);
            return;
        }

        if let Ok(url) = RelayUrl::parse(&value) {
            if self.relays.insert((url, metadata)) {
                self.input.update(cx, |this, cx| {
                    this.set_value("", window, cx);
                });
                cx.notify();
            }
        } else {
            self.set_error("Relay URl is invalid", window, cx);
        }
    }

    fn remove(&mut self, url: &RelayUrl, cx: &mut Context<RelayManager>) {
        self.relays.retain(|(relay, _)| relay != url);
        cx.notify();
    }

    fn publish(&mut self, window: &mut Window, cx: &mut Context<RelayManager>) {
        if self.relays.is_empty() {
            self.set_error("You need to add at least 1 relay", window, cx);
            return;
        }

        let nostr = NostrRegistry::global(cx);
        let client = nostr.read(cx).client();
        let signer = nostr.read(cx).signer();

        let relays = self.relays.clone();

        self.set_updating(true, cx);

        let task: Task<Result<(), Error>> = cx.background_spawn(async move {
            let event = nip65::RelayList::new(relays)
                .finalize_async(&signer)
                .await?;

            // Set relay list for current user
            client.send_event(&event).await?;

            Ok(())
        });

        self.tasks.push(cx.spawn_in(window, async move |this, cx| {
            match task.await {
                Ok(_) => {
                    this.update_in(cx, |this, window, cx| {
                        this.gossip.set_updating(false, cx);
                        this.gossip.load(window, cx);

                        window.push_notification("Update successful", cx);
                    })?;
                }
                Err(error) => {
                    this.update_in(cx, |this, window, cx| {
                        this.gossip.set_updating(false, cx);
                        this.gossip.set_error(error.to_string(), window, cx);
                    })?;
                }
            }

            Ok(())
        }));
    }

    fn set_error<E>(&mut self, error: E, window: &mut Window, cx: &mut Context<RelayManager>)
    where
        E: Into<SharedString>,
    {
        self.error = Some(error.into());
        cx.notify();

        self.tasks.push(cx.spawn_in(window, async move |this, cx| {
            cx.background_executor().timer(Duration::from_secs(2)).await;

            // Clear the error message after a delay
            this.update(cx, |this, cx| {
                this.gossip.error = None;
                cx.notify();
            })?;

            Ok(())
        }));
    }

    fn set_updating(&mut self, updating: bool, cx: &mut Context<RelayManager>) {
        self.updating = updating;
        cx.notify();
    }
}

impl InboxRelays {
    fn new(window: &mut Window, cx: &mut Context<RelayManager>) -> Self {
        let input = cx.new(|cx| InputState::new(window, cx).placeholder("wss://example.com"));

        Self {
            input,
            relays: HashSet::new(),
            updating: false,
            error: None,
            tasks: vec![],
        }
    }

    fn load(&mut self, window: &mut Window, cx: &mut Context<RelayManager>) {
        let nostr = NostrRegistry::global(cx);
        let client = nostr.read(cx).client();

        let Some(public_key) = nostr.read(cx).current_user() else {
            return;
        };

        let task: Task<Result<Vec<RelayUrl>, Error>> = cx.background_spawn(async move {
            let filter = Filter::new()
                .kind(Kind::InboxRelays)
                .author(public_key)
                .limit(1);

            if let Some(event) = client.database().query(filter).await?.into_iter().next() {
                Ok(nip17::extract_relay_list(&event).collect())
            } else {
                Err(anyhow!("Not found."))
            }
        });

        self.tasks.push(cx.spawn_in(window, async move |this, cx| {
            let relays = task.await?;

            this.update(cx, |this, cx| {
                this.inbox.relays.extend(relays);
                cx.notify();
            })?;

            Ok(())
        }));
    }

    fn add(&mut self, window: &mut Window, cx: &mut Context<RelayManager>) {
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

    fn remove(&mut self, url: &RelayUrl, cx: &mut Context<RelayManager>) {
        self.relays.remove(url);
        cx.notify();
    }

    fn publish(&mut self, window: &mut Window, cx: &mut Context<RelayManager>) {
        if self.relays.is_empty() {
            self.set_error("You need to add at least 1 relay", window, cx);
            return;
        }

        let nostr = NostrRegistry::global(cx);
        let client = nostr.read(cx).client();
        let signer = nostr.read(cx).signer();

        let tags: Vec<Tag> = self
            .relays
            .iter()
            .map(|relay| Tag::from(Nip17Tag::Relay(relay.to_owned())))
            .collect();

        self.set_updating(true, cx);

        let task: Task<Result<(), Error>> = cx.background_spawn(async move {
            let event = EventBuilder::new(Kind::InboxRelays, "")
                .tags(tags)
                .finalize_async(&signer)
                .await?;

            // Set messaging relays
            client.send_event(&event).to_nip65().await?;

            Ok(())
        });

        self.tasks.push(cx.spawn_in(window, async move |this, cx| {
            match task.await {
                Ok(_) => {
                    this.update_in(cx, |this, window, cx| {
                        this.inbox.set_updating(false, cx);
                        this.inbox.load(window, cx);

                        window.push_notification("Update successful", cx);
                    })?;
                }
                Err(error) => {
                    this.update_in(cx, |this, window, cx| {
                        this.inbox.set_updating(false, cx);
                        this.inbox.set_error(error.to_string(), window, cx);
                    })?;
                }
            }

            Ok(())
        }));
    }

    fn set_error<E>(&mut self, error: E, window: &mut Window, cx: &mut Context<RelayManager>)
    where
        E: Into<SharedString>,
    {
        self.error = Some(error.into());
        cx.notify();

        self.tasks.push(cx.spawn_in(window, async move |this, cx| {
            cx.background_executor().timer(Duration::from_secs(2)).await;

            // Clear the error message after a delay
            this.update(cx, |this, cx| {
                this.inbox.error = None;
                cx.notify();
            })?;

            Ok(())
        }));
    }

    fn set_updating(&mut self, updating: bool, cx: &mut Context<RelayManager>) {
        self.updating = updating;
        cx.notify();
    }
}

impl Render for RelayManager {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let content = if self.active_tab == 0 {
            self.render_gossip(cx).into_any_element()
        } else {
            self.render_inbox(cx).into_any_element()
        };

        v_flex()
            .gap_4()
            .text_style(TextStyle::Callout)
            .child(self.render_tab_bar(cx))
            .child(content)
            .child(self.render_actions(cx))
    }
}

fn metadata_item(manager: &WeakEntity<RelayManager>, metadata: RelayMetadata) -> PopupMenuItem {
    let manager = manager.clone();
    let label = match metadata {
        RelayMetadata::Read => "Read",
        RelayMetadata::Write => "Write",
    };

    PopupMenuItem::new(label).on_click(move |_, _, cx| {
        if let Err(error) = manager.update(cx, |this, cx| {
            this.gossip.metadata = Some(metadata);
            cx.notify();
        }) {
            log::error!("Failed to set relay metadata: {error}");
        }
    })
}

fn relay_row(
    url: RelayUrl,
    badge: Option<SharedString>,
    on_remove: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
    cx: &App,
) -> Stateful<Div> {
    let label = SharedString::from(url.to_string());

    h_flex()
        .id(label.clone())
        .group("")
        .w_full()
        .h_8()
        .px_2()
        .justify_between()
        .rounded(px(button_radius()))
        .bg(cx.theme().surface)
        .child(
            h_flex()
                .gap_1()
                .text_style(TextStyle::Callout)
                .child(label)
                .when_some(badge, |this, badge| {
                    this.child(
                        div()
                            .p_0p5()
                            .rounded_xs()
                            .text_style(TextStyle::Caption2)
                            .child(badge),
                    )
                }),
        )
        .child(
            Button::new("remove")
                .icon(IconName::Close)
                .xsmall()
                .ghost()
                .invisible()
                .group_hover("", |this| this.visible())
                .on_click(on_remove),
        )
}

fn empty_relays(cx: &App) -> impl IntoElement {
    h_flex()
        .h_20()
        .justify_center()
        .border_2()
        .border_dashed()
        .border_color(cx.theme().border)
        .rounded(px(panel_radius()))
        .text_style(TextStyle::Callout)
        .text_align(TextAlign::Center)
        .child(SharedString::from("Please add some relays."))
}
