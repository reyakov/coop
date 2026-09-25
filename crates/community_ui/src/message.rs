use std::collections::BTreeMap;

use common::TimestampExt;
use community::ChatMessage;
use gpui::prelude::FluentBuilder;
use gpui::{AnyElement, App, IntoElement, ParentElement, SharedString, Styled, div};
use nostr_sdk::prelude::*;
use person::PersonRegistry;
use settings::AppSettings;
use theme::ActiveTheme;
use ui::avatar::Avatar;
use ui::h_flex;
use ui::message::MessageRow;

pub(crate) fn render(
    ix: usize,
    message: &ChatMessage,
    content: AnyElement,
    show_author: bool,
    cx: &App,
) -> AnyElement {
    let persons = PersonRegistry::global(cx);
    let author = persons.read(cx).get(&message.author, cx);
    let hide_avatar = AppSettings::get_hide_avatar(cx);

    MessageRow::new(ix)
        .show_author(show_author)
        .hide_avatar(hide_avatar)
        .avatar(
            Avatar::new(author.avatar())
                .seed(author.avatar_seed())
                .flex_shrink_0(),
        )
        .author(author.name())
        .timestamp(Timestamp::from_secs(message.at_ms / 1000).to_human_time())
        .when(message.edited_at.is_some(), |this| {
            this.header_extra(div().child("(edited)"))
        })
        .child(content)
        .when(!message.reactions.is_empty(), |this| {
            this.child(reactions(message, cx))
        })
        .into_any_element()
}

/// The placeholder shown in place of the body of a deleted message.
pub(crate) fn deleted(cx: &App) -> AnyElement {
    div()
        .text_color(cx.theme().text_danger)
        .child("Message deleted")
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
