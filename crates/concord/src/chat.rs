use std::cmp::Reverse;
use std::collections::BTreeMap;
use std::fmt;

use anyhow::Result;
use nostr_sdk::prelude::*;

use crate::derive::channel_group_key;
use crate::edition::{
    AuthorityCitation, TAG_CITATION, canonical_decimal, citation_from, citation_tag,
};
use crate::stream::{
    KIND_WRAP, KIND_WRAP_EPHEMERAL, OpenedStream, SealForm, StreamError, build_rumor_ms,
    build_seal, channel_binding_tags, check_channel_binding, open_wrap, resolve_ms_strict,
    wrap_seal,
};
use crate::{ChannelId, Epoch, GroupKey, decode_hex_32};

pub const KIND_MESSAGE: u16 = 9;
pub const KIND_COMMENT: u16 = 1111;
pub const KIND_REACTION: u16 = 7;
pub const KIND_DELETE: u16 = 5;
pub const KIND_EDIT: u16 = 3302;
pub const KIND_FILE: u16 = 15;
pub const KIND_TIMER_NOTICE: u16 = 1740;
pub const KIND_WEBXDC: u16 = 3310;
pub const KIND_TYPING: u16 = 23311;

const TAG_QUOTE: &str = "q";
const TAG_TARGET: &str = "e";
const TAG_TARGET_KIND: &str = "k";
const TAG_ROOT: &str = "E";
const TAG_ROOT_KIND: &str = "K";
const TAG_ROOT_AUTHOR: &str = "P";
const TAG_TARGET_AUTHOR: &str = "p";
const TAG_EXPIRATION: &str = "expiration";
const TAG_TIMER: &str = "timer";

#[derive(Debug)]
pub enum ChatError {
    Stream(StreamError),
    NotEncryptedSealed,
    UnknownKind(u16),
    MissingTag(&'static str),
    DuplicateTag(&'static str),
    BadTag(&'static str),
    /// Neither a delete nor a timer notice may be erased by the policy it carries.
    ExemptExpiration,
}

impl fmt::Display for ChatError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ChatError::Stream(error) => write!(f, "stream: {error}"),
            ChatError::NotEncryptedSealed => write!(f, "chat rumor must ride an encrypted seal"),
            ChatError::UnknownKind(kind) => write!(f, "not a chat rumor kind: {kind}"),
            ChatError::MissingTag(name) => write!(f, "missing chat tag: {name}"),
            ChatError::DuplicateTag(name) => write!(f, "duplicate chat tag: {name}"),
            ChatError::BadTag(name) => write!(f, "malformed chat tag: {name}"),
            ChatError::ExemptExpiration => {
                write!(f, "a delete or timer notice must not carry an expiration")
            }
        }
    }
}

impl std::error::Error for ChatError {}

impl From<StreamError> for ChatError {
    fn from(error: StreamError) -> Self {
        ChatError::Stream(error)
    }
}

/// A chat event another chat event refers to: a quote, a comment's parent, a reaction's target.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ReplyRef {
    pub id: EventId,
    pub author: Option<PublicKey>,
}

/// A reference that also names the referenced event's kind, which `K`/`k` must commit on the wire.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Target {
    pub reply: ReplyRef,
    pub kind: u16,
}

#[derive(Debug, Clone)]
pub enum ChatAction {
    Message {
        reply_to: Option<ReplyRef>,
        thread_root: Option<ReplyRef>,
    },
    Reaction {
        target: EventId,
        emoji: String,
    },
    Edit {
        target: EventId,
        content: String,
    },
    Delete {
        target: EventId,
        target_kind: Option<u16>,
        citation: Option<AuthorityCitation>,
    },
    Typing,
    Opaque,
    TimerNotice {
        seconds: u64,
    },
}

#[derive(Debug, Clone)]
pub struct ChatRumor {
    pub id: EventId,
    pub author: PublicKey,
    pub kind: Kind,
    pub channel: ChannelId,
    pub epoch: Epoch,
    pub at_ms: u64,
    pub content: String,
    pub expiration: Option<Timestamp>,
    pub action: ChatAction,
}

/// A channel's timeline row, with every edit, delete and reaction folded in.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChatMessage {
    pub id: EventId,
    pub author: PublicKey,
    pub channel: ChannelId,
    pub epoch: Epoch,
    pub kind: Kind,
    pub content: String,
    pub reply_to: Option<EventId>,
    pub thread_root: Option<EventId>,
    pub at_ms: u64,
    pub expiration: Option<Timestamp>,
    pub edited_at: Option<u64>,
    pub deleted: bool,
    pub reactions: BTreeMap<PublicKey, String>,
}

pub fn build_message(
    author: PublicKey,
    channel: &ChannelId,
    epoch: Epoch,
    content: &str,
    quote: Option<&ReplyRef>,
    at_ms: u64,
    timer: Option<u64>,
) -> UnsignedEvent {
    let mut tags = channel_binding_tags(channel, epoch);

    if let Some(quote) = quote {
        tags.push(reply_tag(TAG_QUOTE, quote));
    }

    tags.extend(expiration_tag(at_ms, timer));

    build_rumor_ms(KIND_MESSAGE, author, content, tags, at_ms)
}

/// `parent` is the immediate parent; a `None` root means the parent is the thread's root.
#[allow(clippy::too_many_arguments)]
pub fn build_comment(
    author: PublicKey,
    channel: &ChannelId,
    epoch: Epoch,
    content: &str,
    parent: &Target,
    root: Option<&Target>,
    at_ms: u64,
    timer: Option<u64>,
) -> UnsignedEvent {
    let root = root.unwrap_or(parent);
    let mut tags = channel_binding_tags(channel, epoch);

    tags.push(Tag::custom(TAG_ROOT_KIND, [root.kind.to_string()]));
    tags.push(reply_tag(TAG_ROOT, &root.reply));
    if let Some(root_author) = root.reply.author {
        tags.push(Tag::custom(TAG_ROOT_AUTHOR, [root_author.to_hex()]));
    }

    tags.push(Tag::custom(TAG_TARGET_KIND, [parent.kind.to_string()]));
    tags.push(reply_tag(TAG_TARGET, &parent.reply));
    if let Some(parent_author) = parent.reply.author {
        tags.push(Tag::custom(TAG_TARGET_AUTHOR, [parent_author.to_hex()]));
    }

    tags.extend(expiration_tag(at_ms, timer));

    build_rumor_ms(KIND_COMMENT, author, content, tags, at_ms)
}

pub fn build_reaction(
    author: PublicKey,
    channel: &ChannelId,
    epoch: Epoch,
    target: &Target,
    emoji: &str,
    at_ms: u64,
    timer: Option<u64>,
) -> UnsignedEvent {
    let mut tags = channel_binding_tags(channel, epoch);

    tags.push(Tag::custom(TAG_TARGET, [target.reply.id.to_hex()]));
    if let Some(target_author) = target.reply.author {
        tags.push(Tag::custom(TAG_TARGET_AUTHOR, [target_author.to_hex()]));
    }
    tags.push(Tag::custom(TAG_TARGET_KIND, [target.kind.to_string()]));

    tags.extend(expiration_tag(at_ms, timer));

    build_rumor_ms(KIND_REACTION, author, emoji, tags, at_ms)
}

pub fn build_edit(
    author: PublicKey,
    channel: &ChannelId,
    epoch: Epoch,
    target: EventId,
    content: &str,
    at_ms: u64,
    timer: Option<u64>,
) -> UnsignedEvent {
    let mut tags = channel_binding_tags(channel, epoch);
    tags.push(Tag::custom(TAG_TARGET, [target.to_hex()]));

    tags.extend(expiration_tag(at_ms, timer));

    build_rumor_ms(KIND_EDIT, author, content, tags, at_ms)
}

/// CORD-08 §4: informational, gated by the roster rather than by the fold.
pub fn build_timer_notice(
    author: PublicKey,
    channel: &ChannelId,
    epoch: Epoch,
    seconds: u64,
    at_ms: u64,
) -> UnsignedEvent {
    let mut tags = channel_binding_tags(channel, epoch);
    tags.push(Tag::custom(TAG_TIMER, [seconds.to_string()]));

    build_rumor_ms(KIND_TIMER_NOTICE, author, "", tags, at_ms)
}

/// Derived from the signed `created_at`, so a later metadata edit never reaches back.
fn expiration_tag(at_ms: u64, timer: Option<u64>) -> Option<Tag> {
    timer.map(|timer| Tag::custom(TAG_EXPIRATION, [(at_ms / 1000 + timer).to_string()]))
}

pub fn build_delete(
    author: PublicKey,
    channel: &ChannelId,
    epoch: Epoch,
    target: EventId,
    target_kind: Option<u16>,
    citation: Option<&AuthorityCitation>,
    at_ms: u64,
) -> UnsignedEvent {
    let mut tags = channel_binding_tags(channel, epoch);
    tags.push(Tag::custom(TAG_TARGET, [target.to_hex()]));

    if let Some(target_kind) = target_kind {
        tags.push(Tag::custom(TAG_TARGET_KIND, [target_kind.to_string()]));
    }

    if let Some(citation) = citation {
        tags.push(citation_tag(citation));
    }

    build_rumor_ms(KIND_DELETE, author, "", tags, at_ms)
}

pub fn build_typing(
    author: PublicKey,
    channel: &ChannelId,
    epoch: Epoch,
    at_ms: u64,
) -> UnsignedEvent {
    build_rumor_ms(
        KIND_TYPING,
        author,
        "",
        channel_binding_tags(channel, epoch),
        at_ms,
    )
}

/// `ephemeral` picks the 21059 wrap, which relays must not store.
pub fn seal_rumor(
    rumor: &UnsignedEvent,
    group: &GroupKey,
    author: &Keys,
    ephemeral: bool,
) -> Result<(Event, Keys), ChatError> {
    let kind = rumor.kind.as_u16();

    if !is_chat_kind(kind) {
        return Err(ChatError::UnknownKind(kind));
    }

    let seal = build_seal(rumor, SealForm::Encrypted, group, author)?;
    let wrap_kind = if ephemeral {
        KIND_WRAP_EPHEMERAL
    } else {
        KIND_WRAP
    };

    // The wrap's copy is for relays; the inner one drives the local purge.
    let expiration: Vec<Tag> = rumor
        .tags
        .iter()
        .filter(|tag| tag.as_slice().first().map(String::as_str) == Some(TAG_EXPIRATION))
        .cloned()
        .collect();

    Ok(wrap_seal(
        &seal,
        group,
        wrap_kind,
        rumor.created_at,
        &expiration,
    )?)
}

/// The claimed channel and epoch must both be the ones that opened the wrap.
pub fn open(
    wrap: &Event,
    group: &GroupKey,
    channel: &ChannelId,
    epoch: Epoch,
) -> Result<(OpenedStream, ChatRumor), ChatError> {
    let opened = open_wrap(wrap, group)?;

    if opened.seal_form != SealForm::Encrypted {
        return Err(ChatError::NotEncryptedSealed);
    }

    check_channel_binding(&opened.rumor, channel, epoch)?;

    let chat = typed(&opened.rumor, channel, epoch)?;

    Ok((opened, chat))
}

/// `secret` is the `community_root` for a public channel, its own key for a private one.
pub fn plane_keys(
    held: &[(Epoch, [u8; 32])],
    channel: &ChannelId,
) -> Result<Vec<(Epoch, GroupKey)>> {
    held.iter()
        .map(|(epoch, secret)| Ok((*epoch, channel_group_key(secret, channel, *epoch)?)))
        .collect()
}

pub fn fold(
    rumors: &[ChatRumor],
    now: Timestamp,
    can_delete: impl Fn(&PublicKey, Option<&AuthorityCitation>, &PublicKey) -> bool,
) -> Vec<ChatMessage> {
    let mut order: Vec<usize> = (0..rumors.len()).collect();
    order.sort_by_key(|&index| (rumors[index].at_ms, rumors[index].id));

    let mut messages: Vec<ChatMessage> = Vec::new();
    let mut slot: BTreeMap<EventId, usize> = BTreeMap::new();

    for index in order {
        let rumor = &rumors[index];

        if expired(rumor, now) {
            continue;
        }

        let (reply_to, thread_root) = match &rumor.action {
            ChatAction::Message {
                reply_to,
                thread_root,
            } => (
                reply_to.map(|reply| reply.id),
                thread_root.map(|reply| reply.id),
            ),
            ChatAction::TimerNotice { .. } => (None, None),
            _ => continue,
        };

        slot.insert(rumor.id, messages.len());
        messages.push(ChatMessage {
            id: rumor.id,
            author: rumor.author,
            channel: rumor.channel,
            epoch: rumor.epoch,
            kind: rumor.kind,
            content: rumor.content.clone(),
            reply_to,
            thread_root,
            at_ms: rumor.at_ms,
            expiration: rumor.expiration,
            edited_at: None,
            deleted: false,
            reactions: BTreeMap::new(),
        });
    }

    let mut mutations: Vec<usize> = (0..rumors.len()).collect();
    mutations.sort_by_key(|&index| (rumors[index].at_ms, Reverse(rumors[index].id)));

    for index in mutations {
        let rumor = &rumors[index];

        match &rumor.action {
            ChatAction::Edit { target, content } => {
                let Some(&slot) = slot.get(target) else {
                    continue;
                };
                let message = &mut messages[slot];

                if message.deleted || message.author != rumor.author {
                    continue;
                }

                message.content = content.clone();
                message.edited_at = Some(rumor.at_ms);
            }
            ChatAction::Delete {
                target, citation, ..
            } => {
                let Some(&slot) = slot.get(target) else {
                    continue;
                };

                let author = messages[slot].author;

                if author == rumor.author || can_delete(&rumor.author, citation.as_ref(), &author) {
                    messages[slot].deleted = true;
                }
            }
            ChatAction::Reaction { target, emoji } => {
                let Some(&slot) = slot.get(target) else {
                    continue;
                };

                messages[slot].reactions.insert(rumor.author, emoji.clone());
            }
            ChatAction::Message { .. }
            | ChatAction::Typing
            | ChatAction::TimerNotice { .. }
            | ChatAction::Opaque => {}
        }
    }

    messages.sort_by_key(|message| (Reverse(message.at_ms), message.id));

    messages
}

/// CORD-08 §3: an expired rumor is never displayed, whatever its ingest path.
pub fn expired(rumor: &ChatRumor, now: Timestamp) -> bool {
    rumor.expiration.is_some_and(|expiration| expiration <= now)
}

fn is_chat_kind(kind: u16) -> bool {
    matches!(
        kind,
        KIND_MESSAGE
            | KIND_COMMENT
            | KIND_REACTION
            | KIND_DELETE
            | KIND_EDIT
            | KIND_FILE
            | KIND_TIMER_NOTICE
            | KIND_WEBXDC
            | KIND_TYPING
    )
}

fn typed(rumor: &UnsignedEvent, channel: &ChannelId, epoch: Epoch) -> Result<ChatRumor, ChatError> {
    let expiration = expiration_of(rumor)?;

    if expiration.is_some() && matches!(rumor.kind.as_u16(), KIND_DELETE | KIND_TIMER_NOTICE) {
        return Err(ChatError::ExemptExpiration);
    }

    Ok(ChatRumor {
        id: rumor.id.unwrap_or_else(|| rumor.compute_id()),
        author: rumor.pubkey,
        kind: rumor.kind,
        channel: *channel,
        epoch,
        at_ms: resolve_ms_strict(rumor)?,
        content: rumor.content.clone(),
        expiration,
        action: action_of(rumor)?,
    })
}

fn action_of(rumor: &UnsignedEvent) -> Result<ChatAction, ChatError> {
    let kind = rumor.kind.as_u16();

    match kind {
        KIND_MESSAGE | KIND_FILE => Ok(ChatAction::Message {
            reply_to: optional_reply(rumor, TAG_QUOTE)?,
            thread_root: None,
        }),
        KIND_COMMENT => Ok(ChatAction::Message {
            reply_to: optional_reply(rumor, TAG_TARGET)?,
            thread_root: optional_reply(rumor, TAG_ROOT)?,
        }),
        KIND_REACTION => Ok(ChatAction::Reaction {
            target: required_id(rumor, TAG_TARGET)?,
            emoji: rumor.content.clone(),
        }),
        KIND_EDIT => Ok(ChatAction::Edit {
            target: required_id(rumor, TAG_TARGET)?,
            content: rumor.content.clone(),
        }),
        KIND_DELETE => Ok(ChatAction::Delete {
            target: required_id(rumor, TAG_TARGET)?,
            target_kind: optional_kind(rumor, TAG_TARGET_KIND)?,
            citation: optional_citation(rumor)?,
        }),
        KIND_TYPING => Ok(ChatAction::Typing),
        KIND_TIMER_NOTICE => Ok(ChatAction::TimerNotice {
            seconds: timer_of(rumor)?,
        }),
        KIND_WEBXDC => Ok(ChatAction::Opaque),
        other => Err(ChatError::UnknownKind(other)),
    }
}

fn timer_of(rumor: &UnsignedEvent) -> Result<u64, ChatError> {
    let fields = tag(rumor, TAG_TIMER)?.ok_or(ChatError::MissingTag(TAG_TIMER))?;

    canonical_decimal(value(fields, TAG_TIMER)?).ok_or(ChatError::BadTag(TAG_TIMER))
}

fn optional_reply(
    rumor: &UnsignedEvent,
    name: &'static str,
) -> Result<Option<ReplyRef>, ChatError> {
    let Some(fields) = tag(rumor, name)? else {
        return Ok(None);
    };

    // NIP-C7 `q` and NIP-22 `E`/`e` put a relay hint at index 2 and the referenced author at index 3.
    let author = match fields.get(3).map(String::as_str) {
        Some(hex) if !hex.is_empty() => Some(pubkey(hex, name)?),
        _ => None,
    };

    Ok(Some(ReplyRef {
        id: hex_id(fields, name)?,
        author,
    }))
}

fn required_id(rumor: &UnsignedEvent, name: &'static str) -> Result<EventId, ChatError> {
    let fields = tag(rumor, name)?.ok_or(ChatError::MissingTag(name))?;
    hex_id(fields, name)
}

fn optional_kind(rumor: &UnsignedEvent, name: &'static str) -> Result<Option<u16>, ChatError> {
    let Some(fields) = tag(rumor, name)? else {
        return Ok(None);
    };

    let raw = value(fields, name)?;
    let kind = canonical_decimal(raw).ok_or(ChatError::BadTag(name))?;

    u16::try_from(kind)
        .map(Some)
        .map_err(|_| ChatError::BadTag(name))
}

fn optional_citation(rumor: &UnsignedEvent) -> Result<Option<AuthorityCitation>, ChatError> {
    let Some(fields) = tag(rumor, TAG_CITATION)? else {
        return Ok(None);
    };

    citation_from(fields)
        .map(Some)
        .ok_or(ChatError::BadTag(TAG_CITATION))
}

pub fn expiration_of(rumor: &UnsignedEvent) -> Result<Option<Timestamp>, ChatError> {
    let Some(fields) = tag(rumor, TAG_EXPIRATION)? else {
        return Ok(None);
    };

    let seconds = canonical_decimal(value(fields, TAG_EXPIRATION)?)
        .ok_or(ChatError::BadTag(TAG_EXPIRATION))?;

    Ok(Some(Timestamp::from_secs(seconds)))
}

fn reply_tag(name: &str, reply: &ReplyRef) -> Tag {
    Tag::custom(
        name,
        [
            reply.id.to_hex(),
            String::new(),
            reply
                .author
                .map(|author| author.to_hex())
                .unwrap_or_default(),
        ],
    )
}

fn tag<'a>(
    rumor: &'a UnsignedEvent,
    name: &'static str,
) -> Result<Option<&'a [String]>, ChatError> {
    let mut found: Option<&[String]> = None;

    for candidate in rumor.tags.iter() {
        let fields = candidate.as_slice();

        if fields.first().map(String::as_str) != Some(name) {
            continue;
        }

        if found.is_some() {
            return Err(ChatError::DuplicateTag(name));
        }

        found = Some(fields);
    }

    Ok(found)
}

fn value<'a>(fields: &'a [String], name: &'static str) -> Result<&'a str, ChatError> {
    fields
        .get(1)
        .map(String::as_str)
        .ok_or(ChatError::BadTag(name))
}

fn hex_id(fields: &[String], name: &'static str) -> Result<EventId, ChatError> {
    let bytes = decode_hex_32(value(fields, name)?).map_err(|_| ChatError::BadTag(name))?;

    EventId::from_slice(&bytes).map_err(|_| ChatError::BadTag(name))
}

fn pubkey(hex: &str, name: &'static str) -> Result<PublicKey, ChatError> {
    let bytes = decode_hex_32(hex).map_err(|_| ChatError::BadTag(name))?;

    PublicKey::from_slice(&bytes).map_err(|_| ChatError::BadTag(name))
}

#[cfg(test)]
mod tests {
    use super::*;

    const SECRET: [u8; 32] = [0x2du8; 32];
    const AT: u64 = 1_700_000_000_417;

    /// Well past every timestamp these tests use.
    fn now() -> Timestamp {
        Timestamp::from_secs(2_000_000_000)
    }

    fn channel() -> ChannelId {
        ChannelId::from_bytes([0x9cu8; 32])
    }

    fn group() -> GroupKey {
        channel_group_key(&SECRET, &channel(), Epoch(0)).expect("derives")
    }

    fn sealed(rumor: &UnsignedEvent, group: &GroupKey, author: &Keys) -> Event {
        seal_rumor(rumor, group, author, false).expect("seals").0
    }

    fn read(rumor: &UnsignedEvent, group: &GroupKey, author: &Keys, epoch: Epoch) -> ChatRumor {
        open(&sealed(rumor, group, author), group, &channel(), epoch)
            .expect("opens")
            .1
    }

    fn target(id: EventId, author: &Keys) -> Target {
        Target {
            reply: ReplyRef {
                id,
                author: Some(author.public_key()),
            },
            kind: KIND_MESSAGE,
        }
    }

    #[test]
    fn a_second_holder_folds_edits_reactions_and_a_self_delete() {
        let alice = Keys::generate();
        let carol = Keys::generate();
        let group = group();

        let message = build_message(
            alice.public_key(),
            &channel(),
            Epoch(0),
            "hello",
            None,
            AT,
            None,
        );
        let id = message.compute_id();

        let rumors = vec![
            read(&message, &group, &alice, Epoch(0)),
            read(
                &build_reaction(
                    carol.public_key(),
                    &channel(),
                    Epoch(0),
                    &target(id, &alice),
                    "🔥",
                    AT + 1_000,
                    None,
                ),
                &group,
                &carol,
                Epoch(0),
            ),
            read(
                &build_edit(
                    alice.public_key(),
                    &channel(),
                    Epoch(0),
                    id,
                    "hello (fixed)",
                    AT + 2_000,
                    None,
                ),
                &group,
                &alice,
                Epoch(0),
            ),
            read(
                &build_delete(
                    alice.public_key(),
                    &channel(),
                    Epoch(0),
                    id,
                    Some(KIND_MESSAGE),
                    None,
                    AT + 3_000,
                ),
                &group,
                &alice,
                Epoch(0),
            ),
        ];

        let folded = fold(&rumors, now(), |_, _, _| false);

        assert_eq!(folded.len(), 1);
        assert_eq!(folded[0].id, id);
        assert_eq!(folded[0].content, "hello (fixed)");
        assert_eq!(folded[0].edited_at, Some(AT + 2_000));
        assert_eq!(
            folded[0].reactions.get(&carol.public_key()),
            Some(&"🔥".to_owned())
        );
        assert!(folded[0].deleted);
    }

    #[test]
    fn an_edit_or_delete_from_another_author_is_ignored() {
        let alice = Keys::generate();
        let bob = Keys::generate();
        let group = group();

        let message = build_message(
            alice.public_key(),
            &channel(),
            Epoch(0),
            "hello",
            None,
            AT,
            None,
        );
        let id = message.compute_id();

        let rumors = vec![
            read(&message, &group, &alice, Epoch(0)),
            read(
                &build_edit(
                    bob.public_key(),
                    &channel(),
                    Epoch(0),
                    id,
                    "mine now",
                    AT + 1_000,
                    None,
                ),
                &group,
                &bob,
                Epoch(0),
            ),
            read(
                &build_delete(
                    bob.public_key(),
                    &channel(),
                    Epoch(0),
                    id,
                    Some(KIND_MESSAGE),
                    None,
                    AT + 2_000,
                ),
                &group,
                &bob,
                Epoch(0),
            ),
        ];

        let folded = fold(&rumors, now(), |_, _, _| false);

        assert_eq!(folded.len(), 1);
        assert_eq!(folded[0].content, "hello");
        assert_eq!(folded[0].edited_at, None);
        assert!(!folded[0].deleted);
    }

    #[test]
    fn a_comment_carries_its_root_and_its_parent() {
        let alice = Keys::generate();
        let bob = Keys::generate();
        let group = group();

        let root = build_message(
            alice.public_key(),
            &channel(),
            Epoch(0),
            "root",
            None,
            AT,
            None,
        );
        let root_id = root.compute_id();
        let parent = build_message(
            bob.public_key(),
            &channel(),
            Epoch(0),
            "parent",
            None,
            AT + 1_000,
            None,
        );
        let parent_id = parent.compute_id();

        let comment = build_comment(
            alice.public_key(),
            &channel(),
            Epoch(0),
            "deep",
            &target(parent_id, &bob),
            Some(&target(root_id, &alice)),
            AT + 2_000,
            None,
        );

        assert!(comment.tags.iter().any(|tag| tag.as_slice() == ["K", "9"]));
        assert!(
            comment
                .tags
                .iter()
                .any(|tag| { tag.as_slice()[0] == "E" && tag.as_slice()[1] == root_id.to_hex() })
        );
        assert!(
            comment
                .tags
                .iter()
                .any(|tag| { tag.as_slice()[0] == "e" && tag.as_slice()[1] == parent_id.to_hex() })
        );

        let rumor = read(&comment, &group, &alice, Epoch(0));
        let ChatAction::Message {
            reply_to,
            thread_root,
        } = &rumor.action
        else {
            panic!("a comment is a message row")
        };

        assert_eq!(reply_to.map(|reply| reply.id), Some(parent_id));
        assert_eq!(thread_root.map(|root| root.id), Some(root_id));
    }

    #[test]
    fn a_rumor_bound_to_another_channel_or_epoch_is_rejected() {
        let alice = Keys::generate();
        let group = group();

        let plain = build_message(
            alice.public_key(),
            &channel(),
            Epoch(0),
            "hello",
            None,
            AT,
            None,
        );
        assert!(
            open(
                &sealed(&plain, &group, &alice),
                &group,
                &channel(),
                Epoch(0)
            )
            .is_ok()
        );

        // The keyholder re-addresses their own rumor: the binding is judged
        // against the plane whose key opened the wrap, never the rumor's claim.
        let elsewhere = ChannelId::from_bytes([0xeeu8; 32]);
        assert!(matches!(
            open(
                &sealed(&plain, &group, &alice),
                &group,
                &elsewhere,
                Epoch(0)
            ),
            Err(ChatError::Stream(StreamError::ChannelMismatch))
        ));

        let stale = build_message(
            alice.public_key(),
            &channel(),
            Epoch(1),
            "stale",
            None,
            AT,
            None,
        );
        assert!(matches!(
            open(
                &sealed(&stale, &group, &alice),
                &group,
                &channel(),
                Epoch(0)
            ),
            Err(ChatError::Stream(StreamError::EpochMismatch))
        ));

        // Chat is encrypted-seal only (CORD-02 §5), and a retired kind is not a
        // chat rumor however well-formed it looks.
        let seal = build_seal(&plain, SealForm::Plaintext, &group, &alice).expect("seals");
        let (wrap, _) = wrap_seal(
            &seal,
            &group,
            KIND_WRAP,
            Timestamp::from_secs(AT / 1000),
            &[],
        )
        .expect("wraps");
        assert!(matches!(
            open(&wrap, &group, &channel(), Epoch(0)),
            Err(ChatError::NotEncryptedSealed)
        ));

        let ghost = build_rumor_ms(
            3300,
            alice.public_key(),
            "v1 ghost",
            channel_binding_tags(&channel(), Epoch(0)),
            AT,
        );
        assert!(matches!(
            seal_rumor(&ghost, &group, &alice, false),
            Err(ChatError::UnknownKind(3300))
        ));

        let mut tags = channel_binding_tags(&channel(), Epoch(0));
        tags.push(Tag::custom(TAG_TARGET, ["ab".repeat(32)]));
        tags.push(Tag::custom(TAG_TARGET, ["cd".repeat(32)]));
        let ambiguous = build_rumor_ms(KIND_DELETE, alice.public_key(), "", tags, AT);
        assert!(matches!(
            open(
                &sealed(&ambiguous, &group, &alice),
                &group,
                &channel(),
                Epoch(0)
            ),
            Err(ChatError::DuplicateTag(TAG_TARGET))
        ));
    }

    #[test]
    fn a_moderator_delete_needs_the_roster_and_a_citation() {
        let alice = Keys::generate();
        let moderator = Keys::generate();
        let peer = Keys::generate();
        let group = group();

        let message = read(
            &build_message(
                alice.public_key(),
                &channel(),
                Epoch(0),
                "hello",
                None,
                AT,
                None,
            ),
            &group,
            &alice,
            Epoch(0),
        );
        let id = message.id;
        let citation = AuthorityCitation {
            entity: [0x33; 32],
            version: 1,
            hash: [0x44; 32],
        };

        let delete = |author: &Keys, citation: Option<&AuthorityCitation>| {
            read(
                &build_delete(
                    author.public_key(),
                    &channel(),
                    Epoch(0),
                    id,
                    Some(KIND_MESSAGE),
                    citation,
                    AT + 1_000,
                ),
                &group,
                author,
                Epoch(0),
            )
        };

        let can_delete =
            |actor: &PublicKey, citation: Option<&AuthorityCitation>, author: &PublicKey| {
                actor != author && citation.is_some() && actor == &moderator.public_key()
            };

        let cited = vec![message.clone(), delete(&moderator, Some(&citation))];
        assert!(matches!(
            &cited[1].action,
            ChatAction::Delete { citation: Some(parsed), .. } if *parsed == citation
        ));
        assert!(
            fold(&cited, now(), can_delete)[0].deleted,
            "a cited moderator delete lands"
        );

        let uncited = vec![message.clone(), delete(&moderator, None)];
        assert!(
            !fold(&uncited, now(), can_delete)[0].deleted,
            "an uncited delete names no rank"
        );

        let peer_delete = vec![message.clone(), delete(&peer, Some(&citation))];
        assert!(
            !fold(&peer_delete, now(), can_delete)[0].deleted,
            "a peer's delete is not authority"
        );

        let own = vec![message.clone(), delete(&alice, None)];
        assert!(
            fold(&own, now(), |_, _, _| false)[0].deleted,
            "a self-delete never consults the predicate"
        );
    }

    #[test]
    fn a_timer_rides_durable_rumors_and_expiry_gates_the_fold() {
        let alice = Keys::generate();
        let group = group();
        let expires = (AT / 1000 + 60).to_string();

        // Computed from the signed `created_at`, and mirrored onto the wrap so
        // relays drop the ciphertext too.
        let message = build_message(
            alice.public_key(),
            &channel(),
            Epoch(0),
            "tick",
            None,
            AT,
            Some(60),
        );
        assert!(
            message
                .tags
                .iter()
                .any(|tag| tag.as_slice() == [TAG_EXPIRATION, expires.as_str()])
        );
        assert!(
            sealed(&message, &group, &alice)
                .tags
                .iter()
                .any(|tag| tag.as_slice() == [TAG_EXPIRATION, expires.as_str()])
        );

        let live = read(&message, &group, &alice, Epoch(0));
        assert_eq!(live.expiration, Some(Timestamp::from_secs(AT / 1000 + 60)));
        assert!(!expired(&live, Timestamp::from_secs(AT / 1000 + 59)));
        assert!(expired(&live, Timestamp::from_secs(AT / 1000 + 60)));
        assert_eq!(
            fold(
                std::slice::from_ref(&live),
                Timestamp::from_secs(AT / 1000 + 59),
                |_, _, _| false
            )
            .len(),
            1
        );
        assert_eq!(
            fold(&[live], Timestamp::from_secs(AT / 1000 + 60), |_, _, _| {
                false
            })
            .len(),
            0
        );

        // A delete is a tombstone and a notice documents the policy, so neither
        // may be erased by the policy it carries.
        let mut expiring = channel_binding_tags(&channel(), Epoch(0));
        expiring.push(Tag::custom(TAG_EXPIRATION, ["1"]));
        expiring.push(Tag::custom(TAG_TARGET, ["ab".repeat(32)]));

        for kind in [KIND_DELETE, KIND_TIMER_NOTICE] {
            let rumor = build_rumor_ms(kind, alice.public_key(), "", expiring.clone(), AT);
            assert!(matches!(
                open(
                    &sealed(&rumor, &group, &alice),
                    &group,
                    &channel(),
                    Epoch(0)
                ),
                Err(ChatError::ExemptExpiration)
            ));
        }

        // A notice is a row of its own; whether its author may be believed
        // about policy is the roster's call, not the fold's.
        let notice = build_timer_notice(alice.public_key(), &channel(), Epoch(0), 3_600, AT);
        let folded = fold(
            &[read(&notice, &group, &alice, Epoch(0))],
            now(),
            |_, _, _| false,
        );
        assert_eq!(folded.len(), 1);
        assert_eq!(folded[0].kind, Kind::Custom(KIND_TIMER_NOTICE));

        let mut malformed = channel_binding_tags(&channel(), Epoch(0));
        malformed.push(Tag::custom(TAG_TIMER, ["060"]));
        let rumor = build_rumor_ms(KIND_TIMER_NOTICE, alice.public_key(), "", malformed, AT);
        assert!(matches!(
            open(
                &sealed(&rumor, &group, &alice),
                &group,
                &channel(),
                Epoch(0)
            ),
            Err(ChatError::BadTag(TAG_TIMER))
        ));
    }
}
