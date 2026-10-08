use gpui::{
    AnyElement, App, IntoElement, ParentElement, SharedString, Styled, WeakEntity, Window, div, px,
    relative,
};
use nip29::{Group, GroupMetadata};
use theme::ActiveTheme;
use ui::avatar::Avatar;
use ui::{Sizable, StyledExt, WindowExtension, h_flex, v_flex};

pub(crate) fn open(group: WeakEntity<Group>, window: &mut Window, cx: &mut App) {
    window.open_dialog(cx, move |this, _window, cx| {
        this.max_h(px(600.))
            .show_close(true)
            .title("About Group")
            .child(body(&group, cx))
    });
}

fn body(group: &WeakEntity<Group>, cx: &App) -> AnyElement {
    let Some(entity) = group.upgrade() else {
        return div().into_any_element();
    };

    let group = entity.read(cx);
    let mut body = v_flex().gap_3().text_sm().child(header(group, cx));

    if let Some(metadata) = group.metadata() {
        if let Some(about) = metadata.about.as_ref() {
            body = body.child(
                div()
                    .text_color(cx.theme().text_muted)
                    .child(SharedString::from(about.clone())),
            );
        }

        let flags = flags(metadata);

        if !flags.is_empty() {
            body = body.child(section(
                "Flags",
                wrap(flags.into_iter().map(|f| chip(f, cx))),
                cx,
            ));
        }

        if let Some(parent) = metadata.parent.as_ref() {
            body = body.child(section(
                "Parent",
                div()
                    .child(SharedString::from(parent.as_str().to_owned()))
                    .into_any_element(),
                cx,
            ));
        }

        if !metadata.children.is_empty() {
            body = body.child(section(
                "Children",
                wrap(
                    metadata
                        .children
                        .iter()
                        .map(|child| chip(child.as_str().to_owned(), cx)),
                ),
                cx,
            ));
        }

        if let Some(kinds) = metadata.supported_kinds.as_ref() {
            let items: Vec<AnyElement> = kinds
                .iter()
                .map(|kind| chip(kind.to_string(), cx))
                .collect();

            body = body.child(section(
                "Supported kinds",
                if items.is_empty() {
                    div()
                        .text_color(cx.theme().text_placeholder)
                        .child("None")
                        .into_any_element()
                } else {
                    wrap(items)
                },
                cx,
            ));
        }
    }

    let roles = group.roles().roles();
    if !roles.is_empty() {
        body = body.child(section(
            "Roles",
            wrap(roles.iter().map(|role| chip(role.name.clone(), cx))),
            cx,
        ));
    }

    let pins = group.pins().pins();
    if !pins.is_empty() {
        body = body.child(section(
            "Pinned",
            v_flex()
                .gap_1()
                .children(
                    pins.iter()
                        .map(|pin| {
                            div()
                                .truncate()
                                .text_color(cx.theme().text_muted)
                                .child(crate::util::pin_label(group, pin))
                                .into_any_element()
                        })
                        .collect::<Vec<_>>(),
                )
                .into_any_element(),
            cx,
        ));
    }

    body.into_any_element()
}

fn header(group: &Group, cx: &App) -> AnyElement {
    let key = group.key();

    h_flex()
        .gap_3()
        .items_center()
        .child(
            Avatar::from_source(group.display_image())
                .seed(key.cache_tag())
                .large(),
        )
        .child(
            v_flex()
                .min_w_0()
                .child(
                    div()
                        .text_base()
                        .font_semibold()
                        .truncate()
                        .child(group.display_name()),
                )
                .child(
                    div()
                        .text_xs()
                        .text_color(cx.theme().text_placeholder)
                        .truncate()
                        .child(SharedString::from(format!(
                            "{} · {}",
                            key.id().as_str(),
                            key.relay().as_str()
                        ))),
                ),
        )
        .into_any_element()
}

fn section(label: &'static str, content: AnyElement, cx: &App) -> AnyElement {
    v_flex()
        .gap_1()
        .child(
            div()
                .text_xs()
                .font_medium()
                .text_color(cx.theme().text_muted)
                .child(label),
        )
        .child(content)
        .into_any_element()
}

fn wrap(items: impl IntoIterator<Item = AnyElement>) -> AnyElement {
    h_flex()
        .flex_wrap()
        .gap_1()
        .children(items)
        .into_any_element()
}

fn chip(label: impl Into<SharedString>, cx: &App) -> AnyElement {
    div()
        .px_2()
        .py_1()
        .rounded(cx.theme().radius)
        .bg(cx.theme().secondary_background)
        .text_color(cx.theme().secondary_foreground)
        .text_xs()
        .line_height(relative(1.))
        .child(label.into())
        .into_any_element()
}

fn flags(metadata: &GroupMetadata) -> Vec<&'static str> {
    let mut flags = Vec::new();

    if metadata.private {
        flags.push("Private");
    }
    if metadata.restricted {
        flags.push("Restricted");
    }
    if metadata.hidden {
        flags.push("Hidden");
    }
    if metadata.closed {
        flags.push("Closed");
    }
    if metadata.livekit {
        flags.push("LiveKit");
    }

    flags
}
