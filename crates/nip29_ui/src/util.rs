use anyhow::Result;
use gpui::{App, Div, Entity, ParentElement, SharedString, Styled, Task, Window};
use nip29::{Group, Pin, Row};
use nostr_sdk::prelude::Url;
use theme::ActiveTheme;
use ui::input::{Input, InputState};
use ui::notification::Notification;
use ui::{WindowExtension, v_flex};

const RUN_WINDOW_SECS: u64 = 300;

pub(crate) fn display_name(group: &Group) -> SharedString {
    group
        .metadata()
        .and_then(|metadata| metadata.name())
        .map_or_else(
            || SharedString::from(group.key().id().as_str()),
            SharedString::from,
        )
}

pub(crate) fn attachment_name(url: &Url) -> SharedString {
    url.path_segments()
        .and_then(|mut segments| segments.next_back())
        .filter(|name| !name.is_empty())
        .map_or_else(
            || SharedString::from(url.as_str()),
            |name| SharedString::from(name.to_owned()),
        )
}

pub(crate) fn opens_run(rows: &[Row], index: usize) -> bool {
    let (Some(current), Some(previous)) = (
        rows.get(index),
        index.checked_sub(1).and_then(|index| rows.get(index)),
    ) else {
        return true;
    };

    if !matches!((current, previous), (Row::Message(_), Row::Message(_))) {
        return true;
    }

    current.author() != previous.author()
        || current
            .created_at()
            .as_secs()
            .saturating_sub(previous.created_at().as_secs())
            > RUN_WINDOW_SECS
}

pub(crate) fn field(label: &'static str, input: &Entity<InputState>, cx: &App) -> Div {
    v_flex()
        .gap_1()
        .text_sm()
        .text_color(cx.theme().text_muted)
        .child(label)
        .child(Input::new(input))
}

pub(crate) fn pin_label(group: &Group, pin: &Pin) -> SharedString {
    match pin {
        Pin::Address(coordinate) => SharedString::from(format!("{coordinate}")),
        Pin::Event(id) => {
            let snippet = group
                .message(*id)
                .and_then(|message| message.content.lines().next())
                .map(str::trim)
                .filter(|line| !line.is_empty());

            match snippet {
                Some(snippet) => SharedString::from(snippet.chars().take(48).collect::<String>()),
                None => SharedString::from(id.to_hex().chars().take(8).collect::<String>()),
            }
        }
    }
}

/// Await a group write and surface any relay error as a notification.
pub(crate) fn report(window: &mut Window, cx: &mut App, task: Task<Result<()>>) {
    window
        .spawn(cx, async move |cx| -> Result<()> {
            if let Err(error) = task.await {
                cx.update(|window, cx| {
                    window.push_notification(Notification::error(error.to_string()), cx);
                })?;
            }
            Ok(())
        })
        .detach();
}
