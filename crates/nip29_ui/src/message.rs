use std::collections::BTreeMap;

use chat::Message;
use common::TimestampExt;
use gpui::prelude::FluentBuilder;
use gpui::{
    AnyElement, App, InteractiveElement, IntoElement, ObjectFit, ParentElement, SharedString,
    SharedUri, Styled, StyledImage, WeakEntity, div, img, px,
};
use nip29::{Activity, Group, Reaction};
use nostr_sdk::prelude::{EventId, Kind, PublicKey};
use person::PersonRegistry;
use settings::AppSettings;
use state::NostrRegistry;
use theme::{ActiveTheme, TextStyle, Typeset as _, bubble_radius, control_radius};
use ui::avatar::Avatar;
use ui::menu::{ContextMenu, PopupMenu, PopupMenuItem};
use ui::message::MessageRow;
use ui::{Icon, IconName, Sizable, StyledExt, h_flex};

use crate::GroupPanel;

const REACTIONS: [&str; 6] = ["👍", "❤️", "😂", "🎉", "😮", "😢"];

pub(crate) struct Actions {
    pub group: WeakEntity<Group>,
    pub panel: WeakEntity<GroupPanel>,
    pub admin: bool,
    pub pinned: bool,
}

pub(crate) fn render(
    ix: usize,
    message: &Message,
    content: AnyElement,
    show_author: bool,
    reactions: &[Reaction],
    actions: Actions,
    cx: &App,
) -> AnyElement {
    let persons = PersonRegistry::global(cx);
    let author = persons.read(cx).get(&message.author, cx);
    let hide_avatar = AppSettings::get_hide_avatar(cx);

    let row = MessageRow::new(ix)
        .show_author(show_author)
        .hide_avatar(hide_avatar)
        .avatar(
            Avatar::new(author.avatar())
                .seed(author.avatar_seed())
                .flex_shrink_0(),
        )
        .author(author.name())
        .timestamp(message.created_at.to_human_time())
        .child(content)
        .when(!message.media.is_empty(), |this| {
            this.child(media(&message.media, cx))
        })
        .when(!reactions.is_empty(), |this| {
            this.child(reaction_bar(reactions, cx))
        })
        .into_any_element();

    let id = message.id;
    let admin = actions.admin;
    let pinned = actions.pinned;
    let group = actions.group;
    let panel = actions.panel;

    ContextMenu::new(
        format!("message-menu-{}", id.to_hex()),
        row,
        move |menu, window, cx| {
            let menu = menu.submenu("React", window, cx, {
                let group = group.clone();
                let panel = panel.clone();

                move |menu, _window, _cx| react_menu(menu, group.clone(), panel.clone(), id)
            });

            if !admin {
                return menu;
            }

            menu.separator()
                .item({
                    let group = group.clone();
                    let panel = panel.clone();

                    PopupMenuItem::new(if pinned { "Unpin" } else { "Pin" }).on_click(
                        move |_event, _window, cx| {
                            if let Err(error) = panel.update_in(cx, |panel, window, cx| {
                                panel.toggle_pin(group.clone(), id, window, cx);
                            }) {
                                log::warn!("nip29: pinning a message failed: {error}");
                            }
                        },
                    )
                })
                .item({
                    let group = group.clone();
                    let panel = panel.clone();

                    PopupMenuItem::new("Delete message").on_click(move |_event, _window, cx| {
                        if let Err(error) = panel.update_in(cx, |panel, window, cx| {
                            panel.delete_message(group.clone(), id, window, cx);
                        }) {
                            log::warn!("nip29: deleting a message failed: {error}");
                        }
                    })
                })
        },
    )
    .into_any_element()
}

fn react_menu(
    menu: PopupMenu,
    group: WeakEntity<Group>,
    panel: WeakEntity<GroupPanel>,
    id: EventId,
) -> PopupMenu {
    REACTIONS.iter().fold(menu, |menu, emoji| {
        let emoji = *emoji;
        let group = group.clone();
        let panel = panel.clone();

        menu.item(
            PopupMenuItem::new(emoji).on_click(move |_event, _window, cx| {
                if let Err(error) = panel.update_in(cx, |panel, window, cx| {
                    panel.react(group.clone(), id, emoji.to_owned(), window, cx);
                }) {
                    log::warn!("nip29: sending a reaction failed: {error}");
                }
            }),
        )
    })
}

pub(crate) fn render_activity(ix: usize, activity: &Activity, cx: &App) -> AnyElement {
    let persons = PersonRegistry::global(cx);
    let subject = persons.read(cx).get(&activity.subject(), cx);
    let hide_avatar = AppSettings::get_hide_avatar(cx);

    h_flex()
        .id(ix)
        .w_full()
        .gap(px(theme::SPACE))
        .py_1()
        .px_3()
        .hover(|this| this.bg(cx.theme().element_hover))
        .child(badge(activity, cx))
        .when(!hide_avatar, |this| {
            this.child(
                Avatar::new(subject.avatar())
                    .seed(subject.avatar_seed())
                    .small()
                    .flex_shrink_0(),
            )
        })
        .child(
            div()
                .min_w_0()
                .truncate()
                .text_style(TextStyle::Callout)
                .font_semibold()
                .child(subject.name()),
        )
        .child(
            div()
                .min_w_0()
                .truncate()
                .text_style(TextStyle::Callout)
                .text_color(cx.theme().text_faint)
                .child(SharedString::from(action(activity))),
        )
        .child(
            div()
                .flex_shrink_0()
                .ml_1()
                .text_style(TextStyle::Caption)
                .text_color(cx.theme().text_faint)
                .child(activity.created_at.to_ago()),
        )
        .into_any_element()
}

fn badge(activity: &Activity, cx: &App) -> AnyElement {
    let (icon, color) = match activity.kind {
        Kind::GroupJoinRequest => (IconName::Login, cx.theme().accent),
        Kind::GroupPutUser => (IconName::Login, cx.theme().accent),
        Kind::GroupLeaveRequest => (IconName::Logout, cx.theme().danger),
        Kind::GroupRemoveUser => (IconName::Ban, cx.theme().danger),
        _ => (IconName::Info, cx.theme().text_faint),
    };

    h_flex()
        .flex_shrink_0()
        .justify_center()
        .size_8()
        .child(Icon::new(icon).small().text_color(color))
        .into_any_element()
}

fn action(activity: &Activity) -> String {
    match activity.kind {
        Kind::GroupJoinRequest => "joined the group".to_owned(),
        Kind::GroupLeaveRequest => "left the group".to_owned(),
        Kind::GroupRemoveUser => "was removed from the group".to_owned(),
        Kind::GroupPutUser => format!("is now {}", activity.roles.join(", ")),
        _ => String::new(),
    }
}

fn reaction_bar(reactions: &[Reaction], cx: &App) -> AnyElement {
    let me = NostrRegistry::global(cx).read(cx).current_user();

    let mut grouped: BTreeMap<String, Vec<PublicKey>> = BTreeMap::new();

    for reaction in reactions {
        grouped
            .entry(reaction.emoji.clone())
            .or_default()
            .push(reaction.author);
    }

    h_flex()
        .mt_1()
        .gap_1()
        .children(grouped.into_iter().map(|(emoji, authors)| {
            let mine = me.is_some_and(|me| authors.contains(&me));

            h_flex()
                .gap(px(theme::SPACE))
                .py_0p5()
                .px_1()
                .rounded(px(control_radius()))
                .text_style(TextStyle::Caption)
                .border_1()
                .when(mine, |this| {
                    this.text_color(cx.theme().text)
                        .bg(cx.theme().surface_raised)
                        .border_color(cx.theme().element_active)
                })
                .when(!mine, |this| this.border_color(cx.theme().border))
                .child(SharedString::from(emoji))
                .child(SharedString::from(authors.len().to_string()))
        }))
        .into_any_element()
}

fn media(media: &[SharedUri], cx: &App) -> AnyElement {
    if let [only] = media {
        return div()
            .child(
                img(only.clone())
                    .border_1()
                    .border_color(cx.theme().border_faint)
                    .h(px(250.))
                    .object_fit(ObjectFit::Cover)
                    .rounded(px(bubble_radius())),
            )
            .into_any_element();
    }

    div()
        .w_full()
        .flex_1()
        .flex()
        .flex_row()
        .flex_wrap()
        .gap(px(theme::SPACE))
        .children(media.iter().enumerate().map(|(ix, item)| {
            div().id(format!("media-{ix}")).child(
                img(item.clone())
                    .h_32()
                    .border_1()
                    .border_color(cx.theme().border_faint)
                    .rounded(px(bubble_radius())),
            )
        }))
        .into_any_element()
}
