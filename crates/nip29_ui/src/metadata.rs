use gpui::{App, AppContext, Entity, ParentElement, Styled, WeakEntity, Window};
use nip29::{Group, GroupMetadata};
use ui::dialog::DialogButtonProps;
use ui::input::InputState;
use ui::switch::Switch;
use ui::{WindowExtension, v_flex};

use crate::util::field;

#[derive(Clone, Copy, Default)]
struct Flags {
    private: bool,
    restricted: bool,
    hidden: bool,
    closed: bool,
    livekit: bool,
}

impl Flags {
    fn of(metadata: &GroupMetadata) -> Self {
        Self {
            private: metadata.private,
            restricted: metadata.restricted,
            hidden: metadata.hidden,
            closed: metadata.closed,
            livekit: metadata.livekit,
        }
    }
}

pub(crate) fn open(group: WeakEntity<Group>, window: &mut Window, cx: &mut App) {
    let base = group
        .read_with(cx, |group, _cx| group.metadata().cloned())
        .ok()
        .flatten()
        .unwrap_or_default();

    let name = cx.new(|cx| {
        InputState::new(window, cx)
            .placeholder("Group name")
            .default_value(base.name.clone().unwrap_or_default())
    });

    let about = cx.new(|cx| {
        InputState::new(window, cx)
            .placeholder("About")
            .default_value(base.about.clone().unwrap_or_default())
    });

    let picture = cx.new(|cx| {
        InputState::new(window, cx)
            .placeholder("https://example.com/picture.png")
            .default_value(base.picture.clone().unwrap_or_default())
    });

    let flags = cx.new(|_cx| Flags::of(&base));

    window.open_dialog(cx, move |this, _window, cx| {
        let current = *flags.read(cx);

        this.confirm()
            .title("Edit Group")
            .button_props(
                DialogButtonProps::default()
                    .cancel_text("Cancel")
                    .ok_text("Save"),
            )
            .child(
                v_flex()
                    .gap_3()
                    .child(field("Name", &name, cx))
                    .child(field("About", &about, cx))
                    .child(field("Picture", &picture, cx))
                    .child(
                        v_flex()
                            .gap_2()
                            .child(
                                Switch::new("private")
                                    .label("Private")
                                    .description("Only members can read messages.")
                                    .checked(current.private)
                                    .on_click(toggle(&flags, |flags| &mut flags.private)),
                            )
                            .child(
                                Switch::new("restricted")
                                    .label("Restricted")
                                    .description("Only members can send messages.")
                                    .checked(current.restricted)
                                    .on_click(toggle(&flags, |flags| &mut flags.restricted)),
                            )
                            .child(
                                Switch::new("hidden")
                                    .label("Hidden")
                                    .description("Hide the group from non-members.")
                                    .checked(current.hidden)
                                    .on_click(toggle(&flags, |flags| &mut flags.hidden)),
                            )
                            .child(
                                Switch::new("closed")
                                    .label("Closed")
                                    .description("Ignore join requests.")
                                    .checked(current.closed)
                                    .on_click(toggle(&flags, |flags| &mut flags.closed)),
                            )
                            .child(
                                Switch::new("livekit")
                                    .label("LiveKit")
                                    .description("Announce audio and video support.")
                                    .checked(current.livekit)
                                    .on_click(toggle(&flags, |flags| &mut flags.livekit)),
                            ),
                    ),
            )
            .on_ok({
                let group = group.clone();
                let name = name.clone();
                let about = about.clone();
                let picture = picture.clone();
                let flags = flags.clone();

                move |_event, window, cx| {
                    let mut next = group
                        .read_with(cx, |group, _cx| group.metadata().cloned())
                        .ok()
                        .flatten()
                        .unwrap_or_default();

                    next.name = value(&name, cx);
                    next.about = value(&about, cx);
                    next.picture = value(&picture, cx);

                    let current = flags.read(cx);
                    next.private = current.private;
                    next.restricted = current.restricted;
                    next.hidden = current.hidden;
                    next.closed = current.closed;
                    next.livekit = current.livekit;

                    match group.update(cx, |group, cx| group.edit_metadata(&next, cx)) {
                        Ok(task) => crate::util::report(window, cx, task),
                        Err(error) => log::warn!("nip29: editing the group failed: {error}"),
                    }

                    true
                }
            })
    });
}

fn toggle(
    flags: &Entity<Flags>,
    select: fn(&mut Flags) -> &mut bool,
) -> impl Fn(&bool, &mut Window, &mut App) + 'static {
    let flags = flags.clone();

    move |next, _window, cx| {
        flags.update(cx, |flags, cx| {
            *select(flags) = *next;
            cx.notify();
        });
    }
}

fn value(input: &Entity<InputState>, cx: &App) -> Option<String> {
    let value = input.read(cx).value().trim().to_owned();

    (!value.is_empty()).then_some(value)
}
