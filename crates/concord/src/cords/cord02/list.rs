use std::collections::BTreeMap;
use std::collections::btree_map::Entry;
use std::fmt;

use nostr_sdk::prelude::*;
use serde::{Deserialize, Serialize};

use crate::cord01::{self, NIP44_MAX_PLAINTEXT};
use crate::cord05::{ChannelGrant, CommunityInvite};
use crate::{CommunityId, Epoch, Extra};

pub const KIND_COMMUNITY_LIST: u16 = 13302;
pub const MAX_MEMBERSHIPS: usize = 50;

#[derive(Debug)]
pub enum ListError {
    Kind(u16),
    Crypto(String),
    Json(String),
    TooManyMemberships(usize),
    Oversize(usize),
}

impl fmt::Display for ListError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ListError::Kind(kind) => write!(f, "not a community list kind: {kind}"),
            ListError::Crypto(error) => write!(f, "crypto: {error}"),
            ListError::Json(error) => write!(f, "json: {error}"),
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

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct JoinMaterial {
    pub community_id: CommunityId,
    pub owner: PublicKey,
    pub owner_salt: String,
    pub community_root: String,
    pub root_epoch: Epoch,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub control_pk: Option<PublicKey>,
    /// Present only when the holder is staff.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub control_root: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub channels: Vec<ChannelGrant>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub relays: Vec<String>,
    pub name: String,
    #[serde(flatten)]
    pub extra: Extra,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CommunityListEntry {
    pub community_id: CommunityId,
    pub seed: JoinMaterial,
    pub current: JoinMaterial,
    pub added_at: u64,
    #[serde(flatten)]
    pub extra: Extra,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Tombstone {
    pub community_id: CommunityId,
    pub removed_at: u64,
    #[serde(flatten)]
    pub extra: Extra,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct CommunityList {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub entries: Vec<CommunityListEntry>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tombstones: Vec<Tombstone>,
    #[serde(flatten)]
    pub extra: Extra,
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
        control_root: control_root.map(|key| data_encoding::HEXLOWER.encode(key)),
        channels: invite.channels.clone(),
        relays: invite.relays.clone(),
        name: invite.name.clone(),
        extra: Extra::default(),
    }
}

pub fn merge(held: CommunityList, incoming: CommunityList) -> CommunityList {
    let mut entries: BTreeMap<CommunityId, CommunityListEntry> = BTreeMap::new();

    for entry in held.entries.into_iter().chain(incoming.entries) {
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
        entries: entries.into_values().collect(),
        tombstones: tombstones.into_values().collect(),
        extra,
    }
}

pub async fn build_list_event<S>(keys: &S, list: &CommunityList) -> Result<Event, ListError>
where
    S: AsyncGetPublicKey + AsyncSignEvent + AsyncNip44 + ?Sized,
{
    list.fits()?;

    let json = serde_json::to_string(list).map_err(json_error)?;
    let content = cord01::seal_to_self(keys, &json).await?;

    EventBuilder::new(Kind::Custom(KIND_COMMUNITY_LIST), content)
        .finalize_async(keys)
        .await
        .map_err(crypto_error)
}

pub async fn parse_list_event<S>(keys: &S, event: &Event) -> Result<CommunityList, ListError>
where
    S: AsyncGetPublicKey + AsyncNip44 + ?Sized,
{
    if event.kind.as_u16() != KIND_COMMUNITY_LIST {
        return Err(ListError::Kind(event.kind.as_u16()));
    }

    let json = cord01::open_to_self(keys, &event.content).await?;

    serde_json::from_str(&json).map_err(json_error)
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

pub(crate) fn union(into: &mut Extra, other: Extra) {
    for (key, value) in other {
        let replace = match into.get(&key) {
            Some(existing) => canonical(&value) < canonical(existing),
            None => true,
        };

        if replace {
            into.insert(key, value);
        }
    }
}

pub(crate) fn canonical<T: Serialize>(value: &T) -> String {
    serde_json::to_string(value).unwrap_or_default()
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
    fn a_tombstone_is_terminal_until_a_newer_join_outruns_it() {
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

        // A re-join genuinely newer than the removal does.
        let rejoined = list(vec![entry(
            id(0x11),
            material(id(0x11), owner, "Room", 0),
            material(id(0x11), owner, "Room", 0),
            7_000,
        )]);
        let live = merge(left, rejoined);
        assert!(live.is_live(&id(0x11)));

        // And the older removal is not re-applied on top of it.
        assert!(merge(live, removal(id(0x11), 6_000)).is_live(&id(0x11)));
    }

    #[test]
    fn a_second_device_reconstructs_membership_from_13302() {
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
            extra: Extra::default(),
        };

        let event = smol::block_on(build_list_event(&me, &mine)).expect("builds");
        assert_eq!(event.kind, Kind::Custom(KIND_COMMUNITY_LIST));
        assert_eq!(
            smol::block_on(parse_list_event(&me, &event)).expect("parses"),
            mine
        );
        assert!(
            !smol::block_on(parse_list_event(&me, &event))
                .expect("parses")
                .is_live(&id(0x33))
        );

        // Only the member's own keys open it, and an unreadable list is "no news".
        let stranger = Keys::generate();
        assert!(smol::block_on(parse_list_event(&stranger, &event)).is_err());

        // Unknown fields survive the round trip, so a republish cannot wipe them.
        let mut held = mine.clone();
        held.extra
            .insert("future".to_owned(), serde_json::json!({"deep": [1, 2]}));
        held.entries[0]
            .current
            .extra
            .insert("held_roots".to_owned(), serde_json::json!([{"epoch": 1}]));
        let rebuilt = smol::block_on(parse_list_event(
            &me,
            &smol::block_on(build_list_event(&me, &held)).expect("builds"),
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
            smol::block_on(build_list_event(&me, &crowded)),
            Err(ListError::TooManyMemberships(n)) if n == MAX_MEMBERSHIPS + 1
        ));

        let oversized = list(vec![entry(
            id(0x11),
            material(id(0x11), owner, &"x".repeat(NIP44_MAX_PLAINTEXT), 0),
            material(id(0x11), owner, "Room", 0),
            AT,
        )]);
        assert!(matches!(oversized.fits(), Err(ListError::Oversize(_))));
    }

    const AT: u64 = 1_719_800_000_000;
}
