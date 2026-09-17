use std::cmp::Reverse;
use std::collections::{BTreeMap, BTreeSet};
use std::sync::LazyLock;

use anyhow::{Result, anyhow};
use nostr_sdk::prelude::*;
use serde::{Deserialize, Serialize};

use crate::chat::{self, ChatRumor, plane_keys};
use crate::control::{
    ChannelMetadata, CommunityGenesis, CommunityMetadata, ControlFold, ROOT_EPOCH,
};
use crate::derive::control_signer_group_key;
use crate::edition::{EntityHead, Floors, ParsedEdition, vsk};
use crate::stream::{KIND_WRAP_EPHEMERAL, OpenedStream};
use crate::{ChannelId, CommunityId, Epoch, GroupKey};

static LOCAL_KEYS: LazyLock<Keys> = LazyLock::new(Keys::generate);

const MAX_PAGES: usize = 8;
const CHANNEL_TAG: SingleLetterTag = SingleLetterTag::LOWERCASE_C;
const MARK_TAG: SingleLetterTag = SingleLetterTag::LOWERCASE_T;
const MARK_VALUE: &str = "concord";
const WRAP_TAG: &str = "e";
const KIND_TAG: &str = "k";
const STATE_PREFIX: &str = "concord/";

/// An already-expired rumor is refused at ingest. Returns whether it was kept.
pub async fn cache_rumor(
    database: &dyn NostrDatabase,
    channel: &ChannelId,
    opened: &OpenedStream,
) -> Result<bool> {
    if chat::expiration_of(&opened.rumor)?.is_some_and(|expiration| expiration <= Timestamp::now())
    {
        return Ok(false);
    }

    let tags = vec![
        Tag::identifier(opened.rumor_id),
        Tag::custom(KIND_TAG, [opened.rumor.kind.to_string()]),
        Tag::custom(WRAP_TAG, [opened.wrapper_id.to_string()]),
        Tag::custom(MARK_TAG.as_str(), [MARK_VALUE]),
        Tag::custom(CHANNEL_TAG.as_str(), [channel.to_hex()]),
        Tag::public_key(opened.author),
    ];
    let at = Timestamp::from_secs(opened.at_ms / 1000);
    let event = EventBuilder::new(Kind::ApplicationSpecificData, opened.rumor.as_json())
        .tags(tags)
        .custom_created_at(at)
        .finalize_async(&*LOCAL_KEYS)
        .await?;

    database.save_event(&event).await?;

    Ok(true)
}

pub async fn purge_expired(
    database: &dyn NostrDatabase,
    channel: &ChannelId,
    now: Timestamp,
) -> Result<usize> {
    let filter = Filter::new()
        .kind(Kind::ApplicationSpecificData)
        .custom_tag(MARK_TAG, MARK_VALUE)
        .custom_tag(CHANNEL_TAG, channel.to_hex());

    let mut expired = Vec::new();

    for event in database.query(filter).await? {
        let Ok(rumor) = UnsignedEvent::from_json(&event.content) else {
            continue;
        };

        let Ok(Some(expiration)) = chat::expiration_of(&rumor) else {
            continue;
        };

        if expiration <= now {
            expired.push(event.id);
        }
    }

    let purged = expired.len();

    if purged > 0 {
        database.delete(Filter::new().ids(expired)).await?;
    }

    Ok(purged)
}

pub async fn query_rumors(
    database: &dyn NostrDatabase,
    channel: &ChannelId,
    until: Option<Timestamp>,
    limit: usize,
) -> Result<Vec<UnsignedEvent>> {
    let mut filter = Filter::new()
        .kind(Kind::ApplicationSpecificData)
        .custom_tag(MARK_TAG, MARK_VALUE)
        .custom_tag(CHANNEL_TAG, channel.to_hex());

    if let Some(until) = until {
        filter = filter.until(until);
    }

    let mut newest: BTreeMap<String, Event> = BTreeMap::new();
    for event in database.query(filter).await? {
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

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChannelKeyRef {
    pub id: ChannelId,
    pub name: String,
    pub private: bool,
    pub epoch: Epoch,
}

/// One local document per community, keyed by `concord/<community_id>`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CommunityState {
    pub id: CommunityId,
    pub owner: PublicKey,
    pub owner_salt: [u8; 32],
    pub community_root: [u8; 32],
    pub root_epoch: Epoch,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub control_root: Option<[u8; 32]>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub control_pks: BTreeMap<u64, PublicKey>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub channels: Vec<ChannelKeyRef>,
    pub relays: Vec<RelayUrl>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub heads: Vec<EntityHead>,
    #[serde(default, skip_serializing_if = "BTreeSet::is_empty")]
    pub banned: BTreeSet<PublicKey>,
    #[serde(default)]
    pub dissolved: bool,
    pub added_at_ms: u64,
}

impl CommunityState {
    pub fn from_genesis(
        genesis: &CommunityGenesis,
        editions: &[ParsedEdition],
        added_at_ms: u64,
    ) -> Result<Self> {
        let mut channels = Vec::new();
        let mut heads = Vec::with_capacity(editions.len());
        let mut relays = Vec::new();

        for edition in editions {
            heads.push(EntityHead {
                entity: edition.entity,
                version: edition.version,
                self_hash: edition.self_hash,
                rumor_id: edition.rumor_id,
            });

            match edition.subkind.as_str() {
                vsk::COMMUNITY_METADATA => {
                    let metadata: CommunityMetadata = serde_json::from_str(&edition.content)?;
                    relays.extend(
                        metadata
                            .relays
                            .iter()
                            .filter_map(|relay| RelayUrl::parse(relay).ok()),
                    );
                }
                vsk::CHANNEL_METADATA => {
                    let metadata: ChannelMetadata = serde_json::from_str(&edition.content)?;
                    channels.push(ChannelKeyRef {
                        id: ChannelId::from_bytes(edition.entity),
                        name: metadata.name,
                        private: metadata.private,
                        epoch: ROOT_EPOCH,
                    });
                }
                _ => {}
            }
        }

        let control_pks = BTreeMap::from([(
            ROOT_EPOCH.0,
            control_signer_group_key(
                &genesis.control_root,
                &genesis.identity.community_id,
                ROOT_EPOCH,
            )?
            .pk(),
        )]);

        Ok(Self {
            id: genesis.identity.community_id,
            owner: genesis.identity.owner,
            owner_salt: genesis.identity.owner_salt,
            community_root: genesis.community_root,
            root_epoch: ROOT_EPOCH,
            control_root: Some(genesis.control_root),
            control_pks,
            channels,
            relays,
            heads,
            banned: BTreeSet::new(),
            dissolved: false,
            added_at_ms,
        })
    }

    pub fn identifier(&self) -> String {
        state_identifier(&self.id)
    }

    pub fn floors(&self) -> Floors {
        self.heads
            .iter()
            .map(|head| (head.entity, head.clone()))
            .collect()
    }

    pub fn apply_fold(&mut self, fold: &ControlFold) {
        self.heads = fold.floors.values().cloned().collect();
        self.banned = fold.banned.clone();

        if let Some(community) = &fold.community {
            self.relays = community
                .relays
                .iter()
                .filter_map(|relay| RelayUrl::parse(relay).ok())
                .collect();
        }

        for (id, metadata) in &fold.channels {
            if metadata.deleted.unwrap_or(false) {
                self.channels.retain(|channel| channel.id != *id);
                continue;
            }

            match self.channels.iter_mut().find(|channel| channel.id == *id) {
                Some(channel) => {
                    channel.name = metadata.name.clone();

                    if !metadata.private {
                        channel.private = false;
                    }
                }
                None if !metadata.private => self.channels.push(ChannelKeyRef {
                    id: *id,
                    name: metadata.name.clone(),
                    private: false,
                    epoch: self.root_epoch,
                }),
                None => {}
            }
        }
    }
}

fn state_identifier(id: &CommunityId) -> String {
    format!("{STATE_PREFIX}{}", id.to_hex())
}

pub async fn save_state<D>(database: &D, state: &CommunityState) -> Result<()>
where
    D: NostrDatabase,
{
    let event = EventBuilder::new(Kind::ApplicationSpecificData, serde_json::to_string(state)?)
        .tags([Tag::identifier(state.identifier())])
        .finalize_async(&*LOCAL_KEYS)
        .await?;

    database.save_event(&event).await?;

    Ok(())
}

pub async fn load_state<D>(database: &D, id: &CommunityId) -> Result<Option<CommunityState>>
where
    D: NostrDatabase,
{
    let filter = Filter::new()
        .kind(Kind::ApplicationSpecificData)
        .identifier(state_identifier(id))
        .limit(1);

    match database.query(filter).await?.into_iter().next() {
        Some(event) => Ok(Some(serde_json::from_str(&event.content)?)),
        None => Ok(None),
    }
}

pub async fn backfill(
    client: &Client,
    database: &dyn NostrDatabase,
    channel: &ChannelId,
    held: &[(Epoch, [u8; 32])],
    until: Option<Timestamp>,
    limit: usize,
) -> Result<Vec<ChatRumor>> {
    let planes = plane_keys(held, channel)?;
    let authors: Vec<PublicKey> = planes.iter().map(|(_, group)| group.pk()).collect();

    let mut cursor = until;
    let mut seen: BTreeSet<EventId> = BTreeSet::new();
    let mut found: Vec<ChatRumor> = Vec::new();

    for _ in 0..MAX_PAGES {
        let page = fetch_page(client, &authors, cursor, limit).await?;

        if page.is_empty() {
            break;
        }

        let (fresh, next) = advance(&page, &planes, channel, cursor, limit, &mut seen);

        for (opened, rumor) in fresh {
            if cache_rumor(database, channel, &opened).await? {
                found.push(rumor);
            }
        }

        match next {
            Some(next) => cursor = Some(next),
            None => break,
        }
    }

    found.sort_by_key(|rumor| (Reverse(rumor.at_ms), rumor.id));
    found.truncate(limit);

    Ok(found)
}

fn advance(
    page: &BTreeSet<Event>,
    planes: &[(Epoch, GroupKey)],
    channel: &ChannelId,
    cursor: Option<Timestamp>,
    limit: usize,
    seen: &mut BTreeSet<EventId>,
) -> (Vec<(OpenedStream, ChatRumor)>, Option<Timestamp>) {
    let mut fresh = Vec::new();

    for wrap in page {
        let Some((epoch, group)) = planes.iter().find(|(_, group)| group.pk() == wrap.pubkey)
        else {
            continue;
        };

        let Ok((opened, rumor)) = chat::open(wrap, group, channel, *epoch) else {
            continue;
        };

        if seen.insert(rumor.id) {
            fresh.push((opened, rumor));
        }
    }

    if fresh.is_empty() || page.len() < limit {
        return (fresh, None);
    }

    let oldest = page.iter().map(|event| event.created_at).min();

    match oldest {
        Some(oldest) if cursor != Some(oldest) => (fresh, Some(oldest)),
        _ => (fresh, None),
    }
}

async fn fetch_page(
    client: &Client,
    authors: &[PublicKey],
    until: Option<Timestamp>,
    limit: usize,
) -> Result<BTreeSet<Event>> {
    let mut filter = Filter::new()
        .kinds([Kind::GiftWrap, Kind::Custom(KIND_WRAP_EPHEMERAL)])
        .authors(authors.iter().copied())
        .limit(limit);

    if let Some(until) = until {
        filter = filter.until(until);
    }

    Ok(client.fetch_events(filter).await?)
}

#[cfg(test)]
mod tests {
    use nostr_memory::MemoryDatabase;

    use super::*;
    use crate::Epoch;
    use crate::chat::{build_message, seal_rumor};
    use crate::derive::channel_group_key;
    use crate::stream::{
        KIND_WRAP, SealForm, build_rumor_ms, build_seal, channel_binding_tags, open_wrap, wrap_seal,
    };

    const SECRET: [u8; 32] = [0x07u8; 32];
    const NEXT_SECRET: [u8; 32] = [0x11u8; 32];

    /// What a relay does with an inclusive `until` and a `limit`.
    fn serve_page(
        relay: &BTreeSet<Event>,
        cursor: Option<Timestamp>,
        limit: usize,
    ) -> BTreeSet<Event> {
        let mut events: Vec<Event> = relay
            .iter()
            .filter(|event| cursor.is_none_or(|cursor| event.created_at <= cursor))
            .cloned()
            .collect();

        events.sort_by_key(|event| Reverse(event.created_at));
        events.truncate(limit);
        events.into_iter().collect()
    }

    #[test]
    fn history_pages_back_across_a_rekey() {
        let channel = ChannelId::from_bytes([0x9cu8; 32]);
        let author = Keys::generate();
        let held = [(Epoch(0), SECRET), (Epoch(1), NEXT_SECRET)];
        let planes = plane_keys(&held, &channel).expect("derives");

        // Three messages a second apart: a page boundary falls between each.
        let base = 1_700_000_000_000;
        let mut relay: BTreeSet<Event> = BTreeSet::new();

        for (content, secret, epoch, at_ms) in [
            ("before the rekey", &SECRET, Epoch(0), base),
            ("still before", &SECRET, Epoch(0), base + 1_000),
            ("after the rekey", &NEXT_SECRET, Epoch(1), base + 2_000),
        ] {
            let group = channel_group_key(secret, &channel, epoch).expect("derives");
            let rumor = build_message(
                author.public_key(),
                &channel,
                epoch,
                content,
                None,
                at_ms,
                None,
            );
            relay.insert(seal_rumor(&rumor, &group, &author, false).expect("seals").0);
        }

        let mut seen = BTreeSet::new();
        let mut found = Vec::new();
        let mut cursor = None;

        for _ in 0..3 {
            let page = serve_page(&relay, cursor, 2);
            let (fresh, next) = advance(&page, &planes, &channel, cursor, 2, &mut seen);

            found.extend(fresh.into_iter().map(|(_, rumor)| rumor));

            match next {
                Some(next) => cursor = Some(next),
                None => break,
            }
        }

        found.sort_by_key(|rumor| (Reverse(rumor.at_ms), rumor.id));

        let contents: Vec<&str> = found.iter().map(|rumor| rumor.content.as_str()).collect();
        assert_eq!(
            contents,
            ["after the rekey", "still before", "before the rekey"]
        );
    }

    #[test]
    fn rumors_read_back_after_a_restart() {
        let database = MemoryDatabase::unbounded();
        let channel = ChannelId::from_bytes([0xabu8; 32]);
        let author = Keys::generate();

        smol::block_on(async {
            let group = channel_group_key(&SECRET, &channel, Epoch(0)).expect("derives");

            for (content, at_ms) in [("first", 1_000_000u64), ("second", 2_000_000)] {
                let rumor = build_rumor_ms(
                    9,
                    author.public_key(),
                    content,
                    channel_binding_tags(&channel, Epoch(0)),
                    at_ms,
                );
                let seal = build_seal(&rumor, SealForm::Encrypted, &group, &author).expect("seals");
                let (wrap, _) = wrap_seal(
                    &seal,
                    &group,
                    KIND_WRAP,
                    Timestamp::from_secs(at_ms / 1000),
                    &[],
                )
                .expect("wraps");

                let opened = open_wrap(&wrap, &group).expect("opens");
                cache_rumor(&database, &channel, &opened)
                    .await
                    .expect("caches");
            }

            // The group key is gone; only the local cache stands in for it.
            let rumors = query_rumors(&database, &channel, None, 10)
                .await
                .expect("queries");
            assert_eq!(rumors.len(), 2, "both messages come back");
            assert_eq!(rumors[0].content, "second", "newest first");
            assert_eq!(rumors[1].content, "first");

            // A page boundary in message time, not in cache time.
            let until = Timestamp::from_secs(1_500);
            let page = query_rumors(&database, &channel, Some(until), 10)
                .await
                .expect("queries");
            assert_eq!(page.len(), 1);
            assert_eq!(page[0].content, "first");

            let capped = query_rumors(&database, &channel, None, 1)
                .await
                .expect("queries");
            assert_eq!(capped.len(), 1);
            assert_eq!(capped[0].content, "second");
        });
    }

    #[test]
    fn an_expired_rumor_is_refused_at_ingest_and_purged_by_the_sweep() {
        let database = MemoryDatabase::unbounded();
        let channel = ChannelId::from_bytes([0x77u8; 32]);
        let author = Keys::generate();
        let group = channel_group_key(&SECRET, &channel, Epoch(0)).expect("derives");
        let now = Timestamp::now().as_secs();

        smol::block_on(async {
            // A live timer is stored; one that already elapsed is refused at ingest.
            assert!(
                cache(
                    &database,
                    &group,
                    &channel,
                    &author,
                    "live",
                    Some(3_600),
                    now
                )
                .await
            );
            assert!(
                !cache(
                    &database,
                    &group,
                    &channel,
                    &author,
                    "gone",
                    Some(1),
                    now - 120
                )
                .await
            );

            let stored = query_rumors(&database, &channel, None, 10)
                .await
                .expect("queries");
            assert_eq!(stored.len(), 1);
            assert_eq!(stored[0].content, "live");

            // Hiding is not disappearing: the sweep removes the row itself,
            // judged on the rumor's own signed tag.
            let purged = purge_expired(&database, &channel, Timestamp::from_secs(now + 7_200))
                .await
                .expect("sweeps");
            assert_eq!(purged, 1);
            assert!(
                query_rumors(&database, &channel, None, 10)
                    .await
                    .expect("queries")
                    .is_empty()
            );

            // An untimed rumor is never swept, whatever the clock says.
            assert!(cache(&database, &group, &channel, &author, "timeless", None, now).await);
            let purged = purge_expired(&database, &channel, Timestamp::from_secs(now + 86_400))
                .await
                .expect("sweeps");
            assert_eq!(purged, 0);
        });
    }

    async fn cache(
        database: &MemoryDatabase,
        group: &GroupKey,
        channel: &ChannelId,
        author: &Keys,
        content: &str,
        timer: Option<u64>,
        at_secs: u64,
    ) -> bool {
        let rumor = build_message(
            author.public_key(),
            channel,
            Epoch(0),
            content,
            None,
            at_secs * 1_000,
            timer,
        );
        let (wrap, _) = seal_rumor(&rumor, group, author, false).expect("seals");
        let opened = open_wrap(&wrap, group).expect("opens");

        cache_rumor(database, channel, &opened)
            .await
            .expect("caches")
    }
}
