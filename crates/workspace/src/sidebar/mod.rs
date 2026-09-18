use std::collections::BTreeSet;
use std::ops::Range;
use std::rc::Rc;

use auto_update::AutoUpdater;
use chat::{ChatEvent, ChatRegistry, Room, RoomKind};
use common::TimestampExt;
use gpui::prelude::FluentBuilder;
use gpui::{
    AnyElement, App, Context, ElementId, Entity, EventEmitter, FocusHandle, Focusable,
    InteractiveElement, IntoElement, ParentElement, Render, SharedString, Styled, Subscription,
    UniformListScrollHandle, Window, div, px, retain_all, uniform_list,
};
use person::PersonRegistry;
use smallvec::{SmallVec, smallvec};
use state::NostrRegistry;
use theme::{ActiveTheme, TABBAR_HEIGHT};
use ui::avatar::Avatar;
use ui::button::{Button, ButtonVariants};
use ui::dock::{Panel, PanelEvent};
use ui::indicator::Indicator;
use ui::menu::{DropdownMenu, PopupMenuItem};
use ui::scroll::Scrollbar;
use ui::{
    IconName, Sizable, StyledExt, TRAFFIC_LIGHT_PADDING, h_flex, title_bar_drag_handlers, v_flex,
};

use crate::Command;

mod entry;
mod tree;

use entry::ROOM_ENTRY_GROUP;
pub(crate) use entry::RoomEntry;
use tree::{SidebarRow, TreeRow, TreeRowKind, TreeSection, dummy_communities};

/// Sidebar.
pub struct Sidebar {
    focus_handle: FocusHandle,
    scroll_handle: UniformListScrollHandle,

    /// Whether there are new chat requests
    new_requests: bool,

    /// Expanded tree sections
    expanded: BTreeSet<TreeSection>,

    /// Pinned room ids, in pin order
    pinned_rooms: Vec<u64>,

    /// Event subscriptions
    _subscriptions: SmallVec<[Subscription; 1]>,
}

impl Sidebar {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let chat = ChatRegistry::global(cx);

        let mut subscriptions = smallvec![];

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
            focus_handle: cx.focus_handle(),
            scroll_handle: UniformListScrollHandle::new(),
            new_requests: false,
            expanded: BTreeSet::from([TreeSection::Community, TreeSection::Messages]),
            pinned_rooms: Vec::new(),
            _subscriptions: subscriptions,
        }
    }

    fn toggle_section(&mut self, section: TreeSection, cx: &mut Context<Self>) {
        if !self.expanded.remove(&section) {
            self.expanded.insert(section);
        }

        if section == TreeSection::Requests {
            self.new_requests = false;
        }

        cx.notify();
    }

    fn is_expanded(&self, section: TreeSection) -> bool {
        self.expanded.contains(&section)
    }

    fn pin_room(&mut self, room_id: u64, cx: &mut Context<Self>) {
        if !self.pinned_rooms.contains(&room_id) {
            self.pinned_rooms.push(room_id);
        }
        self.expanded.insert(TreeSection::Pins);
        cx.notify();
    }

    fn unpin_room(&mut self, room_id: u64, cx: &mut Context<Self>) {
        self.pinned_rooms.retain(|id| *id != room_id);
        cx.notify();
    }

    fn is_pinned(&self, room_id: u64) -> bool {
        self.pinned_rooms.contains(&room_id)
    }

    fn tree_rows(&self, cx: &App) -> Vec<SidebarRow> {
        let chat = ChatRegistry::global(cx);
        let chat = chat.read(cx);

        let mut rows = Vec::new();

        let pinned: Vec<Entity<Room>> = self
            .pinned_rooms
            .iter()
            .filter_map(|room_id| chat.room(room_id, cx))
            .filter_map(|room| room.upgrade())
            .collect();

        if !pinned.is_empty() {
            rows.push(SidebarRow::Section {
                section: TreeSection::Pins,
                count: pinned.len(),
            });

            if self.is_expanded(TreeSection::Pins) {
                rows.extend(pinned.into_iter().map(|room| SidebarRow::Room {
                    room,
                    depth: 1,
                    pinned: true,
                }));
            }
        }

        let requests = chat.rooms(&RoomKind::Request, cx);
        rows.push(SidebarRow::Section {
            section: TreeSection::Requests,
            count: requests.len(),
        });

        if self.is_expanded(TreeSection::Requests) {
            if requests.is_empty() {
                rows.push(SidebarRow::Hint {
                    text: "No pending requests".into(),
                    depth: 1,
                });
            } else {
                rows.extend(requests.into_iter().map(|room| {
                    let pinned = self.is_pinned(room.read(cx).id);
                    SidebarRow::Room {
                        room,
                        depth: 1,
                        pinned,
                    }
                }));
            }
        }

        let communities = dummy_communities();
        rows.push(SidebarRow::Section {
            section: TreeSection::Community,
            count: communities.len(),
        });

        if self.is_expanded(TreeSection::Community) {
            if communities.is_empty() {
                rows.push(SidebarRow::Hint {
                    text: "No communities yet".into(),
                    depth: 1,
                });
            } else {
                rows.extend(
                    communities
                        .iter()
                        .map(|entry| SidebarRow::Community { entry, depth: 1 }),
                );
            }
        }

        let messages = chat.rooms(&RoomKind::Ongoing, cx);
        rows.push(SidebarRow::Section {
            section: TreeSection::Messages,
            count: messages.len(),
        });

        if self.is_expanded(TreeSection::Messages) {
            if messages.is_empty() {
                rows.push(SidebarRow::Hint {
                    text: "No conversations yet".into(),
                    depth: 1,
                });
            } else {
                rows.extend(messages.into_iter().map(|room| {
                    let pinned = self.is_pinned(room.read(cx).id);
                    SidebarRow::Room {
                        room,
                        depth: 1,
                        pinned,
                    }
                }));
            }
        }

        rows
    }

    fn render_rows(
        &self,
        range: Range<usize>,
        rows: &[SidebarRow],
        cx: &Context<Self>,
    ) -> Vec<AnyElement> {
        rows.get(range.clone())
            .into_iter()
            .flatten()
            .enumerate()
            .map(|(offset, row)| {
                let index = range.start + offset;

                match row {
                    SidebarRow::Section { section, count } => {
                        let section = *section;

                        TreeRow::new(
                            ElementId::NamedInteger("tree-row".into(), index as u64),
                            TreeRowKind::Section,
                            section.label(),
                        )
                        .caret(if self.is_expanded(section) {
                            IconName::CaretDown
                        } else {
                            IconName::CaretRight
                        })
                        .icon(section.icon())
                        .count(*count)
                        .when(
                            section == TreeSection::Requests && self.new_requests,
                            |this| this.dot(),
                        )
                        .on_click(cx.listener(move |this, _event, _window, cx| {
                            this.toggle_section(section, cx);
                        }))
                        .into_any_element()
                    }
                    SidebarRow::Room {
                        room,
                        depth,
                        pinned,
                    } => {
                        let pinned = *pinned;
                        let room_id = room.read(cx).id;
                        let public_key = room.read(cx).display_member(cx).public_key();
                        let name = room.read(cx).display_name(cx);
                        let avatar = room.read(cx).display_image(cx);
                        let kind = room.read(cx).kind;
                        let created_at = room.read(cx).created_at.to_ago();
                        let room_clone = room.clone();
                        let handler = cx.listener(move |_this, _event, window, cx| {
                            ChatRegistry::global(cx).update(cx, |chat, cx| {
                                chat.emit_room(&room_clone, window, cx);
                            });
                        });

                        let sidebar = cx.entity().downgrade();
                        let trailing =
                            Button::new(ElementId::NamedInteger("room-menu".into(), index as u64))
                                .icon(IconName::Ellipsis)
                                .ghost_alt()
                                .xsmall()
                                .compact()
                                .invisible()
                                .group_hover(ROOM_ENTRY_GROUP, |style| style.visible())
                                .dropdown_menu(move |this, _window, _cx| {
                                    let sidebar = sidebar.clone();

                                    if pinned {
                                        this.item(PopupMenuItem::new("Unpin").on_click(
                                            move |_event, _window, cx| {
                                                if let Err(error) =
                                                    sidebar.update(cx, |sidebar, cx| {
                                                        sidebar.unpin_room(room_id, cx);
                                                    })
                                                {
                                                    log::error!("Failed to unpin room: {error}");
                                                }
                                            },
                                        ))
                                    } else {
                                        this.item(PopupMenuItem::new("Pin").on_click(
                                            move |_event, _window, cx| {
                                                if let Err(error) =
                                                    sidebar.update(cx, |sidebar, cx| {
                                                        sidebar.pin_room(room_id, cx);
                                                    })
                                                {
                                                    log::error!("Failed to pin room: {error}");
                                                }
                                            },
                                        ))
                                    }
                                });

                        RoomEntry::new(index)
                            .name(name)
                            .avatar(avatar)
                            .public_key(public_key)
                            .kind(kind)
                            .created_at(created_at)
                            .depth(*depth)
                            .trailing(trailing)
                            .on_click(handler)
                            .into_any_element()
                    }
                    SidebarRow::Community { entry, depth } => TreeRow::new(
                        ElementId::NamedInteger("tree-row".into(), index as u64),
                        TreeRowKind::Community,
                        entry.name,
                    )
                    .depth(*depth)
                    .avatar(entry.name)
                    .into_any_element(),
                    SidebarRow::Hint { text, depth } => TreeRow::new(
                        ElementId::NamedInteger("tree-row".into(), index as u64),
                        TreeRowKind::Hint,
                        text.clone(),
                    )
                    .depth(*depth)
                    .into_any_element(),
                }
            })
            .collect()
    }

    fn render_user(&self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let nostr = NostrRegistry::global(cx);
        let current_user = nostr.read(cx).current_user();

        title_bar_drag_handlers(
            h_flex()
                .id("sidebar-user")
                .w_full()
                .h(TABBAR_HEIGHT)
                .flex_shrink_0()
                .items_center()
                .gap_2()
                .px_2()
                .when(cfg!(target_os = "macos"), |this| {
                    this.pl(px(TRAFFIC_LIGHT_PADDING))
                })
                .when_none(&current_user, |this| {
                    this.child(
                        div()
                            .text_xs()
                            .text_color(cx.theme().text_muted)
                            .child(SharedString::from("Import your identity to continue")),
                    )
                })
                .when_some(current_user.as_ref(), |this, public_key| {
                    let persons = PersonRegistry::global(cx);
                    let profile = persons.read(cx).get(public_key, cx);
                    let avatar = profile.avatar();
                    let name = profile.name();

                    this.child(
                        Button::new("current-user")
                            .child(Avatar::new(avatar.clone()).xsmall())
                            .small()
                            .caret()
                            .compact()
                            .transparent()
                            .dropdown_menu(move |this, _window, cx| {
                                let avatar = avatar.clone();
                                let name = name.clone();

                                this.min_w(px(256.))
                                    .item(PopupMenuItem::element(move |_window, cx| {
                                        h_flex()
                                            .gap_1p5()
                                            .text_xs()
                                            .text_color(cx.theme().text_muted)
                                            .child(Avatar::new(avatar.clone()).xsmall())
                                            .child(name.clone())
                                    }))
                                    .separator()
                                    .menu_with_icon(
                                        "Profile",
                                        IconName::Profile,
                                        Box::new(Command::ShowProfile),
                                    )
                                    .menu_with_icon(
                                        "Contact List",
                                        IconName::Book,
                                        Box::new(Command::ShowContactList),
                                    )
                                    .menu_with_icon(
                                        "Backup",
                                        IconName::UserKey,
                                        Box::new(Command::ShowBackup),
                                    )
                                    .menu_with_icon(
                                        "Themes",
                                        IconName::Sun,
                                        Box::new(Command::ToggleTheme),
                                    )
                                    .when(AutoUpdater::is_available(cx), |this| {
                                        this.separator().menu_with_icon(
                                            "Check for Updates",
                                            IconName::Device,
                                            Box::new(Command::Update),
                                        )
                                    })
                                    .menu_with_icon(
                                        "Settings",
                                        IconName::Settings,
                                        Box::new(Command::ShowSettings),
                                    )
                            }),
                    )
                }),
            window,
            cx,
        )
    }
}

fn nav_item(id: &'static str, icon: IconName, label: &'static str, command: Command) -> Button {
    Button::new(id)
        .icon(icon)
        .label(label)
        .ghost_alt()
        .small()
        .w_full()
        .justify_start()
        .on_click(move |_event, _window, cx| {
            cx.dispatch_action(&command);
        })
}

impl Panel for Sidebar {
    fn panel_id(&self) -> SharedString {
        "Sidebar".into()
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
        let logged_in = nostr.read(cx).current_user().is_some();

        let chat = ChatRegistry::global(cx);
        let loading = chat.read(cx).loading() && logged_in;

        let rows = Rc::new(self.tree_rows(cx));

        v_flex()
            .image_cache(retain_all("sidebar"))
            .size_full()
            .gap_2()
            .child(self.render_user(window, cx))
            .child(
                v_flex()
                    .px_2()
                    .py_1()
                    .gap_1()
                    .child(nav_item(
                        "nav-inbox",
                        IconName::Inbox,
                        "Inbox",
                        Command::ShowInbox,
                    ))
                    .child(nav_item(
                        "nav-browse",
                        IconName::Compass,
                        "Browse",
                        Command::ShowBrowse,
                    ))
                    .child(nav_item(
                        "nav-search",
                        IconName::Search,
                        "Search",
                        Command::ShowSearch,
                    )),
            )
            .child(
                v_flex()
                    .size_full()
                    .flex_1()
                    .gap_1()
                    .child(
                        uniform_list(
                            "sidebar-tree",
                            rows.len(),
                            cx.processor(move |this, range, _window, cx| {
                                this.render_rows(range, rows.as_slice(), cx)
                            }),
                        )
                        .track_scroll(&self.scroll_handle)
                        .flex_1()
                        .h_full()
                        .px_2(),
                    )
                    .child(Scrollbar::vertical(&self.scroll_handle)),
            )
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
    }
}
