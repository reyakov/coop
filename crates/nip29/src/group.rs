use std::collections::{HashMap, HashSet};
use std::time::Duration;

use anyhow::{Result, anyhow, bail};
use chat::Message;
use gpui::{App, AppContext, Context, EventEmitter, SharedString, Task};
use nostr_sdk::prelude::*;
use smallvec::{SmallVec, smallvec};
use state::NostrRegistry;

use crate::LOCAL_KEYS;
use crate::protocol::{
    ACTIVITY_KINDS, Activity, GroupAdmins, GroupId, GroupKey, GroupMembers, GroupMetadata,
    GroupPins, GroupRoles, MEMBERSHIP_KINDS, Membership, PIN_LIST, Pin, RENDER_KINDS, STATE_KINDS,
    TimelineWindow, tag_value,
};

const REPLAY_LIMIT: usize = 100;
const PAGE_LIMIT: usize = 50;
const PAGE_TIMEOUT: Duration = Duration::from_secs(10);
const LEAVE_WINDOW_SECS: u64 = 5 * 60;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Absorbed {
    Ignored,
    Shown { fresh: bool, updated: bool },
    Updated,
}

#[derive(Debug, Clone)]
pub struct Reaction {
    pub id: EventId,
    pub emoji: String,
    pub author: PublicKey,
}

#[derive(Debug, Clone)]
pub struct Invite {
    pub id: EventId,
    pub code: String,
    pub created_at: Timestamp,
}

#[derive(Debug, Clone)]
pub enum Row {
    Message(Box<Message>),
    Activity(Activity),
}

impl Row {
    pub fn id(&self) -> EventId {
        match self {
            Self::Message(message) => message.id,
            Self::Activity(activity) => activity.id,
        }
    }

    pub fn author(&self) -> PublicKey {
        match self {
            Self::Message(message) => message.author,
            Self::Activity(activity) => activity.author,
        }
    }

    pub fn created_at(&self) -> Timestamp {
        match self {
            Self::Message(message) => message.created_at,
            Self::Activity(activity) => activity.created_at,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GroupEvent {
    Incoming(Event),
    Sent(Event),
    Updated,
    Eose,
    Refused(String),
}

pub struct Group {
    key: GroupKey,
    metadata: Option<GroupMetadata>,
    members: GroupMembers,
    admins: GroupAdmins,
    roles: GroupRoles,
    pins: GroupPins,
    membership: Membership,
    membership_events: Vec<Event>,
    elsewhere: Option<GroupKey>,
    state_ids: HashMap<Kind, EventId>,
    rows: Vec<Row>,
    index: HashSet<EventId>,
    reactions: HashMap<EventId, Vec<Reaction>>,
    /// Active invite codes; revocations arrive as 9005 deletions.
    invites: Vec<Invite>,
    leave_requests: HashMap<PublicKey, Timestamp>,
    window: TimelineWindow,
    oldest: Option<Timestamp>,
    loading: bool,
    loading_more: bool,
    active: bool,
    refused: Option<String>,
    tasks: SmallVec<[Task<()>; 2]>,
}

impl EventEmitter<GroupEvent> for Group {}

impl Group {
    pub(crate) fn new(key: GroupKey) -> Self {
        Self {
            key,
            metadata: None,
            members: GroupMembers::default(),
            admins: GroupAdmins::default(),
            roles: GroupRoles::default(),
            pins: GroupPins::default(),
            membership: Membership::Unknown,
            membership_events: Vec::new(),
            elsewhere: None,
            state_ids: HashMap::new(),
            rows: Vec::new(),
            index: HashSet::new(),
            reactions: HashMap::new(),
            invites: Vec::new(),
            leave_requests: HashMap::new(),
            window: TimelineWindow::new(),
            oldest: None,
            loading: true,
            loading_more: false,
            active: false,
            refused: None,
            tasks: smallvec![],
        }
    }

    pub fn key(&self) -> &GroupKey {
        &self.key
    }

    pub fn metadata(&self) -> Option<&GroupMetadata> {
        self.metadata.as_ref()
    }

    pub fn display_name(&self) -> SharedString {
        let name = self
            .metadata
            .as_ref()
            .and_then(|metadata| metadata.name())
            .unwrap_or_else(|| self.key.id().as_str());

        SharedString::from(name.to_owned())
    }

    pub fn display_image(&self) -> Option<SharedString> {
        self.metadata
            .as_ref()
            .and_then(|metadata| metadata.picture.clone())
            .map(SharedString::from)
    }

    pub fn total_members(&self) -> usize {
        self.members.len()
    }

    pub fn members(&self) -> &GroupMembers {
        &self.members
    }

    pub fn admins(&self) -> &GroupAdmins {
        &self.admins
    }

    pub fn roles(&self) -> &GroupRoles {
        &self.roles
    }

    pub fn pins(&self) -> &GroupPins {
        &self.pins
    }

    pub fn membership(&self) -> &Membership {
        &self.membership
    }

    pub fn elsewhere(&self) -> Option<&GroupKey> {
        self.elsewhere.as_ref()
    }

    pub fn rows(&self) -> &[Row] {
        &self.rows
    }

    pub fn message(&self, id: EventId) -> Option<&Message> {
        self.rows.iter().find_map(|row| match row {
            Row::Message(message) if message.id == id => Some(message.as_ref()),
            _ => None,
        })
    }

    pub fn reactions(&self, id: &EventId) -> &[Reaction] {
        self.reactions.get(id).map(Vec::as_slice).unwrap_or(&[])
    }

    pub fn invites(&self) -> &[Invite] {
        &self.invites
    }

    pub fn loading_more(&self) -> bool {
        self.loading_more
    }

    pub(crate) fn is_active(&self) -> bool {
        self.active
    }

    pub(crate) fn activate(&mut self) {
        self.active = true;
    }

    pub fn oldest(&self) -> Option<Timestamp> {
        self.oldest
    }

    pub fn refused(&self) -> Option<&str> {
        self.refused.as_deref()
    }

    pub fn send_message(
        &mut self,
        content: &str,
        replies: Vec<EventId>,
        cx: &mut Context<Self>,
    ) -> Task<Result<()>> {
        self.dispatch(move |id, prev| id.message(content, &replies, prev), cx)
    }

    pub fn send_reaction(
        &mut self,
        target: EventId,
        emoji: String,
        cx: &mut Context<Self>,
    ) -> Task<Result<()>> {
        self.dispatch(move |id, prev| id.reaction(target, &emoji, prev), cx)
    }

    pub fn remove_user(
        &mut self,
        public_key: PublicKey,
        cx: &mut Context<Self>,
    ) -> Task<Result<()>> {
        self.dispatch(move |id, prev| id.remove_user(public_key, prev), cx)
    }

    pub fn put_user(
        &mut self,
        public_key: PublicKey,
        roles: Vec<String>,
        cx: &mut Context<Self>,
    ) -> Task<Result<()>> {
        self.dispatch(move |id, prev| id.put_user(public_key, &roles, prev), cx)
    }

    pub fn edit_metadata(
        &mut self,
        metadata: &GroupMetadata,
        cx: &mut Context<Self>,
    ) -> Task<Result<()>> {
        self.dispatch(move |id, prev| id.edit_metadata(metadata, prev), cx)
    }

    pub fn delete_event(&mut self, id: EventId, cx: &mut Context<Self>) -> Task<Result<()>> {
        self.dispatch(move |gid, prev| gid.delete_event(id, prev), cx)
    }

    pub fn delete_group(&mut self, cx: &mut Context<Self>) -> Task<Result<()>> {
        self.dispatch(GroupId::delete_group, cx)
    }

    pub fn create_invite(&mut self, code: &str, cx: &mut Context<Self>) -> Task<Result<()>> {
        self.dispatch(move |id, prev| id.create_invite(code, prev), cx)
    }

    pub fn update_pins(&mut self, pins: Vec<Pin>, cx: &mut Context<Self>) -> Task<Result<()>> {
        self.dispatch(move |id, prev| id.update_pin_list(&pins, prev), cx)
    }

    fn dispatch<F>(&mut self, build: F, cx: &mut Context<Self>) -> Task<Result<()>>
    where
        F: FnOnce(&GroupId, Option<Tag>) -> EventBuilder,
    {
        let nostr = NostrRegistry::global(cx);

        let Some(me) = nostr.read(cx).current_user() else {
            return Task::ready(Err(anyhow!("nip29: no signed-in account")));
        };

        let builder = build(self.key.id(), self.window.previous_tag(me));
        let published = self.publish(builder, cx);

        cx.spawn(async move |this, cx| {
            let event = published.await?;

            this.update(cx, |_this, cx| {
                cx.emit(GroupEvent::Sent(event));
            })?;

            Ok(())
        })
    }

    pub(crate) fn publish(&self, builder: EventBuilder, cx: &App) -> Task<Result<Event>> {
        let nostr = NostrRegistry::global(cx).read(cx);
        let client = nostr.client();
        let signer = nostr.signer();
        let relay = self.key.relay().clone();

        cx.background_spawn(async move {
            if let Err(error) = client.add_relay(relay.clone()).and_connect().await {
                log::warn!("nip29: connecting {relay} failed: {error}");
            }

            let event = builder.finalize_async(&signer).await?;
            let output = client.send_event(&event).to([relay]).await?;

            if let Some(reason) = output.failed.into_values().next() {
                bail!("{reason}");
            }

            Ok(event)
        })
    }

    pub(crate) fn mark_pending(&mut self, since: Timestamp, cx: &mut Context<Self>) {
        if self.membership != Membership::Member {
            self.membership = Membership::Pending { since };
            cx.emit(GroupEvent::Updated);
            cx.notify();
        }
    }

    pub(crate) fn mark_member(&mut self, cx: &mut Context<Self>) {
        if self.membership == Membership::Member {
            return;
        }
        self.membership = Membership::Member;
        cx.emit(GroupEvent::Updated);
        cx.notify();
    }

    pub(crate) fn mark_refused(&mut self, reason: String, cx: &mut Context<Self>) {
        if self.membership == Membership::Member {
            return;
        }
        self.membership = Membership::Refused {
            reason: Some(reason),
        };
        cx.emit(GroupEvent::Updated);
        cx.notify();
    }

    pub(crate) fn set_elsewhere(&mut self, elsewhere: Option<GroupKey>, cx: &mut Context<Self>) {
        if self.elsewhere == elsewhere {
            return;
        }
        self.elsewhere = elsewhere;
        cx.emit(GroupEvent::Updated);
        cx.notify();
    }

    pub(crate) fn connect(&mut self, cx: &mut Context<Self>) {
        let nostr = NostrRegistry::global(cx);
        let client = nostr.read(cx).client();

        let Some(me) = nostr.read(cx).current_user() else {
            return;
        };

        let key = self.key.clone();
        let id = crate::subscription_id(&key);
        let relay = key.relay().clone();

        self.tasks.push(cx.background_spawn(async move {
            if let Err(error) = client.add_relay(relay.clone()).and_connect().await {
                log::warn!("nip29: connecting {relay} failed: {error}");
            }

            let filters = [
                key.id().timeline_filter(REPLAY_LIMIT),
                key.id().membership_filter(me),
                key.id().moderation_filter(REPLAY_LIMIT),
            ]
            .into_iter()
            .chain(key.id().state_filters())
            .collect::<Vec<_>>();

            if let Err(error) = client
                .subscribe(ReqTarget::single(relay, filters))
                .with_id(id)
                .await
            {
                log::warn!("nip29: subscribing to {} failed: {error}", key.id());
            }
        }));
    }

    pub(crate) fn load_page(&mut self, cx: &mut Context<Self>) {
        let Some(until) = self.oldest else {
            return;
        };

        self.loading_more = true;
        cx.notify();

        let nostr = NostrRegistry::global(cx);
        let client = nostr.read(cx).client();

        let id = crate::page_subscription(&self.key);
        let relay = self.key.relay().clone();
        let filter = self.key.id().timeline_page(PAGE_LIMIT, until);

        self.tasks.push(cx.background_spawn(async move {
            client.add_relay(relay.clone()).and_connect().await.ok();

            let opts = SubscribeAutoCloseOptions::default()
                .exit_policy(ReqExitPolicy::ExitOnEOSE)
                .timeout(Some(PAGE_TIMEOUT));

            if let Err(error) = client
                .subscribe(ReqTarget::single(relay, [filter]))
                .with_id(id)
                .close_on(opts)
                .await
            {
                log::warn!("nip29: loading older messages failed: {error}");
            }
        }));
    }

    pub(crate) fn page_finished(&mut self, cx: &mut Context<Self>) {
        self.loading_more = false;
        cx.emit(GroupEvent::Updated);
        cx.notify();
    }

    pub(crate) fn save_envelope(&self, event: Event, cx: &App) -> Task<Result<()>> {
        let cache_tag = self.key.cache_tag();
        let nostr = NostrRegistry::global(cx);
        let client = nostr.read(cx).client();

        cx.background_spawn(async move {
            let tags = vec![
                Tag::identifier(event.id),
                Tag::custom("r", [cache_tag]),
                Tag::custom("k", [event.kind.to_string()]),
            ];

            let envelope = EventBuilder::new(Kind::ApplicationSpecificData, event.as_json())
                .tags(tags)
                .finalize_async(&*LOCAL_KEYS)
                .await?;

            client.database().save_event(&envelope).await?;

            Ok(())
        })
    }

    pub(crate) fn load_envelopes(&self, cx: &App, state_only: bool) -> Task<Result<Vec<Event>>> {
        let cache_tag = self.key.cache_tag();
        let nostr = NostrRegistry::global(cx);
        let client = nostr.read(cx).client();

        cx.background_spawn(async move {
            let mut filter = Filter::new()
                .kind(Kind::ApplicationSpecificData)
                .custom_tag(SingleLetterTag::LOWERCASE_R, cache_tag);

            if state_only {
                let kinds = STATE_KINDS
                    .iter()
                    .chain(MEMBERSHIP_KINDS.iter())
                    .map(|kind| kind.to_string());

                filter = filter.custom_tags(SingleLetterTag::LOWERCASE_K, kinds);
            }

            let mut events: Vec<Event> = client
                .database()
                .query(filter)
                .await?
                .into_iter()
                .filter_map(|envelope| Event::from_json(&envelope.content).ok())
                .collect();

            events.sort_by_key(|event| event.created_at);

            Ok(events)
        })
    }

    pub(crate) fn hydrate(&mut self, events: Vec<Event>, me: PublicKey, cx: &mut Context<Self>) {
        if events.is_empty() {
            return;
        }

        let mut rendered = Vec::new();
        let mut changed = false;

        for event in &events {
            match self.absorb(event, me) {
                Absorbed::Shown {
                    fresh: true,
                    updated,
                } => {
                    rendered.push(event.clone());
                    changed |= updated;
                }
                Absorbed::Shown {
                    fresh: false,
                    updated,
                } => changed |= updated,
                Absorbed::Updated => changed = true,
                Absorbed::Ignored => {}
            }
        }

        for event in rendered {
            cx.emit(GroupEvent::Incoming(event));
        }

        if changed {
            cx.emit(GroupEvent::Updated);
        }

        cx.notify();
    }

    pub(crate) fn forget(&mut self, id: EventId, cx: &mut Context<Self>) {
        if self.retract(id) {
            cx.emit(GroupEvent::Updated);
            cx.notify();
        }
    }

    /// Remove an event and everything attached to it.
    fn retract(&mut self, id: EventId) -> bool {
        self.window.remove(id);

        if !self.index.remove(&id) {
            return false;
        }

        self.rows.retain(|row| row.id() != id);
        self.invites.retain(|invite| invite.id != id);
        self.reactions.remove(&id);

        for reactions in self.reactions.values_mut() {
            reactions.retain(|reaction| reaction.id != id);
        }

        self.reactions.retain(|_, reactions| !reactions.is_empty());

        true
    }

    pub(crate) fn eose(&mut self, cx: &mut Context<Self>) {
        self.loading = false;
        cx.emit(GroupEvent::Eose);
        cx.notify();
    }

    pub(crate) fn refuse(&mut self, reason: String, cx: &mut Context<Self>) {
        self.loading = false;
        self.refused = Some(reason.clone());
        cx.emit(GroupEvent::Refused(reason));
        cx.notify();
    }

    fn absorb(&mut self, event: &Event, me: PublicKey) -> Absorbed {
        if STATE_KINDS.contains(&event.kind) {
            self.window.note(event);

            return if self.absorb_state(event, me) {
                Absorbed::Updated
            } else {
                Absorbed::Ignored
            };
        }

        // NIP-29: relays must reject events whose
        // `previous` references are absent from their database.
        let previous = event.tags.iter().find(|tag| tag.kind() == "previous");
        if self.window.references_seen(previous) == Some(false) {
            log::warn!(
                "nip29: event {} carries previous references not seen on {}",
                event.id,
                self.key.relay()
            );
        }

        self.track(event);

        let updated = self.absorb_membership(event, me);

        if ACTIVITY_KINDS.contains(&event.kind) {
            return Absorbed::Shown {
                fresh: self.push_activity(event),
                updated,
            };
        }

        if event.kind == Kind::Reaction {
            return if self.push_reaction(event) {
                Absorbed::Updated
            } else {
                Absorbed::Ignored
            };
        }

        if event.kind == Kind::GroupCreateInvite {
            return if self.push_invite(event) {
                Absorbed::Updated
            } else {
                Absorbed::Ignored
            };
        }

        if event.kind == Kind::GroupDeleteEvent {
            let retracted = event
                .tags
                .event_ids()
                .next()
                .is_some_and(|target| self.retract(target));

            return if retracted {
                Absorbed::Updated
            } else {
                Absorbed::Ignored
            };
        }

        if RENDER_KINDS.contains(&event.kind) {
            return Absorbed::Shown {
                fresh: self.push_row(event),
                updated,
            };
        }

        if updated {
            Absorbed::Updated
        } else {
            Absorbed::Ignored
        }
    }

    fn absorb_membership(&mut self, event: &Event, me: PublicKey) -> bool {
        if !MEMBERSHIP_KINDS.contains(&event.kind) {
            return false;
        }

        if !event.tags.public_keys().any(|key| key == me) {
            return false;
        }

        if !self
            .membership_events
            .iter()
            .any(|existing| existing.id == event.id)
        {
            self.membership_events.push(event.clone());
        }

        self.refresh_membership(me)
    }

    fn track(&mut self, event: &Event) {
        self.window.push(event.clone());

        if self.oldest.is_none_or(|oldest| event.created_at < oldest) {
            self.oldest = Some(event.created_at);
        }
    }

    fn absorb_state(&mut self, event: &Event, me: PublicKey) -> bool {
        match event.kind {
            Kind::GroupMetadata => match GroupMetadata::parse(event) {
                Ok(metadata) => self.metadata = Some(metadata),
                Err(_) => return false,
            },
            Kind::GroupAdmins => match GroupAdmins::parse(event) {
                Ok(admins) => {
                    self.admins = admins;
                    self.adopt_roster_membership(me);
                }
                Err(_) => return false,
            },
            Kind::GroupMembers => match GroupMembers::parse(event) {
                Ok(members) => {
                    self.members = members;
                    self.adopt_roster_membership(me);
                }
                Err(_) => return false,
            },
            Kind::GroupRoles => match GroupRoles::parse(event) {
                Ok(roles) => self.roles = roles,
                Err(_) => return false,
            },
            Kind::Custom(PIN_LIST) => match GroupPins::parse(event) {
                Ok(pins) => self.pins = pins,
                Err(_) => return false,
            },
            _ => return false,
        }

        self.state_ids.insert(event.kind, event.id) != Some(event.id)
    }

    fn adopt_roster_membership(&mut self, me: PublicKey) -> bool {
        if self.membership != Membership::Unknown {
            return false;
        }

        let listed = self.admins.contains(&me) || self.members.iter().any(|member| *member == me);

        if !listed {
            return false;
        }

        self.membership = Membership::Member;
        true
    }

    fn push_row(&mut self, event: &Event) -> bool {
        if !self.index.insert(event.id) {
            return false;
        }

        let message = Message::from(event);
        let position = self
            .rows
            .partition_point(|row| row.created_at() <= message.created_at);

        self.rows.insert(position, Row::Message(Box::new(message)));

        true
    }

    fn push_reaction(&mut self, event: &Event) -> bool {
        let Some(target) = event.tags.event_ids().next() else {
            return false;
        };

        if !self.index.insert(event.id) {
            return false;
        }

        self.reactions.entry(target).or_default().push(Reaction {
            id: event.id,
            emoji: event.content.clone(),
            author: event.pubkey,
        });

        true
    }

    fn push_invite(&mut self, event: &Event) -> bool {
        let Some(code) = tag_value(&event.tags, "code") else {
            return false;
        };

        if !self.index.insert(event.id) {
            return false;
        }

        self.invites.push(Invite {
            id: event.id,
            code: code.to_owned(),
            created_at: event.created_at,
        });

        true
    }

    fn push_activity(&mut self, event: &Event) -> bool {
        let activity = match Activity::parse(event) {
            Ok(activity) => activity,
            Err(error) => {
                log::warn!("nip29: ignoring a malformed group activity: {error}");
                return false;
            }
        };

        if activity.kind == Kind::GroupPutUser && activity.roles.is_empty() {
            return false;
        }

        if activity.kind == Kind::GroupRemoveUser
            && self
                .leave_requests
                .get(&activity.subject())
                .is_some_and(|left| {
                    activity.created_at >= *left
                        && activity.created_at.as_secs().saturating_sub(left.as_secs())
                            <= LEAVE_WINDOW_SECS
                })
        {
            return false;
        }

        if !self.index.insert(activity.id) {
            return false;
        }

        if activity.kind == Kind::GroupLeaveRequest {
            self.leave_requests
                .insert(activity.subject(), activity.created_at);
        }

        let position = self
            .rows
            .partition_point(|row| row.created_at() <= activity.created_at);

        self.rows.insert(position, Row::Activity(activity));

        true
    }

    fn refresh_membership(&mut self, me: PublicKey) -> bool {
        let derived = Membership::from_events(&self.membership_events, me);

        if derived != Membership::Unknown && derived != self.membership {
            self.membership = derived;
            return true;
        }

        false
    }
}
