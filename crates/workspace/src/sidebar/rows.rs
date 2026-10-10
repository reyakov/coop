use std::ops::Range;

use chat::{Room, RoomKind};
use common::TimestampExt;
use gpui::prelude::FluentBuilder;
use gpui::{
    AnyElement, App, Context, Entity, IntoElement, ParentElement, SharedString, Styled, WeakEntity,
    Window, div, px,
};
use nip29::Group;
use nostr_sdk::prelude::*;
use person::PersonRegistry;
use settings::AppSettings;
use theme::{ActiveTheme, TextStyle, Typeset as _, button_radius};
use ui::avatar::Avatar;
use ui::dialog::DialogButtonProps;
use ui::dock::{ClosePanel, DockArea, DockPlacement, PanelHandle};
use ui::menu::{ContextMenu, PopupMenuItem};
use ui::nav::Nav;
use ui::{Icon, IconName, Selectable, Sizable, StyledExt, WindowExtension, h_flex, v_flex};

use super::Sidebar;
use super::list::ListSection;
use super::search::MessageHit;
use crate::dialogs::{profile, screening};

/// Open a room in the dock, screening new requests when enabled.
fn open_room(room: &Entity<Room>, dock: &WeakEntity<DockArea>, window: &mut Window, cx: &mut App) {
    ui::dock::add_panel_to(
        dock,
        PanelHandle::new(chat_ui::init(room.downgrade(), window, cx)),
        DockPlacement::Center,
        window,
        cx,
    );

    if room.read(cx).kind != RoomKind::Ongoing && AppSettings::get_screening(cx) {
        let peer = room.read(cx).display_member(cx).public_key();
        let view = screening::init(peer, window, cx);

        window.open_dialog(cx, move |this, _window, _cx| {
            this.confirm()
                .child(view.clone())
                .button_props(
                    DialogButtonProps::default()
                        .cancel_text("Ignore")
                        .ok_text("Response"),
                )
                .on_cancel(move |_event, window, cx| {
                    window.dispatch_action(Box::new(ClosePanel), cx);
                    true
                })
        });
    }
}

impl Sidebar {
    pub(super) fn render_rows(
        &self,
        range: Range<usize>,
        rows: &[SidebarRow],
        cx: &Context<Sidebar>,
    ) -> Vec<AnyElement> {
        let hide_avatar = AppSettings::get_hide_avatar(cx);

        rows.get(range.clone())
            .into_iter()
            .flatten()
            .enumerate()
            .map(|(offset, row)| {
                let index = range.start + offset;

                match row {
                    SidebarRow::Section(section) => {
                        let section = *section;
                        let label = section.label();

                        let open = match section {
                            ListSection::Pins => self.pins_open,
                            ListSection::Groups => self.is_groups_open(cx),
                            ListSection::Chats => self.is_chats_open(cx),
                        };

                        let icon = if open {
                            IconName::CaretDown
                        } else {
                            IconName::CaretRight
                        };

                        Nav::new(label)
                            .label(label)
                            .suffix(Icon::new(icon).small().text_color(cx.theme().text_faint))
                            .text_style(TextStyle::Caption)
                            .font_semibold()
                            .text_color(cx.theme().text_faint)
                            .on_click(cx.listener(move |this, _event, _window, cx| {
                                match section {
                                    ListSection::Pins => this.pins_open = !this.pins_open,
                                    ListSection::Groups => {
                                        this.groups_open = Some(!this.is_groups_open(cx))
                                    }
                                    ListSection::Chats => {
                                        this.chats_open = Some(!this.is_chats_open(cx))
                                    }
                                }
                                cx.notify();
                            }))
                            .into_any_element()
                    }
                    SidebarRow::Heading(label) => {
                        let label = *label;

                        Nav::new(label)
                            .label(label)
                            .text_style(TextStyle::Caption)
                            .font_semibold()
                            .text_color(cx.theme().text_faint)
                            .into_any_element()
                    }
                    SidebarRow::Empty(text) => v_flex()
                        .h_10()
                        .w_full()
                        .px_1p5()
                        .items_center()
                        .child(
                            h_flex()
                                .h_9()
                                .w_full()
                                .justify_center()
                                .border_1()
                                .border_dashed()
                                .border_color(cx.theme().border)
                                .rounded(px(button_radius()))
                                .text_style(TextStyle::Caption)
                                .text_color(cx.theme().text_faint)
                                .child(*text),
                        )
                        .into_any_element(),
                    SidebarRow::Room { room } => {
                        let name = room.read(cx).display_name(cx);
                        let picture = room.read(cx).display_image(cx);
                        let seed = room.read(cx).display_image_seed(cx);
                        let created_at = room.read(cx).created_at.to_ago();
                        let id = room.read(cx).id;
                        let member = room.read(cx).members().first().copied();
                        let pinned = AppSettings::global(cx).read(cx).pinned_room(id, cx);
                        let dock = self.dock.clone();
                        let room = room.clone();

                        let nav = Nav::new(index)
                            .label(name)
                            .text_style(TextStyle::Callout)
                            .when(!hide_avatar, |this| {
                                this.prefix(
                                    Avatar::from_source(picture)
                                        .seed(seed)
                                        .small()
                                        .flex_shrink_0(),
                                )
                            })
                            .suffix(
                                div()
                                    .font_normal()
                                    .text_style(TextStyle::Caption)
                                    .text_color(cx.theme().text_faint)
                                    .child(created_at),
                            )
                            .on_click(move |_event, window, cx| {
                                open_room(&room, &dock, window, cx);
                            });

                        ContextMenu::new(
                            format!("room-menu-{index}"),
                            nav,
                            move |menu, _window, _cx| {
                                let menu = menu.item(
                                    PopupMenuItem::new(if pinned { "Unpin" } else { "Pin" })
                                        .on_click(move |_, _window, cx| {
                                            AppSettings::global(cx).update(cx, |settings, cx| {
                                                settings.toggle_pinned_room(id, cx);
                                            });
                                        }),
                                );

                                if let Some(member) = member {
                                    menu.item(PopupMenuItem::new("View profile").on_click(
                                        move |_, window, cx| {
                                            profile::open(member, window, cx);
                                        },
                                    ))
                                } else {
                                    menu
                                }
                            },
                        )
                        .into_any_element()
                    }
                    SidebarRow::Contact { public_key } => {
                        let persons = PersonRegistry::global(cx);
                        let person = persons.read(cx).get(public_key, cx);
                        let selected = self.selected_contacts.contains(public_key);
                        let sidebar = cx.entity().downgrade();
                        let public_key = public_key.to_owned();

                        Nav::new(index)
                            .label(person.name())
                            .text_style(TextStyle::Callout)
                            .when(!hide_avatar, |this| {
                                this.prefix(
                                    Avatar::from_source(person.avatar())
                                        .seed(person.avatar_seed())
                                        .small()
                                        .flex_shrink_0(),
                                )
                            })
                            .selected(selected)
                            .on_click(move |_event, _window, cx| {
                                if let Err(error) = sidebar.update(cx, |this, cx| {
                                    this.toggle_contact(&public_key, cx);
                                }) {
                                    log::error!("Failed to select contact: {error}");
                                }
                            })
                            .into_any_element()
                    }
                    SidebarRow::Profile { public_key } => {
                        let persons = PersonRegistry::global(cx);
                        let person = persons.read(cx).get(public_key, cx);
                        let public_key = public_key.to_owned();

                        Nav::new(index)
                            .label(person.name())
                            .text_style(TextStyle::Callout)
                            .when(!hide_avatar, |this| {
                                this.prefix(
                                    Avatar::from_source(person.avatar())
                                        .seed(person.avatar_seed())
                                        .small()
                                        .flex_shrink_0(),
                                )
                            })
                            .on_click(move |_event, window, cx| {
                                profile::open(public_key, window, cx);
                            })
                            .into_any_element()
                    }
                    SidebarRow::Group { group } => {
                        let dock = self.dock.clone();
                        let sidebar = cx.entity().downgrade();
                        let group = group.clone();

                        let name = group.read(cx).display_name();
                        let picture = group.read(cx).display_image();
                        let seed = group.read(cx).key().cache_tag();
                        let tag = seed.clone();
                        let pinned = AppSettings::global(cx).read(cx).pinned_group(&tag, cx);
                        let weak_group = group.downgrade();

                        let suffix = group
                            .read(cx)
                            .membership()
                            .status()
                            .or_else(|| group.read(cx).elsewhere().map(|_| "Moved"))
                            .map(SharedString::from);

                        let nav = Nav::new(index)
                            .label(name)
                            .text_style(TextStyle::Callout)
                            .when(!hide_avatar, |this| {
                                this.prefix(
                                    Avatar::from_source(picture)
                                        .seed(seed)
                                        .small()
                                        .flex_shrink_0(),
                                )
                            })
                            .when_some(suffix, |this, suffix| {
                                this.suffix(
                                    div()
                                        .font_normal()
                                        .text_style(TextStyle::Caption)
                                        .text_color(cx.theme().text_faint)
                                        .child(suffix),
                                )
                            })
                            .on_click(move |_event, window, cx| {
                                ui::dock::add_panel_to(
                                    &dock,
                                    PanelHandle::new(nip29_ui::init(group.clone(), window, cx)),
                                    DockPlacement::Center,
                                    window,
                                    cx,
                                );

                                if let Err(error) = sidebar.update(cx, |this, cx| {
                                    this.show_group(&group, cx);
                                }) {
                                    log::error!("Failed to show the group in the sidebar: {error}");
                                }
                            });

                        ContextMenu::new(
                            format!("group-menu-{index}"),
                            nav,
                            move |menu, _window, _cx| {
                                menu.item(
                                    PopupMenuItem::new(if pinned { "Unpin" } else { "Pin" })
                                        .on_click({
                                            let tag = tag.clone();
                                            move |_, _window, cx| {
                                                AppSettings::global(cx).update(
                                                    cx,
                                                    |settings, cx| {
                                                        settings.toggle_pinned_group(&tag, cx);
                                                    },
                                                );
                                            }
                                        }),
                                )
                                .item(
                                    PopupMenuItem::new("Leave").on_click({
                                        let group = weak_group.clone();
                                        move |_, window, cx| {
                                            nip29_ui::confirm_leave(group.clone(), window, cx);
                                        }
                                    }),
                                )
                            },
                        )
                        .into_any_element()
                    }
                    SidebarRow::Message { hit } => {
                        let person = PersonRegistry::global(cx).read(cx).get(&hit.author, cx);
                        let name = hit.room.read(cx).display_name(cx);
                        let detail = format!("{} · {}", name, hit.created_at.to_ago());
                        let dock = self.dock.clone();
                        let room = hit.room.clone();
                        let content = hit.content.clone();

                        Nav::new(index)
                            .label(content)
                            .text_style(TextStyle::Callout)
                            .font_medium()
                            .when(!hide_avatar, |this| {
                                this.prefix(
                                    Avatar::from_source(person.avatar())
                                        .seed(person.avatar_seed())
                                        .small()
                                        .flex_shrink_0(),
                                )
                            })
                            .suffix(
                                div()
                                    .font_normal()
                                    .text_style(TextStyle::Caption)
                                    .text_color(cx.theme().text_faint)
                                    .child(detail),
                            )
                            .on_click(move |_event, window, cx| {
                                open_room(&room, &dock, window, cx);
                            })
                            .into_any_element()
                    }
                }
            })
            .collect()
    }
}

/// A row shown in the sidebar's lists.
pub(super) enum SidebarRow {
    Section(ListSection),
    Room {
        room: Entity<Room>,
    },
    Group {
        group: Entity<Group>,
    },
    Contact {
        public_key: PublicKey,
    },
    /// A profile found by global search.
    Profile {
        public_key: PublicKey,
    },
    Message {
        hit: MessageHit,
    },
    /// A non-interactive label, such as a search results section.
    Heading(&'static str),
    Empty(&'static str),
}
