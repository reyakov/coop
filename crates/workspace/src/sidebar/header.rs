#[cfg(not(target_arch = "wasm32"))]
use auto_update::AutoUpdater;
use gpui::prelude::FluentBuilder;
#[cfg(not(target_arch = "wasm32"))]
use gpui::{AnyElement, Entity, IntoElement};
use gpui::{
    App, Context, Div, InteractiveElement, ParentElement, Stateful, Styled, Window, div, px,
};
#[cfg(not(target_arch = "wasm32"))]
use gpui_kit::component::Disableable;
use gpui_kit::component::button::{Button, ButtonVariant, ButtonVariants};
use gpui_kit::component::dialog::DialogButtonProps;
use gpui_kit::component::menu::{DropdownMenu, PopupMenuItem};
use gpui_kit::component::{ActiveTheme, Icon, Selectable, Sizable, WindowExt, h_flex, v_flex};
use nostr_sdk::prelude::*;
use state::NostrRegistry;
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

        h_flex()
            .id("sidebar-user")
            .w_full()
            .h(TAB_BAR_HEIGHT)
            .flex_shrink_0()
            .justify_between()
            .px_2()
            .when(cfg!(target_os = "macos"), |this| this.pl(px(80.)))
            .child(
                Button::new("current-user")
                    .text()
                    .dropdown_caret(true)
                    .min_w_0()
                    .flex_shrink_1()
                    .child(Avatar::new(avatar).seed(profile.avatar_seed()).small())
                    .dropdown_menu(move |this, _window, _cx| {
                        this.menu_with_icon(
                            "Profile",
                            IconName::Profile,
                            Box::new(Command::ShowProfile),
                        )
                        .menu_with_icon("Relays", IconName::Relay, Box::new(Command::ShowRelayList))
                        .separator()
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
