use std::collections::{BTreeMap, BTreeSet};

use anyhow::Result;
use data_encoding::HEXLOWER;
use nostr_sdk::prelude::*;
use serde::{Deserialize, Serialize};

use crate::cord02::list::{CommunityListEntry, JoinMaterial};
use crate::cord02::{
    ChannelMetadata, CommunityGenesis, CommunityMetadata, ControlFold, ROOT_EPOCH,
};
use crate::cord04::{EntityHead, Floors, ParsedEdition, vsk};
use crate::cord05::ChannelGrant;
use crate::derive::control_signer_group_key;
use crate::{ChannelId, CommunityId, Epoch, Extra, decode_hex_32};

/// The `concord/` namespace for locally-keyed documents.
pub const STATE_PREFIX: &str = "concord/";

/// A key epoch the client still holds, retained so history stays readable.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct HeldKey {
    pub epoch: Epoch,
    pub key: [u8; 32],
    /// The publish time of the rotation that superseded this key.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub retired_at: Option<Timestamp>,
}

/// A community root epoch the client still holds, retained for the same reason.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct HeldRoot {
    pub epoch: Epoch,
    pub key: [u8; 32],
    /// The epoch's Control Plane signer, when the rotation delivered one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub control_pk: Option<PublicKey>,
    /// The publish time of the rotation that superseded this root.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub retired_at: Option<Timestamp>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChannelKeyRef {
    pub id: ChannelId,
    pub name: String,
    pub private: bool,
    pub epoch: Epoch,
    /// The channel's read secret when the member was granted it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub key: Option<[u8; 32]>,
    /// Keys this one superseded, retained so a rotation never blanks history.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub priors: Vec<HeldKey>,
}

impl ChannelKeyRef {
    /// The write coordinate: only the current epoch is ever published under.
    pub fn current(&self) -> Option<(Epoch, [u8; 32])> {
        self.key.map(|key| (self.epoch, key))
    }
}

/// How far a channel's history sync has reached, in wrap times.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChannelCursor {
    /// The newest wrap ingested, so a live subscription knows where to resume.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub newest: Option<Timestamp>,
    /// The oldest wrap paged back to, so the next round resumes below it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub oldest: Option<Timestamp>,
    /// History verifiably swept to the bottom.
    #[serde(default)]
    pub exhausted: bool,
}

impl ChannelCursor {
    pub fn merge(self, round: Self) -> Self {
        Self {
            newest: later(self.newest, round.newest),
            oldest: earlier(self.oldest, round.oldest),
            exhausted: self.exhausted || round.exhausted,
        }
    }
}

fn later(held: Option<Timestamp>, round: Option<Timestamp>) -> Option<Timestamp> {
    match (held, round) {
        (Some(held), Some(round)) => Some(held.max(round)),
        (held, None) => held,
        (None, round) => round,
    }
}

fn earlier(held: Option<Timestamp>, round: Option<Timestamp>) -> Option<Timestamp> {
    match (held, round) {
        (Some(held), Some(round)) => Some(held.min(round)),
        (held, None) => held,
        (None, round) => round,
    }
}

/// One local document per community, keyed by `concord/<community_id>`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CommunityState {
    pub id: CommunityId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
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
    /// Where each channel's history sync has reached.
    #[serde(
        default,
        rename = "channel_cursors",
        skip_serializing_if = "BTreeMap::is_empty"
    )]
    pub cursors: BTreeMap<ChannelId, ChannelCursor>,
    /// Root epochs the community has rotated past that this client still holds.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub held_roots: Vec<HeldRoot>,
    /// The epoch a channel rotation removed us at.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub channel_cuts: BTreeMap<ChannelId, Epoch>,
    /// The npubs whose rotation minted an epoch of this community we verified.
    #[serde(default, skip_serializing_if = "BTreeSet::is_empty")]
    pub refounders: BTreeSet<PublicKey>,
    /// The base epoch we were excluded at.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub removed_at: Option<Epoch>,
    /// A complete rotation ahead of our epoch predates our join and carries no blob.
    #[serde(default)]
    pub stranded: bool,
    #[serde(default)]
    pub dissolved: bool,
    /// When this community joined the member's list, in milliseconds.
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
        let mut name = None;

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
                    name = label(&metadata.name);
                }
                vsk::CHANNEL_METADATA => {
                    let metadata: ChannelMetadata = serde_json::from_str(&edition.content)?;
                    channels.push(ChannelKeyRef {
                        id: ChannelId::from_bytes(edition.entity),
                        name: metadata.name,
                        private: metadata.private,
                        epoch: ROOT_EPOCH,
                        key: None,
                        priors: Vec::new(),
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
            name,
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
            cursors: BTreeMap::new(),
            held_roots: Vec::new(),
            channel_cuts: BTreeMap::new(),
            refounders: BTreeSet::new(),
            removed_at: None,
            stranded: false,
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
                priors: Vec::new(),
            });
        }

        Ok(Self {
            id: material.community_id,
            name: label(&material.name),
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
            cursors: BTreeMap::new(),
            held_roots: Vec::new(),
            channel_cuts: BTreeMap::new(),
            refounders: BTreeSet::new(),
            removed_at: None,
            stranded: false,
            dissolved: false,
            added_at_ms,
        })
    }

    pub fn identifier(&self) -> String {
        state_identifier(&self.id)
    }

    /// Every root epoch we hold, the current one first.
    pub fn roots(&self) -> Vec<HeldRoot> {
        let mut roots = Vec::with_capacity(self.held_roots.len() + 1);
        roots.push(HeldRoot {
            epoch: self.root_epoch,
            key: self.community_root,
            control_pk: self.control_pks.get(&self.root_epoch.0).copied(),
            retired_at: None,
        });
        roots.extend(self.held_roots.iter().copied());
        roots
    }

    /// Every secret held for a channel, newest epoch first.
    pub fn held_keys(&self, channel: &ChannelId) -> Vec<HeldKey> {
        let Some(held) = self.channels.iter().find(|held| held.id == *channel) else {
            return Vec::new();
        };

        if held.private {
            let mut keys: Vec<HeldKey> = held
                .key
                .map(|key| HeldKey {
                    epoch: held.epoch,
                    key,
                    retired_at: None,
                })
                .into_iter()
                .collect();
            keys.extend(held.priors.iter().copied());
            return keys;
        }

        self.roots()
            .into_iter()
            .map(|root| HeldKey {
                epoch: root.epoch,
                key: root.key,
                retired_at: root.retired_at,
            })
            .collect()
    }

    /// Whether a channel rotation removed us at or after `epoch`.
    pub fn channel_cut(&self, channel: &ChannelId, epoch: Epoch) -> bool {
        self.channel_cuts
            .get(channel)
            .is_some_and(|cut| epoch <= *cut)
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

            if let Some(name) = label(&community.name) {
                self.name = Some(name);
            }
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
                    priors: Vec::new(),
                }),
                None => {}
            }
        }
    }
}

pub fn list_entry(state: &CommunityState, name: &str) -> CommunityListEntry {
    let material = JoinMaterial {
        community_id: state.id,
        owner: state.owner,
        owner_salt: HEXLOWER.encode(&state.owner_salt),
        community_root: HEXLOWER.encode(&state.community_root),
        root_epoch: state.root_epoch,
        control_pk: state.control_pks.get(&state.root_epoch.0).copied(),
        control_root: state.control_root.map(|root| HEXLOWER.encode(&root)),
        channels: state
            .channels
            .iter()
            .map(|channel| ChannelGrant {
                id: channel.id,
                key: channel.key.map(|key| HEXLOWER.encode(&key)),
                epoch: channel.epoch,
                name: channel.name.clone(),
                extra: Extra::default(),
            })
            .collect(),
        relays: state.relays.iter().map(RelayUrl::to_string).collect(),
        name: name.to_owned(),
        extra: Extra::default(),
    };

    CommunityListEntry {
        community_id: state.id,
        seed: material.clone(),
        current: material,
        added_at: state.added_at_ms,
        extra: Extra::default(),
    }
}

fn label(name: &str) -> Option<String> {
    let trimmed = name.trim();
    (!trimmed.is_empty()).then(|| trimmed.to_owned())
}

/// The local document key a community's state is stored under.
pub fn state_identifier(id: &CommunityId) -> String {
    format!("{STATE_PREFIX}{}", id.to_hex())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_cursor_merge_only_moves_forward_and_never_seals() {
        let held = ChannelCursor {
            newest: Some(Timestamp::from_secs(1_000)),
            oldest: Some(Timestamp::from_secs(5_000)),
            exhausted: false,
        };

        // An incomplete round reports nothing and moves neither bound.
        assert_eq!(held.merge(ChannelCursor::default()), held);

        let merged = held.merge(ChannelCursor {
            newest: Some(Timestamp::from_secs(2_000)),
            oldest: Some(Timestamp::from_secs(3_000)),
            exhausted: true,
        });
        assert_eq!(
            merged,
            ChannelCursor {
                newest: Some(Timestamp::from_secs(2_000)),
                oldest: Some(Timestamp::from_secs(3_000)),
                exhausted: true,
            }
        );

        // A later round that learned less cannot walk either bound back.
        assert_eq!(
            merged.merge(ChannelCursor {
                newest: Some(Timestamp::from_secs(1_500)),
                oldest: Some(Timestamp::from_secs(4_000)),
                exhausted: false,
            }),
            merged
        );
    }

    /// A cursor stored when the boundaries were milliseconds must not be read as
    /// seconds. The key it was stored under is gone, so the document's counters
    /// are ignored and the channel re-syncs rather than being sealed off by an
    /// `exhausted` that outlived the bounds it was earned against.
    #[test]
    fn a_cursor_stored_in_the_old_unit_is_dropped_rather_than_reinterpreted() {
        let channel = ChannelId::from_bytes([0x9c; 32]);
        let cursor = ChannelCursor {
            newest: Some(Timestamp::from_secs(1_700_000_000)),
            exhausted: true,
            ..ChannelCursor::default()
        };
        let mut state = CommunityState {
            id: CommunityId::from_bytes([0x42; 32]),
            name: None,
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

        // The document a version that stored milliseconds wrote: its own key,
        // and boundaries padded by a thousand.
        let mut stored = serde_json::Map::new();
        stored.insert(
            channel.to_hex(),
            serde_json::json!({
                "newest_ms": 1_700_000_000_000u64,
                "oldest_ms": 1_699_999_000_000u64,
                "exhausted": true
            }),
        );

        let mut legacy = serde_json::to_value(&state).expect("serializes");
        legacy
            .as_object_mut()
            .expect("a document")
            .insert("cursors".to_owned(), serde_json::Value::Object(stored));

        let read: CommunityState = serde_json::from_value(legacy).expect("deserializes");

        assert!(
            read.cursors.is_empty(),
            "a millisecond cursor is not a seconds cursor"
        );

        // A typed cursor still round-trips under the key it is written with.
        state.cursors.insert(channel, cursor);

        let document = serde_json::to_value(&state).expect("serializes");
        let read: CommunityState = serde_json::from_value(document).expect("deserializes");

        assert_eq!(read.cursors.get(&channel), Some(&cursor));
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
}
