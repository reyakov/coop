use gpui::{App, AppContext, Div, Entity, ParentElement, Styled, Window, px};
use nip29::{GroupId, GroupKey, GroupMetadata, GroupsRegistry};
use nostr_sdk::prelude::*;
use theme::ActiveTheme;
use ui::input::{Input, InputState};
use ui::notification::Notification;
use ui::{WindowExtension, v_flex};

pub fn open(window: &mut Window, cx: &mut App) {
    let name = cx.new(|cx| InputState::new(window, cx).placeholder("Group name"));
    let id = cx.new(|cx| InputState::new(window, cx).placeholder("Group id"));
    let relay = cx.new(|cx| InputState::new(window, cx).placeholder("wss://relay.example.com"));

    window.open_dialog(cx, move |this, _window, cx| {
        let name = name.clone();
        let id = id.clone();
        let relay = relay.clone();

        this.width(px(380.))
            .confirm()
            .title("New group")
            .child(
                v_flex()
                    .gap_3()
                    .child(field("Name", &name, cx))
                    .child(field("Group id", &id, cx))
                    .child(field("Relay", &relay, cx)),
            )
            .on_ok(move |_event, window, cx| create(&name, &id, &relay, window, cx))
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
