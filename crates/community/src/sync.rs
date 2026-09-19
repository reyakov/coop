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

pub async fn create<S>(
    client: &Client,
    signer: &S,
    metadata: &cord02::CommunityMetadata,
) -> Result<CommunityState>
where
    S: AsyncGetPublicKey + AsyncSignEvent + AsyncNip44 + ?Sized,
{
    let at_secs = Timestamp::now().as_secs();
    let genesis = cord02::genesis(signer, metadata, at_secs).await?;
    let id = genesis.identity.community_id;

    let read = control_group_key(&genesis.community_root, &id, cord02::ROOT_EPOCH)?;
    let address = control_signer_group_key(&genesis.control_root, &id, cord02::ROOT_EPOCH)?.pk();

    let mut editions = Vec::with_capacity(genesis.wraps.len());

    for wrap in &genesis.wraps {
        editions.push(cord02::open_edition(wrap, &read, &address, true)?);
    }

    let state = CommunityState::from_genesis(&genesis, &editions, at_secs.saturating_mul(1000))?;
    store::save_state(client, &state).await?;

    publish_wraps(client, &genesis.wraps, &state.relays).await;

    if let Err(error) = record_membership(client, signer, &state, &metadata.name).await {
        log::warn!(
            "community {}: recording the membership failed: {error}",
            state.id.to_hex()
        );
    }

    Ok(state)
}

/// Best-effort publication of the genesis wraps to the community's relays.
async fn publish_wraps(client: &Client, wraps: &[Event], relays: &[RelayUrl]) {
    for url in relays {
        if let Err(error) = client.add_relay(url).and_connect().await {
            log::warn!("community genesis: failed to add relay {url}: {error}");
        }
    }

    for wrap in wraps {
        let sent = if relays.is_empty() {
            client.send_event(wrap).broadcast().await
        } else {
            client.send_event(wrap).to(relays.iter().cloned()).await
        };

        match sent {
            Ok(output) if output.failed.is_empty() => {}
            Ok(output) => log::warn!(
                "community genesis: {} relay(s) rejected {}",
                output.failed.len(),
                wrap.id
            ),
            Err(error) => log::warn!("community genesis: publishing {} failed: {error}", wrap.id),
        }
    }
}

async fn record_membership<S>(
    client: &Client,
    signer: &S,
    state: &CommunityState,
    name: &str,
) -> Result<()>
where
    S: AsyncGetPublicKey + AsyncSignEvent + AsyncNip44 + ?Sized,
{
    let self_pk = signer.get_public_key_async().await?;
    let held = load_list(client, signer, self_pk).await?;

    let frags = held.as_ref().map_or(1, |list| list.frags);

    if frags > 1 {
        log::warn!(
            "community {}: the list spans {frags} fragments; deferring the membership write",
            state.id.to_hex()
        );
        return Ok(());
    }

    let entry = store::list_entry(state, name);
    let list = match held {
        Some(held) => held.joined(entry),
        None => CommunityList::default().joined(entry),
    };

    let previous = newest_fragment_at(client, self_pk).await?;
    let now = Timestamp::now().as_secs();
    let at_secs = previous.map_or(now, |previous| now.max(previous.as_secs() + 1));

    let event = cord02::list::build_list_event(signer, &list, 0, at_secs).await?;
    publish_list(client, &event).await;

    Ok(())
}

async fn publish_list(client: &Client, event: &Event) {
    match client.send_event(event).to_nip65().await {
        Ok(output) if output.failed.is_empty() => {}
        Ok(output) => log::warn!(
            "community list: {} relay(s) rejected the publish",
            output.failed.len()
        ),
        Err(error) => log::warn!("community list: publish failed: {error}"),
    }
}

/// The newest `created_at` the account holds across its list fragments.
async fn newest_fragment_at(client: &Client, self_pk: PublicKey) -> Result<Option<Timestamp>> {
    Ok(newest_fragments(client, self_pk)
        .await?
        .into_values()
        .map(|event| event.created_at)
        .max())
}

/// The subscription id carrying the account's own Community List.
pub const LIST_SUBSCRIPTION: &str = "concord/list";

pub fn list_subscription_id() -> SubscriptionId {
    SubscriptionId::new(LIST_SUBSCRIPTION)
}

pub fn is_list_subscription(id: &SubscriptionId) -> bool {
    id.as_str() == LIST_SUBSCRIPTION
}

/// Subscribes to the account's community list.
pub async fn subscribe_list(client: &Client, self_pk: PublicKey) -> Result<()> {
    let id = list_subscription_id();
    client.unsubscribe(&id).await?;

    let filter = Filter::new()
        .kind(Kind::Custom(KIND_COMMUNITY_LIST))
        .author(self_pk);

    let output = client
        .subscribe(ReqTarget::auto(vec![filter]))
        .with_id(id)
        .await?;

    if !output.failed.is_empty() {
        log::warn!(
            "community list: {} relay(s) rejected the subscription",
            output.failed.len()
        );
    }

    Ok(())
}

/// Discovers the current account's communities: every live membership the List
/// carries, plus any locally-held membership the List does not mention.
///
/// A held membership is dropped only when the List carries a tombstone at least
/// as new as it, because absence from the List is never a fact (§8).
pub async fn load(
    client: &Client,
    signer: &UniversalSigner,
    self_pk: PublicKey,
) -> Result<Vec<CommunityState>> {
    let list = match load_list(client, signer, self_pk).await? {
        Some(list) => list,
        None => return store::load_states(client).await,
    };

    let mut held: BTreeMap<CommunityId, CommunityState> = store::load_states(client)
        .await?
        .into_iter()
        .map(|state| (state.id, state))
        .collect();

    held.retain(|id, state| !retired(&list, id, state.added_at_ms));

    for entry in &list.entries {
        if !list.is_live(&entry.community_id) {
            continue;
        }

        let fresh = match CommunityState::from_join_material(&entry.current, entry.added_at) {
            Ok(fresh) => fresh,
            Err(error) => {
                log::warn!(
                    "ignoring unreadable community {} from the list: {error}",
                    entry.community_id.to_hex()
                );
                continue;
            }
        };

        let state = match held.remove(&entry.community_id) {
            Some(materialized) => refresh(materialized, fresh),
            None => fresh,
        };

        store::save_state(client, &state).await?;
        held.insert(entry.community_id, state);
    }

    Ok(held.into_values().collect())
}

fn retired(list: &CommunityList, id: &CommunityId, added_at_ms: u64) -> bool {
    list.tombstones
        .iter()
        .find(|tombstone| tombstone.community_id == *id)
        .is_some_and(|tombstone| tombstone.removed_at >= added_at_ms)
}

fn refresh(mut held: CommunityState, fresh: CommunityState) -> CommunityState {
    held.owner = fresh.owner;
    held.owner_salt = fresh.owner_salt;
    held.community_root = fresh.community_root;
    held.root_epoch = fresh.root_epoch;
    held.added_at_ms = fresh.added_at_ms;

    if fresh.control_root.is_some() {
        held.control_root = fresh.control_root;
    }

    for (epoch, address) in fresh.control_pks {
        held.control_pks.insert(epoch, address);
    }

    held.relays = fresh.relays;

    for channel in fresh.channels {
        match held.channels.iter_mut().find(|held| held.id == channel.id) {
            Some(held) => {
                held.name = channel.name;
                held.epoch = channel.epoch;

                if channel.private {
                    held.private = true;
                    held.key = channel.key;
                }
            }
            None => held.channels.push(channel),
        }
    }

    held
}

/// The newest held copy of each fragment, keyed by its `d` index.
async fn newest_fragments(client: &Client, self_pk: PublicKey) -> Result<BTreeMap<u64, Event>> {
    let filter = Filter::new()
        .kind(Kind::Custom(KIND_COMMUNITY_LIST))
        .author(self_pk);

    let mut newest: BTreeMap<u64, Event> = BTreeMap::new();

    for event in client.database().query(filter).await? {
        let Ok(index) = cord02::list::fragment_index(&event) else {
            continue;
        };

        match newest.get(&index) {
            Some(existing) if existing.created_at >= event.created_at => {}
            _ => {
                newest.insert(index, event);
            }
        }
    }

    Ok(newest)
}

/// Every fragment of the account's list in the local database, merged.
async fn load_list<S>(
    client: &Client,
    signer: &S,
    self_pk: PublicKey,
) -> Result<Option<CommunityList>>
where
    S: AsyncGetPublicKey + AsyncNip44 + ?Sized,
{
    let mut merged: Option<CommunityList> = None;

    for event in newest_fragments(client, self_pk).await?.into_values() {
        match cord02::list::parse_list_event(signer, &event).await {
            Ok(list) => {
                merged = Some(match merged {
                    Some(held) => cord02::list::merge(held, list),
                    None => list,
                });
            }
            Err(error) => {
                log::warn!("ignoring unreadable community list {}: {error}", event.id);
            }
        }
    }

    Ok(merged)
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

        let filter = subscription_filter(&planes);
        let addresses: BTreeSet<PublicKey> = planes.iter().map(|plane| plane.address).collect();
        assert_eq!(filter.authors, Some(addresses));
        assert_eq!(filter.kinds, Some(BTreeSet::from([Kind::from(KIND_WRAP)])));
    }

    fn metadata(name: &str) -> cord02::CommunityMetadata {
        cord02::CommunityMetadata {
            name: name.to_owned(),
            ..cord02::CommunityMetadata::default()
        }
    }

    fn held(id: CommunityId, control_pk: PublicKey) -> CommunityState {
        CommunityState {
            id,
            owner: Keys::generate().public_key(),
            owner_salt: [0x01; 32],
            community_root: [0x02; 32],
            root_epoch: Epoch(0),
            control_root: Some([0x03; 32]),
            control_pks: BTreeMap::from([(0, control_pk)]),
            channels: vec![concord::store::ChannelKeyRef {
                id: ChannelId::from_bytes([0x9c; 32]),
                name: "general".to_owned(),
                private: false,
                epoch: Epoch(0),
                key: None,
            }],
            relays: Vec::new(),
            heads: Vec::new(),
            banned: BTreeSet::new(),
            dissolved: false,
            added_at_ms: 1_700_000_000_000,
        }
    }

    /// Puts a List in the database as the account's own fragment, exactly as a
    /// relay would have delivered it.
    async fn store_fragment<S>(client: &Client, signer: &S, list: &CommunityList)
    where
        S: AsyncGetPublicKey + AsyncSignEvent + AsyncNip44 + ?Sized,
    {
        let event = cord02::list::build_list_event(signer, list, 0, 1_700_000_000)
            .await
            .expect("builds");
        client.database().save_event(&event).await.expect("saves");
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

            let created = create(&client, &signer, &metadata("coop"))
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
                    &metadata("coop two"),
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

    /// The other half of `create`: the membership must reach the account's
    /// Community List, and a later create must union into it rather than replace
    /// it (CORD-02 §8 read-modify-write).
    #[test]
    fn creating_a_community_records_the_membership_in_the_list() {
        smol::block_on(async {
            let client = client();
            let keys = Keys::generate();
            let signer = UniversalSigner::new(keys.clone());
            let self_pk = keys.public_key();

            let created = create(&client, &signer, &metadata("coop"))
                .await
                .expect("creates");

            let list = load_list(&client, &signer, self_pk)
                .await
                .expect("reads")
                .expect("a list");

            assert_eq!(list.frags, 1);
            assert!(list.is_complete([0]), "the whole list is one fragment");
            assert!(list.is_live(&created.id));

            let entry = list
                .entries
                .iter()
                .find(|entry| entry.community_id == created.id)
                .expect("the membership");
            assert_eq!(
                entry.seed, entry.current,
                "a fresh membership has one anchor"
            );
            assert_eq!(entry.current.name, "coop");
            assert_eq!(entry.current.owner, created.owner);
            assert_eq!(entry.current.root_epoch, created.root_epoch);
            assert_eq!(
                entry.current.control_pk,
                created.control_pks.get(&0).copied()
            );
            assert!(
                entry.current.control_root.is_some(),
                "the owner holds the control root"
            );
            assert_eq!(entry.current.channels.len(), created.channels.len());
            assert_eq!(entry.added_at, created.added_at_ms);

            // A second create unions into the same document: the first
            // membership survives and both are live.
            let second = create(&client, &signer, &metadata("second"))
                .await
                .expect("creates");

            let grown = load_list(&client, &signer, self_pk)
                .await
                .expect("reads")
                .expect("a list");

            assert!(grown.is_live(&created.id));
            assert!(grown.is_live(&second.id));
            assert_eq!(grown.entries.len(), 2);
        });
    }

    /// The discovery fix: a membership the List carries is materialized even when
    /// no state document has ever been written for it.
    #[test]
    fn load_materializes_a_membership_the_list_carries_with_no_state_document() {
        smol::block_on(async {
            let client = client();
            let keys = Keys::generate();
            let signer = UniversalSigner::new(keys.clone());

            let listed = held(
                CommunityId::from_bytes([0x42; 32]),
                Keys::generate().public_key(),
            );
            let list = CommunityList::default().joined(store::list_entry(&listed, "coop"));
            store_fragment(&client, &signer, &list).await;

            assert!(
                store::load_state(&client, &listed.id)
                    .await
                    .expect("reads")
                    .is_none()
            );

            let loaded = load(&client, &signer, keys.public_key())
                .await
                .expect("loads");
            let materialized = loaded
                .iter()
                .find(|state| state.id == listed.id)
                .expect("the list materializes the community");

            assert_eq!(materialized.owner, listed.owner);
            assert_eq!(materialized.community_root, listed.community_root);
            assert_eq!(materialized.control_root, listed.control_root);
            assert_eq!(materialized.control_pks, listed.control_pks);
            assert_eq!(materialized.channels, listed.channels);
            assert_eq!(materialized.added_at_ms, listed.added_at_ms);

            // Discovery writes the document, so the next load is warm.
            assert_eq!(
                store::load_state(&client, &listed.id)
                    .await
                    .expect("reads")
                    .map(|state| state.id),
                Some(listed.id)
            );
        });
    }

    /// Absence from the List is never a fact (§8): a held membership the List
    /// does not mention survives alongside the one it does.
    #[test]
    fn load_keeps_a_local_membership_the_list_does_not_mention() {
        smol::block_on(async {
            let client = client();
            let keys = Keys::generate();
            let signer = UniversalSigner::new(keys.clone());

            let listed = held(
                CommunityId::from_bytes([0x42; 32]),
                Keys::generate().public_key(),
            );
            let local = held(
                CommunityId::from_bytes([0x43; 32]),
                Keys::generate().public_key(),
            );

            let list = CommunityList::default().joined(store::list_entry(&listed, "listed"));
            store_fragment(&client, &signer, &list).await;
            store::save_state(&client, &local).await.expect("saves");

            let loaded = load(&client, &signer, keys.public_key())
                .await
                .expect("loads");
            let ids: BTreeSet<CommunityId> = loaded.iter().map(|state| state.id).collect();

            assert!(ids.contains(&listed.id));
            assert!(ids.contains(&local.id));
        });
    }

    /// Only a tombstone subtracts a membership (§8).
    #[test]
    fn a_tombstone_drops_a_held_membership() {
        smol::block_on(async {
            let client = client();
            let keys = Keys::generate();
            let signer = UniversalSigner::new(keys.clone());

            let local = held(
                CommunityId::from_bytes([0x42; 32]),
                Keys::generate().public_key(),
            );
            store::save_state(&client, &local).await.expect("saves");

            let list = CommunityList::default().tombstoned(local.id, u64::MAX);
            store_fragment(&client, &signer, &list).await;

            let loaded = load(&client, &signer, keys.public_key())
                .await
                .expect("loads");

            assert!(loaded.iter().all(|state| state.id != local.id));
        });
    }

    /// The `concord/list` id must not be read as a community's subscription, or
    /// every list event would refresh a community instead of triggering `load`.
    #[test]
    fn the_list_subscription_is_not_read_as_a_community_subscription() {
        let id = CommunityId::from_bytes([0x42; 32]);

        assert!(is_list_subscription(&list_subscription_id()));
        assert!(community_of(&list_subscription_id()).is_none());
        assert_eq!(community_of(&subscription_id(&id)), Some(id));
    }
}
