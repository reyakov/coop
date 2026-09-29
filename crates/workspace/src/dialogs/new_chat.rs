use std::ops::Range;
use std::rc::Rc;

use anyhow::Error;
use chat::{ChatRegistry, Room, RoomKind};
use gpui::prelude::FluentBuilder;
use gpui::{
    AnyElement, App, AppContext, Context, Entity, InteractiveElement, IntoElement, ParentElement,
    Render, SharedString, StatefulInteractiveElement as _, Styled, Subscription, Task, WeakEntity,
    Window, div, px, uniform_list,
};
use nostr_sdk::prelude::*;
use person::PersonRegistry;
use settings::AppSettings;
use state::NostrRegistry;
use theme::ActiveTheme;
use ui::avatar::Avatar;
use ui::button::{Button, ButtonVariants};
use ui::dialog::DialogButtonProps;
use ui::dock::{DockArea, DockPlacement, PanelHandle};
use ui::input::{Input, InputEvent, InputState};
use ui::nav::Nav;
use ui::{Disableable, IconName, Sizable, WindowExtension, h_flex, v_flex};

/// Maximum number of members a chat can have.
const MAX_MEMBERS: usize = 3;

pub fn open(dock: WeakEntity<DockArea>, window: &mut Window, cx: &mut App) {
    let view = cx.new(|cx| NewChat::new(dock, window, cx));

    window.open_dialog(cx, {
        let view = view.clone();
        move |this, _window, _cx| {
            this.width(px(420.))
                .title("New chat")
                .confirm()
                .button_props(
                    DialogButtonProps::default()
                        .cancel_text("Cancel")
                        .ok_text("Start"),
                )
                .on_ok({
                    let view = view.clone();
                    move |_, window, cx| view.update(cx, |this, cx| this.start_chat(window, cx))
                })
                .child(view.clone())
        }
    });
}

pub struct NewChat {
    dock: WeakEntity<DockArea>,
    /// Find input, used to filter contacts or enter a new public key
    input: Entity<InputState>,
    /// Current user's contacts
    contacts: Option<Vec<PublicKey>>,
    /// Public keys selected as members of the chat
    members: Vec<PublicKey>,
    /// Whether a NIP-05 address is being resolved
    resolving: bool,
    error: Option<SharedString>,
    tasks: Vec<Task<Result<(), Error>>>,
    _subscription: Option<Subscription>,
}

impl NewChat {
    fn new(dock: WeakEntity<DockArea>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let input = cx.new(|cx| {
            InputState::new(window, cx).placeholder("Search or enter npub or user@address.me")
        });

        let subscription = cx.subscribe_in(&input, window, |this, _input, event, window, cx| {
            if let InputEvent::PressEnter { .. } = event {
                this.submit(window, cx)
            }
        });

        // Load the contacts once the view is mounted
        cx.defer_in(window, |this, window, cx| {
            this.load_contacts(window, cx);
        });

        Self {
            dock,
            input,
            contacts: None,
            members: Vec::new(),
            resolving: false,
            error: None,
            tasks: Vec::new(),
            _subscription: Some(subscription),
        }
    }

    /// Load the signed-in user's contact list.
    fn load_contacts(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.contacts.is_some() {
            return;
        }

        let nostr = NostrRegistry::global(cx);
        let client = nostr.read(cx).client();

        let Some(current_user) = nostr.read(cx).current_user() else {
            return;
        };

        let task: Task<Result<Vec<PublicKey>, Error>> = cx.background_spawn(async move {
            let filter = Filter::new()
                .author(current_user)
                .kind(Kind::ContactList)
                .limit(1);

            let contacts: Vec<PublicKey> = client
                .database()
                .query(filter)
                .await?
                .into_iter()
                .next()
                .map(|event| event.tags.public_keys().collect())
                .unwrap_or_default();

            Ok(contacts)
        });

        self.tasks.push(cx.spawn_in(window, async move |this, cx| {
            match task.await {
                Ok(contacts) => {
                    this.update(cx, |this, cx| {
                        this.contacts = Some(contacts);
                        cx.notify();
                    })?;
                }
                Err(error) => {
                    this.update(cx, |this, cx| {
                        this.set_error(error.to_string(), cx);
                    })?;
                }
            }

            Ok(())
        }));
    }

    /// The contacts matching the current input, or every contact when empty.
    fn filtered_contacts(&self, cx: &Context<Self>) -> Vec<PublicKey> {
        let Some(contacts) = self.contacts.as_ref() else {
            return Vec::new();
        };

        let persons = PersonRegistry::global(cx);
        let query = self.input.read(cx).value().trim().to_lowercase();

        if query.is_empty() {
            return contacts.clone();
        }

        contacts
            .iter()
            .filter(|public_key| {
                if public_key.to_hex().to_lowercase().contains(&query) {
                    return true;
                }
                let name = persons.read(cx).get(public_key, cx).name();
                name.to_lowercase().contains(&query)
            })
            .cloned()
            .collect()
    }

    /// Resolve the input into a public key and add it as a member.
    fn submit(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.resolving {
            return;
        }

        let query = self.input.read(cx).value().trim().to_string();

        if query.is_empty() {
            return;
        }

        // A raw public key or an npub can be added directly.
        if let Ok(public_key) = PublicKey::parse(&query) {
            if self.try_add(public_key, cx) {
                self.clear_input(window, cx);
            }
            return;
        }

        // A NIP-05 address is resolved before being added.
        if let Ok(address) = Nip05Address::parse(&query) {
            let nostr = NostrRegistry::global(cx);
            let task = nostr.read(cx).query_address(address, cx);

            self.set_resolving(true, window, cx);

            self.tasks.push(cx.spawn_in(window, async move |this, cx| {
                let result = task.await;

                this.update_in(cx, |this, window, cx| {
                    this.set_resolving(false, window, cx);

                    match result {
                        Ok(public_key) => {
                            if this.try_add(public_key, cx) {
                                this.clear_input(window, cx);
                            }
                        }
                        Err(error) => this.set_error(error.to_string(), cx),
                    }
                })?;

                Ok(())
            }));
            return;
        }

        // Fall back to a local search by contact name.
        let mut matches = self.filtered_contacts(cx);
        match matches.len() {
            0 => self.set_error("Enter a valid npub or NIP-05 address", cx),
            1 => {
                let public_key = matches.remove(0);
                if self.try_add(public_key, cx) {
                    self.clear_input(window, cx);
                }
            }
            _ => self.set_error("Multiple contacts match, pick one from the list", cx),
        }
    }

    /// Add a public key to the members if possible, otherwise show an error.
    fn try_add(&mut self, public_key: PublicKey, cx: &mut Context<Self>) -> bool {
        let nostr = NostrRegistry::global(cx);

        let Some(current_user) = nostr.read(cx).current_user() else {
            self.set_error("You are not signed in", cx);
            return false;
        };

        if public_key == current_user {
            self.set_error("You cannot add yourself", cx);
            return false;
        }

        if self.members.contains(&public_key) {
            self.set_error("This person is already added", cx);
            return false;
        }

        if self.members.len() >= MAX_MEMBERS {
            self.set_error(format!("A chat can have up to {MAX_MEMBERS} members"), cx);
            return false;
        }

        self.members.push(public_key);
        self.error = None;
        cx.notify();

        true
    }

    /// Toggle a contact's membership.
    fn toggle_member(&mut self, public_key: &PublicKey, cx: &mut Context<Self>) {
        if self.members.iter().any(|member| member == public_key) {
            self.members.retain(|member| member != public_key);
            self.error = None;
            cx.notify();
        } else {
            self.try_add(public_key.to_owned(), cx);
        }
    }

    fn clear_input(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.input.update(cx, |this, cx| {
            this.set_value("", window, cx);
        });
    }

    /// Toggle the input's loading spinner while a NIP-05 address is resolved.
    fn set_resolving(&mut self, resolving: bool, window: &mut Window, cx: &mut Context<Self>) {
        self.input.update(cx, |this, cx| {
            this.set_loading(resolving, window, cx);
        });
        self.resolving = resolving;
        cx.notify();
    }

    fn start_chat(&mut self, window: &mut Window, cx: &mut Context<Self>) -> bool {
        if self.members.is_empty() {
            self.set_error("Add at least one person to chat with", cx);
            return false;
        }

        let nostr = NostrRegistry::global(cx);
        let chat = ChatRegistry::global(cx);

        let Some(current_user) = nostr.read(cx).current_user() else {
            self.set_error("You are not signed in", cx);
            return false;
        };

        let room = Room::new(current_user, self.members.clone())
            .organize(&current_user)
            .kind(RoomKind::Ongoing);

        let room = chat.update(cx, |chat, cx| {
            let room = cx.new(|_| room);
            chat.track_room(&room, cx);
            room
        });

        ui::dock::add_panel_to(
            &self.dock,
            PanelHandle::new(chat_ui::init(room.downgrade(), window, cx)),
            DockPlacement::Center,
            window,
            cx,
        );

        true
    }

    fn set_error(&mut self, message: impl Into<SharedString>, cx: &mut Context<Self>) {
        self.error = Some(message.into());
        cx.notify();
    }

    fn render_contacts(
        &self,
        range: Range<usize>,
        contacts: &[PublicKey],
        cx: &mut Context<Self>,
    ) -> Vec<AnyElement> {
        let persons = PersonRegistry::global(cx);
        let start = range.start;

        contacts
            .get(range)
            .into_iter()
            .flatten()
            .enumerate()
            .map(|(offset, public_key)| {
                let profile = persons.read(cx).get(public_key, cx);
                let public_key = *public_key;

                Nav::new(start + offset)
                    .label(profile.name())
                    .text_sm()
                    .when(!AppSettings::get_hide_avatar(cx), |this| {
                        this.prefix(
                            Avatar::from_source(profile.avatar())
                                .seed(profile.avatar_seed())
                                .small()
                                .flex_shrink_0(),
                        )
                    })
                    .on_click(cx.listener(move |this, _event, _window, cx| {
                        this.toggle_member(&public_key, cx);
                    }))
                    .into_any_element()
            })
            .collect()
    }

    fn render_members(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let persons = PersonRegistry::global(cx);

        h_flex()
            .flex_wrap()
            .gap_1()
            .children(self.members.iter().map(|public_key| {
                let profile = persons.read(cx).get(public_key, cx);
                let public_key = *public_key;
                let id = SharedString::from(format!("m-{}", public_key.to_hex()));

                h_flex()
                    .id(id)
                    .gap_1()
                    .p_0p5()
                    .rounded_full()
                    .bg(cx.theme().ghost_element_background_alt)
                    .text_size(px(10.))
                    .hover(|this| this.bg(cx.theme().ghost_element_hover))
                    .child(
                        Avatar::from_source(profile.avatar())
                            .seed(profile.avatar_seed())
                            .xsmall()
                            .flex_shrink_0(),
                    )
                    .child(div().max_w_32().truncate().child(profile.name()))
                    .on_click(cx.listener(move |this, _event, _window, cx| {
                        this.toggle_member(&public_key, cx);
                    }))
            }))
    }
}

impl Render for NewChat {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let contacts = Rc::new(self.filtered_contacts(cx));
        let contact_count = contacts.len();
        let notice = SharedString::from(format!("Group Chat can have up to {MAX_MEMBERS} members"));

        v_flex()
            .gap_3()
            .child(
                h_flex()
                    .gap_1()
                    .w_full()
                    .child(Input::new(&self.input).flex_1().text_sm().cleanable(true))
                    .child(
                        Button::new("add")
                            .icon(IconName::Plus)
                            .tooltip("Add person")
                            .ghost()
                            .large()
                            .disabled(self.resolving)
                            .on_click(cx.listener(|this, _event, window, cx| {
                                this.submit(window, cx);
                            })),
                    ),
            )
            .when(!self.members.is_empty(), |this| {
                this.child(self.render_members(cx))
            })
            .map(|this| {
                if let Some(error) = self.error.clone() {
                    this.child(
                        div()
                            .text_xs()
                            .text_color(cx.theme().text_danger)
                            .child(error),
                    )
                } else {
                    this.child(
                        div()
                            .text_xs()
                            .text_color(cx.theme().text_placeholder)
                            .child(notice),
                    )
                }
            })
            .child(
                uniform_list(
                    "contacts",
                    contact_count,
                    cx.processor(move |this, range, _window, cx| {
                        this.render_contacts(range, contacts.as_slice(), cx)
                    }),
                )
                .h_80(),
            )
    }
}
