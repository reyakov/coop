use std::collections::{BTreeMap, BTreeSet};

use anyhow::Result;
use concord::cord01::KIND_WRAP;
use concord::cord02::list::{CommunityList, KIND_COMMUNITY_LIST};
use concord::cord02::{self, ControlFold};
use concord::cord04::AuthorityCitation;
use concord::cord04::roles::{Permissions, citation_ok};
use concord::derive::{
    channel_group_key, control_group_key, control_signer_group_key, guestbook_group_key,
};
use concord::store::{self, CommunityState};
use concord::{ChannelId, CommunityId, Epoch, GroupKey};
use nostr_sdk::prelude::*;
use state::UniversalSigner;

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
    SubscriptionId::new(format!("{}{}", store::STATE_PREFIX, id.to_hex()))
}

pub fn community_of(subscription_id: &SubscriptionId) -> Option<CommunityId> {
    subscription_id
        .as_str()
        .strip_prefix(store::STATE_PREFIX)?
        .parse()
        .ok()
}

#[derive(Debug, Clone)]
pub struct Snapshot {
    pub state: CommunityState,
    pub control: ControlFold,
    pub members: BTreeSet<PublicKey>,
}

/// Mints a community owned by `signer` and persists it locally.
pub async fn create<S>(
    client: &Client,
    signer: &S,
    metadata: &cord02::CommunityMetadata,
) -> Result<CommunityState>
where
    S: AsyncGetPublicKey + AsyncSignEvent + ?Sized,
{
    let at_secs = Timestamp::now().as_secs();
    let genesis = cord02::genesis(signer, metadata, at_secs).await?;
    let id = genesis.identity.community_id;

    let read = control_group_key(&genesis.community_root, &id, cord02::ROOT_EPOCH)?;
    let address = control_signer_group_key(&genesis.control_root, &id, cord02::ROOT_EPOCH)?.pk();

    let mut editions = Vec::with_capacity(genesis.wraps.len());

    for wrap in &genesis.wraps {
        editions.push(cord02::open_edition(wrap, &read, &address, true)?);
        client.database().save_event(wrap).await?;
    }

    let state = CommunityState::from_genesis(&genesis, &editions, at_secs.saturating_mul(1000))?;
    store::save_state(client, &state).await?;

    Ok(state)
}

/// Discovers the current account's communities from the local database.
pub async fn load(
    client: &Client,
    signer: &UniversalSigner,
    self_pk: PublicKey,
) -> Result<Vec<CommunityState>> {
    let mut states = store::load_states(client).await?;

    if let Some(list) = load_list(client, signer, self_pk).await? {
        states.retain(|state| list.is_live(&state.id));
    }

    Ok(states)
}

async fn load_list(
    client: &Client,
    signer: &UniversalSigner,
    self_pk: PublicKey,
) -> Result<Option<CommunityList>> {
    let filter = Filter::new()
        .kind(Kind::Custom(KIND_COMMUNITY_LIST))
        .author(self_pk)
        .limit(1);

    let Some(event) = client.database().query(filter).await?.into_iter().next() else {
        return Ok(None);
    };

    Ok(Some(cord02::list::parse_list_event(signer, &event).await?))
}

/// Rebuilds a community from the wraps already in the local database.
pub async fn fold(client: &Client, state: &CommunityState) -> Result<Option<Snapshot>> {
    let planes = planes(state)?;

    if planes.is_empty() {
        return Ok(None);
    }

    let wraps = client
        .database()
        .query(subscription_filter(&planes))
        .await?;

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
                    store::cache_rumor(client, &channel, &opened).await?;
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
    store::save_state(client, &state).await?;

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
    use nostr_memory::MemoryDatabase;

    use super::*;

    fn client() -> Client {
        ClientBuilder::default()
            .database(MemoryDatabase::unbounded())
            .build()
    }

    #[test]
    fn planes_address_the_control_guestbook_and_only_public_channels() {
        let owner = Keys::generate().public_key();
        let control_pk = Keys::generate().public_key();
        let general = ChannelId::from_bytes([0x9c; 32]);

        let state = CommunityState {
            id: CommunityId::from_bytes([0x42; 32]),
            owner,
            owner_salt: [0x01; 32],
            community_root: [0x02; 32],
            root_epoch: Epoch(0),
            control_root: None,
            control_pks: BTreeMap::from([(0, control_pk)]),
            channels: vec![
                concord::store::ChannelKeyRef {
                    id: general,
                    name: "general".to_owned(),
                    private: false,
                    epoch: Epoch(0),
                    key: None,
                },
                concord::store::ChannelKeyRef {
                    id: ChannelId::from_bytes([0x9d; 32]),
                    name: "staff".to_owned(),
                    private: true,
                    epoch: Epoch(0),
                    key: Some([0x04; 32]),
                },
            ],
            relays: vec![RelayUrl::parse("wss://relay.example").expect("a url")],
            heads: Vec::new(),
            banned: BTreeSet::new(),
            dissolved: false,
            added_at_ms: 0,
        };

        let planes = planes(&state).expect("planes");

        // Control at the root epoch, the guestbook, and the public channel. The
        // private channel is skipped: its address derives from the granted key,
        // not the community_root.
        assert_eq!(planes.len(), 3);
        assert!(planes.iter().any(|plane| plane.address == control_pk));
        assert!(
            planes
                .iter()
                .any(|plane| matches!(plane.kind, PlaneKind::Guestbook))
        );
        assert!(
            planes
                .iter()
                .any(|plane| matches!(plane.kind, PlaneKind::Channel(id, _) if id == general))
        );

        // The filter author-lists every plane, so the subscription actually
        // reaches the events the fold reads.
        let filter = subscription_filter(&planes);
        let addresses: BTreeSet<PublicKey> = planes.iter().map(|plane| plane.address).collect();
        assert_eq!(filter.authors, Some(addresses));
        assert_eq!(filter.kinds, Some(BTreeSet::from([Kind::from(KIND_WRAP)])));
    }

    fn metadata(name: &str, relay: &str) -> cord02::CommunityMetadata {
        cord02::CommunityMetadata {
            name: name.to_owned(),
            relays: vec![relay.to_owned()],
            ..cord02::CommunityMetadata::default()
        }
    }

    /// What `CommunityRegistry` needs from a created community: a state document
    /// `load` finds, a control plane the subscription filter actually addresses,
    /// and a fold that survives an inbound control edit.
    #[test]
    fn creating_a_community_persists_a_state_that_subscribes_and_folds() {
        smol::block_on(async {
            let client = client();
            let keys = Keys::generate();
            let signer = UniversalSigner::new(keys.clone());

            let created = create(&client, &signer, &metadata("coop", "wss://relay.example"))
                .await
                .expect("creates");

            let loaded = load(&client, &signer, keys.public_key())
                .await
                .expect("loads");
            assert_eq!(loaded, vec![created.clone()]);

            // The subscription filter must address the genesis wraps, or the registry
            // would listen to a plane nothing is ever published on.
            let planes = planes(&created).expect("planes");
            let wraps = client
                .database()
                .query(subscription_filter(&planes))
                .await
                .expect("queries");
            assert_eq!(wraps.len(), created.heads.len());
            assert!(wraps.iter().all(|wrap| wrap.kind == Kind::from(KIND_WRAP)));

            let snapshot = fold(&client, &created)
                .await
                .expect("folds")
                .expect("a control plane");
            assert_eq!(snapshot.state.channels.len(), 1);
            assert_eq!(snapshot.members, BTreeSet::from([keys.public_key()]));
            assert_eq!(
                snapshot
                    .control
                    .community
                    .as_ref()
                    .map(|metadata| metadata.name.as_str()),
                Some("coop")
            );

            // An inbound control edit made by the owner folds over the created state.
            let community_head = created
                .heads
                .iter()
                .find(|head| head.entity == *created.id.as_bytes())
                .expect("a community head");
            let writer = cord02::ControlWriter {
                author: created.owner,
                read: control_group_key(&created.community_root, &created.id, cord02::ROOT_EPOCH)
                    .expect("a reading key"),
                signer: control_signer_group_key(
                    &created.control_root.expect("a control root"),
                    &created.id,
                    cord02::ROOT_EPOCH,
                )
                .expect("a signing key"),
            };

            let (wrap, _) = writer
                .set_community_metadata(
                    &keys,
                    &created.id,
                    &metadata("coop two", "wss://relay.example"),
                    Some(community_head),
                    None,
                    Timestamp::now().as_secs() + 1,
                )
                .await
                .expect("publishes");
            client.database().save_event(&wrap).await.expect("saves");

            let updated = fold(&client, &created)
                .await
                .expect("folds")
                .expect("a control plane");
            assert_eq!(
                updated
                    .control
                    .community
                    .as_ref()
                    .map(|metadata| metadata.name.as_str()),
                Some("coop two")
            );
        });
    }
}
