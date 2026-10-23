use chat::Message;
use common::TimestampExt;
use gpui::prelude::FluentBuilder;
use gpui::{
    AnyElement, App, Entity, InteractiveElement, IntoElement, ParentElement, SharedString, Styled,
    WeakEntity, div,
};
use gpui_kit::assets::IconName;
use gpui_kit::base::StyledExt as _;
use gpui_kit::component::menu::{ContextMenuExt as _, PopupMenuItem};
use gpui_kit::component::{ActiveTheme, Icon, Sizable, h_flex};
use nip29::{Activity, Group, Reaction};
use nostr_sdk::prelude::{EventId, Kind};
use person::Directory;
use settings::AppSettings;
use state::NostrRegistry;
use ui::{
    Avatar, MessageMedia, MessageRow, REACTION_EMOJIS, ReactionBar, ReplyPreview, group_reactions,
    react_menu,
};

use crate::GroupPanel;

pub(crate) struct Actions {
    pub group: WeakEntity<Group>,
    pub panel: WeakEntity<GroupPanel>,
    pub admin: bool,
    pub pinned: bool,
    pub persons: Entity<Directory>,
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
    let hide_avatar = AppSettings::get_hide_avatar(cx);

    let persons = actions.persons.clone();
    let author = persons.read(cx).person(&message.author, cx);

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
                .small()
                .flex_shrink_0(),
        )
        .author(author.name())
        .timestamp(message.created_at.to_human_time())
        .when(!message.replies_to.is_empty(), |this| {
            this.children(replies(
                &message.replies_to,
                &persons,
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
        });

    let id = message.id;
    let admin = actions.admin;
    let pinned = actions.pinned;
    let group = actions.group;
    let panel = actions.panel;

    // Reading the panel here would re-enter its ongoing update, so resolve
    // the current user's custom emojis through the shared person directory.
    let custom = nostr
        .read(cx)
        .current_user()
        .map(|current_user| {
            persons
                .read(cx)
                .person(&current_user, cx)
                .emojis()
                .iter()
                .map(|(shortcode, _)| format!(":{shortcode}:"))
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();

    let row = div()
        .id(SharedString::from(format!("message-menu-{}", id.to_hex())))
        .w_full()
        .child(row)
        .context_menu(move |menu, window, cx| {
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
                    let custom = custom.clone();

                    move |menu, _window, _cx| {
                        let group = group.clone();
                        let panel = panel.clone();

                        react_menu(menu, REACTION_EMOJIS, &custom, move |emoji, _window, cx| {
                            if let Err(error) = panel.update_in(cx, |panel, window, cx| {
                                panel.react(group.clone(), id, emoji.to_owned(), window, cx);
                            }) {
                                log::warn!("nip29: sending a reaction failed: {error}");
                            }
                        })
                    }
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
        });

    row.into_any_element()
}

pub(crate) fn render_activity(
    ix: usize,
    activity: &Activity,
    persons: &Entity<Directory>,
    cx: &App,
) -> AnyElement {
    let subject = persons.read(cx).person(&activity.subject(), cx);
    let hide_avatar = AppSettings::get_hide_avatar(cx);

    h_flex()
        .id(ix)
        .w_full()
        .gap_2()
        .py_1()
        .px_3()
        .hover(|this| this.bg(cx.theme().list_hover))
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
                .text_xs()
                .font_semibold()
                .child(subject.name()),
        )
        .child(
            div()
                .min_w_0()
                .truncate()
                .text_xs()
                .text_color(cx.theme().muted_foreground)
                .child(SharedString::from(action(activity))),
        )
        .child(
            div()
                .flex_shrink_0()
                .ml_1()
                .text_xs()
                .text_color(cx.theme().muted_foreground)
                .child(activity.created_at.to_ago()),
        )
        .into_any_element()
}

fn badge(activity: &Activity, cx: &App) -> AnyElement {
    let (icon, color) = match activity.kind {
        Kind::GroupJoinRequest => (IconName::LogIn, cx.theme().primary),
        Kind::GroupPutUser => (IconName::LogIn, cx.theme().primary),
        Kind::GroupLeaveRequest => (IconName::LogOut, cx.theme().danger),
        Kind::GroupRemoveUser => (IconName::Ban, cx.theme().danger),
        _ => (IconName::Info, cx.theme().muted_foreground),
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
    persons: &Entity<Directory>,
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
            let author = persons.read(cx).person(&message.author, cx);

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
