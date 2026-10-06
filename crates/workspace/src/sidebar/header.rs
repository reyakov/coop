use auto_update::AutoUpdater;
use gpui::prelude::FluentBuilder;
use gpui::{
    AnyElement, App, Context, Div, Entity, InteractiveElement, IntoElement, ParentElement,
    Stateful, Styled, div, px,
};
use nostr_sdk::prelude::*;
use person::PersonRegistry;
use theme::TABBAR_HEIGHT;
use ui::avatar::Avatar;
use ui::button::{Button, ButtonVariants};
use ui::menu::DropdownMenu;
use ui::{Disableable, IconName, Selectable, Sizable, TRAFFIC_LIGHT_PADDING, h_flex};

use super::Sidebar;
use crate::Command;

impl Sidebar {
    pub(super) fn render_user(
        &self,
        current_user: &PublicKey,
        cx: &mut Context<Self>,
    ) -> Stateful<Div> {
        let updater = AutoUpdater::try_global(cx);
        let persons = PersonRegistry::global(cx);
        let profile = persons.read(cx).get(current_user, cx);
        let avatar = profile.avatar();
        let avatar_seed = profile.avatar_seed();

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
                        this.min_w(px(256.))
                            .menu_with_icon(
                                "Profile",
                                IconName::Profile,
                                Box::new(Command::ShowProfile),
                            )
                            .menu_with_icon(
                                "Relays",
                                IconName::Relay,
                                Box::new(Command::ShowRelayList),
                            )
                            .separator()
                            .menu_with_icon("Themes", IconName::Sun, Box::new(Command::ToggleTheme))
                            .menu_with_icon(
                                "Settings",
                                IconName::Settings,
                                Box::new(Command::ShowSettings),
                            )
                    }),
            )
            .child(div().flex_1())
            .when_some(updater, |this, updater| {
                this.child(self.render_updater(updater, cx))
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
                        menu.menu_with_icon(
                            "New Group",
                            IconName::Group,
                            Box::new(Command::NewGroup),
                        )
                        .menu_with_icon("Join Group", IconName::Door, Box::new(Command::JoinGroup))
                        .separator()
                        .menu_with_icon(
                            "New Chat",
                            IconName::Message,
                            Box::new(Command::NewChat),
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
