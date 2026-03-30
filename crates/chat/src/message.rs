use std::hash::Hash;
use std::ops::Range;

use common::{EventUtils, NostrParser};
use gpui::SharedString;
use nostr_sdk::prelude::*;

/// New message.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct NewMessage {
    pub room: u64,
    pub gift_wrap: EventId,
    pub rumor: UnsignedEvent,
}

impl NewMessage {
    pub fn new(gift_wrap: EventId, rumor: UnsignedEvent) -> Self {
        let room = rumor.uniq_id();

        Self {
            room,
            gift_wrap,
            rumor,
        }
    }
}

/// Trash message.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct FailedMessage {
    pub raw_event: SharedString,
    pub reason: SharedString,
}

impl FailedMessage {
    pub fn new<T>(event: &Event, reason: T) -> Self
    where
        T: Into<SharedString>,
    {
        Self {
            raw_event: SharedString::from(event.as_json()),
            reason: reason.into(),
        }
    }
}

/// Message.
#[derive(Debug, Clone, Hash, PartialEq, Eq)]
pub enum Message {
    User(RenderedMessage),
    Warning(String, Timestamp),
    System(Timestamp),
}

impl Message {
    pub fn user<I>(user: I) -> Self
    where
        I: Into<RenderedMessage>,
    {
        Self::User(user.into())
    }

    pub fn warning<I>(content: I) -> Self
    where
        I: Into<String>,
    {
        Self::Warning(content.into(), Timestamp::now())
    }

    pub fn system() -> Self {
        Self::System(Timestamp::default())
    }

    fn timestamp(&self) -> &Timestamp {
        match self {
            Message::User(msg) => &msg.created_at,
            Message::Warning(_, ts) => ts,
            Message::System(ts) => ts,
        }
    }
}

impl From<&NewMessage> for Message {
    fn from(val: &NewMessage) -> Self {
        Self::User(val.into())
    }
}

impl From<&UnsignedEvent> for Message {
    fn from(val: &UnsignedEvent) -> Self {
        Self::User(val.into())
    }
}

impl Ord for Message {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        match (self, other) {
            // System always comes first
            (Message::System(_), Message::System(_)) => self.timestamp().cmp(other.timestamp()),
            (Message::System(_), _) => std::cmp::Ordering::Less,
            (_, Message::System(_)) => std::cmp::Ordering::Greater,

            // For non-system messages, compare by timestamp
            _ => self.timestamp().cmp(other.timestamp()),
        }
    }
}

impl PartialOrd for Message {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

#[derive(Debug, Clone)]
pub struct Mention {
    pub public_key: PublicKey,
    pub range: Range<usize>,
}

impl Mention {
    pub fn new(public_key: PublicKey, range: Range<usize>) -> Self {
        Self { public_key, range }
    }
}

/// Rendered message.
#[derive(Debug, Clone)]
pub struct RenderedMessage {
    pub id: EventId,
    /// Author's public key
    pub author: PublicKey,
    /// The content/text of the message
    pub content: String,
    /// Message created time as unix timestamp
    pub created_at: Timestamp,
    /// List of mentioned public keys in the message
    pub mentions: Vec<Mention>,
    /// List of event of the message this message is a reply to
    pub replies_to: Vec<EventId>,
}

impl From<&Event> for RenderedMessage {
    fn from(val: &Event) -> Self {
        let mentions = extract_mentions(&val.content);
        let replies_to = extract_reply_ids(&val.tags);

        Self {
            id: val.id,
            author: val.pubkey,
            content: val.content.clone(),
            created_at: val.created_at,
            mentions,
            replies_to,
        }
    }
}

impl From<&UnsignedEvent> for RenderedMessage {
    fn from(val: &UnsignedEvent) -> Self {
        let mentions = extract_mentions(&val.content);
        let replies_to = extract_reply_ids(&val.tags);

        Self {
            // Event ID must be known
            id: val.id.unwrap(),
            author: val.pubkey,
            content: val.content.clone(),
            created_at: val.created_at,
            mentions,
            replies_to,
        }
    }
}

impl From<&NewMessage> for RenderedMessage {
    fn from(val: &NewMessage) -> Self {
        let mentions = extract_mentions(&val.rumor.content);
        let replies_to = extract_reply_ids(&val.rumor.tags);

        Self {
            // Event ID must be known
            id: val.rumor.id.unwrap(),
            author: val.rumor.pubkey,
            content: val.rumor.content.clone(),
            created_at: val.rumor.created_at,
            mentions,
            replies_to,
        }
    }
}

impl Eq for RenderedMessage {}

impl PartialEq for RenderedMessage {
    fn eq(&self, other: &Self) -> bool {
        self.id == other.id
    }
}

impl Ord for RenderedMessage {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        self.created_at.cmp(&other.created_at)
    }
}

impl PartialOrd for RenderedMessage {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl Hash for RenderedMessage {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        self.id.hash(state);
    }
}

/// Extracts all mentions (public keys) from a content string.
fn extract_mentions(content: &str) -> Vec<Mention> {
    let parser = NostrParser::new();
    let tokens = parser.parse(content);

    tokens
        .filter_map(|token| match token.value {
            Nip21::Pubkey(public_key) => Some(Mention::new(public_key, token.range)),
            Nip21::Profile(profile) => Some(Mention::new(profile.public_key, token.range)),
            _ => None,
        })
        .collect()
}

/// Extracts all reply (ids) from the event tags.
fn extract_reply_ids(inner: &Tags) -> Vec<EventId> {
    let mut replies_to = vec![];

    for tag in inner.filter(TagKind::e()) {
        if let Some(id) = tag.content().and_then(|id| EventId::parse(id).ok()) {
            replies_to.push(id);
        }
    }

    for tag in inner.filter(TagKind::q()) {
        if let Some(id) = tag.content().and_then(|id| EventId::parse(id).ok()) {
            replies_to.push(id);
        }
    }

    replies_to
}
