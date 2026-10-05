use std::collections::HashSet;
use std::ops::Range;
use std::rc::Rc;

use anyhow::Error;
use auto_update::AutoUpdater;
use chat::{ChatEvent, ChatRegistry, Room, RoomKind};
use common::TimestampExt;
use gpui::prelude::FluentBuilder;
use gpui::{
    AnyElement, App, AppContext, ClipboardItem, Context, Div, Entity, EventEmitter, FocusHandle,
    Focusable, InteractiveElement, IntoElement, ObjectFit, ParentElement, Render, SharedString,
    Stateful, Styled, StyledImage, Subscription, Task, UniformListScrollHandle, WeakEntity, Window,
    div, img, px, relative, retain_all, uniform_list,
};
use nip29::{Group, GroupKey, GroupsEvent, GroupsRegistry};
use nostr_sdk::prelude::*;
use person::PersonRegistry;
use settings::AppSettings;
use smallvec::{SmallVec, smallvec};
use state::{NostrRegistry, StateEvent};
use theme::{ActiveTheme, TABBAR_HEIGHT};
use ui::avatar::Avatar;
use ui::button::{Button, ButtonCustomVariant, ButtonVariants};
use ui::dialog::DialogButtonProps;
use ui::dock::{ClosePanel, DockArea, DockPlacement, Panel, PanelEvent, PanelHandle};
use ui::indicator::Indicator;
use ui::menu::{ContextMenu, DropdownMenu, PopupMenuItem};
use ui::nav::Nav;
use ui::nav_item::NavItem;
use ui::notification::Notification;
use ui::scroll::Scrollbar;
use ui::{
    Disableable, Icon, IconName, Selectable, Sizable, StyledExt, TRAFFIC_LIGHT_PADDING,
    WindowExtension, h_flex, title_bar_drag_handlers, v_flex,
};

use crate::Command;
use crate::dialogs::{import, profile, screening};

mod utils;

pub(crate) use utils::pick_banner;

/// A collapsible section of the sidebar's main list.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ListSection {
    Pins,
    Groups,
    Chats,
}

impl ListSection {
    fn label(self) -> &'static str {
        match self {
            Self::Pins => "Pinned",
            Self::Groups => "Groups",
            Self::Chats => "Chats",
        }
    }
}

pub enum SidebarRow {
    Section(ListSection),
    Room { room: Entity<Room> },
    Group { group: Entity<Group> },
    Contact { public_key: PublicKey },
    Empty(&'static str),
}

/// A collapsible section of the group view.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum GroupSection {
    Admins,
    Members,
}

impl GroupSection {
    fn label(self) -> &'static str {
        match self {
            Self::Admins => "Admins",
            Self::Members => "Members",
        }
    }
}

/// A row in the group view.
enum GroupRow {
    Section(GroupSection),
    Member {
        public_key: PublicKey,
        roles: Vec<String>,
    },
}

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

    tasks: SmallVec<[Task<Result<(), Error>>; 1]>,
    _subscriptions: SmallVec<[Subscription; 4]>,
}

impl Sidebar {
    pub fn new(window: &mut Window, dock: WeakEntity<DockArea>, cx: &mut Context<Self>) -> Self {
        let chat = ChatRegistry::global(cx);
        let groups = GroupsRegistry::global(cx);
        let nostr = NostrRegistry::global(cx);

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
            cx.subscribe_in(&nostr, window, |this, _, event, _window, cx| {
                // Re-pick the background each time the signed-out screen is shown.
                if let StateEvent::NoSigner = event {
                    this.banner = pick_banner();
                    cx.notify();
                }
            }),
        );

        Self {
            focus_handle: cx.focus_handle(),
            list_scroll: UniformListScrollHandle::new(),
            dock,
            banner: pick_banner(),
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
            tasks: smallvec![],
            _subscriptions: subscriptions,
        }
    }

    /// Show the group view, replacing the main list.
    fn show_group(&mut self, group: &Entity<Group>, cx: &mut Context<Self>) {
        self.group = Some(group.downgrade());
        self.group_open = true;
        cx.notify();
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

    /// Leave the group view, returning the sidebar to its main list.
    fn hide_group(&mut self, cx: &mut Context<Self>) {
        if !self.group_open {
            return;
        }

        self.group_open = false;
        cx.notify();
    }

    /// Leave the contact picker and clear its selection.
    fn close_contacts(&mut self) {
        self.contacts_open = false;
        self.selected_contacts.clear();
    }

    /// Show or hide the contact picker in the inbox list.
    fn toggle_contacts(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.contacts_open = !self.contacts_open;

        if !self.contacts_open {
            self.selected_contacts.clear();
        } else {
            self.close_requests();
            if self.contacts.is_none() {
                self.load_contacts(window, cx);
            }
        }

        cx.notify();
    }

    /// Leave the requests list in the inbox.
    fn close_requests(&mut self) {
        self.requests_open = false;
    }

    /// Show or hide the chat requests in the inbox list.
    fn toggle_requests(&mut self, cx: &mut Context<Self>) {
        self.requests_open = !self.requests_open;

        if self.requests_open {
            self.close_contacts();
            self.new_requests = false;
        }

        cx.notify();
    }

    /// Load the signed-in user's contact list into the contact picker.
    fn load_contacts(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let nostr = NostrRegistry::global(cx);
        let client = nostr.read(cx).client();

        let Some(public_key) = nostr.read(cx).current_user() else {
            return;
        };

        let task: Task<Result<Vec<PublicKey>, Error>> = cx.background_spawn(async move {
            let filter = Filter::new()
                .author(public_key)
                .kind(Kind::ContactList)
                .limit(1);

            let contacts: Vec<PublicKey> = client
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
                        this.contacts = Some(contacts);
                        cx.notify();
                    })?;
                }
                Err(error) => {
                    cx.update(|window, cx| {
                        window.push_notification(
                            Notification::error(error.to_string()).autohide(false),
                            cx,
                        );
                    })?;
                }
            }

            Ok(())
        }));
    }

    /// Toggle a contact's selection in the contact picker.
    fn toggle_contact(&mut self, public_key: &PublicKey, cx: &mut Context<Self>) {
        if !self.selected_contacts.remove(public_key) {
            self.selected_contacts.insert(public_key.to_owned());
        }
        cx.notify();
    }

    /// Start a chat with the contacts selected in the picker.
    fn create_chat(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.selected_contacts.is_empty() {
            return;
        }

        let nostr = NostrRegistry::global(cx);
        let Some(public_key) = nostr.read(cx).current_user() else {
            return;
        };

        let chat = ChatRegistry::global(cx);
        let async_chat = chat.downgrade();
        let dock = self.dock.clone();

        let receivers: Vec<PublicKey> = self.selected_contacts.iter().copied().collect();

        self.tasks.push(cx.spawn_in(window, async move |this, cx| {
            let room = async_chat.update_in(cx, |chat, _window, cx| {
                let room = cx.new(|_| {
                    Room::new(public_key, receivers)
                        .organize(&public_key)
                        .kind(RoomKind::Ongoing)
                });
                chat.track_room(&room, cx);
                room
            })?;

            cx.update(|window, cx| {
                ui::dock::add_panel_to(
                    &dock,
                    PanelHandle::new(chat_ui::init(room.downgrade(), window, cx)),
                    DockPlacement::Center,
                    window,
                    cx,
                );
            })?;

            // Leave the picker now that the chat is open
            this.update(cx, |this, cx| {
                this.close_contacts();
                cx.notify();
            })?;

            Ok(())
        }));
    }

    fn render_user(&self, current_user: &PublicKey, cx: &mut Context<Self>) -> Stateful<Div> {
        let updater = AutoUpdater::try_global(cx);
        let persons = PersonRegistry::global(cx);
        let profile = persons.read(cx).get(current_user, cx);
        let avatar = profile.avatar();
        let avatar_seed = profile.avatar_seed();
        let name = profile.name();

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
            .child(
                Button::new("current-user")
                    .child(
                        Avatar::new(avatar.clone())
                            .seed(avatar_seed.clone())
                            .small(),
                    )
                    .small()
                    .caret()
                    .compact()
                    .transparent()
                    .dropdown_menu(move |this, _window, _cx| {
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
                            .menu_with_icon("Inbox", IconName::Inbox, Box::new(Command::ShowInbox))
                            .menu_with_icon(
                                "Search",
                                IconName::Search,
                                Box::new(Command::ShowSearch),
                            )
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
                            .menu_with_icon("Themes", IconName::Sun, Box::new(Command::ToggleTheme))
                            .separator()
                            .menu_with_icon(
                                "Settings",
                                IconName::Settings,
                                Box::new(Command::ShowSettings),
                            )
                    }),
            )
            .when_some(updater, |this, updater| {
                this.child(self.render_updater(updater, cx))
            })
            .child(
                Button::new("sidebar-new")
                    .icon(IconName::PlusCircle)
                    .tooltip("New")
                    .ghost()
                    .small()
                    .dropdown_menu(|menu, _window, _cx| {
                        menu.menu_with_icon(
                            "New Chat",
                            IconName::Message,
                            Box::new(Command::NewChat),
                        )
                        .menu_with_icon("New Group", IconName::Group, Box::new(Command::NewGroup))
                        .separator()
                        .menu_with_icon(
                            "Join Group",
                            IconName::Door,
                            Box::new(Command::JoinGroup),
                        )
                    }),
            )
            .when(self.group_open, |this| {
                this.child(
                    Button::new("sidebar-back")
                        .icon(IconName::ArrowLeft)
                        .tooltip("Back")
                        .ghost()
                        .small()
                        .on_click(cx.listener(|this, _event, _window, cx| {
                            this.hide_group(cx);
                        })),
                )
            })
    }

    fn render_updater(&self, updater: Entity<AutoUpdater>, cx: &mut App) -> AnyElement {
        let status = updater.read(cx).status();
        let up_to_date = updater.read(cx).up_to_date();
        let staged = updater.read(cx).staged();

        h_flex()
            .gap_2()
            .when(!up_to_date, |this| {
                this.child(
                    Button::new("update-status")
                        .icon(IconName::ArrowDownCircle)
                        .tooltip(status)
                        .small()
                        .warning()
                        .disabled(true),
                )
            })
            .when(staged, |this| {
                this.child(
                    Button::new("restart-to-update")
                        .icon(IconName::ArrowDownCircle)
                        .tooltip("Quit and relaunch into the installed update")
                        .small()
                        .ghost()
                        .on_click(move |_, _window, cx| {
                            updater.update(cx, |this, cx| {
                                this.restart(cx);
                            });
                        }),
                )
            })
            .into_any_element()
    }

    fn render_list(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let requests_open = self.requests_open;
        let contacts_open = self.contacts_open && !requests_open;
        let rows = Rc::new(self.rows_for(cx));

        // Whether to show the create button for the contact picker
        let create_button = contacts_open && !self.selected_contacts.is_empty();

        let (list_id, scroll_handle, list_title) = if contacts_open {
            ("sidebar-contacts", &self.contacts_scroll, Some("Contacts"))
        } else if requests_open {
            ("sidebar-requests", &self.requests_scroll, Some("Requests"))
        } else {
            ("sidebar-list", &self.list_scroll, None)
        };

        let request_count = ChatRegistry::global(cx)
            .read(cx)
            .count(&RoomKind::Request, cx);

        v_flex()
            .size_full()
            .flex_1()
            .min_h_0()
            .relative()
            .gap_2()
            .child(
                v_flex()
                    .px_2()
                    .gap_1()
                    .child(
                        NavItem::new("browse", "Browse", Icon::new(IconName::Compass).small())
                            .on_click(|_, window, cx| {
                                window.dispatch_action(Box::new(Command::ShowBrowse), cx)
                            }),
                    )
                    .child(
                        NavItem::new("reqs", "Requests", Icon::new(IconName::Invite).small())
                            .when(request_count > 0, |this| {
                                this.suffix(
                                    h_flex()
                                        .gap_1()
                                        .items_center()
                                        .child(
                                            h_flex()
                                                .py_0p5()
                                                .px_1()
                                                .min_w_6()
                                                .justify_center()
                                                .text_size(px(10.))
                                                .text_color(cx.theme().text_muted)
                                                .text_center()
                                                .rounded(cx.theme().radius)
                                                .bg(cx.theme().elevated_surface_background)
                                                .child(SharedString::from(
                                                    request_count.to_string(),
                                                )),
                                        )
                                        .when(self.new_requests && !requests_open, |this| {
                                            this.child(
                                                div().size_1().rounded_full().bg(cx.theme().cursor),
                                            )
                                        }),
                                )
                            })
                            .when(requests_open, |this| {
                                this.bg(cx.theme().ghost_element_active)
                            })
                            .on_click(cx.listener(|this, _event, _window, cx| {
                                this.toggle_requests(cx);
                            })),
                    )
                    .child(
                        NavItem::new("contacts", "Contacts", Icon::new(IconName::Book).small())
                            .when(contacts_open, |this| {
                                this.bg(cx.theme().ghost_element_active)
                            })
                            .on_click(cx.listener(|this, _event, window, cx| {
                                this.toggle_contacts(window, cx);
                            })),
                    ),
            )
            .when_some(list_title, |this, list_title| {
                this.child(
                    div()
                        .px_4()
                        .text_xs()
                        .font_semibold()
                        .text_color(cx.theme().text_placeholder)
                        .child(list_title),
                )
            })
            .child(
                div()
                    .min_h_0()
                    .flex_1()
                    .child(
                        uniform_list(
                            list_id,
                            rows.len(),
                            cx.processor(move |this, range, _window, cx| {
                                this.render_rows(range, rows.as_slice(), cx)
                            }),
                        )
                        .track_scroll(scroll_handle)
                        .h_full()
                        .px_2(),
                    )
                    .child(Scrollbar::vertical(scroll_handle)),
            )
            .when(create_button, |this| {
                this.child(
                    v_flex()
                        .absolute()
                        .bottom_0()
                        .left_0()
                        .justify_center()
                        .h_20()
                        .w_full()
                        .px_4()
                        .border_t_1()
                        .border_color(cx.theme().border)
                        .bg(cx.theme().background)
                        .child(
                            Button::new("create-chat")
                                .label(if self.selected_contacts.len() > 1 {
                                    "Create Group Chat"
                                } else {
                                    "Create Chat"
                                })
                                .primary()
                                .on_click(cx.listener(|this, _ev, window, cx| {
                                    this.create_chat(window, cx);
                                })),
                        )
                        .child(
                            h_flex()
                                .gap_1()
                                .text_xs()
                                .text_color(cx.theme().text_muted)
                                .child("Selected")
                                .child(div().font_semibold().child(SharedString::from(
                                    self.selected_contacts.len().to_string(),
                                ))),
                        ),
                )
            })
            .into_any_element()
    }

    fn rows_for(&self, cx: &App) -> Vec<SidebarRow> {
        if self.contacts_open {
            return self
                .contacts
                .clone()
                .unwrap_or_default()
                .into_iter()
                .map(|public_key| SidebarRow::Contact { public_key })
                .collect();
        }

        if self.requests_open {
            let chat = ChatRegistry::global(cx);
            return chat
                .read(cx)
                .rooms(&RoomKind::Request, cx)
                .into_iter()
                .map(|room| SidebarRow::Room { room })
                .collect();
        }

        let mut rows = Vec::new();

        let chat = ChatRegistry::global(cx);
        let pinned_rooms = AppSettings::get_pinned_rooms(cx);

        let (pinned_rooms, rooms): (Vec<_>, Vec<_>) = chat
            .read(cx)
            .rooms(&RoomKind::Ongoing, cx)
            .into_iter()
            .partition(|room| pinned_rooms.contains(&room.read(cx).id));

        let registry = GroupsRegistry::global(cx);
        let pinned_group_ids = AppSettings::get_pinned_groups(cx);

        let (pinned_groups, groups): (Vec<_>, Vec<_>) = registry
            .read(cx)
            .groups()
            .to_vec()
            .into_iter()
            .partition(|group| {
                let tag = group.read(cx).key().cache_tag();
                pinned_group_ids.iter().any(|pinned| pinned == &tag)
            });

        rows.push(SidebarRow::Section(ListSection::Pins));
        if self.pins_open {
            if pinned_rooms.is_empty() && pinned_groups.is_empty() {
                rows.push(SidebarRow::Empty("Nothing pinned yet"));
            } else {
                rows.extend(
                    pinned_groups
                        .into_iter()
                        .map(|group| SidebarRow::Group { group }),
                );
                rows.extend(
                    pinned_rooms
                        .into_iter()
                        .map(|room| SidebarRow::Room { room }),
                );
            }
        }

        rows.push(SidebarRow::Section(ListSection::Groups));
        if self.is_groups_open(cx) {
            if groups.is_empty() {
                rows.push(SidebarRow::Empty("No groups yet"));
            } else {
                rows.extend(groups.into_iter().map(|group| SidebarRow::Group { group }));
            }
        }

        rows.push(SidebarRow::Section(ListSection::Chats));
        if self.is_chats_open(cx) {
            if rooms.is_empty() {
                rows.push(SidebarRow::Empty("No chats yet"));
            } else {
                rows.extend(rooms.into_iter().map(|room| SidebarRow::Room { room }));
            }
        }

        rows
    }

    /// Whether anything is pinned; drives the sections' default expansion.
    fn pinned_any(cx: &App) -> bool {
        !AppSettings::get_pinned_rooms(cx).is_empty()
            || !AppSettings::get_pinned_groups(cx).is_empty()
    }

    fn is_chats_open(&self, cx: &App) -> bool {
        self.chats_open.unwrap_or_else(|| !Self::pinned_any(cx))
    }

    fn is_groups_open(&self, cx: &App) -> bool {
        self.groups_open.unwrap_or_else(|| !Self::pinned_any(cx))
    }

    fn render_rows(
        &self,
        range: Range<usize>,
        rows: &[SidebarRow],
        cx: &Context<Sidebar>,
    ) -> Vec<AnyElement> {
        let hide_avatar = AppSettings::get_hide_avatar(cx);

        rows.get(range.clone())
            .into_iter()
            .flatten()
            .enumerate()
            .map(|(offset, row)| {
                let index = range.start + offset;

                match row {
                    SidebarRow::Section(section) => {
                        let section = *section;
                        let open = match section {
                            ListSection::Pins => self.pins_open,
                            ListSection::Groups => self.is_groups_open(cx),
                            ListSection::Chats => self.is_chats_open(cx),
                        };
                        let icon = if open {
                            IconName::CaretDown
                        } else {
                            IconName::CaretRight
                        };
                        let label = section.label();

                        Nav::new(label)
                            .label(label)
                            .suffix(Icon::new(icon).small().text_color(cx.theme().icon_muted))
                            .text_xs()
                            .font_semibold()
                            .text_color(cx.theme().text_placeholder)
                            .on_click(cx.listener(move |this, _event, _window, cx| {
                                match section {
                                    ListSection::Pins => this.pins_open = !this.pins_open,
                                    ListSection::Groups => {
                                        this.groups_open = Some(!this.is_groups_open(cx))
                                    }
                                    ListSection::Chats => {
                                        this.chats_open = Some(!this.is_chats_open(cx))
                                    }
                                }
                                cx.notify();
                            }))
                            .into_any_element()
                    }
                    SidebarRow::Empty(text) => h_flex()
                        .h_10()
                        .w_full()
                        .px_1p5()
                        .py_1()
                        .justify_center()
                        .border_1()
                        .border_dashed()
                        .border_color(cx.theme().border)
                        .rounded(cx.theme().radius)
                        .justify_center()
                        .text_xs()
                        .text_color(cx.theme().text_placeholder)
                        .child(*text)
                        .into_any_element(),
                    SidebarRow::Room { room } => {
                        let name = room.read(cx).display_name(cx);
                        let picture = room.read(cx).display_image(cx);
                        let seed = room.read(cx).display_image_seed(cx);
                        let created_at = room.read(cx).created_at.to_ago();
                        let kind = room.read(cx).kind;
                        let peer = room.read(cx).display_member(cx).public_key();
                        let id = room.read(cx).id;
                        let member = room.read(cx).members().first().copied();
                        let pinned = AppSettings::global(cx).read(cx).pinned_room(id, cx);
                        let dock = self.dock.clone();
                        let room = room.clone();

                        let nav = Nav::new(index)
                            .label(name)
                            .text_sm()
                            .font_medium()
                            .when(!hide_avatar, |this| {
                                this.prefix(
                                    Avatar::from_source(picture)
                                        .seed(seed)
                                        .small()
                                        .flex_shrink_0(),
                                )
                            })
                            .suffix(
                                div()
                                    .font_normal()
                                    .text_xs()
                                    .text_color(cx.theme().text_placeholder)
                                    .child(created_at),
                            )
                            .on_click(move |_event, window, cx| {
                                ui::dock::add_panel_to(
                                    &dock,
                                    PanelHandle::new(chat_ui::init(room.downgrade(), window, cx)),
                                    DockPlacement::Center,
                                    window,
                                    cx,
                                );
                                if kind != RoomKind::Ongoing && AppSettings::get_screening(cx) {
                                    let screening = screening::init(peer, window, cx);

                                    window.open_dialog(cx, move |this, _window, _cx| {
                                        this.confirm()
                                            .child(screening.clone())
                                            .button_props(
                                                DialogButtonProps::default()
                                                    .cancel_text("Ignore")
                                                    .ok_text("Response"),
                                            )
                                            .on_cancel(move |_event, window, cx| {
                                                window.dispatch_action(Box::new(ClosePanel), cx);
                                                true
                                            })
                                    });
                                }
                            });

                        ContextMenu::new(
                            format!("room-menu-{index}"),
                            nav,
                            move |menu, _window, _cx| {
                                let menu = menu.item(
                                    PopupMenuItem::new(if pinned { "Unpin" } else { "Pin" })
                                        .on_click(move |_, _window, cx| {
                                            AppSettings::global(cx).update(cx, |settings, cx| {
                                                settings.toggle_pinned_room(id, cx);
                                            });
                                        }),
                                );

                                if let Some(member) = member {
                                    menu.item(PopupMenuItem::new("View profile").on_click(
                                        move |_, window, cx| {
                                            profile::open(member, window, cx);
                                        },
                                    ))
                                } else {
                                    menu
                                }
                            },
                        )
                        .into_any_element()
                    }
                    SidebarRow::Contact { public_key } => {
                        let persons = PersonRegistry::global(cx);
                        let person = persons.read(cx).get(public_key, cx);
                        let selected = self.selected_contacts.contains(public_key);
                        let sidebar = cx.entity().downgrade();
                        let public_key = public_key.to_owned();

                        Nav::new(index)
                            .label(person.name())
                            .text_sm()
                            .font_medium()
                            .when(!hide_avatar, |this| {
                                this.prefix(
                                    Avatar::from_source(person.avatar())
                                        .seed(person.avatar_seed())
                                        .small()
                                        .flex_shrink_0(),
                                )
                            })
                            .selected(selected)
                            .on_click(move |_event, _window, cx| {
                                if let Err(error) = sidebar.update(cx, |this, cx| {
                                    this.toggle_contact(&public_key, cx);
                                }) {
                                    log::error!("Failed to select contact: {error}");
                                }
                            })
                            .into_any_element()
                    }
                    SidebarRow::Group { group } => {
                        let dock = self.dock.clone();
                        let sidebar = cx.entity().downgrade();
                        let group = group.clone();

                        let name = group.read(cx).display_name();
                        let picture = group.read(cx).display_image();
                        let seed = group.read(cx).key().cache_tag();
                        let tag = seed.clone();
                        let pinned = AppSettings::global(cx).read(cx).pinned_group(&tag, cx);
                        let weak_group = group.downgrade();

                        let suffix = group
                            .read(cx)
                            .membership()
                            .status()
                            .or_else(|| group.read(cx).elsewhere().map(|_| "Moved"))
                            .map(SharedString::from);

                        let nav = Nav::new(index)
                            .label(name)
                            .text_sm()
                            .font_medium()
                            .when(!hide_avatar, |this| {
                                this.prefix(
                                    Avatar::from_source(picture)
                                        .seed(seed)
                                        .small()
                                        .flex_shrink_0(),
                                )
                            })
                            .when_some(suffix, |this, suffix| {
                                this.suffix(
                                    div()
                                        .font_normal()
                                        .text_xs()
                                        .text_color(cx.theme().text_placeholder)
                                        .child(suffix),
                                )
                            })
                            .on_click(move |_event, window, cx| {
                                ui::dock::add_panel_to(
                                    &dock,
                                    PanelHandle::new(nip29_ui::init(group.clone(), window, cx)),
                                    DockPlacement::Center,
                                    window,
                                    cx,
                                );

                                if let Err(error) = sidebar.update(cx, |this, cx| {
                                    this.show_group(&group, cx);
                                }) {
                                    log::error!("Failed to show the group in the sidebar: {error}");
                                }
                            });

                        ContextMenu::new(
                            format!("group-menu-{index}"),
                            nav,
                            move |menu, _window, _cx| {
                                menu.item(
                                    PopupMenuItem::new(if pinned { "Unpin" } else { "Pin" })
                                        .on_click({
                                            let tag = tag.clone();
                                            move |_, _window, cx| {
                                                AppSettings::global(cx).update(
                                                    cx,
                                                    |settings, cx| {
                                                        settings.toggle_pinned_group(&tag, cx);
                                                    },
                                                );
                                            }
                                        }),
                                )
                                .item(
                                    PopupMenuItem::new("Leave").on_click({
                                        let group = weak_group.clone();
                                        move |_, window, cx| {
                                            nip29_ui::confirm_leave(group.clone(), window, cx);
                                        }
                                    }),
                                )
                            },
                        )
                        .into_any_element()
                    }
                }
            })
            .collect()
    }

    fn render_group(&mut self, group: Entity<Group>, cx: &mut Context<Self>) -> AnyElement {
        let admin = NostrRegistry::global(cx)
            .read(cx)
            .current_user()
            .is_some_and(|me| group.read(cx).admins().contains(&me));

        let (name, seed, picture, total_members, rows) = {
            let group = group.read(cx);
            let admins = group.admins();

            let mut admin_rows = Vec::new();
            let mut member_rows = Vec::new();

            for (public_key, roles) in admins.entries() {
                admin_rows.push(GroupRow::Member {
                    public_key: *public_key,
                    roles: roles.clone(),
                });
            }

            for public_key in group.members().iter() {
                if admins.contains(public_key) {
                    continue;
                }

                member_rows.push(GroupRow::Member {
                    public_key: *public_key,
                    roles: Vec::new(),
                });
            }

            let mut rows = Vec::new();

            if !admin_rows.is_empty() {
                rows.push(GroupRow::Section(GroupSection::Admins));
                if self.admins_open {
                    rows.append(&mut admin_rows);
                }
            }

            if !member_rows.is_empty() {
                rows.push(GroupRow::Section(GroupSection::Members));
                if self.members_open {
                    rows.append(&mut member_rows);
                }
            }

            (
                group.display_name(),
                group.key().cache_tag(),
                group.display_image(),
                SharedString::from(format!("{} members", group.total_members())),
                rows,
            )
        };

        let rows = Rc::new(rows);
        let member_group = group.downgrade();

        v_flex()
            .flex_1()
            .min_h_0()
            .w_full()
            .gap_2()
            .child(
                h_flex()
                    .px_2()
                    .flex_shrink_0()
                    .justify_between()
                    .child(
                        h_flex()
                            .pl_0p5()
                            .gap_1()
                            .flex_shrink_1()
                            .child(Avatar::from_source(picture).seed(seed).small())
                            .child(
                                v_flex()
                                    .flex_1()
                                    .min_w_0()
                                    .child(
                                        div()
                                            .text_xs()
                                            .truncate()
                                            .line_height(relative(1.2))
                                            .font_semibold()
                                            .child(name),
                                    )
                                    .child(
                                        div()
                                            .text_size(px(10.))
                                            .text_color(cx.theme().text_muted)
                                            .child(total_members),
                                    ),
                            ),
                    )
                    .child(
                        h_flex()
                            .gap_1()
                            .child(nip29_ui::actions(group.downgrade(), cx)),
                    ),
            )
            .child(
                div()
                    .min_h_0()
                    .flex_1()
                    .child(
                        uniform_list(
                            "group-rows",
                            rows.len(),
                            cx.processor(move |this, range, _window, cx| {
                                this.render_group_rows(
                                    range,
                                    rows.as_slice(),
                                    &member_group,
                                    admin,
                                    cx,
                                )
                            }),
                        )
                        .track_scroll(&self.group_scroll)
                        .h_full()
                        .px_2(),
                    )
                    .child(Scrollbar::vertical(&self.group_scroll)),
            )
            .into_any_element()
    }

    fn render_group_rows(
        &self,
        range: Range<usize>,
        rows: &[GroupRow],
        group: &WeakEntity<Group>,
        admin: bool,
        cx: &mut Context<Self>,
    ) -> Vec<AnyElement> {
        rows.get(range)
            .into_iter()
            .flatten()
            .map(|row| match row {
                GroupRow::Section(section) => self.section_row(*section, cx),
                GroupRow::Member { public_key, roles } => {
                    self.member_row(public_key, roles, group, admin, cx)
                }
            })
            .collect()
    }

    fn section_row(&self, section: GroupSection, cx: &mut Context<Self>) -> AnyElement {
        let open = match section {
            GroupSection::Admins => self.admins_open,
            GroupSection::Members => self.members_open,
        };
        let icon = if open {
            IconName::CaretDown
        } else {
            IconName::CaretRight
        };
        let label = section.label();

        Nav::new(label)
            .label(label)
            .suffix(Icon::new(icon).small().text_color(cx.theme().icon_muted))
            .text_xs()
            .font_semibold()
            .text_color(cx.theme().text_placeholder)
            .on_click(cx.listener(move |this, _event, _window, cx| {
                match section {
                    GroupSection::Admins => this.admins_open = !this.admins_open,
                    GroupSection::Members => this.members_open = !this.members_open,
                }
                cx.notify();
            }))
            .into_any_element()
    }

    fn member_row(
        &self,
        public_key: &PublicKey,
        roles: &[String],
        group: &WeakEntity<Group>,
        admin: bool,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let hide_avatar = AppSettings::get_hide_avatar(cx);
        let persons = PersonRegistry::global(cx);
        let person = persons.read(cx).get(public_key, cx);
        let public_key = *public_key;

        let group = group.clone();

        Nav::new(public_key.to_hex())
            .label(person.name())
            .text_sm()
            .font_medium()
            .clickable(true)
            .when(!hide_avatar, |this| {
                this.prefix(
                    Avatar::new(person.avatar())
                        .seed(person.avatar_seed())
                        .small()
                        .flex_shrink_0(),
                )
            })
            .when(!roles.is_empty(), |this| {
                this.suffix(
                    div()
                        .truncate()
                        .text_xs()
                        .text_color(cx.theme().text_placeholder)
                        .child(SharedString::from(roles.join(", "))),
                )
            })
            .dropdown_menu(move |menu, _window, _cx| {
                let menu = menu
                    .item(
                        PopupMenuItem::new("View profile").on_click(move |_, window, cx| {
                            profile::open(public_key, window, cx);
                        }),
                    )
                    .item(PopupMenuItem::new("Copy").on_click(move |_, _, cx| {
                        let npub = public_key.to_bech32().unwrap_or_default();
                        cx.write_to_clipboard(ClipboardItem::new_string(npub));
                    }));

                if !admin {
                    return menu;
                }

                menu.separator()
                    .item(PopupMenuItem::new("Remove").on_click({
                        let group = group.clone();
                        move |_event, window, cx| {
                            nip29_ui::remove_member(group.clone(), public_key, window, cx);
                        }
                    }))
            })
            .into_any_element()
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

        v_flex()
            .image_cache(retain_all("sidebar"))
            .size_full()
            .relative()
            .gap_2()
            .bg(cx.theme().surface_background)
            .border_r_1()
            .border_color(cx.theme().border_variant)
            .when_some(current_user.as_ref(), |this, current_user| {
                this.child(title_bar_drag_handlers(
                    self.render_user(current_user, cx),
                    window,
                    cx,
                ))
            })
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
                    .child(
                        v_flex()
                            .size_full()
                            .justify_end()
                            .gap_4()
                            .p_4()
                            .child(img("brand/headline.png").max_w_48())
                            .child(
                                Button::new("import")
                                    .label("Import Identity")
                                    .custom(
                                        ButtonCustomVariant::new(window, cx)
                                            .color(gpui::white())
                                            .foreground(gpui::black())
                                            .hover(gpui::white().opacity(0.9))
                                            .active(gpui::white().opacity(0.8)),
                                    )
                                    .large()
                                    .font_semibold()
                                    .on_click(|_, window, cx| {
                                        import::open(window, cx);
                                    }),
                            ),
                    )
            })
            .map(|this| match group {
                Some(group) if self.group_open => this.child(self.render_group(group, cx)),
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
