use std::collections::BTreeMap;

use common::TimestampExt;
use community::ChatMessage;
use gpui::prelude::FluentBuilder;
use gpui::{
    AnyElement, App, InteractiveElement, IntoElement, ParentElement, SharedString, Styled, div, px,
};
use nostr_sdk::prelude::*;
use person::PersonRegistry;
use settings::AppSettings;
use theme::ActiveTheme;
use ui::avatar::Avatar;
use ui::{StyledExt, h_flex, v_flex};

pub(crate) fn render(ix: usize, message: &ChatMessage, show_author: bool, cx: &App) -> AnyElement {
    let persons = PersonRegistry::global(cx);
    let author = persons.read(cx).get(&message.author, cx);
    let hide_avatar = AppSettings::get_hide_avatar(cx);

    div()
        .id(ix)
        .w_full()
        .py_1()
        .px_3()
        .hover(|this| this.bg(cx.theme().surface_background))
        .child(
            h_flex()
                .items_start()
                .gap_3()
                .when(!hide_avatar, |this| {
                    if show_author {
                        this.child(
                            Avatar::new(author.avatar())
                                .seed(author.avatar_seed())
                                .flex_shrink_0(),
                        )
                    } else {
                        this.child(div().flex_shrink_0().w(px(32.)))
                    }
                })
                .child(
                    v_flex()
                        .flex_1()
                        .min_w_0()
                        .gap_0p5()
                        .when(show_author, |this| {
                            this.child(
                                h_flex()
                                    .gap_2()
                                    .text_sm()
                                    .text_color(cx.theme().text_placeholder)
                                    .child(div().font_semibold().child(author.name()))
                                    .child(
                                        Timestamp::from_secs(message.at_ms / 1000).to_human_time(),
                                    )
                                    .when(message.edited_at.is_some(), |this| {
                                        this.child(div().child("(edited)"))
                                    }),
                            )
                        })
                        .child(content(message, cx))
                        .when(!message.reactions.is_empty(), |this| {
                            this.child(reactions(message, cx))
                        }),
                ),
        )
        .into_any_element()
}

fn content(message: &ChatMessage, cx: &App) -> AnyElement {
    if message.deleted {
        return div()
            .text_color(cx.theme().text_danger)
            .child("Message deleted")
            .into_any_element();
    }

    div()
        .child(SharedString::from(&message.content))
        .into_any_element()
}

fn reactions(message: &ChatMessage, cx: &App) -> AnyElement {
    let mut grouped: BTreeMap<&str, usize> = BTreeMap::new();

    for emoji in message.reactions.values() {
        *grouped.entry(emoji.as_str()).or_default() += 1;
    }

    h_flex()
        .mt_1()
        .gap_1()
        .children(grouped.into_iter().map(|(emoji, count)| {
            h_flex()
                .gap_1()
                .py_0p5()
                .px_1()
                .rounded(cx.theme().radius)
                .border_1()
                .border_color(cx.theme().border)
                .text_xs()
                .child(SharedString::from(emoji))
                .child(SharedString::from(count.to_string()))
        }))
        .into_any_element()
}
