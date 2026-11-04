#[cfg(not(target_arch = "wasm32"))]
use auto_update::AutoUpdater;
use gpui::prelude::FluentBuilder;
use gpui::{
    Anchor, App, Context, Div, InteractiveElement, IntoElement, ParentElement, SharedString,
    Stateful, Styled, Window, div, px,
};
#[cfg(not(target_arch = "wasm32"))]
use gpui::{AnyElement, Entity};
use gpui_kit::base::StyledExt as _;
#[cfg(not(target_arch = "wasm32"))]
use gpui_kit::component::Disableable;
use gpui_kit::component::button::{Button, ButtonVariant, ButtonVariants};
use gpui_kit::component::clipboard::Clipboard;
use gpui_kit::component::dialog::DialogButtonProps;
use gpui_kit::component::menu::{DropdownMenu, PopupMenuItem};
use gpui_kit::component::popover::Popover;
use gpui_kit::component::separator::Separator;
use gpui_kit::component::{ActiveTheme, Icon, Selectable, Sizable, WindowExt, h_flex, v_flex};
use nostr_sdk::prelude::*;
use person::shorten_pubkey;
use settings::AppSettings;
use state::{CLIENT_NAME, NostrRegistry};
use ui::{Avatar, IconName};

use super::Sidebar;
use crate::Command;
use crate::dock::TAB_BAR_HEIGHT;
use crate::nav::Nav;

impl Sidebar {
    pub(super) fn render_user(
        &self,
        current_user: &PublicKey,
        cx: &mut Context<Self>,
    ) -> Stateful<Div> {
        #[cfg(not(target_arch = "wasm32"))]
        let updater = AutoUpdater::try_global(cx);
        let profile = self.directory.read(cx).person(current_user, cx);
        let avatar = profile.avatar();
        let encryption_key = profile
            .announcement()
            .map(|announcement| announcement.public_key());
        let encryption_enabled = AppSettings::get_nip4e(cx);

        h_flex()
            .id("sidebar-user")
            .w_full()
            .h(TAB_BAR_HEIGHT)
            .flex_shrink_0()
            .justify_between()
            .px_2p5()
            .when(cfg!(target_os = "macos"), |this| this.pl(px(80.)))
            .child(
                Button::new("user")
                    .text()
                    .dropdown_caret(true)
                    .min_w_0()
                    .flex_shrink_1()
                    .child(Avatar::new(avatar).seed(profile.avatar_seed()).small())
                    .dropdown_menu(move |this, _window, _cx| {
                        this.menu("Profile", Box::new(Command::ShowProfile))
                            .menu("Relays", Box::new(Command::ShowRelayList))
                            .separator()
                            .label(format!("{CLIENT_NAME} {}", env!("CARGO_PKG_VERSION")))
                            .item(
                                PopupMenuItem::element(|_, cx| {
                                    div().text_color(cx.theme().danger).child("Log out")
                                })
                                .on_click(|_, window, cx| {
                                    open_logout_dialog(window, cx);
                                }),
                            )
                    }),
            )
            .child({
                let actions = h_flex().gap_1();

                #[cfg(not(target_arch = "wasm32"))]
                let actions = actions.when_some(updater, |this, updater| {
                    this.child(self.render_updater(updater, cx))
                });

                actions
                    .when(encryption_enabled, |actions| {
                        actions.when_some(encryption_key, |actions, encryption_key| {
                            actions.child(self.render_encryption(encryption_key))
                        })
                    })
                    .child(
                        Button::new("sidebar-search")
                            .icon(IconName::Search)
                            .tooltip("Search")
                            .selected(self.search_open)
                            .ghost()
                            .small()
                            .on_click(cx.listener(|this, _event, window, cx| {
                                this.toggle_search(window, cx);
                            })),
                    )
                    .child(
                        Button::new("sidebar-new")
                            .icon(IconName::PlusChat)
                            .tooltip("New")
                            .ghost()
                            .small()
                            .dropdown_menu(|menu, _window, _cx| {
                                menu.label("Public Group")
                                    .menu("New Group", Box::new(Command::NewGroup))
                                    .menu("Join Group", Box::new(Command::JoinGroup))
                                    .separator()
                                    .menu("New Chat", Box::new(Command::NewChat))
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
            })
    }

    /// The encryption key popover: public key with copy, backup and reset
    fn render_encryption(&self, encryption_key: PublicKey) -> impl IntoElement {
        Popover::new("sidebar-encryption")
            .anchor(Anchor::TopRight)
            .trigger(
                Button::new("encryption-key")
                    .icon(IconName::Lock)
                    .tooltip("Encryption Key")
                    .ghost()
                    .small(),
            )
            .content(move |_, _, cx| {
                let popover = cx.entity();
                let Ok(npub) = encryption_key.to_bech32();

                v_flex()
                    .w_64()
                    .gap_2()
                    .text_sm()
                    .child(
                        div()
                            .text_xs()
                            .font_semibold()
                            .text_color(cx.theme().muted_foreground)
                            .child("Public key"),
                    )
                    .child(
                        h_flex()
                            .items_center()
                            .gap_1()
                            .child(
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .truncate()
                                    .text_color(cx.theme().muted_foreground)
                                    .child(SharedString::from(shorten_pubkey(encryption_key, 12))),
                            )
                            .child(
                                Clipboard::new("encryption-key-copy")
                                    .tooltip("Copy public key")
                                    .value(npub),
                            ),
                    )
                    .child(Separator::horizontal())
                    .child(
                        h_flex()
                            .justify_end()
                            .gap_2()
                            .child(
                                Button::new("encryption-backup")
                                    .label("Backup")
                                    .outline()
                                    .small()
                                    .on_click({
                                        let popover = popover.clone();
                                        move |_, window, cx| {
                                            popover
                                                .update(cx, |state, cx| state.dismiss(window, cx));
                                            window.dispatch_action(
                                                Box::new(Command::BackupEncryption),
                                                cx,
                                            );
                                        }
                                    }),
                            )
                            .child(
                                Button::new("encryption-reset")
                                    .label("Reset")
                                    .ghost()
                                    .small()
                                    .on_click(move |_, window, cx| {
                                        popover.update(cx, |state, cx| state.dismiss(window, cx));
                                        window.dispatch_action(
                                            Box::new(Command::ResetEncryption),
                                            cx,
                                        );
                                    }),
                            ),
                    )
            })
    }

    pub(super) fn render_footer(&self, loading: bool, _cx: &mut Context<Self>) -> Div {
        v_flex()
            .p_2()
            .flex_shrink_0()
            .gap_1()
            .items_start()
            .justify_start()
            .when(!loading, |this| {
                this.child(
                    Nav::new("settings")
                        .prefix(Icon::new(IconName::Settings).small())
                        .center_prefix(true)
                        .label("Settings")
                        .text_sm()
                        .on_click(|_, window, cx| {
                            window.dispatch_action(Box::new(Command::ShowSettings), cx)
                        }),
                )
            })
    }

    #[cfg(not(target_arch = "wasm32"))]
    fn render_updater(&self, updater: Entity<AutoUpdater>, cx: &mut App) -> AnyElement {
        let status = updater.read(cx).status();
        let up_to_date = updater.read(cx).up_to_date();
        let staged = updater.read(cx).staged();

        h_flex()
            .gap_2()
            .when(!up_to_date, |this| {
                this.child(
                    Button::new("update")
                        .icon(IconName::ArrowDownCircle)
                        .tooltip(status)
                        .small()
                        .ghost()
                        .disabled(true),
                )
            })
            .when(staged, |this| {
                this.child(
                    Button::new("restart")
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
}

/// Confirm signing out, then drop the stored credential like signed's exit dialog.
fn open_logout_dialog(window: &mut Window, cx: &mut App) {
    const DESCRIPTION: &str = "Are you sure you want to log out? You will need to re-enter your credentials to sign back in.";

    window.open_alert_dialog(cx, |alert, _, _| {
        alert
            .title("Log Out?")
            .description(DESCRIPTION)
            .button_props(
                DialogButtonProps::default()
                    .ok_text("Log out")
                    .ok_variant(ButtonVariant::Danger)
                    .show_cancel(true),
            )
            .on_ok(|_, _, cx| {
                let nostr = NostrRegistry::global(cx);
                nostr.update(cx, |nostr, cx| nostr.sign_out(cx));
                true
            })
    });
}
