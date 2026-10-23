use std::collections::BTreeMap;

use anyhow::Result;
use gpui::prelude::FluentBuilder;
use gpui::{
    Action, AnyElement, App, AppContext, Context, Entity, EventEmitter, FocusHandle, Focusable,
    FollowMode, IntoElement, ListAlignment, ListScrollEvent, ListState, ParentElement,
    PathPromptOptions, Render, SharedString, Styled, Subscription, Task, WeakEntity, Window, div,
    list, px,
};
use gpui_kit::assets::IconName;
use gpui_kit::base::StyledExt as _;
use gpui_kit::component::button::{Button, ButtonVariant, ButtonVariants};
use gpui_kit::component::dialog::DialogButtonProps;
use gpui_kit::component::dock::{Panel, PanelEvent};
use gpui_kit::component::input::{InputEvent, Textarea, TextareaState};
use gpui_kit::component::notification::Notification;
use gpui_kit::component::scroll::Scrollbar;
use gpui_kit::component::{ActiveTheme, Disableable, Icon, Sizable, WindowExt, h_flex, v_flex};
use nip29::{Group, GroupEvent, GroupKey, GroupsRegistry, Membership, Pin, Row};
use nostr_sdk::prelude::*;
use person::Directory;
use serde::Deserialize;
use settings::AppSettings;
use smallvec::{SmallVec, smallvec};
use state::{NostrRegistry, upload};
use ui::{Avatar, RenderedText, WelcomeMessage, rendered_text};
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
    let delete = group.clone();
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
                    .icon(IconName::SquarePen)
                    .tooltip("Edit group")
                    .ghost()
                    .small()
                    .on_click(move |_event, window, cx| {
                        metadata::open(edit.clone(), window, cx);
                    }),
            )
            .child(
                Button::new("group-invite")
                    .icon(IconName::UserRoundPlus)
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
            .child(
                Button::new("group-delete")
                    .icon(IconName::CircleX)
                    .tooltip("Delete group")
                    .ghost()
                    .small()
                    .on_click(move |_event, window, cx| {
                        confirm_delete(delete.clone(), window, cx);
                    }),
            )
        })
        .child(
            Button::new("leave")
                .icon(IconName::LogOut)
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

    window.open_alert_dialog(cx, move |this, _window, _cx| {
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
                    .text_xs()
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

pub fn confirm_delete(group: WeakEntity<Group>, window: &mut Window, cx: &mut App) {
    let Some(name) = group.read_with(cx, |group, _cx| group.display_name()).ok() else {
        return;
    };

    let Some(key) = group.read_with(cx, |group, _cx| group.key().clone()).ok() else {
        return;
    };

    window.open_alert_dialog(cx, move |this, _window, _cx| {
        let key = key.clone();

        this.confirm()
            .width(px(360.))
            .title(format!("Delete {name}?"))
            .button_props(
                DialogButtonProps::default()
                    .cancel_text("Cancel")
                    .ok_text("Delete")
                    .ok_variant(ButtonVariant::Danger),
            )
            .child(
                div()
                    .text_xs()
                    .child("The relay will remove this group for everyone."),
            )
            .on_ok(move |_event, window, cx| {
                let registry = GroupsRegistry::global(cx);
                let sent = registry.update(cx, |registry, cx| registry.delete_group(&key, cx));
                report(window, cx, sent);

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
    focus_handle: FocusHandle,
    group: WeakEntity<Group>,
    /// Person directory for members, message authors, and activity subjects
    directory: Entity<Directory>,
    list_state: ListState,
    head: Option<EventId>,
    /// The message the composer is replying to, if any.
    reply_to: Option<EventId>,
    rendered_texts_by_id: BTreeMap<EventId, RenderedText>,
    input: Entity<TextareaState>,
    /// Uploaded, non-encrypted attachments waiting to be sent.
    attachments: Vec<Url>,
    /// Whether an upload is in flight.
    uploading: bool,
    /// Whether the user closed the encryption notice this visit
    notice_dismissed: bool,
    tasks: SmallVec<[Task<Result<()>>; 4]>,
    _subscriptions: SmallVec<[Subscription; 3]>,
}

impl GroupPanel {
    pub(crate) fn new(group: Entity<Group>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let (name, count, head) = {
            let group = group.read(cx);
            let rows = group.rows();

            (display_name(group), rows.len(), rows.first().map(Row::id))
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
                this.track_participants(cx);
                cx.notify();
            }
        }));

        subscriptions.push(cx.observe(&group, |this, _group, cx| {
            this.sync(cx);
            this.track_participants(cx);
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

        let mut this = Self {
            focus_handle: cx.focus_handle(),
            group: group.downgrade(),
            directory: Directory::new(cx),
            list_state: ListState::new(count + 2, ListAlignment::Bottom, px(1024.)),
            head,
            reply_to: None,
            notice_dismissed: false,
            rendered_texts_by_id: BTreeMap::new(),
            input,
            attachments: Vec::new(),
            uploading: false,
            tasks: smallvec![],
            _subscriptions: subscriptions,
        };

        this.track_participants(cx);

        this
    }

    /// Track the keys rendered by this panel: members, message authors, and
    /// activity subjects
    fn track_participants(&mut self, cx: &mut Context<Self>) {
        let Some(group) = self.group.upgrade() else {
            return;
        };

        let group = group.read(cx);
        let mut keys: Vec<PublicKey> = group.members().iter().copied().collect();
        keys.extend(group.rows().iter().map(|row| match row {
            Row::Message(message) => message.author,
            Row::Activity(activity) => activity.subject(),
        }));

        self.directory
            .update(cx, |directory, cx| directory.track(keys, cx));
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

    fn scroll_to(&self, id: &EventId, cx: &App) {
        let Some(group) = self.group.upgrade() else {
            return;
        };

        let rows = group.read(cx).rows();

        if let Some(ix) = rows.iter().position(|row| row.id() == *id) {
            // The welcome and load-older rows precede the messages.
            self.list_state.scroll_to_reveal_item(ix + 2);
        }
    }

    fn start_reply(&mut self, id: EventId, window: &mut Window, cx: &mut Context<Self>) {
        self.reply_to = Some(id);
        self.input.update(cx, |input, cx| {
            input.focus(window, cx);
        });
        cx.notify();
    }

    fn cancel_reply(&mut self, cx: &mut Context<Self>) {
        if self.reply_to.take().is_some() {
            cx.notify();
        }
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

        let replies: Vec<EventId> = self.reply_to.take().into_iter().collect();
        let emojis = self.emoji_tags(&content, cx);

        let Ok(send) = self.group.update(cx, |group, cx| {
            group.send_message(&content, replies, emojis, cx)
        }) else {
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

    /// The signed-in user's custom emoji tags referenced in `content`
    fn emoji_tags(&self, content: &str, cx: &App) -> Vec<Tag> {
        let Some(current_user) = NostrRegistry::global(cx).read(cx).current_user() else {
            return Vec::new();
        };

        self.directory
            .read(cx)
            .person(&current_user, cx)
            .emoji_tags(content)
    }

    fn react(
        &mut self,
        group: WeakEntity<Group>,
        id: EventId,
        emoji: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let emojis = self.emoji_tags(&emoji, cx);

        let Ok(react) = group.update(cx, |group, cx| group.send_reaction(id, emoji, emojis, cx))
        else {
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
            .gap_2()
            .px_3()
            .py_2()
            .text_xs()
            .border_b_1()
            .border_color(cx.theme().border)
            .child(
                Icon::new(IconName::Info)
                    .small()
                    .text_color(cx.theme().muted_foreground),
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

    /// Notice shown while viewing a public group, where messages are plaintext
    fn render_encryption_notice(&self, cx: &mut Context<Self>) -> AnyElement {
        let is_public = self
            .group
            .upgrade()
            .and_then(|group| group.read(cx).metadata().map(|metadata| !metadata.private))
            .unwrap_or(false);

        if !is_public || self.notice_dismissed || AppSettings::get_hide_group_notice(cx) {
            return div().into_any_element();
        }

        h_flex()
            .flex_shrink_0()
            .w_full()
            .gap_2()
            .px_3()
            .py_2()
            .text_xs()
            .bg(cx.theme().secondary)
            .child(
                Icon::new(IconName::Info)
                    .small()
                    .text_color(cx.theme().warning),
            )
            .child(div().flex_1().min_w_0().child(
                "This group is public. Messages are not encrypted and can be read by anyone.",
            ))
            .child(
                h_flex()
                    .gap_1()
                    .child(
                        Button::new("hide")
                            .label("Don't show again")
                            .xsmall()
                            .secondary()
                            .on_click(cx.listener(|this, _event, _window, cx| {
                                AppSettings::update_hide_group_notice(true, cx);
                                this.notice_dismissed = true;
                                cx.notify();
                            })),
                    )
                    .child(
                        Button::new("dismiss")
                            .icon(IconName::X)
                            .small()
                            .ghost()
                            .tooltip("Close")
                            .on_click(cx.listener(|this, _event, _window, cx| {
                                this.notice_dismissed = true;
                                cx.notify();
                            })),
                    ),
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
                    .rounded(cx.theme().radius)
                    .bg(cx.theme().primary)
                    .text_xs()
                    .text_color(cx.theme().muted_foreground)
                    .child(pin_label(group, pin))
            }))
            .into_any_element()
    }

    fn render_welcome(&self, cx: &App) -> AnyElement {
        let (name, avatar) = match self.group.upgrade() {
            Some(group) => {
                let group = group.read(cx);
                let picture = group
                    .metadata()
                    .and_then(|metadata| metadata.picture.clone());

                (
                    display_name(group),
                    Avatar::new(picture.map(Into::into))
                        .seed(group.key().cache_tag())
                        .large(),
                )
            }
            None => (SharedString::from("this group"), Avatar::new(None).large()),
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
            .gap_2()
            .px_3()
            .py_2()
            .text_xs()
            .text_color(cx.theme().muted_foreground);

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
                            .ghost()
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
            Row::Activity(activity) => {
                return message::render_activity(ix, activity, &self.directory, cx);
            }
        };

        let nostr = NostrRegistry::global(cx);
        let directory = self.directory.clone();

        let content = self
            .rendered_texts_by_id
            .entry(message.id)
            .or_insert_with(|| {
                rendered_text(
                    &message.content,
                    message
                        .mentions
                        .iter()
                        .map(|mention| (mention.range.clone(), mention.public_key)),
                    &message.emojis,
                    &directory,
                    cx,
                )
            })
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
            group.reactions(&message.id),
            message::Actions {
                group: self.group.clone(),
                panel: cx.weak_entity(),
                admin,
                pinned,
                persons: self.directory.clone(),
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
            .when_some(self.reply_to, |this, target| {
                this.child(self.render_reply_target(target, cx))
            })
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
                            .on_click(cx.listener(|this, _event, window, cx| {
                                this.upload(window, cx);
                            })),
                    )
                    .child(Textarea::new(&self.input).appearance(false).flex_1())
                    .child(
                        Button::new("send")
                            .icon(ui::IconName::PaperPlaneFill)
                            .tooltip("Send")
                            .ghost()
                            .on_click(cx.listener(|this, _event, window, cx| {
                                this.send(window, cx);
                            })),
                    ),
            )
            .into_any_element()
    }

    fn render_reply_target(&self, target: EventId, cx: &mut Context<Self>) -> AnyElement {
        let context = self.group.upgrade().and_then(|entity| {
            entity.read_with(cx, |group, cx| {
                let message = group.message(target)?;
                let author = self.directory.read(cx).person(&message.author, cx);
                let preview = message.preview();
                let line = preview.lines().next().unwrap_or_default().trim();

                Some((
                    author.name(),
                    SharedString::from(line.chars().take(80).collect::<String>()),
                ))
            })
        });

        let (author, snippet) = context.unwrap_or_else(|| {
            (
                SharedString::from("Message"),
                SharedString::from(target.to_hex().chars().take(8).collect::<String>()),
            )
        });

        h_flex()
            .min_w_0()
            .items_center()
            .gap_1()
            .px_2()
            .py_1()
            .rounded(cx.theme().radius)
            .bg(cx.theme().secondary)
            .text_xs()
            .child(
                div()
                    .flex_shrink_0()
                    .text_color(cx.theme().muted_foreground)
                    .child("Replying to"),
            )
            .child(
                div()
                    .flex_shrink_0()
                    .max_w(px(140.))
                    .truncate()
                    .font_semibold()
                    .text_color(cx.theme().foreground)
                    .child(author),
            )
            .child(
                div()
                    .min_w_0()
                    .flex_1()
                    .truncate()
                    .text_color(cx.theme().muted_foreground)
                    .child(snippet),
            )
            .child(
                Button::new("cancel-reply")
                    .icon(ui::IconName::Close)
                    .tooltip("Cancel reply")
                    .ghost()
                    .small()
                    .on_click(cx.listener(|this, _event, _window, cx| {
                        this.cancel_reply(cx);
                    })),
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
                    .border_color(cx.theme().border)
                    .rounded(cx.theme().radius)
                    .text_xs()
                    .text_color(cx.theme().muted_foreground)
                    .child(Icon::new(IconName::Link).xsmall())
                    .child(
                        div()
                            .max_w(px(200.))
                            .truncate()
                            .child(attachment_name(&url)),
                    )
                    .child(
                        Button::new(SharedString::from(format!("remove-attachment-{ix}")))
                            .icon(ui::IconName::Close)
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

impl gpui_kit::component::dock::BasePanel for GroupPanel {
    fn panel_name(&self) -> &'static str {
        "GroupPanel"
    }

    fn set_active(&mut self, active: bool, window: &mut Window, cx: &mut Context<Self>) {
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

    fn on_removed(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.set_active(false, window, cx);
    }
}

impl Panel for GroupPanel {
    fn title(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.group
            .upgrade()
            .map(|group| {
                let group = group.read(cx);

                h_flex()
                    .gap_1()
                    .text_xs()
                    .child(
                        Avatar::new(
                            group
                                .metadata()
                                .and_then(|metadata| metadata.picture.clone())
                                .map(Into::into),
                        )
                        .seed(group.key().cache_tag())
                        .xsmall(),
                    )
                    .child(display_name(group))
                    .into_any_element()
            })
            .unwrap_or_else(|| div().text_xs().child("Unknown").into_any_element())
    }

    fn toolbar_buttons(
        &mut self,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<Vec<Button>> {
        None
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
            .child(self.render_encryption_notice(cx))
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
                                        .text_xs()
                                        .text_color(cx.theme().muted_foreground)
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
