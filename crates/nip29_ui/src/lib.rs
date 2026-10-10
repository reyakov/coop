use std::collections::BTreeMap;

use anyhow::Result;
use chat_ui::text::rendered_text;
use gpui::prelude::FluentBuilder;
use gpui::{
    Action, AnyElement, App, AppContext, Context, Entity, EventEmitter, FocusHandle, Focusable,
    FollowMode, IntoElement, ListAlignment, ListScrollEvent, ListState, ParentElement,
    PathPromptOptions, Render, SharedString, Styled, Subscription, Task, WeakEntity, Window, div,
    list, px,
};
use nip29::{Group, GroupEvent, GroupKey, GroupsRegistry, Membership, Pin, Row};
use nostr_sdk::prelude::*;
use person::PersonRegistry;
use serde::Deserialize;
use settings::AppSettings;
use smallvec::{SmallVec, smallvec};
use state::{NostrRegistry, upload};
use theme::{ActiveTheme, TextStyle, Typeset as _, control_radius};
use ui::avatar::Avatar;
use ui::button::{Button, ButtonVariant, ButtonVariants};
use ui::dialog::DialogButtonProps;
use ui::dock::{Panel, PanelEvent};
use ui::input::{InputEvent, Textarea, TextareaState};
use ui::markdown::RenderedText;
use ui::message::WelcomeMessage;
use ui::notification::Notification;
use ui::scroll::Scrollbar;
use ui::{Disableable, Icon, IconName, Sizable, WindowExtension, h_flex, v_flex};
use util::{attachment_name, display_name, opens_run, pin_label, report};

mod details;
mod invite;
mod message;
mod metadata;
mod util;

/// Ask the shell to show the sidebar's information for a group.
#[derive(Action, Clone, PartialEq, Eq, Deserialize)]
#[action(namespace = nip29_ui, no_json)]
pub struct ShowGroupMeta {
    pub relay: RelayUrl,
    pub group: String,
}

/// Ask the shell to hide the sidebar's information for a group.
#[derive(Action, Clone, PartialEq, Eq, Deserialize)]
#[action(namespace = nip29_ui, no_json)]
pub struct HideGroupMeta {
    pub relay: RelayUrl,
    pub group: String,
}

/// Ask the shell to open the channel creation dialog under a group.
#[derive(Action, Clone, PartialEq, Eq, Deserialize)]
#[action(namespace = nip29_ui, no_json)]
pub struct CreateChannel {
    pub relay: RelayUrl,
    pub parent: String,
}

const ADMIN_ROLE: &str = "admin";
const LOAD_OLDER_THRESHOLD: usize = 20;

enum Notice {
    Relay(String),
    Join {
        refused: Option<String>,
        closed: bool,
    },
    Pending,
    Unsupported,
}

impl Notice {
    fn of(group: &Group) -> Option<Self> {
        let reason = group.refused()?;
        Some(Self::Relay(reason.to_owned()))
    }
}

pub fn init(group: Entity<Group>, window: &mut Window, cx: &mut App) -> Entity<GroupPanel> {
    let key = group.read(cx).key().clone();
    let groups = GroupsRegistry::global(cx);

    groups.update(cx, |registry, cx| {
        registry.activate(&key, cx);
    });

    cx.new(|cx| GroupPanel::new(group, window, cx))
}

/// Build the group's action buttons for the sidebar: info, admin actions and leave.
pub fn actions(group: WeakEntity<Group>, cx: &App) -> AnyElement {
    let nostr = NostrRegistry::global(cx);
    let me = nostr.read(cx).current_user();

    let admin = group
        .read_with(cx, |group, _cx| {
            me.is_some_and(|me| group.admins().contains(&me))
        })
        .unwrap_or(false);

    let info = group.clone();
    let edit = group.clone();
    let invite = group.clone();
    let channel = group.clone();
    let leave = group;

    h_flex()
        .gap_1()
        .child(
            Button::new("group-info")
                .icon(IconName::Info)
                .tooltip("Group info")
                .ghost()
                .small()
                .on_click(move |_event, window, cx| {
                    details::open(info.clone(), window, cx);
                }),
        )
        .when(admin, |this| {
            this.child(
                Button::new("edit")
                    .icon(IconName::Edit)
                    .tooltip("Edit group")
                    .ghost()
                    .small()
                    .on_click(move |_event, window, cx| {
                        metadata::open(edit.clone(), window, cx);
                    }),
            )
            .child(
                Button::new("group-invite")
                    .icon(IconName::Invite)
                    .tooltip("Invite people")
                    .ghost()
                    .small()
                    .on_click(move |_event, window, cx| {
                        invite::open(invite.clone(), window, cx);
                    }),
            )
            .child(
                Button::new("group-channel")
                    .icon(IconName::Plus)
                    .tooltip("Create channel")
                    .ghost()
                    .small()
                    .on_click(move |_, window, cx| {
                        let Ok(key) = channel.read_with(cx, |group, _cx| group.key().clone())
                        else {
                            return;
                        };

                        window.dispatch_action(
                            Box::new(CreateChannel {
                                relay: key.relay().clone(),
                                parent: key.id().as_str().to_owned(),
                            }),
                            cx,
                        );
                    }),
            )
        })
        .child(
            Button::new("leave")
                .icon(IconName::Logout)
                .tooltip("Leave group")
                .ghost()
                .small()
                .on_click(move |_event, window, cx| {
                    confirm_leave(leave.clone(), window, cx);
                }),
        )
        .into_any_element()
}

pub fn confirm_leave(group: WeakEntity<Group>, window: &mut Window, cx: &mut App) {
    let Some(name) = group.read_with(cx, |group, _cx| group.display_name()).ok() else {
        return;
    };

    let Some(key) = group.read_with(cx, |group, _cx| group.key().clone()).ok() else {
        return;
    };

    window.open_dialog(cx, move |this, _window, _cx| {
        let key = key.clone();

        this.confirm()
            .width(px(360.))
            .title(format!("Leave {name}?"))
            .button_props(
                DialogButtonProps::default()
                    .cancel_text("Cancel")
                    .ok_text("Leave")
                    .ok_variant(ButtonVariant::Danger),
            )
            .child(
                div()
                    .text_style(TextStyle::Callout)
                    .child("You will stop receiving messages from this group."),
            )
            .on_ok(move |_event, _window, cx| {
                let registry = GroupsRegistry::global(cx);
                registry.update(cx, |registry, cx| {
                    registry.leave(&key, cx);
                });
                true
            })
    });
}

/// Remove a member from the group and surface any relay error as a notification.
pub fn remove_member(
    group: WeakEntity<Group>,
    public_key: PublicKey,
    window: &mut Window,
    cx: &mut App,
) {
    match group.update(cx, |group, cx| group.remove_user(public_key, cx)) {
        Ok(task) => report(window, cx, task),
        Err(error) => log::warn!("nip29: removing a group member failed: {error}"),
    }
}

/// Grant a member the admin role and surface any relay error as a notification.
pub fn make_admin(
    group: WeakEntity<Group>,
    public_key: PublicKey,
    window: &mut Window,
    cx: &mut App,
) {
    put_user(group, public_key, vec![ADMIN_ROLE.to_owned()], window, cx);
}

/// Revoke a member's roles, returning them to a regular member.
pub fn remove_admin(
    group: WeakEntity<Group>,
    public_key: PublicKey,
    window: &mut Window,
    cx: &mut App,
) {
    put_user(group, public_key, Vec::new(), window, cx);
}

fn put_user(
    group: WeakEntity<Group>,
    public_key: PublicKey,
    roles: Vec<String>,
    window: &mut Window,
    cx: &mut App,
) {
    match group.update(cx, |group, cx| group.put_user(public_key, roles, cx)) {
        Ok(task) => report(window, cx, task),
        Err(error) => log::warn!("nip29: updating a member's roles failed: {error}"),
    }
}

pub struct GroupPanel {
    id: SharedString,
    focus_handle: FocusHandle,
    group: WeakEntity<Group>,
    list_state: ListState,
    head: Option<EventId>,
    rendered_texts_by_id: BTreeMap<EventId, RenderedText>,
    input: Entity<TextareaState>,
    /// Uploaded, non-encrypted attachments waiting to be sent.
    attachments: Vec<Url>,
    /// Whether an upload is in flight.
    uploading: bool,
    tasks: SmallVec<[Task<Result<()>>; 4]>,
    _subscriptions: SmallVec<[Subscription; 3]>,
}

impl GroupPanel {
    pub(crate) fn new(group: Entity<Group>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let (id, name, count, head) = {
            let group = group.read(cx);
            let rows = group.rows();

            (
                SharedString::from(format!("group-{}", group.key().cache_tag())),
                display_name(group),
                rows.len(),
                rows.first().map(Row::id),
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

        subscriptions.push(cx.subscribe(&group, |this, _group, event, cx| {
            if !matches!(event, GroupEvent::Sent(_)) {
                this.sync(cx);
                cx.notify();
            }
        }));

        subscriptions.push(cx.observe(&group, |this, _group, cx| {
            this.sync(cx);
            cx.notify();
        }));

        cx.defer_in(window, |this, _window, cx| {
            this.list_state.set_follow_mode(FollowMode::Tail);
            this.list_state.set_scroll_handler(cx.listener(
                |this, event: &ListScrollEvent, _window, cx| {
                    if event.visible_range.start <= LOAD_OLDER_THRESHOLD {
                        this.load_older(cx);
                    }
                },
            ));
        });

        Self {
            id,
            focus_handle: cx.focus_handle(),
            group: group.downgrade(),
            list_state: ListState::new(count + 2, ListAlignment::Bottom, px(1024.)),
            head,
            rendered_texts_by_id: BTreeMap::new(),
            input,
            attachments: Vec::new(),
            uploading: false,
            tasks: smallvec![],
            _subscriptions: subscriptions,
        }
    }

    fn item_count(&self, cx: &App) -> usize {
        self.group
            .upgrade()
            .map_or(2, |group| group.read(cx).rows().len() + 2)
    }

    fn sync(&mut self, cx: &App) {
        let Some(group) = self.group.upgrade() else {
            return;
        };

        let group = group.read(cx);
        let rows = group.rows();
        let head = rows.first().map(Row::id);
        let total = rows.len() + 2;
        let old_total = self.list_state.item_count();

        if total == old_total {
            self.head = head;
            return;
        }

        let old_len = old_total.saturating_sub(2);
        let prepended = self
            .head
            .and_then(|old_head| rows.iter().position(|row| row.id() == old_head))
            .filter(|prepended| prepended + old_len <= rows.len());

        match prepended {
            Some(prepended) => {
                if prepended > 0 {
                    self.list_state.splice(2..2, prepended);
                }

                let appended = rows.len() - old_len - prepended;

                if appended > 0 {
                    let at = old_total + prepended;
                    self.list_state.splice(at..at, appended);
                }
            }
            None => self.list_state.reset(total),
        }

        self.head = head;
    }

    fn load_older(&mut self, cx: &mut App) {
        let Some(key) = self
            .group
            .upgrade()
            .map(|group| group.read(cx).key().clone())
        else {
            return;
        };

        let groups = GroupsRegistry::global(cx);

        groups.update(cx, |registry, cx| {
            registry.load_more(&key, cx);
        });
    }

    fn send(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let mut content = self.input.read(cx).value().trim().to_owned();

        if !self.attachments.is_empty() {
            let urls = self
                .attachments
                .iter()
                .map(Url::to_string)
                .collect::<Vec<_>>()
                .join("\n");

            content = if content.is_empty() {
                urls
            } else {
                format!("{content}\n{urls}")
            };
        }

        if content.is_empty() {
            return;
        }

        let Ok(send) = self
            .group
            .update(cx, |group, cx| group.send_message(&content, vec![], cx))
        else {
            return;
        };

        self.input.update(cx, |input, cx| {
            input.set_value("", window, cx);
        });
        self.attachments.clear();

        self.tasks
            .push(cx.spawn_in::<_, Result<()>>(window, async move |this, cx| {
                if let Err(error) = send.await {
                    this.update_in(cx, |_this, window, cx| {
                        window.push_notification(Notification::error(error.to_string()), cx);
                    })?;
                }
                Ok(())
            }));
    }

    fn upload(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let server = AppSettings::get_file_server(cx);

        let path = cx.prompt_for_paths(PathPromptOptions {
            files: true,
            directories: false,
            multiple: false,
            prompt: None,
        });

        self.tasks.push(cx.spawn_in(window, async move |this, cx| {
            // Selecting no file means the prompt was cancelled
            let Some(path) = path.await??.and_then(|mut paths| paths.pop()) else {
                return Ok(());
            };

            this.update(cx, |this, cx| this.set_uploading(true, cx))?;

            match upload(server, path, cx).await {
                Ok(url) => {
                    this.update_in(cx, |this, _window, cx| {
                        this.attachments.push(url);
                        this.set_uploading(false, cx);
                    })?;
                }
                Err(error) => {
                    this.update_in(cx, |this, window, cx| {
                        this.set_uploading(false, cx);

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

    fn set_uploading(&mut self, uploading: bool, cx: &mut Context<Self>) {
        self.uploading = uploading;
        cx.notify();
    }

    fn remove_attachment(&mut self, url: &Url, cx: &mut Context<Self>) {
        self.attachments.retain(|attachment| attachment != url);
        cx.notify();
    }

    fn notice(&self, cx: &App) -> Option<Notice> {
        self.group
            .upgrade()
            .and_then(|group| Notice::of(group.read(cx)))
    }

    fn react(
        &mut self,
        group: WeakEntity<Group>,
        id: EventId,
        emoji: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Ok(react) = group.update(cx, |group, cx| group.send_reaction(id, emoji, cx)) else {
            return;
        };

        self.tasks.push(cx.spawn_in(window, async move |this, cx| {
            if let Err(error) = react.await {
                this.update_in(cx, |_this, window, cx| {
                    window.push_notification(Notification::error(error.to_string()), cx);
                })?;
            }
            Ok(())
        }));
    }

    fn delete_message(
        &mut self,
        group: WeakEntity<Group>,
        id: EventId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Ok(delete) = group.update(cx, |group, cx| group.delete_event(id, cx)) else {
            return;
        };

        self.tasks.push(cx.spawn_in(window, async move |this, cx| {
            if let Err(error) = delete.await {
                this.update_in(cx, |_this, window, cx| {
                    window.push_notification(Notification::error(error.to_string()), cx);
                })?;
            }
            Ok(())
        }));
    }

    fn toggle_pin(
        &mut self,
        group: WeakEntity<Group>,
        id: EventId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Ok(update) = group.update(cx, |group, cx| {
            let mut pins = group.pins().pins().to_vec();

            match pins.iter().position(|pin| *pin == Pin::Event(id)) {
                Some(ix) => {
                    pins.remove(ix);
                }
                None => pins.push(Pin::Event(id)),
            }

            group.update_pins(pins, cx)
        }) else {
            return;
        };

        self.tasks.push(cx.spawn_in(window, async move |this, cx| {
            if let Err(error) = update.await {
                this.update_in(cx, |_this, window, cx| {
                    window.push_notification(Notification::error(error.to_string()), cx);
                })?;
            }
            Ok(())
        }));
    }

    fn render_banner(&self, cx: &mut Context<Self>) -> AnyElement {
        let Some(group) = self.group.upgrade() else {
            return div().into_any_element();
        };

        let Some(moved) = group.read(cx).elsewhere().cloned() else {
            return div().into_any_element();
        };

        let relay = moved.relay().clone();
        let message = SharedString::from(format!("This group may have moved to {relay}."));

        h_flex()
            .flex_shrink_0()
            .w_full()
            .gap(px(theme::SPACE))
            .px_3()
            .py_2()
            .text_style(TextStyle::Callout)
            .border_b_1()
            .border_color(cx.theme().border)
            .child(
                Icon::new(IconName::Info)
                    .small()
                    .text_color(cx.theme().text_faint),
            )
            .child(div().flex_1().min_w_0().truncate().child(message))
            .child(
                Button::new("group-moved")
                    .label("Join")
                    .small()
                    .on_click(cx.listener(move |this, _event, _window, cx| {
                        this.request_join(moved.clone(), cx);
                    })),
            )
            .into_any_element()
    }

    fn render_pins(&self, cx: &mut Context<Self>) -> AnyElement {
        let Some(group) = self.group.upgrade() else {
            return div().into_any_element();
        };

        let group = group.read(cx);
        let pins = group.pins().pins();

        if pins.is_empty() {
            return div().into_any_element();
        }

        h_flex()
            .flex_shrink_0()
            .w_full()
            .flex_wrap()
            .gap_1()
            .px_3()
            .py_1p5()
            .border_b_1()
            .border_color(cx.theme().border)
            .children(pins.iter().map(|pin| {
                div()
                    .px_1p5()
                    .py_0p5()
                    .rounded(px(control_radius()))
                    .bg(cx.theme().solid)
                    .text_style(TextStyle::Caption)
                    .text_color(cx.theme().text_muted)
                    .child(pin_label(group, pin))
            }))
            .into_any_element()
    }

    fn render_welcome(&self, cx: &App) -> AnyElement {
        let (name, avatar) = match self.group.upgrade() {
            Some(group) => {
                let group = group.read(cx);
                let seed = group.key().cache_tag();
                let picture = group
                    .metadata()
                    .and_then(|metadata| metadata.picture.clone());

                (
                    display_name(group),
                    Avatar::from_source(picture).seed(seed).large(),
                )
            }
            None => (SharedString::from("this group"), Avatar::new(None)),
        };

        WelcomeMessage::new("welcome")
            .icon(avatar)
            .title(format!("Welcome to {name}"))
            .message(format!("This is the start of {name}."))
            .into_any_element()
    }

    fn render_older(&self, cx: &mut Context<Self>) -> AnyElement {
        let Some(group) = self.group.upgrade() else {
            return div().into_any_element();
        };

        let (has_older, loading) = {
            let group = group.read(cx);

            (group.oldest().is_some(), group.loading_more())
        };

        if !has_older {
            return div().into_any_element();
        }

        h_flex()
            .w_full()
            .justify_center()
            .py_2()
            .child(
                Button::new("load-older")
                    .label(if loading {
                        "Loading earlier messages…"
                    } else {
                        "Load earlier messages"
                    })
                    .ghost()
                    .small()
                    .loading(loading)
                    .disabled(loading)
                    .on_click(cx.listener(|this, _event, _window, cx| this.load_older(cx))),
            )
            .into_any_element()
    }

    /// Ask the relay to admit the user into the group.
    fn request_join(&self, key: GroupKey, cx: &mut App) {
        GroupsRegistry::global(cx).update(cx, |registry, cx| {
            registry.join_key(key, None, cx);
        });
    }

    fn render_notice(&self, notice: &Notice, cx: &mut Context<Self>) -> AnyElement {
        let row = h_flex()
            .w_full()
            .justify_center()
            .items_center()
            .gap(px(theme::SPACE))
            .px_3()
            .py_2()
            .text_style(TextStyle::Callout)
            .text_color(cx.theme().text_muted);

        match notice {
            Notice::Relay(reason) => row
                .child(SharedString::from(reason.as_str()))
                .into_any_element(),
            Notice::Join { refused, closed } => {
                let row = match refused {
                    Some(reason) => row.child(SharedString::from(reason.as_str())),
                    None => row,
                };

                if *closed {
                    return row
                        .child("Group is closed. An invite link is required.")
                        .into_any_element();
                }

                let Some(key) = self
                    .group
                    .upgrade()
                    .map(|group| group.read(cx).key().clone())
                else {
                    return row.into_any_element();
                };

                row.child("You're in preview mode. Join this group to talk.")
                    .child(
                        Button::new("join")
                            .label("Request to Join")
                            .small()
                            .ghost_alt()
                            .on_click(cx.listener(move |this, _event, _window, cx| {
                                this.request_join(key.clone(), cx);
                            })),
                    )
                    .into_any_element()
            }
            Notice::Pending => row
                .child("Waiting for the relay to approve.")
                .into_any_element(),
            Notice::Unsupported => row
                .child("This group doesn't support text messages.")
                .into_any_element(),
        }
    }

    fn render_row(&mut self, ix: usize, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        if ix == 0 {
            return self.render_welcome(cx);
        }

        if ix == 1 {
            return self.render_older(cx);
        }

        let Some(group) = self.group.upgrade() else {
            return div().into_any_element();
        };

        let group = group.read(cx);
        let rows = group.rows();

        let Some(row) = rows.get(ix - 2) else {
            return div().into_any_element();
        };

        let message = match row {
            Row::Message(message) => message,
            Row::Activity(activity) => return message::render_activity(ix, activity, cx),
        };

        let nostr = NostrRegistry::global(cx);
        let persons = PersonRegistry::global(cx);

        let content = self
            .rendered_texts_by_id
            .entry(message.id)
            .or_insert_with(|| rendered_text(&message.content, &message.mentions, &persons, cx))
            .element(ix.into(), window, cx);

        let admin = nostr
            .read(cx)
            .current_user()
            .is_some_and(|me| group.admins().contains(&me));

        let show_author = opens_run(rows, ix - 2);
        let pinned = group.pins().pins().contains(&Pin::Event(message.id));

        message::render(
            ix,
            message,
            content,
            show_author,
            message::Actions {
                group: self.group.clone(),
                panel: cx.weak_entity(),
                admin,
                pinned,
            },
            cx,
        )
    }

    fn render_composer(&self, cx: &mut Context<Self>) -> AnyElement {
        let notice = self.group.upgrade().and_then(|group| {
            let group = group.read(cx);

            if !group
                .metadata()
                .is_none_or(|metadata| metadata.supports(Kind::ChatMessage))
            {
                return Some(Notice::Unsupported);
            }

            match group.membership() {
                Membership::Member => None,
                Membership::Pending { .. } => Some(Notice::Pending),
                _ => Some(Notice::Join {
                    refused: None,
                    closed: group.metadata().is_some_and(|metadata| metadata.closed),
                }),
            }
        });

        if let Some(notice) = notice {
            return self.render_notice(&notice, cx);
        }

        v_flex()
            .flex_shrink_0()
            .w_full()
            .p_2()
            .gap_1()
            .children(self.render_attachments(cx))
            .child(
                h_flex()
                    .gap_1()
                    .items_end()
                    .child(
                        Button::new("upload")
                            .icon(IconName::Upload)
                            .tooltip("Upload file")
                            .loading(self.uploading)
                            .disabled(self.uploading)
                            .ghost()
                            .large()
                            .on_click(cx.listener(|this, _event, window, cx| {
                                this.upload(window, cx);
                            })),
                    )
                    .child(Textarea::new(&self.input).appearance(false).flex_1())
                    .child(
                        Button::new("send")
                            .icon(IconName::PaperPlaneFill)
                            .tooltip("Send")
                            .ghost()
                            .large()
                            .on_click(cx.listener(|this, _event, window, cx| {
                                this.send(window, cx);
                            })),
                    ),
            )
            .into_any_element()
    }

    fn render_attachments(&self, cx: &mut Context<Self>) -> Vec<AnyElement> {
        self.attachments
            .iter()
            .enumerate()
            .map(|(ix, url)| {
                let url = url.clone();

                h_flex()
                    .items_center()
                    .gap_1()
                    .h_8()
                    .px_2()
                    .border_1()
                    .border_color(cx.theme().border_faint)
                    .rounded(px(control_radius()))
                    .text_style(TextStyle::Caption)
                    .text_color(cx.theme().text_muted)
                    .child(Icon::new(IconName::Link).xsmall())
                    .child(
                        div()
                            .max_w(px(200.))
                            .truncate()
                            .child(attachment_name(&url)),
                    )
                    .child(
                        Button::new(SharedString::from(format!("remove-attachment-{ix}")))
                            .icon(IconName::Close)
                            .tooltip("Remove")
                            .ghost()
                            .xsmall()
                            .on_click(cx.listener(move |this, _event, _window, cx| {
                                this.remove_attachment(&url, cx);
                            })),
                    )
                    .into_any_element()
            })
            .collect()
    }
}

impl Panel for GroupPanel {
    fn panel_id(&self) -> SharedString {
        self.id.clone()
    }

    fn title(&self, cx: &App) -> AnyElement {
        self.group
            .upgrade()
            .map(|group| {
                let group = group.read(cx);
                let seed = group.key().cache_tag();
                let picture = group
                    .metadata()
                    .and_then(|metadata| metadata.picture.clone());

                h_flex()
                    .gap_1()
                    .text_style(TextStyle::Caption)
                    .child(Avatar::from_source(picture).seed(seed).xsmall())
                    .child(display_name(group))
                    .into_any_element()
            })
            .unwrap_or_else(|| {
                div()
                    .text_style(TextStyle::Caption)
                    .child("Unknown")
                    .into_any_element()
            })
    }

    fn toolbar_buttons(&self, _window: &Window, _cx: &App) -> Vec<Button> {
        Vec::new()
    }

    fn set_active(&self, active: bool, window: &mut Window, cx: &mut App) {
        let Some(group) = self.group.upgrade() else {
            return;
        };

        let (relay, id) = {
            let group = group.read(cx);

            (
                group.key().relay().clone(),
                group.key().id().as_str().to_owned(),
            )
        };

        if active {
            window.dispatch_action(Box::new(ShowGroupMeta { relay, group: id }), cx);
        } else {
            window.dispatch_action(Box::new(HideGroupMeta { relay, group: id }), cx);
        }
    }
}

impl EventEmitter<PanelEvent> for GroupPanel {}

impl Focusable for GroupPanel {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Render for GroupPanel {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let notice = self.notice(cx);
        let empty = self.item_count(cx) == 2;

        v_flex()
            .size_full()
            .min_w_0()
            .child(self.render_banner(cx))
            .child(self.render_pins(cx))
            .child(
                v_flex()
                    .flex_1()
                    .min_h_0()
                    .relative()
                    .map(|this| {
                        if empty {
                            this.child(
                                v_flex().size_full().justify_center().child(match notice {
                                    Some(notice) => self.render_notice(&notice, cx),
                                    None => h_flex()
                                        .size_full()
                                        .justify_center()
                                        .text_style(TextStyle::Callout)
                                        .text_color(cx.theme().text_faint)
                                        .child("No messages yet")
                                        .into_any_element(),
                                }),
                            )
                        } else {
                            this.when_some(notice.as_ref(), |this, notice| {
                                this.child(self.render_notice(notice, cx))
                            })
                            .child(
                                list(
                                    self.list_state.clone(),
                                    cx.processor(move |this, ix, window, cx| {
                                        this.render_row(ix, window, cx)
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
