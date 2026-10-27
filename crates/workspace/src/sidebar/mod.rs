use std::collections::HashSet;

use anyhow::Error;
use chat::{ChatEvent, ChatRegistry};
use common::DebouncedDelay;
use gpui::prelude::FluentBuilder;
use gpui::{
    App, AppContext, Context, Div, Entity, EventEmitter, FocusHandle, Focusable,
    InteractiveElement, IntoElement, ObjectFit, ParentElement, Render, SharedString, Styled,
    StyledImage, Subscription, Task, WeakEntity, Window, div, img, retain_all,
};
use gpui_kit::base::{Presence, Transition};
use gpui_kit::component::dock::{DockArea, Panel, PanelEvent};
use gpui_kit::component::input::{InputEvent, InputState};
use gpui_kit::component::notification::Notification;
use gpui_kit::component::progress::ProgressCircle;
use gpui_kit::component::{ActiveTheme, WindowExt, h_flex, v_flex};
use instant::Duration;
use nip29::{Group, GroupKey, GroupsEvent, GroupsRegistry};
use nostr_sdk::prelude::*;
use person::{Directory, PersonRegistry};
pub(crate) use rows::open_room;
use search::SearchResult;
use smallvec::{SmallVec, smallvec};
use state::{FIND_DELAY, NostrRegistry, StateEvent};
use ui::title_bar_drag_handlers;

use crate::dock::TAB_BAR_HEIGHT;

mod group_view;
mod header;
mod inbox;
mod list;
mod onboarding;
mod rows;
mod search;

const PROGRESS_TICK_MS: u64 = 500;
const PROGRESS_CAP: f32 = 95.0;

pub struct Sidebar {
    focus_handle: FocusHandle,
    dock: WeakEntity<DockArea>,
    /// Random background shown behind the signed-out screen.
    banner: SharedString,

    /// The group whose information the sidebar is browsing, if any.
    group: Option<WeakEntity<Group>>,
    /// Whether the sidebar is showing the group's information instead of its list.
    group_open: bool,
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
    /// Person directory for contacts, search hits, and group members
    directory: Entity<Directory>,
    /// Contacts selected in the contact picker.
    selected_contacts: HashSet<PublicKey>,

    /// Whether the main list is showing chat requests instead of chats and groups.
    requests_open: bool,
    new_requests: bool,

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

    /// Simulated gift wrap processing progress, shown while the backlog loads
    chat_progress: f32,
    /// Simulated web of trust sync progress
    sync_progress: f32,

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

        let mut tasks = smallvec![];
        let mut subscriptions = smallvec![];

        subscriptions.push(
            cx.subscribe_in(&chat, window, move |this, _, event, _, cx| {
                match event {
                    ChatEvent::Ping => {
                        this.new_requests = true;
                        cx.notify();
                    }
                    ChatEvent::Loaded => {
                        this.chat_progress = 0.0;
                        cx.notify();
                    }
                    _ => {}
                };
            }),
        );

        subscriptions.push(
            cx.subscribe_in(&groups, window, |_this, _, event, window, cx| {
                match event {
                    GroupsEvent::Updated => {
                        // TODO: we must not trigger re-render directly like this
                        cx.notify();
                    }
                    GroupsEvent::Error(error) => {
                        window.push_notification(Notification::error(error.clone()), cx);
                    }
                    GroupsEvent::Created {
                        warning: Some(warning),
                        ..
                    } => {
                        window.push_notification(Notification::warning(warning.clone()), cx);
                    }
                    _ => {}
                };
            }),
        );

        subscriptions.push(
            cx.subscribe_in(
                &nostr,
                window,
                |this, state, event, window, cx| match event {
                    StateEvent::NoSigner => {
                        this.banner = onboarding::pick_banner();
                        if this.search_open {
                            this.toggle_search(window, cx);
                        }
                        cx.notify();
                    }
                    StateEvent::SignerChanged => {
                        if let Some(current_user) = state.read(cx).current_user() {
                            this.directory.update(cx, |directory, cx| {
                                directory.track([current_user], cx);
                            });
                        }
                    }
                    _ => {}
                },
            ),
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

        tasks.push(cx.spawn(async move |this, cx| {
            let timer = Duration::from_millis(PROGRESS_TICK_MS);
            loop {
                cx.background_executor().timer(timer).await;
                this.update(cx, |this, cx| {
                    this.tick_progress(cx);
                })?;
            }
        }));

        Self {
            focus_handle: cx.focus_handle(),
            dock,
            banner: onboarding::pick_banner(),
            group: None,
            group_open: false,
            admins_open: true,
            members_open: true,
            pins_open: true,
            groups_open: None,
            chats_open: None,
            contacts_open: false,
            contacts: None,
            directory: Directory::new(cx),
            selected_contacts: HashSet::new(),
            requests_open: false,
            new_requests: false,
            search_open: false,
            global_open: false,
            find_debouncer: DebouncedDelay::new(),
            find_input,
            results: Vec::new(),
            global_finding: false,
            global_results: None,
            global_task: None,
            chat_progress: 0.0,
            sync_progress: 0.0,
            tasks,
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

    /// Advance the simulated loading progress of any active background work.
    fn tick_progress(&mut self, cx: &mut Context<Self>) {
        let logged_in = NostrRegistry::global(cx).read(cx).current_user().is_some();
        let chat = ChatRegistry::global(cx);
        let wot = PersonRegistry::global(cx).read(cx).wot();

        let chat_loading = logged_in && chat.read(cx).loading();
        let syncing = logged_in && wot.read(cx).is_syncing();

        let mut changed = false;

        if chat_loading {
            let next = self.advance_progress(self.chat_progress);
            if next > self.chat_progress {
                self.chat_progress = next;
                changed = true;
            }
        } else if self.chat_progress != 0.0 {
            self.chat_progress = 0.0;
            changed = true;
        }

        if syncing {
            let next = self.advance_progress(self.sync_progress);
            if next > self.sync_progress {
                self.sync_progress = next;
                changed = true;
            }
        } else if self.sync_progress != 0.0 {
            self.sync_progress = 0.0;
            changed = true;
        }

        if changed {
            cx.notify();
        }
    }

    /// Bump simulated progress by a random step
    fn advance_progress(&self, progress: f32) -> f32 {
        (progress + rand::random::<f32>() * 8.0 + 1.0).min(PROGRESS_CAP)
    }

    /// A floating pill showing simulated progress for a background operation.
    fn render_loading_pill(
        &self,
        id: &'static str,
        label: &'static str,
        progress: f32,
        opacity: f32,
        cx: &Context<Self>,
    ) -> Div {
        h_flex()
            .opacity(opacity)
            .h_9()
            .w_full()
            .px_5()
            .gap_2p5()
            .border_t_1()
            .border_color(cx.theme().border)
            .child(
                ProgressCircle::new(id)
                    .value(progress)
                    .color(cx.theme().primary)
                    .accessibility_label(label),
            )
            .child(
                div()
                    .text_xs()
                    .text_color(cx.theme().sidebar_foreground)
                    .child(label),
            )
    }
}

impl gpui_kit::component::dock::BasePanel for Sidebar {
    fn panel_name(&self) -> &'static str {
        "Sidebar"
    }

    fn closable(&self, _cx: &App) -> bool {
        false
    }

    fn zoomable(&self, _cx: &App) -> bool {
        false
    }
}

impl Panel for Sidebar {
    fn title(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        SharedString::from("Sidebar")
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

        let wot = PersonRegistry::global(cx).read(cx).wot();
        let syncing = wot.read(cx).is_syncing();

        let group = self.group.clone().and_then(|group| group.upgrade());
        let searching = self.search_open && !self.find_input.read(cx).value().is_empty();
        let picking_contacts = self.contacts_open && !self.requests_open;

        v_flex()
            .image_cache(retain_all("sidebar"))
            .size_full()
            .relative()
            .justify_between()
            .bg(cx.theme().sidebar)
            .text_color(cx.theme().sidebar_foreground)
            .child(
                v_flex()
                    .flex_1()
                    .min_h_0()
                    .when_some(current_user.as_ref(), |this, current_user| {
                        this.child(title_bar_drag_handlers(
                            self.render_user(current_user, cx),
                            window,
                            cx,
                        ))
                    })
                    .when(self.search_open, |this| this.child(self.render_find(cx)))
                    .when(!logged_in, |this| {
                        this.child(title_bar_drag_handlers(
                            div()
                                .id("onboarding-drag")
                                .absolute()
                                .top_0()
                                .left_0()
                                .h(TAB_BAR_HEIGHT)
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
                    }),
            )
            .when(logged_in && !picking_contacts, |this| {
                this.child(self.render_footer(loading || syncing, cx))
            })
            .when(logged_in, |this| {
                let pill_motion = Transition::new(cx.theme().motion_tokens().duration_normal)
                    .easing(cx.theme().motion_tokens().easing_move.clone());

                let chat_presence = Presence::new("chat-presence", loading)
                    .transition(pill_motion.clone())
                    .sample(window, cx);

                let wot_presence = Presence::new("wot-presence", syncing)
                    .transition(pill_motion)
                    .sample(window, cx);

                this.child(
                    v_flex()
                        .absolute()
                        .bottom_0()
                        .left_0()
                        .w_full()
                        .bg(cx.theme().sidebar)
                        .text_color(cx.theme().sidebar_foreground)
                        .children(chat_presence.should_render().then(|| {
                            self.render_loading_pill(
                                "chat-sync",
                                "Syncing messages",
                                self.chat_progress,
                                chat_presence.progress,
                                cx,
                            )
                        }))
                        .children(wot_presence.should_render().then(|| {
                            self.render_loading_pill(
                                "wot-sync",
                                "Syncing contacts",
                                self.sync_progress,
                                wot_presence.progress,
                                cx,
                            )
                        })),
                )
            })
            .into_any_element()
    }
}
