use anyhow::Error;
use chat::{ChatRegistry, Room, RoomKind};
use gpui::{
    AnyElement, App, AppContext, Context, IntoElement, ParentElement, SharedString, Styled, Task,
    Window, div,
};
use nostr_sdk::prelude::*;
use state::NostrRegistry;
use theme::ActiveTheme;
use ui::button::{Button, ButtonVariants};
use ui::dock::{DockPlacement, PanelHandle};
use ui::notification::Notification;
use ui::{StyledExt, WindowExtension, h_flex, v_flex};

use super::Sidebar;
use super::rows::SidebarRow;

impl Sidebar {
    /// Leave the contact picker and clear its selection.
    fn close_contacts(&mut self) {
        self.contacts_open = false;
        self.selected_contacts.clear();
    }

    /// Show or hide the contact picker in the inbox list.
    pub(super) fn toggle_contacts(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.contacts_open = !self.contacts_open;

        if !self.contacts_open {
            self.selected_contacts.clear();
        } else {
            self.close_requests();
            if self.contacts.is_none() {
                self.load_contacts(window, cx);
            }
        }

        cx.notify();
    }

    /// Load the signed-in user's contact list into the contact picker.
    fn load_contacts(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let nostr = NostrRegistry::global(cx);
        let client = nostr.read(cx).client();

        let Some(public_key) = nostr.read(cx).current_user() else {
            return;
        };

        let task: Task<Result<Vec<PublicKey>, Error>> = cx.background_spawn(async move {
            let filter = Filter::new()
                .author(public_key)
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
                    cx.update(|window, cx| {
                        window.push_notification(
                            Notification::error(error.to_string()).autohide(false),
                            cx,
                        );
                    })?;
                }
            }

            Ok(())
        }));
    }

    /// Toggle a contact's selection in the contact picker.
    pub(super) fn toggle_contact(&mut self, public_key: &PublicKey, cx: &mut Context<Self>) {
        if !self.selected_contacts.remove(public_key) {
            self.selected_contacts.insert(public_key.to_owned());
        }
        cx.notify();
    }

    /// Start a chat with the contacts selected in the picker.
    pub(super) fn create_chat(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.selected_contacts.is_empty() {
            return;
        }

        let nostr = NostrRegistry::global(cx);
        let Some(public_key) = nostr.read(cx).current_user() else {
            return;
        };

        let chat = ChatRegistry::global(cx);
        let async_chat = chat.downgrade();
        let dock = self.dock.clone();

        let receivers: Vec<PublicKey> = self.selected_contacts.iter().copied().collect();

        self.tasks.push(cx.spawn_in(window, async move |this, cx| {
            let room = async_chat.update_in(cx, |chat, _window, cx| {
                let room = cx.new(|_| {
                    Room::new(public_key, receivers)
                        .organize(&public_key)
                        .kind(RoomKind::Ongoing)
                });
                chat.track_room(&room, cx);
                room
            })?;

            cx.update(|window, cx| {
                ui::dock::add_panel_to(
                    &dock,
                    PanelHandle::new(chat_ui::init(room.downgrade(), window, cx)),
                    DockPlacement::Center,
                    window,
                    cx,
                );
            })?;

            // Leave the picker now that the chat is open
            this.update(cx, |this, cx| {
                this.close_contacts();
                cx.notify();
            })?;

            Ok(())
        }));
    }

    /// Leave the requests list in the inbox.
    fn close_requests(&mut self) {
        self.requests_open = false;
    }

    /// Show or hide the chat requests in the inbox list.
    pub(super) fn toggle_requests(&mut self, cx: &mut Context<Self>) {
        self.requests_open = !self.requests_open;

        if self.requests_open {
            self.close_contacts();
            self.new_requests = false;
        }

        cx.notify();
    }

    /// The signed-in user's contacts shown in the picker.
    pub(super) fn contacts_rows(&self) -> Vec<SidebarRow> {
        self.contacts
            .clone()
            .unwrap_or_default()
            .into_iter()
            .map(|public_key| SidebarRow::Contact { public_key })
            .collect()
    }

    /// The chat requests awaiting a response.
    pub(super) fn requests_rows(&self, cx: &App) -> Vec<SidebarRow> {
        ChatRegistry::global(cx)
            .read(cx)
            .rooms(&RoomKind::Request, cx)
            .into_iter()
            .map(|room| SidebarRow::Room { room })
            .collect()
    }

    /// The overlay with the create button and selection count.
    pub(super) fn render_create_overlay(&self, cx: &mut Context<Self>) -> AnyElement {
        v_flex()
            .absolute()
            .bottom_0()
            .left_0()
            .justify_center()
            .h_20()
            .w_full()
            .px_4()
            .border_t_1()
            .border_color(cx.theme().border)
            .bg(cx.theme().background)
            .child(
                Button::new("create-chat")
                    .label(if self.selected_contacts.len() > 1 {
                        "Create Group Chat"
                    } else {
                        "Create Chat"
                    })
                    .primary()
                    .on_click(cx.listener(|this, _ev, window, cx| {
                        this.create_chat(window, cx);
                    })),
            )
            .child(
                h_flex()
                    .gap_1()
                    .text_xs()
                    .text_color(cx.theme().text_muted)
                    .child("Selected")
                    .child(
                        div()
                            .font_semibold()
                            .child(SharedString::from(self.selected_contacts.len().to_string())),
                    ),
            )
            .into_any_element()
    }
}
