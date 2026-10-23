use std::rc::Rc;

use chat::{ChatRegistry, RoomKind};
use gpui::prelude::FluentBuilder;
use gpui::{
    AnyElement, App, Context, IntoElement, ParentElement, SharedString, Styled, div, uniform_list,
};
use gpui_kit::base::StyledExt as _;
use gpui_kit::component::{ActiveTheme, Icon, Sizable, h_flex, v_flex};
use nip29::GroupsRegistry;
use settings::AppSettings;
use ui::IconName;

use super::Sidebar;
use super::rows::SidebarRow;
use crate::Command;
use crate::nav::Nav;

/// A collapsible section of the sidebar's main list.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum ListSection {
    Pins,
    Groups,
    Chats,
}

impl ListSection {
    pub(super) fn label(self) -> &'static str {
        match self {
            Self::Pins => "Pinned",
            Self::Groups => "Public Groups",
            Self::Chats => "Chats",
        }
    }
}

impl Sidebar {
    pub(super) fn render_list(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let requests_open = self.requests_open;
        let contacts_open = self.contacts_open && !requests_open;
        let rows = Rc::new(self.rows_for(cx));

        // Whether to show the create button for the contact picker
        let create_button = contacts_open && !self.selected_contacts.is_empty();

        let (list_id, title) = if contacts_open {
            ("sidebar-contacts", Some("Contacts"))
        } else if requests_open {
            ("sidebar-requests", Some("Requests"))
        } else {
            ("sidebar-list", None)
        };

        let request_count = ChatRegistry::global(cx)
            .read(cx)
            .count(&RoomKind::Request, cx);

        v_flex()
            .size_full()
            .flex_1()
            .min_h_0()
            .relative()
            .child(
                v_flex()
                    .px_2()
                    .gap_1()
                    .items_start()
                    .justify_start()
                    .child(
                        Nav::new("browse")
                            .prefix(Icon::new(IconName::Compass).small())
                            .label("Browse")
                            .text_sm()
                            .on_click(|_, window, cx| {
                                window.dispatch_action(Box::new(Command::ShowBrowse), cx)
                            }),
                    )
                    .child(
                        Nav::new("reqs")
                            .prefix(Icon::new(IconName::Invite).small())
                            .label("Requests")
                            .text_sm()
                            .when(request_count > 0, |this| {
                                this.suffix(
                                    h_flex()
                                        .gap_1()
                                        .child(
                                            h_flex()
                                                .py_0p5()
                                                .px_1()
                                                .min_w_6()
                                                .justify_center()
                                                .text_xs()
                                                .font_medium()
                                                .text_color(cx.theme().muted_foreground)
                                                .text_center()
                                                .rounded(cx.theme().radius)
                                                .bg(cx.theme().secondary)
                                                .child(SharedString::from(
                                                    request_count.to_string(),
                                                )),
                                        )
                                        .when(self.new_requests && !requests_open, |this| {
                                            this.child(
                                                div()
                                                    .size_1()
                                                    .rounded_full()
                                                    .bg(cx.theme().primary),
                                            )
                                        }),
                                )
                            })
                            .when(requests_open, |this| this.bg(cx.theme().list_active))
                            .on_click(cx.listener(|this, _event, _window, cx| {
                                this.toggle_requests(cx);
                            })),
                    )
                    .child(
                        Nav::new("contacts")
                            .prefix(Icon::new(IconName::Book).small())
                            .label("Contacts")
                            .text_sm()
                            .when(contacts_open, |this| this.bg(cx.theme().list_active))
                            .on_click(cx.listener(|this, _event, window, cx| {
                                this.toggle_contacts(window, cx);
                            })),
                    ),
            )
            .when_some(title, |this, title| {
                this.child(
                    h_flex()
                        .px_4()
                        .h_10()
                        .w_full()
                        .flex_shrink_0()
                        .text_color(cx.theme().muted_foreground)
                        .text_xs()
                        .font_semibold()
                        .child(title),
                )
            })
            .child(
                uniform_list(
                    list_id,
                    rows.len(),
                    cx.processor(move |this, range, _window, cx| {
                        this.render_rows(range, rows.as_slice(), cx)
                    }),
                )
                .min_h_0()
                .flex_1()
                .px_2(),
            )
            .when(create_button, |this| {
                this.child(self.render_create_overlay(cx))
            })
            .into_any_element()
    }

    fn rows_for(&self, cx: &App) -> Vec<SidebarRow> {
        if self.contacts_open {
            return self.contacts_rows();
        }

        if self.requests_open {
            return self.requests_rows(cx);
        }

        let mut rows = Vec::new();

        let chat = ChatRegistry::global(cx);
        let pinned_rooms = AppSettings::get_pinned_rooms(cx);

        let (pinned_rooms, rooms): (Vec<_>, Vec<_>) = chat
            .read(cx)
            .rooms(&RoomKind::Ongoing, cx)
            .into_iter()
            .partition(|room| pinned_rooms.contains(&room.read(cx).id));

        let registry = GroupsRegistry::global(cx);
        let pinned_group_ids = AppSettings::get_pinned_groups(cx);

        let (pinned_groups, groups): (Vec<_>, Vec<_>) = registry
            .read(cx)
            .groups()
            .to_vec()
            .into_iter()
            .partition(|group| {
                let tag = group.read(cx).key().cache_tag();
                pinned_group_ids.iter().any(|pinned| pinned == &tag)
            });

        rows.push(SidebarRow::Section(ListSection::Pins));
        if self.pins_open {
            if pinned_rooms.is_empty() && pinned_groups.is_empty() {
                rows.push(SidebarRow::Empty("Nothing pinned yet"));
            } else {
                rows.extend(
                    pinned_groups
                        .into_iter()
                        .map(|group| SidebarRow::Group { group }),
                );
                rows.extend(
                    pinned_rooms
                        .into_iter()
                        .map(|room| SidebarRow::Room { room }),
                );
            }
        }

        rows.push(SidebarRow::Section(ListSection::Groups));
        if self.is_groups_open(cx) {
            if groups.is_empty() {
                rows.push(SidebarRow::Empty("No groups yet"));
            } else {
                rows.extend(groups.into_iter().map(|group| SidebarRow::Group { group }));
            }
        }

        rows.push(SidebarRow::Section(ListSection::Chats));
        if self.is_chats_open(cx) {
            if rooms.is_empty() {
                rows.push(SidebarRow::Empty("No chats yet"));
            } else {
                rows.extend(rooms.into_iter().map(|room| SidebarRow::Room { room }));
            }
        }

        rows
    }

    /// Whether anything is pinned; drives the sections' default expansion.
    fn pinned_any(cx: &App) -> bool {
        !AppSettings::get_pinned_rooms(cx).is_empty()
            || !AppSettings::get_pinned_groups(cx).is_empty()
    }

    pub(super) fn is_chats_open(&self, cx: &App) -> bool {
        self.chats_open.unwrap_or_else(|| !Self::pinned_any(cx))
    }

    pub(super) fn is_groups_open(&self, cx: &App) -> bool {
        self.groups_open.unwrap_or_else(|| !Self::pinned_any(cx))
    }
}
