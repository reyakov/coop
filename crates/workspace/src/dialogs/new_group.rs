use std::rc::Rc;

use gpui::{App, AppContext, ClickEvent, Div, Entity, ParentElement, Styled, Window, px};
use nip29::{GroupId, GroupKey, GroupMetadata, GroupsRegistry};
use nostr_sdk::prelude::*;
use theme::ActiveTheme;
use ui::button::{Button, ButtonVariants};
use ui::input::{Input, InputState};
use ui::notification::Notification;
use ui::{IconName, StyledExt, WindowExtension, h_flex, v_flex};

type SubmitFn = Rc<dyn Fn(&ClickEvent, &mut Window, &mut App) -> bool>;

pub fn open(window: &mut Window, cx: &mut App) {
    let name = cx.new(|cx| InputState::new(window, cx).placeholder("Group name"));
    let id = cx.new(|cx| InputState::new(window, cx).placeholder("Group id"));
    let relay = cx.new(|cx| InputState::new(window, cx).placeholder("wss://relay.example.com"));

    let submit: SubmitFn = {
        let (name, id, relay) = (name.clone(), id.clone(), relay.clone());
        Rc::new(move |_, window, cx| create(&name, &id, &relay, window, cx))
    };

    window.open_dialog(cx, move |this, _window, cx| {
        this.width(px(380.))
            .title("New group")
            .on_ok({
                let submit = submit.clone();
                move |event, window, cx| submit(event, window, cx)
            })
            .child(
                v_flex()
                    .gap_3()
                    .child(field("Name", &name, cx))
                    .child(field("Group id", &id, cx))
                    .child(field("Relay", &relay, cx)),
            )
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
                        Button::new("create")
                            .icon(IconName::CheckCircle)
                            .label("Create")
                            .primary()
                            .font_semibold()
                            .on_click({
                                let submit = submit.clone();
                                move |event, window, cx| {
                                    if submit(event, window, cx) {
                                        window.close_dialog(cx);
                                    }
                                }
                            }),
                    ),
            )
    });
}

fn field(label: &'static str, input: &Entity<InputState>, cx: &App) -> Div {
    v_flex()
        .gap_1()
        .text_sm()
        .text_color(cx.theme().text_muted)
        .child(label)
        .child(Input::new(input))
}

fn create(
    name: &Entity<InputState>,
    id: &Entity<InputState>,
    relay: &Entity<InputState>,
    window: &mut Window,
    cx: &mut App,
) -> bool {
    let name_value = name.read(cx).value().trim().to_owned();
    let raw_id = id.read(cx).value().trim().to_owned();
    let raw_relay = relay.read(cx).value().trim().to_owned();

    if name_value.is_empty() {
        window.push_notification(Notification::error("Enter a group name"), cx);
        return false;
    }

    let id = if raw_id.is_empty() {
        slug(&name_value)
    } else {
        raw_id
    };

    let id = match GroupId::new(id) {
        Ok(id) => id,
        Err(error) => {
            window.push_notification(Notification::error(error.to_string()), cx);
            return false;
        }
    };

    let relay = match RelayUrl::parse(&raw_relay) {
        Ok(relay) => relay,
        Err(error) => {
            window.push_notification(Notification::error(error.to_string()), cx);
            return false;
        }
    };

    let mut metadata = GroupMetadata::default();
    metadata.name = Some(name_value);

    let registry = GroupsRegistry::global(cx);
    registry.update(cx, |registry, cx| {
        registry.create(GroupKey::new(relay, id), metadata, cx);
    });

    true
}

fn slug(name: &str) -> String {
    let mut slug = String::with_capacity(name.len());
    let mut separator = false;

    for character in name.chars() {
        if character.is_ascii_alphanumeric() {
            slug.push(character.to_ascii_lowercase());
            separator = false;
        } else if !separator && !slug.is_empty() {
            slug.push('-');
            separator = true;
        }
    }

    slug.trim_end_matches('-').to_owned()
}
