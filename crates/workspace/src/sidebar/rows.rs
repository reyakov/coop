use std::ops::Range;

use chat::{Room, RoomKind};
use common::TimestampExt;
use gpui::prelude::FluentBuilder;
use gpui::{
    AnyElement, App, Context, Entity, InteractiveElement as _, IntoElement, ParentElement,
    SharedString, Styled, WeakEntity, Window, div,
};
use gpui_kit::assets::IconName;
use gpui_kit::base::StyledExt as _;
use gpui_kit::component::dialog::DialogButtonProps;
use gpui_kit::component::dock::{ClosePanel, DockArea, DockPlacement, PanelHandle};
use gpui_kit::component::list::ListItem;
use gpui_kit::component::menu::{ContextMenuExt as _, PopupMenuItem};
use gpui_kit::component::tag::Tag;
use gpui_kit::component::{ActiveTheme, Icon, Sizable, WindowExt, h_flex};
use nip29::Group;
use nostr_sdk::prelude::*;
use settings::AppSettings;
use ui::Avatar;

use super::Sidebar;
use super::list::ListSection;
use super::search::MessageHit;
use crate::dialogs::{profile, screening};

/// Open a room in the dock, screening new requests when enabled.
pub(crate) fn open_room(
    room: &Entity<Room>,
    dock: &WeakEntity<DockArea>,
    window: &mut Window,
    cx: &mut App,
) {
    crate::dock::add_panel_to(
        dock,
        PanelHandle::new(chat_ui::init(room.downgrade(), window, cx)),
        DockPlacement::Center,
        window,
        cx,
    );

    if room.read(cx).kind != RoomKind::Ongoing && AppSettings::get_screening(cx) {
        let peer = room.read(cx).display_member(cx).public_key();
        let view = screening::init(peer, window, cx);

        window.open_alert_dialog(cx, move |alert, _window, _cx| {
            alert
                .confirm()
                .child(view.clone())
                .button_props(
                    DialogButtonProps::default()
                        .cancel_text("Ignore")
                        .ok_text("Respond"),
                )
                .on_cancel(move |_event, window, cx| {
                    window.dispatch_action(Box::new(ClosePanel), cx);
                    true
                })
        });
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

impl Sidebar {
    pub(super) fn render_rows(
        &self,
        range: Range<usize>,
        rows: &[SidebarRow],
        cx: &Context<Sidebar>,
    ) -> Vec<AnyElement> {
        let hide_avatar = AppSettings::get_hide_avatar(cx);

        rows.get(range)
            .into_iter()
            .flatten()
            .map(|row| {
                let element = match row {
                    SidebarRow::Section(section) => {
                        let section = *section;
                        let label = section.label();

                        let open = match section {
                            ListSection::Pins => self.pins_open,
                            ListSection::Groups => self.is_groups_open(cx),
                            ListSection::Chats => self.is_chats_open(cx),
                        };

                        let icon = if open {
                            IconName::ChevronDown
                        } else {
                            IconName::ChevronRight
                        };

                        ListItem::new(label)
                            .accessibility_label(label)
                            .h_8()
                            .rounded(cx.theme().radius)
                            .cursor_pointer()
                            .child(div().flex_1().min_w_0().truncate().child(label))
                            .suffix(move |_, cx| {
                                div().flex_shrink_0().child(
                                    Icon::new(icon)
                                        .small()
                                        .text_color(cx.theme().muted_foreground),
                                )
                            })
                            .text_xs()
                            .font_semibold()
                            .text_color(cx.theme().muted_foreground)
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

                        ListItem::new(label)
                            .accessibility_label(label)
                            .h_8()
                            .rounded(cx.theme().radius)
                            .child(div().flex_1().min_w_0().truncate().child(label))
                            .text_xs()
                            .font_semibold()
                            .text_color(cx.theme().muted_foreground)
                            .into_any_element()
                    }
                    SidebarRow::Empty(text) => h_flex()
                        .h_8()
                        .w_full()
                        .px_2()
                        .text_xs()
                        .text_color(cx.theme().muted_foreground)
                        .child(*text)
                        .into_any_element(),
                    SidebarRow::Room { room } => {
                        let id = room.read(cx).id;
                        let name = room.read(cx).display_name(cx);
                        let seed = room.read(cx).display_image_seed(cx);
                        let created_at = room.read(cx).created_at.to_ago();
                        let member = room.read(cx).members().first().copied();
                        let spam =
                            room.read(cx).is_spam(cx) && room.read(cx).kind == RoomKind::Request;

                        let pinned = AppSettings::global(cx).read(cx).pinned_room(id, cx);
                        let dock = self.dock.clone();
                        let room = room.clone();

                        let item = ListItem::new(id)
                            .accessibility_label(name.clone())
                            .h_8()
                            .rounded(cx.theme().radius)
                            .cursor_pointer()
                            .text_sm()
                            .when(!hide_avatar, |this| {
                                this.child(
                                    div().flex_shrink_0().child(
                                        Avatar::new(room.read(cx).display_image(cx))
                                            .seed(seed)
                                            .small(),
                                    ),
                                )
                            })
                            .child(div().flex_1().min_w_0().truncate().child(name))
                            .suffix(move |_, cx| {
                                div().flex_shrink_0().child(
                                    h_flex()
                                        .gap_1()
                                        .when(spam, |this| {
                                            this.child(Tag::danger().xsmall().child("Spam"))
                                        })
                                        .when(!spam, |this| {
                                            this.child(
                                                div()
                                                    .font_normal()
                                                    .text_xs()
                                                    .text_color(cx.theme().muted_foreground)
                                                    .child(created_at.clone()),
                                            )
                                        }),
                                )
                            })
                            .on_click(move |_event, window, cx| {
                                open_room(&room, &dock, window, cx);
                            });

                        div()
                            .id(id)
                            .w_full()
                            .child(item)
                            .context_menu(move |menu, _window, _cx| {
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
                            })
                            .into_any_element()
                    }
                    SidebarRow::Contact { public_key } => {
                        let person = self.directory.read(cx).person(public_key, cx);
                        let selected = self.selected_contacts.contains(public_key);
                        let sidebar = cx.entity().downgrade();
                        let public_key = public_key.to_owned();

                        ListItem::new(public_key.to_hex())
                            .accessibility_label(person.name())
                            .h_8()
                            .rounded(cx.theme().radius)
                            .cursor_pointer()
                            .text_sm()
                            .when(!hide_avatar, |this| {
                                this.child(
                                    div().flex_shrink_0().child(
                                        Avatar::new(person.avatar())
                                            .seed(person.avatar_seed())
                                            .small(),
                                    ),
                                )
                            })
                            .child(div().flex_1().min_w_0().truncate().child(person.name()))
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
                        let person = self.directory.read(cx).person(public_key, cx);
                        let public_key = public_key.to_owned();

                        ListItem::new(public_key.to_hex())
                            .accessibility_label(person.name())
                            .h_8()
                            .rounded(cx.theme().radius)
                            .cursor_pointer()
                            .text_sm()
                            .when(!hide_avatar, |this| {
                                this.child(
                                    div().flex_shrink_0().child(
                                        Avatar::new(person.avatar())
                                            .seed(person.avatar_seed())
                                            .small(),
                                    ),
                                )
                            })
                            .child(div().flex_1().min_w_0().truncate().child(person.name()))
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

                        let item = ListItem::new(tag.clone())
                            .accessibility_label(name.clone())
                            .h_8()
                            .rounded(cx.theme().radius)
                            .cursor_pointer()
                            .text_sm()
                            .when(!hide_avatar, |this| {
                                this.child(
                                    div().flex_shrink_0().child(
                                        Avatar::new(group.read(cx).display_image())
                                            .seed(seed)
                                            .small(),
                                    ),
                                )
                            })
                            .child(div().flex_1().min_w_0().truncate().child(name))
                            .when_some(suffix, |this, suffix| {
                                this.suffix(move |_, cx| {
                                    div().flex_shrink_0().child(
                                        div()
                                            .font_normal()
                                            .text_xs()
                                            .text_color(cx.theme().muted_foreground)
                                            .child(suffix.clone()),
                                    )
                                })
                            })
                            .on_click(move |_event, window, cx| {
                                crate::dock::add_panel_to(
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

                        div()
                            .id(tag.clone())
                            .w_full()
                            .child(item)
                            .context_menu(move |menu, _window, _cx| {
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
                            })
                            .into_any_element()
                    }
                    SidebarRow::Message { hit } => {
                        let person = self.directory.read(cx).person(&hit.author, cx);
                        let name = hit.room.read(cx).display_name(cx);
                        let detail = format!("{} · {}", name, hit.created_at.to_ago());
                        let dock = self.dock.clone();
                        let room = hit.room.clone();
                        let content = hit.content.clone();

                        ListItem::new(hit.author.to_hex())
                            .accessibility_label(content.clone())
                            .h_8()
                            .rounded(cx.theme().radius)
                            .cursor_pointer()
                            .text_sm()
                            .font_medium()
                            .when(!hide_avatar, |this| {
                                this.child(
                                    div().flex_shrink_0().child(
                                        Avatar::new(person.avatar())
                                            .seed(person.avatar_seed())
                                            .small(),
                                    ),
                                )
                            })
                            .child(div().flex_1().min_w_0().truncate().child(content))
                            .suffix(move |_, cx| {
                                div().flex_shrink_0().child(
                                    div()
                                        .font_normal()
                                        .text_xs()
                                        .text_color(cx.theme().muted_foreground)
                                        .child(detail.clone()),
                                )
                            })
                            .on_click(move |_event, window, cx| {
                                open_room(&room, &dock, window, cx);
                            })
                            .into_any_element()
                    }
                };
                // Keep a small gap between rows so selected rows read as pills.
                div().pb_0p5().child(element).into_any_element()
            })
            .collect()
    }
}
