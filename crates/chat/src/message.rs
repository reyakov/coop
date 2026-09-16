use std::hash::Hash;
use std::ops::Range;

use common::{EventExt, NostrParser, extract_and_remove_media_urls};
use gpui::{SharedString, SharedUri};
use nostr_sdk::prelude::*;
use state::FileAttachment;

pub const KIND_FILE_MESSAGE: Kind = Kind::Custom(15);

/// Rendered message.
#[derive(Debug, Clone)]
pub struct Message {
    pub id: EventId,
    /// Author's public key
    pub author: PublicKey,
    /// The content/text of the message
    pub content: String,
    /// List of media URLs in the message
    pub media: Vec<SharedUri>,
    /// Message created time as unix timestamp
    pub created_at: Timestamp,
    /// List of mentioned public keys in the message
    pub mentions: Vec<Mention>,
    /// List of event of the message this message is a reply to
    pub replies_to: Vec<EventId>,
    /// Encrypted file attachment
    pub file: Option<FileAttachment>,
}

impl From<&Event> for Message {
    fn from(val: &Event) -> Self {
        from_parts(
            val.id,
            val.pubkey,
            val.created_at,
            val.kind,
            &val.content,
            &val.tags,
        )
    }
}

impl From<&UnsignedEvent> for Message {
    fn from(val: &UnsignedEvent) -> Self {
        from_parts(
            // Event ID must be known
            val.id.unwrap(),
            val.pubkey,
            val.created_at,
            val.kind,
            &val.content,
            &val.tags,
        )
    }
}

impl From<&NewMessage> for Message {
    fn from(val: &NewMessage) -> Self {
        from_parts(
            // Event ID must be known
            val.rumor.id.unwrap(),
            val.rumor.pubkey,
            val.rumor.created_at,
            val.rumor.kind,
            &val.rumor.content,
            &val.rumor.tags,
        )
    }
}

fn from_parts(
    id: EventId,
    author: PublicKey,
    created_at: Timestamp,
    kind: Kind,
    content: &str,
    tags: &Tags,
) -> Message {
    let file = if kind == KIND_FILE_MESSAGE {
        FileAttachment::from_tags(content, tags)
    } else {
        None
    };
    let has_file = file.is_some();

    let replies_to = extract_reply_ids(tags);

    // For file messages `.content` is the encrypted blob URL, not text or media
    let mentions = if has_file {
        Vec::new()
    } else {
        extract_mentions(content)
    };

    let (media, content) = if has_file {
        (Vec::new(), String::new())
    } else {
        extract_and_remove_media_urls(content)
    };

    Message {
        id,
        author,
        content,
        media,
        created_at,
        mentions,
        replies_to,
        file,
    }
}

impl Eq for Message {}

impl PartialEq for Message {
    fn eq(&self, other: &Self) -> bool {
        self.id == other.id
    }
}

impl Ord for Message {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        self.created_at.cmp(&other.created_at)
    }
}

impl PartialOrd for Message {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl Hash for Message {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        self.id.hash(state);
    }
}

impl Message {
    /// Single-line representation for reply previews, notifications and copy.
    pub fn preview(&self) -> SharedString {
        if let Some(file) = &self.file {
            return format!("[File] {}", file.display_name()).into();
        }

        self.content.clone().into()
    }
}

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

    for tag in inner.iter().filter(|tag| tag.kind() == "e") {
        if let Some(id) = tag.content().and_then(|id| EventId::parse(id).ok()) {
            replies_to.push(id);
        }
    }

    for tag in inner.iter().filter(|tag| tag.kind() == "q") {
        if let Some(id) = tag.content().and_then(|id| EventId::parse(id).ok()) {
            replies_to.push(id);
        }
    }

    replies_to
}
