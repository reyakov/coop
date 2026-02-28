use std::sync::Arc;

use ::settings::AppSettings;
use chat::{ChatEvent, ChatRegistry, InboxState};
use gpui::prelude::FluentBuilder;
use gpui::{
    div, px, Action, App, AppContext, Axis, Context, Entity, InteractiveElement, IntoElement,
    ParentElement, Render, SharedString, Styled, Subscription, Window,
};
use person::PersonRegistry;
use serde::Deserialize;
use smallvec::{smallvec, SmallVec};
use state::{NostrRegistry, RelayState};
use theme::{ActiveTheme, Theme, ThemeRegistry, SIDEBAR_WIDTH};
use title_bar::TitleBar;
use ui::avatar::Avatar;
use ui::button::{Button, ButtonVariants};
use ui::dock_area::dock::DockPlacement;
use ui::dock_area::panel::PanelView;
use ui::dock_area::{ClosePanel, DockArea, DockItem};
use ui::menu::DropdownMenu;
use ui::{h_flex, v_flex, IconName, Root, Sizable, WindowExtension};

use crate::dialogs::settings;
use crate::panels::{
    backup, contact_list, encryption_key, greeter, messaging_relays, profile, relay_list,
};
use crate::sidebar;

pub fn init(window: &mut Window, cx: &mut App) -> Entity<Workspace> {
    cx.new(|cx| Workspace::new(window, cx))
}

#[derive(Action, Clone, PartialEq, Eq, Deserialize)]
#[action(namespace = workspace, no_json)]
enum Command {
    ToggleTheme,

    RefreshRelayList,
    RefreshMessagingRelays,

    ShowRelayList,
    ShowMessaging,
    ShowEncryption,
    ShowProfile,
    ShowSettings,
    ShowBackup,
    ShowContactList,
}

pub struct Workspace {
    /// App's Title Bar
    titlebar: Entity<TitleBar>,

    /// App's Dock Area
    dock: Entity<DockArea>,

    /// Event subscriptions
    _subscriptions: SmallVec<[Subscription; 3]>,
}

impl Workspace {
    fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let chat = ChatRegistry::global(cx);
        let titlebar = cx.new(|_| TitleBar::new());
        let dock = cx.new(|cx| DockArea::new(window, cx));

        let mut subscriptions = smallvec![];

        subscriptions.push(
            // Observe system appearance and update theme
            cx.observe_window_appearance(window, |_this, window, cx| {
                Theme::sync_system_appearance(Some(window), cx);
            }),
        );

        subscriptions.push(
            // Observe all events emitted by the chat registry
            cx.subscribe_in(&chat, window, move |this, chat, ev, window, cx| {
                match ev {
                    ChatEvent::OpenRoom(id) => {
                        if let Some(room) = chat.read(cx).room(id, cx) {
                            this.dock.update(cx, |this, cx| {
                                this.add_panel(
                                    Arc::new(chat_ui::init(room, window, cx)),
                                    DockPlacement::Center,
                                    window,
                                    cx,
                                );
                            });
                        }
                    }
                    ChatEvent::CloseRoom(..) => {
                        this.dock.update(cx, |this, cx| {
                            // Force focus to the tab panel
                            this.focus_tab_panel(window, cx);

                            // Dispatch the close panel action
                            cx.defer_in(window, |_, window, cx| {
                                window.dispatch_action(Box::new(ClosePanel), cx);
                                window.close_all_modals(cx);
                            });
                        });
                    }
                    _ => {}
                };
            }),
        );

        subscriptions.push(
            // Observe the chat registry
            cx.observe(&chat, move |this, chat, cx| {
                let ids = this.panel_ids(cx);

                chat.update(cx, |this, cx| {
                    this.refresh_rooms(ids, cx);
                });
            }),
        );

        // Set the default layout for app's dock
        cx.defer_in(window, |this, window, cx| {
            this.set_layout(window, cx);
        });

        Self {
            titlebar,
            dock,
            _subscriptions: subscriptions,
        }
    }

    /// Add panel to the dock
    pub fn add_panel<P>(panel: P, placement: DockPlacement, window: &mut Window, cx: &mut App)
    where
        P: PanelView,
    {
        if let Some(root) = window.root::<Root>().flatten() {
            if let Ok(workspace) = root.read(cx).view().clone().downcast::<Self>() {
                workspace.update(cx, |this, cx| {
                    this.dock.update(cx, |this, cx| {
                        this.add_panel(Arc::new(panel), placement, window, cx);
                    });
                });
            }
        }
    }

    /// Get all panel ids
    fn panel_ids(&self, cx: &App) -> Option<Vec<u64>> {
        let ids: Vec<u64> = self
            .dock
            .read(cx)
            .items
            .panel_ids(cx)
            .into_iter()
            .filter_map(|panel| panel.parse::<u64>().ok())
            .collect();

        Some(ids)
    }

    /// Set the dock layout
    fn set_layout(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let weak_dock = self.dock.downgrade();

        // Sidebar
        let left = DockItem::panel(Arc::new(sidebar::init(window, cx)));

        // Main workspace
        let center = DockItem::split_with_sizes(
            Axis::Vertical,
            vec![DockItem::tabs(
                vec![Arc::new(greeter::init(window, cx))],
                None,
                &weak_dock,
                window,
                cx,
            )],
            vec![None],
            &weak_dock,
            window,
            cx,
        );

        // Update the dock layout
        self.dock.update(cx, |this, cx| {
            this.set_left_dock(left, Some(SIDEBAR_WIDTH), true, window, cx);
            this.set_center(center, window, cx);
        });
    }

    fn on_command(&mut self, command: &Command, window: &mut Window, cx: &mut Context<Self>) {
        match command {
            Command::ShowSettings => {
                let view = settings::init(window, cx);

                window.open_modal(cx, move |this, _window, _cx| {
                    this.width(px(520.))
                        .show_close(true)
                        .pb_4()
                        .title("Preferences")
                        .child(view.clone())
                });
            }
            Command::ShowProfile => {
                let nostr = NostrRegistry::global(cx);
                let signer = nostr.read(cx).signer();

                if let Some(public_key) = signer.public_key() {
                    self.dock.update(cx, |this, cx| {
                        this.add_panel(
                            Arc::new(profile::init(public_key, window, cx)),
                            DockPlacement::Right,
                            window,
                            cx,
                        );
                    });
                }
            }
            Command::ShowContactList => {
                self.dock.update(cx, |this, cx| {
                    this.add_panel(
                        Arc::new(contact_list::init(window, cx)),
                        DockPlacement::Right,
                        window,
                        cx,
                    );
                });
            }
            Command::ShowBackup => {
                self.dock.update(cx, |this, cx| {
                    this.add_panel(
                        Arc::new(backup::init(window, cx)),
                        DockPlacement::Right,
                        window,
                        cx,
                    );
                });
            }
            Command::ShowEncryption => {
                let nostr = NostrRegistry::global(cx);
                let signer = nostr.read(cx).signer();

                if let Some(public_key) = signer.public_key() {
                    self.dock.update(cx, |this, cx| {
                        this.add_panel(
                            Arc::new(encryption_key::init(public_key, window, cx)),
                            DockPlacement::Right,
                            window,
                            cx,
                        );
                    });
                }
            }
            Command::ShowMessaging => {
                self.dock.update(cx, |this, cx| {
                    this.add_panel(
                        Arc::new(messaging_relays::init(window, cx)),
                        DockPlacement::Right,
                        window,
                        cx,
                    );
                });
            }
            Command::ShowRelayList => {
                self.dock.update(cx, |this, cx| {
                    this.add_panel(
                        Arc::new(relay_list::init(window, cx)),
                        DockPlacement::Right,
                        window,
                        cx,
                    );
                });
            }
            Command::RefreshRelayList => {
                let nostr = NostrRegistry::global(cx);
                nostr.update(cx, |this, cx| {
                    this.ensure_relay_list(cx);
                });
            }
            Command::RefreshMessagingRelays => {
                let chat = ChatRegistry::global(cx);
                chat.update(cx, |this, cx| {
                    this.ensure_messaging_relays(cx);
                });
            }
            Command::ToggleTheme => {
                self.theme_selector(window, cx);
            }
        }
    }

    fn theme_selector(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        window.open_modal(cx, move |this, _window, cx| {
            let registry = ThemeRegistry::global(cx);
            let themes = registry.read(cx).themes();

            this.width(px(520.))
                .show_close(true)
                .title("Select theme")
                .pb_4()
                .child(v_flex().gap_2().w_full().children({
                    let mut items = vec![];

                    for (ix, (path, theme)) in themes.iter().enumerate() {
                        items.push(
                            h_flex()
                                .group("")
                                .px_2()
                                .h_8()
                                .w_full()
                                .justify_between()
                                .rounded(cx.theme().radius)
                                .hover(|this| this.bg(cx.theme().elevated_surface_background))
                                .child(
                                    h_flex()
                                        .gap_1p5()
                                        .flex_1()
                                        .text_sm()
                                        .child(theme.name.clone())
                                        .child(
                                            div()
                                                .text_xs()
                                                .italic()
                                                .text_color(cx.theme().text_muted)
                                                .child(theme.author.clone()),
                                        ),
                                )
                                .child(
                                    h_flex()
                                        .gap_1()
                                        .invisible()
                                        .group_hover("", |this| this.visible())
                                        .child(
                                            Button::new(format!("url-{ix}"))
                                                .icon(IconName::Link)
                                                .ghost()
                                                .small()
                                                .on_click({
                                                    let theme = theme.clone();
                                                    move |_ev, _window, cx| {
                                                        cx.open_url(&theme.url);
                                                    }
                                                }),
                                        )
                                        .child(
                                            Button::new(format!("set-{ix}"))
                                                .icon(IconName::Check)
                                                .primary()
                                                .small()
                                                .on_click({
                                                    let path = path.clone();
                                                    move |_ev, window, cx| {
                                                        let settings = AppSettings::global(cx);
                                                        let path = path.clone();

                                                        settings.update(cx, |this, cx| {
                                                            this.set_theme(path, window, cx);
                                                        })
                                                    }
                                                }),
                                        ),
                                ),
                        );
                    }

                    items
                }))
        });
    }

    fn titlebar_left(&mut self, _window: &mut Window, cx: &Context<Self>) -> impl IntoElement {
        let nostr = NostrRegistry::global(cx);
        let signer = nostr.read(cx).signer();
        let current_user = signer.public_key();

        h_flex()
            .flex_shrink_0()
            .justify_between()
            .gap_2()
            .when_some(current_user.as_ref(), |this, public_key| {
                let persons = PersonRegistry::global(cx);
                let profile = persons.read(cx).get(public_key, cx);

                this.child(
                    Button::new("current-user")
                        .child(Avatar::new(profile.avatar()).xsmall())
                        .small()
                        .caret()
                        .compact()
                        .transparent()
                        .dropdown_menu(move |this, _window, _cx| {
                            this.min_w(px(256.))
                                .label(profile.name())
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
                                .separator()
                                .menu_with_icon(
                                    "Settings",
                                    IconName::Settings,
                                    Box::new(Command::ShowSettings),
                                )
                        }),
                )
            })
            .when(nostr.read(cx).creating(), |this| {
                this.child(div().text_xs().text_color(cx.theme().text_muted).child(
                    SharedString::from("Coop is creating a new identity for you..."),
                ))
            })
            .when(!nostr.read(cx).connected(), |this| {
                this.child(
                    div()
                        .text_xs()
                        .text_color(cx.theme().text_muted)
                        .child(SharedString::from("Connecting...")),
                )
            })
    }

    fn titlebar_right(&mut self, _window: &mut Window, cx: &Context<Self>) -> impl IntoElement {
        let nostr = NostrRegistry::global(cx);
        let signer = nostr.read(cx).signer();
        let relay_list = nostr.read(cx).relay_list_state();

        let chat = ChatRegistry::global(cx);
        let inbox_state = chat.read(cx).state(cx);

        let Some(pkey) = signer.public_key() else {
            return div();
        };

        h_flex()
            .when(!cx.theme().platform.is_mac(), |this| this.pr_2())
            .gap_3()
            .child(
                Button::new("key")
                    .icon(IconName::UserKey)
                    .tooltip("Decoupled encryption key")
                    .small()
                    .ghost()
                    .on_click(|_ev, window, cx| {
                        window.dispatch_action(Box::new(Command::ShowEncryption), cx);
                    }),
            )
            .child(
                h_flex()
                    .gap_2()
                    .child(
                        div()
                            .text_xs()
                            .text_color(cx.theme().text_muted)
                            .map(|this| match inbox_state {
                                InboxState::Checking => this.child(div().child(
                                    SharedString::from("Fetching user's messaging relay list..."),
                                )),
                                InboxState::RelayNotAvailable => {
                                    this.child(div().text_color(cx.theme().warning_active).child(
                                        SharedString::from(
                                            "User hasn't configured a messaging relay list",
                                        ),
                                    ))
                                }
                                _ => this,
                            }),
                    )
                    .child(
                        Button::new("inbox")
                            .icon(IconName::Inbox)
                            .tooltip("Inbox")
                            .small()
                            .ghost()
                            .when(inbox_state.subscribing(), |this| this.indicator())
                            .dropdown_menu(move |this, _window, _cx| {
                                this.min_w(px(260.))
                                    .label("Messaging Relays")
                                    .menu_element_with_disabled(
                                        Box::new(Command::ShowRelayList),
                                        true,
                                        move |_window, cx| {
                                            let persons = PersonRegistry::global(cx);
                                            let profile = persons.read(cx).get(&pkey, cx);
                                            let urls = profile.messaging_relays();

                                            v_flex()
                                                .gap_1()
                                                .w_full()
                                                .items_start()
                                                .justify_start()
                                                .children({
                                                    let mut items = vec![];

                                                    for url in urls.iter() {
                                                        items.push(
                                                            h_flex()
                                                                .h_6()
                                                                .w_full()
                                                                .gap_2()
                                                                .px_2()
                                                                .text_xs()
                                                                .bg(cx
                                                                    .theme()
                                                                    .elevated_surface_background)
                                                                .rounded(cx.theme().radius)
                                                                .child(
                                                                    div()
                                                                        .size_1()
                                                                        .rounded_full()
                                                                        .bg(gpui::green()),
                                                                )
                                                                .child(SharedString::from(
                                                                    url.to_string(),
                                                                )),
                                                        );
                                                    }

                                                    items
                                                })
                                        },
                                    )
                                    .separator()
                                    .menu_with_icon(
                                        "Reload",
                                        IconName::Refresh,
                                        Box::new(Command::RefreshMessagingRelays),
                                    )
                                    .menu_with_icon(
                                        "Update relays",
                                        IconName::Settings,
                                        Box::new(Command::ShowMessaging),
                                    )
                            }),
                    ),
            )
            .child(
                h_flex()
                    .gap_2()
                    .child(
                        div()
                            .text_xs()
                            .text_color(cx.theme().text_muted)
                            .map(|this| match relay_list {
                                RelayState::Checking => this
                                    .child(div().child(SharedString::from(
                                        "Fetching user's relay list...",
                                    ))),
                                RelayState::NotConfigured => {
                                    this.child(div().text_color(cx.theme().warning_active).child(
                                        SharedString::from("User hasn't configured a relay list"),
                                    ))
                                }
                                _ => this,
                            }),
                    )
                    .child(
                        Button::new("relay-list")
                            .icon(IconName::Relay)
                            .tooltip("User's relay list")
                            .small()
                            .ghost()
                            .when(relay_list.configured(), |this| this.indicator())
                            .dropdown_menu(move |this, _window, _cx| {
                                this.min_w(px(260.))
                                    .label("Relays")
                                    .menu_element_with_disabled(
                                        Box::new(Command::ShowRelayList),
                                        true,
                                        move |_window, cx| {
                                            let nostr = NostrRegistry::global(cx);
                                            let urls = nostr.read(cx).read_only_relays(&pkey, cx);

                                            v_flex()
                                                .gap_1()
                                                .w_full()
                                                .items_start()
                                                .justify_start()
                                                .children({
                                                    let mut items = vec![];

                                                    for url in urls.into_iter() {
                                                        items.push(
                                                            h_flex()
                                                                .h_6()
                                                                .w_full()
                                                                .gap_2()
                                                                .px_2()
                                                                .text_xs()
                                                                .bg(cx
                                                                    .theme()
                                                                    .elevated_surface_background)
                                                                .rounded(cx.theme().radius)
                                                                .child(
                                                                    div()
                                                                        .size_1()
                                                                        .rounded_full()
                                                                        .bg(gpui::green()),
                                                                )
                                                                .child(url),
                                                        );
                                                    }

                                                    items
                                                })
                                        },
                                    )
                                    .separator()
                                    .menu_with_icon(
                                        "Reload",
                                        IconName::Refresh,
                                        Box::new(Command::RefreshRelayList),
                                    )
                                    .menu_with_icon(
                                        "Update relay list",
                                        IconName::Settings,
                                        Box::new(Command::ShowRelayList),
                                    )
                            }),
                    ),
            )
    }
}

impl Render for Workspace {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let modal_layer = Root::render_modal_layer(window, cx);
        let notification_layer = Root::render_notification_layer(window, cx);

        // Titlebar elements
        let left = self.titlebar_left(window, cx).into_any_element();
        let right = self.titlebar_right(window, cx).into_any_element();

        // Update title bar children
        self.titlebar.update(cx, |this, _cx| {
            this.set_children(vec![left, right]);
        });

        div()
            .id(SharedString::from("workspace"))
            .on_action(cx.listener(Self::on_command))
            .relative()
            .size_full()
            .child(
                v_flex()
                    .relative()
                    .size_full()
                    // Title Bar
                    .child(self.titlebar.clone())
                    // Dock
                    .child(self.dock.clone()),
            )
            // Notifications
            .children(notification_layer)
            // Modals
            .children(modal_layer)
    }
}
