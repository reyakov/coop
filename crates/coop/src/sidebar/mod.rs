use std::collections::HashSet;
use std::ops::Range;
use std::time::Duration;

use anyhow::{Context as AnyhowContext, Error};
use chat::{ChatEvent, ChatRegistry, Room, RoomKind};
use common::{DebouncedDelay, RenderedTimestamp};
use entry::RoomEntry;
use gpui::prelude::FluentBuilder;
use gpui::{
    div, uniform_list, App, AppContext, Context, Entity, EventEmitter, FocusHandle, Focusable,
    IntoElement, ParentElement, Render, RetainAllImageCache, SharedString, Styled, Subscription,
    Task, UniformListScrollHandle, Window,
};
use nostr_sdk::prelude::*;
use person::PersonRegistry;
use smallvec::{smallvec, SmallVec};
use state::{NostrRegistry, FIND_DELAY};
use theme::{ActiveTheme, TITLEBAR_HEIGHT};
use ui::button::{Button, ButtonVariants};
use ui::dock_area::panel::{Panel, PanelEvent};
use ui::indicator::Indicator;
use ui::input::{InputEvent, InputState, TextInput};
use ui::notification::Notification;
use ui::scroll::Scrollbar;
use ui::{h_flex, v_flex, Icon, IconName, Selectable, Sizable, StyledExt, WindowExtension};

mod entry;

const INPUT_PLACEHOLDER: &str = "Find or start a conversation";

pub fn init(window: &mut Window, cx: &mut App) -> Entity<Sidebar> {
    cx.new(|cx| Sidebar::new(window, cx))
}

/// Sidebar.
pub struct Sidebar {
    name: SharedString,
    focus_handle: FocusHandle,
    scroll_handle: UniformListScrollHandle,

    /// Image cache
    image_cache: Entity<RetainAllImageCache>,

    /// Find input state
    find_input: Entity<InputState>,

    /// Debounced delay for find input
    find_debouncer: DebouncedDelay<Self>,

    /// Whether a search is in progress
    finding: bool,

    /// Whether the find input is focused
    find_focused: bool,

    /// Find results
    find_results: Entity<Option<Vec<PublicKey>>>,

    /// Async find operation
    find_task: Option<Task<Result<(), Error>>>,

    /// Whether there are search results
    has_search: bool,

    /// Whether there are new chat requests
    new_requests: bool,

    /// Selected public keys
    selected_pkeys: Entity<HashSet<PublicKey>>,

    /// Chatroom filter
    filter: Entity<RoomKind>,

    /// User's contacts
    contact_list: Entity<Option<Vec<PublicKey>>>,

    /// Async tasks
    tasks: SmallVec<[Task<Result<(), Error>>; 1]>,

    /// Event subscriptions
    _subscriptions: SmallVec<[Subscription; 1]>,
}

impl Sidebar {
    fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let chat = ChatRegistry::global(cx);
        let filter = cx.new(|_| RoomKind::Ongoing);
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
                        this.set_input_focus(true, window, cx);
                        this.get_contact_list(window, cx);
                    }
                    _ => {}
                };
            }),
        );

        subscriptions.push(
            // Subscribe for registry new events
            cx.subscribe_in(&chat, window, move |this, _s, event, _window, cx| {
                if event == &ChatEvent::Ping {
                    this.new_requests = true;
                    cx.notify();
                };
            }),
        );

        Self {
            name: "Sidebar".into(),
            focus_handle: cx.focus_handle(),
            scroll_handle: UniformListScrollHandle::new(),
            image_cache: RetainAllImageCache::new(cx),
            find_input,
            find_debouncer: DebouncedDelay::new(),
            find_results,
            find_task: None,
            find_focused: false,
            finding: false,
            has_search: false,
            new_requests: false,
            contact_list,
            selected_pkeys,
            filter,
            tasks: smallvec![],
            _subscriptions: subscriptions,
        }
    }

    /// Get the contact list.
    fn get_contact_list(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let nostr = NostrRegistry::global(cx);
        let client = nostr.read(cx).client();

        let task: Task<Result<HashSet<PublicKey>, Error>> = cx.background_spawn(async move {
            let signer = client.signer().context("Signer not found")?;
            let public_key = signer.get_public_key().await?;
            let contacts = client.database().contacts_public_keys(public_key).await?;

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
                        window.push_notification(Notification::error(e.to_string()), cx);
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
    fn set_finding(&mut self, status: bool, _window: &mut Window, cx: &mut Context<Self>) {
        // Disable the input to prevent duplicate requests
        self.find_input.update(cx, |this, cx| {
            this.set_disabled(status, cx);
            this.set_loading(status, cx);
        });
        // Set the search status
        self.finding = status;
        cx.notify();
    }

    /// Set the focus status of the input element.
    fn set_input_focus(&mut self, status: bool, window: &mut Window, cx: &mut Context<Self>) {
        self.find_focused = status;
        cx.notify();

        // Focus to the input element
        if !status {
            window.focus_prev(cx);
        }
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

    /// Select a public key in the sidebar.
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

    /// Check if a public key is selected in the sidebar.
    fn is_selected(&self, public_key: &PublicKey, cx: &App) -> bool {
        self.selected_pkeys.read(cx).contains(public_key)
    }

    /// Get all selected public keys in the sidebar.
    fn get_selected(&self, cx: &Context<Self>) -> HashSet<PublicKey> {
        self.selected_pkeys.read(cx).clone()
    }

    /// Create a new room
    fn create_room(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let chat = ChatRegistry::global(cx);
        let async_chat = chat.downgrade();

        let nostr = NostrRegistry::global(cx);
        let signer = nostr.read(cx).signer();

        // Get all selected public keys
        let receivers = self.get_selected(cx);

        self.tasks.push(cx.spawn_in(window, async move |this, cx| {
            let public_key = signer.get_public_key().await?;

            // Create a new room and emit it
            async_chat.update_in(cx, |this, _window, cx| {
                let room = cx.new(|_| {
                    Room::new(public_key, receivers)
                        .organize(&public_key)
                        .kind(RoomKind::Ongoing)
                });
                this.emit_room(&room, cx);
            })?;

            // Reset the find panel
            this.update_in(cx, |this, window, cx| {
                this.reset(window, cx);
            })?;

            Ok(())
        }));
    }

    /// Get the active filter.
    fn current_filter(&self, kind: &RoomKind, cx: &Context<Self>) -> bool {
        self.filter.read(cx) == kind
    }

    /// Set the active filter for the sidebar.
    fn set_filter(&mut self, kind: RoomKind, window: &mut Window, cx: &mut Context<Self>) {
        self.set_input_focus(false, window, cx);
        self.filter.update(cx, |this, cx| {
            *this = kind;
            cx.notify();
        });
        self.new_requests = false;
    }

    fn render_list_items(&self, range: Range<usize>, cx: &Context<Self>) -> Vec<impl IntoElement> {
        let chat = ChatRegistry::global(cx);
        let rooms = chat.read(cx).rooms(self.filter.read(cx), cx);

        rooms
            .get(range.clone())
            .into_iter()
            .flatten()
            .enumerate()
            .map(|(ix, item)| {
                let room = item.read(cx);
                let room_clone = item.clone();
                let public_key = room.display_member(cx).public_key();
                let handler = cx.listener(move |_this, _ev, _window, cx| {
                    ChatRegistry::global(cx).update(cx, |s, cx| {
                        s.emit_room(&room_clone, cx);
                    });
                });

                RoomEntry::new(range.start + ix)
                    .name(room.display_name(cx))
                    .avatar(room.display_image(cx))
                    .public_key(public_key)
                    .kind(room.kind)
                    .created_at(room.created_at.to_ago())
                    .on_click(handler)
                    .into_any_element()
            })
            .collect()
    }

    /// Render the contact list
    fn render_results(&self, range: Range<usize>, cx: &Context<Self>) -> Vec<impl IntoElement> {
        let persons = PersonRegistry::global(cx);

        // Get the contact list
        let Some(results) = self.find_results.read(cx) else {
            return vec![];
        };

        // Map the contact list to a list of elements
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

                RoomEntry::new(range.start + ix)
                    .name(profile.name())
                    .avatar(profile.avatar())
                    .on_click(handler)
                    .selected(selected)
                    .into_any_element()
            })
            .collect()
    }

    /// Render the contact list
    fn render_contacts(&self, range: Range<usize>, cx: &Context<Self>) -> Vec<impl IntoElement> {
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

                RoomEntry::new(range.start + ix)
                    .name(profile.name())
                    .avatar(profile.avatar())
                    .on_click(handler)
                    .selected(selected)
                    .into_any_element()
            })
            .collect()
    }
}

impl Panel for Sidebar {
    fn panel_id(&self) -> SharedString {
        self.name.clone()
    }
}

impl EventEmitter<PanelEvent> for Sidebar {}

impl Focusable for Sidebar {
    fn focus_handle(&self, _: &App) -> gpui::FocusHandle {
        self.focus_handle.clone()
    }
}

impl Render for Sidebar {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let chat = ChatRegistry::global(cx);
        let loading = chat.read(cx).loading();
        let total_rooms = chat.read(cx).count(self.filter.read(cx), cx);

        // Whether the find panel should be shown
        let show_find_panel = self.has_search || self.find_focused;

        // Set button label based on total selected users
        let button_label = if self.selected_pkeys.read(cx).len() > 1 {
            "Create Group DM"
        } else {
            "Create DM"
        };

        v_flex()
            .image_cache(self.image_cache.clone())
            .size_full()
            .gap_2()
            .child(
                h_flex()
                    .h(TITLEBAR_HEIGHT)
                    .border_b_1()
                    .border_color(cx.theme().border_variant)
                    .bg(cx.theme().elevated_surface_background)
                    .child(
                        TextInput::new(&self.find_input)
                            .appearance(false)
                            .bordered(false)
                            .small()
                            .text_xs()
                            .when(!self.find_input.read(cx).loading, |this| {
                                this.suffix(
                                    Button::new("find-icon")
                                        .icon(IconName::Search)
                                        .tooltip("Press Enter to search")
                                        .transparent()
                                        .small(),
                                )
                            }),
                    ),
            )
            .child(
                h_flex()
                    .px_2()
                    .gap_2()
                    .justify_center()
                    .when(show_find_panel, |this| {
                        this.child(
                            Button::new("search-results")
                                .icon(IconName::Search)
                                .tooltip("All search results")
                                .small()
                                .ghost_alt()
                                .font_semibold()
                                .flex_1()
                                .selected(true),
                        )
                    })
                    .child(
                        Button::new("all")
                            .map(|this| {
                                if self.current_filter(&RoomKind::Ongoing, cx) {
                                    this.icon(IconName::InboxFill)
                                } else {
                                    this.icon(IconName::Inbox)
                                }
                            })
                            .when(!show_find_panel, |this| this.label("Inbox"))
                            .tooltip("All ongoing conversations")
                            .small()
                            .ghost_alt()
                            .font_semibold()
                            .flex_1()
                            .selected(
                                !show_find_panel && self.current_filter(&RoomKind::Ongoing, cx),
                            )
                            .on_click(cx.listener(|this, _ev, window, cx| {
                                this.set_filter(RoomKind::Ongoing, window, cx);
                            })),
                    )
                    .child(
                        Button::new("requests")
                            .map(|this| {
                                if self.current_filter(&RoomKind::Request, cx) {
                                    this.icon(IconName::FistbumpFill)
                                } else {
                                    this.icon(IconName::Fistbump)
                                }
                            })
                            .when(!show_find_panel, |this| this.label("Requests"))
                            .tooltip("Incoming new conversations")
                            .small()
                            .ghost_alt()
                            .font_semibold()
                            .flex_1()
                            .selected(
                                !show_find_panel && !self.current_filter(&RoomKind::Ongoing, cx),
                            )
                            .when(self.new_requests, |this| {
                                this.child(div().size_1().rounded_full().bg(cx.theme().cursor))
                            })
                            .on_click(cx.listener(|this, _ev, window, cx| {
                                this.set_filter(RoomKind::default(), window, cx);
                            })),
                    ),
            )
            .when(!show_find_panel && !loading && total_rooms == 0, |this| {
                this.child(
                    div().px_2().child(
                        v_flex()
                            .p_3()
                            .h_24()
                            .border_2()
                            .border_dashed()
                            .border_color(cx.theme().border_variant)
                            .rounded(cx.theme().radius_lg)
                            .items_center()
                            .justify_center()
                            .text_center()
                            .child(
                                div()
                                    .text_sm()
                                    .font_semibold()
                                    .child(SharedString::from("No conversations")),
                            )
                            .child(div().text_xs().text_color(cx.theme().text_muted).child(
                                SharedString::from(
                                    "Start a conversation with someone to get started.",
                                ),
                            )),
                    ),
                )
            })
            .child(
                v_flex()
                    .h_full()
                    .px_1p5()
                    .gap_1()
                    .flex_1()
                    .overflow_y_hidden()
                    .when(show_find_panel, |this| {
                        this.gap_3()
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
                                                .child(SharedString::from("Results")),
                                        )
                                        .child(
                                            uniform_list(
                                                "rooms",
                                                results.len(),
                                                cx.processor(|this, range, _window, cx| {
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
                                                .child(Icon::new(IconName::ChevronDown))
                                                .child(SharedString::from("Suggestions")),
                                        )
                                        .child(
                                            uniform_list(
                                                "contacts",
                                                contacts.len(),
                                                cx.processor(move |this, range, _window, cx| {
                                                    this.render_contacts(range, cx)
                                                }),
                                            )
                                            .flex_1()
                                            .h_full(),
                                        ),
                                )
                            })
                    })
                    .when(!show_find_panel, |this| {
                        this.child(
                            uniform_list(
                                "rooms",
                                total_rooms,
                                cx.processor(|this, range, _window, cx| {
                                    this.render_list_items(range, cx)
                                }),
                            )
                            .track_scroll(&self.scroll_handle)
                            .flex_1()
                            .h_full(),
                        )
                        .child(Scrollbar::vertical(&self.scroll_handle))
                    }),
            )
            .when(!self.selected_pkeys.read(cx).is_empty(), |this| {
                this.child(
                    div()
                        .absolute()
                        .bottom_0()
                        .left_0()
                        .h_9()
                        .w_full()
                        .px_2()
                        .child(
                            Button::new("create")
                                .label(button_label)
                                .primary()
                                .small()
                                .shadow_lg()
                                .on_click(cx.listener(move |this, _ev, window, cx| {
                                    this.create_room(window, cx);
                                })),
                        ),
                )
            })
            .when(loading, |this| {
                this.child(
                    div()
                        .absolute()
                        .bottom_2()
                        .left_0()
                        .h_9()
                        .w_full()
                        .px_8()
                        .child(
                            h_flex()
                                .gap_2()
                                .w_full()
                                .h_9()
                                .justify_center()
                                .bg(cx.theme().background.opacity(0.85))
                                .border_color(cx.theme().border_disabled)
                                .border_1()
                                .when(cx.theme().shadow, |this| this.shadow_xs())
                                .rounded_full()
                                .text_xs()
                                .font_semibold()
                                .text_color(cx.theme().text_muted)
                                .child(Indicator::new().small().color(cx.theme().icon_accent))
                                .child(SharedString::from("Getting messages...")),
                        ),
                )
            })
    }
}
