use std::collections::BTreeSet;
use std::ops::Range;
use std::rc::Rc;

use auto_update::AutoUpdater;
use chat::{ChatEvent, ChatRegistry, Room, RoomKind};
use common::TimestampExt;
use community::{CommunityEvent, CommunityMetadata, CommunityRegistry};
use gpui::prelude::FluentBuilder;
use gpui::{
    AnyElement, App, AppContext, Context, ElementId, Entity, EventEmitter, FocusHandle, Focusable,
    InteractiveElement, IntoElement, ParentElement, Render, SharedString, Styled, Subscription,
    UniformListScrollHandle, Window, div, px, retain_all, uniform_list,
};
use person::PersonRegistry;
use settings::AppSettings;
use smallvec::{SmallVec, smallvec};
use state::NostrRegistry;
use theme::{ActiveTheme, TABBAR_HEIGHT};
use ui::avatar::Avatar;
use ui::button::{Button, ButtonVariants};
use ui::dock::{Panel, PanelEvent};
use ui::indicator::Indicator;
use ui::input::{Input, InputState};
use ui::menu::{ContextMenu, DropdownMenu, PopupMenuItem};
use ui::nav_item::NavItem;
use ui::scroll::Scrollbar;
use ui::{
    Icon, IconName, Sizable, StyledExt, TRAFFIC_LIGHT_PADDING, WindowExtension, h_flex,
    title_bar_drag_handlers, v_flex,
};

use crate::Command;

mod entry;
mod tree;

pub(crate) use entry::RoomEntry;
use tree::{SidebarRow, TreeRow, TreeRowKind, TreeSection};

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
    _subscriptions: SmallVec<[Subscription; 2]>,
}

impl Sidebar {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let settings = AppSettings::global(cx).read(cx).entity().clone();
        let chat = ChatRegistry::global(cx);
        let communities = CommunityRegistry::global(cx);

        let mut subscriptions = smallvec![];

        subscriptions.push(
            cx.subscribe_in(&chat, window, move |this, _s, event, _window, cx| {
                if event == &ChatEvent::Ping {
                    this.new_requests = true;
                    cx.notify();
                };
            }),
        );

        subscriptions.push(cx.observe(&settings, move |this, _settings, cx| {
            this.restore_state(cx);
        }));

        subscriptions.push(
            cx.subscribe(&communities, |_this, _communities, event, _cx| {
                if let CommunityEvent::Error(error) = event {
                    log::error!("community: {error}");
                }
            }),
        );

        Self {
            focus_handle: cx.focus_handle(),
            scroll_handle: UniformListScrollHandle::new(),
            new_requests: false,
            expanded: load_expanded(cx),
            pinned_rooms: AppSettings::get_pinned_rooms(cx),
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

        self.save_expanded(cx);
        cx.notify();
    }

    fn is_expanded(&self, section: TreeSection) -> bool {
        self.expanded.contains(&section)
    }

    fn restore_state(&mut self, cx: &mut Context<Self>) {
        let pinned_rooms = AppSettings::get_pinned_rooms(cx);
        let expanded = load_expanded(cx);

        if self.pinned_rooms == pinned_rooms && self.expanded == expanded {
            return;
        }

        self.pinned_rooms = pinned_rooms;
        self.expanded = expanded;
        cx.notify();
    }

    fn save_expanded(&self, cx: &mut App) {
        let keys = self
            .expanded
            .iter()
            .map(|section| section.key().to_string())
            .collect();
        AppSettings::update_expanded_sections(Some(keys), cx);
    }

    fn pin_room(&mut self, room_id: u64, cx: &mut Context<Self>) {
        if !self.pinned_rooms.contains(&room_id) {
            self.pinned_rooms.push(room_id);
        }
        self.expanded.insert(TreeSection::Pins);

        AppSettings::update_pinned_rooms(self.pinned_rooms.clone(), cx);
        self.save_expanded(cx);
        cx.notify();
    }

    fn unpin_room(&mut self, room_id: u64, cx: &mut Context<Self>) {
        self.pinned_rooms.retain(|id| *id != room_id);

        AppSettings::update_pinned_rooms(self.pinned_rooms.clone(), cx);
        cx.notify();
    }

    fn is_pinned(&self, room_id: u64) -> bool {
        self.pinned_rooms.contains(&room_id)
    }

    fn new_community(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let name_input = cx.new(|cx| InputState::new(window, cx).placeholder("Community name"));

        window.open_modal(cx, move |this, _window, _cx| {
            let name_input = name_input.clone();

            this.width(px(380.))
                .confirm()
                .title("New community")
                .child(Input::new(&name_input))
                .on_ok(move |_event, _window, cx| {
                    let name = name_input.read(cx).value().trim().to_owned();

                    if name.is_empty() {
                        return false;
                    }

                    let metadata = CommunityMetadata {
                        name,
                        ..CommunityMetadata::default()
                    };

                    CommunityRegistry::global(cx)
                        .update(cx, |registry, cx| registry.create(metadata, cx));

                    true
                })
        });
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

        let registry = CommunityRegistry::global(cx);
        let communities = registry.read(cx).communities();

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
                        .cloned()
                        .map(|community| SidebarRow::Community {
                            community,
                            depth: 1,
                        }),
                );
            }

            rows.push(SidebarRow::NewCommunity { depth: 1 });
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
                        let picture = room.read(cx).display_image(cx);
                        let seed = room.read(cx).display_image_seed(cx);
                        let kind = room.read(cx).kind;
                        let created_at = room.read(cx).created_at.to_ago();
                        let room_clone = room.clone();
                        let handler = cx.listener(move |_this, _event, window, cx| {
                            ChatRegistry::global(cx).update(cx, |chat, cx| {
                                chat.emit_room(&room_clone, window, cx);
                            });
                        });

                        let entry = RoomEntry::new(index)
                            .name(name)
                            .avatar(picture)
                            .seed(seed)
                            .public_key(public_key)
                            .kind(kind)
                            .created_at(created_at)
                            .depth(*depth)
                            .on_click(handler);

                        let sidebar = cx.entity().downgrade();
                        ContextMenu::new(
                            ElementId::NamedInteger("room-context-menu".into(), index as u64),
                            entry,
                            move |this, _window, _cx| {
                                let sidebar = sidebar.clone();

                                if pinned {
                                    this.item(PopupMenuItem::new("Unpin").on_click(
                                        move |_event, _window, cx| {
                                            if let Err(error) = sidebar.update(cx, |sidebar, cx| {
                                                sidebar.unpin_room(room_id, cx);
                                            }) {
                                                log::error!("Failed to unpin room: {error}");
                                            }
                                        },
                                    ))
                                } else {
                                    this.item(PopupMenuItem::new("Pin").on_click(
                                        move |_event, _window, cx| {
                                            if let Err(error) = sidebar.update(cx, |sidebar, cx| {
                                                sidebar.pin_room(room_id, cx);
                                            }) {
                                                log::error!("Failed to pin room: {error}");
                                            }
                                        },
                                    ))
                                }
                            },
                        )
                        .into_any_element()
                    }
                    SidebarRow::Community { community, depth } => {
                        let community = community.read(cx);

                        TreeRow::new(
                            ElementId::NamedInteger("tree-row".into(), index as u64),
                            TreeRowKind::Community,
                            community.name(),
                        )
                        .depth(*depth)
                        .avatar(community.id().to_hex())
                        .picture(community.icon())
                        .into_any_element()
                    }
                    SidebarRow::NewCommunity { depth } => TreeRow::new(
                        ElementId::NamedInteger("tree-row".into(), index as u64),
                        TreeRowKind::Hint,
                        "New community",
                    )
                    .depth(*depth)
                    .icon(IconName::Plus)
                    .on_click(cx.listener(|this, _event, window, cx| {
                        this.new_community(window, cx);
                    }))
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
                    let avatar_seed = profile.avatar_seed();
                    let name = profile.name();

                    this.child(
                        Button::new("current-user")
                            .child(
                                Avatar::new(avatar.clone())
                                    .seed(avatar_seed.clone())
                                    .xsmall(),
                            )
                            .small()
                            .caret()
                            .compact()
                            .transparent()
                            .dropdown_menu(move |this, _window, cx| {
                                let avatar = avatar.clone();
                                let avatar_seed = avatar_seed.clone();
                                let name = name.clone();

                                this.min_w(px(256.))
                                    .item(PopupMenuItem::element(move |_window, cx| {
                                        h_flex()
                                            .gap_1p5()
                                            .text_xs()
                                            .text_color(cx.theme().text_muted)
                                            .child(
                                                Avatar::new(avatar.clone())
                                                    .seed(avatar_seed.clone())
                                                    .xsmall(),
                                            )
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

fn load_expanded(cx: &App) -> BTreeSet<TreeSection> {
    let Some(keys) = AppSettings::get_expanded_sections(cx) else {
        return BTreeSet::from([TreeSection::Community, TreeSection::Messages]);
    };

    keys.iter()
        .filter_map(|key| TreeSection::from_key(key.as_str()))
        .collect()
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
                    .child(
                        NavItem::new("nav-inbox", "Inbox", Icon::new(IconName::Inbox).small())
                            .on_click(|_event, _window, cx| {
                                cx.dispatch_action(&Command::ShowInbox)
                            }),
                    )
                    .child(
                        NavItem::new("nav-browse", "Browse", Icon::new(IconName::Compass).small())
                            .on_click(|_event, _window, cx| {
                                cx.dispatch_action(&Command::ShowBrowse)
                            }),
                    )
                    .child(
                        NavItem::new("nav-search", "Search", Icon::new(IconName::Search).small())
                            .on_click(|_event, _window, cx| {
                                cx.dispatch_action(&Command::ShowSearch)
                            }),
                    ),
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
