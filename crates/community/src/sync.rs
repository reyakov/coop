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
    client: &Client,
    signer: &UniversalSigner,
    self_pk: PublicKey,
) -> Result<Vec<CommunityState>> {
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

    if let Some(list) = load_list(client, signer, self_pk).await? {
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

    let json = signer.nip44_decrypt_async(&self_pk, &event.content).await?;

    Ok(Some(serde_json::from_str(&json)?))
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
