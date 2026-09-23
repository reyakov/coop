use std::ops::Range;
use std::rc::Rc;

use auto_update::AutoUpdater;
use chat::{ChatEvent, ChatRegistry, RoomKind};
use common::TimestampExt;
use community::{ChannelId, Community, CommunityEvent, CommunityRegistry};
use gpui::prelude::FluentBuilder;
use gpui::{
    AnyElement, App, Context, ElementId, Entity, EventEmitter, FocusHandle, Focusable,
    InteractiveElement, IntoElement, ObjectFit, ParentElement, Render, ScrollHandle, SharedString,
    StatefulInteractiveElement, Styled, StyledImage, Subscription, UniformListScrollHandle,
    WeakEntity, Window, div, img, px, retain_all, uniform_list,
};
use nostr_sdk::prelude::*;
use person::PersonRegistry;
use settings::AppSettings;
use smallvec::{SmallVec, smallvec};
use state::NostrRegistry;
use theme::{ActiveTheme, TABBAR_HEIGHT};
use ui::avatar::Avatar;
use ui::button::{Button, ButtonVariants};
use ui::dock::{Panel, PanelEvent};
use ui::indicator::Indicator;
use ui::menu::{DropdownMenu, PopupMenuItem};
use ui::nav_item::NavItem;
use ui::notification::Notification;
use ui::scroll::Scrollbar;
use ui::{
    Icon, IconName, Sizable, StyledExt, TRAFFIC_LIGHT_PADDING, WindowExtension, h_flex,
    title_bar_drag_handlers, v_flex,
};

use crate::Command;

mod onboarding;
mod tab;
mod tree;

use tab::{SidebarTab, TabBar};
use tree::SidebarRow;
pub(crate) use tree::{TreeRow, TreeRowKind};

pub struct Sidebar {
    focus_handle: FocusHandle,
    scroll_handles: [UniformListScrollHandle; 3],
    /// Scroll state of the channel and member lists
    community_scroll: ScrollHandle,
    active_tab: SidebarTab,
    /// The community the sidebar is browsing, if any
    community: Option<WeakEntity<Community>>,
    /// Expanded state of the community's sections
    channels_open: bool,
    admins_open: bool,
    members_open: bool,
    /// Whether there are new chat requests
    new_requests: bool,
    _subscriptions: SmallVec<[Subscription; 4]>,
}

impl Sidebar {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let chat = ChatRegistry::global(cx);
        let communities = CommunityRegistry::global(cx);
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

        subscriptions.push(cx.subscribe_in(
            &communities,
            window,
            |_this, _communities, event, window, cx| {
                if let CommunityEvent::Error(error) = event {
                    window
                        .push_notification(Notification::error(error.clone()).autohide(false), cx);
                }
            },
        ));

        subscriptions.push(cx.observe(&nostr, |_this, _nostr, cx| cx.notify()));

        subscriptions.push(cx.observe(&communities, |_this, _communities, cx| cx.notify()));

        Self {
            focus_handle: cx.focus_handle(),
            scroll_handles: [
                UniformListScrollHandle::new(),
                UniformListScrollHandle::new(),
                UniformListScrollHandle::new(),
            ],
            community_scroll: ScrollHandle::default(),
            active_tab: SidebarTab::Recents,
            community: None,
            channels_open: true,
            admins_open: true,
            members_open: true,
            new_requests: false,
            _subscriptions: subscriptions,
        }
    }

    fn select_tab(&mut self, tab: SidebarTab, cx: &mut Context<Self>) {
        if self.active_tab == tab {
            return;
        }

        self.active_tab = tab;
        cx.notify();
    }

    fn open_community(
        &mut self,
        community: Entity<Community>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let id = community.read(cx).id().to_hex();
        let settings = AppSettings::global(cx);

        settings.update(cx, |settings, cx| {
            settings.record_recent_community(id, cx);
        });

        CommunityRegistry::global(cx).update(cx, |registry, cx| {
            registry.emit_community(&community, window, cx);
        });

        self.community = Some(community.downgrade());
        cx.notify();
    }

    fn close_community(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(community) = self.community.take() else {
            return;
        };

        if let Ok(id) = community.read_with(cx, |community, _cx| community.id()) {
            CommunityRegistry::global(cx).update(cx, |registry, cx| {
                registry.emit_close(id, window, cx);
            });
        }

        cx.notify();
    }

    fn render_user(&self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let nostr = NostrRegistry::global(cx);
        let current_user = nostr.read(cx).current_user();

        let mut row = h_flex()
            .id("sidebar-user")
            .w_full()
            .h(TABBAR_HEIGHT)
            .flex_shrink_0()
            .items_center()
            .gap_2()
            .px_2()
            .when(cfg!(target_os = "macos"), |this| {
                this.pl(px(TRAFFIC_LIGHT_PADDING))
            });

        if let Some(public_key) = current_user.as_ref() {
            let persons = PersonRegistry::global(cx);
            let profile = persons.read(cx).get(public_key, cx);
            let avatar = profile.avatar();
            let avatar_seed = profile.avatar_seed();
            let name = profile.name();

            row = row.child(
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
            );
        }

        if self.community.is_some() {
            row = row.child(div().flex_1()).child(
                Button::new("sidebar-back")
                    .icon(IconName::ArrowLeft)
                    .tooltip("Back")
                    .ghost()
                    .small()
                    .on_click(cx.listener(|this, _event, window, cx| {
                        this.close_community(window, cx);
                    })),
            );
        }

        title_bar_drag_handlers(row, window, cx).into_any_element()
    }
}

fn recent_communities(cx: &App) -> Vec<Entity<Community>> {
    const LIMIT: usize = 3;

    let registry = CommunityRegistry::global(cx);
    let communities = registry.read(cx).communities();
    let recent = AppSettings::get_recent_communities(cx);

    let mut rows: Vec<Entity<Community>> = recent
        .iter()
        .filter_map(|id| {
            communities
                .iter()
                .find(|community| community.read(cx).id().to_hex() == *id)
        })
        .take(LIMIT)
        .cloned()
        .collect();

    if rows.is_empty() {
        rows = communities.iter().take(LIMIT).cloned().collect();
    }

    rows
}

fn rows_for(tab: SidebarTab, cx: &App) -> Vec<SidebarRow> {
    match tab {
        SidebarTab::Recents => {
            let chat = ChatRegistry::global(cx);
            let rooms = chat.read(cx).rooms(&RoomKind::Ongoing, cx);
            let registry = CommunityRegistry::global(cx);
            let community_count = registry.read(cx).communities().len();
            let communities = recent_communities(cx);

            if communities.is_empty() && rooms.is_empty() {
                return vec![SidebarRow::Hint {
                    text: "Nothing recent yet".into(),
                }];
            }

            let mut rows = Vec::new();

            if !communities.is_empty() {
                rows.push(SidebarRow::Section {
                    label: "Communities".into(),
                    count: community_count,
                });
                rows.extend(
                    communities
                        .into_iter()
                        .map(|community| SidebarRow::Community { community }),
                );
                rows.push(SidebarRow::Action {
                    label: "Show all communities".into(),
                    tab: SidebarTab::Communities,
                });
            }

            if !rooms.is_empty() {
                rows.push(SidebarRow::Section {
                    label: "Chats".into(),
                    count: rooms.len(),
                });
                rows.extend(
                    rooms
                        .into_iter()
                        .take(5)
                        .map(|room| SidebarRow::Room { room }),
                );
                rows.push(SidebarRow::Action {
                    label: "Show all chats".into(),
                    tab: SidebarTab::Chats,
                });
            }

            rows
        }
        SidebarTab::Chats => {
            let chat = ChatRegistry::global(cx);
            let chat = chat.read(cx);
            let messages = chat.rooms(&RoomKind::Ongoing, cx);

            let mut rows = vec![SidebarRow::Section {
                label: "Chats".into(),
                count: messages.len(),
            }];

            if messages.is_empty() {
                rows.push(SidebarRow::Hint {
                    text: "No conversations yet".into(),
                });
            } else {
                rows.extend(messages.into_iter().map(|room| SidebarRow::Room { room }));
            }

            rows
        }
        SidebarTab::Communities => {
            let registry = CommunityRegistry::global(cx);
            let communities = registry.read(cx).communities();

            let mut rows = vec![SidebarRow::Section {
                label: "Communities".into(),
                count: communities.len(),
            }];

            if communities.is_empty() {
                rows.push(SidebarRow::Hint {
                    text: "No communities yet".into(),
                });
            } else {
                rows.extend(
                    communities
                        .iter()
                        .cloned()
                        .map(|community| SidebarRow::Community { community }),
                );
            }

            rows
        }
    }
}

fn render_rows(range: Range<usize>, rows: &[SidebarRow], cx: &Context<Sidebar>) -> Vec<AnyElement> {
    rows.get(range.clone())
        .into_iter()
        .flatten()
        .enumerate()
        .map(|(offset, row)| {
            let index = range.start + offset;

            match row {
                SidebarRow::Section { label, count } => TreeRow::new(
                    ElementId::NamedInteger("tree-row".into(), index as u64),
                    TreeRowKind::Section,
                    label.clone(),
                )
                .count(*count)
                .into_any_element(),
                SidebarRow::Room { room } => {
                    let name = room.read(cx).display_name(cx);
                    let picture = room.read(cx).display_image(cx);
                    let seed = room.read(cx).display_image_seed(cx);
                    let created_at = room.read(cx).created_at.to_ago();
                    let room_clone = room.clone();

                    let handler = cx.listener(move |_this, _event, window, cx| {
                        ChatRegistry::global(cx).update(cx, |chat, cx| {
                            chat.emit_room(&room_clone, window, cx);
                        });
                    });

                    TreeRow::new(
                        ElementId::NamedInteger("tree-row".into(), index as u64),
                        TreeRowKind::Room,
                        name,
                    )
                    .avatar(seed)
                    .picture(picture)
                    .created_at(created_at)
                    .on_click(handler)
                    .into_any_element()
                }
                SidebarRow::Community { community } => {
                    let name = community.read(cx).name();
                    let seed = community.read(cx).id().to_hex();
                    let picture = community.read(cx).icon();
                    let community = community.clone();

                    TreeRow::new(
                        ElementId::NamedInteger("tree-row".into(), index as u64),
                        TreeRowKind::Community,
                        name,
                    )
                    .avatar(seed)
                    .picture(picture)
                    .on_click(cx.listener(move |this, _event, window, cx| {
                        this.open_community(community.clone(), window, cx);
                    }))
                    .into_any_element()
                }
                SidebarRow::Action { label, tab } => {
                    let tab = *tab;

                    TreeRow::new(
                        ElementId::NamedInteger("tree-row".into(), index as u64),
                        TreeRowKind::Action,
                        label.clone(),
                    )
                    .icon(IconName::ArrowRight)
                    .on_click(cx.listener(move |this, _event, _window, cx| {
                        this.select_tab(tab, cx);
                    }))
                    .into_any_element()
                }
                SidebarRow::Hint { text } => TreeRow::new(
                    ElementId::NamedInteger("tree-row".into(), index as u64),
                    TreeRowKind::Hint,
                    text.clone(),
                )
                .into_any_element(),
            }
        })
        .collect()
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
        let (logged_in, ready) = {
            let nostr = NostrRegistry::global(cx);
            (
                nostr.read(cx).current_user().is_some(),
                nostr.read(cx).ready(),
            )
        };

        if !logged_in {
            if !ready {
                return v_flex()
                    .size_full()
                    .bg(cx.theme().surface_background)
                    .border_r_1()
                    .border_color(cx.theme().border_variant)
                    .into_any_element();
            }
            return onboarding::render(window, cx).into_any_element();
        }

        let community = self
            .community
            .clone()
            .and_then(|community| community.upgrade());

        if community.is_none() {
            self.community = None;
        }

        v_flex()
            .image_cache(retain_all("sidebar"))
            .size_full()
            .relative()
            .gap_2()
            .bg(cx.theme().surface_background)
            .border_r_1()
            .border_color(cx.theme().border_variant)
            .child(self.render_user(window, cx))
            .map(|this| match community {
                Some(community) => this.child(self.render_community(community, cx)),
                None => this.child(self.render_tabs(cx)),
            })
            .into_any_element()
    }
}

impl Sidebar {
    fn render_tabs(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let chat = ChatRegistry::global(cx);
        let loading = chat.read(cx).loading();

        let sidebar = cx.entity().downgrade();
        let active_tab = self.active_tab;
        let rows = Rc::new(rows_for(active_tab, cx));
        let scroll_handle = &self.scroll_handles[active_tab.index()];

        v_flex()
            .size_full()
            .flex_1()
            .min_h_0()
            .gap_1()
            .when(active_tab.chat(), |this| {
                this.child(
                    v_flex()
                        .px_2()
                        .gap_1()
                        .child(
                            NavItem::new(
                                "nav-contacts",
                                "Contacts",
                                Icon::new(IconName::Book).small(),
                            )
                            .on_click(|_event, window, cx| {
                                window.dispatch_action(Box::new(Command::ShowContactList), cx)
                            }),
                        )
                        .child(
                            NavItem::new(
                                "nav-requests",
                                "Requests",
                                Icon::new(IconName::Invite).small(),
                            )
                            .when(self.new_requests, |this| {
                                this.suffix(div().size_1().rounded_full().bg(cx.theme().cursor))
                            })
                            .on_click({
                                let sidebar = sidebar.clone();
                                move |_event, window, cx| {
                                    if let Err(error) = sidebar.update(cx, |this, cx| {
                                        this.new_requests = false;
                                        cx.notify();
                                    }) {
                                        log::error!("Failed to clear new requests: {error}");
                                    }
                                    window.dispatch_action(Box::new(Command::ShowRequests), cx);
                                }
                            }),
                        )
                        .child(
                            NavItem::new(
                                "nav-new-chat",
                                "New chat",
                                Icon::new(IconName::Plus).small(),
                            )
                            .on_click(|_event, window, cx| {
                                window.dispatch_action(Box::new(Command::NewChat), cx)
                            }),
                        ),
                )
            })
            .when(active_tab.community(), |this| {
                this.child(
                    v_flex()
                        .px_2()
                        .gap_1()
                        .child(
                            NavItem::new(
                                "nav-browse",
                                "Browse",
                                Icon::new(IconName::Compass).small(),
                            )
                            .on_click(|_event, window, cx| {
                                window.dispatch_action(Box::new(Command::ShowBrowse), cx)
                            }),
                        )
                        .child(
                            NavItem::new(
                                "nav-new-community",
                                "New community",
                                Icon::new(IconName::Plus).small(),
                            )
                            .on_click(|_event, window, cx| {
                                window.dispatch_action(Box::new(Command::NewCommunity), cx)
                            }),
                        ),
                )
            })
            .child(
                v_flex()
                    .size_full()
                    .flex_1()
                    .min_h_0()
                    .gap_1()
                    .pb_12()
                    .child(
                        uniform_list(
                            active_tab.list_id(),
                            rows.len(),
                            cx.processor(move |_this, range, _window, cx| {
                                render_rows(range, rows.as_slice(), cx)
                            }),
                        )
                        .track_scroll(scroll_handle)
                        .flex_1()
                        .h_full()
                        .px_2(),
                    )
                    .child(Scrollbar::vertical(scroll_handle)),
            )
            .child(TabBar::new(active_tab).on_select({
                let sidebar = sidebar.clone();
                move |tab, _window, cx| {
                    if let Err(error) = sidebar.update(cx, |this, cx| this.select_tab(tab, cx)) {
                        log::error!("Failed to switch sidebar tab: {error}");
                    }
                }
            }))
            .when(loading, |this| {
                this.child(
                    div()
                        .absolute()
                        .bottom_16()
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

    fn render_community(
        &mut self,
        community: Entity<Community>,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let (channels, active, admins, members, banner) = {
            let community = community.read(cx);
            let owner = community.state().owner;
            let mut admins = Vec::new();
            let mut members = Vec::new();

            for public_key in community.members() {
                if community.control().roles.is_staff(public_key, &owner) {
                    admins.push(*public_key);
                } else {
                    members.push(*public_key);
                }
            }

            (
                community
                    .channels()
                    .iter()
                    .map(|channel| (channel.id, channel.name.clone(), channel.private))
                    .collect::<Vec<_>>(),
                community.active_channel(),
                admins,
                members,
                community.banner(),
            )
        };

        let sections = v_flex()
            .id("community-sections")
            .flex_1()
            .min_h_0()
            .w_full()
            .px_2()
            .pb_2()
            .track_scroll(&self.community_scroll)
            .overflow_y_scroll()
            .child(section_row(
                "channels",
                "Channels",
                channels.len(),
                self.channels_open,
                |sidebar| sidebar.channels_open = !sidebar.channels_open,
                cx,
            ))
            .when(self.channels_open, |this| {
                this.children(channels.into_iter().map(|(id, name, private)| {
                    channel_row(id, name, private, active == Some(id), &community, cx)
                }))
            })
            .child(section_row(
                "admins",
                "Admins",
                admins.len(),
                self.admins_open,
                |sidebar| sidebar.admins_open = !sidebar.admins_open,
                cx,
            ))
            .when(self.admins_open, |this| {
                this.children(
                    admins
                        .iter()
                        .map(|public_key| member_row("community-admin", *public_key, cx)),
                )
            })
            .child(section_row(
                "members",
                "Members",
                members.len(),
                self.members_open,
                |sidebar| sidebar.members_open = !sidebar.members_open,
                cx,
            ))
            .when(self.members_open, |this| {
                this.children(
                    members
                        .iter()
                        .map(|public_key| member_row("member", *public_key, cx)),
                )
            });

        v_flex()
            .flex_1()
            .min_h_0()
            .w_full()
            .gap_2()
            .when_some(banner, |this, banner| {
                this.child(
                    div().px_2().child(
                        img(banner)
                            .w_full()
                            .h(px(80.))
                            .rounded(cx.theme().radius)
                            .object_fit(ObjectFit::Cover),
                    ),
                )
            })
            .child(sections)
            .child(Scrollbar::vertical(&self.community_scroll))
            .into_any_element()
    }
}

fn section_row(
    id: &'static str,
    label: &'static str,
    count: usize,
    open: bool,
    toggle: impl Fn(&mut Sidebar) + 'static,
    cx: &mut Context<Sidebar>,
) -> AnyElement {
    div()
        .flex_shrink_0()
        .child(
            TreeRow::new(ElementId::Name(id.into()), TreeRowKind::Section, label)
                .icon(if open {
                    IconName::CaretDown
                } else {
                    IconName::CaretRight
                })
                .count(count)
                .on_click(cx.listener(move |this, _event, _window, cx| {
                    toggle(this);
                    cx.notify();
                })),
        )
        .into_any_element()
}

fn channel_row(
    id: ChannelId,
    name: String,
    private: bool,
    selected: bool,
    community: &Entity<Community>,
    cx: &mut Context<Sidebar>,
) -> AnyElement {
    let community = community.clone();

    div()
        .flex_shrink_0()
        .rounded(cx.theme().radius)
        .child(
            TreeRow::new(
                ElementId::Name(SharedString::from(format!(
                    "community-channel-{}",
                    id.to_hex()
                ))),
                TreeRowKind::Room,
                name,
            )
            .icon(if private {
                IconName::Lock
            } else {
                IconName::Message
            })
            .on_click(cx.listener(move |_this, _event, _window, cx| {
                community.update(cx, |community, cx| community.set_active_channel(id, cx));
                cx.notify();
            })),
        )
        .when(selected, |this| this.bg(cx.theme().ghost_element_active))
        .into_any_element()
}

fn member_row(prefix: &str, public_key: PublicKey, cx: &App) -> AnyElement {
    let persons = PersonRegistry::global(cx);
    let person = persons.read(cx).get(&public_key, cx);

    div()
        .flex_shrink_0()
        .child(
            TreeRow::new(
                ElementId::Name(SharedString::from(format!(
                    "{prefix}-{}",
                    public_key.to_hex()
                ))),
                TreeRowKind::Room,
                person.name(),
            )
            .avatar(person.avatar_seed())
            .picture(person.avatar()),
        )
        .into_any_element()
}
