use anyhow::Result;
use gpui::{
    App, AppContext, ClipboardItem, ParentElement, SharedString, Styled, WeakEntity, Window, div,
    px,
};
use nip29::Group;
use nostr_sdk::prelude::SecretKey;
use theme::ActiveTheme;
use ui::dialog::DialogButtonProps;
use ui::input::InputState;
use ui::notification::Notification;
use ui::{WindowExtension, v_flex};

use crate::field;

const CODE_LEN: usize = 12;

pub(crate) fn open(group: WeakEntity<Group>, window: &mut Window, cx: &mut App) {
    let code = SecretKey::generate().to_secret_hex();
    let code: String = code.chars().take(CODE_LEN).collect();

    let input = cx.new(|cx| {
        InputState::new(window, cx)
            .placeholder("Invite code")
            .default_value(code)
    });

    window.open_dialog(cx, move |this, _window, cx| {
        let group = group.clone();
        let input = input.clone();

        this.width(px(420.))
            .confirm()
            .title("Invite link")
            .button_props(
                DialogButtonProps::default()
                    .cancel_text("Cancel")
                    .ok_text("Copy link"),
            )
            .child(
                v_flex().gap_3().child(
                    div()
                        .text_sm()
                        .text_color(cx.theme().text_muted)
                        .child("Share the code with whoever should be able to join. The relay must accept it."),
                )
                .child(field("Code", &input, cx)),
            )
            .on_ok(move |_event, window, cx| {
                let code = input.read(cx).value().trim().to_owned();

                if code.is_empty() {
                    return false;
                }

                let link = group
                    .read_with(cx, |group, _cx| {
                        group.metadata().and_then(|metadata| {
                            metadata
                                .naddr(group.key().relay(), Some(&code))
                                .ok()
                                .map(SharedString::from)
                        })
                    })
                    .ok()
                    .flatten();

                let Some(link) = link else {
                    window.push_notification(
                        Notification::error(
                            "The relay doesn't publish group details, so no invite link can be made",
                        ),
                        cx,
                    );
                    return false;
                };

                match group.update(cx, |group, cx| group.create_invite(&code, cx)) {
                    Ok(task) => {
                        window
                            .spawn(cx, async move |cx| -> Result<()> {
                                if let Err(error) = task.await {
                                    cx.update(|window, cx| {
                                        window.push_notification(
                                            Notification::error(error.to_string()),
                                            cx,
                                        );
                                    })?;
                                    return Ok(());
                                }

                                cx.update(|window, cx| {
                                    cx.write_to_clipboard(ClipboardItem::new_string(
                                        link.to_string(),
                                    ));
                                    window.push_notification(
                                        Notification::success("Invite link copied"),
                                        cx,
                                    );
                                })?;

                                Ok(())
                            })
                            .detach();
                    }
                    Err(error) => log::warn!("nip29: creating an invite failed: {error}"),
                }

                true
            })
    });
}
