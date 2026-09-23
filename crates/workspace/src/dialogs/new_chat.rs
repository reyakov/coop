use chat::{ChatRegistry, Room, RoomKind};
use gpui::prelude::FluentBuilder;
use gpui::{
    App, AppContext, Context, Entity, IntoElement, ParentElement, Render, SharedString, Styled,
    Subscription, Window, div, px,
};
use nostr_sdk::prelude::*;
use state::NostrRegistry;
use theme::ActiveTheme;
use ui::button::{Button, ButtonVariants};
use ui::input::{Input, InputEvent, InputState};
use ui::{StyledExt, WindowExtension, v_flex};

pub fn open(window: &mut Window, cx: &mut App) {
    let view = cx.new(|cx| NewChat::new(window, cx));

    window.open_modal(cx, move |this, _window, _cx| {
        this.width(px(420.)).title("New chat").child(view.clone())
    });
}

pub struct NewChat {
    /// Public key input
    input: Entity<InputState>,

    /// Error message
    error: Option<SharedString>,

    /// Input subscription
    _subscription: Option<Subscription>,
}

impl NewChat {
    fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let input = cx.new(|cx| InputState::new(window, cx).placeholder("npub"));

        let subscription = cx.subscribe_in(&input, window, |this, _input, event, window, cx| {
            if let InputEvent::PressEnter { .. } = event {
                this.start_chat(window, cx);
            }
        });

        Self {
            input,
            error: None,
            _subscription: Some(subscription),
        }
    }

    fn start_chat(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let value = self.input.read(cx).value().to_string();

        let Ok(peer) = PublicKey::parse(&value) else {
            self.set_error("Public key is invalid", cx);
            return;
        };

        let nostr = NostrRegistry::global(cx);
        let Some(current_user) = nostr.read(cx).current_user() else {
            self.set_error("You are not signed in", cx);
            return;
        };

        if peer == current_user {
            self.set_error("You cannot chat with yourself", cx);
            return;
        }

        let room = Room::new(current_user, [peer])
            .organize(&current_user)
            .kind(RoomKind::Ongoing);

        let chat = ChatRegistry::global(cx);
        chat.update(cx, |chat, cx| {
            let room = cx.new(|_| room);
            chat.emit_room(&room, window, cx);
        });

        window.close_modal(cx);
    }

    fn set_error(&mut self, message: impl Into<SharedString>, cx: &mut Context<Self>) {
        self.error = Some(message.into());
        cx.notify();
    }
}

impl Render for NewChat {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        v_flex()
            .gap_4()
            .child(
                v_flex()
                    .gap_1()
                    .text_color(cx.theme().text_muted)
                    .child("Public key of the person you want to chat with")
                    .child(Input::new(&self.input)),
            )
            .child(
                Button::new("start-chat")
                    .label("Start chat")
                    .primary()
                    .font_semibold()
                    .on_click(cx.listener(|this, _event, window, cx| {
                        this.start_chat(window, cx);
                    })),
            )
            .when_some(self.error.clone(), |this, error| {
                this.child(
                    div()
                        .text_xs()
                        .text_center()
                        .text_color(cx.theme().text_danger)
                        .child(error),
                )
            })
    }
}
