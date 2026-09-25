use std::ops::Range;
use std::rc::Rc;

use auto_update::AutoUpdater;
use chat::{ChatEvent, ChatRegistry, Room, RoomKind};
use common::TimestampExt;
use community::{ChannelId, Community, CommunityEvent, CommunityRegistry};
use gpui::prelude::FluentBuilder;
use gpui::{
    AnyElement, App, Context, Div, Entity, EventEmitter, FocusHandle, Focusable,
    InteractiveElement, IntoElement, ObjectFit, ParentElement, Render, SharedString, Stateful,
    Styled, StyledImage, Subscription, UniformListScrollHandle, WeakEntity, Window, div, img, px,
    retain_all, uniform_list,
};
use nostr_sdk::prelude::*;
use person::PersonRegistry;
use smallvec::{SmallVec, smallvec};
use state::{NostrRegistry, StateEvent};
use theme::{ActiveTheme, TABBAR_HEIGHT};
use ui::avatar::Avatar;
use ui::button::{Button, ButtonCustomVariant, ButtonVariants};
use ui::dock::{DockArea, DockPlacement, Panel, PanelEvent, PanelHandle};
use ui::indicator::Indicator;
use ui::menu::{DropdownMenu, PopupMenuItem};
use ui::nav::Nav;
use ui::nav_item::NavItem;
use ui::notification::Notification;
use ui::scroll::Scrollbar;
use ui::tab::Tab;
use ui::tab::tab_bar::TabBar;
use ui::{
    Disableable, Icon, IconName, Selectable, Sizable, StyledExt, TRAFFIC_LIGHT_PADDING,
    WindowExtension, h_flex, title_bar_drag_handlers, v_flex,
};

use crate::Command;
use crate::dialogs::import;

mod tab;
mod utils;

use tab::SidebarTab;
pub(crate) use utils::{nav_avatar, nav_icon, pick_banner};

pub enum SidebarRow {
    Room { room: Entity<Room> },
    Community { community: Entity<Community> },
}

/// A collapsible group of rows in the sidebar's community view.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum CommunitySection {
    Channels,
    Admins,
    Members,
}

/// A row in the sidebar's community view.
pub enum CommunityRow {
    Section(CommunitySection),
    Channel {
        id: ChannelId,
        name: SharedString,
        private: bool,
        selected: bool,
    },
    Member {
        public_key: PublicKey,
    },
}

pub struct Sidebar {
    focus_handle: FocusHandle,
    scroll_handles: [UniformListScrollHandle; 2],
    /// Scroll state of the community's channel and member lists
    community_scroll: UniformListScrollHandle,
    /// The dock the sidebar opens its panels in
    dock: WeakEntity<DockArea>,
    /// Background shown behind the signed-out screen, picked at random
    banner: SharedString,
    active_tab: SidebarTab,
    /// The community the sidebar is browsing, if any
    community: Option<WeakEntity<Community>>,
    channels_open: bool,
    admins_open: bool,
    members_open: bool,
    new_requests: bool,
    _subscriptions: SmallVec<[Subscription; 4]>,
}

impl Sidebar {
    pub fn new(window: &mut Window, dock: WeakEntity<DockArea>, cx: &mut Context<Self>) -> Self {
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
                    window.push_notification(Notification::error(error.clone()), cx);
                }
            },
        ));

        subscriptions.push(
            cx.subscribe_in(&nostr, window, |this, _nostr, event, _window, cx| {
                // Re-pick the background each time the signed-out screen is shown.
                if let StateEvent::NoSigner = event {
                    this.banner = pick_banner();
                    cx.notify();
                }
            }),
        );

        Self {
            focus_handle: cx.focus_handle(),
            scroll_handles: [
                UniformListScrollHandle::new(),
                UniformListScrollHandle::new(),
            ],
            community_scroll: UniformListScrollHandle::new(),
            dock,
            banner: pick_banner(),
            active_tab: SidebarTab::Inbox,
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

    /// Leave the community view, returning the sidebar to its tab list.
    fn reset_community(&mut self, cx: &mut Context<Self>) {
        if self.community.take().is_none() {
            return;
        }
        cx.notify();
    }

    fn render_user(&self, current_user: &PublicKey, cx: &mut Context<Self>) -> Stateful<Div> {
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
            .child(div().flex_1())
            .when_some(AutoUpdater::try_global(cx), |this, updater| {
                this.child(self.render_updater(updater, cx))
            })
            .when(self.community.is_some(), |this| {
                this.child(
                    Button::new("sidebar-back")
                        .icon(IconName::ArrowLeft)
                        .tooltip("Back")
                        .ghost()
                        .small()
                        .on_click(cx.listener(|this, _event, _window, cx| {
                            this.reset_community(cx);
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

    fn render_tabs(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let sidebar = cx.entity().downgrade();
        let active_tab = self.active_tab;
        let rows = Rc::new(self.rows_for(active_tab, cx));
        let scroll_handle = &self.scroll_handles[active_tab.index()];

        v_flex()
            .size_full()
            .flex_1()
            .min_h_0()
            .gap_2()
            .child(
                div().px_2().child(
                    TabBar::new("sidebar-tabs")
                        .segmented(true)
                        .selected_index(active_tab.index())
                        .child(Tab::new().label(SidebarTab::Inbox.label()))
                        .child(Tab::new().label(SidebarTab::Communities.label()))
                        .on_click({
                            let sidebar = sidebar.clone();
                            move |index, _window, cx| {
                                let Some(tab) = SidebarTab::ALL.get(*index).copied() else {
                                    return;
                                };
                                if let Err(error) =
                                    sidebar.update(cx, |this, cx| this.select_tab(tab, cx))
                                {
                                    log::error!("Failed to switch sidebar tab: {error}");
                                }
                            }
                        }),
                ),
            )
            .map(|this| match active_tab {
                SidebarTab::Inbox => this.child(
                    v_flex()
                        .px_2()
                        .gap_1()
                        .child(
                            NavItem::new(
                                "new-chat",
                                "New Chat",
                                Icon::new(IconName::Message).small(),
                            )
                            .on_click(|_event, window, cx| {
                                window.dispatch_action(Box::new(Command::NewChat), cx)
                            }),
                        )
                        .child(
                            NavItem::new("reqs", "Requests", Icon::new(IconName::Invite).small())
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
                            NavItem::new("contacts", "Contacts", Icon::new(IconName::Book).small())
                                .on_click(|_event, window, cx| {
                                    window.dispatch_action(Box::new(Command::ShowContactList), cx)
                                }),
                        ),
                ),
                SidebarTab::Communities => this.child(
                    v_flex()
                        .px_2()
                        .gap_1()
                        .child(
                            NavItem::new(
                                "new-community",
                                "New Community",
                                Icon::new(IconName::Group).small(),
                            )
                            .on_click(|_, window, cx| {
                                window.dispatch_action(Box::new(Command::NewCommunity), cx)
                            }),
                        )
                        .child(
                            NavItem::new("browse", "Browse", Icon::new(IconName::Compass).small())
                                .on_click(|_, window, cx| {
                                    window.dispatch_action(Box::new(Command::ShowBrowse), cx)
                                }),
                        ),
                ),
            })
            .child(
                div()
                    .px_4()
                    .text_xs()
                    .font_semibold()
                    .text_color(cx.theme().text_placeholder)
                    .child(active_tab.list_title()),
            )
            .child(
                div()
                    .min_h_0()
                    .flex_1()
                    .child(
                        uniform_list(
                            active_tab.list_id(),
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
            .into_any_element()
    }

    fn render_community(
        &mut self,
        community: Entity<Community>,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let (banner, rows) = {
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

            let active = community.active_channel();
            let banner = community.banner();

            let mut rows = vec![CommunityRow::Section(CommunitySection::Channels)];
            if self.channels_open {
                rows.extend(
                    community
                        .channels()
                        .iter()
                        .map(|channel| CommunityRow::Channel {
                            id: channel.id,
                            name: channel.name.clone().into(),
                            private: channel.private,
                            selected: active == Some(channel.id),
                        }),
                );
            }

            rows.push(CommunityRow::Section(CommunitySection::Admins));
            if self.admins_open {
                rows.extend(
                    admins
                        .into_iter()
                        .map(|public_key| CommunityRow::Member { public_key }),
                );
            }

            rows.push(CommunityRow::Section(CommunitySection::Members));
            if self.members_open {
                rows.extend(
                    members
                        .into_iter()
                        .map(|public_key| CommunityRow::Member { public_key }),
                );
            }

            (banner, rows)
        };

        let rows = Rc::new(rows);

        v_flex()
            .flex_1()
            .min_h_0()
            .w_full()
            .gap_2()
            .when_some(banner, |this, banner| {
                this.child(
                    div().px_2().flex_shrink_0().child(
                        img(banner)
                            .w_full()
                            .h_20()
                            .rounded(cx.theme().radius_lg)
                            .object_fit(ObjectFit::Cover),
                    ),
                )
            })
            .child(
                div()
                    .min_h_0()
                    .flex_1()
                    .child(
                        uniform_list(
                            "community-rows",
                            rows.len(),
                            cx.processor(move |this, range, _window, cx| {
                                this.render_community_rows(range, rows.as_slice(), &community, cx)
                            }),
                        )
                        .track_scroll(&self.community_scroll)
                        .h_full()
                        .px_2(),
                    )
                    .child(Scrollbar::vertical(&self.community_scroll)),
            )
            .into_any_element()
    }

    fn render_community_rows(
        &self,
        range: Range<usize>,
        rows: &[CommunityRow],
        community: &Entity<Community>,
        cx: &mut Context<Self>,
    ) -> Vec<AnyElement> {
        rows.get(range)
            .into_iter()
            .flatten()
            .map(|row| match row {
                CommunityRow::Section(section) => self.section_row(section, cx),
                CommunityRow::Member { public_key } => self.member_row(public_key, cx),
                CommunityRow::Channel {
                    id,
                    name,
                    private,
                    selected,
                } => self.channel_row(*id, name.clone(), *private, *selected, community, cx),
            })
            .collect()
    }

    fn rows_for(&self, tab: SidebarTab, cx: &App) -> Vec<SidebarRow> {
        match tab {
            SidebarTab::Inbox => {
                let chat = ChatRegistry::global(cx);
                chat.read(cx)
                    .rooms(&RoomKind::Ongoing, cx)
                    .into_iter()
                    .map(|room| SidebarRow::Room { room })
                    .collect()
            }
            SidebarTab::Communities => {
                let registry = CommunityRegistry::global(cx);
                registry
                    .read(cx)
                    .communities()
                    .iter()
                    .cloned()
                    .map(|community| SidebarRow::Community { community })
                    .collect()
            }
        }
    }

    fn render_rows(
        &self,
        range: Range<usize>,
        rows: &[SidebarRow],
        cx: &Context<Sidebar>,
    ) -> Vec<AnyElement> {
        rows.get(range.clone())
            .into_iter()
            .flatten()
            .enumerate()
            .map(|(offset, row)| {
                let index = range.start + offset;

                match row {
                    SidebarRow::Room { room } => {
                        let name = room.read(cx).display_name(cx);
                        let picture = room.read(cx).display_image(cx);
                        let seed = room.read(cx).display_image_seed(cx);
                        let created_at = room.read(cx).created_at.to_ago();
                        let dock = self.dock.clone();
                        let room = room.clone();

                        Nav::new(SharedString::from(format!("room-{index}")))
                            .label(name)
                            .text_sm()
                            .font_medium()
                            .when_some(nav_avatar(Some(seed), picture, cx), |this, avatar| {
                                this.prefix(avatar)
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
                            })
                            .into_any_element()
                    }
                    SidebarRow::Community { community } => {
                        let name = community.read(cx).name();
                        let seed = community.read(cx).id().to_hex();
                        let picture = community.read(cx).icon();
                        let dock = self.dock.clone();
                        let sidebar = cx.entity().downgrade();
                        let community = community.clone();

                        Nav::new(SharedString::from(format!("com-{index}")))
                            .label(name)
                            .text_sm()
                            .when_some(nav_avatar(Some(seed), picture, cx), |this, avatar| {
                                this.prefix(avatar)
                            })
                            .on_click(move |_event, window, cx| {
                                ui::dock::add_panel_to(
                                    &dock,
                                    PanelHandle::new(community_ui::init(
                                        community.clone(),
                                        window,
                                        cx,
                                    )),
                                    DockPlacement::Center,
                                    window,
                                    cx,
                                );

                                if let Err(error) = sidebar.update(cx, |this, cx| {
                                    this.community = Some(community.downgrade());
                                    cx.notify();
                                }) {
                                    log::error!("Failed to show community in sidebar: {error}");
                                }
                            })
                            .into_any_element()
                    }
                }
            })
            .collect()
    }

    fn section_row(&self, section: &CommunitySection, cx: &mut Context<Sidebar>) -> AnyElement {
        let section = *section;
        let (label, open) = match section {
            CommunitySection::Channels => ("Channels", self.channels_open),
            CommunitySection::Admins => ("Admins", self.admins_open),
            CommunitySection::Members => ("Members", self.members_open),
        };
        let icon = if open {
            IconName::CaretDown
        } else {
            IconName::CaretRight
        };

        Nav::new(label)
            .label(label)
            .suffix(nav_icon(icon, cx))
            .text_xs()
            .font_semibold()
            .text_color(cx.theme().text_placeholder)
            .on_click(cx.listener(move |this, _ev, _window, cx| {
                match section {
                    CommunitySection::Channels => this.channels_open = !this.channels_open,
                    CommunitySection::Admins => this.admins_open = !this.admins_open,
                    CommunitySection::Members => this.members_open = !this.members_open,
                }
                cx.notify();
            }))
            .into_any_element()
    }

    fn channel_row(
        &self,
        id: ChannelId,
        name: SharedString,
        private: bool,
        selected: bool,
        community: &Entity<Community>,
        cx: &mut Context<Sidebar>,
    ) -> AnyElement {
        let community = community.clone();
        let icon = if private {
            IconName::Lock
        } else {
            IconName::Hashtag
        };

        Nav::new(id.to_hex())
            .label(name)
            .prefix(nav_icon(icon, cx))
            .text_sm()
            .font_medium()
            .selected(selected)
            .on_click(cx.listener(move |_this, _event, _window, cx| {
                community.update(cx, |community, cx| {
                    community.set_active_channel(id, cx);
                });
                cx.notify();
            }))
            .into_any_element()
    }

    fn member_row(&self, public_key: &PublicKey, cx: &App) -> AnyElement {
        let persons = PersonRegistry::global(cx);
        let person = persons.read(cx).get(public_key, cx);

        Nav::new(public_key.to_hex())
            .label(person.name())
            .text_sm()
            .font_medium()
            .when_some(
                nav_avatar(Some(person.avatar_seed()), person.avatar(), cx),
                |this, avatar| this.prefix(avatar),
            )
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

        let community = self
            .community
            .clone()
            .and_then(|community| community.upgrade());

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
            .map(|this| match community {
                Some(community) => this.child(self.render_community(community, cx)),
                None => this.child(self.render_tabs(cx)),
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
