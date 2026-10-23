use std::hash::Hash;
use std::ops::Range;

use common::{EventExt, NostrParser, emojis_from_tags, extract_and_remove_media_urls, nevent_refs};
use gpui::{SharedString, SharedUri};
use nostr_sdk::prelude::*;
use state::FileAttachment;

pub(crate) const KIND_FILE_MESSAGE: Kind = Kind::Custom(15);

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
    /// Custom emoji (NIP-30) declared by this message's `emoji` tags
    pub emojis: Vec<(String, Url)>,
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
    let emojis = emojis_from_tags(tags);

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
        emojis,
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

        // Media-only messages carry their URLs outside `.content`
        if self.content.is_empty() && !self.media.is_empty() {
            return self
                .media
                .iter()
                .map(|url| url.to_string())
                .collect::<Vec<_>>()
                .join(" ")
                .into();
        }

        let references = nevent_refs(&self.content);
        if references.is_empty() {
            return self.content.clone().into();
        }

        let mut preview = String::with_capacity(self.content.len());
        let mut last = 0;

        for reference in references {
            preview.push_str(&self.content[last..reference.range.start]);
            preview.push_str(&reference.label);
            last = reference.range.end;
        }
        preview.push_str(&self.content[last..]);

        preview.into()
    }

    /// Image URL of a `:shortcode:` reaction, if the shortcode is declared
    pub fn reaction_image(content: &str, emojis: &[(String, Url)]) -> Option<Url> {
        let shortcode = content.strip_prefix(':')?.strip_suffix(':')?;
        let (_, url) = emojis.iter().find(|(name, _)| name == shortcode)?;
        Some(url.clone())
    }
}

/// New message.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct NewMessage {
    pub room: u64,
    pub rumor: UnsignedEvent,
}

impl NewMessage {
    pub fn new(_gift_wrap: EventId, rumor: UnsignedEvent) -> Self {
        let room = rumor.uniq_id();

        Self { room, rumor }
    }
}

/// Trash message.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) struct FailedMessage {
    pub raw_event: SharedString,
    pub reason: SharedString,
}

impl FailedMessage {
    pub(crate) fn new<T>(event: &Event, reason: T) -> Self
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
    pub(crate) fn new(public_key: PublicKey, range: Range<usize>) -> Self {
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

#[cfg(test)]
mod tests {
    use super::*;

    fn rumor(kind: Kind, content: &str, tags: Tags) -> UnsignedEvent {
        let sender = Keys::generate();

        let mut rumor = EventBuilder::new(kind, content)
            .tags(tags)
            .finalize_unsigned(sender.public_key());
        rumor.ensure_id();
        rumor
    }

    fn emoji_tag(shortcode: &str, url: &str) -> Tag {
        Tag::parse(vec!["emoji", shortcode, url]).expect("valid emoji tag")
    }

    #[test]
    fn message_parses_emoji_tags() {
        let tags = Tags::from_list(vec![emoji_tag(
            "soapbox",
            "https://example.com/soapbox.png",
        )]);

        let message = Message::from(&rumor(Kind::PrivateDirectMessage, "hi :soapbox:", tags));

        assert_eq!(message.emojis.len(), 1);
        assert_eq!(message.emojis[0].0, "soapbox");
    }

    #[test]
    fn reaction_image_matches_declared_shortcode() {
        let emojis = vec![(
            String::from("soapbox"),
            Url::parse("https://example.com/soapbox.png").unwrap(),
        )];

        assert_eq!(
            Message::reaction_image(":soapbox:", &emojis),
            Some(Url::parse("https://example.com/soapbox.png").unwrap())
        );
        assert_eq!(Message::reaction_image("😂", &emojis), None);
        assert_eq!(Message::reaction_image(":unknown:", &emojis), None);
    }

    fn valid_nevent() -> String {
        Nip19Event::new(EventId::from_byte_array([7; 32]))
            .to_bech32()
            .expect("valid nevent")
    }

    #[test]
    fn image_only_messages_preview_their_url() {
        let url = "https://example.com/picture.png";
        let message = Message::from(&rumor(Kind::PrivateDirectMessage, url, Tags::new()));

        assert!(message.content.is_empty());
        assert_eq!(message.preview().to_string(), url);
    }

    #[test]
    fn preview_compacts_nevent_references() {
        let nevent = valid_nevent();
        let message = Message::from(&rumor(
            Kind::PrivateDirectMessage,
            &format!("see nostr:{nevent}"),
            Tags::new(),
        ));

        let label = format!("https://njump.me/nevent1...{}", &nevent[nevent.len() - 4..]);
        assert_eq!(message.preview().to_string(), format!("see {label}"));
    }
}
