use std::collections::btree_map::Entry;
use std::collections::{BTreeMap, BTreeSet};
use std::fmt;

use data_encoding::HEXLOWER;
use nostr_sdk::prelude::*;
use serde::{Deserialize, Deserializer, Serialize, Serializer};

use crate::cord01::{self, NIP44_MAX_PLAINTEXT};
use crate::cord05::{ChannelGrant, CommunityInvite};
use crate::utils::{base64_to_hex32, base64url, canonical, hex32_to_base64, union};
use crate::{ChannelId, CommunityId, Epoch, Extra};

pub const KIND_COMMUNITY_LIST: u16 = 33302;
pub const MAX_MEMBERSHIPS: usize = 50;

#[derive(Debug)]
pub enum ListError {
    Kind(u16),
    Crypto(String),
    Json(String),
    Encoding(String),
    Fragment(String),
    TooManyMemberships(usize),
    Oversize(usize),
}

impl fmt::Display for ListError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ListError::Kind(kind) => write!(f, "not a community list kind: {kind}"),
            ListError::Crypto(error) => write!(f, "crypto: {error}"),
            ListError::Json(error) => write!(f, "json: {error}"),
            ListError::Encoding(error) => write!(f, "encoding: {error}"),
            ListError::Fragment(error) => write!(f, "fragment: {error}"),
            ListError::TooManyMemberships(count) => {
                write!(
                    f,
                    "list carries {count} memberships (cap {MAX_MEMBERSHIPS})"
                )
            }
            ListError::Oversize(len) => {
                write!(f, "list is {len} bytes (cap {NIP44_MAX_PLAINTEXT})")
            }
        }
    }
}

impl std::error::Error for ListError {}

impl From<cord01::StreamError> for ListError {
    fn from(error: cord01::StreamError) -> Self {
        ListError::Crypto(error.to_string())
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct JoinMaterial {
    pub community_id: CommunityId,
    pub owner: PublicKey,
    pub owner_salt: String,
    pub community_root: String,
    pub root_epoch: Epoch,
    pub control_pk: Option<PublicKey>,
    /// Present only when the holder is staff.
    pub control_root: Option<String>,
    pub channels: Vec<ChannelGrant>,
    pub relays: Vec<String>,
    pub name: String,
    pub extra: Extra,
}

#[derive(Debug, Clone, PartialEq)]
pub struct CommunityListEntry {
    pub community_id: CommunityId,
    pub seed: JoinMaterial,
    pub current: JoinMaterial,
    pub added_at: u64,
    pub extra: Extra,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Tombstone {
    pub community_id: CommunityId,
    pub removed_at: u64,
    pub extra: Extra,
}

#[derive(Debug, Clone, PartialEq)]
pub struct CommunityList {
    /// How many fragments this List has. Every fragment declares it.
    pub frags: u64,
    pub entries: Vec<CommunityListEntry>,
    pub tombstones: Vec<Tombstone>,
    pub extra: Extra,
}

impl Default for CommunityList {
    fn default() -> Self {
        Self {
            frags: 1,
            entries: Vec::new(),
            tombstones: Vec::new(),
            extra: Extra::default(),
        }
    }
}

impl CommunityList {
    pub fn is_live(&self, community_id: &CommunityId) -> bool {
        let added = self
            .entries
            .iter()
            .find(|entry| entry.community_id == *community_id)
            .map(|entry| entry.added_at);

        match added {
            None => false,
            Some(added) => self
                .tombstones
                .iter()
                .find(|tombstone| tombstone.community_id == *community_id)
                .is_none_or(|tombstone| added > tombstone.removed_at),
        }
    }

    pub fn is_complete<I>(&self, held: I) -> bool
    where
        I: IntoIterator<Item = u64>,
    {
        let held: BTreeSet<u64> = held.into_iter().collect();
        (0..self.frags).all(|index| held.contains(&index))
    }

    pub fn fits(&self) -> Result<(), ListError> {
        if self.entries.len() > MAX_MEMBERSHIPS {
            return Err(ListError::TooManyMemberships(self.entries.len()));
        }

        let json = serde_json::to_string(self).map_err(json_error)?;

        if json.len() > NIP44_MAX_PLAINTEXT {
            return Err(ListError::Oversize(json.len()));
        }

        Ok(())
    }
}

pub fn join_material(invite: &CommunityInvite, control_root: Option<&[u8; 32]>) -> JoinMaterial {
    JoinMaterial {
        community_id: invite.community_id,
        owner: invite.owner,
        owner_salt: invite.owner_salt.clone(),
        community_root: invite.community_root.clone(),
        root_epoch: invite.root_epoch,
        control_pk: invite.control_pk,
        control_root: control_root.map(|key| HEXLOWER.encode(key)),
        channels: invite.channels.clone(),
        relays: invite.relays.clone(),
        name: invite.name.clone(),
        extra: Extra::default(),
    }
}

pub fn merge(held: CommunityList, incoming: CommunityList) -> CommunityList {
    let mut entries: BTreeMap<CommunityId, CommunityListEntry> = BTreeMap::new();

    for mut entry in held.entries.into_iter().chain(incoming.entries) {
        normalize(&mut entry);

        match entries.entry(entry.community_id) {
            Entry::Vacant(slot) => {
                slot.insert(entry);
            }
            Entry::Occupied(mut slot) => merge_entry(slot.get_mut(), entry),
        }
    }

    let mut tombstones: BTreeMap<CommunityId, Tombstone> = BTreeMap::new();

    for tombstone in held.tombstones.into_iter().chain(incoming.tombstones) {
        match tombstones.entry(tombstone.community_id) {
            Entry::Vacant(slot) => {
                slot.insert(tombstone);
            }
            Entry::Occupied(mut slot) => {
                let held = slot.get_mut();
                held.removed_at = held.removed_at.max(tombstone.removed_at);
                union(&mut held.extra, tombstone.extra);
            }
        }
    }

    let mut extra = held.extra;
    union(&mut extra, incoming.extra);

    CommunityList {
        frags: held.frags.max(incoming.frags),
        entries: entries.into_values().collect(),
        tombstones: tombstones.into_values().collect(),
        extra,
    }
}

pub async fn build_list_event<S>(
    signer: &S,
    list: &CommunityList,
    fragment: u64,
) -> Result<Event, ListError>
where
    S: AsyncGetPublicKey + AsyncSignEvent + AsyncNip44 + ?Sized,
{
    list.fits()?;

    let json = serde_json::to_string(list).map_err(json_error)?;
    let content = cord01::seal_to_self(signer, &json).await?;

    EventBuilder::new(Kind::Custom(KIND_COMMUNITY_LIST), content)
        .tag(Tag::identifier(fragment.to_string()))
        .finalize_async(signer)
        .await
        .map_err(crypto_error)
}

pub async fn parse_list_event<S>(signer: &S, event: &Event) -> Result<CommunityList, ListError>
where
    S: AsyncGetPublicKey + AsyncNip44 + ?Sized,
{
    if event.kind.as_u16() != KIND_COMMUNITY_LIST {
        return Err(ListError::Kind(event.kind.as_u16()));
    }

    fragment_index(event)?;

    let json = cord01::open_to_self(signer, &event.content).await?;

    serde_json::from_str(&json).map_err(json_error)
}

pub fn fragment_index(event: &Event) -> Result<u64, ListError> {
    let value = event
        .tags
        .identifier()
        .ok_or_else(|| ListError::Fragment("missing d tag".to_owned()))?;

    value
        .parse()
        .map_err(|_| ListError::Fragment(format!("d tag is not a fragment index: {value}")))
}

fn decode_base64(value: &str, field: &str) -> Result<[u8; 32], ListError> {
    base64url::decode_32(value).map_err(|error| ListError::Encoding(format!("{field}: {error}")))
}

fn decode_community_id(value: &str) -> Result<CommunityId, ListError> {
    Ok(CommunityId::from_bytes(decode_base64(
        value,
        "community_id",
    )?))
}

fn decode_public_key(value: &str, field: &str) -> Result<PublicKey, ListError> {
    PublicKey::from_slice(&decode_base64(value, field)?)
        .map_err(|error| ListError::Encoding(format!("{field}: {error}")))
}

fn encode_hex_32(value: &str, field: &str) -> Result<String, ListError> {
    hex32_to_base64(value).map_err(|error| ListError::Encoding(format!("{field}: {error}")))
}

fn decode_hex_32_base64(value: &str, field: &str) -> Result<String, ListError> {
    base64_to_hex32(value).map_err(|error| ListError::Encoding(format!("{field}: {error}")))
}

#[derive(Debug, Serialize, Deserialize)]
struct WireList {
    #[serde(default = "one_fragment")]
    frags: u64,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    entries: Vec<WireEntry>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    tombstones: Vec<WireTombstone>,
    #[serde(flatten)]
    extra: Extra,
}

fn one_fragment() -> u64 {
    1
}

#[derive(Debug, Serialize, Deserialize)]
struct WireEntry {
    community_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    seed: Option<WireSnapshot>,
    current: WireSnapshot,
    added_at: u64,
    #[serde(flatten)]
    extra: Extra,
}

#[derive(Debug, Serialize, Deserialize)]
struct WireTombstone {
    community_id: String,
    removed_at: u64,
    #[serde(flatten)]
    extra: Extra,
}

#[derive(Debug, Serialize, Deserialize)]
struct WireSnapshot {
    owner: String,
    owner_salt: String,
    community_root: String,
    root_epoch: Epoch,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    control_pk: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    control_root: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    channels: Vec<WireChannel>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    relays: Vec<String>,
    name: String,
    #[serde(flatten)]
    extra: Extra,
}

#[derive(Debug, Serialize, Deserialize)]
struct WireChannel {
    id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    key: Option<String>,
    epoch: Epoch,
    #[serde(default)]
    name: String,
    #[serde(flatten)]
    extra: Extra,
}

impl WireList {
    fn encode(list: &CommunityList) -> Result<Self, ListError> {
        // §8: a retired entry is not written — the tombstone alone carries the
        // state, since a membership is live only while an entry outranks its
        // removal. The entry stays in the in-memory document.
        let mut entries = Vec::with_capacity(list.entries.len());
        for entry in &list.entries {
            if list.is_live(&entry.community_id) {
                entries.push(WireEntry::encode(entry)?);
            }
        }

        let mut tombstones = Vec::with_capacity(list.tombstones.len());
        for tombstone in &list.tombstones {
            tombstones.push(WireTombstone::encode(tombstone)?);
        }

        Ok(Self {
            frags: list.frags,
            entries,
            tombstones,
            extra: list.extra.clone(),
        })
    }

    fn decode(self) -> Result<CommunityList, ListError> {
        let mut entries = Vec::with_capacity(self.entries.len());
        for entry in self.entries {
            entries.push(entry.decode()?);
        }

        let mut tombstones = Vec::with_capacity(self.tombstones.len());
        for tombstone in self.tombstones {
            tombstones.push(tombstone.decode()?);
        }

        Ok(CommunityList {
            frags: self.frags.max(1),
            entries,
            tombstones,
            extra: self.extra,
        })
    }
}

impl WireEntry {
    fn encode(entry: &CommunityListEntry) -> Result<Self, ListError> {
        let current = WireSnapshot::encode(&entry.current)?;

        let mut seed = entry.seed.clone();
        normalize_snapshot(&mut seed, &entry.current);

        let seed = if seed == entry.current {
            None
        } else {
            Some(WireSnapshot::encode(&seed)?)
        };

        Ok(Self {
            community_id: base64url::encode(entry.community_id.as_bytes()),
            seed,
            current,
            added_at: entry.added_at,
            extra: entry.extra.clone(),
        })
    }

    fn decode(self) -> Result<CommunityListEntry, ListError> {
        let community_id = decode_community_id(&self.community_id)?;
        let current = self.current.decode(community_id)?;
        let seed = match self.seed {
            Some(seed) => seed.decode(community_id)?,
            None => current.clone(),
        };

        let mut entry = CommunityListEntry {
            community_id,
            seed,
            current,
            added_at: self.added_at,
            extra: self.extra,
        };
        normalize(&mut entry);

        Ok(entry)
    }
}

impl WireTombstone {
    fn encode(tombstone: &Tombstone) -> Result<Self, ListError> {
        Ok(Self {
            community_id: base64url::encode(tombstone.community_id.as_bytes()),
            removed_at: tombstone.removed_at,
            extra: tombstone.extra.clone(),
        })
    }

    fn decode(self) -> Result<Tombstone, ListError> {
        Ok(Tombstone {
            community_id: decode_community_id(&self.community_id)?,
            removed_at: self.removed_at,
            extra: self.extra,
        })
    }
}

impl WireSnapshot {
    fn encode(material: &JoinMaterial) -> Result<Self, ListError> {
        let mut channels = Vec::with_capacity(material.channels.len());
        for channel in &material.channels {
            channels.push(WireChannel::encode(channel)?);
        }

        Ok(Self {
            owner: base64url::encode(&material.owner.to_bytes()),
            owner_salt: encode_hex_32(&material.owner_salt, "owner_salt")?,
            community_root: encode_hex_32(&material.community_root, "community_root")?,
            root_epoch: material.root_epoch,
            control_pk: material
                .control_pk
                .map(|key| base64url::encode(&key.to_bytes())),
            control_root: match &material.control_root {
                Some(root) => Some(encode_hex_32(root, "control_root")?),
                None => None,
            },
            channels,
            relays: material.relays.clone(),
            name: material.name.clone(),
            extra: material.extra.clone(),
        })
    }

    fn decode(self, community_id: CommunityId) -> Result<JoinMaterial, ListError> {
        let mut channels = Vec::with_capacity(self.channels.len());
        for channel in self.channels {
            channels.push(channel.decode()?);
        }

        Ok(JoinMaterial {
            community_id,
            owner: decode_public_key(&self.owner, "owner")?,
            owner_salt: decode_hex_32_base64(&self.owner_salt, "owner_salt")?,
            community_root: decode_hex_32_base64(&self.community_root, "community_root")?,
            root_epoch: self.root_epoch,
            control_pk: match self.control_pk {
                Some(value) => Some(decode_public_key(&value, "control_pk")?),
                None => None,
            },
            control_root: match self.control_root {
                Some(value) => Some(decode_hex_32_base64(&value, "control_root")?),
                None => None,
            },
            channels,
            relays: self.relays,
            name: self.name,
            extra: self.extra,
        })
    }
}

impl WireChannel {
    fn encode(grant: &ChannelGrant) -> Result<Self, ListError> {
        Ok(Self {
            id: base64url::encode(grant.id.as_bytes()),
            key: match &grant.key {
                Some(key) => Some(encode_hex_32(key, "channel key")?),
                None => None,
            },
            epoch: grant.epoch,
            name: grant.name.clone(),
            extra: grant.extra.clone(),
        })
    }

    fn decode(self) -> Result<ChannelGrant, ListError> {
        Ok(ChannelGrant {
            id: ChannelId::from_bytes(decode_base64(&self.id, "channel id")?),
            key: match self.key {
                Some(value) => Some(decode_hex_32_base64(&value, "channel key")?),
                None => None,
            },
            epoch: self.epoch,
            name: self.name,
            extra: self.extra,
        })
    }
}

impl Serialize for CommunityList {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        WireList::encode(self)
            .map_err(serde::ser::Error::custom)?
            .serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for CommunityList {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        WireList::deserialize(deserializer)?
            .decode()
            .map_err(serde::de::Error::custom)
    }
}

impl Serialize for JoinMaterial {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        WireSnapshot::encode(self)
            .map_err(serde::ser::Error::custom)?
            .serialize(serializer)
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Snapshot {
    Seed,
    Current,
}

fn merge_entry(held: &mut CommunityListEntry, incoming: CommunityListEntry) {
    held.added_at = held.added_at.max(incoming.added_at);
    held.seed = pick(&held.seed, &incoming.seed, Snapshot::Seed).clone();
    held.current = pick(&held.current, &incoming.current, Snapshot::Current).clone();
    union(&mut held.extra, incoming.extra);
    normalize(held);
}

fn pick<'a>(
    held: &'a JoinMaterial,
    incoming: &'a JoinMaterial,
    which: Snapshot,
) -> &'a JoinMaterial {
    let preferred = match which {
        Snapshot::Seed => incoming.root_epoch < held.root_epoch,
        Snapshot::Current => incoming.root_epoch > held.root_epoch,
    };

    if preferred {
        return incoming;
    }

    if incoming.root_epoch == held.root_epoch && canonical(incoming) < canonical(held) {
        return incoming;
    }

    held
}

fn normalize(entry: &mut CommunityListEntry) {
    entry.seed.community_id = entry.community_id;
    entry.current.community_id = entry.community_id;
    normalize_snapshot(&mut entry.seed, &entry.current);
}

fn normalize_snapshot(seed: &mut JoinMaterial, current: &JoinMaterial) {
    seed.community_id = current.community_id;
    seed.name = current.name.clone();
    seed.relays = current.relays.clone();

    for seed_channel in &mut seed.channels {
        if let Some(current_channel) = current
            .channels
            .iter()
            .find(|channel| channel.id == seed_channel.id)
        {
            seed_channel.name = current_channel.name.clone();
        }
    }
}

fn json_error(error: serde_json::Error) -> ListError {
    ListError::Json(error.to_string())
}

fn crypto_error(error: impl fmt::Display) -> ListError {
    ListError::Crypto(error.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn id(byte: u8) -> CommunityId {
        CommunityId::from_bytes([byte; 32])
    }

    fn material(
        community_id: CommunityId,
        owner: PublicKey,
        name: &str,
        epoch: u64,
    ) -> JoinMaterial {
        JoinMaterial {
            community_id,
            owner,
            owner_salt: "33".repeat(32),
            community_root: "44".repeat(32),
            root_epoch: Epoch(epoch),
            control_pk: None,
            control_root: None,
            channels: vec![],
            relays: vec!["wss://relay.example".to_owned()],
            name: name.to_owned(),
            extra: Extra::default(),
        }
    }

    fn entry(
        community_id: CommunityId,
        seed: JoinMaterial,
        current: JoinMaterial,
        added_at: u64,
    ) -> CommunityListEntry {
        CommunityListEntry {
            community_id,
            seed,
            current,
            added_at,
            extra: Extra::default(),
        }
    }

    fn list(entries: Vec<CommunityListEntry>) -> CommunityList {
        CommunityList {
            entries,
            ..Default::default()
        }
    }

    fn removal(community_id: CommunityId, removed_at: u64) -> CommunityList {
        CommunityList {
            tombstones: vec![Tombstone {
                community_id,
                removed_at,
                extra: Extra::default(),
            }],
            ..Default::default()
        }
    }

    #[test]
    fn merge_keeps_the_earlier_seed_and_the_later_current_either_way_round() {
        let owner = Keys::generate().public_key();
        let older = material(id(0x11), owner, "Room", 1);
        let newer = material(id(0x11), owner, "Room", 3);

        let a = list(vec![entry(id(0x11), older.clone(), newer.clone(), 5_000)]);
        let b = list(vec![entry(id(0x11), newer, older, 5_000)]);

        for merged in [merge(a.clone(), b.clone()), merge(b, a)] {
            let merged = merged.entries.first().expect("one membership");
            assert_eq!(
                merged.seed.root_epoch,
                Epoch(1),
                "seed anchors the earliest epoch held"
            );
            assert_eq!(merged.current.root_epoch, Epoch(3));
            assert_eq!(merged.added_at, 5_000);
        }

        // An epoch tie breaks on the whole snapshot's bytes, and does so for both
        // orders, so two devices never flap competing republishes.
        let alpha = material(id(0x11), owner, "Alpha", 2);
        let beta = material(id(0x11), owner, "Beta", 2);
        let a = list(vec![entry(id(0x11), alpha.clone(), alpha, 1)]);
        let b = list(vec![entry(id(0x11), beta.clone(), beta, 1)]);

        let first = merge(a.clone(), b.clone());
        assert_eq!(first, merge(b, a));
        assert_eq!(first.entries[0].current.name, "Alpha");
    }

    #[test]
    fn a_tombstone_is_terminal_and_the_entry_it_retires_is_never_written() {
        let owner = Keys::generate().public_key();
        let joined = entry(
            id(0x11),
            material(id(0x11), owner, "Room", 0),
            material(id(0x11), owner, "Room", 0),
            5_000,
        );

        let left = merge(list(vec![joined.clone()]), removal(id(0x11), 6_000));
        assert!(!left.is_live(&id(0x11)));
        assert_eq!(
            left.entries.len(),
            1,
            "a retired entry stays in the document"
        );

        // A stale device re-merging the entry cannot resurrect it.
        assert!(!merge(left.clone(), list(vec![joined.clone()])).is_live(&id(0x11)));

        // The tombstone alone is written, so the entry's key material leaves the wire.
        let written = serde_json::to_string(&left).expect("writes");
        assert!(
            !written.contains("\"entries\""),
            "a retired entry is not written"
        );
        let reparsed: CommunityList = serde_json::from_str(&written).expect("parses");
        assert_eq!(reparsed.tombstones.len(), 1);
        assert!(!reparsed.is_live(&id(0x11)));

        // A re-join genuinely newer than the removal does, and is written again.
        let rejoined = merge(
            reparsed,
            list(vec![entry(
                id(0x11),
                material(id(0x11), owner, "Room", 0),
                material(id(0x11), owner, "Room", 0),
                7_000,
            )]),
        );
        assert!(rejoined.is_live(&id(0x11)));
        assert!(
            serde_json::to_string(&rejoined)
                .expect("writes")
                .contains("\"entries\"")
        );

        // And the older removal is not re-applied on top of it.
        assert!(merge(rejoined, removal(id(0x11), 6_000)).is_live(&id(0x11)));
    }

    #[test]
    fn frags_disagreement_resolves_to_the_larger_value_and_completeness_is_by_index() {
        let two = CommunityList {
            frags: 2,
            ..Default::default()
        };
        let three = CommunityList {
            frags: 3,
            ..Default::default()
        };

        assert_eq!(
            merge(two.clone(), three.clone()).frags,
            3,
            "the larger fragment count wins"
        );
        assert_eq!(merge(three.clone(), two).frags, 3);

        assert!(!three.is_complete([0, 1]));
        assert!(three.is_complete([0, 1, 2]));
        assert!(three.is_complete([2, 1, 0]), "order does not matter");
        assert!(
            three.is_complete([0, 1, 2, 7]),
            "indices at or above frags are out of range"
        );
    }

    #[test]
    fn a_second_device_reconstructs_membership_from_the_list() {
        let me = Keys::generate();
        let owner = Keys::generate().public_key();
        let mine = CommunityList {
            entries: vec![
                entry(
                    id(0x11),
                    material(id(0x11), owner, "Room", 1),
                    material(id(0x11), owner, "Room", 4),
                    AT,
                ),
                entry(
                    id(0x22),
                    material(id(0x22), owner, "Other", 0),
                    material(id(0x22), owner, "Other", 0),
                    AT + 1,
                ),
            ],
            tombstones: vec![Tombstone {
                community_id: id(0x33),
                removed_at: AT,
                extra: Extra::default(),
            }],
            ..Default::default()
        };

        let event = smol::block_on(build_list_event(&me, &mine, 1)).expect("builds");
        assert_eq!(event.kind, Kind::Custom(KIND_COMMUNITY_LIST));
        assert_eq!(fragment_index(&event).expect("a fragment index"), 1);
        assert_eq!(
            smol::block_on(parse_list_event(&me, &event)).expect("parses"),
            mine
        );

        // Only the member's own keys open it, and an unreadable list is "no news".
        let stranger = Keys::generate();
        assert!(smol::block_on(parse_list_event(&stranger, &event)).is_err());

        // A fragment with no `d` tag is not a fragment at all.
        let untagged = EventBuilder::new(Kind::Custom(KIND_COMMUNITY_LIST), event.content.clone())
            .finalize(&me)
            .expect("signs");
        assert!(matches!(
            smol::block_on(parse_list_event(&me, &untagged)),
            Err(ListError::Fragment(_))
        ));

        // Unknown fields survive the round trip, so a republish cannot wipe them
        // — including on a channel, where a dropped field destroys key material.
        let mut held = mine.clone();
        held.extra
            .insert("future".to_owned(), serde_json::json!({"deep": [1, 2]}));
        held.entries[0]
            .current
            .extra
            .insert("held_roots".to_owned(), serde_json::json!([{"epoch": 1}]));
        held.entries[0].current.channels = vec![ChannelGrant {
            id: ChannelId::from_bytes([0x9c; 32]),
            key: Some("55".repeat(32)),
            epoch: Epoch(2),
            name: "staff".to_owned(),
            extra: Extra::default(),
        }];
        held.entries[0].current.channels[0]
            .extra
            .insert("read_key".to_owned(), serde_json::json!("aa".repeat(32)));
        let rebuilt = smol::block_on(parse_list_event(
            &me,
            &smol::block_on(build_list_event(&me, &held, 0)).expect("builds"),
        ))
        .expect("parses");
        assert_eq!(rebuilt, held);

        // The write gate refuses an over-cap or oversized List before publishing.
        let crowded = list(
            (0..=MAX_MEMBERSHIPS)
                .map(|index| {
                    let community_id = CommunityId::from_bytes([index as u8; 32]);
                    entry(
                        community_id,
                        material(community_id, owner, "Room", 0),
                        material(community_id, owner, "Room", 0),
                        AT,
                    )
                })
                .collect(),
        );
        assert!(matches!(
            smol::block_on(build_list_event(&me, &crowded, 0)),
            Err(ListError::TooManyMemberships(n)) if n == MAX_MEMBERSHIPS + 1
        ));

        let oversized = list(vec![entry(
            id(0x11),
            material(id(0x11), owner, "Room", 0),
            material(id(0x11), owner, &"x".repeat(NIP44_MAX_PLAINTEXT), 0),
            AT,
        )]);
        assert!(matches!(oversized.fits(), Err(ListError::Oversize(_))));
    }

    /// The worked example in `examples.md` §6.2, verbatim. Five of its base64url
    /// values leave non-zero trailing bits, so a strict decoder rejects them.
    const EXAMPLE: &str = r#"{
      "frags": 2,
      "entries": [
        {
          "community_id": "PxpVK3nQ7sB1yTfWm4dLxZ0aRcE9uHgKjNvOpQrStUv",
          "current": {
            "owner":          "nC7hQ2eRtYuIoPaSdFgHjKlZxCvBnM1qW3eR5tY7uI9",
            "owner_salt":     "qhEwR9tYuIoPaSdFgHjKlZxCvBnM1qW3eR5tY7uI0oP",
            "community_root": "d70Xa1QwErTyUiOpAsDfGhJkLzXcVbNm2Qw4Er6Ty8U",
            "root_epoch":     3,
            "control_pk":     "DU8vB4nM6qW1eR3tY5uI7oP9aS0dF2gH4jK6lZ8xC0v",
            "channels": [
              { "id":  "Ch1dQwErTyUiOpAsDfGhJkLzXcVbNm2Qw4Er6Ty8U0i",
                "key": "K3yAsDfGhJkLzXcVbNm1Qw2Er3Ty4Ui5Op6As7Df8Gh",
                "epoch": 2, "name": "staff" }
            ],
            "relays": ["wss://relay.example.com"],
            "name": "Example Community"
          },
          "added_at": 1719800000000
        }
      ],
      "tombstones": [
        { "community_id": "u9RfLmWx3PqZtYvBnKjHgFdSaQwErTyUiOp2C4E6G8I", "removed_at": 1722400000000 }
      ]
    }"#;

    #[test]
    fn the_spec_example_parses_and_the_writer_canonicalizes_it() {
        let parsed: CommunityList = serde_json::from_str(EXAMPLE).expect("the spec example parses");

        assert_eq!(parsed.frags, 2);
        assert_eq!(parsed.tombstones.len(), 1);

        let entry = parsed.entries.first().expect("one membership");
        assert_eq!(entry.current.root_epoch, Epoch(3));
        assert_eq!(entry.current.name, "Example Community");
        assert_eq!(entry.current.relays, ["wss://relay.example.com"]);
        assert!(
            entry.current.control_root.is_none(),
            "a non-staff snapshot holds no control_root"
        );
        assert_eq!(
            entry.seed, entry.current,
            "an absent seed reads as equal to current"
        );

        let channel = entry.current.channels.first().expect("a private channel");
        assert_eq!(channel.name, "staff");
        assert_eq!(channel.epoch, Epoch(2));
        assert!(channel.key.is_some());

        let written = serde_json::to_string(&parsed).expect("writes");
        assert!(written.contains("\"frags\":2"));
        assert!(
            !written.contains("\"seed\""),
            "a seed equal to current is omitted"
        );
        assert!(
            !written.contains("\"current\":{\"community_id\""),
            "an embedded snapshot omits community_id and inherits the entry's"
        );

        // Every writer emits the canonical spelling, so a non-canonical input is
        // stabilized here and two devices converge on identical bytes.
        let value: serde_json::Value = serde_json::from_str(&written).expect("valid");
        let owner = value["entries"][0]["current"]["owner"]
            .as_str()
            .expect("an owner");
        assert_eq!(owner, base64url::encode(&entry.current.owner.to_bytes()));
        assert_ne!(owner, "nC7hQ2eRtYuIoPaSdFgHjKlZxCvBnM1qW3eR5tY7uI9");

        // Reading its own output is a fixed point.
        let again: CommunityList = serde_json::from_str(&written).expect("parses");
        assert_eq!(again, parsed);
        assert_eq!(serde_json::to_string(&again).expect("writes"), written);
    }

    #[test]
    fn a_rename_rewrites_the_seed_cosmetics_and_collapses_the_snapshot() {
        let owner = Keys::generate().public_key();
        let channel = ChannelId::from_bytes([0x9c; 32]);

        let mut current = material(id(0x11), owner, "New name", 5);
        current.channels = vec![ChannelGrant {
            id: channel,
            key: Some("55".repeat(32)),
            epoch: Epoch(4),
            name: "new channel".to_owned(),
            extra: Extra::default(),
        }];

        let mut seed = material(id(0x11), owner, "Old name", 1);
        seed.relays = vec!["wss://stale.example".to_owned()];
        seed.channels = vec![ChannelGrant {
            id: channel,
            key: Some("55".repeat(32)),
            epoch: Epoch(2),
            name: "old channel".to_owned(),
            extra: Extra::default(),
        }];

        let written =
            serde_json::to_string(&list(vec![entry(id(0x11), seed, current.clone(), AT)]))
                .expect("writes");
        let value: serde_json::Value = serde_json::from_str(&written).expect("valid");
        let written_seed = &value["entries"][0]["seed"];
        assert_eq!(written_seed["name"], "New name");
        assert_eq!(written_seed["relays"][0], "wss://relay.example");
        assert_eq!(written_seed["channels"][0]["name"], "new channel");
        assert_eq!(
            written_seed["root_epoch"].as_u64(),
            Some(1),
            "the rewrite touches no key material"
        );

        // A seed that differs from current only in cosmetics is the same bytes
        // after the rewrite, so it is omitted entirely.
        let mut stale = material(id(0x11), owner, "Old name", 5);
        stale.channels = current.channels.clone();
        let collapsed = serde_json::to_string(&list(vec![entry(id(0x11), stale, current, AT)]))
            .expect("writes");
        assert!(!collapsed.contains("\"seed\""));
    }

    const AT: u64 = 1_719_800_000_000;
}
