use gpui::{App, AppContext, ParentElement, Styled, Window, px};
use nip29::{GroupReference, GroupsRegistry};
use ui::WindowExtension;
use ui::input::{Input, InputState};
use ui::notification::Notification;

pub fn open(window: &mut Window, cx: &mut App) {
    let input = cx.new(|cx| InputState::new(window, cx).placeholder("naddr1…"));

    window.open_dialog(cx, move |this, _window, _cx| {
        let input = input.clone();

        this.width(px(380.))
            .confirm()
            .title("Join Group")
            .child(Input::new(&input).cleanable(true).flex_1().text_sm())
            .on_ok(move |_event, window, cx| {
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
    });
}
