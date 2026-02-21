use std::sync::Arc;

use chat::{ChatEvent, ChatRegistry};
use gpui::prelude::FluentBuilder;
use gpui::{
    div, rems, App, AppContext, Axis, Context, Entity, InteractiveElement, IntoElement,
    ParentElement, Render, SharedString, Styled, Subscription, Window,
};
use person::PersonRegistry;
use smallvec::{smallvec, SmallVec};
use state::{NostrRegistry, RelayState};
use theme::{ActiveTheme, Theme, SIDEBAR_WIDTH, TITLEBAR_HEIGHT};
use title_bar::TitleBar;
use ui::avatar::Avatar;
use ui::button::{Button, ButtonVariants};
use ui::dock_area::dock::DockPlacement;
use ui::dock_area::panel::{PanelStyle, PanelView};
use ui::dock_area::{ClosePanel, DockArea, DockItem};
use ui::menu::DropdownMenu;
use ui::{h_flex, v_flex, Root, Sizable, WindowExtension};

use crate::panels::greeter;
use crate::sidebar;

pub fn init(window: &mut Window, cx: &mut App) -> Entity<Workspace> {
    cx.new(|cx| Workspace::new(window, cx))
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
        let dock = cx.new(|cx| DockArea::new(window, cx).style(PanelStyle::TabBar));

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

    fn titlebar_left(&mut self, _window: &mut Window, cx: &Context<Self>) -> impl IntoElement {
        let chat = ChatRegistry::global(cx);
        let nostr = NostrRegistry::global(cx);
        let signer = nostr.read(cx).signer();
        let current_user = signer.public_key();

        h_flex()
            .h(TITLEBAR_HEIGHT)
            .flex_shrink_0()
            .justify_between()
            .gap_2()
            .when_some(current_user.as_ref(), |this, public_key| {
                let persons = PersonRegistry::global(cx);
                let profile = persons.read(cx).get(public_key, cx);

                this.child(
                    Button::new("current-user")
                        .child(Avatar::new(profile.avatar()).size(rems(1.25)))
                        .small()
                        .caret()
                        .compact()
                        .transparent()
                        .dropdown_menu(move |this, _window, _cx| {
                            this.label(profile.name())
                                .separator()
                                .menu("Profile", Box::new(ClosePanel))
                                .menu("Backup", Box::new(ClosePanel))
                                .menu("Themes", Box::new(ClosePanel))
                                .menu("Settings", Box::new(ClosePanel))
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
            .map(|this| match nostr.read(cx).relay_list_state() {
                RelayState::Checking => this.child(
                    div()
                        .text_xs()
                        .text_color(cx.theme().text_muted)
                        .child(SharedString::from("Fetching user's relay list...")),
                ),
                RelayState::NotConfigured => this.child(
                    h_flex()
                        .h_6()
                        .w_full()
                        .px_1()
                        .text_xs()
                        .text_color(cx.theme().warning_foreground)
                        .bg(cx.theme().warning_background)
                        .rounded_sm()
                        .child(SharedString::from("User hasn't configured a relay list")),
                ),
                _ => this,
            })
            .map(|this| match chat.read(cx).relay_state(cx) {
                RelayState::Checking => {
                    this.child(div().text_xs().text_color(cx.theme().text_muted).child(
                        SharedString::from("Fetching user's messaging relay list..."),
                    ))
                }
                RelayState::NotConfigured => this.child(
                    h_flex()
                        .h_6()
                        .w_full()
                        .px_2()
                        .text_xs()
                        .text_color(cx.theme().warning_foreground)
                        .bg(cx.theme().warning_background)
                        .rounded_full()
                        .child(SharedString::from(
                            "User hasn't configured a messaging relay list",
                        )),
                ),
                _ => this,
            })
    }

    fn titlebar_right(&mut self, _window: &mut Window, _cx: &Context<Self>) -> impl IntoElement {
        h_flex().h(TITLEBAR_HEIGHT).flex_shrink_0()
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
