use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::sync::Arc;

pub use actions::*;
use anyhow::{Context as AnyhowContext, Error};
use chat::{Message, RenderedMessage, Room, RoomEvent, SendReport, SendStatus};
use common::RenderedTimestamp;
use gpui::prelude::FluentBuilder;
use gpui::{
    AnyElement, App, AppContext, ClipboardItem, Context, Entity, EventEmitter, FocusHandle,
    Focusable, InteractiveElement, IntoElement, ListAlignment, ListOffset, ListState, MouseButton,
    ObjectFit, ParentElement, PathPromptOptions, Render, SharedString, StatefulInteractiveElement,
    Styled, StyledImage, Subscription, Task, WeakEntity, Window, deferred, div, img, list, px, red,
    relative, svg, white,
};
use itertools::Itertools;
use nostr_sdk::prelude::*;
use person::{Person, PersonRegistry};
use settings::{AppSettings, SignerKind};
use smallvec::{SmallVec, smallvec};
use smol::lock::RwLock;
use state::{NostrRegistry, upload};
use theme::ActiveTheme;
use ui::avatar::Avatar;
use ui::button::{Button, ButtonVariants};
use ui::dock::{Panel, PanelEvent};
use ui::input::{InputEvent, InputState, TextInput};
use ui::menu::DropdownMenu;
use ui::notification::Notification;
use ui::scroll::Scrollbar;
use ui::{
    Disableable, Icon, IconName, InteractiveElementExt, Sizable, StyledExt, WindowExtension,
    h_flex, v_flex,
};

use crate::text::RenderedText;

mod actions;
mod text;

const ANNOUNCEMENT: &str =
    "This conversation is private. Only members can see each other's messages.";

pub fn init(room: WeakEntity<Room>, window: &mut Window, cx: &mut App) -> Entity<ChatPanel> {
    cx.new(|cx| ChatPanel::new(room, window, cx))
}

/// Chat Panel
pub struct ChatPanel {
    id: SharedString,
    focus_handle: FocusHandle,

    /// Chat Room
    room: WeakEntity<Room>,

    /// Message list state
    list_state: ListState,

    /// All messages
    messages: BTreeSet<Message>,

    /// Mapping message ids to their rendered texts
    rendered_texts_by_id: BTreeMap<EventId, RenderedText>,

    /// Mapping message (rumor event) ids to their reports
    reports_by_id: Entity<BTreeMap<EventId, Vec<SendReport>>>,

    /// Chat input state
    input: Entity<InputState>,

    /// Subject input state
    subject_input: Entity<InputState>,

    /// Subject bar visibility
    subject_bar: Entity<bool>,

    /// Sent message ids
    sent_ids: Arc<RwLock<Vec<EventId>>>,

    /// Replies to
    replies_to: Entity<HashSet<EventId>>,

    /// Media Attachment
    attachments: Entity<Vec<Url>>,

    /// Upload state
    uploading: bool,

    /// Async operations
    tasks: Vec<Task<Result<(), Error>>>,

    /// Event subscriptions
    subscriptions: SmallVec<[Subscription; 3]>,
}

impl ChatPanel {
    pub fn new(room: WeakEntity<Room>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        // Define attachments and replies_to entities
        let attachments = cx.new(|_| vec![]);
        let replies_to = cx.new(|_| HashSet::new());
        let reports_by_id = cx.new(|_| BTreeMap::new());

        // Define list of messages
        let messages = BTreeSet::from([Message::system()]);
        let list_state = ListState::new(messages.len(), ListAlignment::Bottom, px(1024.));

        // Get room id and name
        let (id, name) = room
            .read_with(cx, |this, _cx| {
                let id = this.id.to_string().into();
                let name = this.display_name(cx);

                (id, name)
            })
            .unwrap_or(("Unknown".into(), "Message...".into()));

        // Define input state
        let input = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder(format!("Message {}", name))
                .auto_grow(1, 20)
                .prevent_new_line_on_enter()
                .clean_on_escape()
        });

        // Define subject input state
        let subject_input = cx.new(|cx| InputState::new(window, cx).placeholder("New subject..."));
        let subject_bar = cx.new(|_cx| false);

        // Define subscriptions
        let mut subscriptions = smallvec![];

        subscriptions.push(
            // Subscribe the chat input event
            cx.subscribe_in(&input, window, move |this, _input, event, window, cx| {
                if let InputEvent::PressEnter { .. } = event {
                    this.send_text_message(window, cx);
                };
            }),
        );

        subscriptions.push(
            // Subscribe the subject input event
            cx.subscribe_in(
                &subject_input,
                window,
                move |this, _input, event, window, cx| {
                    if let InputEvent::PressEnter { .. } = event {
                        this.change_subject(window, cx);
                    };
                },
            ),
        );

        // Define all functions that will run after the current cycle
        cx.defer_in(window, |this, window, cx| {
            this.connect(cx);
            this.handle_notifications(cx);
            this.subscribe_room_events(window, cx);
            this.get_messages(window, cx);
        });

        Self {
            focus_handle: cx.focus_handle(),
            id,
            messages,
            room,
            list_state,
            input,
            subject_input,
            subject_bar,
            replies_to,
            attachments,
            rendered_texts_by_id: BTreeMap::new(),
            reports_by_id,
            sent_ids: Arc::new(RwLock::new(Vec::new())),
            uploading: false,
            subscriptions,
            tasks: vec![],
        }
    }

    /// Get messaging relays and announcement for each member
    fn connect(&mut self, cx: &mut Context<Self>) {
        if let Some(room) = self.room.upgrade() {
            let task = room.read(cx).connect(cx);
            self.tasks.push(task);
        }
    }

    /// Handle nostr notifications
    fn handle_notifications(&mut self, cx: &mut Context<Self>) {
        let nostr = NostrRegistry::global(cx);
        let client = nostr.read(cx).client();
        let sent_ids = self.sent_ids.clone();
        let reports = self.reports_by_id.downgrade();

        let (tx, rx) = flume::bounded::<Arc<SendStatus>>(256);

        self.tasks.push(cx.background_spawn(async move {
            let mut notifications = client.notifications();

            while let Some(notification) = notifications.next().await {
                if let ClientNotification::Message {
                    message:
                        RelayMessage::Ok {
                            event_id,
                            status,
                            message,
                        },
                    relay_url,
                } = notification
                {
                    let sent_ids = sent_ids.read().await;

                    if sent_ids.contains(&event_id) {
                        let status = if status {
                            SendStatus::ok(event_id, relay_url)
                        } else {
                            SendStatus::failed(event_id, relay_url, message.into())
                        };
                        tx.send_async(Arc::new(status)).await.ok();
                    }
                }
            }

            Ok(())
        }));

        self.tasks.push(cx.spawn(async move |_this, cx| {
            while let Ok(status) = rx.recv_async().await {
                reports.update(cx, |this, cx| {
                    for reports in this.values_mut() {
                        for report in reports.iter_mut() {
                            let Some(output) = report.output.as_mut() else {
                                continue;
                            };
                            match &*status {
                                SendStatus::Ok { id, relay } => {
                                    if output.id() == id {
                                        output.success.insert(relay.clone());
                                    }
                                }
                                SendStatus::Failed { id, relay, message } => {
                                    if output.id() == id {
                                        output.failed.insert(relay.clone(), message.clone());
                                    }
                                }
                            }
                            cx.notify();
                        }
                    }
                })?;
            }
            Ok(())
        }));
    }

    /// Subscribe to room events
    fn subscribe_room_events(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(room) = self.room.upgrade() {
            self.subscriptions.push(cx.subscribe_in(
                &room,
                window,
                move |this, _room, event, window, cx| {
                    match event {
                        RoomEvent::Incoming(message) => {
                            this.insert_message(message, false, cx);
                        }
                        RoomEvent::Reload => {
                            this.get_messages(window, cx);
                        }
                    };
                },
            ));
        }
    }

    /// Load all messages belonging to this room
    fn get_messages(&mut self, _window: &mut Window, cx: &mut Context<Self>) {
        let Ok(get_messages) = self.room.read_with(cx, |this, cx| this.get_messages(cx)) else {
            return;
        };

        self.tasks.push(cx.spawn(async move |this, cx| {
            let events = get_messages.await?;

            // Update message list
            this.update(cx, |this, cx| {
                this.insert_messages(&events, cx);
            })?;

            Ok(())
        }));
    }

    /// Get user input content and merged all attachments if available
    fn get_input_value(&self, cx: &Context<Self>) -> String {
        // Get input's value
        let mut content = self.input.read(cx).value().trim().to_string();

        // Get all attaches and merge its with message
        let attachments = self.attachments.read(cx);

        if !attachments.is_empty() {
            let urls = attachments
                .iter()
                .map(|url| url.to_string())
                .collect_vec()
                .join("\n");

            if content.is_empty() {
                content = urls;
            } else {
                content = format!("{content}\n{urls}");
            }
        }

        content
    }

    fn change_subject(&mut self, _window: &mut Window, cx: &mut Context<Self>) {
        let subject = self.subject_input.read(cx).value();

        self.room
            .update(cx, |this, cx| {
                this.set_subject(subject, cx);
            })
            .ok();
    }

    fn send_text_message(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        // Get the message which includes all attachments
        let content = self.get_input_value(cx);

        // Return if message is empty
        if content.trim().is_empty() {
            window.push_notification("Cannot send an empty message", cx);
            return;
        }

        self.send_message(&content, window, cx);
    }

    /// Send a message to all members of the chat
    fn send_message(&mut self, value: &str, window: &mut Window, cx: &mut Context<Self>) {
        if value.trim().is_empty() {
            window.push_notification("Cannot send an empty message", cx);
            return;
        }

        // Get room entity
        let room = self.room.clone();

        // Get content and replies
        let replies: Vec<EventId> = self.replies_to.read(cx).iter().copied().collect();
        let content = value.to_string();

        self.tasks.push(cx.spawn_in(window, async move |this, cx| {
            let room = room.upgrade().context("Room is not available")?;

            this.update_in(cx, |this, window, cx| {
                match room.read(cx).rumor(content, replies, cx) {
                    Some(rumor) => {
                        this.insert_message(&rumor, true, cx);
                        this.send_and_wait(rumor, window, cx);
                        this.clear(window, cx);
                    }
                    None => {
                        window.push_notification("Failed to create message", cx);
                    }
                }
            })?;

            Ok(())
        }));
    }

    /// Send message in the background and wait for the response
    fn send_and_wait(&mut self, rumor: UnsignedEvent, window: &mut Window, cx: &mut Context<Self>) {
        let sent_ids = self.sent_ids.clone();
        // This can't fail, because we already ensured that the ID is set
        let id = rumor.id.unwrap();

        // Upgrade room reference
        let Some(room) = self.room.upgrade() else {
            return;
        };

        // Get the send message task
        let Some(task) = room.read(cx).send(rumor, cx) else {
            window.push_notification("Failed to send message", cx);
            return;
        };

        self.tasks.push(cx.spawn_in(window, async move |this, cx| {
            // Send and get reports
            let outputs = task.await;

            // Add sent IDs to the list
            let mut sent_ids = sent_ids.write().await;
            sent_ids.extend(outputs.iter().filter_map(|output| output.gift_wrap_id));

            // Update the state
            this.update(cx, |this, cx| {
                this.insert_reports(id, outputs, cx);
            })?;

            Ok(())
        }))
    }

    /// Clear the input field, attachments, and replies
    ///
    /// Only run after sending a message
    fn clear(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.input.update(cx, |this, cx| {
            this.set_value("", window, cx);
        });
        self.attachments.update(cx, |this, cx| {
            this.clear();
            cx.notify();
        });
        self.replies_to.update(cx, |this, cx| {
            this.clear();
            cx.notify();
        })
    }

    /// Insert reports
    fn insert_reports(&mut self, id: EventId, reports: Vec<SendReport>, cx: &mut Context<Self>) {
        self.reports_by_id.update(cx, |this, cx| {
            this.insert(id, reports);
            cx.notify();
        });
    }

    /// Insert a message into the chat panel
    fn insert_message<E>(&mut self, m: E, scroll: bool, cx: &mut Context<Self>)
    where
        E: Into<Message>,
    {
        let old_len = self.messages.len();

        // Extend the messages list with the new events
        if self.messages.insert(m.into()) {
            self.list_state.splice(old_len..old_len, 1);

            if scroll {
                self.list_state.scroll_to(ListOffset {
                    item_ix: self.list_state.item_count(),
                    offset_in_item: px(0.0),
                });
            }

            cx.notify();
        }
    }

    /// Convert and insert a vector of nostr events into the chat panel
    fn insert_messages(&mut self, events: &[UnsignedEvent], cx: &mut Context<Self>) {
        for event in events.iter() {
            // Bulk inserting messages, so no need to scroll to the latest message
            self.insert_message(event, false, cx);
        }
    }

    /// Check if a message is pending
    fn sent_pending(&self, id: &EventId, cx: &App) -> bool {
        self.reports_by_id
            .read(cx)
            .get(id)
            .is_some_and(|reports| reports.iter().all(|r| r.pending()))
    }

    /// Check if a message has any reports
    fn has_reports(&self, id: &EventId, cx: &App) -> bool {
        self.reports_by_id.read(cx).get(id).is_some()
    }

    /// Get all sent reports for a message by its ID
    fn sent_reports(&self, id: &EventId, cx: &App) -> Option<Vec<SendReport>> {
        self.reports_by_id.read(cx).get(id).cloned()
    }

    /// Get a message by its ID
    fn message(&self, id: &EventId) -> Option<&RenderedMessage> {
        self.messages.iter().find_map(|msg| {
            if let Message::User(rendered) = msg
                && &rendered.id == id
            {
                return Some(rendered);
            }
            None
        })
    }

    fn scroll_to(&self, id: EventId) {
        if let Some(ix) = self.messages.iter().position(|m| {
            if let Message::User(msg) = m {
                msg.id == id
            } else {
                false
            }
        }) {
            self.list_state.scroll_to_reveal_item(ix);
        }
    }

    fn copy_author(&self, public_key: &PublicKey, cx: &App) {
        let content = public_key.to_bech32().unwrap();
        let item = ClipboardItem::new_string(content);

        cx.write_to_clipboard(item);
    }

    fn copy_message(&self, id: &EventId, cx: &App) {
        let Some(message) = self.message(id) else {
            return;
        };
        let content = message.content.to_string();
        let item = ClipboardItem::new_string(content);

        cx.write_to_clipboard(item);
    }

    fn reply_to(&mut self, id: &EventId, cx: &mut Context<Self>) {
        if let Some(text) = self.message(id) {
            self.replies_to.update(cx, |this, cx| {
                this.insert(text.id);
                cx.notify();
            });
        }
    }

    fn remove_reply(&mut self, id: &EventId, cx: &mut Context<Self>) {
        self.replies_to.update(cx, |this, cx| {
            this.remove(id);
            cx.notify();
        });
    }

    fn upload(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        // Get the user's configured blossom server
        let server = AppSettings::get_file_server(cx);

        // Ask user for file upload
        let path = cx.prompt_for_paths(PathPromptOptions {
            files: true,
            directories: false,
            multiple: false,
            prompt: None,
        });

        self.tasks.push(cx.spawn_in(window, async move |this, cx| {
            this.update(cx, |this, cx| {
                this.set_uploading(true, cx);
            })?;

            let mut paths = path.await??.context("Not found")?;
            let path = paths.pop().context("No path")?;

            // Upload via blossom client
            match upload(server, path, cx).await {
                Ok(url) => {
                    this.update_in(cx, |this, _window, cx| {
                        this.add_attachment(url, cx);
                        this.set_uploading(false, cx);
                    })?;
                }
                Err(e) => {
                    this.update_in(cx, |this, window, cx| {
                        this.set_uploading(false, cx);
                        window.push_notification(
                            Notification::error(e.to_string()).autohide(false),
                            cx,
                        );
                    })?;
                }
            }

            Ok(())
        }));
    }

    fn set_uploading(&mut self, uploading: bool, cx: &mut Context<Self>) {
        self.uploading = uploading;
        cx.notify();
    }

    fn add_attachment(&mut self, url: Url, cx: &mut Context<Self>) {
        self.attachments.update(cx, |this, cx| {
            this.push(url);
            cx.notify();
        });
    }

    fn remove_attachment(&mut self, url: &Url, _window: &mut Window, cx: &mut Context<Self>) {
        self.attachments.update(cx, |this, cx| {
            if let Some(ix) = this.iter().position(|this| this == url) {
                this.remove(ix);
                cx.notify();
            }
        });
    }

    fn profile(&self, public_key: &PublicKey, cx: &App) -> Person {
        let persons = PersonRegistry::global(cx);
        persons.read(cx).get(public_key, cx)
    }

    fn on_command(&mut self, command: &Command, window: &mut Window, cx: &mut Context<Self>) {
        match command {
            Command::Insert(content) => {
                self.send_message(content, window, cx);
            }
            Command::ChangeSubject(subject) => {
                if self
                    .room
                    .update(cx, |this, cx| {
                        this.set_subject(subject, cx);
                    })
                    .is_err()
                {
                    window.push_notification(
                        Notification::error("Failed to change subject").autohide(false),
                        cx,
                    );
                }
            }
            Command::ChangeSigner(kind) => {
                if self
                    .room
                    .update(cx, |this, cx| {
                        this.set_signer_kind(kind, cx);
                    })
                    .is_err()
                {
                    window.push_notification(
                        Notification::error("Failed to change signer").autohide(false),
                        cx,
                    );
                }
            }
            Command::ToggleBackup => {
                if self
                    .room
                    .update(cx, |this, cx| {
                        this.set_backup(cx);
                    })
                    .is_err()
                {
                    window.push_notification(
                        Notification::error("Failed to toggle backup").autohide(false),
                        cx,
                    );
                }
            }
            Command::Copy(public_key) => {
                self.copy_author(public_key, cx);
            }
            Command::Relays(public_key) => {
                self.open_relays(public_key, window, cx);
            }
            Command::Njump(public_key) => {
                self.open_njump(public_key, cx);
            }
        }
    }

    fn open_relays(&mut self, public_key: &PublicKey, window: &mut Window, cx: &mut Context<Self>) {
        let profile = self.profile(public_key, cx);

        window.open_modal(cx, move |this, _window, cx| {
            let relays = profile.messaging_relays();

            this.title("Messaging Relays")
                .show_close(true)
                .child(v_flex().gap_1().children({
                    let mut items = vec![];

                    for url in relays.iter() {
                        items.push(
                            h_flex()
                                .h_7()
                                .px_2()
                                .gap_2()
                                .bg(cx.theme().elevated_surface_background)
                                .rounded(cx.theme().radius)
                                .text_sm()
                                .child(div().size_1p5().rounded_full().bg(gpui::green()))
                                .child(SharedString::from(url.to_string())),
                        );
                    }

                    items
                }))
        });
    }

    fn open_njump(&mut self, public_key: &PublicKey, cx: &mut Context<Self>) {
        let content = format!("https://njump.me/{}", public_key.to_bech32().unwrap());
        cx.open_url(&content);
    }

    fn render_announcement(&self, ix: usize, cx: &Context<Self>) -> AnyElement {
        v_flex()
            .id(ix)
            .h_40()
            .w_full()
            .gap_3()
            .p_3()
            .items_center()
            .justify_center()
            .text_center()
            .text_xs()
            .text_color(cx.theme().text_placeholder)
            .line_height(relative(1.3))
            .child(
                svg()
                    .path("brand/coop.svg")
                    .size_12()
                    .text_color(cx.theme().ghost_element_active),
            )
            .child(SharedString::from(ANNOUNCEMENT))
            .into_any_element()
    }

    fn render_warning(&self, ix: usize, content: SharedString, cx: &Context<Self>) -> AnyElement {
        div()
            .id(ix)
            .w_full()
            .py_2()
            .px_3()
            .child(
                h_flex()
                    .w_full()
                    .gap_3()
                    .text_sm()
                    .child(
                        h_flex()
                            .flex_shrink_0()
                            .size_8()
                            .justify_center()
                            .rounded_full()
                            .bg(cx.theme().warning_background)
                            .text_color(cx.theme().warning_foreground)
                            .child(Icon::new(IconName::Warning).small()),
                    )
                    .child(
                        div()
                            .flex_1()
                            .w_full()
                            .flex_initial()
                            .overflow_hidden()
                            .child(content),
                    ),
            )
            .into_any_element()
    }

    fn render_message(
        &mut self,
        ix: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        if let Some(message) = self.messages.iter().nth(ix) {
            match message {
                Message::User(rendered) => {
                    let persons = PersonRegistry::global(cx);
                    let text = self
                        .rendered_texts_by_id
                        .entry(rendered.id)
                        .or_insert_with(|| {
                            RenderedText::new(&rendered.content, &rendered.mentions, &persons, cx)
                        })
                        .element(ix.into(), window, cx);

                    self.render_text_message(ix, rendered, text, cx)
                }
                Message::Warning(content, _timestamp) => {
                    self.render_warning(ix, SharedString::from(content), cx)
                }
                Message::System(_timestamp) => self.render_announcement(ix, cx),
            }
        } else {
            self.render_warning(ix, SharedString::from("Message not found"), cx)
        }
    }

    fn render_text_message(
        &self,
        ix: usize,
        message: &RenderedMessage,
        rendered_text: AnyElement,
        cx: &Context<Self>,
    ) -> AnyElement {
        let id = message.id;
        let author = self.profile(&message.author, cx);
        let pk = author.public_key();

        let replies = message.replies_to.as_slice();
        let has_replies = !replies.is_empty();

        let sent_pending = self.sent_pending(&id, cx);
        let has_reports = self.has_reports(&id, cx);

        // Hide avatar setting
        let hide_avatar = AppSettings::get_hide_avatar(cx);

        div()
            .id(ix)
            .group("")
            .relative()
            .w_full()
            .py_1()
            .px_3()
            .child(
                div()
                    .flex()
                    .gap_3()
                    .when(!hide_avatar, |this| {
                        this.child(
                            Avatar::new(author.avatar())
                                .flex_shrink_0()
                                .relative()
                                .dropdown_menu(move |this, _window, _cx| {
                                    this.menu("Public Key", Box::new(Command::Copy(pk)))
                                        .menu("View Relays", Box::new(Command::Relays(pk)))
                                        .separator()
                                        .menu("View on njump.me", Box::new(Command::Njump(pk)))
                                }),
                        )
                    })
                    .child(
                        v_flex()
                            .flex_1()
                            .w_full()
                            .flex_initial()
                            .overflow_hidden()
                            .child(
                                h_flex()
                                    .gap_2()
                                    .text_sm()
                                    .text_color(cx.theme().text_placeholder)
                                    .child(
                                        div()
                                            .font_semibold()
                                            .text_color(cx.theme().text)
                                            .child(author.name()),
                                    )
                                    .child(message.created_at.to_human_time())
                                    .when(sent_pending, |this| {
                                        this.child(SharedString::from("• Sending..."))
                                    })
                                    .when(has_reports, |this| {
                                        this.child(deferred(self.render_sent_reports(&id, cx)))
                                    }),
                            )
                            .when(has_replies, |this| {
                                this.children(self.render_message_replies(replies, cx))
                            })
                            .child(rendered_text),
                    ),
            )
            .child(
                div()
                    .group_hover("", |this| this.bg(cx.theme().element_active))
                    .absolute()
                    .left_0()
                    .top_0()
                    .w(px(2.))
                    .h_full()
                    .bg(cx.theme().border_transparent),
            )
            .child(self.render_actions(&id, &pk, cx))
            .on_mouse_down(
                MouseButton::Middle,
                cx.listener(move |this, _, _window, cx| {
                    this.copy_message(&id, cx);
                }),
            )
            .on_double_click(cx.listener(move |this, _, _window, cx| {
                this.reply_to(&id, cx);
            }))
            .hover(|this| this.bg(cx.theme().surface_background))
            .into_any_element()
    }

    fn render_message_replies(
        &self,
        replies: &[EventId],
        cx: &Context<Self>,
    ) -> impl IntoIterator<Item = impl IntoElement> {
        let mut items = Vec::with_capacity(replies.len());

        for (ix, id) in replies.iter().enumerate() {
            let Some(message) = self.message(id) else {
                continue;
            };
            let author = self.profile(&message.author, cx);

            items.push(
                div()
                    .id(ix)
                    .w_full()
                    .px_2()
                    .border_l_2()
                    .border_color(cx.theme().element_selected)
                    .text_sm()
                    .child(
                        div()
                            .text_color(cx.theme().text_accent)
                            .child(author.name()),
                    )
                    .child(
                        div()
                            .w_full()
                            .text_ellipsis()
                            .line_clamp(1)
                            .child(SharedString::from(&message.content)),
                    )
                    .hover(|this| this.bg(cx.theme().elevated_surface_background))
                    .on_click({
                        let id = *id;
                        cx.listener(move |this, _event, _window, _cx| {
                            this.scroll_to(id);
                        })
                    }),
            );
        }

        items
    }

    fn render_sent_reports(&self, id: &EventId, cx: &App) -> impl IntoElement {
        let reports = self.sent_reports(id, cx);

        let success = reports
            .as_ref()
            .is_some_and(|reports| reports.iter().any(|r| r.success()));

        let failed = reports
            .as_ref()
            .is_some_and(|reports| reports.iter().all(|r| r.failed()));

        let label = if success {
            SharedString::from("• Sent")
        } else if failed {
            SharedString::from("• Failed")
        } else {
            SharedString::from("• Sending...")
        };

        div()
            .id(SharedString::from(id.to_hex()))
            .child(label)
            .when(failed, |this| this.text_color(cx.theme().text_danger))
            .when_some(reports, |this, reports| {
                this.on_click(move |_e, window, cx| {
                    let reports = reports.clone();

                    window.open_modal(cx, move |this, _window, cx| {
                        this.title(SharedString::from("Sent Reports"))
                            .show_close(true)
                            .child(v_flex().gap_4().children({
                                let mut items = Vec::with_capacity(reports.len());

                                for report in reports.iter() {
                                    items.push(Self::render_report(report, cx))
                                }

                                items
                            }))
                    });
                })
            })
    }

    fn render_report(report: &SendReport, cx: &App) -> impl IntoElement {
        let persons = PersonRegistry::global(cx);
        let profile = persons.read(cx).get(&report.receiver, cx);
        let name = profile.name();
        let avatar = profile.avatar();

        v_flex()
            .gap_2()
            .w_full()
            .child(
                h_flex()
                    .gap_2()
                    .text_sm()
                    .child(SharedString::from("Sent to:"))
                    .child(
                        h_flex()
                            .gap_1()
                            .font_semibold()
                            .child(Avatar::new(avatar).small())
                            .child(name.clone()),
                    ),
            )
            .when_some(report.error.clone(), |this, error| {
                this.child(
                    h_flex()
                        .flex_wrap()
                        .justify_center()
                        .p_1()
                        .h_16()
                        .w_full()
                        .text_sm()
                        .rounded(cx.theme().radius)
                        .bg(cx.theme().warning_background)
                        .text_color(cx.theme().warning_foreground)
                        .child(div().flex_1().w_full().text_center().child(error)),
                )
            })
            .when_some(report.output.clone(), |this, output| {
                this.child(
                    v_flex()
                        .gap_2()
                        .w_full()
                        .children({
                            let mut items = Vec::with_capacity(output.failed.len());

                            for (url, msg) in output.failed.into_iter() {
                                items.push(
                                    v_flex()
                                        .gap_0p5()
                                        .p_1()
                                        .w_full()
                                        .rounded(cx.theme().radius)
                                        .bg(cx.theme().danger_background)
                                        .child(
                                            div()
                                                .text_xs()
                                                .font_semibold()
                                                .line_height(relative(1.25))
                                                .child(SharedString::from(url.to_string())),
                                        )
                                        .child(
                                            div()
                                                .text_xs()
                                                .text_color(cx.theme().danger_foreground)
                                                .line_height(relative(1.25))
                                                .child(SharedString::from(msg.to_string())),
                                        ),
                                )
                            }

                            items
                        })
                        .children({
                            let mut items = Vec::with_capacity(output.success.len());

                            for url in output.success.into_iter() {
                                items.push(
                                    v_flex()
                                        .gap_0p5()
                                        .p_1()
                                        .w_full()
                                        .rounded(cx.theme().radius)
                                        .bg(cx.theme().elevated_surface_background)
                                        .child(
                                            div()
                                                .text_xs()
                                                .font_semibold()
                                                .line_height(relative(1.25))
                                                .child(SharedString::from(url.to_string())),
                                        )
                                        .child(
                                            div()
                                                .text_xs()
                                                .line_height(relative(1.25))
                                                .child(SharedString::from("Successfully")),
                                        ),
                                )
                            }

                            items
                        }),
                )
            })
    }

    fn render_actions(
        &self,
        id: &EventId,
        public_key: &PublicKey,
        cx: &Context<Self>,
    ) -> impl IntoElement {
        h_flex()
            .p_0p5()
            .gap_1()
            .invisible()
            .absolute()
            .right_4()
            .top_neg_2()
            .when(cx.theme().shadow, |this| this.shadow_sm())
            .rounded(cx.theme().radius)
            .border_1()
            .border_color(cx.theme().border)
            .bg(cx.theme().background)
            .child(
                Button::new("reply")
                    .icon(IconName::Reply)
                    .tooltip("Reply")
                    .small()
                    .ghost()
                    .on_click({
                        let id = id.to_owned();
                        cx.listener(move |this, _event, _window, cx| {
                            this.reply_to(&id, cx);
                        })
                    }),
            )
            .child(
                Button::new("copy")
                    .icon(IconName::Copy)
                    .tooltip("Copy")
                    .small()
                    .ghost()
                    .on_click({
                        let id = id.to_owned();
                        cx.listener(move |this, _event, _window, cx| {
                            this.copy_message(&id, cx);
                        })
                    }),
            )
            .child(div().flex_shrink_0().h_4().w_px().bg(cx.theme().border))
            .child(
                Button::new("advance")
                    .icon(IconName::Ellipsis)
                    .small()
                    .ghost()
                    .dropdown_menu({
                        let public_key = *public_key;
                        let _id = *id;
                        move |this, _window, _cx| {
                            this.menu("Copy author", Box::new(Command::Copy(public_key)))
                            /*
                            .menu(
                                "Trace",
                                Box::new(Command::Trace(id)),
                            )
                            */
                        }
                    }),
            )
            .group_hover("", |this| this.visible())
    }

    fn render_attachment(&self, url: &Url, cx: &Context<Self>) -> impl IntoElement {
        div()
            .id(SharedString::from(url.to_string()))
            .relative()
            .w_16()
            .child(
                img(url.as_str())
                    .size_16()
                    .when(cx.theme().shadow, |this| this.shadow_lg())
                    .rounded(cx.theme().radius)
                    .object_fit(ObjectFit::ScaleDown),
            )
            .child(
                div()
                    .absolute()
                    .top_neg_2()
                    .right_neg_2()
                    .size_4()
                    .flex()
                    .items_center()
                    .justify_center()
                    .rounded_full()
                    .bg(red())
                    .child(Icon::new(IconName::Close).size_2().text_color(white())),
            )
            .on_click({
                let url = url.clone();
                cx.listener(move |this, _, window, cx| {
                    this.remove_attachment(&url, window, cx);
                })
            })
    }

    fn render_attachment_list(
        &self,
        _window: &Window,
        cx: &Context<Self>,
    ) -> impl IntoIterator<Item = impl IntoElement> {
        let mut items = vec![];

        for url in self.attachments.read(cx).iter() {
            items.push(self.render_attachment(url, cx));
        }

        items
    }

    fn render_reply(&self, id: &EventId, cx: &Context<Self>) -> impl IntoElement {
        if let Some(text) = self.message(id) {
            let persons = PersonRegistry::global(cx);
            let profile = persons.read(cx).get(&text.author, cx);

            div()
                .w_full()
                .pl_2()
                .border_l_2()
                .border_color(cx.theme().element_active)
                .child(
                    div()
                        .flex()
                        .items_center()
                        .justify_between()
                        .child(
                            div()
                                .flex()
                                .items_baseline()
                                .gap_1()
                                .text_xs()
                                .text_color(cx.theme().text_muted)
                                .child(SharedString::from("Replying to:"))
                                .child(
                                    div()
                                        .text_color(cx.theme().text_accent)
                                        .child(profile.name()),
                                ),
                        )
                        .child(
                            Button::new("remove-reply")
                                .icon(IconName::Close)
                                .xsmall()
                                .ghost()
                                .on_click({
                                    let id = text.id;
                                    cx.listener(move |this, _, _, cx| {
                                        this.remove_reply(&id, cx);
                                    })
                                }),
                        ),
                )
                .child(
                    div()
                        .w_full()
                        .text_sm()
                        .text_ellipsis()
                        .line_clamp(1)
                        .child(SharedString::from(&text.content)),
                )
        } else {
            div()
        }
    }

    fn render_reply_list(
        &self,
        _window: &Window,
        cx: &Context<Self>,
    ) -> impl IntoIterator<Item = impl IntoElement> {
        let mut items = vec![];

        for id in self.replies_to.read(cx).iter() {
            items.push(self.render_reply(id, cx));
        }

        items
    }

    fn render_config_menu(&self, _window: &mut Window, cx: &Context<Self>) -> impl IntoElement {
        let (backup, signer_kind) = self
            .room
            .read_with(cx, |this, _cx| {
                (this.config().backup(), this.config().signer_kind().clone())
            })
            .ok()
            .unwrap_or((true, SignerKind::default()));

        Button::new("encryption")
            .icon(IconName::Settings2)
            .tooltip("Configuration")
            .ghost()
            .large()
            .dropdown_menu(move |this, _window, _cx| {
                let auto = matches!(signer_kind, SignerKind::Auto);
                let encryption = matches!(signer_kind, SignerKind::Encryption);
                let user = matches!(signer_kind, SignerKind::User);

                this.label("Signer")
                    .menu_with_check_and_disabled(
                        "Auto",
                        auto,
                        Box::new(Command::ChangeSigner(SignerKind::Auto)),
                        auto,
                    )
                    .menu_with_check_and_disabled(
                        "Decoupled Encryption Key",
                        encryption,
                        Box::new(Command::ChangeSigner(SignerKind::Encryption)),
                        encryption,
                    )
                    .menu_with_check_and_disabled(
                        "User Identity",
                        user,
                        Box::new(Command::ChangeSigner(SignerKind::User)),
                        user,
                    )
                    .separator()
                    .label("Backup")
                    .menu_with_check("Backup messages", backup, Box::new(Command::ToggleBackup))
            })
    }

    fn render_emoji_menu(&self, _window: &Window, _cx: &Context<Self>) -> impl IntoElement {
        Button::new("emoji")
            .icon(IconName::Emoji)
            .ghost()
            .large()
            .dropdown_menu_with_anchor(gpui::Corner::BottomLeft, move |this, _window, _cx| {
                this.horizontal()
                    .menu("👍", Box::new(Command::Insert("👍")))
                    .menu("👎", Box::new(Command::Insert("👎")))
                    .menu("😄", Box::new(Command::Insert("😄")))
                    .menu("🎉", Box::new(Command::Insert("🎉")))
                    .menu("😕", Box::new(Command::Insert("😕")))
                    .menu("❤️", Box::new(Command::Insert("❤️")))
                    .menu("🚀", Box::new(Command::Insert("🚀")))
                    .menu("👀", Box::new(Command::Insert("👀")))
            })
    }
}

impl Panel for ChatPanel {
    fn panel_id(&self) -> SharedString {
        self.id.clone()
    }

    fn title(&self, cx: &App) -> AnyElement {
        self.room
            .read_with(cx, |this, cx| {
                let label = this.display_name(cx);
                let url = this.display_image(cx);

                h_flex()
                    .gap_1p5()
                    .child(Avatar::new(url).xsmall())
                    .child(label)
                    .into_any_element()
            })
            .unwrap_or(div().child("Unknown").into_any_element())
    }

    fn toolbar_buttons(&self, _window: &Window, _cx: &App) -> Vec<Button> {
        let subject_bar = self.subject_bar.clone();

        vec![
            Button::new("subject")
                .icon(IconName::Input)
                .tooltip("Change subject")
                .small()
                .ghost()
                .on_click(move |_ev, _window, cx| {
                    subject_bar.update(cx, |this, cx| {
                        *this = !*this;
                        cx.notify();
                    });
                }),
        ]
    }
}

impl EventEmitter<PanelEvent> for ChatPanel {}

impl Focusable for ChatPanel {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Render for ChatPanel {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        v_flex()
            .on_action(cx.listener(Self::on_command))
            .size_full()
            .when(*self.subject_bar.read(cx), |this| {
                this.child(
                    h_flex()
                        .h_12()
                        .w_full()
                        .px_2()
                        .gap_2()
                        .border_b_1()
                        .border_color(cx.theme().border)
                        .child(
                            TextInput::new(&self.subject_input)
                                .text_sm()
                                .small()
                                .bordered(false),
                        )
                        .child(
                            Button::new("change")
                                .icon(IconName::CheckCircle)
                                .label("Change")
                                .secondary()
                                .disabled(self.uploading)
                                .on_click(cx.listener(move |this, _ev, window, cx| {
                                    this.change_subject(window, cx);
                                })),
                        ),
                )
            })
            .child(
                v_flex()
                    .flex_1()
                    .relative()
                    .child(
                        list(
                            self.list_state.clone(),
                            cx.processor(move |this, ix, window, cx| {
                                this.render_message(ix, window, cx)
                            }),
                        )
                        .size_full(),
                    )
                    .child(Scrollbar::vertical(&self.list_state)),
            )
            .child(
                v_flex()
                    .flex_shrink_0()
                    .p_2()
                    .w_full()
                    .gap_1p5()
                    .children(self.render_attachment_list(window, cx))
                    .children(self.render_reply_list(window, cx))
                    .child(
                        h_flex()
                            .items_end()
                            .child(
                                Button::new("upload")
                                    .icon(IconName::Plus)
                                    .tooltip("Upload media")
                                    .loading(self.uploading)
                                    .disabled(self.uploading)
                                    .ghost()
                                    .large()
                                    .on_click(cx.listener(move |this, _ev, window, cx| {
                                        this.upload(window, cx);
                                    })),
                            )
                            .child(
                                TextInput::new(&self.input)
                                    .appearance(false)
                                    .text_sm()
                                    .flex_1(),
                            )
                            .child(
                                h_flex()
                                    .pl_1()
                                    .gap_1()
                                    .child(self.render_emoji_menu(window, cx))
                                    .child(self.render_config_menu(window, cx))
                                    .child(
                                        Button::new("send")
                                            .icon(IconName::PaperPlaneFill)
                                            .disabled(self.uploading)
                                            .ghost()
                                            .large()
                                            .on_click(cx.listener(move |this, _ev, window, cx| {
                                                this.send_text_message(window, cx);
                                            })),
                                    ),
                            ),
                    ),
            )
    }
}
