use std::rc::Rc;

use gpui::{App, AppContext, ClickEvent, ParentElement, Styled, Window, px};
use nip29::{GroupReference, GroupsRegistry};
use ui::button::{Button, ButtonVariants};
use ui::input::{Input, InputState};
use ui::notification::Notification;
use ui::{IconName, StyledExt, WindowExtension, h_flex, v_flex};

type SubmitFn = Rc<dyn Fn(&ClickEvent, &mut Window, &mut App) -> bool>;

pub fn open(window: &mut Window, cx: &mut App) {
    let input = cx.new(|cx| InputState::new(window, cx).placeholder("naddr1…"));

    let join: SubmitFn = {
        let input = input.clone();
        Rc::new(move |_, window, cx| {
            let groups = GroupsRegistry::global(cx);

            let reference = match GroupReference::parse(input.read(cx).value().trim()) {
                Ok(reference) => reference,
                Err(error) => {
                    window.push_notification(Notification::error(error.to_string()), cx);
                    return false;
                }
            };

            groups.update(cx, |registry, cx| {
                registry.join(reference, cx);
            });

            true
        })
    };

    window.open_dialog(cx, move |this, _window, _cx| {
        this.width(px(380.))
            .title("Join Group")
            .on_ok({
                let join = join.clone();
                move |event, window, cx| join(event, window, cx)
            })
            .child(
                v_flex()
                    .gap_3()
                    .child(Input::new(&input).cleanable(true).text_sm())
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
                            .child(
                                Button::new("join")
                                    .icon(IconName::CheckCircle)
                                    .label("Join")
                                    .primary()
                                    .font_semibold()
                                    .on_click({
                                        let join = join.clone();
                                        move |event, window, cx| {
                                            if join(event, window, cx) {
                                                window.close_dialog(cx);
                                            }
                                        }
                                    }),
                            ),
                    ),
            )
    });
}
