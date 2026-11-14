use std::cell::Cell;
use std::collections::HashMap;
use std::rc::Rc;
use std::sync::Arc;

use ::settings::{AppSettings, AppearanceMode, persisted_onboarded};
use anyhow::Error;
#[cfg(not(target_arch = "wasm32"))]
use auto_update::AutoUpdater;
use chat::{ChatEvent, ChatRegistry};
#[cfg(not(target_arch = "wasm32"))]
use common::download_dir;
use gpui::{
    Action, App, AppContext, Context, Entity, Focusable, InteractiveElement, IntoElement,
    ParentElement, Pixels, Render, SharedString, Styled, Subscription, Task, Window, div, px,
};
use gpui_kit::component::button::Button;
use gpui_kit::component::command::{
    Command as CommandPalette, CommandGroup, CommandItem, CommandState,
};
use gpui_kit::component::dock::{DockArea, DockLayout, DockPlacement, Panel, PanelHandle, PanelId};
use gpui_kit::component::notification::{Notification, NotificationType};
use gpui_kit::component::{
    ActiveTheme, IndexPath, Theme, ThemeConfig, ThemeRegistry, WindowExt, v_flex,
};
use nip4e::{DeviceEvent, DeviceRegistry};
use nip29::{GroupId, GroupKey};
use nostr_sdk::prelude::*;
use serde::Deserialize;
use smallvec::{SmallVec, smallvec};
use state::{NostrRegistry, StateEvent};

use crate::dialogs::restore::RestoreEncryption;
use crate::dialogs::{
    import, join_group, new_chat, new_group, onboarding, profile, relays, settings,
};
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

struct DeviceNotification;
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
    ToggleTheme,
    ShowBackup,
    ShowContactList,
    ShowInbox,
    ShowBrowse,
    ShowWallet,
    NewChat,
    NewGroup,
    JoinGroup,
}

pub struct Workspace {
    dock: Entity<DockArea>,
    sidebar: Entity<Sidebar>,
    /// Group panels docked by group key, so reopening a group focuses its panel.
    /// Group panels hide their group, so the dock cannot tell them apart.
    group_panels: HashMap<GroupKey, PanelId>,
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

        subscriptions.push(cx.observe_window_appearance(window, |_this, window, cx| {
            if AppSettings::get_appearance_mode(cx) == AppearanceMode::System {
                Theme::sync_system_appearance(Some(window), cx);
                AppSettings::apply_theme_style(cx);
            }
        }));

        let login_nostr = nostr.clone();
        subscriptions.push(
            // Subscribe to the nostr events
            cx.subscribe_in(&nostr, window, move |_, _, event, window, cx| match event {
                StateEvent::SignerChanged => {
                    window.close_all_dialogs(cx);

                    // Show the first-run onboarding once the user is signed in
                    if !persisted_onboarded() && login_nostr.read(cx).current_user().is_some() {
                        onboarding::open(window, cx);
                    }
                }
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
                            .id::<DeviceNotification>()
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
                            .id::<DeviceNotification>()
                            .message(MSG)
                            .with_type(NotificationType::Info)
                            .action(|_this, _window, _cx| {
                                Button::new("retry").label("Retry").on_click(
                                    move |_this, window, cx| {
                                        let device = DeviceRegistry::global(cx);
                                        device.update(cx, |this, cx| {
                                            this.get_announcement(cx);
                                        });
                                        window.remove_notification::<DeviceNotification>(cx);
                                    },
                                )
                            });

                        window.push_notification(note, cx);
                    }
                    DeviceEvent::Set => {
                        let note = Notification::new()
                            .id::<DeviceNotification>()
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

        #[cfg(not(target_arch = "wasm32"))]
        {
            let wallet = ::nip60::WalletRegistry::global(cx);

            subscriptions.push(cx.subscribe_in(
                &wallet,
                window,
                |_this, _wallet, event, window, cx| match event {
                    ::nip60::WalletEvent::Error(error) => {
                        window.push_notification(Notification::error(error.clone()), cx);
                    }
                    ::nip60::WalletEvent::Updated => {}
                },
            ));
        }

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

            if !persisted_onboarded() && nostr.read(cx).current_user().is_some() {
                onboarding::open(window, cx);
            }
        });

        Self {
            dock,
            sidebar,
            group_panels: HashMap::new(),
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

    fn add_group_panel_to_dock(
        &mut self,
        group_key: GroupKey,
        panel: PanelHandle,
        placement: DockPlacement,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let panel_id = panel.panel_id(cx);
        let docked = self.group_panels.get(&group_key).copied();

        let mut added = false;
        let mut focused = false;

        self.dock.update(cx, |area, cx| {
            if let Some(id) = docked
                && area.panel(id).is_some()
            {
                crate::dock::focus_panel(area, id, window, cx);
                focused = true;
                return;
            }
            crate::dock::add_panel(area, panel, placement, window, cx);
            added = area.panel(panel_id).is_some();
        });

        if focused {
            return;
        }

        if added {
            self.group_panels.insert(group_key, panel_id);
        } else {
            self.group_panels.remove(&group_key);
        }
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
            Command::ToggleTheme => {
                self.theme_selector(window, cx);
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
            Command::ShowWallet =>
            {
                #[cfg(not(target_arch = "wasm32"))]
                if AppSettings::get_wallet(cx) {
                    let panel = nip60_ui::wallet::init(window, cx);
                    self.add_panel_to_dock(panel, DockPlacement::Center, window, cx);
                }
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

                #[cfg(not(target_arch = "wasm32"))]
                {
                    let save_dialog =
                        cx.prompt_for_new_path(download_dir(), Some("encryption.txt"));

                    self.tasks.push(cx.spawn_in(window, async move |_this, cx| {
                        // Get the output path from the save dialog
                        let output_path = match save_dialog.await {
                            Ok(Ok(Some(path))) => path,
                            Ok(Ok(None)) | Err(_) => return Ok(()),
                            Ok(Err(error)) => {
                                cx.update(|window, cx| {
                                    let message =
                                        format!("Failed to pick save location: {error:#}");
                                    let note = Notification::error(message).autohide(false);
                                    window.push_notification(note, cx);
                                })?;
                                return Ok(());
                            }
                        };

                        // Get the backup task
                        let backup = device
                            .read_with(cx, |this, cx| this.backup(output_path.clone(), cx))?;

                        // Run the backup task
                        backup.await?;

                        // Open the backup file with the system's default application
                        cx.update(|_window, cx| {
                            cx.open_with_system(output_path.as_path());
                        })?;

                        Ok(())
                    }));
                }

                #[cfg(target_arch = "wasm32")]
                self.tasks.push(cx.spawn_in(window, async move |_this, cx| {
                    let backup = device.read_with(cx, |this, cx| this.backup_bytes(cx))?;
                    let content = backup.await?;

                    cx.update(|_window, _cx| {
                        state::web_file::download(
                            "encryption.txt",
                            "text/plain",
                            content.as_bytes(),
                        )
                    })
                    .map_err(|e| anyhow::anyhow!(e))??;

                    Ok(())
                }));
            }
            Command::ImportEncryption => {
                self.import_encryption(window, cx);
            }
            Command::Update =>
            {
                #[cfg(not(target_arch = "wasm32"))]
                if let Some(auto_updater) = AutoUpdater::try_global(cx) {
                    auto_updater.update(cx, |this, cx| this.check(cx));
                }
            }
        }
    }

    /// Browse and apply the bundled themes.
    fn theme_selector(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let state = cx.new(|cx| CommandState::new(window, cx));
        let focus_on_mount = Rc::new(Cell::new(true));

        window.open_dialog(cx, move |dialog, _, _| {
            let state = state.clone();
            let focus_on_mount = focus_on_mount.clone();

            dialog
                .close_button(false)
                .p_0()
                .width(px(420.))
                .content(move |content, window, cx| {
                    if focus_on_mount.replace(false) {
                        let state = state.clone();
                        window.defer(cx, move |window, cx| {
                            state.read(cx).focus_handle(cx).focus(window, cx);
                        });
                    }

                    let active = Theme::global(cx).theme_name().clone();
                    let (lights, darks) = Self::theme_entries(cx);

                    content.child(
                        CommandPalette::new(&state)
                            .bordered(false)
                            .rounded(cx.theme().radius_lg)
                            .placeholder("Search themes...")
                            .max_h(px(420.))
                            .on_confirm(move |index, _, cx| {
                                if let Some(name) = Self::theme_name_at(index, cx) {
                                    AppSettings::select_theme(&name, cx);
                                }
                            })
                            .group(
                                CommandGroup::new()
                                    .label("Light Themes")
                                    .items(Self::theme_items(&lights, &active)),
                            )
                            .group(
                                CommandGroup::new()
                                    .label("Dark Themes")
                                    .items(Self::theme_items(&darks, &active)),
                            ),
                    )
                })
        });
    }

    /// Sorted bundled themes partitioned into light and dark groups
    fn theme_entries(cx: &App) -> (Vec<Rc<ThemeConfig>>, Vec<Rc<ThemeConfig>>) {
        let (mut lights, mut darks) = (vec![], vec![]);

        for theme in ThemeRegistry::global(cx).sorted_themes() {
            if theme.mode.is_dark() {
                darks.push(theme.clone());
            } else {
                lights.push(theme.clone());
            }
        }

        (lights, darks)
    }

    /// One palette row per theme.
    fn theme_items(themes: &[Rc<ThemeConfig>], active: &SharedString) -> Vec<CommandItem> {
        themes
            .iter()
            .map(|theme| {
                CommandItem::new()
                    .label(theme.name.clone())
                    .checked(&theme.name == active)
                    .keywords([if theme.mode.is_dark() {
                        "dark"
                    } else {
                        "light"
                    }])
            })
            .collect()
    }

    /// The theme registered at the palette's original group position.
    fn theme_name_at(index: IndexPath, cx: &App) -> Option<SharedString> {
        let (lights, darks) = Self::theme_entries(cx);

        match index.section {
            0 => lights.get(index.row),
            1 => darks.get(index.row),
            _ => None,
        }
        .map(|theme| theme.name.clone())
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
