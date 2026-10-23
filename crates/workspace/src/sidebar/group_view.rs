use std::ops::Range;
use std::rc::Rc;

use gpui::prelude::FluentBuilder;
use gpui::{
    AnyElement, ClipboardItem, Context, Entity, InteractiveElement as _, IntoElement,
    ParentElement, SharedString, Styled, WeakEntity, div, px, relative, uniform_list,
};
use gpui_kit::base::StyledExt as _;
use gpui_kit::component::menu::{ContextMenuExt as _, PopupMenuItem};
use gpui_kit::component::{ActiveTheme, Icon, Sizable, h_flex, v_flex};
use nip29::Group;
use nostr_sdk::prelude::*;
use settings::AppSettings;
use state::NostrRegistry;
use ui::{Avatar, IconName};

use super::Sidebar;
use crate::dialogs::profile;
use crate::nav::Nav;

/// A collapsible section of the group view.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum GroupSection {
    Admins,
    Members,
}

impl GroupSection {
    fn label(self) -> &'static str {
        match self {
            Self::Admins => "Admins",
            Self::Members => "Members",
        }
    }
}

/// A row in the group view.
enum GroupRow {
    Section(GroupSection),
    Member {
        public_key: PublicKey,
        roles: Vec<String>,
    },
}

impl Sidebar {
    /// Show the group view, replacing the main list.
    pub(super) fn show_group(&mut self, group: &Entity<Group>, cx: &mut Context<Self>) {
        self.group = Some(group.downgrade());
        self.group_open = true;

        let group = group.read(cx);
        let mut keys: Vec<PublicKey> = group.members().iter().copied().collect();
        keys.extend(
            group
                .admins()
                .entries()
                .iter()
                .map(|(public_key, _)| *public_key),
        );
        self.directory
            .update(cx, |directory, cx| directory.track(keys, cx));

        cx.notify();
    }

    /// Leave the group view, returning the sidebar to its main list.
    pub(super) fn hide_group(&mut self, cx: &mut Context<Self>) {
        if !self.group_open {
            return;
        }

        self.group_open = false;
        cx.notify();
    }

    pub(super) fn render_group(
        &mut self,
        group: Entity<Group>,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let admin = NostrRegistry::global(cx)
            .read(cx)
            .current_user()
            .is_some_and(|me| group.read(cx).admins().contains(&me));

        let (name, seed, picture, total_members, rows) = {
            let group = group.read(cx);
            let admins = group.admins();

            let mut admin_rows = Vec::new();
            let mut member_rows = Vec::new();

            for (public_key, roles) in admins.entries() {
                admin_rows.push(GroupRow::Member {
                    public_key: *public_key,
                    roles: roles.clone(),
                });
            }

            for public_key in group.members().iter() {
                if admins.contains(public_key) {
                    continue;
                }

                member_rows.push(GroupRow::Member {
                    public_key: *public_key,
                    roles: Vec::new(),
                });
            }

            let mut rows = Vec::new();

            if !admin_rows.is_empty() {
                rows.push(GroupRow::Section(GroupSection::Admins));
                if self.admins_open {
                    rows.append(&mut admin_rows);
                }
            }

            if !member_rows.is_empty() {
                rows.push(GroupRow::Section(GroupSection::Members));
                if self.members_open {
                    rows.append(&mut member_rows);
                }
            }

            (
                group.display_name(),
                group.key().cache_tag(),
                group.display_image(),
                SharedString::from(format!("{} members", group.total_members())),
                rows,
            )
        };

        let rows = Rc::new(rows);
        let member_group = group.downgrade();

        v_flex()
            .flex_1()
            .min_h_0()
            .w_full()
            .gap_2()
            .child(
                h_flex()
                    .px_4()
                    .flex_shrink_0()
                    .justify_between()
                    .child(
                        h_flex()
                            .gap_1()
                            .flex_shrink_1()
                            .child(Avatar::new(picture).seed(seed.clone()).small())
                            .child(
                                v_flex()
                                    .flex_1()
                                    .min_w_0()
                                    .child(
                                        div()
                                            .text_xs()
                                            .truncate()
                                            .line_height(relative(1.125))
                                            .font_semibold()
                                            .child(name),
                                    )
                                    .child(
                                        div()
                                            .text_size(px(8.0))
                                            .font_medium()
                                            .text_color(cx.theme().muted_foreground)
                                            .child(total_members),
                                    ),
                            ),
                    )
                    .child(
                        h_flex()
                            .gap_1()
                            .child(nip29_ui::actions(group.downgrade(), cx)),
                    ),
            )
            .child(
                div().min_h_0().flex_1().child(
                    uniform_list(
                        "group-rows",
                        rows.len(),
                        cx.processor(move |this, range, _window, cx| {
                            this.render_group_rows(range, rows.as_slice(), &member_group, admin, cx)
                        }),
                    )
                    .h_full()
                    .px_2(),
                ),
            )
            .into_any_element()
    }

    fn render_group_rows(
        &self,
        range: Range<usize>,
        rows: &[GroupRow],
        group: &WeakEntity<Group>,
        admin: bool,
        cx: &mut Context<Self>,
    ) -> Vec<AnyElement> {
        rows.get(range)
            .into_iter()
            .flatten()
            .map(|row| match row {
                GroupRow::Section(section) => self.section_row(*section, cx),
                GroupRow::Member { public_key, roles } => {
                    self.member_row(public_key, roles, group, admin, cx)
                }
            })
            .collect()
    }

    fn section_row(&self, section: GroupSection, cx: &mut Context<Self>) -> AnyElement {
        let open = match section {
            GroupSection::Admins => self.admins_open,
            GroupSection::Members => self.members_open,
        };
        let icon = if open {
            IconName::CaretDown
        } else {
            IconName::CaretRight
        };
        let label = section.label();

        Nav::new(label)
            .label(label)
            .suffix(
                Icon::new(icon)
                    .small()
                    .text_color(cx.theme().muted_foreground),
            )
            .text_xs()
            .font_semibold()
            .text_color(cx.theme().muted_foreground)
            .on_click(cx.listener(move |this, _event, _window, cx| {
                match section {
                    GroupSection::Admins => this.admins_open = !this.admins_open,
                    GroupSection::Members => this.members_open = !this.members_open,
                }
                cx.notify();
            }))
            .into_any_element()
    }

    fn member_row(
        &self,
        public_key: &PublicKey,
        roles: &[String],
        group: &WeakEntity<Group>,
        admin: bool,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let hide_avatar = AppSettings::get_hide_avatar(cx);

        let person = self.directory.read(cx).person(public_key, cx);
        let group = group.clone();

        let public_key = *public_key;
        let member_is_admin = !roles.is_empty();

        let is_self = NostrRegistry::global(cx)
            .read(cx)
            .current_user()
            .is_some_and(|me| me == public_key);

        let nav = Nav::new(public_key.to_hex())
            .label(person.name())
            .text_sm()
            .clickable(true)
            .when(!hide_avatar, |this| {
                this.prefix(
                    Avatar::new(person.avatar())
                        .seed(person.avatar_seed())
                        .small(),
                )
            })
            .when(!roles.is_empty(), |this| {
                this.suffix(
                    div()
                        .font_normal()
                        .truncate()
                        .text_xs()
                        .text_color(cx.theme().muted_foreground)
                        .child(SharedString::from(roles.join(", "))),
                )
            });
        let row = div()
            .id(public_key.to_hex())
            .w_full()
            .child(nav)
            .context_menu(move |menu, _window, _cx| {
                let menu = menu
                    .item(
                        PopupMenuItem::new("View profile").on_click(move |_, window, cx| {
                            profile::open(public_key, window, cx);
                        }),
                    )
                    .item(PopupMenuItem::new("Copy").on_click(move |_, _, cx| {
                        let npub = public_key.to_bech32().unwrap_or_default();
                        cx.write_to_clipboard(ClipboardItem::new_string(npub));
                    }));

                if !admin || is_self {
                    return menu;
                }

                let menu = menu.separator().item(if member_is_admin {
                    PopupMenuItem::new("Remove admin").on_click({
                        let group = group.clone();
                        move |_event, window, cx| {
                            nip29_ui::remove_admin(group.clone(), public_key, window, cx);
                        }
                    })
                } else {
                    PopupMenuItem::new("Make admin").on_click({
                        let group = group.clone();
                        move |_event, window, cx| {
                            nip29_ui::make_admin(group.clone(), public_key, window, cx);
                        }
                    })
                });

                menu.separator()
                    .item(PopupMenuItem::new("Remove").on_click({
                        let group = group.clone();
                        move |_event, window, cx| {
                            nip29_ui::remove_member(group.clone(), public_key, window, cx);
                        }
                    }))
            });

        row.into_any_element()
    }
}
