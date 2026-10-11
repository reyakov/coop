use std::collections::hash_map::DefaultHasher;
use std::fmt;
use std::hash::{Hash, Hasher};
use std::str::FromStr;

use anyhow::{Result, anyhow, bail};
use nostr_sdk::prelude::*;

pub(crate) const TIMELINE_WINDOW: usize = 50;

const PREVIOUS_REFS: usize = 3;
const PREVIOUS_REF_LEN: usize = 8;

pub(crate) const RENDER_KINDS: [Kind; 5] = [
    Kind::TextNote,
    Kind::ChatMessage,
    Kind::Thread,
    Kind::Comment,
    Kind::Reaction,
];

pub(crate) const PIN_LIST: u16 = 39_005;
pub(crate) const UPDATE_PIN_LIST: u16 = 9_010;

pub(crate) const STATE_KINDS: [Kind; 5] = [
    Kind::GroupMetadata,
    Kind::GroupAdmins,
    Kind::GroupMembers,
    Kind::GroupRoles,
    Kind::Custom(PIN_LIST),
];

pub(crate) const MEMBERSHIP_KINDS: [Kind; 2] = [Kind::GroupPutUser, Kind::GroupRemoveUser];

pub(crate) const ACTIVITY_KINDS: [Kind; 4] = [
    Kind::GroupPutUser,
    Kind::GroupRemoveUser,
    Kind::GroupJoinRequest,
    Kind::GroupLeaveRequest,
];

fn has_tag(tags: &Tags, name: &str) -> bool {
    tags.iter().any(|tag| tag.kind() == name)
}

pub(crate) fn tag_value<'a>(tags: &'a Tags, name: &str) -> Option<&'a str> {
    tags.iter()
        .find(|tag| tag.kind() == name)
        .and_then(|tag| tag.content())
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct GroupId(String);

impl GroupId {
    pub fn new(id: impl Into<String>) -> Result<Self> {
        let id = id.into();
        let id = id.trim();

        if id.is_empty() {
            bail!("group id is empty");
        }

        Ok(Self(id.to_owned()))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    pub fn h_tag(&self) -> Tag {
        Tag::custom("h", [self.0.clone()])
    }

    pub fn timeline_filter(&self, limit: usize) -> Filter {
        Filter::new()
            .custom_tag(SingleLetterTag::LOWERCASE_H, self.0.clone())
            .kinds(RENDER_KINDS)
            .kinds(ACTIVITY_KINDS)
            .limit(limit)
    }

    pub fn timeline_page(&self, limit: usize, until: Timestamp) -> Filter {
        self.timeline_filter(limit).until(until)
    }

    pub fn state_filters(&self) -> Vec<Filter> {
        STATE_KINDS
            .iter()
            .map(|kind| Filter::new().identifier(self.0.clone()).kind(*kind))
            .collect()
    }

    pub fn membership_filter(&self, me: PublicKey) -> Filter {
        Filter::new()
            .custom_tag(SingleLetterTag::LOWERCASE_H, self.0.clone())
            .custom_tag(SingleLetterTag::LOWERCASE_P, me.to_hex())
            .kinds(MEMBERSHIP_KINDS)
    }

    pub fn moderation_filter(&self, limit: usize) -> Filter {
        Filter::new()
            .custom_tag(SingleLetterTag::LOWERCASE_H, self.0.clone())
            .kinds([Kind::GroupCreateInvite, Kind::GroupDeleteEvent])
            .limit(limit)
    }

    pub fn message(
        &self,
        content: &str,
        replies: &[EventId],
        previous: Option<Tag>,
    ) -> EventBuilder {
        EventBuilder::new(Kind::ChatMessage, content)
            .tag(self.h_tag())
            .tags(replies.iter().copied().map(Tag::event))
            .tag_maybe(previous)
    }

    pub fn reaction(&self, target: EventId, emoji: &str, previous: Option<Tag>) -> EventBuilder {
        EventBuilder::new(Kind::Reaction, emoji)
            .tag(self.h_tag())
            .tag(Tag::event(target))
            .tag_maybe(previous)
    }

    pub fn join_request(&self, code: Option<&str>, reason: &str) -> EventBuilder {
        EventBuilder::new(Kind::GroupJoinRequest, reason)
            .tag(self.h_tag())
            .tag_maybe(code.map(|code| Tag::custom("code", [code])))
    }

    pub fn leave_request(&self, reason: &str) -> EventBuilder {
        EventBuilder::new(Kind::GroupLeaveRequest, reason).tag(self.h_tag())
    }

    pub fn remove_user(&self, public_key: PublicKey, previous: Option<Tag>) -> EventBuilder {
        EventBuilder::new(Kind::GroupRemoveUser, "")
            .tag(self.h_tag())
            .tag(Tag::public_key(public_key))
            .tag_maybe(previous)
    }

    pub fn put_user(
        &self,
        public_key: PublicKey,
        roles: &[String],
        previous: Option<Tag>,
    ) -> EventBuilder {
        EventBuilder::new(Kind::GroupPutUser, "")
            .tag(self.h_tag())
            .tag(Tag::custom(
                "p",
                std::iter::once(public_key.to_hex()).chain(roles.iter().cloned()),
            ))
            .tag_maybe(previous)
    }

    pub fn edit_metadata(&self, metadata: &GroupMetadata, previous: Option<Tag>) -> EventBuilder {
        EventBuilder::new(Kind::GroupEditMetadata, "")
            .tags(self.metadata_tags(metadata))
            .tag_maybe(previous)
    }

    pub fn create_group(&self, previous: Option<Tag>) -> EventBuilder {
        EventBuilder::new(Kind::GroupCreateGroup, "")
            .tag(self.h_tag())
            .tag_maybe(previous)
    }

    fn metadata_tags(&self, metadata: &GroupMetadata) -> Vec<Tag> {
        let mut tags = vec![self.h_tag()];

        for (name, value) in [
            ("name", &metadata.name),
            ("picture", &metadata.picture),
            ("banner", &metadata.banner),
            ("about", &metadata.about),
        ] {
            if let Some(value) = value {
                tags.push(Tag::custom(name, [value.clone()]));
            }
        }

        for (name, set) in [
            ("private", metadata.private),
            ("restricted", metadata.restricted),
            ("hidden", metadata.hidden),
            ("closed", metadata.closed),
            ("livekit", metadata.livekit),
        ] {
            if set {
                tags.push(Tag::custom(name, Vec::<String>::new()));
            }
        }

        if let Some(parent) = &metadata.parent {
            tags.push(Tag::custom("parent", [parent.as_str()]));
        }

        for child in &metadata.children {
            tags.push(Tag::custom("child", [child.as_str()]));
        }

        if let Some(kinds) = &metadata.supported_kinds {
            tags.push(Tag::custom(
                "supported_kinds",
                kinds.iter().map(ToString::to_string),
            ));
        }

        tags
    }

    pub fn delete_event(&self, id: EventId, previous: Option<Tag>) -> EventBuilder {
        EventBuilder::new(Kind::GroupDeleteEvent, "")
            .tag(self.h_tag())
            .tag(Tag::event(id))
            .tag_maybe(previous)
    }

    pub fn delete_group(&self, previous: Option<Tag>) -> EventBuilder {
        EventBuilder::new(Kind::GroupDeleteGroup, "")
            .tag(self.h_tag())
            .tag_maybe(previous)
    }

    pub fn create_invite(&self, code: &str, previous: Option<Tag>) -> EventBuilder {
        EventBuilder::new(Kind::GroupCreateInvite, "")
            .tag(self.h_tag())
            .tag(Tag::custom("code", [code]))
            .tag_maybe(previous)
    }

    pub fn update_pin_list(&self, pins: &[Pin], previous: Option<Tag>) -> EventBuilder {
        EventBuilder::new(Kind::Custom(UPDATE_PIN_LIST), "")
            .tag(self.h_tag())
            .tags(pins.iter().map(Pin::tag))
            .tag_maybe(previous)
    }
}

impl fmt::Display for GroupId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct GroupKey {
    relay: RelayUrl,
    id: GroupId,
}

impl GroupKey {
    pub fn new(relay: RelayUrl, id: GroupId) -> Self {
        Self { relay, id }
    }

    pub fn relay(&self) -> &RelayUrl {
        &self.relay
    }

    pub fn id(&self) -> &GroupId {
        &self.id
    }

    pub fn cache_tag(&self) -> String {
        format!("{}|{}", self.relay.as_str(), self.id)
    }

    pub fn uniq_id(&self) -> u64 {
        let mut hasher = DefaultHasher::new();
        self.hash(&mut hasher);
        hasher.finish()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GroupReference {
    id: GroupId,
    relay: RelayUrl,
    invite: Option<String>,
}

impl GroupReference {
    pub fn parse(input: &str) -> Result<Self> {
        let input = input.trim();

        let (address, query) = match input.split_once('?') {
            Some((address, query)) => (address, Some(query)),
            None => (input, None),
        };

        let coordinate = Nip19Coordinate::from_bech32(address)?;

        if coordinate.kind != Kind::GroupMetadata {
            bail!("not a group reference: kind {}", coordinate.kind);
        }

        let relay = coordinate
            .relays
            .first()
            .cloned()
            .ok_or_else(|| anyhow!("group reference has no relay hint"))?;

        let id = GroupId::new(coordinate.identifier.as_str())?;

        let invite = query
            .and_then(|query| {
                query
                    .split('&')
                    .find_map(|pair| pair.strip_prefix("invite="))
            })
            .map(str::to_owned);

        Ok(Self { id, relay, invite })
    }

    pub fn invite(&self) -> Option<&str> {
        self.invite.as_deref()
    }

    pub fn key(&self) -> GroupKey {
        GroupKey::new(self.relay.clone(), self.id.clone())
    }
}

impl FromStr for GroupReference {
    type Err = anyhow::Error;

    fn from_str(input: &str) -> Result<Self> {
        Self::parse(input)
    }
}

#[derive(Debug, Clone, Default)]
pub struct GroupMetadata {
    pub name: Option<String>,
    pub picture: Option<String>,
    pub(crate) banner: Option<String>,
    pub about: Option<String>,
    pub private: bool,
    pub restricted: bool,
    pub hidden: bool,
    pub closed: bool,
    pub livekit: bool,
    pub parent: Option<GroupId>,
    pub children: Vec<GroupId>,
    pub supported_kinds: Option<Vec<Kind>>,
    id: Option<GroupId>,
    author: Option<PublicKey>,
}

impl GroupMetadata {
    pub fn parse(event: &Event) -> Result<Self> {
        if event.kind != Kind::GroupMetadata {
            bail!("not a group metadata event: kind {}", event.kind);
        }

        let identifier = event
            .tags
            .identifier()
            .ok_or_else(|| anyhow!("group metadata has no d tag"))?;

        let content: serde_json::Value =
            serde_json::from_str(event.content.as_str()).unwrap_or(serde_json::Value::Null);

        let field = |key: &str| -> Option<String> {
            content
                .get(key)
                .and_then(serde_json::Value::as_str)
                .map(str::to_owned)
                .or_else(|| tag_value(&event.tags, key).map(str::to_owned))
        };

        let mut supported_kinds: Option<Vec<Kind>> = None;

        for tag in event
            .tags
            .iter()
            .filter(|tag| tag.kind() == "supported_kinds")
        {
            let kinds = supported_kinds.get_or_insert_with(Vec::new);

            for value in tag.as_slice().iter().skip(1) {
                kinds.push(Kind::from_str(value)?);
            }
        }

        let parent = match tag_value(&event.tags, "parent") {
            Some(parent) => Some(GroupId::new(parent)?),
            None => None,
        };

        let mut children = Vec::new();

        for tag in event.tags.iter().filter(|tag| tag.kind() == "child") {
            if let Some(child) = tag.content() {
                children.push(GroupId::new(child)?);
            }
        }

        Ok(Self {
            name: field("name"),
            picture: field("picture"),
            banner: field("banner"),
            about: field("about"),
            private: has_tag(&event.tags, "private"),
            restricted: has_tag(&event.tags, "restricted"),
            hidden: has_tag(&event.tags, "hidden"),
            closed: has_tag(&event.tags, "closed"),
            livekit: has_tag(&event.tags, "livekit"),
            parent,
            children,
            supported_kinds,
            id: Some(GroupId::new(identifier)?),
            author: Some(event.pubkey),
        })
    }

    pub(crate) fn id(&self) -> Option<&GroupId> {
        self.id.as_ref()
    }

    pub fn name(&self) -> Option<&str> {
        self.name.as_deref()
    }

    pub fn supports(&self, kind: Kind) -> bool {
        match &self.supported_kinds {
            None => true,
            Some(kinds) => kinds.contains(&kind),
        }
    }

    pub fn naddr(&self, relay: &RelayUrl, invite: Option<&str>) -> Result<String> {
        let id = self.id.as_ref().ok_or_else(|| anyhow!("no id"))?;
        let author = self.author.ok_or_else(|| anyhow!("no author"))?;

        let coordinate = Coordinate::new(Kind::GroupMetadata, author).identifier(id.as_str());
        let address = Nip19Coordinate::new(coordinate, [relay.clone()]).to_bech32()?;

        Ok(match invite {
            Some(invite) => format!("{address}?invite={invite}"),
            None => address,
        })
    }
}

/// Input for creating a group: access flags are presence-only per NIP-29.
#[derive(Debug, Clone, Default)]
pub struct GroupCreateOptions {
    pub name: String,
    pub about: Option<String>,
    pub picture: Option<String>,
    pub private: bool,
    pub closed: bool,
    pub restricted: bool,
    pub hidden: bool,
    /// List the group in the NIP-44 encrypted section of the user's group list.
    pub list_private: bool,
    pub custom_id: Option<String>,
    pub parent: Option<GroupId>,
}

impl GroupCreateOptions {
    pub(crate) fn metadata(&self, name: String) -> GroupMetadata {
        GroupMetadata {
            name: Some(name),
            picture: self.picture.clone(),
            about: self.about.clone(),
            private: self.private,
            restricted: self.restricted,
            hidden: self.hidden,
            closed: self.closed,
            parent: self.parent.clone(),
            ..Default::default()
        }
    }
}

#[derive(Debug, Clone)]
pub struct GroupCandidate {
    pub key: GroupKey,
    /// The timestamp of the metadata event the candidate was built from.
    pub(crate) created_at: Timestamp,
    pub(crate) metadata: GroupMetadata,
}

impl GroupCandidate {
    pub fn display_name(&self) -> &str {
        self.metadata
            .name()
            .unwrap_or_else(|| self.key.id().as_str())
    }

    pub fn display_image(&self) -> Option<&str> {
        self.metadata.picture.as_deref()
    }

    pub fn display_about(&self) -> Option<&str> {
        self.metadata.about.as_deref()
    }
}

#[derive(Debug, Clone, Default)]
pub struct GroupAdmins(Vec<(PublicKey, Vec<String>)>);

impl GroupAdmins {
    pub fn parse(event: &Event) -> Result<Self> {
        if event.kind != Kind::GroupAdmins {
            bail!("not a group admins event: kind {}", event.kind);
        }

        let mut admins = Vec::new();

        for tag in event.tags.iter().filter(|tag| tag.kind() == "p") {
            let mut fields = tag.as_slice().iter().skip(1);

            let public_key = fields
                .next()
                .ok_or_else(|| anyhow!("admin tag has no public key"))?;
            let public_key = PublicKey::from_hex(public_key)?;

            admins.push((public_key, fields.cloned().collect()));
        }

        Ok(Self(admins))
    }

    pub fn entries(&self) -> &[(PublicKey, Vec<String>)] {
        &self.0
    }

    pub fn contains(&self, public_key: &PublicKey) -> bool {
        self.0.iter().any(|(key, _)| key == public_key)
    }
}

#[derive(Debug, Clone, Default)]
pub struct GroupMembers(Vec<PublicKey>);

impl GroupMembers {
    pub fn parse(event: &Event) -> Result<Self> {
        if event.kind != Kind::GroupMembers {
            bail!("not a group members event: kind {}", event.kind);
        }

        Ok(Self(event.tags.public_keys().collect()))
    }

    pub fn iter(&self) -> impl Iterator<Item = &PublicKey> {
        self.0.iter()
    }

    pub fn len(&self) -> usize {
        self.0.len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Role {
    pub name: String,
}

#[derive(Debug, Clone, Default)]
pub struct GroupRoles(Vec<Role>);

impl GroupRoles {
    pub fn parse(event: &Event) -> Result<Self> {
        if event.kind != Kind::GroupRoles {
            bail!("not a group roles event: kind {}", event.kind);
        }

        let mut roles = Vec::new();

        for tag in event.tags.iter().filter(|tag| tag.kind() == "role") {
            let mut fields = tag.as_slice().iter().skip(1);
            let name = fields
                .next()
                .ok_or_else(|| anyhow!("role tag has no name"))?;

            roles.push(Role { name: name.clone() });
        }

        Ok(Self(roles))
    }

    pub fn roles(&self) -> &[Role] {
        &self.0
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Pin {
    Event(EventId),
    Address(Coordinate),
}

impl Pin {
    fn tag(&self) -> Tag {
        match self {
            Pin::Event(id) => Tag::event(*id),
            Pin::Address(coordinate) => Tag::coordinate(coordinate.clone(), None),
        }
    }
}

#[derive(Debug, Clone, Default)]
pub struct GroupPins(Vec<Pin>);

impl GroupPins {
    pub fn parse(event: &Event) -> Result<Self> {
        if event.kind != Kind::Custom(PIN_LIST) {
            bail!("not a pinned list event: kind {}", event.kind);
        }

        let mut pins = Vec::new();

        for tag in event.tags.iter() {
            match tag.kind() {
                "e" => {
                    let Some(id) = tag.content() else {
                        continue;
                    };

                    pins.push(Pin::Event(EventId::from_hex(id)?));
                }
                "a" => {
                    let Some(address) = tag.content() else {
                        continue;
                    };

                    pins.push(Pin::Address(Coordinate::from_str(address)?));
                }
                _ => {}
            }
        }

        Ok(Self(pins))
    }

    pub fn pins(&self) -> &[Pin] {
        &self.0
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct GroupListEntry {
    pub id: GroupId,
    pub relay: RelayUrl,
    pub name: Option<String>,
    /// Kept in the encrypted section of the list event, not in public tags.
    pub private: bool,
}

#[derive(Debug, Clone, Default)]
pub(crate) struct GroupList {
    entries: Vec<GroupListEntry>,
    /// The raw private section of the last seen list event.
    content: Option<String>,
    /// Whether the content was decrypted in this session; an opaque section
    /// (another client's encryption, or a refused decrypt) is carried verbatim.
    readable: bool,
}

impl GroupList {
    pub fn parse(event: &Event) -> Result<Self> {
        if event.kind != Kind::SimpleGroups {
            bail!("not a group list event: kind {}", event.kind);
        }

        let mut entries = Vec::new();

        for tag in event.tags.iter().filter(|tag| tag.kind() == "group") {
            let fields = tag.as_slice();

            let (Some(id), Some(relay)) = (fields.get(1), fields.get(2)) else {
                continue;
            };

            let Ok(id) = GroupId::new(id.as_str()) else {
                continue;
            };
            let Ok(relay) = RelayUrl::parse(relay.as_str()) else {
                continue;
            };

            entries.push(GroupListEntry {
                id,
                relay,
                name: fields.get(3).cloned(),
                private: false,
            });
        }

        let content = if event.content.is_empty() {
            None
        } else {
            Some(event.content.clone())
        };

        Ok(Self {
            entries,
            readable: content.is_none(),
            content,
        })
    }

    pub fn entries(&self) -> &[GroupListEntry] {
        &self.entries
    }

    pub fn entry(&self, key: &GroupKey) -> Option<&GroupListEntry> {
        self.entries
            .iter()
            .find(|entry| entry.id == *key.id() && entry.relay == *key.relay())
    }

    pub fn upsert(&mut self, entry: GroupListEntry) {
        match self
            .entries
            .iter_mut()
            .find(|existing| existing.id == entry.id && existing.relay == entry.relay)
        {
            Some(existing) => *existing = entry,
            None => self.entries.push(entry),
        }
    }

    pub fn remove(&mut self, key: &GroupKey) {
        self.entries
            .retain(|entry| entry.id != *key.id() || entry.relay != *key.relay());
    }

    /// The raw private section of the list event, if any.
    pub(crate) fn content(&self) -> Option<&str> {
        self.content.as_deref()
    }

    pub(crate) fn content_readable(&self) -> bool {
        self.readable
    }

    pub(crate) fn set_content(&mut self, content: Option<String>, readable: bool) {
        self.content = content;
        self.readable = readable;
    }

    pub(crate) fn has_private(&self) -> bool {
        self.entries.iter().any(|entry| entry.private)
    }

    /// Mark the private section as readable after a successful decrypt.
    pub(crate) fn mark_readable(&mut self) {
        self.readable = true;
    }

    /// Add decrypted private entries; the public listing wins on collision.
    pub(crate) fn merge_private(&mut self, entries: Vec<GroupListEntry>) {
        for entry in entries {
            let public = self.entries.iter().any(|existing| {
                !existing.private && existing.id == entry.id && existing.relay == entry.relay
            });

            if !public {
                self.upsert(entry);
            }
        }
    }

    /// The tags of the public section of the list event.
    pub(crate) fn public_tags(&self) -> Vec<Tag> {
        let mut tags = Vec::with_capacity(self.entries.len() * 2);

        for entry in &self.entries {
            if entry.private {
                continue;
            }

            let mut values = vec![
                entry.id.as_str().to_owned(),
                entry.relay.as_str().to_owned(),
            ];

            if let Some(name) = &entry.name {
                values.push(name.clone());
            }

            tags.push(Tag::custom("group", values));
            tags.push(Tag::custom("r", [entry.relay.as_str()]));
        }

        tags
    }

    /// The private entries encoded as a tag array, ready for encryption.
    pub(crate) fn encode_private(&self) -> Option<String> {
        let tags: Vec<Vec<String>> = self
            .entries
            .iter()
            .filter(|entry| entry.private)
            .map(|entry| {
                let mut values = vec![
                    entry.id.as_str().to_owned(),
                    entry.relay.as_str().to_owned(),
                ];

                if let Some(name) = &entry.name {
                    values.push(name.clone());
                }

                values
            })
            .collect();

        if tags.is_empty() {
            None
        } else {
            serde_json::to_string(&tags).ok()
        }
    }
}

/// Parse a decrypted private section; `None` when it isn't a tag array.
pub(crate) fn parse_private_section(plaintext: &str) -> Option<Vec<GroupListEntry>> {
    let tags = serde_json::from_str::<Vec<Vec<String>>>(plaintext).ok()?;

    let mut entries = Vec::new();

    for tag in tags {
        if tag.first().map(String::as_str) != Some("group") {
            continue;
        }

        let (Some(id), Some(relay)) = (tag.get(1), tag.get(2)) else {
            continue;
        };

        let Ok(id) = GroupId::new(id) else {
            continue;
        };
        let Ok(relay) = RelayUrl::parse(relay) else {
            continue;
        };

        entries.push(GroupListEntry {
            id,
            relay,
            name: tag.get(3).cloned(),
            private: true,
        });
    }

    Some(entries)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Membership {
    Unknown,
    Pending { since: Timestamp },
    Member,
    Refused { reason: Option<String> },
    Removed,
}

impl Membership {
    pub fn status(&self) -> Option<&'static str> {
        match self {
            Self::Pending { .. } => Some("Requested"),
            Self::Refused { .. } => Some("Refused"),
            Self::Removed => Some("Removed"),
            Self::Member | Self::Unknown => None,
        }
    }

    pub fn from_events(events: &[Event], me: PublicKey) -> Self {
        let mut latest: Option<&Event> = None;

        for event in events {
            if !MEMBERSHIP_KINDS.contains(&event.kind) {
                continue;
            }

            if !event.tags.public_keys().any(|key| key == me) {
                continue;
            }

            let newer = match latest {
                None => true,
                Some(current) => {
                    event.created_at > current.created_at
                        || (event.created_at == current.created_at
                            && current.kind == Kind::GroupPutUser
                            && event.kind == Kind::GroupRemoveUser)
                }
            };

            if newer {
                latest = Some(event);
            }
        }

        match latest {
            Some(event) if event.kind == Kind::GroupRemoveUser => Membership::Removed,
            Some(_) => Membership::Member,
            None => Membership::Unknown,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Activity {
    pub id: EventId,
    pub kind: Kind,
    pub author: PublicKey,
    pub target: Option<PublicKey>,
    pub roles: Vec<String>,
    pub created_at: Timestamp,
}

impl Activity {
    pub fn parse(event: &Event) -> Result<Self> {
        if !ACTIVITY_KINDS.contains(&event.kind) {
            bail!("not a group activity event: kind {}", event.kind);
        }

        let mut fields = event
            .tags
            .iter()
            .find(|tag| tag.kind() == "p")
            .map(|tag| tag.as_slice().iter().skip(1))
            .into_iter()
            .flatten();

        let target = match fields.next() {
            Some(value) => Some(PublicKey::from_hex(value)?),
            None => None,
        };

        if target.is_none() && MEMBERSHIP_KINDS.contains(&event.kind) {
            bail!("a membership event has no p tag");
        }

        Ok(Self {
            id: event.id,
            kind: event.kind,
            author: event.pubkey,
            target,
            roles: fields.cloned().collect(),
            created_at: event.created_at,
        })
    }

    pub fn subject(&self) -> PublicKey {
        match self.kind {
            Kind::GroupPutUser | Kind::GroupRemoveUser => self.target.unwrap_or(self.author),
            _ => self.author,
        }
    }
}

#[derive(Debug, Clone, Default)]
pub(crate) struct TimelineWindow(Vec<Event>);

impl TimelineWindow {
    pub fn new() -> Self {
        Self(Vec::new())
    }

    pub fn push(&mut self, event: Event) {
        self.0.retain(|existing| existing.id != event.id);
        let position = self.0.partition_point(|e| e.created_at > event.created_at);

        self.0.insert(position, event);
        self.0.truncate(TIMELINE_WINDOW);
    }

    pub fn remove(&mut self, id: EventId) {
        self.0.retain(|event| event.id != id);
    }

    pub fn previous_tag(&self, me: PublicKey) -> Option<Tag> {
        let references: Vec<String> = self
            .0
            .iter()
            .filter(|event| event.pubkey != me)
            .take(PREVIOUS_REFS)
            .map(|event| event.id.to_hex().chars().take(PREVIOUS_REF_LEN).collect())
            .collect();

        if references.is_empty() {
            None
        } else {
            Some(Tag::custom("previous", references))
        }
    }
}
