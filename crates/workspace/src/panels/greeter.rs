use std::time::Duration;

use anyhow::Error;
use chat::{ChatRegistry, Message, Room, RoomKind};
use common::TimestampExt;
use gpui::prelude::FluentBuilder;
use gpui::{
    AnyElement, App, AppContext, Context, Entity, EventEmitter, FocusHandle, Focusable,
    InteractiveElement, IntoElement, ParentElement, Render, SharedString,
    StatefulInteractiveElement, Styled, Subscription, Task, WeakEntity, Window, div, px, svg,
};
use nostr_sdk::prelude::*;
use smallvec::{SmallVec, smallvec};
use state::{NostrRegistry, StateEvent};
use theme::{ActiveTheme, TextStyle, Typeset as _, button_radius};
use ui::avatar::Avatar;
use ui::button::{Button, ButtonVariants};
use ui::dock::{DockArea, DockPlacement, Panel, PanelEvent, PanelHandle, add_panel_to};
use ui::group_box::{GroupBox, GroupBoxVariants};
use ui::nav::Nav;
use ui::scroll::ScrollableElement;
use ui::{Icon, IconName, Sizable, StyledExt, h_flex, v_flex};

use super::browse;
use crate::sidebar::open_room;

/// How many requests and follow ups to show per section.
const SECTION_SIZE: usize = 3;
/// Delay to coalesce bursts of chat registry updates into one reload.
const REFRESH_DELAY: Duration = Duration::from_millis(300);
/// Public posts browser on the web.
const JUMBLE_URL: &str = "https://jumble.social";

/// Rooms with their kind, paired with the task loading their messages
type PendingMessages = Vec<(
    Entity<Room>,
    RoomKind,
    Task<Result<Vec<UnsignedEvent>, Error>>,
)>;

pub fn init(dock: WeakEntity<DockArea>, window: &mut Window, cx: &mut App) -> Entity<GreeterPanel> {
    cx.new(|cx| GreeterPanel::new(dock, window, cx))
}

pub struct GreeterPanel {
    name: SharedString,
    focus_handle: FocusHandle,
    dock: WeakEntity<DockArea>,

    /// Open chat requests paired with their last message, newest first
    requests: Vec<(Entity<Room>, SharedString)>,
    /// Ongoing chats whose latest message is not from the user, with previews
    follow_ups: Vec<(Entity<Room>, SharedString)>,

    /// The async load of the greeter's data
    load_task: Option<Task<Result<(), Error>>>,
    /// The coalesced reload scheduled after registry updates
    refresh_task: Option<Task<Result<(), Error>>>,
    /// Event subscriptions
    _subscriptions: SmallVec<[Subscription; 2]>,
}

impl GreeterPanel {
    fn new(dock: WeakEntity<DockArea>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let chat = ChatRegistry::global(cx);
        let nostr = NostrRegistry::global(cx);

        let mut subscriptions = smallvec![];

        subscriptions.push(
            // Reload when the user signs in or out
            cx.subscribe(&nostr, |this, _, event, cx| {
                if let StateEvent::SignerChanged = event {
                    this.load(cx);
                }
            }),
        );

        subscriptions.push(
            // Reload soon after rooms load, messages arrive, or requests change
            cx.observe(&chat, |this, _, cx| {
                this.schedule_refresh(cx);
            }),
        );

        // Run at the end of the current cycle
        cx.defer_in(window, |this, _window, cx| {
            this.load(cx);
        });

        Self {
            name: "Onboarding".into(),
            focus_handle: cx.focus_handle(),
            dock,
            requests: vec![],
            follow_ups: vec![],
            load_task: None,
            refresh_task: None,
            _subscriptions: subscriptions,
        }
    }

    /// Load the data for the sections shown while signed in
    fn load(&mut self, cx: &mut Context<Self>) {
        let chat = ChatRegistry::global(cx);
        let nostr = NostrRegistry::global(cx);

        let Some(public_key) = nostr.read(cx).current_user() else {
            // Dropping the tasks cancels any in-flight load
            self.load_task = None;
            self.refresh_task = None;
            self.requests.clear();
            self.follow_ups.clear();
            cx.notify();
            return;
        };

        // The latest message of each room drives its preview and follow up state
        let pending: PendingMessages = chat
            .read(cx)
            .rooms(&RoomKind::Ongoing, cx)
            .into_iter()
            .chain(chat.read(cx).rooms(&RoomKind::Request, cx))
            .map(|room| {
                let kind = room.read(cx).kind;
                let messages = room.read(cx).get_messages(cx);
                (room, kind, messages)
            })
            .collect();

        self.load_task.replace(cx.spawn(async move |this, cx| {
            let mut requests = Vec::new();
            let mut follow_ups = Vec::new();

            for (room, kind, messages) in pending {
                let messages = match messages.await {
                    Ok(messages) => messages,
                    Err(error) => {
                        log::warn!("Failed to load messages for the greeter: {error}");
                        continue;
                    }
                };

                let Some(last) = messages.last() else {
                    continue;
                };

                let preview = Message::from(last).preview();

                if kind == RoomKind::Request {
                    requests.push((room, preview));
                } else if last.pubkey != public_key {
                    follow_ups.push((room, preview));
                }
            }

            this.update(cx, |this, cx| {
                follow_ups.sort_by(|a, b| b.0.read(cx).cmp(a.0.read(cx)));
                follow_ups.truncate(SECTION_SIZE);
                this.follow_ups = follow_ups;
                this.requests = requests;

                cx.notify();
            })?;

            Ok(())
        }));
    }

    /// Schedule one reload, collapsing bursts of registry updates
    fn schedule_refresh(&mut self, cx: &mut Context<Self>) {
        self.refresh_task.replace(cx.spawn(async move |this, cx| {
            cx.background_executor().timer(REFRESH_DELAY).await;
            this.update(cx, |this, cx| this.load(cx))?;
            Ok(())
        }));
    }

    fn render_requests(&mut self, cx: &mut Context<Self>) -> Option<AnyElement> {
        if self.requests.is_empty() {
            return None;
        }

        let dock = self.dock.clone();

        let rows: Vec<AnyElement> = self
            .requests
            .iter()
            .take(SECTION_SIZE)
            .enumerate()
            .map(|(index, (room, preview))| {
                let room = room.clone();
                let dock = dock.clone();
                let name = room.read(cx).display_name(cx);
                let picture = room.read(cx).display_image(cx);
                let seed = room.read(cx).display_image_seed(cx);
                let created_at = room.read(cx).created_at.to_ago();
                let preview = preview.clone();

                h_flex()
                    .id(format!("request-{index}"))
                    .group("")
                    .w_full()
                    .h_9()
                    .px_1p5()
                    .gap_2()
                    .rounded(px(button_radius()))
                    .cursor_pointer()
                    .hover(|this| this.bg(cx.theme().element_hover))
                    .on_click({
                        let room = room.clone();
                        let dock = dock.clone();
                        move |_, window, cx| {
                            open_room(&room, &dock, window, cx);
                        }
                    })
                    .child(
                        Avatar::from_source(picture)
                            .seed(seed)
                            .small()
                            .flex_shrink_0(),
                    )
                    .child(
                        div()
                            .min_w_0()
                            .truncate()
                            .text_style(TextStyle::Callout)
                            .child(name),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .text_style(TextStyle::Callout)
                            .text_color(cx.theme().text_muted)
                            .italic()
                            .child(preview),
                    )
                    .child(
                        div()
                            .relative()
                            .flex_shrink_0()
                            .h_6()
                            .flex()
                            .items_center()
                            .justify_end()
                            .child(
                                div().group_hover("", |this| this.invisible()).child(
                                    div()
                                        .text_style(TextStyle::Caption)
                                        .text_color(cx.theme().text_faint)
                                        .child(created_at),
                                ),
                            )
                            .child(
                                Button::new(format!("response-{index}"))
                                    .label("Response")
                                    .xsmall()
                                    .primary()
                                    .invisible()
                                    .group_hover("", |this| this.visible())
                                    .on_click({
                                        let room = room.clone();
                                        let dock = dock.clone();
                                        move |_, window, cx| {
                                            cx.stop_propagation();
                                            open_room(&room, &dock, window, cx);
                                        }
                                    }),
                            ),
                    )
                    .into_any_element()
            })
            .collect();

        Some(
            GroupBox::new()
                .id("requests")
                .title("Requests")
                .fill()
                .child(v_flex().gap_0().w_full().children(rows))
                .into_any_element(),
        )
    }

    fn render_follow_ups(&mut self, cx: &mut Context<Self>) -> Option<AnyElement> {
        if self.follow_ups.is_empty() {
            return None;
        }

        let dock = self.dock.clone();

        let rows: Vec<AnyElement> = self
            .follow_ups
            .iter()
            .enumerate()
            .map(|(index, (room, preview))| {
                let room = room.clone();
                let dock = dock.clone();
                let name = room.read(cx).display_name(cx);
                let picture = room.read(cx).display_image(cx);
                let seed = room.read(cx).display_image_seed(cx);
                let created_at = room.read(cx).created_at.to_ago();
                let preview = preview.clone();

                h_flex()
                    .id(format!("follow-up-{index}"))
                    .group("")
                    .w_full()
                    .h_9()
                    .px_1p5()
                    .gap_2()
                    .rounded(px(button_radius()))
                    .cursor_pointer()
                    .hover(|this| this.bg(cx.theme().element_hover))
                    .on_click({
                        let room = room.clone();
                        let dock = dock.clone();
                        move |_, window, cx| {
                            open_room(&room, &dock, window, cx);
                        }
                    })
                    .child(
                        Avatar::from_source(picture)
                            .seed(seed)
                            .small()
                            .flex_shrink_0(),
                    )
                    .child(
                        div()
                            .min_w_0()
                            .truncate()
                            .text_style(TextStyle::Callout)
                            .child(name),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .text_style(TextStyle::Callout)
                            .text_color(cx.theme().text_muted)
                            .italic()
                            .child(preview),
                    )
                    .child(
                        div()
                            .relative()
                            .flex_shrink_0()
                            .h_6()
                            .flex()
                            .items_center()
                            .justify_end()
                            .child(
                                div()
                                    .absolute()
                                    .left_0()
                                    .right_0()
                                    .top_0()
                                    .bottom_0()
                                    .flex()
                                    .items_center()
                                    .justify_center()
                                    .group_hover("", |this| this.invisible())
                                    .child(
                                        div()
                                            .font_normal()
                                            .text_style(TextStyle::Caption)
                                            .text_color(cx.theme().text_faint)
                                            .child(created_at),
                                    ),
                            )
                            .child(
                                Button::new(format!("reply-{index}"))
                                    .label("Reply")
                                    .xsmall()
                                    .primary()
                                    .invisible()
                                    .group_hover("", |this| this.visible())
                                    .on_click({
                                        let room = room.clone();
                                        let dock = dock.clone();
                                        move |_, window, cx| {
                                            cx.stop_propagation();
                                            open_room(&room, &dock, window, cx);
                                        }
                                    }),
                            ),
                    )
                    .into_any_element()
            })
            .collect();

        Some(
            GroupBox::new()
                .id("follow-ups")
                .title("Follow ups")
                .fill()
                .child(v_flex().gap_0().w_full().children(rows))
                .into_any_element(),
        )
    }

    fn render_find_friends(&mut self, cx: &mut Context<Self>) -> impl IntoElement {
        v_flex()
            .gap_1p5()
            .w_full()
            .child(
                div()
                    .text_color(cx.theme().text_muted)
                    .text_style(TextStyle::Subheadline)
                    .font_semibold()
                    .child("Find new friends"),
            )
            .child(
                v_flex()
                    .w_full()
                    .child(
                        Nav::new("browse-group")
                            .prefix(
                                Icon::new(IconName::Compass)
                                    .small()
                                    .text_color(cx.theme().text_faint),
                            )
                            .label("Browse Group")
                            .text_style(TextStyle::Callout)
                            .on_click(cx.listener(|this, _, window, cx| {
                                let panel = browse::init(this.dock.clone(), window, cx);
                                add_panel_to(
                                    &this.dock,
                                    PanelHandle::new(panel),
                                    DockPlacement::Center,
                                    window,
                                    cx,
                                );
                            })),
                    )
                    .child(
                        Nav::new("browse-posts")
                            .prefix(Icon::new(IconName::Link).small())
                            .label("Browse Nostr via Jumble")
                            .text_style(TextStyle::Callout)
                            .on_click(|_, _, cx| {
                                cx.open_url(JUMBLE_URL);
                            }),
                    ),
            )
    }
}

impl Panel for GreeterPanel {
    fn panel_id(&self) -> SharedString {
        self.name.clone()
    }

    fn title(&self, cx: &App) -> AnyElement {
        h_flex()
            .gap_1()
            .text_style(TextStyle::Caption)
            .child(
                svg()
                    .path("brand/coop.svg")
                    .size_4()
                    .text_color(cx.theme().text_muted),
            )
            .child("Welcome")
            .into_any_element()
    }
}

impl EventEmitter<PanelEvent> for GreeterPanel {}

impl Focusable for GreeterPanel {
    fn focus_handle(&self, _: &App) -> gpui::FocusHandle {
        self.focus_handle.clone()
    }
}

impl Render for GreeterPanel {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let nostr = NostrRegistry::global(cx);
        let logged_in = nostr.read(cx).current_user().is_some();

        v_flex()
            .size_full()
            .when(!logged_in, |this| {
                this.items_center().justify_center().child(
                    svg()
                        .path("brand/coop.svg")
                        .size_12()
                        .text_color(cx.theme().text_faint),
                )
            })
            .when(logged_in, |this| {
                this.child(
                    v_flex()
                        .size_full()
                        .p_2()
                        .gap_6()
                        .children(self.render_requests(cx))
                        .children(self.render_follow_ups(cx))
                        .child(self.render_find_friends(cx)),
                )
            })
            .overflow_y_scrollbar()
    }
}
