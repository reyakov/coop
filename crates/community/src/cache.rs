use std::collections::BTreeMap;
use std::sync::LazyLock;

use anyhow::{Result, anyhow};
use concord::cord01::OpenedStream;
use concord::state::{CommunityState, STATE_PREFIX, state_identifier};
use concord::{ChannelId, CommunityId, cord03};
use nostr_sdk::prelude::*;

static LOCAL_KEYS: LazyLock<Keys> = LazyLock::new(|| {
    Keys::new(SecretKey::from_slice(&[0x43; 32]).expect("a fixed 32-byte scalar is a valid key"))
});

const CHANNEL_TAG: SingleLetterTag = SingleLetterTag::LOWERCASE_C;
const MARK_TAG: SingleLetterTag = SingleLetterTag::LOWERCASE_T;
const MARK_VALUE: &str = "concord";
const WRAP_TAG: &str = "e";
const KIND_TAG: SingleLetterTag = SingleLetterTag::LOWERCASE_K;

/// An already-expired rumor is refused at ingest. Returns whether it was kept.
pub async fn cache_rumor(
    client: &Client,
    channel: &ChannelId,
    opened: &OpenedStream,
) -> Result<bool> {
    let at = Timestamp::from_secs(opened.at_ms / 1000);

    if cord03::expiration_of(&opened.rumor)?
        .is_some_and(|expiration| expiration <= Timestamp::now())
    {
        return Ok(false);
    }

    let tags = vec![
        Tag::identifier(opened.rumor_id),
        Tag::custom(KIND_TAG.as_str(), [opened.rumor.kind.to_string()]),
        Tag::custom(WRAP_TAG, [opened.wrapper_id.to_string()]),
        Tag::custom(MARK_TAG.as_str(), [MARK_VALUE]),
        Tag::custom(CHANNEL_TAG.as_str(), [channel.to_hex()]),
        Tag::public_key(opened.author),
    ];

    let event = EventBuilder::new(Kind::ApplicationSpecificData, opened.rumor.as_json())
        .tags(tags)
        .custom_created_at(at)
        .finalize_async(&*LOCAL_KEYS)
        .await?;

    client.database().save_event(&event).await?;

    Ok(true)
}

pub async fn purge_expired(client: &Client, channel: &ChannelId, now: Timestamp) -> Result<usize> {
    let filter = Filter::new()
        .kind(Kind::ApplicationSpecificData)
        .custom_tag(MARK_TAG, MARK_VALUE)
        .custom_tag(CHANNEL_TAG, channel.to_hex());

    let mut expired = Vec::new();

    for event in client.database().query(filter).await? {
        let Ok(rumor) = UnsignedEvent::from_json(&event.content) else {
            continue;
        };

        let Ok(Some(expiration)) = cord03::expiration_of(&rumor) else {
            continue;
        };

        if expiration <= now {
            expired.push(event.id);
        }
    }

    let purged = expired.len();

    if purged > 0 {
        client.database().delete(Filter::new().ids(expired)).await?;
    }

    Ok(purged)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Observed {
    pub author: PublicKey,
    pub at_ms: u64,
}

/// The cached rumors of `channel`, keyed by the wrap they were opened from.
pub async fn wrapper_index(
    client: &Client,
    channel: &ChannelId,
) -> Result<BTreeMap<EventId, Observed>> {
    let filter = Filter::new()
        .kind(Kind::ApplicationSpecificData)
        .custom_tag(MARK_TAG, MARK_VALUE)
        .custom_tag(CHANNEL_TAG, channel.to_hex());

    let mut index = BTreeMap::new();

    for event in client.database().query(filter).await? {
        let (Some(wrapper_id), Some(author)) = (
            event.tags.event_ids().next(),
            event.tags.public_keys().next(),
        ) else {
            continue;
        };

        index.insert(
            wrapper_id,
            Observed {
                author,
                at_ms: event.created_at.as_secs().saturating_mul(1000),
            },
        );
    }

    Ok(index)
}

/// Cached rumors for `channel`, newest first, deduplicated by rumor id.
pub async fn query_rumors(
    client: &Client,
    channel: &ChannelId,
    until: Option<Timestamp>,
    limit: usize,
    kinds: Option<&[u16]>,
) -> Result<Vec<UnsignedEvent>> {
    let mut filter = Filter::new()
        .kind(Kind::ApplicationSpecificData)
        .custom_tag(MARK_TAG, MARK_VALUE)
        .custom_tag(CHANNEL_TAG, channel.to_hex());

    if let Some(kinds) = kinds {
        filter = filter.custom_tags(KIND_TAG, kinds.iter().map(u16::to_string));
    }

    if let Some(until) = until {
        filter = filter.until(until);
    }

    let mut newest: BTreeMap<String, Event> = BTreeMap::new();
    for event in client.database().query(filter).await? {
        let Some(rumor_id) = event.tags.identifier() else {
            continue;
        };

        match newest.get(&rumor_id) {
            Some(existing) if existing.created_at >= event.created_at => {}
            _ => {
                newest.insert(rumor_id, event);
            }
        }
    }

    let mut events: Vec<Event> = newest.into_values().collect();
    events.sort_by_key(|event| std::cmp::Reverse(event.created_at));
    events.truncate(limit);

    let mut rumors = Vec::with_capacity(events.len());
    for event in events {
        let rumor = UnsignedEvent::from_json(event.content)
            .map_err(|error| anyhow!("cached rumor is not a valid event: {error}"))?;
        rumors.push(rumor);
    }

    Ok(rumors)
}

pub async fn save_state(client: &Client, state: &CommunityState) -> Result<()> {
    let event = EventBuilder::new(Kind::ApplicationSpecificData, serde_json::to_string(state)?)
        .tags([Tag::identifier(state.identifier())])
        .finalize_async(&*LOCAL_KEYS)
        .await?;

    client.database().save_event(&event).await?;

    Ok(())
}

pub async fn load_state(client: &Client, id: &CommunityId) -> Result<Option<CommunityState>> {
    let filter = Filter::new()
        .kind(Kind::ApplicationSpecificData)
        .identifier(state_identifier(id))
        .limit(1);

    match client.database().query(filter).await?.into_iter().next() {
        Some(event) => Ok(Some(serde_json::from_str(&event.content)?)),
        None => Ok(None),
    }
}

/// The newest state document per community carried in the local database.
pub async fn load_states(client: &Client) -> Result<Vec<CommunityState>> {
    let filter = Filter::new().kind(Kind::ApplicationSpecificData);
    let mut newest: BTreeMap<CommunityId, Event> = BTreeMap::new();

    for event in client.database().query(filter).await? {
        let Some(id) = state_document_of(&event) else {
            continue;
        };

        match newest.get(&id) {
            Some(existing) if existing.created_at >= event.created_at => {}
            _ => {
                newest.insert(id, event);
            }
        }
    }

    let mut states = Vec::with_capacity(newest.len());

    for event in newest.into_values() {
        match serde_json::from_str::<CommunityState>(&event.content) {
            Ok(state) => states.push(state),
            Err(error) => log::warn!("ignoring malformed community state {}: {error}", event.id),
        }
    }

    Ok(states)
}

fn state_document_of(event: &Event) -> Option<CommunityId> {
    let identifier = event.tags.identifier()?;
    let hex = identifier.strip_prefix(STATE_PREFIX)?;
    hex.parse().ok()
}
