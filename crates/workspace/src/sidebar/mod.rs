use std::collections::HashSet;

use anyhow::Error;
use chat::{ChatEvent, ChatRegistry};
use common::DebouncedDelay;
use gpui::prelude::FluentBuilder;
use gpui::{
    AnyElement, App, AppContext, Context, Entity, EventEmitter, FocusHandle, Focusable,
    InteractiveElement, IntoElement, ObjectFit, ParentElement, Render, SharedString, Styled,
    StyledImage, Subscription, Task, UniformListScrollHandle, WeakEntity, Window, div, img,
    retain_all,
};
use instant::Duration;
use nip29::{Group, GroupKey, GroupsEvent, GroupsRegistry};
use nostr_sdk::prelude::*;
use smallvec::{SmallVec, smallvec};
use state::{FIND_DELAY, NostrRegistry, StateEvent};
use theme::{ActiveTheme, TABBAR_HEIGHT};
use ui::dock::{DockArea, Panel, PanelEvent};
use ui::indicator::Indicator;
use ui::input::{InputEvent, InputState};
use ui::notification::Notification;
use ui::{Sizable, StyledExt, WindowExtension, h_flex, title_bar_drag_handlers, v_flex};

mod group_view;
mod header;
mod inbox;
mod list;
mod onboarding;
mod rows;
mod search;

use search::SearchResult;

pub struct Sidebar {
    focus_handle: FocusHandle,
    list_scroll: UniformListScrollHandle,
    dock: WeakEntity<DockArea>,
    /// Random background shown behind the signed-out screen.
    banner: SharedString,

    /// The group whose information the sidebar is browsing, if any.
    group: Option<WeakEntity<Group>>,
    /// Whether the sidebar is showing the group's information instead of its list.
    group_open: bool,
    /// Scroll state of the group's member list.
    group_scroll: UniformListScrollHandle,
    /// Whether the group view's admins section is expanded.
    admins_open: bool,
    /// Whether the group view's members section is expanded.
    members_open: bool,

    /// Whether the main list's pinned section is expanded.
    pins_open: bool,
    /// Whether the user expanded or collapsed the main list's groups section.
    groups_open: Option<bool>,
    /// Same as `groups_open` for the chats section.
    chats_open: Option<bool>,

    /// Whether the main list is showing contacts to start a chat with.
    contacts_open: bool,
    /// The signed-in user's contacts, loaded when the contact picker opens.
    contacts: Option<Vec<PublicKey>>,
    /// Contacts selected in the contact picker.
    selected_contacts: HashSet<PublicKey>,
    /// Scroll state of the contact picker's list.
    contacts_scroll: UniformListScrollHandle,

    /// Whether the main list is showing chat requests instead of chats and groups.
    requests_open: bool,
    new_requests: bool,
    requests_scroll: UniformListScrollHandle,

    /// Whether the search bar is shown below the header.
    search_open: bool,
    /// Whether searches also cover global (relay) profiles.
    global_open: bool,
    /// Debounced delay for the global search while typing.
    find_debouncer: DebouncedDelay<Self>,
    /// Search input state.
    find_input: Entity<InputState>,
    /// Search results: rooms and groups by name, then messages by content.
    results: Vec<SearchResult>,
    /// Whether a global (relay) search is in progress.
    global_finding: bool,
    /// Profiles found by the last global search, if one was triggered.
    global_results: Option<Vec<PublicKey>>,
    /// The async global profile search.
    global_task: Option<Task<()>>,
    /// Scroll state of the search results list.
    search_scroll: UniformListScrollHandle,

    tasks: SmallVec<[Task<Result<(), Error>>; 1]>,
    _subscriptions: SmallVec<[Subscription; 4]>,
}

impl Sidebar {
    pub fn new(window: &mut Window, dock: WeakEntity<DockArea>, cx: &mut Context<Self>) -> Self {
        let chat = ChatRegistry::global(cx);
        let groups = GroupsRegistry::global(cx);
        let nostr = NostrRegistry::global(cx);

        let find_input = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder("Type to search")
                .clean_on_escape()
        });

        let mut subscriptions = smallvec![];

        subscriptions.push(
            cx.subscribe_in(&chat, window, move |this, _s, event, _window, cx| {
                if event == &ChatEvent::Ping {
                    this.new_requests = true;
                    cx.notify();
                };
            }),
        );

        subscriptions.push(
            cx.subscribe_in(&groups, window, |_this, _, event, window, cx| {
                match event {
                    GroupsEvent::Updated => {
                        // TODO: find a better way?
                        cx.notify();
                    }
                    GroupsEvent::Error(error) => {
                        window.push_notification(Notification::error(error.clone()), cx);
                    }
                    _ => {}
                };
            }),
        );

        subscriptions.push(
            cx.subscribe_in(&nostr, window, |this, _, event, window, cx| {
                if let StateEvent::NoSigner = event {
                    this.banner = onboarding::pick_banner();
                    if this.search_open {
                        this.toggle_search(window, cx);
                    }
                    cx.notify();
                }
            }),
        );

        subscriptions.push(cx.subscribe_in(
            &find_input,
            window,
            |this, state, event, window, cx| match event {
                InputEvent::PressEnter { .. } => {
                    this.search(cx);

                    if this.global_open {
                        this.global_search(cx);
                    }
                }
                InputEvent::Change => {
                    if state.read(cx).value().is_empty() {
                        this.clear_results(cx);
                    } else {
                        this.search(cx);

                        if this.global_open {
                            this.find_debouncer.fire_new(
                                Duration::from_millis(FIND_DELAY),
                                window,
                                cx,
                                |this, window, cx| this.debounced_global(window, cx),
                            );
                        }
                    }
                }
                _ => {}
            },
        ));

        Self {
            focus_handle: cx.focus_handle(),
            list_scroll: UniformListScrollHandle::new(),
            dock,
            banner: onboarding::pick_banner(),
            group: None,
            group_open: false,
            group_scroll: UniformListScrollHandle::new(),
            admins_open: true,
            members_open: true,
            pins_open: true,
            groups_open: None,
            chats_open: None,
            contacts_open: false,
            contacts: None,
            selected_contacts: HashSet::new(),
            contacts_scroll: UniformListScrollHandle::new(),
            requests_open: false,
            requests_scroll: UniformListScrollHandle::new(),
            new_requests: false,
            search_open: false,
            global_open: false,
            find_debouncer: DebouncedDelay::new(),
            find_input,
            results: Vec::new(),
            global_finding: false,
            global_results: None,
            global_task: None,
            search_scroll: UniformListScrollHandle::new(),
            tasks: smallvec![],
            _subscriptions: subscriptions,
        }
    }

    /// Show the group view without toggling it closed.
    pub(crate) fn show_group_meta(&mut self, key: &GroupKey, cx: &mut Context<Self>) {
        let groups = GroupsRegistry::global(cx);

        let Some(group) = groups.read(cx).group(key, cx) else {
            return;
        };

        self.show_group(&group, cx);
    }

    /// Hide the group view if the sidebar is showing the given group.
    pub(crate) fn hide_group_meta(&mut self, key: &GroupKey, cx: &mut Context<Self>) {
        let showing = self
            .group
            .as_ref()
            .and_then(|group| group.upgrade())
            .is_some_and(|group| group.read(cx).key() == key);

        if showing {
            self.hide_group(cx);
        }
    }
}

impl Panel for Sidebar {
    fn panel_id(&self) -> SharedString {
        "Sidebar".into()
    }

    fn title(&self, _cx: &App) -> AnyElement {
        SharedString::from("Sidebar").into_any_element()
    }

    fn closable(&self, _cx: &App) -> bool {
        false
    }

    fn zoomable(&self, _cx: &App) -> bool {
        false
    }
}

impl EventEmitter<PanelEvent> for Sidebar {}

impl Focusable for Sidebar {
    fn focus_handle(&self, _: &App) -> gpui::FocusHandle {
        self.focus_handle.clone()
    }
}

impl Render for Sidebar {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let nostr = NostrRegistry::global(cx);
        let current_user = nostr.read(cx).current_user();
        let logged_in = current_user.is_some();

        let chat = ChatRegistry::global(cx);
        let loading = chat.read(cx).loading();

        let group = self.group.clone().and_then(|group| group.upgrade());
        let searching = self.search_open && !self.find_input.read(cx).value().is_empty();

        v_flex()
            .image_cache(retain_all("sidebar"))
            .size_full()
            .relative()
            .gap_2()
            .bg(cx.theme().surface_background)
            .when_some(current_user.as_ref(), |this, current_user| {
                this.child(title_bar_drag_handlers(
                    self.render_user(current_user, cx),
                    window,
                    cx,
                ))
            })
            .when(self.search_open, |this| this.child(self.render_find(cx)))
            .when(!logged_in, |this| {
                this.relative()
                    .child(title_bar_drag_handlers(
                        div()
                            .id("onboarding-drag")
                            .absolute()
                            .top_0()
                            .left_0()
                            .h(TABBAR_HEIGHT)
                            .w_full(),
                        window,
                        cx,
                    ))
                    .child(
                        div().absolute().inset_0().child(
                            img(self.banner.clone())
                                .size_full()
                                .object_fit(ObjectFit::Cover),
                        ),
                    )
                    .child(onboarding::render_signed_out(window, cx))
            })
            .map(|this| match group {
                Some(group) if self.group_open && !searching => {
                    this.child(self.render_group(group, cx))
                }
                _ if searching => this.child(self.render_results(cx)),
                _ => this.child(self.render_list(cx)),
            })
            .when(loading && logged_in, |this| {
                this.child(
                    div()
                        .absolute()
                        .bottom_4()
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
                                .when(cx.theme().shadow, |this| this.shadow_md())
                                .rounded_full()
                                .text_xs()
                                .font_semibold()
                                .text_color(cx.theme().text_muted)
                                .child(Indicator::new().small().color(cx.theme().icon_accent))
                                .child("Getting messages..."),
                        ),
                )
            })
            .into_any_element()
    }
}
