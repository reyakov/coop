use std::sync::Arc;

use ::settings::{AppSettings, AppearanceMode};
use anyhow::Error;
use auto_update::AutoUpdater;
use chat::{ChatEvent, ChatRegistry};
use common::download_dir;
use gpui::{
    Action, App, AppContext, Context, Entity, InteractiveElement, IntoElement, ParentElement,
    Pixels, Render, SharedString, Styled, Subscription, Task, Window, div, px,
};
use gpui_kit::component::button::Button;
use gpui_kit::component::dock::{DockArea, DockLayout, DockPlacement, Panel, PanelHandle};
use gpui_kit::component::notification::{Notification, NotificationType};
use gpui_kit::component::{ActiveTheme, Theme, WindowExt, v_flex};
use nip4e::{DeviceEvent, DeviceRegistry};
use nip29::{GroupId, GroupKey};
use nostr_sdk::prelude::*;
use serde::Deserialize;
use smallvec::{SmallVec, smallvec};
use state::{NostrRegistry, StateEvent};

use crate::dialogs::restore::RestoreEncryption;
use crate::dialogs::{import, join_group, new_chat, new_group, profile, relays, settings};
use crate::panels::{backup, browse, contact_list, greeter, inbox};
use crate::sidebar::Sidebar;

mod dialogs;
mod dock;
mod nav;
mod panels;
mod sidebar;

pub use dock::TAB_BAR_HEIGHT;

/// Default sidebar width.
const SIDEBAR_WIDTH: Pixels = px(240.);

pub fn init(window: &mut Window, cx: &mut App) -> Entity<Workspace> {
    cx.new(|cx| Workspace::new(window, cx))
}

struct DeviceNotifcation;
struct MsgRelayNotification;

#[derive(Action, Clone, PartialEq, Eq, Deserialize)]
#[action(namespace = workspace, no_json)]
enum Command {
    Update,
    RefreshMessagingRelays,
    BackupEncryption,
    ImportEncryption,
    RefreshEncryption,
    ResetEncryption,
    ShowRelayList,
    ShowProfile,
    ShowSettings,
    ShowBackup,
    ShowContactList,
    ShowInbox,
    ShowBrowse,
    NewChat,
    NewGroup,
    JoinGroup,
}

pub struct Workspace {
    dock: Entity<DockArea>,
    sidebar: Entity<Sidebar>,
    /// Async tasks
    tasks: Vec<Task<Result<(), Error>>>,
    /// Event subscriptions
    _subscriptions: SmallVec<[Subscription; 7]>,
}

impl Workspace {
    fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let chat = ChatRegistry::global(cx);
        let device = DeviceRegistry::global(cx);
        let nostr = NostrRegistry::global(cx);

        let dock = cx.new(|cx| {
            let skin = dock::CoopDockSkin::new(cx);
            gpui_kit::component::dock::DockArea::new("coop", None, window, cx).with_renderer(skin)
        });
        let sidebar = cx.new(|cx| Sidebar::new(window, dock.downgrade(), cx));
        let left_sidebar = sidebar.clone();

        let workspace = cx.entity().downgrade();
        let mut subscriptions = smallvec![];

        if AppSettings::get_appearance_mode(cx) == AppearanceMode::System {
            subscriptions.push(cx.observe_window_appearance(window, |_this, window, cx| {
                Theme::sync_system_appearance(Some(window), cx);
            }));
        }

        subscriptions.push(
            // Subscribe to the nostr events
            cx.subscribe_in(&nostr, window, move |_, _, event, window, cx| match event {
                StateEvent::SignerChanged => window.close_all_dialogs(cx),
                StateEvent::PassphraseRequired => import::open_unlock(window, cx),
                _ => {}
            }),
        );

        subscriptions.push(
            // Observe all events emitted by the device registry
            cx.subscribe_in(&device, window, |_this, _device, event, window, cx| {
                match event {
                    DeviceEvent::Requesting(code) => {
                        const MSG: &str = "Please open another client and approve the request \
                                           for the encryption key. Compare the code with the \
                                           one shown there before approving.";

                        let note = Notification::new()
                            .id::<DeviceNotifcation>()
                            .autohide(false)
                            .title(format!("Wait for approval · {code}"))
                            .message(MSG)
                            .with_type(NotificationType::Info);

                        window.push_notification(note, cx);
                    }
                    DeviceEvent::NotSet => {
                        const MSG: &str = "No encryption key announcement was found. It may \
                                           still be propagating, or you can restore one \
                                           from the settings.";

                        let note = Notification::new()
                            .id::<DeviceNotifcation>()
                            .message(MSG)
                            .with_type(NotificationType::Info)
                            .action(|_this, _window, _cx| {
                                Button::new("retry").label("Retry").on_click(
                                    move |_this, window, cx| {
                                        let device = DeviceRegistry::global(cx);
                                        device.update(cx, |this, cx| {
                                            this.get_announcement(cx);
                                        });
                                        window.remove_notification::<DeviceNotifcation>(cx);
                                    },
                                )
                            });

                        window.push_notification(note, cx);
                    }
                    DeviceEvent::Set => {
                        let note = Notification::new()
                            .id::<DeviceNotifcation>()
                            .message("Encryption Key has been set")
                            .with_type(NotificationType::Success);

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
            cx.subscribe_in(&chat, window, move |_this, _chat, ev, window, cx| {
                match ev {
                    ChatEvent::InboxRelayNotFound => {
                        const MSG: &str = "Messaging Relays not found. Cannot receive messages.";

                        window.push_notification(
                            Notification::warning(MSG)
                                .id::<MsgRelayNotification>()
                                .autohide(false)
                                .action(|_this, _window, _cx| {
                                    Button::new("retry").label("Retry").on_click(
                                        move |_this, window, cx| {
                                            let chat = ChatRegistry::global(cx);
                                            chat.update(cx, |this, cx| {
                                                this.get_metadata(cx);
                                            });
                                            window.remove_notification::<MsgRelayNotification>(cx);
                                        },
                                    )
                                }),
                            cx,
                        );
                    }
                    ChatEvent::Error(error) => {
                        window.push_notification(Notification::error(error).autohide(false), cx);
                    }
                    _ => {}
                };
            }),
        );

        App::on_action(cx, {
            let workspace = workspace.clone();
            move |action, cx| {
                let Some(workspace) = workspace.upgrade() else {
                    return;
                };
                workspace.update(cx, |this, cx| {
                    this.show_group_meta(action, cx);
                });
            }
        });

        App::on_action(cx, move |action, cx| {
            let Some(workspace) = workspace.upgrade() else {
                return;
            };
            workspace.update(cx, |this, cx| {
                this.hide_group_meta(action, cx);
            });
        });

        let greeter_dock = dock.downgrade();

        cx.defer_in(window, move |this, window, cx| {
            let sidebar = PanelHandle::new(left_sidebar);
            let greeter = PanelHandle::new(greeter::init(greeter_dock, window, cx));

            this.dock.update(cx, |this, cx| {
                let left = DockLayout::tabs().panel_view(Arc::new(sidebar), cx);
                let center = DockLayout::v_split()
                    .child(DockLayout::tabs().panel_view(Arc::new(greeter), cx), None);

                this.set_dock(DockPlacement::Left, left, window, cx);
                this.set_dock_size(DockPlacement::Left, SIDEBAR_WIDTH, window, cx);
                this.set_center(center, window, cx);
            });
        });

        Self {
            dock,
            sidebar,
            tasks: vec![],
            _subscriptions: subscriptions,
        }
    }

    /// Add a panel to the workspace dock from anywhere that has the window.
    /// The workspace is the window's root view, so it is reachable directly.
    pub fn add_panel<P: Panel>(
        panel: Entity<P>,
        placement: DockPlacement,
        window: &mut Window,
        cx: &mut App,
    ) {
        let Some(root) = window.root::<Self>().flatten() else {
            return;
        };
        root.update(cx, |this, cx| {
            this.add_panel_to_dock(panel, placement, window, cx)
        });
    }

    /// Add a panel to the dock, or focus it if it is already docked.
    fn add_panel_to_dock<P: Panel>(
        &mut self,
        panel: Entity<P>,
        placement: DockPlacement,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.dock.update(cx, |area, cx| {
            crate::dock::add_panel(area, PanelHandle::new(panel), placement, window, cx)
        });
    }

    /// Handle command events
    fn on_command(&mut self, command: &Command, window: &mut Window, cx: &mut Context<Self>) {
        match command {
            Command::ShowSettings => {
                let view = settings::init(window, cx);

                window.open_dialog(cx, move |this, _window, _cx| {
                    this.width(px(650.))
                        .close_button(true)
                        .pb_2()
                        .title("Preferences")
                        .child(view.clone())
                });
            }
            Command::ShowProfile => {
                let nostr = NostrRegistry::global(cx);

                if let Some(public_key) = nostr.read(cx).current_user() {
                    profile::open(public_key, window, cx);
                }
            }
            Command::ShowContactList => {
                self.add_panel_to_dock(
                    contact_list::init(window, cx),
                    DockPlacement::Left,
                    window,
                    cx,
                );
            }
            Command::ShowInbox => {
                self.add_panel_to_dock(inbox::init(window, cx), DockPlacement::Center, window, cx);
            }
            Command::ShowBrowse => {
                let panel = browse::init(self.dock.downgrade(), window, cx);
                self.add_panel_to_dock(panel, DockPlacement::Center, window, cx);
            }
            Command::NewChat => {
                new_chat::open(self.dock.downgrade(), window, cx);
            }
            Command::NewGroup => {
                new_group::open(self.dock.downgrade(), window, cx);
            }
            Command::JoinGroup => {
                join_group::open(window, cx);
            }
            Command::ShowBackup => {
                self.add_panel_to_dock(backup::init(window, cx), DockPlacement::Left, window, cx);
            }
            Command::RefreshMessagingRelays => {
                let chat = ChatRegistry::global(cx);
                // Trigger a refresh of the chat registry
                chat.update(cx, |this, cx| {
                    this.reload(cx);
                });
            }
            Command::ShowRelayList => {
                relays::open(window, cx);
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
            Command::BackupEncryption => {
                let device = DeviceRegistry::global(cx).downgrade();
                let save_dialog = cx.prompt_for_new_path(download_dir(), Some("encryption.txt"));

                self.tasks.push(cx.spawn_in(window, async move |_this, cx| {
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

                    Ok(())
                }));
            }
            Command::ImportEncryption => {
                self.import_encryption(window, cx);
            }
            Command::Update => {
                if let Some(auto_updater) = AutoUpdater::try_global(cx) {
                    auto_updater.update(cx, |this, cx| this.check(cx));
                }
            }
        }
    }

    /// Open the channel creation dialog under the action's group.
    fn on_create_channel(
        &mut self,
        action: &nip29_ui::CreateChannel,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Ok(id) = GroupId::new(action.parent.clone()) else {
            return;
        };

        new_group::open_channel(
            self.dock.downgrade(),
            GroupKey::new(action.relay.clone(), id),
            window,
            cx,
        );
    }

    /// Show the sidebar's information for the group a panel belongs to.
    fn show_group_meta(&mut self, action: &nip29_ui::ShowGroupMeta, cx: &mut Context<Self>) {
        let Ok(id) = GroupId::new(action.group.clone()) else {
            return;
        };

        let key = GroupKey::new(action.relay.clone(), id);

        self.sidebar.update(cx, |sidebar, cx| {
            sidebar.show_group_meta(&key, cx);
        });
    }

    /// Hide the sidebar's information for the group a panel belongs to.
    fn hide_group_meta(&mut self, action: &nip29_ui::HideGroupMeta, cx: &mut Context<Self>) {
        let Ok(id) = GroupId::new(action.group.clone()) else {
            return;
        };

        let key = GroupKey::new(action.relay.clone(), id);

        self.sidebar.update(cx, |sidebar, cx| {
            sidebar.hide_group_meta(&key, cx);
        });
    }

    fn confirm_reset_encryption(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        const ENC_MSG: &str = "Encryption Key is a special key that used to encrypt and decrypt your messages. \
                               Your identity is completely decoupled from all encryption processes to protect your privacy.";

        const ENC_WARN: &str = "By resetting your encryption key, you will lose access to \
                                all your encrypted messages before. This action cannot be undone.";

        let device = DeviceRegistry::global(cx);
        let ent = device.downgrade();

        window.open_alert_dialog(cx, move |alert, _window, cx| {
            let ent = ent.clone();

            alert
                .confirm()
                .title("Reset Encryption Key")
                .description(
                    v_flex()
                        .gap_1()
                        .text_xs()
                        .child(SharedString::from(ENC_MSG))
                        .child(
                            div()
                                .italic()
                                .text_color(cx.theme().danger)
                                .child(SharedString::from(ENC_WARN)),
                        ),
                )
                .on_ok(move |_ev, _window, cx| {
                    ent.update(cx, |this, cx| {
                        this.set_announcement(Keys::generate(), cx);
                    })
                    .ok();
                    // true to close dialog
                    true
                })
        });
    }

    fn import_encryption(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let restore = cx.new(|cx| RestoreEncryption::new(window, cx));
        window.open_dialog(cx, move |this, _window, _cx| {
            this.width(px(420.))
                .title("Restore Encryption")
                .child(restore.clone())
        });
    }
}

impl Render for Workspace {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // Dialog, notification and tooltip layers are owned by the framework
        // Root that gpui_kit::open_window installed for this window.
        div()
            .id("workspace")
            .on_action(cx.listener(Self::on_command))
            .on_action(cx.listener(Self::on_create_channel))
            .relative()
            .size_full()
            .child(self.dock.clone())
    }
}
