use gpui::{
    AnyElement, App, IntoElement, ParentElement, SharedString, Styled, WeakEntity, Window, div,
    rems,
};
use gpui_kit::base::StyledExt as _;
use gpui_kit::component::description_list::{DescriptionItem, DescriptionList};
use gpui_kit::component::tag::Tag;
use gpui_kit::component::{ActiveTheme, Sizable, WindowExt, h_flex, v_flex};
use nip29::{Group, GroupMetadata};
use ui::Avatar;

pub(crate) fn open(group: WeakEntity<Group>, window: &mut Window, cx: &mut App) {
    window.open_dialog(cx, move |this, _window, cx| {
        this.max_h(rems(37.5))
            .close_button(true)
            .title("About group")
            .child(body(&group, cx))
    });
}

fn body(group: &WeakEntity<Group>, cx: &App) -> AnyElement {
    let Some(entity) = group.upgrade() else {
        return div().into_any_element();
    };

    let group = entity.read(cx);
    let mut body = v_flex().gap_3().text_xs().child(header(group, cx));
    let mut items = Vec::new();

    if let Some(metadata) = group.metadata() {
        if let Some(about) = metadata.about.as_ref() {
            body = body.child(
                div()
                    .text_color(cx.theme().muted_foreground)
                    .child(SharedString::from(about.clone())),
            );
        }

        let flags = flags(metadata);

        if !flags.is_empty() {
            items.push(
                DescriptionItem::new("Flags").value(wrap(
                    flags
                        .into_iter()
                        .map(|flag| Tag::secondary().child(flag).into_any_element()),
                )),
            );
        }

        if let Some(parent) = metadata.parent.as_ref() {
            items.push(
                DescriptionItem::new("Parent").value(
                    div()
                        .child(SharedString::from(parent.as_str().to_owned()))
                        .into_any_element(),
                ),
            );
        }

        if !metadata.children.is_empty() {
            items.push(
                DescriptionItem::new("Children").value(wrap(metadata.children.iter().map(
                    |child| {
                        Tag::secondary()
                            .child(SharedString::from(child.as_str().to_owned()))
                            .into_any_element()
                    },
                ))),
            );
        }

        if let Some(kinds) = metadata.supported_kinds.as_ref() {
            let values: Vec<AnyElement> = kinds
                .iter()
                .map(|kind| {
                    Tag::secondary()
                        .child(SharedString::from(kind.to_string()))
                        .into_any_element()
                })
                .collect();

            items.push(
                DescriptionItem::new("Supported kinds").value(if values.is_empty() {
                    div()
                        .text_color(cx.theme().muted_foreground)
                        .child("None")
                        .into_any_element()
                } else {
                    wrap(values)
                }),
            );
        }
    }

    let roles = group.roles().roles();
    if !roles.is_empty() {
        items.push(
            DescriptionItem::new("Roles").value(wrap(roles.iter().map(|role| {
                Tag::secondary()
                    .child(SharedString::from(role.name.clone()))
                    .into_any_element()
            }))),
        );
    }

    let pins = group.pins().pins();
    if !pins.is_empty() {
        items.push(
            DescriptionItem::new("Pinned").value(
                v_flex()
                    .gap_1()
                    .children(
                        pins.iter()
                            .map(|pin| {
                                div()
                                    .truncate()
                                    .text_color(cx.theme().muted_foreground)
                                    .child(crate::util::pin_label(group, pin))
                                    .into_any_element()
                            })
                            .collect::<Vec<_>>(),
                    )
                    .into_any_element(),
            ),
        );
    }

    if !items.is_empty() {
        body = body.child(
            DescriptionList::vertical()
                .bordered(false)
                .columns(1)
                .children(items),
        );
    }

    body.into_any_element()
}

fn header(group: &Group, cx: &App) -> AnyElement {
    let key = group.key();

    h_flex()
        .gap_3()
        .items_center()
        .child(
            Avatar::new(group.display_image())
                .seed(key.cache_tag())
                .large(),
        )
        .child(
            v_flex()
                .min_w_0()
                .child(
                    div()
                        .text_sm()
                        .font_semibold()
                        .truncate()
                        .child(group.display_name()),
                )
                .child(
                    div()
                        .text_xs()
                        .text_color(cx.theme().muted_foreground)
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

fn wrap(items: impl IntoIterator<Item = AnyElement>) -> AnyElement {
    h_flex()
        .flex_wrap()
        .gap_1()
        .children(items)
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
