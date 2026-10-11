use std::sync::Arc;

use gpui::http_client::{AsyncBody, HttpClient};
use gpui::prelude::FluentBuilder;
use gpui::{
    AnyElement, App, AppContext, Entity, IntoElement, ParentElement, SharedString, Styled,
    WeakEntity, Window, div, px,
};
use nip29::{Group, GroupId, GroupMetadata};
use nostr_sdk::prelude::RelayUrl;
use theme::{ActiveTheme, TextStyle, Typeset as _};
use ui::button::{Button, ButtonVariants};
use ui::dialog::DialogButtonProps;
use ui::input::InputState;
use ui::notification::Notification;
use ui::switch::Switch;
use ui::{Sizable, WindowExtension, h_flex, v_flex};

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

    let parent = cx.new(|cx| {
        InputState::new(window, cx)
            .placeholder("Parent group id, empty for a root group")
            .default_value(
                base.parent
                    .as_ref()
                    .map(|id| id.as_str().to_owned())
                    .unwrap_or_default(),
            )
    });

    let children = cx.new(|_cx| base.children.clone());
    let flags = cx.new(|_cx| Flags::of(&base));
    let livekit = cx.new(|_cx| None);

    if let Ok(relay) = group.read_with(cx, |group, _cx| group.key().relay().clone()) {
        let http = cx.http_client();
        let probe = cx.background_spawn(async move { probe_livekit(http, relay).await });
        let livekit = livekit.clone();

        cx.spawn(async move |cx| {
            let supported = probe.await;

            livekit.update(cx, |livekit, cx| {
                *livekit = Some(supported);
                cx.notify();
            });
        })
        .detach();
    }

    window.open_dialog(cx, move |this, _window, cx| {
        let current = *flags.read(cx);
        let supported = *livekit.read(cx) != Some(false);

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
                    .child(field("Parent", &parent, cx))
                    .child(children_section(&children, cx))
                    .child(
                        v_flex()
                            .gap(px(theme::SPACE))
                            .child(
                                Switch::new("private")
                                    .label("Private")
                                    .description("Only members can read group messages")
                                    .checked(current.private)
                                    .on_click(toggle(&flags, |flags| &mut flags.private)),
                            )
                            .child(
                                Switch::new("restricted")
                                    .label("Restricted")
                                    .description("Only members can post messages")
                                    .checked(current.restricted)
                                    .on_click(toggle(&flags, |flags| &mut flags.restricted)),
                            )
                            .child(
                                Switch::new("hidden")
                                    .label("Hidden")
                                    .description("Hidden from non-members, not discoverable")
                                    .checked(current.hidden)
                                    .on_click(toggle(&flags, |flags| &mut flags.hidden)),
                            )
                            .child(
                                Switch::new("closed")
                                    .label("Closed")
                                    .description("Join requests are ignored (invite-only)")
                                    .checked(current.closed)
                                    .on_click(toggle(&flags, |flags| &mut flags.closed)),
                            )
                            .child(
                                Switch::new("livekit")
                                    .label("LiveKit")
                                    .description(if supported {
                                        "Announce audio and video support."
                                    } else {
                                        "The relay doesn't announce LiveKit support."
                                    })
                                    .checked(current.livekit)
                                    .on_click({
                                        let flags = flags.clone();
                                        let livekit = livekit.clone();

                                        move |next, _window, cx| {
                                            if *next && *livekit.read(cx) == Some(false) {
                                                return;
                                            }
                                            flags.update(cx, |flags, cx| {
                                                flags.livekit = *next;
                                                cx.notify();
                                            });
                                        }
                                    }),
                            ),
                    ),
            )
            .on_ok({
                let group = group.clone();
                let name = name.clone();
                let about = about.clone();
                let picture = picture.clone();
                let parent = parent.clone();
                let children = children.clone();
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

                    match value(&parent, cx).map(GroupId::new) {
                        Some(Ok(id)) => next.parent = Some(id),
                        Some(Err(error)) => {
                            window.push_notification(Notification::error(error.to_string()), cx);
                            return false;
                        }
                        None => next.parent = None,
                    }

                    next.children = children.read(cx).clone();

                    // The relay must reject these anyway; catch them early.
                    if let Some(id) = &next.parent {
                        let key = group
                            .read_with(cx, |group, _cx| group.key().id().as_str().to_owned())
                            .ok();

                        if key.as_deref() == Some(id.as_str()) {
                            window.push_notification(
                                Notification::error("A group can't be its own parent"),
                                cx,
                            );
                            return false;
                        }

                        if next.children.iter().any(|child| child == id) {
                            window.push_notification(
                                Notification::error("The parent is one of this group's children"),
                                cx,
                            );
                            return false;
                        }
                    }

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

/// NIP-29: relays announce LiveKit support with a 204 at this well-known path.
async fn probe_livekit(http: Arc<dyn HttpClient>, relay: RelayUrl) -> bool {
    let base = relay
        .as_str()
        .replacen("wss://", "https://", 1)
        .replacen("ws://", "http://", 1);
    let url = format!("{}/.well-known/nip29/livekit", base.trim_end_matches('/'));

    match http.get(&url, AsyncBody::default(), true).await {
        Ok(response) => response.status() == 204,
        Err(_) => false,
    }
}

fn children_section(children: &Entity<Vec<GroupId>>, cx: &App) -> AnyElement {
    let list = children.read(cx);

    let mut section = v_flex()
        .gap_1()
        .text_style(TextStyle::Callout)
        .text_color(cx.theme().text_muted)
        .child("Children");

    if list.is_empty() {
        return section
            .child(
                div()
                    .text_color(cx.theme().text_faint)
                    .child("No subgroups"),
            )
            .into_any_element();
    }

    for (ix, child) in list.iter().enumerate() {
        let order = h_flex()
            .flex_shrink_0()
            .gap_1()
            .when(ix > 0, |this| {
                this.child(move_button(children.clone(), ix, ix - 1, "up"))
            })
            .when(ix + 1 < list.len(), |this| {
                this.child(move_button(children.clone(), ix, ix + 1, "down"))
            });

        section = section.child(
            h_flex()
                .justify_between()
                .gap_2()
                .child(
                    div()
                        .min_w_0()
                        .truncate()
                        .text_color(cx.theme().text)
                        .child(SharedString::from(child.as_str().to_owned())),
                )
                .child(order),
        );
    }

    section.into_any_element()
}

fn move_button(
    children: Entity<Vec<GroupId>>,
    ix: usize,
    target: usize,
    direction: &'static str,
) -> Button {
    Button::new(SharedString::from(format!("child-{direction}-{ix}")))
        .label(if direction == "up" { "Up" } else { "Down" })
        .ghost()
        .small()
        .on_click(move |_event, _window, cx| {
            children.update(cx, |children, cx| {
                if target < children.len() {
                    children.swap(ix, target);
                    cx.notify();
                }
            });
        })
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
