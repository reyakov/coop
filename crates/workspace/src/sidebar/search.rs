use std::collections::HashSet;
use std::rc::Rc;

use chat::{ChatRegistry, Room};
use gpui::{
    AnyElement, App, Context, Entity, IntoElement, ParentElement, SharedString, Styled, Task,
    Window, div, px, uniform_list,
};
use nip29::{Group, GroupsRegistry};
use nostr_sdk::prelude::*;
use state::{FIND_LIMIT, NostrRegistry};
use theme::{ActiveTheme, button_radius};
use ui::button::{Button, ButtonVariants};
use ui::input::Input;
use ui::scroll::Scrollbar;
use ui::{IconName, Selectable, Sizable, h_flex, v_flex};

use super::Sidebar;
use super::rows::SidebarRow;

/// An entry in the sidebar's search results.
pub(super) enum SearchResult {
    Room(Entity<Room>),
    Group(Entity<Group>),
    Message(MessageHit),
}

/// A message found by searching the indexed chat history.
#[derive(Clone)]
pub(super) struct MessageHit {
    pub(super) room: Entity<Room>,
    pub(super) author: PublicKey,
    pub(super) content: SharedString,
    pub(super) created_at: Timestamp,
}

/// Find groups whose display name contains all of the terms.
fn find_groups(terms: &[String], cx: &App) -> Vec<Entity<Group>> {
    GroupsRegistry::global(cx)
        .read(cx)
        .groups()
        .iter()
        .filter(|group| {
            let name = group.read(cx).display_name().to_lowercase();
            terms.iter().all(|term| name.contains(term.as_str()))
        })
        .cloned()
        .collect()
}

impl Sidebar {
    /// Show or hide the search bar below the header.
    pub(super) fn toggle_search(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.search_open = !self.search_open;

        if self.search_open {
            self.find_input
                .update(cx, |input, cx| input.focus(window, cx));
        } else {
            self.find_input.update(cx, |input, cx| {
                input.set_value("", window, cx);
            });
            self.global_open = false;
            self.clear_results(cx);
        }

        cx.notify();
    }

    /// Search rooms and groups by name, then indexed messages by content.
    pub(super) fn search(&mut self, cx: &mut Context<Self>) {
        let query = self.find_input.read(cx).value().to_string();
        let terms: Vec<String> = query
            .to_lowercase()
            .split_whitespace()
            .map(str::to_owned)
            .collect();

        if terms.is_empty() {
            return;
        }

        let chat = ChatRegistry::global(cx);
        let rooms = chat.read(cx).find(&query, cx);
        let groups = find_groups(&terms, cx);

        let messages = chat
            .read(cx)
            .search_messages(&terms, FIND_LIMIT)
            .into_iter()
            .filter_map(|matched| {
                let room = chat.read(cx).room(&matched.room, cx)?.upgrade()?;

                Some(MessageHit {
                    room,
                    author: matched.author,
                    content: matched.content.into(),
                    created_at: matched.created_at,
                })
            })
            .collect::<Vec<_>>();

        self.results = rooms
            .into_iter()
            .map(SearchResult::Room)
            .chain(groups.into_iter().map(SearchResult::Group))
            .chain(messages.into_iter().map(SearchResult::Message))
            .collect();
        cx.notify();
    }

    /// Search profiles on the indexer relays for the current query.
    pub(super) fn global_search(&mut self, cx: &mut Context<Self>) {
        if !self.global_open {
            return;
        }

        let query = self.find_input.read(cx).value().trim().to_string();

        if query.is_empty() {
            return;
        }

        let nostr = NostrRegistry::global(cx);
        let find_profiles = nostr.read(cx).search(&query, cx);

        self.global_finding = true;
        self.global_results = None;
        cx.notify();

        self.global_task = Some(cx.spawn(async move |this, cx| {
            let result = find_profiles.await;

            this.update(cx, |this, cx| {
                this.global_finding = false;
                this.global_results = Some(match result {
                    Ok(profiles) => {
                        // Relays may return the same profile more than once
                        let mut seen = HashSet::new();
                        profiles
                            .into_iter()
                            .filter(|key| seen.insert(*key))
                            .collect()
                    }
                    Err(error) => {
                        log::debug!("Global search failed: {error}");
                        Vec::new()
                    }
                });
                cx.notify();
            })
            .ok();
        }));
    }

    /// Trigger a global search once the input debounce delay has passed.
    pub(super) fn debounced_global(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Task<()> {
        cx.spawn_in(window, async move |this, cx| {
            this.update_in(cx, |this, _window, cx| {
                this.global_search(cx);
            })
            .ok();
        })
    }

    /// Toggle whether searches also cover global (relay) profiles.
    pub(super) fn toggle_global(&mut self, cx: &mut Context<Self>) {
        self.global_open = !self.global_open;

        if self.global_open {
            self.global_search(cx);
        } else {
            self.global_finding = false;
            self.global_results = None;
            self.global_task = None;
        }

        cx.notify();
    }

    /// Discard the search results.
    pub(super) fn clear_results(&mut self, cx: &mut Context<Self>) {
        self.results.clear();
        self.global_finding = false;
        self.global_results = None;
        self.global_task = None;
        cx.notify();
    }

    pub(super) fn render_find(&self, cx: &mut Context<Self>) -> AnyElement {
        h_flex()
            .px_2()
            .flex_shrink_0()
            .w_full()
            .child(
                h_flex()
                    .px_1p5()
                    .gap_1()
                    .w_full()
                    .rounded(px(button_radius()))
                    .bg(cx.theme().surface_raised)
                    .child(Input::new(&self.find_input).appearance(false).small())
                    .child(
                        Button::new("sidebar-global")
                            .icon(IconName::Compass)
                            .tooltip(if self.global_open {
                                "Disable global search"
                            } else {
                                "Enable global search"
                            })
                            .small()
                            .ghost()
                            .selected(self.global_open)
                            .loading(self.global_finding)
                            .on_click(cx.listener(|this, _event, _window, cx| {
                                this.toggle_global(cx);
                            })),
                    ),
            )
            .into_any_element()
    }

    pub(super) fn render_results(&self, cx: &mut Context<Self>) -> AnyElement {
        let rows = Rc::new(self.results_for());

        v_flex()
            .flex_1()
            .min_h_0()
            .relative()
            .child(
                div()
                    .min_h_0()
                    .flex_1()
                    .child(
                        uniform_list(
                            "sidebar-search",
                            rows.len(),
                            cx.processor(move |this, range, _window, cx| {
                                this.render_rows(range, rows.as_slice(), cx)
                            }),
                        )
                        .track_scroll(&self.search_scroll)
                        .h_full()
                        .px_2(),
                    )
                    .child(Scrollbar::vertical(&self.search_scroll)),
            )
            .into_any_element()
    }

    fn results_for(&self) -> Vec<SidebarRow> {
        let mut rooms = Vec::new();
        let mut groups = Vec::new();
        let mut messages = Vec::new();

        for result in &self.results {
            match result {
                SearchResult::Room(room) => rooms.push(room.clone()),
                SearchResult::Group(group) => groups.push(group.clone()),
                SearchResult::Message(hit) => messages.push(hit.clone()),
            }
        }

        let mut rows = Vec::new();

        rows.push(SidebarRow::Heading("Groups"));
        if groups.is_empty() {
            rows.push(SidebarRow::Empty("No groups found"));
        } else {
            rows.extend(groups.into_iter().map(|group| SidebarRow::Group { group }));
        }

        rows.push(SidebarRow::Heading("Rooms"));
        if rooms.is_empty() {
            rows.push(SidebarRow::Empty("No rooms found"));
        } else {
            rows.extend(rooms.into_iter().map(|room| SidebarRow::Room { room }));
        }

        rows.push(SidebarRow::Heading("Messages"));
        if messages.is_empty() {
            rows.push(SidebarRow::Empty("No messages found"));
        } else {
            rows.extend(messages.into_iter().map(|hit| SidebarRow::Message { hit }));
        }

        // Global profiles only appear once a global search was triggered
        if self.global_finding || self.global_results.is_some() {
            rows.push(SidebarRow::Heading("Global"));

            match self.global_results.as_ref() {
                Some(profiles) if profiles.is_empty() => {
                    rows.push(SidebarRow::Empty("No profiles found"));
                }
                Some(profiles) => {
                    rows.extend(profiles.iter().map(|public_key| SidebarRow::Profile {
                        public_key: *public_key,
                    }));
                }
                None => rows.push(SidebarRow::Empty("Searching...")),
            }
        }

        rows
    }
}
