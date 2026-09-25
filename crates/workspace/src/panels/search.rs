use std::collections::HashSet;
use std::ops::Range;

use anyhow::Error;
use chat::{ChatRegistry, Room, RoomKind};
use common::DebouncedDelay;
use gpui::prelude::FluentBuilder;
use gpui::{
    AnyElement, App, AppContext, Context, ElementId, Entity, EventEmitter, FocusHandle, Focusable,
    IntoElement, ParentElement, Render, SharedString, Styled, Subscription, Task, WeakEntity,
    Window, div, uniform_list,
};
use instant::Duration;
use nostr_sdk::prelude::*;
use person::PersonRegistry;
use smallvec::{SmallVec, smallvec};
use state::{FIND_DELAY, NostrRegistry};
use theme::ActiveTheme;
use ui::button::{Button, ButtonVariants};
use ui::dock::{DockArea, DockPlacement, Panel, PanelEvent, PanelHandle};
use ui::input::{Input, InputEvent, InputState};
use ui::nav::Nav;
use ui::notification::Notification;
use ui::{Icon, IconName, Selectable, Sizable, StyledExt, WindowExtension, h_flex, v_flex};

use crate::sidebar::nav_avatar;

const INPUT_PLACEHOLDER: &str = "Find or start a conversation";

pub fn init(dock: WeakEntity<DockArea>, window: &mut Window, cx: &mut App) -> Entity<SearchPanel> {
    cx.new(|cx| SearchPanel::new(dock, window, cx))
}

pub struct SearchPanel {
    name: SharedString,
    focus_handle: FocusHandle,
    /// The dock a started chat opens in
    dock: WeakEntity<DockArea>,

    /// Find input state
    find_input: Entity<InputState>,

    /// Debounced delay for find input
    find_debouncer: DebouncedDelay<Self>,

    /// Whether a search is in progress
    finding: bool,

    /// Find results
    find_results: Entity<Option<Vec<PublicKey>>>,

    /// Async find operation
    find_task: Option<Task<Result<(), Error>>>,

    /// Selected public keys
    selected_pkeys: Entity<HashSet<PublicKey>>,

    /// User's contacts
    contact_list: Entity<Option<Vec<PublicKey>>>,

    /// Async tasks
    tasks: SmallVec<[Task<Result<(), Error>>; 1]>,

    /// Event subscriptions
    _subscriptions: SmallVec<[Subscription; 1]>,
}

impl SearchPanel {
    fn new(dock: WeakEntity<DockArea>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let contact_list = cx.new(|_| None);
        let selected_pkeys = cx.new(|_| HashSet::new());
        let find_results = cx.new(|_| None);
        let find_input = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder(INPUT_PLACEHOLDER)
                .clean_on_escape()
        });

        let mut subscriptions = smallvec![];

        subscriptions.push(
            // Subscribe to find input events
            cx.subscribe_in(&find_input, window, |this, state, event, window, cx| {
                let delay = Duration::from_millis(FIND_DELAY);

                match event {
                    InputEvent::PressEnter { .. } => {
                        this.search(window, cx);
                    }
                    InputEvent::Change => {
                        if state.read(cx).value().is_empty() {
                            // Clear results when input is empty
                            this.reset(window, cx);
                        } else {
                            // Run debounced search
                            this.find_debouncer
                                .fire_new(delay, window, cx, |this, window, cx| {
                                    this.debounced_search(window, cx)
                                });
                        }
                    }
                    InputEvent::Focus => {
                        this.get_contact_list(window, cx);
                    }
                    _ => {}
                };
            }),
        );

        Self {
            name: "Search".into(),
            focus_handle: cx.focus_handle(),
            dock,
            find_input,
            find_debouncer: DebouncedDelay::new(),
            find_results,
            find_task: None,
            finding: false,
            contact_list,
            selected_pkeys,
            tasks: smallvec![],
            _subscriptions: subscriptions,
        }
    }

    /// Get the contact list.
    fn get_contact_list(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let nostr = NostrRegistry::global(cx);
        let client = nostr.read(cx).client();

        let Some(public_key) = nostr.read(cx).current_user() else {
            return;
        };

        let task: Task<Result<HashSet<PublicKey>, Error>> = cx.background_spawn(async move {
            let filter = Filter::new()
                .author(public_key)
                .kind(Kind::ContactList)
                .limit(1);

            let contacts: HashSet<PublicKey> = client
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
            match task.await {
                Ok(contacts) => {
                    this.update(cx, |this, cx| {
                        this.set_contact_list(contacts, cx);
                    })?;
                }
                Err(e) => {
                    cx.update(|window, cx| {
                        window.push_notification(
                            Notification::error(e.to_string()).autohide(false),
                            cx,
                        );
                    })?;
                }
            };

            Ok(())
        }));
    }

    /// Set the contact list with new contacts.
    fn set_contact_list<I>(&mut self, contacts: I, cx: &mut Context<Self>)
    where
        I: IntoIterator<Item = PublicKey>,
    {
        self.contact_list.update(cx, |this, cx| {
            *this = Some(contacts.into_iter().collect());
            cx.notify();
        });
    }

    /// Trigger the debounced search
    fn debounced_search(&self, window: &mut Window, cx: &mut Context<Self>) -> Task<()> {
        cx.spawn_in(window, async move |this, cx| {
            this.update_in(cx, |this, window, cx| {
                this.search(window, cx);
            })
            .ok();
        })
    }

    /// Search
    fn search(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        // Get query
        let query = self.find_input.read(cx).value();

        // Return if the query is empty
        if query.is_empty() {
            return;
        }

        // Block the input until the search completes
        self.set_finding(true, window, cx);

        // Create the search task
        let nostr = NostrRegistry::global(cx);
        let find_users = nostr.read(cx).search(&query, cx);

        // Run task in the main thread
        self.find_task = Some(cx.spawn_in(window, async move |this, cx| {
            let rooms = find_users.await?;

            // Update the UI with the search results
            this.update_in(cx, |this, window, cx| {
                this.set_results(rooms, cx);
                this.set_finding(false, window, cx);
            })?;

            Ok(())
        }));
    }

    /// Set the results of the search
    fn set_results(&mut self, results: Vec<PublicKey>, cx: &mut Context<Self>) {
        self.find_results.update(cx, |this, cx| {
            *this = Some(results);
            cx.notify();
        });
    }

    /// Set the finding status
    fn set_finding(&mut self, status: bool, window: &mut Window, cx: &mut Context<Self>) {
        // Disable the input to prevent duplicate requests
        self.find_input.update(cx, |this, cx| {
            this.set_loading(status, window, cx);
        });
        // Set the search status
        self.finding = status;
        cx.notify();
    }

    fn reset(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        // Clear all search results
        self.find_results.update(cx, |this, cx| {
            *this = None;
            cx.notify();
        });

        // Clear all selected public keys
        self.selected_pkeys.update(cx, |this, cx| {
            this.clear();
            cx.notify();
        });

        // Reset the search status
        self.set_finding(false, window, cx);

        // Cancel the current search task
        self.find_task = None;
        cx.notify();
    }

    /// Select a public key in the search panel.
    fn select(&mut self, public_key: &PublicKey, cx: &mut Context<Self>) {
        self.selected_pkeys.update(cx, |this, cx| {
            if this.contains(public_key) {
                this.remove(public_key);
            } else {
                this.insert(public_key.to_owned());
            }
            cx.notify();
        });
    }

    /// Check if a public key is selected in the search panel.
    fn is_selected(&self, public_key: &PublicKey, cx: &App) -> bool {
        self.selected_pkeys.read(cx).contains(public_key)
    }

    /// Get all selected public keys in the search panel.
    fn get_selected(&self, cx: &Context<Self>) -> HashSet<PublicKey> {
        self.selected_pkeys.read(cx).clone()
    }

    /// Create a new room
    fn create_room(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let chat = ChatRegistry::global(cx);
        let async_chat = chat.downgrade();
        let dock = self.dock.clone();

        let nostr = NostrRegistry::global(cx);
        let Some(public_key) = nostr.read(cx).current_user() else {
            return;
        };

        // Get all selected public keys
        let receivers = self.get_selected(cx);

        self.tasks.push(cx.spawn_in(window, async move |this, cx| {
            // Create a new room and register it
            let room = async_chat.update_in(cx, |chat, _window, cx| {
                let room = cx.new(|_| {
                    Room::new(public_key, receivers)
                        .organize(&public_key)
                        .kind(RoomKind::Ongoing)
                });
                chat.track_room(&room, cx);
                room
            })?;

            // Open it in the dock
            cx.update(|window, cx| {
                ui::dock::add_panel_to(
                    &dock,
                    PanelHandle::new(chat_ui::init(room.downgrade(), window, cx)),
                    DockPlacement::Center,
                    window,
                    cx,
                );
            })?;

            // Reset the find panel
            this.update_in(cx, |this, window, cx| {
                this.reset(window, cx);
            })?;

            Ok(())
        }));
    }

    /// Render the search results
    fn render_results(
        &self,
        range: Range<usize>,
        cx: &Context<Self>,
    ) -> Vec<impl IntoElement + use<>> {
        let persons = PersonRegistry::global(cx);

        // Get the results
        let Some(results) = self.find_results.read(cx) else {
            return vec![];
        };

        // Map the results to a list of elements
        results
            .get(range.clone())
            .into_iter()
            .flatten()
            .enumerate()
            .map(|(ix, public_key)| {
                let selected = self.is_selected(public_key, cx);
                let profile = persons.read(cx).get(public_key, cx);
                let pkey_clone = public_key.to_owned();
                let handler = cx.listener(move |this, _ev, _window, cx| {
                    this.select(&pkey_clone, cx);
                });

                Nav::new(ElementId::NamedInteger(
                    "search-result".into(),
                    (range.start + ix) as u64,
                ))
                .label(profile.name())
                .text_sm()
                .font_medium()
                .when_some(
                    nav_avatar(Some(profile.avatar_seed()), profile.avatar(), cx),
                    |this, avatar| this.prefix(avatar),
                )
                .on_click(handler)
                .selected(selected)
                .into_any_element()
            })
            .collect()
    }

    /// Render the contact list
    fn render_contacts(
        &self,
        range: Range<usize>,
        cx: &Context<Self>,
    ) -> Vec<impl IntoElement + use<>> {
        let persons = PersonRegistry::global(cx);

        // Get the contact list
        let Some(contacts) = self.contact_list.read(cx) else {
            return vec![];
        };

        // Map the contact list to a list of elements
        contacts
            .get(range.clone())
            .into_iter()
            .flatten()
            .enumerate()
            .map(|(ix, public_key)| {
                let selected = self.is_selected(public_key, cx);
                let profile = persons.read(cx).get(public_key, cx);
                let pkey_clone = public_key.to_owned();
                let handler = cx.listener(move |this, _ev, _window, cx| {
                    this.select(&pkey_clone, cx);
                });

                Nav::new(ElementId::NamedInteger(
                    "contact".into(),
                    (range.start + ix) as u64,
                ))
                .label(profile.name().trim())
                .text_sm()
                .font_medium()
                .when_some(
                    nav_avatar(Some(profile.avatar_seed()), profile.avatar(), cx),
                    |this, avatar| this.prefix(avatar),
                )
                .on_click(handler)
                .selected(selected)
                .into_any_element()
            })
            .collect()
    }
}

impl Panel for SearchPanel {
    fn panel_id(&self) -> SharedString {
        self.name.clone()
    }

    fn title(&self, cx: &App) -> AnyElement {
        h_flex()
            .gap_1p5()
            .child(
                Icon::new(IconName::Search)
                    .small()
                    .text_color(cx.theme().icon_muted),
            )
            .child(self.name.clone())
            .into_any_element()
    }
}

impl EventEmitter<PanelEvent> for SearchPanel {}

impl Focusable for SearchPanel {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Render for SearchPanel {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let nostr = NostrRegistry::global(cx);
        let chat = ChatRegistry::global(cx);
        let logged_in = nostr.read(cx).current_user().is_some();
        let loading = chat.read(cx).loading() && logged_in;

        // Set button label based on total selected users
        let button_label = if self.selected_pkeys.read(cx).len() > 1 {
            "Create Group DM"
        } else {
            "Create DM"
        };

        v_flex()
            .size_full()
            .gap_3()
            .p_2()
            .child(
                h_flex().child(
                    Input::new(&self.find_input)
                        .small()
                        .text_xs()
                        .disabled(loading)
                        .when(
                            !self.find_input.read(cx).presentation().is_loading(),
                            |this| {
                                this.suffix(
                                    Button::new("find-icon")
                                        .icon(IconName::Search)
                                        .tooltip("Press Enter to search")
                                        .transparent()
                                        .small(),
                                )
                            },
                        ),
                ),
            )
            .child(
                v_flex()
                    .flex_1()
                    .gap_3()
                    .when_some(self.find_results.read(cx).as_ref(), |this, results| {
                        this.child(
                            v_flex()
                                .gap_1()
                                .flex_1()
                                .border_b_1()
                                .border_color(cx.theme().border_variant)
                                .child(
                                    h_flex()
                                        .gap_0p5()
                                        .text_xs()
                                        .font_semibold()
                                        .text_color(cx.theme().text_muted)
                                        .child(Icon::new(IconName::ChevronDown))
                                        .child("Results"),
                                )
                                .child(
                                    uniform_list(
                                        "rooms",
                                        results.len(),
                                        cx.processor(move |this, range, _window, cx| {
                                            this.render_results(range, cx)
                                        }),
                                    )
                                    .flex_1()
                                    .h_full(),
                                ),
                        )
                    })
                    .when_some(self.contact_list.read(cx).as_ref(), |this, contacts| {
                        this.child(
                            v_flex()
                                .gap_1()
                                .flex_1()
                                .child(
                                    h_flex()
                                        .gap_0p5()
                                        .text_xs()
                                        .font_semibold()
                                        .text_color(cx.theme().text_muted)
                                        .child(Icon::new(IconName::ChevronDown).small())
                                        .child("Contacts"),
                                )
                                .child(
                                    uniform_list(
                                        "contacts",
                                        contacts.len(),
                                        cx.processor(|this, range, _window, cx| {
                                            this.render_contacts(range, cx)
                                        }),
                                    )
                                    .flex_1()
                                    .h_full(),
                                ),
                        )
                    }),
            )
            .when(!self.selected_pkeys.read(cx).is_empty(), |this| {
                this.child(
                    div()
                        .absolute()
                        .bottom_2()
                        .left_0()
                        .h_9()
                        .w_full()
                        .px_4()
                        .child(
                            Button::new("create")
                                .label(button_label)
                                .primary()
                                .rounded()
                                .shadow_md()
                                .on_click(cx.listener(move |this, _ev, window, cx| {
                                    this.create_room(window, cx);
                                })),
                        ),
                )
            })
    }
}
