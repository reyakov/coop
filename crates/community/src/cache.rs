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

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use concord::Epoch;
    use nostr_memory::MemoryDatabase;

    use super::*;

    fn client() -> Client {
        ClientBuilder::default()
            .database(MemoryDatabase::unbounded())
            .build()
    }

    #[test]
    fn load_states_reads_one_document_per_community_and_ignores_other_documents() {
        smol::block_on(async {
            let client = client();

            let state = CommunityState {
                id: CommunityId::from_bytes([0x42; 32]),
                name: Some("Anime and Manga".to_owned()),
                owner: Keys::generate().public_key(),
                owner_salt: [0x01; 32],
                community_root: [0x02; 32],
                root_epoch: Epoch(0),
                control_root: None,
                control_pks: BTreeMap::new(),
                channels: Vec::new(),
                relays: Vec::new(),
                heads: Vec::new(),
                banned: BTreeSet::new(),
                cursors: BTreeMap::new(),
                held_roots: Vec::new(),
                channel_cuts: BTreeMap::new(),
                refounders: BTreeSet::new(),
                removed_at: None,
                stranded: false,
                dissolved: false,
                added_at_ms: 7,
            };

            save_state(&client, &state).await.expect("saves");

            // A cached rumor is also an application-specific document, but not a
            // state document, so the prefix keeps it out of the state scan.
            let other = EventBuilder::new(Kind::ApplicationSpecificData, "{}")
                .tags([Tag::identifier("deadbeef")])
                .finalize(&*LOCAL_KEYS)
                .expect("builds");
            client.database().save_event(&other).await.expect("saves");

            let loaded = load_states(&client).await.expect("loads");

            assert_eq!(loaded, vec![state]);
        });
    }

    #[test]
    fn caching_the_same_rumor_twice_leaves_one_row() {
        smol::block_on(async {
            let client = client();
            let channel = ChannelId::from_bytes([0x9c; 32]);
            let keys = Keys::generate();
            let group = concord::derive::channel_group_key(&[0x07; 32], &channel, Epoch(0))
                .expect("a group key");

            let rumor = concord::cord03::build_message(
                keys.public_key(),
                &channel,
                Epoch(0),
                "twice",
                None,
                1_700_000_000_000,
                None,
            );
            let (wrap, _) = concord::cord03::seal_rumor(&rumor, &group, &keys, false)
                .await
                .expect("seals");
            let (opened, _) =
                concord::cord03::open(&wrap, &group, &channel, Epoch(0)).expect("opens");

            assert!(
                cache_rumor(&client, &channel, &opened)
                    .await
                    .expect("caches")
            );
            assert!(
                cache_rumor(&client, &channel, &opened)
                    .await
                    .expect("caches")
            );

            let cached = query_rumors(&client, &channel, None, 10, None)
                .await
                .expect("reads");
            assert_eq!(cached.len(), 1);
        });
    }
}
