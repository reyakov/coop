use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;

use anyhow::{Context, Result};
use concord::cord01::KIND_WRAP_EPHEMERAL;
use concord::cord02::list::{CommunityList, JoinMaterial, KIND_COMMUNITY_LIST};
use concord::cord02::{self, ControlFold, ImageRef};
use concord::cord04::AuthorityCitation;
use concord::cord04::roles::{Permissions, citation_ok};
use concord::derive::{
    channel_group_key, control_group_key, control_signer_group_key, guestbook_group_key,
};
use concord::state::{CommunityState, HeldKey, HeldRoot, list_entry};
use concord::{ChannelId, CommunityId, Epoch, GroupKey, decode_hex_32};
use gpui::AsyncApp;
use nostr_sdk::prelude::*;
use state::UniversalSigner;

use crate::cache::{self, Observed};
use crate::history::{CURSOR_OVERLAP, Window};

/// How much of what a relay stores a cold subscription replays per relay.
const LIVE_REPLAY: usize = 500;

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
    /// When the rotation that retired this plane's key published.
    pub retired_at: Option<Timestamp>,
}

impl Plane {
    /// Whether a wrap sealed under this plane is still inside its key's life.
    pub fn accepts(&self, wrap: &Event) -> bool {
        self.retired_at
            .is_none_or(|retired| wrap.created_at <= retired)
    }
}

pub fn planes(state: &CommunityState) -> Result<Vec<Plane>> {
    let mut planes = Vec::new();
    let roots = state.roots();

    for (epoch, address) in &state.control_pks {
        let epoch = Epoch(*epoch);
        // An epoch's Control Plane reads under the root that was current then,
        // so a rotation that kept a floor for the prior root republishes here.
        let root = roots
            .iter()
            .find(|root| root.epoch == epoch)
            .copied()
            .unwrap_or(HeldRoot {
                epoch,
                key: state.community_root,
                control_pk: None,
                retired_at: None,
            });

        let group = control_group_key(&root.key, &state.id, epoch)?;
        planes.push(Plane {
            kind: PlaneKind::Control(epoch),
            address: *address,
            group,
            retired_at: root.retired_at,
        });
    }

    if state.control_pks.is_empty() {
        let group = control_group_key(&state.community_root, &state.id, state.root_epoch)?;
        planes.push(Plane {
            kind: PlaneKind::Control(state.root_epoch),
            address: group.pk(),
            group,
            retired_at: None,
        });
    }

    for root in &roots {
        let group = guestbook_group_key(&root.key, &state.id, root.epoch)?;
        planes.push(Plane {
            kind: PlaneKind::Guestbook,
            address: group.pk(),
            group,
            retired_at: root.retired_at,
        });
    }

    for channel in &state.channels {
        for held in state.held_keys(&channel.id) {
            let group = channel_group_key(&held.key, &channel.id, held.epoch)?;
            planes.push(Plane {
                kind: PlaneKind::Channel(channel.id, held.epoch),
                address: group.pk(),
                group,
                retired_at: held.retired_at,
            });
        }
    }

    // A rotation can re-derive an address the current root already produced.
    //
    // A duplicate author would only repeat a filter, so keep the set unique.
    let mut seen = BTreeSet::new();
    planes.retain(|plane| seen.insert(plane.address));

    Ok(planes)
}

pub fn plane_filter(planes: &[Plane]) -> Filter {
    Filter::new()
        .kinds([Kind::GiftWrap, Kind::Custom(KIND_WRAP_EPHEMERAL)])
        .authors(planes.iter().map(|plane| plane.address))
}

pub fn live_filter(planes: &[Plane], window: Window) -> Filter {
    let mut filter = plane_filter(planes);

    if let Some(until) = window.until {
        filter = filter.until(until);
    }

    if let Some(since) = window.since {
        filter = filter.since(since);
    }

    filter.limit(LIVE_REPLAY)
}

/// The window a community's standing subscription opens with.
pub fn live_window(state: &CommunityState, now: Timestamp) -> Window {
    let floor = state
        .channels
        .iter()
        .filter_map(|channel| state.cursors.get(&channel.id)?.newest)
        .min();

    match floor {
        Some(floor) => Window {
            since: Some(floor.min(now) - CURSOR_OVERLAP),
            until: None,
        },
        None => Window::default(),
    }
}

/// The subscription id carrying a community's planes.
pub fn subscription_id(id: &CommunityId) -> SubscriptionId {
    SubscriptionId::new(id.to_hex())
}

pub fn community_of(subscription_id: &SubscriptionId) -> Option<CommunityId> {
    subscription_id.as_str().parse().ok()
}

/// Download and decrypt a community icon into a content-addressed cache file.
pub async fn resolve_image(image: &ImageRef, cx: &AsyncApp) -> Result<PathBuf> {
    let url = Url::parse(&image.url).context("community image url")?;
    state::download_and_decrypt_to_cache(&url, &image.key, &image.nonce, &image.hash, cx).await
}

#[derive(Debug, Clone)]
pub struct Snapshot {
    pub state: CommunityState,
    pub control: ControlFold,
    pub members: BTreeSet<PublicKey>,
    /// Wraps the store holds per channel that no held key can open.
    pub unreadable: BTreeMap<ChannelId, usize>,
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
    cache::save_state(client, &state).await?;

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
pub(crate) async fn publish_wraps(client: &Client, wraps: &[Event], relays: &[RelayUrl]) {
    connect_relays(client, relays).await;

    for wrap in wraps {
        publish_wrap(client, wrap, relays).await;
    }
}

/// Bring the community's relays into the pool before anything is sent through them.
pub(crate) async fn connect_relays(client: &Client, relays: &[RelayUrl]) {
    for url in relays {
        if let Err(error) = client.add_relay(url).and_connect().await {
            log::warn!("community: failed to add relay {url}: {error}");
        }
    }
}

/// Best-effort publication of a single wrap to the community's relays.
pub(crate) async fn publish_wrap(client: &Client, wrap: &Event, relays: &[RelayUrl]) {
    let sent = if relays.is_empty() {
        client.send_event(wrap).broadcast().await
    } else {
        client.send_event(wrap).to(relays.iter().cloned()).await
    };

    match sent {
        Ok(output) if output.failed.is_empty() => {}
        Ok(output) => log::warn!(
            "community: {} relay(s) rejected {}",
            output.failed.len(),
            wrap.id
        ),
        Err(error) => log::warn!("community: publishing {} failed: {error}", wrap.id),
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

    let entry = list_entry(state, name);
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
        None => return cache::load_states(client).await,
    };

    let mut held: BTreeMap<CommunityId, CommunityState> = cache::load_states(client)
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

        let mut state = match held.remove(&entry.community_id) {
            Some(materialized) => refresh(materialized, fresh),
            None => fresh,
        };

        retain_join_root(&mut state, &entry.seed);
        adopt_list_material(&mut state, &entry.seed);
        adopt_list_material(&mut state, &entry.current);

        cache::save_state(client, &state).await?;
        held.insert(entry.community_id, state);
    }

    Ok(held.into_values().collect())
}

/// Take the snapshot authority and retained roots a List entry names.
fn adopt_list_material(state: &mut CommunityState, material: &JoinMaterial) {
    if material.root_epoch.0 > 0
        && let Some(refounder) = material.refounder()
    {
        state.refounders.insert(refounder);
    }

    for root in material.held_roots() {
        if root.epoch >= state.root_epoch || root.epoch.0 == 0 {
            continue;
        }

        if let Some(refounder) = root.refounder {
            state.refounders.insert(refounder);
        }

        retain_root(state, root.epoch, root.key, root.control_pk);
    }
}

fn retain_join_root(state: &mut CommunityState, seed: &JoinMaterial) {
    if seed.root_epoch >= state.root_epoch {
        return;
    }

    let Ok(key) = decode_hex_32(&seed.community_root) else {
        return;
    };

    retain_root(state, seed.root_epoch, key, seed.control_pk);
}

/// Retain a root the community has rotated past, newest first.
fn retain_root(
    state: &mut CommunityState,
    epoch: Epoch,
    key: [u8; 32],
    control_pk: Option<PublicKey>,
) {
    if state.held_roots.iter().any(|root| root.epoch == epoch) {
        return;
    }

    let at = state
        .held_roots
        .iter()
        .position(|root| root.epoch < epoch)
        .unwrap_or(state.held_roots.len());

    state.held_roots.insert(
        at,
        HeldRoot {
            epoch,
            key,
            control_pk,
            retired_at: None,
        },
    );
}

fn retired(list: &CommunityList, id: &CommunityId, added_at_ms: u64) -> bool {
    list.tombstones
        .iter()
        .find(|tombstone| tombstone.community_id == *id)
        .is_some_and(|tombstone| tombstone.removed_at >= added_at_ms)
}

fn refresh(mut held: CommunityState, fresh: CommunityState) -> CommunityState {
    if fresh.root_epoch > held.root_epoch {
        let (epoch, key) = (held.root_epoch, held.community_root);
        let control_pk = held.control_pks.get(&epoch.0).copied();
        retain_root(&mut held, epoch, key, control_pk);
    }

    held.owner = fresh.owner;
    held.owner_salt = fresh.owner_salt;
    held.community_root = fresh.community_root;
    held.root_epoch = fresh.root_epoch;
    held.added_at_ms = fresh.added_at_ms;

    if fresh.control_root.is_some() {
        held.control_root = fresh.control_root;
    }

    if let Some(name) = fresh.name {
        held.name = Some(name);
    }

    for (epoch, address) in fresh.control_pks {
        held.control_pks.insert(epoch, address);
    }

    held.relays = fresh.relays;

    for channel in fresh.channels {
        let cut = held.channel_cuts.get(&channel.id).copied();

        match held.channels.iter_mut().find(|held| held.id == channel.id) {
            Some(held) => {
                held.name = channel.name;

                if cut.is_some_and(|cut| channel.epoch <= cut) {
                    continue;
                }

                if channel.private {
                    held.private = true;

                    if let Some(key) = channel.key
                        && (held.key != Some(key) || held.epoch != channel.epoch)
                    {
                        // The key being superseded still reads everything
                        // written under it, so it is retained, never overwritten.
                        if let Some((epoch, previous)) = held.current()
                            && !held.priors.iter().any(|prior| prior.epoch == epoch)
                        {
                            held.priors.push(HeldKey {
                                epoch,
                                key: previous,
                                retired_at: None,
                            });
                        }

                        held.key = Some(key);
                        held.epoch = channel.epoch;
                    }
                }
            }
            None => {
                if cut.is_none() {
                    held.channels.push(channel);
                }
            }
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

    let wraps = client.database().query(plane_filter(&planes)).await?;

    let mut editions = Vec::new();
    let mut observed: BTreeMap<PublicKey, u64> = BTreeMap::new();
    let mut guestbook_rumors = Vec::new();
    let mut unreadable: BTreeMap<ChannelId, usize> = BTreeMap::new();
    let mut cached: BTreeMap<ChannelId, BTreeMap<EventId, Observed>> = BTreeMap::new();

    for plane in &planes {
        if let PlaneKind::Channel(channel, _) = plane.kind
            && !cached.contains_key(&channel)
        {
            cached.insert(channel, cache::wrapper_index(client, &channel).await?);
        }
    }

    for wrap in &wraps {
        let Some(plane) = planes.iter().find(|plane| plane.address == wrap.pubkey) else {
            continue;
        };

        match plane.kind {
            PlaneKind::Control(_) => {
                // A retired root reads only what was sealed before its rotation.
                if !plane.accepts(wrap) {
                    continue;
                }

                if let Ok(edition) = cord02::open_edition(wrap, &plane.group, &plane.address, true)
                {
                    observe(
                        &mut observed,
                        edition.author,
                        wrap.created_at.as_secs().saturating_mul(1000),
                    );
                    editions.push(edition);
                }
            }
            PlaneKind::Guestbook => {
                if !plane.accepts(wrap) {
                    continue;
                }

                if let Ok((_, rumor)) = cord02::guestbook::open(wrap, &plane.group) {
                    observe(&mut observed, rumor.author, rumor.at_ms);
                    guestbook_rumors.push(rumor);
                }
            }
            PlaneKind::Channel(channel, epoch) => {
                if let Some(row) = cached.get(&channel).and_then(|index| index.get(&wrap.id)) {
                    observe(&mut observed, row.author, row.at_ms);
                    continue;
                }

                let opened = if plane.accepts(wrap) {
                    concord::cord03::open(wrap, &plane.group, &channel, epoch).ok()
                } else {
                    None
                };

                match opened {
                    Some((opened, rumor)) => {
                        cache::cache_rumor(client, &channel, &opened).await?;
                        observe(&mut observed, rumor.author, rumor.at_ms);
                    }
                    None => *unreadable.entry(channel).or_default() += 1,
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
    let coalesced =
        cord02::guestbook::coalesce(&guestbook_rumors, now_ms, &state.refounders, can_kick);

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
    cache::save_state(client, &state).await?;

    Ok(Some(Snapshot {
        state,
        control,
        members,
        unreadable,
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
    use std::time::Duration;

    use nostr_memory::MemoryDatabase;

    use super::*;

    fn client() -> Client {
        ClientBuilder::default()
            .database(MemoryDatabase::unbounded())
            .build()
    }

    #[test]
    fn planes_address_the_control_guestbook_and_every_readable_channel() {
        let owner = Keys::generate().public_key();
        let control_pk = Keys::generate().public_key();
        let general = ChannelId::from_bytes([0x9c; 32]);
        let staff = ChannelId::from_bytes([0x9d; 32]);

        let state = CommunityState {
            id: CommunityId::from_bytes([0x42; 32]),
            name: Some("Anime and Manga".to_owned()),
            owner,
            owner_salt: [0x01; 32],
            community_root: [0x02; 32],
            root_epoch: Epoch(0),
            control_root: None,
            control_pks: BTreeMap::from([(0, control_pk)]),
            channels: vec![
                concord::state::ChannelKeyRef {
                    id: general,
                    name: "general".to_owned(),
                    private: false,
                    epoch: Epoch(0),
                    key: None,
                    priors: Vec::new(),
                },
                concord::state::ChannelKeyRef {
                    id: staff,
                    name: "staff".to_owned(),
                    private: true,
                    epoch: Epoch(0),
                    key: Some([0x04; 32]),
                    priors: Vec::new(),
                },
                concord::state::ChannelKeyRef {
                    id: ChannelId::from_bytes([0x9e; 32]),
                    name: "locked".to_owned(),
                    private: true,
                    epoch: Epoch(0),
                    key: None,
                    priors: Vec::new(),
                },
            ],
            relays: vec![RelayUrl::parse("wss://relay.example").expect("a url")],
            heads: Vec::new(),
            banned: BTreeSet::new(),
            cursors: BTreeMap::new(),
            held_roots: Vec::new(),
            channel_cuts: BTreeMap::new(),
            refounders: BTreeSet::new(),
            removed_at: None,
            stranded: false,
            dissolved: false,
            added_at_ms: 0,
        };

        let planes = planes(&state).expect("planes");

        assert_eq!(planes.len(), 4);
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
        assert!(
            planes
                .iter()
                .any(|plane| matches!(plane.kind, PlaneKind::Channel(id, _) if id == staff)),
            "a private channel whose key is held is subscribed"
        );

        let filter = plane_filter(&planes);
        let addresses: BTreeSet<PublicKey> = planes.iter().map(|plane| plane.address).collect();
        assert_eq!(filter.authors, Some(addresses));
        assert_eq!(
            filter.kinds,
            Some(BTreeSet::from([
                Kind::GiftWrap,
                Kind::Custom(KIND_WRAP_EPHEMERAL)
            ])),
            "the standing subscription asks for both wrap kinds"
        );
    }

    /// A cold subscription asks wide; a warm one resumes at the oldest held
    /// cursor, minus the overlap, so no channel's new region is skipped. A floor
    /// the local clock has not reached is a peer's stamp and is clamped, or the
    /// subscription would ask from a region that is still in the future.
    #[test]
    fn the_live_window_is_wide_cold_and_resumes_at_the_oldest_cursor_warm() {
        let mut state = held(
            CommunityId::from_bytes([0x42; 32]),
            Keys::generate().public_key(),
        );
        let channel = state.channels[0].id;
        let now = Timestamp::now();

        assert_eq!(live_window(&state, now), Window::default());

        state.cursors.insert(
            channel,
            concord::state::ChannelCursor {
                newest: Some(Timestamp::from_secs(2_000_000)),
                oldest: Some(Timestamp::from_secs(1_000)),
                exhausted: false,
            },
        );
        assert_eq!(
            live_window(&state, now),
            Window {
                since: Some(Timestamp::from_secs(2_000_000) - CURSOR_OVERLAP),
                until: None,
            }
        );

        let ahead = now + Duration::from_secs(3_600);
        state.cursors.insert(
            channel,
            concord::state::ChannelCursor {
                newest: Some(ahead),
                oldest: Some(Timestamp::from_secs(1_000)),
                exhausted: false,
            },
        );
        assert_eq!(
            live_window(&state, now),
            Window {
                since: Some(now - CURSOR_OVERLAP),
                until: None,
            },
            "a cursor stamped in the future must not open the REQ ahead of now"
        );
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
            name: Some("Anime and Manga".to_owned()),
            owner: Keys::generate().public_key(),
            owner_salt: [0x01; 32],
            community_root: [0x02; 32],
            root_epoch: Epoch(0),
            control_root: Some([0x03; 32]),
            control_pks: BTreeMap::from([(0, control_pk)]),
            channels: vec![concord::state::ChannelKeyRef {
                id: ChannelId::from_bytes([0x9c; 32]),
                name: "general".to_owned(),
                private: false,
                epoch: Epoch(0),
                key: None,
                priors: Vec::new(),
            }],
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

    /// A Refounding snapshot seeds members this client has never seen publish —
    /// but only on the authority of the npub whose rotation minted the epoch it
    /// seeds (CORD-02 §5), which is why the refounder is recorded at all.
    #[test]
    fn a_refounders_snapshot_seeds_the_members_it_names() {
        smol::block_on(async {
            let client = client();
            let keys = Keys::generate();
            let signer = UniversalSigner::new(keys.clone());
            let refounder = Keys::generate();
            let quiet = Keys::generate().public_key();

            let created = create(&client, &signer, &metadata("coop"))
                .await
                .expect("creates");

            // Past genesis, and past the rotation whose refounder this client
            // verified: genesis mints no epoch by rotation, so it has no
            // snapshot authority at all.
            let mut state = created;
            state.root_epoch = Epoch(1);
            state.refounders.insert(refounder.public_key());

            let group = guestbook_group_key(&state.community_root, &state.id, state.root_epoch)
                .expect("a guestbook plane");
            let chunks = cord02::guestbook::build_snapshot_chunks(
                refounder.public_key(),
                &[quiet],
                [0x77u8; 32],
                state.added_at_ms,
            );

            for chunk in &chunks {
                let (wrap, _) = cord02::guestbook::seal_rumor(chunk, &group, &refounder)
                    .await
                    .expect("seals");
                client.database().save_event(&wrap).await.expect("saves");
            }

            let seeded = fold(&client, &state)
                .await
                .expect("folds")
                .expect("a control plane");
            assert!(
                seeded.members.contains(&quiet),
                "the snapshot's members are the roster"
            );

            // Without the rotation that minted the epoch, the same chunks seed
            // nobody: an unverifiable snapshot is not an authority.
            let mut unverified = state.clone();
            unverified.refounders.clear();

            let folded = fold(&client, &unverified)
                .await
                .expect("folds")
                .expect("a control plane");
            assert!(!folded.members.contains(&quiet));
        });
    }

    /// A List entry that names the npub whose Refounding minted its epoch hands
    /// this client the authority its Guestbook snapshot is honored on, which is
    /// how a device that never held the rotation still reads the seeded roster.
    /// (What the fold then does with that authority is the neighboring test.)
    #[test]
    fn a_list_entrys_refounder_becomes_the_snapshot_authority() {
        smol::block_on(async {
            let client = client();
            let keys = Keys::generate();
            let signer = UniversalSigner::new(keys.clone());
            let refounder = Keys::generate();

            let joined = held(
                CommunityId::from_bytes([0x42; 32]),
                Keys::generate().public_key(),
            );
            let mut rotated = joined.clone();
            rotated.root_epoch = Epoch(2);
            rotated.community_root = [0x44; 32];

            let mut entry = list_entry(&rotated, "coop");
            entry.seed = list_entry(&joined, "coop").seed;
            entry.added_at = joined.added_at_ms;
            entry.current.extra.insert(
                "refounder".to_owned(),
                serde_json::Value::String(refounder.public_key().to_hex()),
            );

            let list = CommunityList::default().joined(entry);
            store_fragment(&client, &signer, &list).await;

            let loaded = load(&client, &signer, keys.public_key())
                .await
                .expect("loads");
            let state = loaded
                .iter()
                .find(|state| state.id == joined.id)
                .expect("loaded");

            assert_eq!(state.root_epoch, Epoch(2));
            assert!(state.refounders.contains(&refounder.public_key()));
        });
    }

    /// A List that has rotated on keeps the root of our join: the material is
    /// the community as we were given it, and the planes that root addressed
    /// stay readable only while it is held.
    #[test]
    fn a_list_that_moved_the_root_on_retains_the_root_of_our_join() {
        smol::block_on(async {
            let client = client();
            let keys = Keys::generate();
            let signer = UniversalSigner::new(keys.clone());

            let joined = held(
                CommunityId::from_bytes([0x42; 32]),
                Keys::generate().public_key(),
            );
            let mut rotated = joined.clone();
            rotated.root_epoch = Epoch(2);
            rotated.community_root = [0x44; 32];

            // The entry as a community that rotated twice since would carry it:
            // the join's material as the seed, the current material as current.
            let mut entry = list_entry(&rotated, "coop");
            entry.seed = list_entry(&joined, "coop").seed;
            entry.added_at = joined.added_at_ms;

            let list = CommunityList::default().joined(entry);
            store_fragment(&client, &signer, &list).await;

            let loaded = load(&client, &signer, keys.public_key())
                .await
                .expect("loads");
            let state = loaded
                .iter()
                .find(|state| state.id == joined.id)
                .expect("loaded");

            assert_eq!(state.root_epoch, Epoch(2));
            assert_eq!(state.community_root, [0x44; 32]);
            assert_eq!(
                state
                    .held_roots
                    .iter()
                    .map(|root| (root.epoch, root.key))
                    .collect::<Vec<_>>(),
                vec![(joined.root_epoch, joined.community_root)]
            );
        });
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

            // The plane filter must address the genesis wraps, or a fold would
            // read a plane nothing is ever published on.
            let planes = planes(&created).expect("planes");
            let wraps = client
                .database()
                .query(plane_filter(&planes))
                .await
                .expect("queries");
            assert_eq!(wraps.len(), created.heads.len());
            assert!(wraps.iter().all(|wrap| wrap.kind == Kind::GiftWrap));

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

    /// The reference counts anyone seen publishing anywhere, and a control edit
    /// is the one publication that leaves no other trace: an npub that has
    /// never joined a channel and never touched the Guestbook exists in no
    /// other plane, so a fold that reads only those reads them as a stranger.
    #[test]
    fn a_control_editions_author_is_observed_as_a_member() {
        smol::block_on(async {
            let client = client();
            let keys = Keys::generate();
            let signer = UniversalSigner::new(keys.clone());
            let editor = Keys::generate();

            let created = create(&client, &signer, &metadata("coop"))
                .await
                .expect("creates");

            let community_head = created
                .heads
                .iter()
                .find(|head| head.entity == *created.id.as_bytes())
                .expect("a community head");
            let writer = cord02::ControlWriter {
                author: editor.public_key(),
                read: control_group_key(&created.community_root, &created.id, cord02::ROOT_EPOCH)
                    .expect("a reading key"),
                signer: control_signer_group_key(
                    &created.control_root.expect("a control root"),
                    &created.id,
                    cord02::ROOT_EPOCH,
                )
                .expect("a signing key"),
            };

            // The editor holds no grant, so the edit is inert: it supersedes
            // nothing. It is still a publication by that npub.
            let (wrap, _) = writer
                .set_community_metadata(
                    &editor,
                    &created.id,
                    &metadata("coop two"),
                    Some(community_head),
                    None,
                    Timestamp::now().as_secs() + 1,
                )
                .await
                .expect("publishes");
            client.database().save_event(&wrap).await.expect("saves");

            let snapshot = fold(&client, &created)
                .await
                .expect("folds")
                .expect("a control plane");

            assert!(
                snapshot.members.contains(&editor.public_key()),
                "an edition's author is a member the fold has seen publishing"
            );
            assert_eq!(
                snapshot
                    .control
                    .community
                    .as_ref()
                    .map(|metadata| metadata.name.as_str()),
                Some("coop"),
                "an unauthorized edit still changes nothing"
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
            let list = CommunityList::default().joined(list_entry(&listed, "coop"));
            store_fragment(&client, &signer, &list).await;

            assert!(
                cache::load_state(&client, &listed.id)
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
                cache::load_state(&client, &listed.id)
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

            let list = CommunityList::default().joined(list_entry(&listed, "listed"));
            store_fragment(&client, &signer, &list).await;
            cache::save_state(&client, &local).await.expect("saves");

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
            cache::save_state(&client, &local).await.expect("saves");

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

    fn event_at(at_ms: u64) -> Event {
        EventBuilder::new(Kind::TextNote, "wrap")
            .custom_created_at(Timestamp::from_secs(at_ms / 1000))
            .finalize(&Keys::generate())
            .expect("signs")
    }

    /// A key a rotation stepped off still reads its own history, but nothing
    /// sealed after the rotation published it.
    #[test]
    fn a_superseded_key_reads_only_up_to_the_rotation_that_retired_it() {
        let channel = ChannelId::from_bytes([0x9c; 32]);
        let mut state = held(
            CommunityId::from_bytes([0x42; 32]),
            Keys::generate().public_key(),
        );

        state.channels = vec![concord::state::ChannelKeyRef {
            id: channel,
            name: "staff".to_owned(),
            private: true,
            epoch: Epoch(1),
            key: Some([0x08; 32]),
            priors: vec![HeldKey {
                epoch: Epoch(0),
                key: [0x07; 32],
                retired_at: Some(Timestamp::from_secs(1_000)),
            }],
        }];

        let planes = planes(&state).expect("planes");
        let retired = planes
            .iter()
            .find(|plane| plane.kind == PlaneKind::Channel(channel, Epoch(0)))
            .expect("the retired epoch keeps a plane");

        assert_eq!(retired.retired_at, Some(Timestamp::from_secs(1_000)));
        assert!(retired.accepts(&event_at(1_000_000)));
        assert!(!retired.accepts(&event_at(1_001_000)));
    }

    /// A public channel reads one plane per held root epoch, and the plane for
    /// the CURRENT root is the one the community writes to now.
    ///
    /// A Refounding moves the community root to a new epoch and every public
    /// channel's plane with it (CORD-03 §1: a public channel's secret is the
    /// community root, at the root epoch). Material the channel carries from
    /// before the rotation names the old epoch, so a reader that trusts it asks
    /// a retired address forever: the channel shows everything written before
    /// the rotation and nothing after, while the control and guestbook planes —
    /// which derive from the roots directly — stay current.
    #[test]
    fn a_refounding_moves_a_public_channels_plane_to_the_new_root_epoch() {
        smol::block_on(async {
            let client = client();
            let keys = Keys::generate();
            let signer = UniversalSigner::new(keys.clone());

            let mut state = create(&client, &signer, &metadata("coop"))
                .await
                .expect("creates");
            let channel = state.channels[0].id;
            let rotated_at = Timestamp::now();
            let prior_root = state.community_root;

            // A Refounding: the root moves to epoch 1, the prior one is held for
            // history, and the channel's own material still names epoch 0.
            state.community_root = [0x5a; 32];
            state.root_epoch = Epoch(1);
            state.held_roots = vec![HeldRoot {
                epoch: Epoch(0),
                key: prior_root,
                control_pk: None,
                retired_at: Some(rotated_at),
            }];

            assert_eq!(state.channels[0].epoch, Epoch(0));

            let current = channel_group_key(&state.community_root, &channel, state.root_epoch)
                .expect("derives");
            let plane = planes(&state)
                .expect("planes")
                .into_iter()
                .find(|plane| plane.address == current.pk())
                .expect("the current root's channel plane is subscribed and read");

            assert_eq!(plane.kind, PlaneKind::Channel(channel, Epoch(1)));
            assert!(
                plane.accepts(
                    &EventBuilder::new(Kind::GiftWrap, "")
                        .finalize(plane.group.keys())
                        .expect("signs")
                )
            );

            // And a message sealed at the current plane folds into the channel,
            // which is what the timeline reads.
            let rumor = concord::cord03::build_message(
                keys.public_key(),
                &channel,
                state.root_epoch,
                "after the refounding",
                None,
                rotated_at.as_secs().saturating_mul(1000),
                None,
            );
            let (wrap, _) = concord::cord03::seal_rumor(&rumor, &plane.group, &signer, false)
                .await
                .expect("seals");
            client.database().save_event(&wrap).await.expect("saves");

            let snapshot = fold(&client, &state)
                .await
                .expect("folds")
                .expect("a control plane");
            assert_eq!(snapshot.unreadable.get(&channel).copied(), None);

            let cached = cache::query_rumors(
                &client,
                &channel,
                None,
                10,
                Some(&concord::cord03::ROW_KINDS),
            )
            .await
            .expect("reads");

            assert_eq!(cached.len(), 1, "a message at the new epoch reads back");
        });
    }

    /// A wrap a subscription delivered lands in the database and nowhere else.
    /// The fold is what turns it into a cached rumor, which is the only thing
    /// the timeline reads, so a live message depends on this step.
    #[test]
    fn a_fold_caches_a_channel_wrap_a_relay_delivered() {
        smol::block_on(async {
            let client = client();
            let keys = Keys::generate();
            let signer = UniversalSigner::new(keys.clone());

            let created = create(&client, &signer, &metadata("coop"))
                .await
                .expect("creates");

            let (channel, epoch, plane) = planes(&created)
                .expect("planes")
                .into_iter()
                .find_map(|plane| match plane.kind {
                    PlaneKind::Channel(channel, epoch) => Some((channel, epoch, plane)),
                    _ => None,
                })
                .expect("a channel plane");

            let rumor = concord::cord03::build_message(
                keys.public_key(),
                &channel,
                epoch,
                "a live message",
                None,
                1_700_000_000_000,
                None,
            );
            let (wrap, _) = concord::cord03::seal_rumor(&rumor, &plane.group, &signer, false)
                .await
                .expect("seals");

            // What the SDK does with a wrap a standing subscription delivered.
            client.database().save_event(&wrap).await.expect("saves");

            let snapshot = fold(&client, &created)
                .await
                .expect("folds")
                .expect("a control plane");

            assert_eq!(snapshot.unreadable.get(&channel).copied(), None);

            let cached = cache::query_rumors(
                &client,
                &channel,
                None,
                10,
                Some(&concord::cord03::ROW_KINDS),
            )
            .await
            .expect("reads");

            assert_eq!(cached.len(), 1, "the delivered wrap is cached as a rumor");
            assert_eq!(cached[0].pubkey, keys.public_key());
        });
    }

    /// A wrap addressed to a held channel plane that will not open is counted, so
    /// the panel can tell a quiet room from one whose history it cannot read.
    #[test]
    fn a_fold_counts_the_channel_wraps_it_cannot_open() {
        smol::block_on(async {
            let client = client();
            let keys = Keys::generate();
            let signer = UniversalSigner::new(keys.clone());

            let created = create(&client, &signer, &metadata("coop"))
                .await
                .expect("creates");

            let (channel, plane) = planes(&created)
                .expect("planes")
                .into_iter()
                .find_map(|plane| match plane.kind {
                    PlaneKind::Channel(channel, _) => Some((channel, plane)),
                    _ => None,
                })
                .expect("a channel plane");

            // Sealed to the channel's own address, but not a seal at all, so no
            // held key opens it.
            let junk = EventBuilder::new(Kind::GiftWrap, "not a seal")
                .custom_created_at(Timestamp::from_secs(1_700_000_000))
                .finalize(plane.group.keys())
                .expect("signs");
            client.database().save_event(&junk).await.expect("saves");

            let snapshot = fold(&client, &created)
                .await
                .expect("folds")
                .expect("a control plane");

            assert_eq!(snapshot.unreadable.get(&channel).copied(), Some(1));
        });
    }
}
