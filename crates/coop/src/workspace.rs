use std::cell::Cell;
use std::rc::Rc;
use std::sync::Arc;

use ::settings::AppSettings;
use chat::{ChatEvent, ChatRegistry};
use common::download_dir;
use device::{DeviceEvent, DeviceRegistry};
use gpui::prelude::FluentBuilder;
use gpui::{
    Action, App, AppContext, Axis, Context, Entity, InteractiveElement, IntoElement, ParentElement,
    Render, SharedString, StatefulInteractiveElement, Styled, Subscription, Window, div, px,
    relative,
};
use nostr_sdk::prelude::*;
use person::PersonRegistry;
use serde::Deserialize;
use smallvec::{SmallVec, smallvec};
use state::{NostrRegistry, StateEvent};
use theme::{ActiveTheme, SIDEBAR_WIDTH, Theme, ThemeRegistry};
use title_bar::TitleBar;
use ui::avatar::Avatar;
use ui::button::{Button, ButtonVariants};
use ui::dock::{ClosePanel, DockArea, DockItem, DockPlacement, PanelView};
use ui::menu::{DropdownMenu, PopupMenuItem};
use ui::notification::{Notification, NotificationKind};
use ui::{Disableable, Icon, IconName, Root, Sizable, WindowExtension, h_flex, v_flex};

use crate::dialogs::restore::RestoreEncryption;
use crate::dialogs::{accounts, settings};
use crate::panels::{backup, contact_list, greeter, messaging_relays, profile, relay_list, trash};
use crate::sidebar;

const PREPARE_MSG: &str = "Coop is preparing a new identity for you. This may take a moment...";
const ENC_MSG: &str = "Encryption Key is a special key that used to encrypt and decrypt your messages. \
                       Your identity is completely decoupled from all encryption processes to protect your privacy.";
const ENC_WARN: &str = "By resetting your encryption key, you will lose access to \
                        all your encrypted messages before. This action cannot be undone.";

pub fn init(window: &mut Window, cx: &mut App) -> Entity<Workspace> {
    cx.new(|cx| Workspace::new(window, cx))
}

struct DeviceNotifcation;
struct SignerNotifcation;
struct RelayNotifcation;

#[derive(Action, Clone, PartialEq, Eq, Deserialize)]
#[action(namespace = workspace, no_json)]
enum Command {
    ToggleTheme,
    ToggleAccount,

    RefreshRelayList,
    RefreshMessagingRelays,
    BackupEncryption,
    ImportEncryption,
    RefreshEncryption,
    ResetEncryption,

    ShowRelayList,
    ShowMessaging,
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

    /// Whether a user's relay list is connected
    relay_connected: bool,

    /// Whether the inbox is connected
    inbox_connected: bool,

    /// Event subscriptions
    _subscriptions: SmallVec<[Subscription; 6]>,
}

impl Workspace {
    fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let chat = ChatRegistry::global(cx);
        let device = DeviceRegistry::global(cx);
        let nostr = NostrRegistry::global(cx);
        let npubs = nostr.read(cx).npubs();

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
            // Observe the npubs entity
            cx.observe_in(&npubs, window, move |this, npubs, window, cx| {
                if !npubs.read(cx).is_empty() {
                    this.account_selector(window, cx);
                }
            }),
        );

        subscriptions.push(
            // Subscribe to the signer events
            cx.subscribe_in(&nostr, window, move |this, _state, event, window, cx| {
                match event {
                    StateEvent::Creating => {
                        let note = Notification::new()
                            .id::<SignerNotifcation>()
                            .title("Preparing a new identity")
                            .message(PREPARE_MSG)
                            .autohide(false)
                            .with_kind(NotificationKind::Info);

                        window.push_notification(note, cx);
                    }
                    StateEvent::Connecting => {
                        let note = Notification::new()
                            .id::<RelayNotifcation>()
                            .message("Connecting to the bootstrap relays...")
                            .with_kind(NotificationKind::Info);

                        window.push_notification(note, cx);
                    }
                    StateEvent::Connected => {
                        let note = Notification::new()
                            .id::<RelayNotifcation>()
                            .message("Connected to the bootstrap relays")
                            .with_kind(NotificationKind::Success);

                        window.push_notification(note, cx);
                    }
                    StateEvent::FetchingRelayList => {
                        let note = Notification::new()
                            .id::<RelayNotifcation>()
                            .message("Getting relay list...")
                            .with_kind(NotificationKind::Info);

                        window.push_notification(note, cx);
                    }
                    StateEvent::RelayNotConfigured => {
                        this.relay_warning(window, cx);
                    }
                    StateEvent::RelayConnected => {
                        window.clear_notification::<RelayNotifcation>(cx);
                        this.set_relay_connected(true, cx);
                    }
                    StateEvent::SignerSet => {
                        this.set_center_layout(window, cx);
                        this.set_relay_connected(false, cx);
                        this.set_inbox_connected(false, cx);
                        // Clear the signer notification
                        window.clear_notification::<SignerNotifcation>(cx);
                    }
                    _ => {}
                };
            }),
        );

        subscriptions.push(
            // Observe all events emitted by the device registry
            cx.subscribe_in(&device, window, |_this, _device, event, window, cx| {
                match event {
                    DeviceEvent::Requesting => {
                        const MSG: &str =
                            "Please open the other client and approve the encryption key request";

                        let note = Notification::new()
                            .id::<DeviceNotifcation>()
                            .title("Wait for approval")
                            .message(MSG)
                            .with_kind(NotificationKind::Info);

                        window.push_notification(note, cx);
                    }
                    DeviceEvent::Creating => {
                        let note = Notification::new()
                            .id::<DeviceNotifcation>()
                            .message("Creating encryption key")
                            .with_kind(NotificationKind::Info);

                        window.push_notification(note, cx);
                    }
                    DeviceEvent::Set => {
                        let note = Notification::new()
                            .id::<DeviceNotifcation>()
                            .message("Encryption Key has been set")
                            .with_kind(NotificationKind::Success);

                        window.push_notification(note, cx);
                    }
                    DeviceEvent::NotSet { reason } => {
                        let note = Notification::new()
                            .id::<DeviceNotifcation>()
                            .title("Cannot setup the encryption key")
                            .message(reason)
                            .autohide(false)
                            .with_kind(NotificationKind::Error);

                        window.push_notification(note, cx);
                    }
                    DeviceEvent::NotSubscribe { reason } => {
                        let note = Notification::new()
                            .id::<DeviceNotifcation>()
                            .title("Cannot getting messages")
                            .message(reason)
                            .autohide(false)
                            .with_kind(NotificationKind::Error);

                        window.push_notification(note, cx);
                    }
                    DeviceEvent::Error(error) => {
                        window.push_notification(Notification::error(error).autohide(false), cx);
                    }
                };
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
                    ChatEvent::Subscribed => {
                        this.set_inbox_connected(true, cx);
                    }
                    ChatEvent::Error(error) => {
                        window.push_notification(Notification::error(error).autohide(false), cx);
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
                    this.refresh_rooms(&ids, cx);
                });
            }),
        );

        // Set the layout at the end of cycle
        cx.defer_in(window, |this, window, cx| {
            this.set_layout(window, cx);
        });

        Self {
            titlebar,
            dock,
            relay_connected: false,
            inbox_connected: false,
            _subscriptions: subscriptions,
        }
    }

    /// Add panel to the dock
    pub fn add_panel<P>(panel: P, placement: DockPlacement, window: &mut Window, cx: &mut App)
    where
        P: PanelView,
    {
        if let Some(root) = window.root::<Root>().flatten()
            && let Ok(workspace) = root.read(cx).view().clone().downcast::<Self>()
        {
            workspace.update(cx, |this, cx| {
                this.dock.update(cx, |this, cx| {
                    this.add_panel(Arc::new(panel), placement, window, cx);
                });
            });
        }
    }

    /// Get all panel ids
    fn panel_ids(&self, cx: &App) -> Vec<u64> {
        self.dock
            .read(cx)
            .items
            .panel_ids(cx)
            .into_iter()
            .filter_map(|panel| panel.parse::<u64>().ok())
            .collect()
    }

    /// Set whether the relay list is connected
    fn set_relay_connected(&mut self, connected: bool, cx: &mut Context<Self>) {
        self.relay_connected = connected;
        cx.notify();
    }

    /// Set whether the inbox is connected
    fn set_inbox_connected(&mut self, connected: bool, cx: &mut Context<Self>) {
        self.inbox_connected = connected;
        cx.notify();
    }

    /// Set the dock layout
    fn set_layout(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let left = DockItem::panel(Arc::new(sidebar::init(window, cx)));

        // Update the dock layout with sidebar on the left
        self.dock.update(cx, |this, cx| {
            this.set_left_dock(left, Some(SIDEBAR_WIDTH), true, window, cx);
        });
    }

    /// Set the center dock layout
    fn set_center_layout(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let dock = self.dock.downgrade();
        let greeter = Arc::new(greeter::init(window, cx));
        let tabs = DockItem::tabs(vec![greeter], None, &dock, window, cx);
        let center = DockItem::split(Axis::Vertical, vec![tabs], &dock, window, cx);

        // Update the layout with center dock
        self.dock.update(cx, |this, cx| {
            this.set_center(center, window, cx);
        });
    }

    /// Handle command events
    fn on_command(&mut self, command: &Command, window: &mut Window, cx: &mut Context<Self>) {
        match command {
            Command::ShowSettings => {
                let view = settings::init(window, cx);

                window.open_modal(cx, move |this, _window, _cx| {
                    this.width(px(520.))
                        .show_close(true)
                        .pb_2()
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
            Command::RefreshMessagingRelays => {
                let chat = ChatRegistry::global(cx);
                chat.update(cx, |this, cx| {
                    this.get_messages(cx);
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
                let signer = nostr.read(cx).signer();

                if let Some(public_key) = signer.public_key() {
                    nostr.update(cx, |this, cx| {
                        this.ensure_relay_list(&public_key, cx);
                    });
                }
            }
            Command::RefreshEncryption => {
                let device = DeviceRegistry::global(cx);
                device.update(cx, |this, cx| {
                    this.get_announcement(cx);
                });
            }
            Command::ResetEncryption => {
                self.confirm_reset_encryption(window, cx);
            }
            Command::ToggleTheme => {
                self.theme_selector(window, cx);
            }
            Command::ToggleAccount => {
                self.account_selector(window, cx);
            }
            Command::BackupEncryption => {
                let device = DeviceRegistry::global(cx).downgrade();
                let save_dialog = cx.prompt_for_new_path(download_dir(), Some("encryption.txt"));

                cx.spawn_in(window, async move |_this, cx| {
                    // Get the output path from the save dialog
                    let output_path = match save_dialog.await {
                        Ok(Ok(Some(path))) => path,
                        Ok(Ok(None)) | Err(_) => return Ok(()),
                        Ok(Err(error)) => {
                            cx.update(|window, cx| {
                                let message = format!("Failed to pick save location: {error:#}");
                                let note = Notification::error(message).autohide(false);
                                window.push_notification(note, cx);
                            })?;
                            return Ok(());
                        }
                    };

                    // Get the backup task
                    let backup =
                        device.read_with(cx, |this, cx| this.backup(output_path.clone(), cx))?;

                    // Run the backup task
                    backup.await?;

                    // Open the backup file with the system's default application
                    cx.update(|_window, cx| {
                        cx.open_with_system(output_path.as_path());
                    })?;

                    Ok::<_, anyhow::Error>(())
                })
                .detach();
            }
            Command::ImportEncryption => {
                self.import_encryption(window, cx);
            }
        }
    }

    fn confirm_reset_encryption(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let device = DeviceRegistry::global(cx);
        let ent = device.downgrade();

        window.open_modal(cx, move |this, _window, cx| {
            let ent = ent.clone();

            this.confirm()
                .show_close(true)
                .title("Reset Encryption Key")
                .child(
                    v_flex()
                        .gap_1()
                        .text_sm()
                        .child(SharedString::from(ENC_MSG))
                        .child(
                            div()
                                .italic()
                                .text_color(cx.theme().text_danger)
                                .child(SharedString::from(ENC_WARN)),
                        ),
                )
                .on_ok(move |_ev, _window, cx| {
                    ent.update(cx, |this, cx| {
                        this.set_announcement(Keys::generate(), cx);
                    })
                    .ok();
                    // true to close modal
                    true
                })
        });
    }

    fn import_encryption(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let restore = cx.new(|cx| RestoreEncryption::new(window, cx));

        window.open_modal(cx, move |this, _window, _cx| {
            this.width(px(520.))
                .title("Restore Encryption")
                .child(restore.clone())
        });
    }

    fn account_selector(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let accounts = accounts::init(window, cx);

        window.open_modal(cx, move |this, _window, _cx| {
            this.width(px(520.))
                .title("Continue with")
                .show_close(false)
                .keyboard(false)
                .overlay_closable(false)
                .child(accounts.clone())
        });
    }

    fn theme_selector(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        window.open_modal(cx, move |this, _window, cx| {
            let registry = ThemeRegistry::global(cx);
            let themes = registry.read(cx).themes();

            this.width(px(520.))
                .show_close(true)
                .title("Select theme")
                .child(v_flex().gap_2().w_full().children({
                    let mut items = vec![];

                    for (ix, (path, theme)) in themes.iter().enumerate() {
                        items.push(
                            h_flex()
                                .id(ix)
                                .group("")
                                .px_2()
                                .h_8()
                                .w_full()
                                .justify_between()
                                .rounded(cx.theme().radius)
                                .bg(cx.theme().ghost_element_background)
                                .hover(|this| this.bg(cx.theme().ghost_element_hover))
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

    fn relay_warning(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        const BODY: &str = "Coop cannot found your gossip relay list. \
                            Maybe you haven't set it yet or relay not responsed";

        let nostr = NostrRegistry::global(cx);
        let signer = nostr.read(cx).signer();

        let Some(public_key) = signer.public_key() else {
            return;
        };

        let entity = nostr.downgrade();
        let loading = Rc::new(Cell::new(false));

        let note = Notification::new()
            .autohide(false)
            .id::<RelayNotifcation>()
            .icon(IconName::Relay)
            .title("Gossip Relays are required")
            .message(BODY)
            .action(move |_this, _window, _cx| {
                let entity = entity.clone();
                let public_key = public_key.to_owned();

                Button::new("retry")
                    .label("Retry")
                    .small()
                    .primary()
                    .loading(loading.get())
                    .disabled(loading.get())
                    .on_click({
                        let loading = Rc::clone(&loading);

                        move |_ev, _window, cx| {
                            // Set loading state to true
                            loading.set(true);
                            // Retry
                            entity
                                .update(cx, |this, cx| {
                                    this.ensure_relay_list(&public_key, cx);
                                })
                                .ok();
                        }
                    })
            });

        window.push_notification(note, cx);
    }

    fn titlebar_left(&mut self, cx: &mut Context<Self>) -> impl IntoElement {
        let nostr = NostrRegistry::global(cx);
        let signer = nostr.read(cx).signer();
        let current_user = signer.public_key();

        h_flex()
            .flex_shrink_0()
            .gap_2()
            .when_none(&current_user, |this| {
                this.child(
                    div()
                        .text_xs()
                        .text_color(cx.theme().text_muted)
                        .child(SharedString::from("Choose an account to continue...")),
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
                        .dropdown_menu(move |this, _window, _cx| {
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
                                .separator()
                                .menu_with_icon(
                                    "Accounts",
                                    IconName::Group,
                                    Box::new(Command::ToggleAccount),
                                )
                                .menu_with_icon(
                                    "Settings",
                                    IconName::Settings,
                                    Box::new(Command::ShowSettings),
                                )
                        }),
                )
            })
    }

    fn titlebar_right(&mut self, cx: &mut Context<Self>) -> impl IntoElement {
        let relay_connected = self.relay_connected;
        let inbox_connected = self.inbox_connected;

        let nostr = NostrRegistry::global(cx);
        let signer = nostr.read(cx).signer();

        let trashes = ChatRegistry::global(cx);
        let trash_messages = trashes.read(cx).count_trash_messages(cx);

        let Some(public_key) = signer.public_key() else {
            return div();
        };

        h_flex()
            .when(!cx.theme().platform.is_mac(), |this| this.pr_2())
            .gap_2()
            .when(trash_messages > 0, |this| {
                this.child(
                    h_flex()
                        .id("trash-messages")
                        .h_6()
                        .px_1()
                        .gap_1()
                        .rounded(cx.theme().radius)
                        .hover(|this| this.bg(cx.theme().ghost_element_hover))
                        .child(
                            Icon::new(IconName::Warning)
                                .small()
                                .text_color(cx.theme().text_danger),
                        )
                        .child(
                            div()
                                .text_xs()
                                .line_height(relative(1.))
                                .child(format!("{trash_messages}")),
                        )
                        .on_click(move |_ev, window, cx| {
                            cx.stop_propagation();
                            // Add the trash panel to the center workspace
                            Self::add_panel(
                                trash::init(window, cx),
                                DockPlacement::Center,
                                window,
                                cx,
                            );
                        }),
                )
            })
            .child(
                Button::new("key")
                    .icon(IconName::UserKey)
                    .tooltip("Decoupled encryption key")
                    .small()
                    .ghost()
                    .dropdown_menu(move |this, _window, cx| {
                        let device = DeviceRegistry::global(cx);
                        let subscribing = device.read(cx).subscribing;
                        let requesting = device.read(cx).requesting;

                        this.min_w(px(260.))
                            .label("Encryption Key")
                            .when(requesting, |this| {
                                this.item(PopupMenuItem::element(move |_window, cx| {
                                    h_flex()
                                        .px_1()
                                        .w_full()
                                        .gap_2()
                                        .text_sm()
                                        .child(
                                            div()
                                                .size_1p5()
                                                .rounded_full()
                                                .bg(cx.theme().icon_accent),
                                        )
                                        .child(SharedString::from("Waiting for approval..."))
                                }))
                            })
                            .item(PopupMenuItem::element(move |_window, cx| {
                                h_flex()
                                    .px_1()
                                    .w_full()
                                    .gap_2()
                                    .text_sm()
                                    .when(!subscribing, |this| {
                                        this.text_color(cx.theme().text_muted)
                                    })
                                    .child(div().size_1p5().rounded_full().map(|this| {
                                        if subscribing {
                                            this.bg(cx.theme().icon_accent)
                                        } else {
                                            this.bg(cx.theme().icon_muted)
                                        }
                                    }))
                                    .map(|this| {
                                        if subscribing {
                                            this.child("Listening for messages")
                                        } else {
                                            this.child("Idle")
                                        }
                                    })
                            }))
                            .separator()
                            .menu_with_icon(
                                "Backup",
                                IconName::Shield,
                                Box::new(Command::BackupEncryption),
                            )
                            .menu_with_icon(
                                "Restore from secret key",
                                IconName::Usb,
                                Box::new(Command::ImportEncryption),
                            )
                            .separator()
                            .menu_with_icon(
                                "Reload",
                                IconName::Refresh,
                                Box::new(Command::RefreshEncryption),
                            )
                            .menu_with_icon(
                                "Reset",
                                IconName::Warning,
                                Box::new(Command::ResetEncryption),
                            )
                    }),
            )
            .child(
                Button::new("inbox")
                    .icon(IconName::Inbox)
                    .small()
                    .ghost()
                    .loading(!inbox_connected)
                    .disabled(!inbox_connected)
                    .when(!inbox_connected, |this| {
                        this.tooltip("Connecting to the user's messaging relays...")
                    })
                    .when(inbox_connected, |this| this.indicator())
                    .dropdown_menu(move |this, _window, cx| {
                        let chat = ChatRegistry::global(cx);
                        let persons = PersonRegistry::global(cx);
                        let profile = persons.read(cx).get(&public_key, cx);

                        let urls: Vec<(SharedString, SharedString)> = profile
                            .messaging_relays()
                            .iter()
                            .map(|url| {
                                (
                                    SharedString::from(url.to_string()),
                                    chat.read(cx).count_messages(url).to_string().into(),
                                )
                            })
                            .collect();

                        // Header
                        let menu = this.min_w(px(260.)).label("Messaging Relays");

                        // Content
                        let menu = urls.into_iter().fold(menu, |this, (url, count)| {
                            this.item(PopupMenuItem::element(move |_window, cx| {
                                h_flex()
                                    .px_1()
                                    .w_full()
                                    .text_sm()
                                    .justify_between()
                                    .child(
                                        h_flex()
                                            .gap_2()
                                            .child(
                                                div()
                                                    .size_1p5()
                                                    .rounded_full()
                                                    .bg(cx.theme().icon_accent),
                                            )
                                            .child(url.clone()),
                                    )
                                    .child(
                                        div()
                                            .text_xs()
                                            .text_color(cx.theme().text_muted)
                                            .child(count.clone()),
                                    )
                            }))
                        });

                        // Footer
                        menu.separator()
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
            )
            .child(
                Button::new("relay-list")
                    .icon(IconName::Relay)
                    .small()
                    .ghost()
                    .loading(!relay_connected)
                    .disabled(!relay_connected)
                    .when(!relay_connected, |this| {
                        this.tooltip("Connecting to the user's relay list...")
                    })
                    .when(relay_connected, |this| this.indicator())
                    .dropdown_menu(move |this, _window, _cx| {
                        this.label("User's Relay List")
                            .separator()
                            .menu_with_icon(
                                "Reload",
                                IconName::Refresh,
                                Box::new(Command::RefreshRelayList),
                            )
                            .menu_with_icon(
                                "Update",
                                IconName::Settings,
                                Box::new(Command::ShowRelayList),
                            )
                    }),
            )
    }
}

impl Render for Workspace {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let modal_layer = Root::render_modal_layer(window, cx);
        let notification_layer = Root::render_notification_layer(window, cx);

        // Titlebar elements
        let left = self.titlebar_left(cx).into_any_element();
        let right = self.titlebar_right(cx).into_any_element();

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
