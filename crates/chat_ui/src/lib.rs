use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::sync::Arc;

pub use actions::*;
use anyhow::{Context as AnyhowContext, Error};
use chat::{Message, RenderedMessage, Room, RoomEvent, SendReport};
use common::RenderedTimestamp;
use gpui::prelude::FluentBuilder;
use gpui::{
    deferred, div, img, list, px, red, relative, rems, svg, white, AnyElement, App, AppContext,
    ClipboardItem, Context, Entity, EventEmitter, FocusHandle, Focusable, InteractiveElement,
    IntoElement, ListAlignment, ListOffset, ListState, MouseButton, ObjectFit, ParentElement,
    PathPromptOptions, Render, SharedString, StatefulInteractiveElement, Styled, StyledImage,
    Subscription, Task, WeakEntity, Window,
};
use itertools::Itertools;
use nostr_sdk::prelude::*;
use person::{Person, PersonRegistry};
use settings::{AppSettings, SignerKind};
use smallvec::{smallvec, SmallVec};
use smol::lock::RwLock;
use state::{upload, NostrRegistry};
use theme::ActiveTheme;
use ui::avatar::Avatar;
use ui::button::{Button, ButtonVariants};
use ui::dock_area::panel::{Panel, PanelEvent};
use ui::indicator::Indicator;
use ui::input::{InputEvent, InputState, TextInput};
use ui::menu::{ContextMenuExt, DropdownMenu};
use ui::notification::Notification;
use ui::scroll::Scrollbar;
use ui::{
    h_flex, v_flex, Disableable, Icon, IconName, InteractiveElementExt, Sizable, StyledExt,
    WindowExtension,
};

use crate::text::RenderedText;

mod actions;
mod text;

const NO_INBOX: &str = "has not set up messaging relays. \
    They will not receive your messages.";
const NO_ANNOUNCEMENT: &str = "has not set up an encryption key. \
    You cannot send messages encrypted with an encryption key to them yet.";

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

    /// Input state
    input: Entity<InputState>,

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
    subscriptions: SmallVec<[Subscription; 2]>,
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

        // Define subscriptions
        let subscriptions =
            smallvec![
                cx.subscribe_in(&input, window, move |this, _input, event, window, cx| {
                    if let InputEvent::PressEnter { .. } = event {
                        this.send_text_message(window, cx);
                    };
                })
            ];

        // Define all functions that will run after the current cycle
        cx.defer_in(window, |this, window, cx| {
            this.connect(window, cx);
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

    /// Handle nostr notifications
    fn handle_notifications(&mut self, cx: &mut Context<Self>) {
        let nostr = NostrRegistry::global(cx);
        let client = nostr.read(cx).client();
        let sent_ids = self.sent_ids.clone();

        let (tx, rx) = flume::bounded::<(EventId, RelayUrl)>(256);

        self.tasks.push(cx.background_spawn(async move {
            let mut notifications = client.notifications();

            while let Some(notification) = notifications.next().await {
                if let ClientNotification::Message {
                    message: RelayMessage::Ok { event_id, .. },
                    relay_url,
                } = notification
                {
                    let sent_ids = sent_ids.read().await;

                    if sent_ids.contains(&event_id) {
                        tx.send_async((event_id, relay_url)).await.ok();
                    }
                }
            }

            Ok(())
        }));

        self.tasks.push(cx.spawn(async move |this, cx| {
            while let Ok((event_id, relay_url)) = rx.recv_async().await {
                this.update(cx, |this, cx| {
                    this.reports_by_id.update(cx, |this, cx| {
                        for reports in this.values_mut() {
                            for report in reports.iter_mut() {
                                if let Some(output) = report.output.as_mut() {
                                    if output.id() == &event_id {
                                        output.success.insert(relay_url.clone());
                                        cx.notify();
                                    }
                                }
                            }
                        }
                    });
                })?;
            }

            Ok(())
        }));
    }

    fn subscribe_room_events(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(room) = self.room.upgrade() else {
            return;
        };

        self.subscriptions.push(
            // Subscribe to room events
            cx.subscribe_in(&room, window, move |this, _room, event, window, cx| {
                match event {
                    RoomEvent::Incoming(message) => {
                        this.insert_message(message, false, cx);
                    }
                    RoomEvent::Reload => {
                        this.get_messages(window, cx);
                    }
                };
            }),
        );
    }

    /// Get all necessary data for each member
    fn connect(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Ok(tasks) = self.room.read_with(cx, |this, cx| this.connect(cx)) else {
            return;
        };

        self.tasks.push(cx.spawn_in(window, async move |this, cx| {
            for (member, task) in tasks.into_iter() {
                match task.await {
                    Ok((has_inbox, has_announcement)) => {
                        this.update(cx, |this, cx| {
                            let persons = PersonRegistry::global(cx);
                            let profile = persons.read(cx).get(&member, cx);

                            if !has_inbox {
                                let content = format!("{} {}", profile.name(), NO_INBOX);
                                let message = Message::warning(content);

                                this.insert_message(message, true, cx);
                            }

                            if !has_announcement {
                                let content = format!("{} {}", profile.name(), NO_ANNOUNCEMENT);
                                let message = Message::warning(content);

                                this.insert_message(message, true, cx);
                            }
                        })?;
                    }
                    Err(e) => {
                        this.update(cx, |this, cx| {
                            this.insert_message(Message::warning(e.to_string()), true, cx);
                        })?;
                    }
                };
            }
            Ok(())
        }));
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

        let Some(room) = self.room.upgrade() else {
            return;
        };

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
            .is_some_and(|reports| reports.iter().any(|r| r.pending()))
    }

    /// Check if a message was sent successfully by its ID
    fn sent_success(&self, id: &EventId, cx: &App) -> bool {
        self.reports_by_id
            .read(cx)
            .get(id)
            .is_some_and(|reports| reports.iter().any(|r| r.success()))
    }

    /// Check if a message failed to send by its ID
    fn sent_failed(&self, id: &EventId, cx: &App) -> Option<bool> {
        self.reports_by_id
            .read(cx)
            .get(id)
            .map(|reports| reports.iter().all(|r| !r.success()))
    }

    /// Get all sent reports for a message by its ID
    fn sent_reports(&self, id: &EventId, cx: &App) -> Option<Vec<SendReport>> {
        self.reports_by_id.read(cx).get(id).cloned()
    }

    /// Get a message by its ID
    fn message(&self, id: &EventId) -> Option<&RenderedMessage> {
        self.messages.iter().find_map(|msg| {
            if let Message::User(rendered) = msg {
                if &rendered.id == id {
                    return Some(rendered);
                }
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

    fn copy_message(&self, id: &EventId, cx: &Context<Self>) {
        if let Some(message) = self.message(id) {
            cx.write_to_clipboard(ClipboardItem::new_string(message.content.to_string()));
        }
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
                        window.push_notification(Notification::error(e.to_string()), cx);
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

    fn profile(&self, public_key: &PublicKey, cx: &Context<Self>) -> Person {
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
                        this.set_subject(*subject, cx);
                    })
                    .is_err()
                {
                    window.push_notification(Notification::error("Failed to change subject"), cx);
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
                    window.push_notification(Notification::error("Failed to change signer"), cx);
                }
            }
        }
    }

    fn render_announcement(&self, ix: usize, cx: &Context<Self>) -> AnyElement {
        const MSG: &str =
            "This conversation is private. Only members can see each other's messages.";

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
            .child(SharedString::from(MSG))
            .into_any_element()
    }

    fn render_warning(&self, ix: usize, content: SharedString, cx: &Context<Self>) -> AnyElement {
        div()
            .id(ix)
            .relative()
            .w_full()
            .py_2()
            .px_3()
            .child(
                h_flex()
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
                    .child(content),
            )
            .child(
                div()
                    .absolute()
                    .left_0()
                    .top_0()
                    .w(px(2.))
                    .h_full()
                    .bg(cx.theme().warning_active),
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
                    let text = self
                        .rendered_texts_by_id
                        .entry(rendered.id)
                        .or_insert_with(|| RenderedText::new(&rendered.content, cx))
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
        let public_key = author.public_key();

        let replies = message.replies_to.as_slice();
        let has_replies = !replies.is_empty();

        // Check if message is sent failed
        let sent_pending = self.sent_pending(&id, cx);

        // Check if message is sent successfully
        let sent_success = self.sent_success(&id, cx);

        // Check if message is sent failed
        let sent_failed = self.sent_failed(&id, cx);

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
                            div()
                                .id(SharedString::from(format!("{ix}-avatar")))
                                .child(Avatar::new(author.avatar()).size(rems(2.)))
                                .context_menu(move |this, _window, _cx| {
                                    let view = Box::new(OpenPublicKey(public_key));
                                    let copy = Box::new(CopyPublicKey(public_key));

                                    this.menu("View Profile", view)
                                        .menu("Copy Public Key", copy)
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
                                        this.child(deferred(Indicator::new().small()))
                                    })
                                    .when(sent_success, |this| {
                                        this.child(deferred(self.render_sent_indicator(&id, cx)))
                                    }),
                            )
                            .when(has_replies, |this| {
                                this.children(self.render_message_replies(replies, cx))
                            })
                            .child(rendered_text)
                            .when_some(sent_failed, |this, failed| {
                                this.when(failed, |this| {
                                    this.child(deferred(self.render_message_reports(&id, cx)))
                                })
                            }),
                    ),
            )
            .child(self.render_border(cx))
            .child(self.render_actions(&id, cx))
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

    fn render_sent_indicator(&self, id: &EventId, cx: &Context<Self>) -> impl IntoElement {
        div()
            .id(SharedString::from(id.to_hex()))
            .child(SharedString::from("• Sent"))
            .when_some(self.sent_reports(id, cx), |this, reports| {
                this.on_click(move |_e, window, cx| {
                    let reports = reports.clone();

                    window.open_modal(cx, move |this, _window, cx| {
                        this.show_close(true)
                            .title(SharedString::from("Sent Reports"))
                            .child(v_flex().pb_4().gap_4().children({
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

    fn render_message_reports(&self, id: &EventId, cx: &Context<Self>) -> impl IntoElement {
        h_flex()
            .id(SharedString::from(id.to_hex()))
            .gap_0p5()
            .text_color(cx.theme().danger_foreground)
            .text_xs()
            .italic()
            .child(Icon::new(IconName::Info).xsmall())
            .child(SharedString::from(
                "Failed to send message. Click to see details.",
            ))
            .when_some(self.sent_reports(id, cx), |this, reports| {
                this.on_click(move |_e, window, cx| {
                    let reports = reports.clone();

                    window.open_modal(cx, move |this, _window, cx| {
                        this.show_close(true)
                            .title(SharedString::from("Sent Reports"))
                            .child(v_flex().gap_4().pb_4().w_full().children({
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
                            .child(Avatar::new(avatar).size(rems(1.25)))
                            .child(name.clone()),
                    ),
            )
            .when_some(report.error.clone(), |this, error| {
                this.child(
                    h_flex()
                        .flex_wrap()
                        .justify_center()
                        .p_2()
                        .h_20()
                        .w_full()
                        .text_sm()
                        .rounded(cx.theme().radius)
                        .bg(cx.theme().danger_background)
                        .text_color(cx.theme().danger_foreground)
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
                                        .py_1()
                                        .px_2()
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
                                                .text_sm()
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
                                        .py_1()
                                        .px_2()
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
                                                .text_sm()
                                                .text_color(cx.theme().secondary_foreground)
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

    fn render_border(&self, cx: &Context<Self>) -> impl IntoElement {
        div()
            .group_hover("", |this| this.bg(cx.theme().element_active))
            .absolute()
            .left_0()
            .top_0()
            .w(px(2.))
            .h_full()
            .bg(cx.theme().border_transparent)
    }

    fn render_actions(&self, id: &EventId, cx: &Context<Self>) -> impl IntoElement {
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
                Button::new("seen-on")
                    .icon(IconName::Ellipsis)
                    .small()
                    .ghost()
                    .dropdown_menu({
                        let id = id.to_owned();
                        move |this, _window, _cx| this.menu("Seen on", Box::new(SeenOn(id)))
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

    fn render_encryption_menu(&self, _window: &mut Window, cx: &Context<Self>) -> impl IntoElement {
        let signer_kind = self
            .room
            .read_with(cx, |this, _cx| this.config().signer_kind().clone())
            .ok()
            .unwrap_or_default();

        Button::new("encryption")
            .icon(IconName::UserKey)
            .ghost()
            .large()
            .dropdown_menu(move |this, _window, _cx| {
                let auto = matches!(signer_kind, SignerKind::Auto);
                let encryption = matches!(signer_kind, SignerKind::Encryption);
                let user = matches!(signer_kind, SignerKind::User);

                this.menu_with_check_and_disabled(
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
                    .child(Avatar::new(url).size(rems(1.25)))
                    .child(label)
                    .into_any_element()
            })
            .unwrap_or(div().child("Unknown").into_any_element())
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
            .child(
                div()
                    .flex_1()
                    .size_full()
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
                                    .flex_1()
                                    .text_sm(),
                            )
                            .child(
                                h_flex()
                                    .pl_1()
                                    .gap_1()
                                    .child(self.render_emoji_menu(window, cx))
                                    .child(self.render_encryption_menu(window, cx))
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
