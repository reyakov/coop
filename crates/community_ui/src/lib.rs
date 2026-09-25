use std::collections::{BTreeMap, HashMap};
use std::fmt;

use anyhow::Result;
use community::{
    ChannelId, ChatMessage, Community, CommunityEvent, Epoch, Intent, LOAD_OLDER_PAGES,
    TIMELINE_PAGE, Timeline,
};
use gpui::prelude::FluentBuilder;
use gpui::{
    AnyElement, App, AppContext, Context, Entity, EventEmitter, FocusHandle, Focusable, FollowMode,
    IntoElement, ListAlignment, ListScrollEvent, ListState, ParentElement, Render, SharedString,
    Styled, Subscription, Task, WeakEntity, Window, div, list, px,
};
use nostr_sdk::prelude::EventId;
use smallvec::{SmallVec, smallvec};
use theme::ActiveTheme;
use ui::avatar::Avatar;
use ui::button::{Button, ButtonVariants};
use ui::dock::{Panel, PanelEvent};
use ui::input::{InputEvent, Textarea, TextareaState};
use ui::markdown::RenderedText;
use ui::message::WelcomeMessage;
use ui::notification::Notification;
use ui::scroll::Scrollbar;
use ui::{Disableable, IconName, Sizable, WindowExtension, h_flex, v_flex};

mod message;

/// How near the top row a scroll has to come before the panel splices older history in.
const LOAD_OLDER_THRESHOLD: usize = 20;
/// A repeat message within this window keeps its run, so it carries no avatar or name.
const RUN_WINDOW_MS: u64 = 300_000;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Notice {
    Stranded,
    Removed(Epoch),
    ChannelRemoved(Epoch),
    MissingKey(Epoch),
    Unreachable,
    Unreadable(usize),
}

impl fmt::Display for Notice {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Notice::Stranded => formatter.write_str(
                "This invite is stale, the community has rotated past the epoch it names",
            ),
            Notice::Removed(epoch) => write!(
                formatter,
                "You were removed from this community at epoch {}. Its history stays readable",
                epoch.0
            ),
            Notice::ChannelRemoved(epoch) => write!(
                formatter,
                "A rotation removed you from this channel at epoch {}",
                epoch.0
            ),
            Notice::MissingKey(epoch) => write!(
                formatter,
                "Messages here can't be read yet, this channel's key for epoch {} is missing",
                epoch.0
            ),
            Notice::Unreachable => formatter.write_str("Couldn't reach the community's relays"),
            Notice::Unreadable(1) => {
                formatter.write_str("1 message here can't be read yet, no key we hold opens it")
            }
            Notice::Unreadable(count) => write!(
                formatter,
                "{count} messages here can't be read yet, no key we hold opens them"
            ),
        }
    }
}

impl Notice {
    fn writable(self) -> bool {
        matches!(self, Notice::Unreachable | Notice::Unreadable(_))
    }
}

pub fn init(
    community: Entity<Community>,
    window: &mut Window,
    cx: &mut App,
) -> Entity<CommunityPanel> {
    cx.new(|cx| CommunityPanel::new(community, window, cx))
}

/// Community Panel
pub struct CommunityPanel {
    id: SharedString,
    focus_handle: FocusHandle,
    /// Community
    community: WeakEntity<Community>,
    /// The selected channel
    channel: Option<ChannelId>,
    /// The selected channel's timeline (oldest first)
    rows: Vec<ChatMessage>,
    /// Rendered markdown content, keyed by message id and dropped when a row changes
    rendered_texts_by_id: BTreeMap<EventId, RenderedText>,
    /// Whether the store holds rows older than `rows`
    has_more: bool,
    /// A round or a page read is in flight
    loading: bool,
    /// Message list state
    list_state: ListState,
    /// Message input state
    input: Entity<TextareaState>,
    /// Spawned reads and publishes, cancelled when the panel closes
    tasks: SmallVec<[Task<Result<()>>; 4]>,
    /// Event subscriptions
    _subscriptions: SmallVec<[Subscription; 2]>,
}

impl CommunityPanel {
    pub fn new(community: Entity<Community>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let (id, name, channel) = {
            let community = community.read(cx);

            (
                SharedString::from(format!("community-{}", community.id().to_hex())),
                community.name(),
                community.active_channel(),
            )
        };

        let input = cx.new(|cx| {
            TextareaState::new(window, cx)
                .placeholder(format!("Message {name}"))
                .auto_grow(1, 20)
                .clean_on_escape()
        });

        let mut subscriptions = smallvec![];

        subscriptions.push(
            cx.subscribe_in(&input, window, |this, _input, event, window, cx| {
                if let InputEvent::PressEnter { .. } = event {
                    this.send(window, cx);
                }
            }),
        );

        subscriptions.push(cx.subscribe_in(
            &community,
            window,
            |_this, _community, event, window, cx| {
                match event {
                    CommunityEvent::Channel(..) => {
                        cx.defer_in(window, |this, window, cx| {
                            this.load(window, cx);
                        });
                    }
                    CommunityEvent::Error(error) => {
                        window.push_notification(Notification::error(error.clone()), cx);
                    }
                    _ => {
                        cx.defer_in(window, |this, window, cx| {
                            this.reload(window, cx);
                        });
                    }
                };
            },
        ));

        cx.defer_in(window, |this, window, cx| {
            this.list_state.set_follow_mode(FollowMode::Tail);
            this.list_state.set_scroll_handler(cx.listener(
                |this, event: &ListScrollEvent, window, cx| {
                    if event.visible_range.start <= LOAD_OLDER_THRESHOLD {
                        this.load_older(window, cx);
                    }
                },
            ));
            this.load(window, cx);
        });

        Self {
            id,
            focus_handle: cx.focus_handle(),
            community: community.downgrade(),
            channel,
            rows: Vec::new(),
            rendered_texts_by_id: BTreeMap::new(),
            has_more: false,
            loading: false,
            list_state: ListState::new(0, ListAlignment::Bottom, px(1024.)),
            input,
            tasks: smallvec![],
            _subscriptions: subscriptions,
        }
    }

    /// The channel to show, following the community's selection.
    fn resolve_channel(&mut self, cx: &App) -> Option<ChannelId> {
        let channel = self
            .community
            .read_with(cx, |community, _cx| community.active_channel())
            .ok()
            .flatten();

        if channel != self.channel {
            self.channel = channel;
            self.rows.clear();
            self.rendered_texts_by_id.clear();
            self.has_more = false;
            self.loading = false;
            self.list_state.reset(2);
        }

        channel
    }

    /// The list's item count: the welcome row, the load-older row, then every message row.
    fn item_count(&self) -> usize {
        self.rows.len() + 2
    }

    /// A timeline read, `before_ms` exclusive, or `None` for the newest rows.
    fn read(
        &self,
        channel: ChannelId,
        before_ms: Option<u64>,
        cx: &App,
    ) -> Option<Task<Result<Timeline>>> {
        self.community
            .read_with(cx, |community, cx| {
                community.timeline(&channel, before_ms, TIMELINE_PAGE, cx)
            })
            .ok()
    }

    /// A channel round, or `None` once the community is gone.
    fn sync(
        &self,
        channel: ChannelId,
        intent: Intent,
        cx: &mut App,
    ) -> Option<Task<Result<community::Progress>>> {
        self.community
            .update(cx, |community, cx| {
                community.sync_channel(&channel, intent, cx)
            })
            .ok()
    }

    /// Paint the selected channel's cache, then catch it up from the relays.
    fn load(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(channel) = self.resolve_channel(cx) else {
            return;
        };

        self.reload(window, cx);

        // Opening a channel twice in a breath asks the relays once.
        if self.due(channel, cx) {
            self.round(channel, Intent::CatchUp, window, cx);
        }
    }

    /// Whether the community would actually round `channel`, or just serve it.
    fn due(&self, channel: ChannelId, cx: &App) -> bool {
        self.community
            .read_with(cx, |community, _cx| community.due(&channel))
            .unwrap_or(true)
    }

    /// Why the room is not showing messages, when it is not simply empty.
    fn notice(&self, cx: &App) -> Option<Notice> {
        let channel = self.channel?;

        self.community
            .read_with(cx, |community, _cx| {
                if community.stranded() {
                    return Some(Notice::Stranded);
                }

                if let Some(epoch) = community.removed_at() {
                    return Some(Notice::Removed(epoch));
                }

                if let Some(epoch) = community.channel_removed_at(&channel) {
                    return Some(Notice::ChannelRemoved(epoch));
                }

                if let Some(epoch) = community.missing_key(&channel) {
                    return Some(Notice::MissingKey(epoch));
                }

                if community
                    .progress(&channel)
                    .is_some_and(|progress| progress.failed && progress.errors > 0)
                {
                    return Some(Notice::Unreachable);
                }

                let unreadable = community.unreadable(&channel);

                (unreadable > 0).then_some(Notice::Unreadable(unreadable))
            })
            .ok()
            .flatten()
    }

    /// Run a round for `channel`, its completion re-reads the timeline.
    fn round(
        &mut self,
        channel: ChannelId,
        intent: Intent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(round) = self.sync(channel, intent, cx) else {
            return;
        };

        self.loading = true;
        cx.notify();

        let task = cx.spawn_in::<_, Result<()>>(window, async move |this, cx| {
            let result = round.await;

            this.update_in(cx, |this, window, cx| {
                this.loading = false;

                if let Err(error) = result {
                    window.push_notification(
                        Notification::error(error.to_string()).autohide(false),
                        cx,
                    );
                }
            })?;

            Ok(())
        });

        self.tasks.push(task);
    }

    /// Run the newest round again after a relay failure.
    fn retry(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(channel) = self.channel else {
            return;
        };

        self.round(channel, Intent::CatchUp, window, cx);
    }

    /// Read the newest page and fold it into what is on screen.
    fn reload(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(channel) = self.resolve_channel(cx) else {
            return;
        };

        let Some(timeline) = self.read(channel, None, cx) else {
            return;
        };

        let task = cx.spawn_in::<_, Result<()>>(window, async move |this, cx| {
            match timeline.await {
                Ok(timeline) => this.update(cx, |this, cx| this.apply(channel, timeline, cx))?,
                Err(error) => {
                    this.update_in(cx, |_this, window, cx| {
                        window.push_notification(
                            Notification::error(error.to_string()).autohide(false),
                            cx,
                        );
                    })?;
                }
            }

            Ok(())
        });

        self.tasks.push(task);
    }

    /// Splice the page of history above the oldest row on screen.
    fn load_older(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.loading || !self.has_more {
            return;
        }

        let Some(channel) = self.channel else {
            return;
        };

        let Some(before_ms) = self
            .rows
            .first()
            .map(|message| message.at_ms.saturating_sub(1))
        else {
            return;
        };

        let Some(page) = self.read(channel, Some(before_ms), cx) else {
            return;
        };

        self.loading = true;

        let task = cx.spawn_in::<_, Result<()>>(window, async move |this, cx| {
            let timeline = match page.await {
                Ok(timeline) => timeline,
                Err(error) => {
                    this.update_in(cx, |this, window, cx| {
                        this.loading = false;
                        window.push_notification(
                            Notification::error(error.to_string()).autohide(false),
                            cx,
                        );
                    })?;
                    return Ok(());
                }
            };

            let swept = this.update(cx, |this, cx| {
                this.prepend(channel, timeline, cx);
                !this.has_more
            })?;

            if !swept {
                this.update(cx, |this, _cx| this.loading = false)?;
                return Ok(());
            }

            let round = this.update(cx, |this, cx| {
                this.sync(
                    channel,
                    Intent::Older {
                        pages: LOAD_OLDER_PAGES,
                    },
                    cx,
                )
            })?;

            let Some(round) = round else {
                this.update(cx, |this, _cx| this.loading = false)?;
                return Ok(());
            };

            match round.await {
                Ok(_) => {
                    let page =
                        this.update(cx, |this, cx| this.read(channel, Some(before_ms), cx))?;

                    if let Some(page) = page {
                        match page.await {
                            Ok(timeline) => {
                                this.update(cx, |this, cx| this.prepend(channel, timeline, cx))?
                            }
                            Err(error) => this.update_in(cx, |_this, window, cx| {
                                window.push_notification(
                                    Notification::error(error.to_string()).autohide(false),
                                    cx,
                                );
                            })?,
                        }
                    }

                    this.update(cx, |this, _cx| this.loading = false)?;
                }
                Err(error) => {
                    this.update_in(cx, |this, window, cx| {
                        this.loading = false;
                        window.push_notification(
                            Notification::error(error.to_string()).autohide(false),
                            cx,
                        );
                    })?;
                }
            }

            Ok(())
        });

        self.tasks.push(task);
    }

    /// Fold a freshly read window into the rows on screen.
    fn apply(&mut self, channel: ChannelId, timeline: Timeline, cx: &mut Context<Self>) {
        if self.channel != Some(channel) {
            return;
        }

        let Timeline { messages, has_more } = timeline;

        let connected = self
            .rows
            .last()
            .is_some_and(|last| messages.iter().any(|message| message.id == last.id));

        if !connected {
            self.rows = messages;
            self.rendered_texts_by_id.clear();
            self.has_more = has_more;
            self.list_state.reset(self.item_count());
            cx.notify();
            return;
        }

        self.merge(messages);
        cx.notify();
    }

    /// Splice older rows in above what is on screen.
    fn prepend(&mut self, channel: ChannelId, timeline: Timeline, cx: &mut Context<Self>) {
        if self.channel != Some(channel) {
            return;
        }

        self.has_more = timeline.has_more;
        self.merge(timeline.messages);
        cx.notify();
    }

    /// Fold read rows into what is on screen, keeping the list in time order.
    fn merge(&mut self, messages: Vec<ChatMessage>) {
        let mut shown: HashMap<EventId, usize> = self
            .rows
            .iter()
            .enumerate()
            .map(|(ix, message)| (message.id, ix))
            .collect();

        let mut fresh = Vec::new();

        for message in messages {
            match shown.get(&message.id).copied() {
                Some(ix) => {
                    // Drop the cached render when the text changes, so edits re-parse.
                    if self.rows[ix].content != message.content {
                        self.rendered_texts_by_id.remove(&message.id);
                    }
                    self.rows[ix] = message;
                }
                None => fresh.push(message),
            }
        }

        for message in fresh {
            if shown.contains_key(&message.id) {
                continue;
            }

            let at = self
                .rows
                .partition_point(|row| (row.at_ms, row.id) <= (message.at_ms, message.id));

            shown.insert(message.id, at);
            self.rows.insert(at, message);
            // The welcome and load-older rows sit above the messages.
            self.list_state.splice(at + 2..at + 2, 1);
        }
    }

    fn send(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let content = self.input.read(cx).value().trim().to_owned();

        if content.is_empty() {
            window.push_notification("Cannot send an empty message", cx);
            return;
        }

        let Some(channel) = self.resolve_channel(cx) else {
            return;
        };

        if let Some(notice) = self.notice(cx).filter(|notice| !notice.writable()) {
            window.push_notification(Notification::error(notice.to_string()).autohide(false), cx);
            return;
        }

        let Ok(send) = self.community.read_with(cx, |community, cx| {
            community.send(&channel, &content, None, cx)
        }) else {
            return;
        };

        let Some(send) = send else {
            window.push_notification(Notification::error("Failed to send the message"), cx);
            return;
        };

        self.input.update(cx, |input, cx| {
            input.set_value("", window, cx);
        });

        let task = cx.spawn_in::<_, Result<()>>(window, async move |this, cx| {
            match send.await {
                Ok(_) => {
                    this.update_in(cx, |this, window, cx| this.reload(window, cx))?;
                }
                Err(error) => {
                    this.update_in(cx, |_this, window, cx| {
                        window.push_notification(
                            Notification::error(error.to_string()).autohide(false),
                            cx,
                        );
                    })?;
                }
            }

            Ok(())
        });

        self.tasks.push(task);
    }

    /// The honest reason this room has nothing to show, with a way out of it.
    fn render_notice(&self, notice: Notice, cx: &mut Context<Self>) -> AnyElement {
        h_flex()
            .w_full()
            .justify_center()
            .items_center()
            .gap_2()
            .px_3()
            .py_2()
            .text_sm()
            .text_color(cx.theme().text_placeholder)
            .child(notice.to_string())
            .when(notice == Notice::Unreachable, |this| {
                this.child(
                    Button::new("retry-round")
                        .label("Retry")
                        .ghost()
                        .small()
                        .loading(self.loading)
                        .on_click(cx.listener(|this, _event, window, cx| this.retry(window, cx))),
                )
            })
            .into_any_element()
    }

    /// The row at index 0: the welcome message for the channel.
    fn render_welcome(&self, cx: &Context<Self>) -> AnyElement {
        let (name, avatar) = self
            .community
            .read_with(cx, |community, _cx| {
                let seed = community.id().to_hex();
                let avatar = match community.icon() {
                    Some(path) => Avatar::from_source(path).seed(seed).large(),
                    None => Avatar::new(None).seed(seed).large(),
                };

                (community.name(), avatar)
            })
            .unwrap_or_else(|_| (SharedString::from("this community"), Avatar::new(None)));

        WelcomeMessage::new("welcome")
            .icon(avatar)
            .title(format!("Welcome to {}", name))
            .message(format!("This is the start of the {} channel.", name))
            .into_any_element()
    }

    /// The row at index 1: the affordance that pages older history in.
    fn render_older(&self, cx: &mut Context<Self>) -> AnyElement {
        if !self.has_more {
            return div().into_any_element();
        }

        h_flex()
            .w_full()
            .justify_center()
            .py_2()
            .child(
                Button::new("load-older")
                    .label(if self.loading {
                        "Loading earlier messages…"
                    } else {
                        "Load earlier messages"
                    })
                    .ghost()
                    .small()
                    .loading(self.loading)
                    .on_click(cx.listener(|this, _event, window, cx| this.load_older(window, cx))),
            )
            .into_any_element()
    }

    /// Whether the row at `index` opens a run from one author.
    fn opens_run(&self, index: usize) -> bool {
        let Some(current) = self.rows.get(index) else {
            return true;
        };

        let Some(previous) = index.checked_sub(1).and_then(|index| self.rows.get(index)) else {
            return true;
        };

        current.author != previous.author
            || current.at_ms.saturating_sub(previous.at_ms) > RUN_WINDOW_MS
    }

    fn render_message(
        &mut self,
        ix: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        if ix == 0 {
            return self.render_welcome(cx);
        }

        if ix == 1 {
            return self.render_older(cx);
        }

        // The welcome and load-older rows sit above the messages.
        let Some(message) = self.rows.get(ix - 2) else {
            return div().into_any_element();
        };

        let show_author = self.opens_run(ix - 2);

        let content = if message.deleted {
            message::deleted(cx)
        } else {
            self.rendered_texts_by_id
                .entry(message.id)
                .or_insert_with(|| RenderedText::new(&message.content, &[], true))
                .element(ix.into(), window, cx)
        };

        message::render(ix, message, content, show_author, cx)
    }

    fn render_composer(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let writable = self.notice(cx).is_none_or(|notice| notice.writable());

        h_flex()
            .flex_shrink_0()
            .w_full()
            .p_2()
            .gap_1()
            .items_end()
            .child(Textarea::new(&self.input).appearance(false).flex_1())
            .child(
                Button::new("send")
                    .icon(IconName::PaperPlaneFill)
                    .tooltip("Send")
                    .ghost()
                    .large()
                    .disabled(!writable)
                    .on_click(cx.listener(|this, _event, window, cx| {
                        this.send(window, cx);
                    })),
            )
    }
}

impl Panel for CommunityPanel {
    fn panel_id(&self) -> SharedString {
        self.id.clone()
    }

    fn title(&self, cx: &App) -> AnyElement {
        self.community
            .read_with(cx, |community, _cx| {
                let seed = community.id().to_hex();
                let avatar = match community.icon() {
                    Some(path) => Avatar::from_source(path).seed(seed).xsmall(),
                    None => Avatar::new(None).seed(seed).xsmall(),
                };

                h_flex()
                    .gap_1()
                    .text_xs()
                    .child(avatar)
                    .child(community.name())
                    .into_any_element()
            })
            .unwrap_or_else(|_| div().text_xs().child("Unknown").into_any_element())
    }

    fn toolbar_buttons(&self, _window: &Window, _cx: &App) -> Vec<Button> {
        vec![]
    }
}

impl EventEmitter<PanelEvent> for CommunityPanel {}

impl Focusable for CommunityPanel {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Render for CommunityPanel {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        v_flex()
            .size_full()
            .min_w_0()
            .child(
                v_flex()
                    .flex_1()
                    .min_h_0()
                    .relative()
                    .map(|this| {
                        let notice = self.notice(cx);

                        if self.rows.is_empty() {
                            this.child(
                                v_flex().size_full().justify_center().child(match notice {
                                    Some(notice) => self.render_notice(notice, cx),
                                    None => h_flex()
                                        .size_full()
                                        .justify_center()
                                        .text_sm()
                                        .text_color(cx.theme().text_placeholder)
                                        .child("No messages yet")
                                        .into_any_element(),
                                }),
                            )
                        } else {
                            this.when_some(notice, |this, notice| {
                                this.child(self.render_notice(notice, cx))
                            })
                            .child(
                                list(
                                    self.list_state.clone(),
                                    cx.processor(move |this, ix, window, cx| {
                                        this.render_message(ix, window, cx)
                                    }),
                                )
                                .size_full(),
                            )
                        }
                    })
                    .child(Scrollbar::vertical(&self.list_state)),
            )
            .child(self.render_composer(cx))
    }
}
