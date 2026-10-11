use chat::Message;
use common::TimestampExt;
use gpui::prelude::FluentBuilder;
use gpui::{
    AnyElement, App, InteractiveElement, IntoElement, ParentElement, SharedString, Styled,
    WeakEntity, div, px,
};
use nip29::{Activity, Group, Reaction};
use nostr_sdk::prelude::{EventId, Kind};
use person::PersonRegistry;
use settings::AppSettings;
use state::NostrRegistry;
use theme::{ActiveTheme, TextStyle, Typeset as _};
use ui::avatar::Avatar;
use ui::menu::{ContextMenu, PopupMenu, PopupMenuItem};
use ui::message::{MessageMedia, MessageRow, ReactionBar, ReplyPreview, group_reactions};
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
    let nostr = NostrRegistry::global(cx);
    let persons = PersonRegistry::global(cx);
    let hide_avatar = AppSettings::get_hide_avatar(cx);

    let author = persons.read(cx).get(&message.author, cx);

    let reaction_groups = group_reactions(
        reactions.iter().map(|reaction| {
            (
                SharedString::from(reaction.emoji.as_str()),
                reaction.image.clone(),
                reaction.author,
            )
        }),
        nostr.read(cx).current_user(),
    );

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
        .when(!message.replies_to.is_empty(), |this| {
            this.children(replies(
                &message.replies_to,
                &actions.group,
                &actions.panel,
                cx,
            ))
        })
        .child(content)
        .when(!message.media.is_empty(), |this| {
            this.child(MessageMedia::new(&message.media))
        })
        .when(!reaction_groups.is_empty(), |this| {
            this.child(ReactionBar::new(reaction_groups))
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
            let menu = menu
                .item({
                    let panel = panel.clone();

                    PopupMenuItem::new("Reply").on_click(move |_event, _window, cx| {
                        let _ = panel.update_in(cx, |panel, window, cx| {
                            panel.start_reply(id, window, cx);
                        });
                    })
                })
                .submenu("React", window, cx, {
                    let group = group.clone();
                    let panel = panel.clone();

                    move |menu, _window, cx| react_menu(menu, group.clone(), panel.clone(), id, cx)
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
                            let _ = panel.update_in(cx, |panel, window, cx| {
                                panel.toggle_pin(group.clone(), id, window, cx);
                            });
                        },
                    )
                })
                .item({
                    let group = group.clone();
                    let panel = panel.clone();

                    PopupMenuItem::new("Delete message").on_click(move |_event, _window, cx| {
                        let _ = panel.update_in(cx, |panel, window, cx| {
                            panel.delete_message(group.clone(), id, window, cx);
                        });
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
    cx: &App,
) -> PopupMenu {
    let menu = REACTIONS.iter().fold(menu, |menu, emoji| {
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
    });

    let emojis = PersonRegistry::current_emojis(cx);
    if emojis.is_empty() {
        return menu;
    }

    let menu = menu.separator();
    emojis.into_iter().fold(menu, |menu, (shortcode, _)| {
        let emoji = format!(":{shortcode}:");
        let group = group.clone();
        let panel = panel.clone();

        menu.item(
            PopupMenuItem::new(emoji.clone()).on_click(move |_event, _window, cx| {
                if let Err(error) = panel.update_in(cx, |panel, window, cx| {
                    panel.react(group.clone(), id, emoji.clone(), window, cx);
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

fn replies(
    targets: &[EventId],
    group: &WeakEntity<Group>,
    panel: &WeakEntity<GroupPanel>,
    cx: &App,
) -> Vec<AnyElement> {
    let Some(entity) = group.upgrade() else {
        return Vec::new();
    };

    let mut items = Vec::with_capacity(targets.len());

    for (ix, id) in targets.iter().enumerate() {
        let context = entity.read_with(cx, |group, cx| {
            let message = group.message(*id)?;
            let persons = PersonRegistry::global(cx);
            let author = persons.read(cx).get(&message.author, cx);

            Some((author.name(), message.preview()))
        });

        let (author, preview) = context.unwrap_or_else(|| {
            (
                SharedString::from("Message"),
                SharedString::from("original not loaded"),
            )
        });

        let id = *id;
        let panel = panel.clone();

        items.push(
            ReplyPreview::new(ix, author, preview)
                .on_click(move |_event, _window, cx| {
                    if let Err(error) = panel.update(cx, |panel, cx| {
                        panel.scroll_to(&id, cx);
                    }) {
                        log::warn!("nip29: scrolling to message failed: {error}");
                    }
                })
                .into_any_element(),
        );
    }

    items
}
