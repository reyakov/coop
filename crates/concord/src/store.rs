use std::cmp::Reverse;
use std::collections::{BTreeMap, BTreeSet};
use std::sync::LazyLock;

use anyhow::{Result, anyhow};
use nostr_sdk::prelude::*;
use serde::{Deserialize, Serialize};

use crate::cord01::{KIND_WRAP_EPHEMERAL, OpenedStream};
use crate::cord02::list::JoinMaterial;
use crate::cord02::{
    ChannelMetadata, CommunityGenesis, CommunityMetadata, ControlFold, ROOT_EPOCH,
};
use crate::cord03::{self, ChatRumor, plane_keys};
use crate::cord04::{EntityHead, Floors, ParsedEdition, vsk};
use crate::derive::control_signer_group_key;
use crate::{ChannelId, CommunityId, Epoch, GroupKey, decode_hex_32};

static LOCAL_KEYS: LazyLock<Keys> = LazyLock::new(Keys::generate);

const MAX_PAGES: usize = 8;
const CHANNEL_TAG: SingleLetterTag = SingleLetterTag::LOWERCASE_C;
const MARK_TAG: SingleLetterTag = SingleLetterTag::LOWERCASE_T;
const MARK_VALUE: &str = "concord";
const WRAP_TAG: &str = "e";
const KIND_TAG: &str = "k";
/// The `concord/` namespace for locally-keyed documents.
pub const STATE_PREFIX: &str = "concord/";

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
        Tag::custom(KIND_TAG, [opened.rumor.kind.to_string()]),
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

pub async fn query_rumors(
    client: &Client,
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

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChannelKeyRef {
    pub id: ChannelId,
    pub name: String,
    pub private: bool,
    pub epoch: Epoch,
    /// The channel's read secret when the member was granted it.
    ///
    /// A public channel derives its key from the `community_root` and carries none.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub key: Option<[u8; 32]>,
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
                        key: None,
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

    pub fn from_join_material(material: &JoinMaterial, added_at_ms: u64) -> Result<Self> {
        let control_pks = match material.control_pk {
            Some(address) => BTreeMap::from([(material.root_epoch.0, address)]),
            None => BTreeMap::new(),
        };

        let mut channels = Vec::with_capacity(material.channels.len());
        for grant in &material.channels {
            let key = match &grant.key {
                Some(key) => Some(decode_hex_32(key)?),
                None => None,
            };

            channels.push(ChannelKeyRef {
                id: grant.id,
                name: grant.name.clone(),
                private: key.is_some(),
                epoch: grant.epoch,
                key,
            });
        }

        Ok(Self {
            id: material.community_id,
            owner: material.owner,
            owner_salt: decode_hex_32(&material.owner_salt)?,
            community_root: decode_hex_32(&material.community_root)?,
            root_epoch: material.root_epoch,
            control_root: match &material.control_root {
                Some(root) => Some(decode_hex_32(root)?),
                None => None,
            },
            control_pks,
            channels,
            relays: material
                .relays
                .iter()
                .filter_map(|relay| RelayUrl::parse(relay).ok())
                .collect(),
            heads: Vec::new(),
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
                    key: None,
                }),
                None => {}
            }
        }
    }
}

fn state_identifier(id: &CommunityId) -> String {
    format!("{STATE_PREFIX}{}", id.to_hex())
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

pub async fn backfill(
    client: &Client,
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
            if cache_rumor(client, channel, &opened).await? {
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

        let Ok((opened, rumor)) = cord03::open(wrap, group, channel, *epoch) else {
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
    use super::*;
    use crate::cord03::{build_message, seal_rumor};
    use crate::cord05::ChannelGrant;
    use crate::derive::channel_group_key;
    use crate::{Epoch, Extra};

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
            relay.insert(
                smol::block_on(seal_rumor(&rumor, &group, &author, false))
                    .expect("seals")
                    .0,
            );
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
    fn from_join_material_materializes_a_subscribable_state_with_or_without_the_control_root() {
        let owner = Keys::generate().public_key();
        let control_pk = Keys::generate().public_key();
        let staff = ChannelId::from_bytes([0x9c; 32]);
        let general = ChannelId::from_bytes([0x9d; 32]);

        let material = JoinMaterial {
            community_id: CommunityId::from_bytes([0x42; 32]),
            owner,
            owner_salt: "01".repeat(32),
            community_root: "02".repeat(32),
            root_epoch: Epoch(3),
            control_pk: Some(control_pk),
            control_root: Some("03".repeat(32)),
            channels: vec![
                ChannelGrant {
                    id: staff,
                    key: Some("04".repeat(32)),
                    epoch: Epoch(2),
                    name: "staff".to_owned(),
                    extra: Extra::default(),
                },
                ChannelGrant {
                    id: general,
                    key: None,
                    epoch: Epoch(0),
                    name: "general".to_owned(),
                    extra: Extra::default(),
                },
            ],
            relays: vec!["wss://relay.example".to_owned()],
            name: "Room".to_owned(),
            extra: Extra::default(),
        };

        let state = CommunityState::from_join_material(&material, 7).expect("materializes");

        assert_eq!(state.id, material.community_id);
        assert_eq!(state.owner, owner);
        assert_eq!(state.owner_salt, [0x01; 32]);
        assert_eq!(state.community_root, [0x02; 32]);
        assert_eq!(state.root_epoch, Epoch(3));
        assert_eq!(state.control_root, Some([0x03; 32]));
        assert_eq!(state.control_pks, BTreeMap::from([(3, control_pk)]));
        assert!(
            state.heads.is_empty(),
            "the first control fold fills the heads"
        );
        assert!(state.banned.is_empty());
        assert!(!state.dissolved);
        assert_eq!(state.relays.len(), 1);
        assert_eq!(state.added_at_ms, 7);

        // A granted key lands on the channel and makes it private; a grant with
        // no key is a public channel.
        let granted = state
            .channels
            .iter()
            .find(|c| c.id == staff)
            .expect("staff");
        assert!(granted.private);
        assert_eq!(granted.key, Some([0x04; 32]));
        assert_eq!(granted.epoch, Epoch(2));
        assert_eq!(granted.name, "staff");

        let public = state
            .channels
            .iter()
            .find(|c| c.id == general)
            .expect("general");
        assert!(!public.private);
        assert_eq!(public.key, None);

        // A member who is not staff carries no control_root, but reading needs no
        // secret: the address rides in the material either way.
        let mut member = material.clone();
        member.control_root = None;
        let state = CommunityState::from_join_material(&member, 7).expect("materializes");
        assert_eq!(state.control_root, None);
        assert_eq!(state.control_pks, BTreeMap::from([(3, control_pk)]));
    }

    #[test]
    fn load_states_reads_one_document_per_community_and_ignores_other_documents() {
        smol::block_on(async {
            let client = ClientBuilder::default()
                .database(nostr_memory::MemoryDatabase::unbounded())
                .build();

            let state = CommunityState {
                id: CommunityId::from_bytes([0x42; 32]),
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
}
