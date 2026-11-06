#![cfg(not(target_arch = "wasm32"))]

use std::rc::Rc;

use gpui::{App, AppContext, ClickEvent, ParentElement, Styled, Window, px};
use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::input::{Input, InputState};
use gpui_kit::component::notification::Notification;
use gpui_kit::component::{WindowExt, h_flex, v_flex};
use nip60::WalletRegistry;
use nostr_sdk::prelude::{EventId, PublicKey};

pub mod wallet;

type SubmitFn = Rc<dyn Fn(&ClickEvent, &mut Window, &mut App) -> bool>;

/// Ask for an amount and send a nutzap to `recipient`, optionally
/// nutzapping the event with `target`.
pub fn open_nutzap(
    window: &mut Window,
    cx: &mut App,
    recipient: PublicKey,
    target: Option<EventId>,
) {
    let input = cx.new(|cx| InputState::new(window, cx).placeholder("Amount (sats)"));

    let send: SubmitFn = {
        let input = input.clone();
        Rc::new(move |_, window, cx| {
            let amount = match input.read(cx).value().trim().parse::<u64>() {
                Ok(amount) if amount > 0 => amount,
                _ => {
                    window.push_notification(Notification::error("Enter an amount in sats"), cx);
                    return false;
                }
            };

            let wallet = WalletRegistry::global(cx);
            wallet.update(cx, |registry, cx| {
                registry.send_nutzap(recipient, amount, target, None, cx);
            });

            true
        })
    };

    window.open_dialog(cx, move |this, _window, _cx| {
        this.width(px(380.))
            .title("Send Nutzap")
            .on_ok({
                let send = send.clone();
                move |event, window, cx| send(event, window, cx)
            })
            .child(
                v_flex()
                    .gap_3()
                    .child(Input::new(&input).cleanable(true))
                    .child(
                        h_flex()
                            .gap_1()
                            .justify_end()
                            .child(
                                Button::new("cancel")
                                    .label("Cancel")
                                    .ghost()
                                    .on_click(|_, window, cx| window.close_dialog(cx)),
                            )
                            .child(Button::new("send").label("Send").primary().on_click({
                                let send = send.clone();
                                move |event, window, cx| {
                                    if send(event, window, cx) {
                                        window.close_dialog(cx);
                                    }
                                }
                            })),
                    ),
            )
    });
}
