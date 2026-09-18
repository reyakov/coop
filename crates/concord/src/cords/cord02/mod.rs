pub mod guestbook;
pub mod list;

use std::collections::{BTreeMap, BTreeSet};

use anyhow::{Result, bail};
use nostr_sdk::prelude::{Event, Keys, PublicKey, Timestamp, UnsignedEvent};
use serde::{Deserialize, Serialize};

use crate::cord01::{KIND_WRAP, SealForm, build_seal, open_wrap_at, wrap_seal_with};
use crate::cord04::roles::{
    AuthorityEdition, CommunityRoles, Grant, MAX_BANLIST, Permissions, Role, Roster, citation_ok,
    fold_roster,
};
use crate::cord04::{
    AuthorityCitation, EditionFields, EditionMeta, EntityHead, Floors, ParsedEdition,
    build_edition, fold_head, parse_edition, vsk,
};
use crate::derive::{
    banlist_locator, community_id_of, control_group_key, control_signer_group_key, grant_locator,
    invite_links_locator, pins_locator, verify_community_id,
};
use crate::{ChannelId, CommunityId, Epoch, Extra, GroupKey, random_32};

pub const MAX_NAME_BYTES: usize = 64;
pub const MAX_DESCRIPTION_BYTES: usize = 10_000;
pub const MAX_RELAYS: usize = 5;
pub const MAX_REGISTRY_LINKS: usize = 64;

pub const GENERAL_CHANNEL: &str = "general";
pub const ROOT_EPOCH: Epoch = Epoch(0);

const GENESIS_VERSION: u64 = 1;

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ImageRef {
    pub url: String,
    pub key: String,
    pub nonce: String,
    pub hash: String,
    #[serde(flatten)]
    pub extra: Extra,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct CommunityMetadata {
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub relays: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub icon: Option<ImageRef>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub banner: Option<ImageRef>,
    /// CORD-08's disappearing-messages timer, in seconds.
    #[serde(
        default,
        deserialize_with = "timer_seconds",
        skip_serializing_if = "Option::is_none"
    )]
    pub message_expiration: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub custom: Option<Extra>,
    #[serde(flatten)]
    pub extra: Extra,
}

/// CORD-08 §1: absent, `0` and malformed all mean off.
fn timer_seconds<'de, D>(deserializer: D) -> Result<Option<u64>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let value = Option::<serde_json::Value>::deserialize(deserializer)?;

    Ok(value
        .and_then(|value| value.as_u64())
        .filter(|seconds| *seconds > 0))
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ChannelMetadata {
    pub name: String,
    pub private: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub voice: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub deleted: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub custom: Option<Extra>,
    #[serde(flatten)]
    pub extra: Extra,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommunityIdentity {
    pub community_id: CommunityId,
    pub owner: PublicKey,
    pub owner_salt: [u8; 32],
}

impl CommunityIdentity {
    pub fn verify(&self) -> bool {
        verify_community_id(&self.community_id, &self.owner.to_bytes(), &self.owner_salt)
    }
}

#[derive(Debug, Clone)]
pub struct CommunityGenesis {
    pub identity: CommunityIdentity,
    pub community_root: [u8; 32],
    pub control_root: [u8; 32],
    pub channel_id: ChannelId,
    pub wraps: Vec<Event>,
}

pub fn genesis(
    owner: &Keys,
    metadata: &CommunityMetadata,
    at_secs: u64,
) -> Result<CommunityGenesis> {
    let metadata_content = encode_metadata(metadata)?;
    let owner_salt = random_32()?;

    let identity = CommunityIdentity {
        community_id: community_id_of(&owner.public_key().to_bytes(), &owner_salt),
        owner: owner.public_key(),
        owner_salt,
    };

    let community_root = random_32()?;
    let control_root = random_32()?;
    let channel_id = ChannelId::from_bytes(random_32()?);

    let read = control_group_key(&community_root, &identity.community_id, ROOT_EPOCH)?;
    let signer = control_signer_group_key(&control_root, &identity.community_id, ROOT_EPOCH)?;

    let channel_content = serde_json::to_string(&ChannelMetadata {
        name: GENERAL_CHANNEL.to_owned(),
        private: false,
        ..ChannelMetadata::default()
    })?;

    let editions = [
        build_edition(EditionFields {
            author: identity.owner,
            subkind: vsk::COMMUNITY_METADATA,
            entity: *identity.community_id.as_bytes(),
            version: GENESIS_VERSION,
            prev: None,
            citation: None,
            content: &metadata_content,
            at_secs,
        }),
        build_edition(EditionFields {
            author: identity.owner,
            subkind: vsk::CHANNEL_METADATA,
            entity: *channel_id.as_bytes(),
            version: GENESIS_VERSION,
            prev: None,
            citation: None,
            content: &channel_content,
            at_secs,
        }),
    ];

    let mut wraps = Vec::with_capacity(editions.len());

    for edition in &editions {
        wraps.push(seal_edition(edition, owner, &read, &signer, at_secs)?);
    }

    Ok(CommunityGenesis {
        identity,
        community_root,
        control_root,
        channel_id,
        wraps,
    })
}

/// Opens a Control Plane wrap from its reading key alone.
pub fn open_edition(
    wrap: &Event,
    read: &GroupKey,
    address: &PublicKey,
    verify_wrap_signature: bool,
) -> Result<ParsedEdition> {
    let opened = open_wrap_at(wrap, address, read.conversation(), verify_wrap_signature)?;

    if opened.seal_form != SealForm::Plaintext {
        bail!("control editions require a plaintext seal");
    }

    Ok(parse_edition(&opened.rumor)?)
}

pub struct ControlWriter {
    pub author: PublicKey,
    pub read: GroupKey,
    pub signer: GroupKey,
}

pub struct Edition<'a> {
    pub subkind: &'a str,
    pub entity: [u8; 32],
    pub content: &'a str,
    /// The head this edition supersedes; `None` starts the chain.
    pub head: Option<&'a EntityHead>,
    pub citation: Option<AuthorityCitation>,
}

impl ControlWriter {
    pub fn publish(
        &self,
        keys: &Keys,
        edition: Edition<'_>,
        at_secs: u64,
    ) -> Result<(Event, EntityHead)> {
        let rumor = build_edition(EditionFields {
            author: self.author,
            subkind: edition.subkind,
            entity: edition.entity,
            version: edition
                .head
                .map_or(GENESIS_VERSION, |head| head.version + 1),
            prev: edition.head.map(|head| head.self_hash),
            citation: edition.citation,
            content: edition.content,
            at_secs,
        });

        let parsed = parse_edition(&rumor)?;
        let wrap = seal_edition(&rumor, keys, &self.read, &self.signer, at_secs)?;

        Ok((wrap, EntityHead::from(&parsed)))
    }

    pub fn set_community_metadata(
        &self,
        keys: &Keys,
        community_id: &CommunityId,
        metadata: &CommunityMetadata,
        head: Option<&EntityHead>,
        citation: Option<AuthorityCitation>,
        at_secs: u64,
    ) -> Result<(Event, EntityHead)> {
        let content = encode_metadata(metadata)?;

        self.publish(
            keys,
            Edition {
                subkind: vsk::COMMUNITY_METADATA,
                entity: *community_id.as_bytes(),
                content: &content,
                head,
                citation,
            },
            at_secs,
        )
    }

    pub fn set_channel_metadata(
        &self,
        keys: &Keys,
        channel: &ChannelId,
        metadata: &ChannelMetadata,
        head: Option<&EntityHead>,
        citation: Option<AuthorityCitation>,
        at_secs: u64,
    ) -> Result<(Event, EntityHead)> {
        if metadata.name.len() > MAX_NAME_BYTES {
            bail!("channel name exceeds {MAX_NAME_BYTES} bytes");
        }

        let content = serde_json::to_string(metadata)?;

        self.publish(
            keys,
            Edition {
                subkind: vsk::CHANNEL_METADATA,
                entity: *channel.as_bytes(),
                content: &content,
                head,
                citation,
            },
            at_secs,
        )
    }

    pub fn set_role(
        &self,
        keys: &Keys,
        role: &Role,
        head: Option<&EntityHead>,
        citation: Option<AuthorityCitation>,
        at_secs: u64,
    ) -> Result<(Event, EntityHead)> {
        if role.name.len() > MAX_NAME_BYTES {
            bail!("role name exceeds {MAX_NAME_BYTES} bytes");
        }

        let content = role.to_content()?;

        self.publish(
            keys,
            Edition {
                subkind: vsk::ROLE,
                entity: *role.role_id.as_bytes(),
                content: &content,
                head,
                citation,
            },
            at_secs,
        )
    }

    pub fn set_grant(
        &self,
        keys: &Keys,
        community_id: &CommunityId,
        grant: &Grant,
        head: Option<&EntityHead>,
        citation: Option<AuthorityCitation>,
        at_secs: u64,
    ) -> Result<(Event, EntityHead)> {
        let content = grant.to_content()?;

        self.publish(
            keys,
            Edition {
                subkind: vsk::GRANT,
                entity: grant_locator(community_id, &grant.member.to_bytes()),
                content: &content,
                head,
                citation,
            },
            at_secs,
        )
    }

    pub fn set_banlist(
        &self,
        keys: &Keys,
        community_id: &CommunityId,
        banned: &BTreeSet<PublicKey>,
        head: Option<&EntityHead>,
        citation: Option<AuthorityCitation>,
        at_secs: u64,
    ) -> Result<(Event, EntityHead)> {
        if banned.len() > MAX_BANLIST {
            bail!("banlist exceeds {MAX_BANLIST} entries");
        }

        let entries: Vec<String> = banned.iter().map(PublicKey::to_hex).collect();
        let content = serde_json::to_string(&entries)?;

        self.publish(
            keys,
            Edition {
                subkind: vsk::BANLIST,
                entity: banlist_locator(community_id),
                content: &content,
                head,
                citation,
            },
            at_secs,
        )
    }

    #[allow(clippy::too_many_arguments)]
    pub fn set_registry(
        &self,
        keys: &Keys,
        community_id: &CommunityId,
        creator: &PublicKey,
        links: &[PublicKey],
        head: Option<&EntityHead>,
        citation: Option<AuthorityCitation>,
        at_secs: u64,
    ) -> Result<(Event, EntityHead)> {
        let entries: Vec<String> = links
            .iter()
            .take(MAX_REGISTRY_LINKS)
            .map(PublicKey::to_hex)
            .collect();
        let content = serde_json::to_string(&entries)?;

        self.publish(
            keys,
            Edition {
                subkind: vsk::INVITE_LINKS,
                entity: invite_links_locator(community_id, &creator.to_bytes()),
                content: &content,
                head,
                citation,
            },
            at_secs,
        )
    }

    /// The whole Pin List, in whichever of CORD-04 §7's two forms the Channel calls for.
    #[allow(clippy::too_many_arguments)]
    pub fn set_pin_list(
        &self,
        keys: &Keys,
        community_id: &CommunityId,
        channel: &ChannelId,
        content: &str,
        head: Option<&EntityHead>,
        citation: Option<AuthorityCitation>,
        at_secs: u64,
    ) -> Result<(Event, EntityHead)> {
        self.publish(
            keys,
            Edition {
                subkind: vsk::PINS,
                entity: pins_locator(community_id, channel),
                content,
                head,
                citation,
            },
            at_secs,
        )
    }
}

fn encode_metadata(metadata: &CommunityMetadata) -> Result<String> {
    if metadata.name.len() > MAX_NAME_BYTES {
        bail!("community name exceeds {MAX_NAME_BYTES} bytes");
    }

    if metadata
        .description
        .as_ref()
        .is_some_and(|description| description.len() > MAX_DESCRIPTION_BYTES)
    {
        bail!("community description exceeds {MAX_DESCRIPTION_BYTES} bytes");
    }

    let mut metadata = metadata.clone();
    metadata.relays.truncate(MAX_RELAYS);
    metadata.message_expiration = metadata.message_expiration.filter(|seconds| *seconds > 0);

    Ok(serde_json::to_string(&metadata)?)
}

#[derive(Debug, Clone, Default)]
pub struct ControlFold {
    pub roles: CommunityRoles,
    pub banned: BTreeSet<PublicKey>,
    pub community: Option<CommunityMetadata>,
    pub channels: BTreeMap<ChannelId, ChannelMetadata>,
    /// Each creator's live link-signer set.
    pub registries: BTreeMap<PublicKey, Vec<PublicKey>>,
    /// Head content per `pins_locator`; the coordinate is one-way, so a fold cannot name its Channel.
    pub pins: BTreeMap<[u8; 32], String>,
    pub floors: Floors,
    pub gapped: bool,
}

impl ControlFold {
    pub fn is_public(&self) -> bool {
        self.registries.values().any(|links| !links.is_empty())
    }

    pub fn pin_content(&self, community_id: &CommunityId, channel: &ChannelId) -> Option<&str> {
        self.pins
            .get(&pins_locator(community_id, channel))
            .map(String::as_str)
    }
}

pub fn fold_control(
    owner: &PublicKey,
    community_id: &CommunityId,
    editions: &[ParsedEdition],
    floors: &Floors,
    held_bans: &BTreeSet<PublicKey>,
) -> ControlFold {
    let authority: Vec<AuthorityEdition> = editions
        .iter()
        .filter_map(|edition| AuthorityEdition::parse(edition, community_id))
        .collect();

    let roster = fold_roster(owner, community_id, &authority, floors, held_bans);
    let metadata = fold_metadata(owner, community_id, editions, &roster, floors);

    let mut floors = roster.floors;
    floors.extend(metadata.floors);

    ControlFold {
        roles: roster.roles,
        banned: roster.banned,
        community: metadata.community,
        channels: metadata.channels,
        registries: metadata.registries,
        pins: metadata.pins,
        floors,
        gapped: roster.gapped || metadata.gapped,
    }
}

#[derive(Debug, Default)]
struct MetadataFold {
    community: Option<CommunityMetadata>,
    channels: BTreeMap<ChannelId, ChannelMetadata>,
    registries: BTreeMap<PublicKey, Vec<PublicKey>>,
    pins: BTreeMap<[u8; 32], String>,
    floors: Floors,
    gapped: bool,
}

fn fold_metadata(
    owner: &PublicKey,
    community_id: &CommunityId,
    editions: &[ParsedEdition],
    roster: &Roster,
    floors: &Floors,
) -> MetadataFold {
    let judge = Judge {
        owner,
        community_id,
        roster,
        floors,
    };
    let community_entity = *community_id.as_bytes();
    let mut community: Vec<&ParsedEdition> = Vec::new();
    let mut channels: BTreeMap<[u8; 32], Vec<&ParsedEdition>> = BTreeMap::new();

    for edition in editions {
        match edition.subkind.as_str() {
            // A channel at the community's own coordinate would corrupt the metadata chain's floor.
            vsk::COMMUNITY_METADATA if edition.entity == community_entity => {
                community.push(edition)
            }
            vsk::CHANNEL_METADATA if edition.entity != community_entity => {
                channels.entry(edition.entity).or_default().push(edition);
            }
            _ => {}
        }
    }

    let mut fold = MetadataFold::default();

    if let Some(head) = authorized_head(
        &judge,
        community_entity,
        &community,
        Permissions::MANAGE_METADATA,
        &mut fold.gapped,
    ) {
        fold.community = serde_json::from_str::<CommunityMetadata>(&head.content)
            .ok()
            .map(|mut metadata| {
                // Up to 5 relays is a recommendation, so a longer set is truncated, not refused.
                metadata.relays.truncate(MAX_RELAYS);
                metadata
            });
        fold.floors.insert(head.entity, EntityHead::from(head));
    }

    for (entity, candidates) in &channels {
        let Some(head) = authorized_head(
            &judge,
            *entity,
            candidates,
            Permissions::MANAGE_CHANNELS,
            &mut fold.gapped,
        ) else {
            continue;
        };

        fold.floors.insert(*entity, EntityHead::from(head));

        if let Ok(metadata) = serde_json::from_str::<ChannelMetadata>(&head.content) {
            fold.channels
                .insert(ChannelId::from_bytes(*entity), metadata);
        }
    }

    fold.registries = fold_registries(&judge, editions, &mut fold.floors, &mut fold.gapped);
    fold.pins = fold_pins(&judge, editions, &mut fold.floors, &mut fold.gapped);

    fold
}

/// A one-way coordinate leaves the `eid` unchecked; violating content reads as empty.
fn fold_pins(
    judge: &Judge<'_>,
    editions: &[ParsedEdition],
    floors: &mut Floors,
    gapped: &mut bool,
) -> BTreeMap<[u8; 32], String> {
    let mut candidates: BTreeMap<[u8; 32], Vec<&ParsedEdition>> = BTreeMap::new();

    for edition in editions {
        if edition.subkind == vsk::PINS {
            candidates.entry(edition.entity).or_default().push(edition);
        }
    }

    let mut pins = BTreeMap::new();

    for (entity, group) in &candidates {
        let Some(head) = authorized_head(judge, *entity, group, Permissions::PIN_MESSAGES, gapped)
        else {
            continue;
        };

        floors.insert(*entity, EntityHead::from(head));
        pins.insert(*entity, head.content.clone());
    }

    pins
}

fn fold_registries(
    judge: &Judge<'_>,
    editions: &[ParsedEdition],
    floors: &mut Floors,
    gapped: &mut bool,
) -> BTreeMap<PublicKey, Vec<PublicKey>> {
    let mut candidates: BTreeMap<[u8; 32], Vec<&ParsedEdition>> = BTreeMap::new();

    for edition in editions {
        if edition.subkind == vsk::INVITE_LINKS
            && invite_links_locator(judge.community_id, &edition.author.to_bytes())
                == edition.entity
        {
            candidates.entry(edition.entity).or_default().push(edition);
        }
    }

    let mut registries = BTreeMap::new();

    for (entity, group) in &candidates {
        let Some(head) = authorized_head(judge, *entity, group, Permissions::CREATE_INVITE, gapped)
        else {
            continue;
        };

        floors.insert(*entity, EntityHead::from(head));

        let Ok(links) = serde_json::from_str::<Vec<String>>(&head.content) else {
            continue;
        };

        registries.insert(
            head.author,
            links
                .iter()
                .filter_map(|link| PublicKey::from_hex(link).ok())
                .take(MAX_REGISTRY_LINKS)
                .collect(),
        );
    }

    registries
}

struct Judge<'a> {
    owner: &'a PublicKey,
    community_id: &'a CommunityId,
    roster: &'a Roster,
    floors: &'a Floors,
}

fn authorized_head<'a>(
    judge: &Judge<'_>,
    entity: [u8; 32],
    candidates: &[&'a ParsedEdition],
    permission: u64,
    gapped: &mut bool,
) -> Option<&'a ParsedEdition> {
    let authorized: Vec<&ParsedEdition> = candidates
        .iter()
        .copied()
        .filter(|edition| {
            // A banned npub's edits are dropped even while a grant naming them still carries the bit.
            !judge.roster.banned.contains(&edition.author)
                && judge
                    .roster
                    .roles
                    .is_authorized(&edition.author, judge.owner, permission)
                && citation_ok(
                    judge.owner,
                    judge.community_id,
                    &edition.author,
                    edition.citation.as_ref(),
                    &judge.roster.floors,
                )
        })
        .collect();

    if authorized.is_empty() {
        return None;
    }

    let metas: Vec<EditionMeta> = authorized
        .iter()
        .map(|edition| EditionMeta::from(*edition))
        .collect();

    let selection = fold_head(&metas, judge.floors.get(&entity));
    *gapped |= selection.gap;

    selection.head.map(|index| authorized[index])
}

fn seal_edition(
    edition: &UnsignedEvent,
    owner: &Keys,
    read: &GroupKey,
    signer: &GroupKey,
    at_secs: u64,
) -> Result<Event> {
    let seal = build_seal(edition, SealForm::Plaintext, read, owner)?;

    let (wrap, _) = wrap_seal_with(
        &seal,
        read.conversation(),
        signer.keys(),
        KIND_WRAP,
        Timestamp::from_secs(at_secs),
        &[],
    )?;

    Ok(wrap)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cord03::{self, build_message, seal_rumor};
    use crate::cord04::pins;
    use crate::cord04::roles::{Grant, MAX_BANLIST, MAX_ROLES_PER_MEMBER, Role, RoleScope};
    use crate::derive::{channel_group_key, grant_locator};
    use crate::store::CommunityState;
    use crate::{Extra, RoleId};

    const AT: u64 = 1_700_000_000;

    fn holder(minted: &CommunityGenesis) -> (GroupKey, GroupKey) {
        let community_id = minted.identity.community_id;

        (
            control_group_key(&minted.community_root, &community_id, ROOT_EPOCH).expect("derives"),
            control_signer_group_key(&minted.control_root, &community_id, ROOT_EPOCH)
                .expect("derives"),
        )
    }

    fn open_all(wraps: &[Event], read: &GroupKey, address: &PublicKey) -> Vec<ParsedEdition> {
        wraps
            .iter()
            .map(|wrap| open_edition(wrap, read, address, true).expect("opens"))
            .collect()
    }

    fn metadata(name: &str) -> CommunityMetadata {
        CommunityMetadata {
            name: name.to_owned(),
            ..CommunityMetadata::default()
        }
    }

    #[test]
    fn metadata_and_channel_edits_reach_a_second_client() {
        let owner = Keys::generate();
        let minted = genesis(&owner, &metadata("coop"), AT).expect("mints");
        let community_id = minted.identity.community_id;
        let owner_pk = owner.public_key();
        let (read, signer) = holder(&minted);

        let genesis_editions = open_all(&minted.wraps, &read, &signer.pk());
        let roster = fold_control(
            &owner_pk,
            &community_id,
            &genesis_editions,
            &Floors::new(),
            &BTreeSet::new(),
        );
        assert_eq!(
            roster.community.as_ref().map(|meta| meta.name.as_str()),
            Some("coop")
        );

        let writer = ControlWriter {
            author: owner_pk,
            read: read.clone(),
            signer: signer.clone(),
        };
        let community_head = roster.floors.get(community_id.as_bytes()).expect("head");
        let channel_head = roster
            .floors
            .get(minted.channel_id.as_bytes())
            .expect("head");

        let (community_wrap, _) = writer
            .set_community_metadata(
                &owner,
                &community_id,
                &CommunityMetadata {
                    relays: vec!["wss://relay.example".to_owned()],
                    ..metadata("coop two")
                },
                Some(community_head),
                None,
                AT + 1,
            )
            .expect("publishes");
        let (channel_wrap, _) = writer
            .set_channel_metadata(
                &owner,
                &minted.channel_id,
                &ChannelMetadata {
                    name: "lobby".to_owned(),
                    private: false,
                    ..ChannelMetadata::default()
                },
                Some(channel_head),
                None,
                AT + 2,
            )
            .expect("publishes");

        let mut edited = genesis_editions.clone();
        edited.extend(open_all(
            &[community_wrap, channel_wrap],
            &read,
            &signer.pk(),
        ));

        let folded = fold_control(
            &owner_pk,
            &community_id,
            &edited,
            &Floors::new(),
            &BTreeSet::new(),
        );
        assert_eq!(
            folded.community.as_ref().map(|meta| meta.name.as_str()),
            Some("coop two")
        );
        assert_eq!(
            folded
                .channels
                .get(&minted.channel_id)
                .map(|channel| channel.name.as_str()),
            Some("lobby")
        );

        // A relay serving only the editions a client already folded past must not walk
        // the community backwards.
        let stale = fold_control(
            &owner_pk,
            &community_id,
            &genesis_editions,
            &folded.floors,
            &BTreeSet::new(),
        );
        assert!(stale.community.is_none());
        assert!(stale.channels.is_empty());

        let mut state =
            CommunityState::from_genesis(&minted, &genesis_editions, AT * 1_000).expect("projects");
        state.apply_fold(&folded);
        assert_eq!(state.channels.len(), 1);
        assert_eq!(state.channels[0].name, "lobby");
        assert_eq!(state.relays.len(), 1);
    }

    #[test]
    fn a_delegated_member_edits_metadata_only_under_its_own_grant() {
        let owner = Keys::generate();
        let member = Keys::generate();
        let minted = genesis(&owner, &metadata("coop"), AT).expect("mints");
        let community_id = minted.identity.community_id;
        let owner_pk = owner.public_key();
        let (read, signer) = holder(&minted);

        let writer = ControlWriter {
            author: owner_pk,
            read: read.clone(),
            signer: signer.clone(),
        };
        let role_id = RoleId::from_bytes([0x07; 32]);
        let role = Role {
            role_id,
            name: "Mod".to_owned(),
            position: 1,
            permissions: Permissions(Permissions::MANAGE_METADATA),
            scope: RoleScope::Server,
            color: 0,
            extra: Extra::default(),
        };

        let (role_wrap, _) = writer
            .publish(
                &owner,
                Edition {
                    subkind: vsk::ROLE,
                    entity: *role_id.as_bytes(),
                    content: &role.to_content().expect("serializes"),
                    head: None,
                    citation: None,
                },
                AT + 1,
            )
            .expect("publishes");
        let (grant_wrap, _) = writer
            .publish(
                &owner,
                Edition {
                    subkind: vsk::GRANT,
                    entity: grant_locator(&community_id, &member.public_key().to_bytes()),
                    content: &Grant {
                        member: member.public_key(),
                        role_ids: vec![role_id],
                        control_wrap: None,
                        extra: Extra::default(),
                    }
                    .to_content()
                    .expect("serializes"),
                    head: None,
                    citation: None,
                },
                AT + 2,
            )
            .expect("publishes");

        let mut base = open_all(&minted.wraps, &read, &signer.pk());
        base.extend(open_all(&[role_wrap, grant_wrap], &read, &signer.pk()));

        let roster = fold_control(
            &owner_pk,
            &community_id,
            &base,
            &Floors::new(),
            &BTreeSet::new(),
        );
        assert!(roster.roles.is_staff(&member.public_key(), &owner_pk));

        let grant = roster
            .floors
            .get(&grant_locator(
                &community_id,
                &member.public_key().to_bytes(),
            ))
            .expect("the member's grant folded");
        let head = roster.floors.get(community_id.as_bytes()).expect("head");

        // The member seals with their own keys and wraps with the staff write key.
        let member_writer = ControlWriter {
            author: member.public_key(),
            read,
            signer: signer.clone(),
        };
        let content = serde_json::to_string(&metadata("coop by mod")).expect("serializes");

        let (uncited, _) = member_writer
            .publish(
                &member,
                Edition {
                    subkind: vsk::COMMUNITY_METADATA,
                    entity: *community_id.as_bytes(),
                    content: &content,
                    head: Some(head),
                    citation: None,
                },
                AT + 3,
            )
            .expect("publishes");
        let (cited, _) = member_writer
            .publish(
                &member,
                Edition {
                    subkind: vsk::COMMUNITY_METADATA,
                    entity: *community_id.as_bytes(),
                    content: &content,
                    head: Some(head),
                    citation: Some(AuthorityCitation {
                        entity: grant.entity,
                        version: grant.version,
                        hash: grant.self_hash,
                    }),
                },
                AT + 4,
            )
            .expect("publishes");

        // Uncited, the edit claims an authority the member never showed.
        let mut forged = base.clone();
        forged.extend(open_all(&[uncited], &member_writer.read, &signer.pk()));
        let folded = fold_control(
            &owner_pk,
            &community_id,
            &forged,
            &Floors::new(),
            &BTreeSet::new(),
        );
        assert_eq!(
            folded.community.as_ref().map(|meta| meta.name.as_str()),
            Some("coop")
        );

        let mut edited_editions = base;
        edited_editions.extend(open_all(&[cited], &member_writer.read, &signer.pk()));
        let folded = fold_control(
            &owner_pk,
            &community_id,
            &edited_editions,
            &Floors::new(),
            &BTreeSet::new(),
        );
        assert_eq!(
            folded.community.as_ref().map(|meta| meta.name.as_str()),
            Some("coop by mod")
        );
    }

    #[test]
    fn a_pin_list_folds_under_its_coordinate_for_a_second_client() {
        let owner = Keys::generate();
        let minted = genesis(&owner, &metadata("coop"), AT).expect("mints");
        let community_id = minted.identity.community_id;
        let owner_pk = owner.public_key();
        let (read, signer) = holder(&minted);

        let channel = minted.channel_id;
        let group =
            channel_group_key(&minted.community_root, &channel, ROOT_EPOCH).expect("derives");
        let author = Keys::generate();

        let rumor = build_message(
            author.public_key(),
            &channel,
            ROOT_EPOCH,
            "pin me",
            None,
            AT * 1_000,
            None,
        );
        let (wrap, _) = seal_rumor(&rumor, &group, &author, false).expect("seals");
        let opened = cord03::open(&wrap, &group, &channel, ROOT_EPOCH)
            .expect("opens")
            .0;
        let entry = pins::build_entry(&opened, &group, &channel).expect("pins");

        let content = pins::publishable(
            &pins::ReadPinList {
                entries: vec![entry],
                sealed: false,
            },
            false,
            &group,
            ROOT_EPOCH,
        )
        .expect("publishes");

        let writer = ControlWriter {
            author: owner_pk,
            read: read.clone(),
            signer: signer.clone(),
        };
        let (pin_wrap, _) = writer
            .set_pin_list(
                &owner,
                &community_id,
                &channel,
                &content,
                None,
                None,
                AT + 1,
            )
            .expect("publishes");

        let mut editions = open_all(&minted.wraps, &read, &signer.pk());
        editions.extend(open_all(&[pin_wrap], &read, &signer.pk()));

        let folded = fold_control(
            &owner_pk,
            &community_id,
            &editions,
            &Floors::new(),
            &BTreeSet::new(),
        );

        // The coordinate derives one-way, so the list is found by naming the Channel.
        let found = pins::read_list(
            folded
                .pin_content(&community_id, &channel)
                .expect("the list folds"),
            |_| None,
        );
        assert_eq!(found.entries.len(), 1);
        assert_eq!(
            pins::verify_entry(&found.entries[0], &channel)
                .expect("verifies")
                .content,
            "pin me"
        );

        let other = ChannelId::from_bytes([0x77; 32]);
        assert!(folded.pin_content(&community_id, &other).is_none());
    }

    #[test]
    fn the_timer_is_never_guessed_and_the_write_caps_hold() {
        let owner = Keys::generate();
        let minted = genesis(&owner, &metadata("coop"), AT).expect("mints");
        let community_id = minted.identity.community_id;
        let owner_pk = owner.public_key();
        let (read, signer) = holder(&minted);

        let writer = ControlWriter {
            author: owner_pk,
            read,
            signer,
        };
        let fold = |metadata: &CommunityMetadata| {
            fold_control(
                &owner_pk,
                &community_id,
                &open_all(
                    &[writer
                        .set_community_metadata(&owner, &community_id, metadata, None, None, AT + 1)
                        .expect("publishes")
                        .0],
                    &writer.read,
                    &writer.signer.pk(),
                ),
                &Floors::new(),
                &BTreeSet::new(),
            )
            .community
            .expect("folds")
        };

        let mut timed = metadata("coop");
        timed.message_expiration = Some(2_592_000);
        assert_eq!(fold(&timed).message_expiration, Some(2_592_000));

        // Absent, zero and garbage all mean off, and garbage never poisons the rest.
        assert_eq!(fold(&metadata("coop")).message_expiration, None);

        let mut off = metadata("coop");
        off.message_expiration = Some(0);
        assert_eq!(fold(&off).message_expiration, None);

        let garbage = serde_json::json!({
            "name": "coop",
            "message_expiration": "later",
        })
        .to_string();
        let folded: CommunityMetadata = serde_json::from_str(&garbage).expect("parses");
        assert_eq!(folded.name, "coop");
        assert_eq!(folded.message_expiration, None);

        // The caps the folds apply also hold on the way out.
        let banned: BTreeSet<PublicKey> = (0..=MAX_BANLIST)
            .map(|_| Keys::generate().public_key())
            .collect();
        assert!(
            writer
                .set_banlist(&owner, &community_id, &banned, None, None, AT + 2)
                .is_err()
        );

        let grant = Grant {
            member: owner_pk,
            role_ids: (0..=MAX_ROLES_PER_MEMBER)
                .map(|index| RoleId::from_bytes([index as u8; 32]))
                .collect(),
            control_wrap: None,
            extra: Extra::default(),
        };
        assert!(grant.to_content().is_err());

        assert!(
            writer
                .set_channel_metadata(
                    &owner,
                    &minted.channel_id,
                    &ChannelMetadata {
                        name: "x".repeat(MAX_NAME_BYTES + 1),
                        private: false,
                        ..ChannelMetadata::default()
                    },
                    None,
                    None,
                    AT + 3,
                )
                .is_err()
        );
    }
}
