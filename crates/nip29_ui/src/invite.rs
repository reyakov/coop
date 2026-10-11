use anyhow::Result;
use common::TimestampExt;
use gpui::prelude::FluentBuilder;
use gpui::{
    App, AppContext, ClipboardItem, Entity, IntoElement, ParentElement, SharedString, Styled,
    WeakEntity, Window, div, px,
};
use nip29::{Group, Invite};
use nostr_sdk::prelude::SecretKey;
use theme::{ActiveTheme, TextStyle, Typeset as _};
use ui::button::{Button, ButtonVariants};
use ui::input::InputState;
use ui::notification::Notification;
use ui::{Sizable, WindowExtension, h_flex, v_flex};

use crate::util::{field, report};

const CODE_LEN: usize = 12;

pub(crate) fn open(group: WeakEntity<Group>, window: &mut Window, cx: &mut App) {
    let input = cx.new(|cx| {
        InputState::new(window, cx)
            .placeholder("Invite code")
            .default_value(random_code())
    });

    window.open_dialog(cx, move |this, _window, cx| {
        this.max_h(px(600.))
            .show_close(true)
            .title("Invites")
            .child(body(group.clone(), input.clone(), cx))
    });
}

fn random_code() -> String {
    SecretKey::generate()
        .to_secret_hex()
        .chars()
        .take(CODE_LEN)
        .collect()
}

fn body(group: WeakEntity<Group>, input: Entity<InputState>, cx: &App) -> impl IntoElement {
    let invites = group
        .upgrade()
        .map(|entity| entity.read(cx).invites().to_vec())
        .unwrap_or_default();

    v_flex()
        .gap_3()
        .text_style(TextStyle::Callout)
        .child(
            div()
                .text_color(cx.theme().text_muted)
                .child("Anyone with a code can ask to join. Revoking a code stops new joins, it doesn't remove anyone."),
        )
        .when(invites.is_empty(), |this| {
            this.child(
                div()
                    .text_color(cx.theme().text_faint)
                    .child("No active invites"),
            )
        })
        .children(invites.iter().map(|invite| invite_row(group.clone(), invite, cx)))
        .child(create_row(group, input, cx))
}

fn invite_row(group: WeakEntity<Group>, invite: &Invite, cx: &App) -> impl IntoElement {
    let created = div()
        .flex_shrink_0()
        .text_style(TextStyle::Caption)
        .text_color(cx.theme().text_faint)
        .child(invite.created_at.to_ago());

    h_flex()
        .justify_between()
        .gap_2()
        .child(
            h_flex()
                .min_w_0()
                .gap_2()
                .items_center()
                .child(
                    div()
                        .min_w_0()
                        .truncate()
                        .child(SharedString::from(invite.code.clone())),
                )
                .child(created),
        )
        .child(
            h_flex()
                .flex_shrink_0()
                .gap_1()
                .child(copy_button(&group, &invite.code))
                .child(revoke_button(&group, invite.id, &invite.code)),
        )
}

fn copy_button(group: &WeakEntity<Group>, code: &str) -> Button {
    let group = group.clone();
    let code = code.to_owned();

    Button::new(SharedString::from(format!("invite-copy-{code}")))
        .label("Copy")
        .ghost()
        .small()
        .on_click(move |_event, window, cx| {
            let Some(link) = group
                .read_with(cx, |group, _cx| invite_link(group, &code))
                .ok()
                .flatten()
            else {
                window.push_notification(
                    Notification::error(
                        "The relay doesn't publish group details, so no invite link can be made",
                    ),
                    cx,
                );
                return;
            };

            cx.write_to_clipboard(ClipboardItem::new_string(link));
            window.push_notification(Notification::success("Invite link copied"), cx);
        })
}

fn revoke_button(group: &WeakEntity<Group>, id: nostr_sdk::prelude::EventId, code: &str) -> Button {
    let group = group.clone();

    Button::new(SharedString::from(format!("invite-revoke-{code}")))
        .label("Revoke")
        .ghost()
        .small()
        .on_click(move |_event, window, cx| {
            match group.update(cx, |group, cx| group.delete_event(id, cx)) {
                Ok(task) => report(window, cx, task),
                Err(error) => log::warn!("nip29: revoking an invite failed: {error}"),
            }
        })
}

fn create_row(group: WeakEntity<Group>, input: Entity<InputState>, cx: &App) -> impl IntoElement {
    h_flex()
        .items_end()
        .gap_2()
        .child(field("New code", &input, cx).flex_1())
        .child(
            Button::new("invite-create")
                .label("Create & copy link")
                .on_click(move |_event, window, cx| {
                    create(group.clone(), input.clone(), window, cx);
                }),
        )
}

fn invite_link(group: &Group, code: &str) -> Option<String> {
    group
        .metadata()
        .and_then(|metadata| metadata.naddr(group.key().relay(), Some(code)).ok())
}

fn create(group: WeakEntity<Group>, input: Entity<InputState>, window: &mut Window, cx: &mut App) {
    let code = input.read(cx).value().trim().to_owned();

    if code.is_empty() {
        window.push_notification(Notification::error("The invite code is empty"), cx);
        return;
    }

    let Some(link) = group
        .read_with(cx, |group, _cx| invite_link(group, &code))
        .ok()
        .flatten()
    else {
        window.push_notification(
            Notification::error(
                "The relay doesn't publish group details, so no invite link can be made",
            ),
            cx,
        );
        return;
    };

    match group.update(cx, |group, cx| group.create_invite(&code, cx)) {
        Ok(task) => {
            window
                .spawn(cx, async move |cx| -> Result<()> {
                    if let Err(error) = task.await {
                        cx.update(|window, cx| {
                            window.push_notification(Notification::error(error.to_string()), cx);
                        })?;
                        return Ok(());
                    }

                    cx.update(|window, cx| {
                        input.update(cx, |input, cx| {
                            input.set_value("", window, cx);
                        });
                        cx.write_to_clipboard(ClipboardItem::new_string(link));
                        window.push_notification(Notification::success("Invite link copied"), cx);
                    })?;

                    Ok(())
                })
                .detach();
        }
        Err(error) => log::warn!("nip29: creating an invite failed: {error}"),
    }
}
