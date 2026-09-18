use std::collections::{BTreeMap, BTreeSet};

use anyhow::Result;
use concord::cord01::KIND_WRAP;
use concord::cord02::list::{CommunityList, KIND_COMMUNITY_LIST};
use concord::cord02::{self, ControlFold};
use concord::cord04::AuthorityCitation;
use concord::cord04::roles::{Permissions, citation_ok};
use concord::derive::{channel_group_key, control_group_key, guestbook_group_key};
use concord::store::{self, CommunityState};
use concord::{ChannelId, CommunityId, Epoch, GroupKey};
use nostr_sdk::prelude::*;
use state::UniversalSigner;

const SUBSCRIPTION_PREFIX: &str = "concord/";
const STATE_PREFIX: &str = "concord/";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PlaneKind {
    Control(Epoch),
    Guestbook,
    Channel(ChannelId, Epoch),
}

#[derive(Debug, Clone)]
pub struct Plane {
    pub kind: PlaneKind,
    /// The wrap's author: the control signer for Control, the group's own key otherwise.
    pub address: PublicKey,
    pub group: GroupKey,
}

pub fn planes(state: &CommunityState) -> Result<Vec<Plane>> {
    let mut planes = Vec::new();

    for (epoch, address) in &state.control_pks {
        let epoch = Epoch(*epoch);
        let group = control_group_key(&state.community_root, &state.id, epoch)?;
        planes.push(Plane {
            kind: PlaneKind::Control(epoch),
            address: *address,
            group,
        });
    }

    let group = guestbook_group_key(&state.community_root, &state.id, state.root_epoch)?;
    planes.push(Plane {
        kind: PlaneKind::Guestbook,
        address: group.pk(),
        group,
    });

    for channel in &state.channels {
        if channel.private {
            continue;
        }

        let group = channel_group_key(&state.community_root, &channel.id, channel.epoch)?;
        planes.push(Plane {
            kind: PlaneKind::Channel(channel.id, channel.epoch),
            address: group.pk(),
            group,
        });
    }

    Ok(planes)
}

/// One `Filter` covering every held plane. The address is the event author,
/// not a `p` tag: a Concord wrap's `p` tag carries a random ephemeral key.
pub fn subscription_filter(planes: &[Plane]) -> Filter {
    Filter::new()
        .kinds([Kind::from(KIND_WRAP)])
        .authors(planes.iter().map(|plane| plane.address))
}

pub fn subscription_id(id: &CommunityId) -> SubscriptionId {
    SubscriptionId::new(format!("{SUBSCRIPTION_PREFIX}{}", id.to_hex()))
}

pub fn community_of(subscription_id: &SubscriptionId) -> Option<CommunityId> {
    subscription_id
        .as_str()
        .strip_prefix(SUBSCRIPTION_PREFIX)?
        .parse()
        .ok()
}

#[derive(Debug, Clone)]
pub struct Snapshot {
    pub state: CommunityState,
    pub control: ControlFold,
    pub members: BTreeSet<PublicKey>,
}

/// Discovers the current account's communities from the local database.
pub async fn load(
    database: &dyn NostrDatabase,
    signer: &UniversalSigner,
    self_pk: PublicKey,
) -> Result<Vec<CommunityState>> {
    let filter = Filter::new().kind(Kind::ApplicationSpecificData);
    let mut newest: BTreeMap<CommunityId, Event> = BTreeMap::new();

    for event in database.query(filter).await? {
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

    if let Some(list) = load_list(database, signer, self_pk).await? {
        states.retain(|state| list.is_live(&state.id));
    }

    Ok(states)
}

fn state_document_of(event: &Event) -> Option<CommunityId> {
    let identifier = event.tags.identifier()?;
    let hex = identifier.strip_prefix(STATE_PREFIX)?;
    hex.parse().ok()
}

async fn load_list(
    database: &dyn NostrDatabase,
    signer: &UniversalSigner,
    self_pk: PublicKey,
) -> Result<Option<CommunityList>> {
    let filter = Filter::new()
        .kind(Kind::Custom(KIND_COMMUNITY_LIST))
        .author(self_pk)
        .limit(1);

    let Some(event) = database.query(filter).await?.into_iter().next() else {
        return Ok(None);
    };

    let json = signer.nip44_decrypt_async(&self_pk, &event.content).await?;

    Ok(Some(serde_json::from_str(&json)?))
}

/// Rebuilds a community from the wraps already in the local database.
pub async fn fold(
    database: &dyn NostrDatabase,
    state: &CommunityState,
) -> Result<Option<Snapshot>> {
    let planes = planes(state)?;

    if planes.is_empty() {
        return Ok(None);
    }

    let wraps = database.query(subscription_filter(&planes)).await?;
    let mut editions = Vec::new();
    let mut observed: BTreeMap<PublicKey, u64> = BTreeMap::new();
    let mut guestbook_rumors = Vec::new();

    for wrap in &wraps {
        let Some(plane) = planes.iter().find(|plane| plane.address == wrap.pubkey) else {
            continue;
        };

        match plane.kind {
            PlaneKind::Control(_) => {
                if let Ok(edition) = cord02::open_edition(wrap, &plane.group, &plane.address, true)
                {
                    editions.push(edition);
                }
            }
            PlaneKind::Guestbook => {
                if let Ok((_, rumor)) = cord02::guestbook::open(wrap, &plane.group) {
                    observe(&mut observed, rumor.author, rumor.at_ms);
                    guestbook_rumors.push(rumor);
                }
            }
            PlaneKind::Channel(channel, epoch) => {
                if let Ok((opened, rumor)) =
                    concord::cord03::open(wrap, &plane.group, &channel, epoch)
                {
                    store::cache_rumor(database, &channel, &opened).await?;
                    observe(&mut observed, rumor.author, rumor.at_ms);
                }
            }
        }
    }

    if editions.is_empty() {
        return Ok(None);
    }

    let control = cord02::fold_control(
        &state.owner,
        &state.id,
        &editions,
        &state.floors(),
        &state.banned,
    );

    let granted: BTreeSet<PublicKey> = control
        .roles
        .grants()
        .filter(|grant| !grant.role_ids.is_empty())
        .map(|grant| grant.member)
        .collect();

    let floors = state.floors();
    let can_kick = |actor: &PublicKey, target: &PublicKey, citation: Option<&AuthorityCitation>| {
        citation_ok(&state.owner, &state.id, actor, citation, &floors)
            && control
                .roles
                .can_act_on_member(actor, &state.owner, target, Permissions::KICK)
    };

    let now_ms = Timestamp::now().as_secs().saturating_mul(1000);
    let coalesced = cord02::guestbook::coalesce(&guestbook_rumors, now_ms, None, can_kick);
    let mut members = cord02::guestbook::complete_memberlist(
        &coalesced,
        &observed,
        &granted,
        &control.banned,
        &BTreeMap::new(),
    );

    // The roster has no grant for the owner, so membership is stated here.
    members.insert(state.owner);

    let mut state = state.clone();
    state.apply_fold(&control);
    store::save_state(database, &state).await?;

    Ok(Some(Snapshot {
        state,
        control,
        members,
    }))
}

fn observe(observed: &mut BTreeMap<PublicKey, u64>, author: PublicKey, at_ms: u64) {
    observed
        .entry(author)
        .and_modify(|seen| *seen = (*seen).max(at_ms))
        .or_insert(at_ms);
}

#[cfg(test)]
mod tests {
    use concord::cord02::list::{CommunityListEntry, JoinMaterial, Tombstone, build_list_event};
    use concord::cord02::{CommunityMetadata, ROOT_EPOCH, genesis, open_edition};
    use concord::cord04::ParsedEdition;
    use concord::derive::control_signer_group_key;
    use concord::store::save_state;
    use nostr_memory::MemoryDatabase;

    use super::*;

    const AT_MS: u64 = 1_719_800_000_000;

    fn community(owner: &Keys) -> CommunityState {
        let metadata = CommunityMetadata {
            name: "Room".to_owned(),
            ..Default::default()
        };
        let genesis = genesis(owner, &metadata, AT_MS / 1000).expect("genesis");
        let read = control_group_key(
            &genesis.community_root,
            &genesis.identity.community_id,
            ROOT_EPOCH,
        )
        .expect("read key");
        let address = control_signer_group_key(
            &genesis.control_root,
            &genesis.identity.community_id,
            ROOT_EPOCH,
        )
        .expect("signer key")
        .pk();

        let editions: Vec<ParsedEdition> = genesis
            .wraps
            .iter()
            .map(|wrap| open_edition(wrap, &read, &address, true).expect("opens"))
            .collect();

        CommunityState::from_genesis(&genesis, &editions, AT_MS).expect("state")
    }

    fn material(state: &CommunityState) -> JoinMaterial {
        JoinMaterial {
            community_id: state.id,
            owner: state.owner,
            owner_salt: "00".repeat(32),
            community_root: "11".repeat(32),
            root_epoch: ROOT_EPOCH,
            control_pk: None,
            control_root: None,
            channels: Vec::new(),
            relays: Vec::new(),
            name: "Room".to_owned(),
            extra: Default::default(),
        }
    }

    fn entry(state: &CommunityState, added_at: u64) -> CommunityListEntry {
        let material = material(state);

        CommunityListEntry {
            community_id: state.id,
            seed: material.clone(),
            current: material,
            added_at,
            extra: Default::default(),
        }
    }

    #[test]
    fn every_held_plane_routes_by_its_wrap_author() {
        let owner = Keys::generate();
        let state = community(&owner);
        let planes = planes(&state).expect("planes");

        assert_eq!(
            planes.len(),
            3,
            "the control epoch, the guestbook and #general"
        );

        let filter = subscription_filter(&planes);
        let expected: BTreeSet<PublicKey> = planes.iter().map(|plane| plane.address).collect();
        assert_eq!(filter.authors, Some(expected));
        assert_eq!(filter.kinds, Some(BTreeSet::from([Kind::from(KIND_WRAP)])));

        assert_eq!(community_of(&subscription_id(&state.id)), Some(state.id));
        assert_eq!(community_of(&SubscriptionId::new("device-giftwrap")), None);
    }

    #[test]
    fn loading_scans_state_documents_and_honours_the_list() {
        smol::block_on(async {
            let keys = Keys::generate();
            let signer = UniversalSigner::new(keys.clone());
            let owner = Keys::generate();
            let state = community(&owner);

            // With no list event, every state document is a community.
            let no_list = MemoryDatabase::unbounded();
            save_state(&no_list, &state).await.expect("saves");
            let loaded = load(&no_list, &signer, keys.public_key())
                .await
                .expect("loads");
            assert_eq!(loaded.len(), 1);
            assert_eq!(loaded[0].id, state.id);

            // A live entry keeps it.
            let event = build_list_event(
                &keys,
                &CommunityList {
                    entries: vec![entry(&state, AT_MS)],
                    ..Default::default()
                },
            )
            .expect("builds");
            let live = MemoryDatabase::unbounded();
            save_state(&live, &state).await.expect("saves");
            live.save_event(&event).await.expect("saves list");
            let loaded = load(&live, &signer, keys.public_key())
                .await
                .expect("loads");
            assert_eq!(loaded.len(), 1);

            // A newer tombstone than the entry retires it.
            let event = build_list_event(
                &keys,
                &CommunityList {
                    entries: vec![entry(&state, AT_MS)],
                    tombstones: vec![Tombstone {
                        community_id: state.id,
                        removed_at: AT_MS + 1,
                        extra: Default::default(),
                    }],
                    ..Default::default()
                },
            )
            .expect("builds");
            let retired = MemoryDatabase::unbounded();
            save_state(&retired, &state).await.expect("saves");
            retired.save_event(&event).await.expect("saves list");
            let loaded = load(&retired, &signer, keys.public_key())
                .await
                .expect("loads");
            assert!(loaded.is_empty());
        });
    }
}
