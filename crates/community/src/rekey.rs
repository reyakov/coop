use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::sync::{Arc, Mutex, MutexGuard};

use anyhow::{Result, bail};
use concord::cord01::{self, KIND_WRAP_EPHEMERAL, SealForm};
use concord::cord04::roles::{CommunityRoles, Permissions};
use concord::cord04::{self, AuthorityCitation};
use concord::cord06::{
    self, Continuity, Refounding, RekeyScope, Rotation, RotationKey, RotationPlan,
};
use concord::derive::{GroupKey, channel_rekey_group_key, epoch_key_commitment};
use concord::state::{CommunityState, HeldKey};
use concord::{ChannelId, CommunityId, Epoch};
use nostr_sdk::prelude::*;
use state::UniversalSigner;

use crate::sync::{self, PlaneKind};

/// Epochs ahead of a held epoch a rotation is looked for.
pub const REKEY_LOOKAHEAD: u64 = 8;
/// How much of what a relay stores a rekey watch replays.
const REKEY_REPLAY: usize = 200;

/// One address the rekey watch asks a relay for, and what a wrap at it means.
#[derive(Debug, Clone)]
pub struct Watch {
    pub address: PublicKey,
    pub group: GroupKey,
    pub scope: RekeyScope,
    pub epoch: Epoch,
}

/// The permission a rotation of `scope` is judged under, by whoever reads it and
/// by this client when it publishes one.
fn permissions(scope: RekeyScope) -> &'static [u64] {
    match scope {
        RekeyScope::Base => &[Permissions::BAN],
        RekeyScope::Channel(_) => &[Permissions::MANAGE_CHANNELS, Permissions::BAN],
    }
}

/// Every address a community's rotations can arrive at.
///
/// The base scope watches the epoch after *every* held root, not only the
/// current one: a rotation published while this client was away is read from
/// the root it stepped off, and the npub that minted an epoch is only knowable
/// from the rotation that minted it.
pub fn watches(state: &CommunityState) -> Result<Vec<Watch>> {
    let mut watches = Vec::new();
    let roots = state.roots();

    for root in &roots {
        let next = Epoch(root.epoch.0 + 1);
        let group = cord06::rekey_group(RekeyScope::Base, &root.key, &state.id, next)?;
        watches.push(Watch {
            address: group.pk(),
            group,
            scope: RekeyScope::Base,
            epoch: next,
        });
    }

    for channel in &state.channels {
        if !channel.private {
            continue;
        }

        for root in &roots {
            for ahead in 1..=REKEY_LOOKAHEAD {
                let epoch = Epoch(channel.epoch.0 + ahead);
                let group = channel_rekey_group_key(&root.key, &channel.id, epoch)?;
                watches.push(Watch {
                    address: group.pk(),
                    group,
                    scope: RekeyScope::Channel(channel.id),
                    epoch,
                });
            }
        }
    }

    Ok(watches)
}

pub fn watch_filter(watches: &[Watch]) -> Filter {
    Filter::new()
        .kind(Kind::GiftWrap)
        .authors(watches.iter().map(|watch| watch.address))
        .limit(REKEY_REPLAY)
}

/// The subscription carrying a community's rekey watch.
pub fn subscription_id(id: &CommunityId) -> SubscriptionId {
    SubscriptionId::new(format!("rekey-{}", &id.to_hex()[..32]))
}

/// Which community a rekey watch subscription belongs to.
#[derive(Default)]
pub struct WatchRegistry {
    watches: Arc<Mutex<HashMap<SubscriptionId, CommunityId>>>,
}

impl Clone for WatchRegistry {
    fn clone(&self) -> Self {
        Self {
            watches: Arc::clone(&self.watches),
        }
    }
}

impl WatchRegistry {
    pub fn register(&self, id: SubscriptionId, community: CommunityId) {
        self.lock().insert(id, community);
    }

    pub fn community_of(&self, id: &SubscriptionId) -> Option<CommunityId> {
        self.lock().get(id).copied()
    }

    /// Forget every watch, because the account that installed them is gone.
    pub fn clear(&self) {
        self.lock().clear();
    }

    fn lock(&self) -> MutexGuard<'_, HashMap<SubscriptionId, CommunityId>> {
        match self.watches.lock() {
            Ok(watches) => watches,
            Err(poisoned) => poisoned.into_inner(),
        }
    }
}

/// What one adoption pass learned, ready to be folded into the held state.
#[derive(Debug, Default)]
pub struct Adoptions {
    pub base: Option<BaseAdoption>,
    pub channels: Vec<ChannelAdoption>,
    /// Channels a complete rotation removed us from, with the epoch that did it.
    pub cuts: Vec<(ChannelId, Epoch)>,
    /// The base epoch a complete rotation excluded us at.
    pub removed_at: Option<Epoch>,
    pub stranded: bool,
    /// The npubs whose rotations minted an epoch of this community.
    pub refounders: BTreeSet<PublicKey>,
}

impl Adoptions {
    pub fn is_empty(&self) -> bool {
        self.base.is_none()
            && self.channels.is_empty()
            && self.cuts.is_empty()
            && self.removed_at.is_none()
            && !self.stranded
            && self.refounders.is_empty()
    }
}

#[derive(Debug, Clone)]
pub struct BaseAdoption {
    pub epoch: Epoch,
    pub key: [u8; 32],
    pub control_pk: Option<PublicKey>,
    /// The new Control Plane signing root, delivered to staff only.
    pub control_root: Option<[u8; 32]>,
    pub stepped: Vec<HeldKey>,
}

#[derive(Debug, Clone)]
pub struct ChannelAdoption {
    pub channel: ChannelId,
    pub epoch: Epoch,
    pub key: [u8; 32],
    /// The keys this rotation stepped off, newest first.
    pub stepped: Vec<HeldKey>,
}

/// Read every rekey wrap the local database holds and adopt what is admissible.
pub async fn adopt(
    client: &Client,
    state: &CommunityState,
    roles: &CommunityRoles,
    signer: &UniversalSigner,
    me: PublicKey,
) -> Result<Adoptions> {
    let watches = watches(state)?;

    let mut authors: BTreeSet<PublicKey> = BTreeSet::new();
    for watch in &watches {
        authors.insert(watch.address);
    }

    if authors.is_empty() {
        return Ok(Adoptions::default());
    }

    let wraps = client
        .database()
        .query(Filter::new().kind(Kind::GiftWrap).authors(authors))
        .await?;

    let mut chunks = Vec::new();
    let mut published: BTreeMap<RotationKey, u64> = BTreeMap::new();

    for wrap in &wraps {
        let Some(watch) = watches.iter().find(|watch| watch.address == wrap.pubkey) else {
            continue;
        };

        let Ok(opened) = cord01::open_wrap(wrap, &watch.group) else {
            continue;
        };

        let Ok(chunk) = cord06::parse_rekey_chunk(&opened) else {
            continue;
        };

        if chunk.scope != watch.scope || chunk.new_epoch != watch.epoch {
            continue;
        }

        let at_ms = wrap.created_at.as_secs().saturating_mul(1000);
        published
            .entry(chunk.correlation())
            .and_modify(|held| *held = (*held).min(at_ms))
            .or_insert(at_ms);

        chunks.push(chunk);
    }

    if chunks.is_empty() {
        return Ok(Adoptions::default());
    }

    let rotations = cord06::collect_rotations(&chunks);
    let mut adoptions = Adoptions::default();

    let mut base = walk(
        RekeyScope::Base,
        permissions(RekeyScope::Base),
        state.root_epoch,
        state.community_root,
        state,
        roles,
        signer,
        me,
        &rotations,
        &published,
    )
    .await?;

    let mut refounders: BTreeSet<PublicKey> = base.refounders.clone();

    for root in state.roots().into_iter().skip(1) {
        let prior = walk(
            RekeyScope::Base,
            permissions(RekeyScope::Base),
            root.epoch,
            root.key,
            state,
            roles,
            signer,
            me,
            &rotations,
            &published,
        )
        .await?;

        refounders.extend(prior.refounders.iter().copied());

        let Some(adopted) = prior.adopted else {
            continue;
        };

        if base
            .adopted
            .as_ref()
            .is_none_or(|held| adopted.epoch > held.epoch)
        {
            base.adopted = Some(adopted);
        }
    }

    if let Some(step) = base.adopted {
        adoptions.base = Some(BaseAdoption {
            epoch: step.epoch,
            key: step.key,
            control_pk: step.control_pk,
            control_root: step.control_root,
            stepped: step.stepped,
        });
    }

    adoptions.refounders = refounders;
    adoptions.removed_at = base.removed_at;
    adoptions.stranded = base.stranded;

    for channel in &state.channels {
        if !channel.private {
            continue;
        }

        let Some((held_epoch, held_key)) = channel.current() else {
            continue;
        };

        let step = walk(
            RekeyScope::Channel(channel.id),
            permissions(RekeyScope::Channel(channel.id)),
            held_epoch,
            held_key,
            state,
            roles,
            signer,
            me,
            &rotations,
            &published,
        )
        .await?;

        if let Some(adopted) = step.adopted {
            adoptions.channels.push(ChannelAdoption {
                channel: channel.id,
                epoch: adopted.epoch,
                key: adopted.key,
                stepped: adopted.stepped,
            });
        }

        if let Some(epoch) = step.removed_at {
            adoptions.cuts.push((channel.id, epoch));
        }
    }

    Ok(adoptions)
}

/// A rotation this client is asked to perform.
#[derive(Debug, Clone)]
pub struct Rewrite {
    pub scope: RekeyScope,
    /// Every member the new key must reach.
    pub recipients: Vec<PublicKey>,
    /// The members it must not reach: whoever the rotation cuts off.
    pub excluded: Vec<PublicKey>,
    /// The rotator's claim to rank, when it holds the grant to cite.
    pub citation: Option<AuthorityCitation>,
}

impl Rewrite {
    /// Whether `rotator` may cut `excluded` off at all.
    pub fn authorized(
        &self,
        roles: &CommunityRoles,
        owner: &PublicKey,
        rotator: &PublicKey,
    ) -> bool {
        permissions(self.scope)
            .iter()
            .any(|bits| cord06::rekey_authorized(roles, owner, rotator, *bits, &self.excluded))
    }
}

/// Publish this client's own rotation of one scope to its next epoch.
#[allow(clippy::too_many_arguments)]
pub async fn rotate(
    client: &Client,
    state: &CommunityState,
    roles: &CommunityRoles,
    signer: &UniversalSigner,
    me: PublicKey,
    rewrite: &Rewrite,
    at: Timestamp,
) -> Result<Epoch> {
    if rewrite.excluded.contains(&me) {
        bail!("a rotation cannot cut off the member who publishes it");
    }

    if !rewrite.recipients.contains(&me) {
        bail!("a rotation must deliver its own rotator");
    }

    if rewrite
        .excluded
        .iter()
        .any(|target| rewrite.recipients.contains(target))
    {
        bail!("a rotation cannot both deliver to and cut off the same member");
    }

    let (held_epoch, held_key) = stepping_off(state, rewrite.scope)?;
    let epoch = Epoch(held_epoch.0 + 1);

    if epoch.0 > cord06::MAX_REKEY_EPOCH {
        bail!("epoch {} is past the rekey ceiling", epoch.0);
    }

    let plan = cord06::plan_rotation(rewrite.scope, epoch)?;
    let new_key = plan.new_key();
    let control_pk = match &plan {
        RotationPlan::Base(refounding) => Some(refounding.signer(&state.id)?.pk().to_bytes()),
        RotationPlan::Channel { .. } => None,
    };

    let mut blobs = Vec::with_capacity(rewrite.recipients.len());

    for recipient in &rewrite.recipients {
        // A refounded Control Plane's root reaches staff only.
        let control_root = match &plan {
            RotationPlan::Base(refounding) => roles
                .is_staff(recipient, &state.owner)
                .then_some(refounding.new_control_root),
            RotationPlan::Channel { .. } => None,
        };

        blobs.push(
            cord06::build_blob(
                signer,
                recipient,
                rewrite.scope,
                epoch,
                &new_key,
                control_pk.as_ref(),
                control_root.as_ref(),
            )
            .await?,
        );
    }

    let group = cord06::rekey_group(rewrite.scope, &state.community_root, &state.id, epoch)?;
    // A rotation that cuts somebody off is the severed kind.
    let severed = !rewrite.excluded.is_empty();

    let mut wraps = cord06::build_rekey_chunks(
        signer,
        &group,
        rewrite.scope,
        epoch,
        held_epoch,
        &epoch_key_commitment(held_epoch, &held_key),
        &blobs,
        rewrite.citation.as_ref(),
        severed,
        at.as_secs(),
    )
    .await?;

    if let RotationPlan::Base(refounding) = &plan {
        let carried = carry_heads(client, state, refounding, at).await?;

        if carried.is_empty() && !state.heads.is_empty() {
            log::warn!("community: a refounding carried no settled head forward");
        }

        wraps.extend(carried);
    }

    sync::publish_wraps(client, &wraps, &state.relays).await;

    log::debug!(
        "community: rotated epoch {} to {} for {} member(s)",
        held_epoch.0,
        epoch.0,
        blobs.len()
    );

    Ok(epoch)
}

/// The epoch and key a rotation steps off.
fn stepping_off(state: &CommunityState, scope: RekeyScope) -> Result<(Epoch, [u8; 32])> {
    match scope {
        RekeyScope::Base => Ok((state.root_epoch, state.community_root)),
        RekeyScope::Channel(channel) => state
            .channels
            .iter()
            .find(|held| held.id == channel)
            .and_then(|held| held.current())
            .ok_or_else(|| anyhow::anyhow!("no current key is held for {}", channel.to_hex())),
    }
}

/// Re-seal the settled control heads under a refounding's new groups.
///
/// A rotation only mints keys. Unless the heads ride across with it, the new
/// epoch's Control Plane starts empty, and a member whose material never carried
/// the root we are stepping off folds no roles, metadata or banlist at all.
async fn carry_heads(
    client: &Client,
    state: &CommunityState,
    refounding: &Refounding,
    at: Timestamp,
) -> Result<Vec<Event>> {
    if state.heads.is_empty() {
        return Ok(Vec::new());
    }

    let planes = sync::planes(state)?;
    let authors: BTreeSet<PublicKey> = planes
        .iter()
        .filter(|plane| matches!(plane.kind, PlaneKind::Control(_)))
        .map(|plane| plane.address)
        .collect();

    if authors.is_empty() {
        return Ok(Vec::new());
    }

    let wraps = client
        .database()
        .query(
            Filter::new()
                .kinds([Kind::GiftWrap, Kind::Custom(KIND_WRAP_EPHEMERAL)])
                .authors(authors),
        )
        .await?;

    let mut seals = Vec::new();
    let mut seen: BTreeSet<EventId> = BTreeSet::new();

    for wrap in &wraps {
        let Some(plane) = planes.iter().find(|plane| {
            matches!(plane.kind, PlaneKind::Control(_)) && plane.address == wrap.pubkey
        }) else {
            continue;
        };

        // A retired root reads only what was sealed before its rotation.
        if !plane.accepts(wrap) {
            continue;
        }

        let Ok(opened) =
            cord01::open_wrap_at(wrap, &plane.address, plane.group.conversation(), true)
        else {
            continue;
        };

        // Only a plaintext seal can be carried: `compact` re-signs nothing and
        // re-wraps the edition the author already sealed.
        if opened.seal_form != SealForm::Plaintext {
            continue;
        }

        let Ok(edition) = cord04::parse_edition(&opened.rumor) else {
            continue;
        };

        let settled = state.heads.iter().any(|head| {
            head.entity == edition.entity
                && head.version == edition.version
                && head.self_hash == edition.self_hash
        });

        // The same edition can be reachable twice once a head has been carried
        // forward before: a head is published once per refounding.
        if settled && seen.insert(opened.seal.id) {
            seals.push(opened.seal);
        }
    }

    if seals.is_empty() {
        return Ok(Vec::new());
    }

    Ok(cord06::compact(
        &seals,
        &refounding.read(&state.id)?,
        &refounding.signer(&state.id)?,
        at.as_secs(),
    )?)
}

/// The key grouping a rotation's chunks, recomputed from a collected rotation.
fn rotation_key(rotation: &Rotation) -> RotationKey {
    (
        rotation.rotator.to_bytes(),
        rotation.scope.id32(),
        rotation.new_epoch.0,
        rotation.prev_commit,
    )
}

/// What one scope's walk found.
#[derive(Debug, Default)]
struct Step {
    adopted: Option<Adopted>,
    removed_at: Option<Epoch>,
    stranded: bool,
    /// The rotators of every base rotation this walk verified, which is who may
    /// seed the Guestbook snapshot of an epoch they minted.
    refounders: BTreeSet<PublicKey>,
}

#[derive(Debug, Clone)]
struct Adopted {
    epoch: Epoch,
    key: [u8; 32],
    control_pk: Option<PublicKey>,
    control_root: Option<[u8; 32]>,
    stepped: Vec<HeldKey>,
}

/// What one rotation offered this client, and when it was published.
#[derive(Debug, Clone, Copy)]
struct Delivery {
    key: [u8; 32],
    control_pk: Option<PublicKey>,
    control_root: Option<[u8; 32]>,
    at_ms: u64,
}

/// Walk a scope's rotations forward, one epoch at a time, off the key held.
#[allow(clippy::too_many_arguments)]
async fn walk(
    scope: RekeyScope,
    permissions: &[u64],
    mut held_epoch: Epoch,
    mut held_key: [u8; 32],
    state: &CommunityState,
    roles: &CommunityRoles,
    signer: &UniversalSigner,
    me: PublicKey,
    rotations: &[Rotation],
    published: &BTreeMap<RotationKey, u64>,
) -> Result<Step> {
    let mut step = Step::default();
    let mut stepped: Vec<HeldKey> = Vec::new();
    let ceiling = held_epoch.0 + REKEY_LOOKAHEAD;

    loop {
        let target = Epoch(held_epoch.0 + 1);

        if target.0 > ceiling {
            break;
        }

        let candidates: Vec<&Rotation> = rotations
            .iter()
            .filter(|rotation| {
                rotation.scope == scope
                    && rotation.new_epoch == target
                    && rotation.is_complete()
                    && !state.banned.contains(&rotation.rotator)
                    && permissions
                        .iter()
                        .any(|bits| roles.is_authorized(&rotation.rotator, &state.owner, *bits))
            })
            .collect();

        if candidates.is_empty() {
            break;
        }

        let mut delivery: Option<Delivery> = None;
        let mut addressed = false;

        for rotation in candidates
            .iter()
            .filter(|rotation| rotation.continuity(held_epoch, &held_key) == Continuity::Extends)
        {
            // Who minted an epoch is proven by the rotation itself, not by a blob:
            // a rotation is only a candidate after continuity against a key we
            // hold, so its rotator minted this epoch whether or not it addressed
            // us. A member who joined on a stale bundle never held the epochs
            // between, and the snapshot that seeds them is only honored on this
            // npub's authority (CORD-02 §5).
            if scope == RekeyScope::Base {
                step.refounders.insert(rotation.rotator);
            }

            let at_ms = published
                .get(&rotation_key(rotation))
                .copied()
                .unwrap_or_default();

            let blobs = cord06::find_my_blobs(
                &rotation.blobs,
                &rotation.rotator,
                &me,
                scope,
                rotation.new_epoch,
            )
            .collect::<Vec<_>>();

            if blobs.is_empty() {
                continue;
            }

            addressed = true;

            for blob in blobs {
                let Ok(delivered) = cord06::open_blob(
                    signer,
                    &rotation.rotator,
                    scope,
                    rotation.new_epoch,
                    blob,
                    &state.id,
                )
                .await
                else {
                    continue;
                };

                let control_pk = delivered
                    .control_pk
                    .and_then(|bytes| PublicKey::from_slice(&bytes).ok());
                let raced = delivery.map(|held| held.key);

                // Racing rotations converge on the lowest new key.
                if raced.is_none_or(|raced| delivered.new_key < raced) {
                    delivery = Some(Delivery {
                        key: delivered.new_key,
                        control_pk,
                        control_root: delivered.control_root,
                        at_ms,
                    });
                } else if let Some(held) = delivery.as_mut() {
                    held.at_ms = held.at_ms.min(at_ms);
                }
            }
        }

        if let Some(delivered) = delivery {
            stepped.insert(
                0,
                HeldKey {
                    epoch: held_epoch,
                    key: held_key,
                    retired_at: Some(Timestamp::from_secs(delivered.at_ms / 1000)),
                },
            );

            held_epoch = target;
            held_key = delivered.key;

            step.adopted = Some(Adopted {
                epoch: target,
                key: delivered.key,
                control_pk: delivered.control_pk,
                control_root: delivered.control_root,
                stepped: stepped.clone(),
            });

            continue;
        }

        if addressed {
            break;
        }

        let judged: Vec<&&Rotation> = candidates
            .iter()
            .filter(|rotation| rotation.continuity(held_epoch, &held_key) != Continuity::Fork)
            .collect();

        let published_at = |rotation: &&Rotation| {
            published
                .get(&rotation_key(rotation))
                .copied()
                .unwrap_or_default()
        };

        if judged.iter().any(|rotation| {
            published_at(rotation) >= state.added_at_ms
                && permissions.iter().any(|bits| {
                    roles.can_act_on_member(&rotation.rotator, &state.owner, &me, *bits)
                })
        }) {
            step.removed_at = Some(target);
        } else if scope == RekeyScope::Base
            && judged
                .iter()
                .any(|rotation| published_at(rotation) < state.added_at_ms)
        {
            step.stranded = true;
        }

        break;
    }

    Ok(step)
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use concord::cord04::roles::CommunityRoles;
    use concord::cord06::{RekeyBlob, build_blob, build_rekey_chunks};
    use concord::derive::{
        base_rekey_group_key, channel_rekey_group_key, control_signer_group_key,
        epoch_key_commitment,
    };
    use concord::state::{ChannelKeyRef, HeldRoot};
    use nostr_memory::MemoryDatabase;

    use super::*;

    const AT_MS: u64 = 1_700_000_000_000;
    const ROOT: [u8; 32] = [0x55; 32];
    const NEW_ROOT: [u8; 32] = [0x66; 32];
    const NEWER_ROOT: [u8; 32] = [0x77; 32];
    const CHANNEL_KEY: [u8; 32] = [0x07; 32];
    const NEW_CHANNEL_KEY: [u8; 32] = [0x08; 32];

    fn client() -> Client {
        ClientBuilder::default()
            .database(MemoryDatabase::unbounded())
            .build()
    }

    fn state(owner: PublicKey, id: CommunityId, channel: ChannelId) -> CommunityState {
        CommunityState {
            id,
            name: Some("Room".to_owned()),
            owner,
            owner_salt: [0x01; 32],
            community_root: ROOT,
            root_epoch: Epoch(0),
            control_root: None,
            control_pks: BTreeMap::from([(0, owner)]),
            channels: vec![ChannelKeyRef {
                id: channel,
                name: "staff".to_owned(),
                private: true,
                epoch: Epoch(0),
                key: Some(CHANNEL_KEY),
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
            added_at_ms: AT_MS,
        }
    }

    async fn store(client: &Client, wraps: &[Event]) {
        for wrap in wraps {
            client.database().save_event(wrap).await.expect("saves");
        }
    }

    fn base_chunks(
        owner: &Keys,
        id: &CommunityId,
        prior_commit: &[u8; 32],
        blobs: &[RekeyBlob],
    ) -> Vec<Event> {
        let group = base_rekey_group_key(&ROOT, id, Epoch(1)).expect("derives");

        smol::block_on(build_rekey_chunks(
            owner,
            &group,
            RekeyScope::Base,
            Epoch(1),
            Epoch(0),
            prior_commit,
            blobs,
            None,
            false,
            AT_MS / 1000,
        ))
        .expect("builds")
    }

    fn channel_chunks(
        owner: &Keys,
        channel: &ChannelId,
        prior_commit: &[u8; 32],
        blobs: &[RekeyBlob],
    ) -> Vec<Event> {
        let scope = RekeyScope::Channel(*channel);
        let group = channel_rekey_group_key(&ROOT, channel, Epoch(1)).expect("derives");

        smol::block_on(build_rekey_chunks(
            owner,
            &group,
            scope,
            Epoch(1),
            Epoch(0),
            prior_commit,
            blobs,
            None,
            false,
            AT_MS / 1000,
        ))
        .expect("builds")
    }

    fn blob_for(
        rotator: &Keys,
        recipient: &Keys,
        scope: RekeyScope,
        key: [u8; 32],
        control_pk: Option<&[u8; 32]>,
    ) -> RekeyBlob {
        smol::block_on(build_blob(
            rotator,
            &recipient.public_key(),
            scope,
            Epoch(1),
            &key,
            control_pk,
            None,
        ))
        .expect("builds")
    }

    #[test]
    fn a_complete_base_rotation_is_adopted_and_retires_the_prior_root() {
        smol::block_on(async {
            let client = client();
            let owner = Keys::generate();
            let me = Keys::generate();
            let id = CommunityId::from_bytes([0x42; 32]);
            let channel = ChannelId::from_bytes([0x9c; 32]);
            let state = state(owner.public_key(), id, channel);

            let control_root = [0xAB; 32];
            let control_pk = control_signer_group_key(&control_root, &id, Epoch(1))
                .expect("derives")
                .pk()
                .to_bytes();

            let blob = blob_for(&owner, &me, RekeyScope::Base, NEW_ROOT, Some(&control_pk));
            let wraps = base_chunks(&owner, &id, &epoch_key_commitment(Epoch(0), &ROOT), &[blob]);
            store(&client, &wraps).await;

            let signer = UniversalSigner::new(me.clone());
            let roles = CommunityRoles::default();
            let adoptions = adopt(&client, &state, &roles, &signer, me.public_key())
                .await
                .expect("reads");

            let base = adoptions.base.expect("adopted");
            assert_eq!(base.epoch, Epoch(1));
            assert_eq!(base.key, NEW_ROOT);
            assert_eq!(base.control_pk, PublicKey::from_slice(&control_pk).ok());
            assert_eq!(base.stepped.len(), 1);
            assert_eq!(base.stepped[0].epoch, Epoch(0));
            assert_eq!(base.stepped[0].key, ROOT);
            assert_eq!(
                base.stepped[0].retired_at,
                Some(Timestamp::from_secs(AT_MS / 1000))
            );
            assert!(adoptions.removed_at.is_none());
            assert!(!adoptions.stranded);
        });
    }

    /// A complete base rotation off a retained root is adopted from it, which
    /// is the only way its epoch's refounder is ever learned: a Guestbook
    /// snapshot seeds members on that npub's authority alone (CORD-02 §5).
    #[test]
    fn a_rotation_past_a_retained_root_is_adopted_and_names_its_refounder() {
        smol::block_on(async {
            let client = client();
            let owner = Keys::generate();
            let me = Keys::generate();
            let id = CommunityId::from_bytes([0x42; 32]);
            let channel = ChannelId::from_bytes([0x9c; 32]);

            // At epoch 1, with the root it stepped off retained.
            let mut state = state(owner.public_key(), id, channel);
            state.root_epoch = Epoch(1);
            state.community_root = NEW_ROOT;
            state.held_roots = vec![HeldRoot {
                epoch: Epoch(0),
                key: ROOT,
                control_pk: None,
                retired_at: None,
            }];

            let blob = smol::block_on(build_blob(
                &owner,
                &me.public_key(),
                RekeyScope::Base,
                Epoch(2),
                &NEWER_ROOT,
                None,
                None,
            ))
            .expect("builds");
            let group = base_rekey_group_key(&NEW_ROOT, &id, Epoch(2)).expect("derives");
            let wraps = smol::block_on(build_rekey_chunks(
                &owner,
                &group,
                RekeyScope::Base,
                Epoch(2),
                Epoch(1),
                &epoch_key_commitment(Epoch(1), &NEW_ROOT),
                &[blob],
                None,
                false,
                AT_MS / 1000,
            ))
            .expect("builds");
            store(&client, &wraps).await;

            let signer = UniversalSigner::new(me.clone());
            let adoptions = adopt(
                &client,
                &state,
                &CommunityRoles::default(),
                &signer,
                me.public_key(),
            )
            .await
            .expect("reads");

            let base = adoptions.base.expect("adopted");
            assert_eq!(base.epoch, Epoch(2));
            assert_eq!(base.key, NEWER_ROOT);
            assert_eq!(base.stepped.len(), 1, "the root it stepped off");
            assert_eq!(base.stepped[0].epoch, Epoch(1));
            assert!(adoptions.refounders.contains(&owner.public_key()));
        });
    }

    /// A rotation that addresses somebody else still names the npub who minted the
    /// epoch: continuity against a key we hold is what proves it. That authority is
    /// what a Guestbook snapshot is honored on, so a member who joined on a stale
    /// bundle can still read the roster the Refounding seeded.
    #[test]
    fn a_rotation_that_delivered_us_no_key_still_names_its_refounder() {
        smol::block_on(async {
            let client = client();
            let owner = Keys::generate();
            let me = Keys::generate();
            let other = Keys::generate();
            let id = CommunityId::from_bytes([0x42; 32]);
            let channel = ChannelId::from_bytes([0x9c; 32]);

            // At epoch 2, holding the root the rotation stepped off but never
            // having walked it: the shape a stale join bundle produces.
            let mut state = state(owner.public_key(), id, channel);
            state.root_epoch = Epoch(2);
            state.community_root = NEWER_ROOT;
            state.held_roots = vec![HeldRoot {
                epoch: Epoch(1),
                key: NEW_ROOT,
                control_pk: None,
                retired_at: None,
            }];

            let blob = smol::block_on(build_blob(
                &owner,
                &other.public_key(),
                RekeyScope::Base,
                Epoch(2),
                &NEWER_ROOT,
                None,
                None,
            ))
            .expect("builds");
            let group = base_rekey_group_key(&NEW_ROOT, &id, Epoch(2)).expect("derives");
            let wraps = smol::block_on(build_rekey_chunks(
                &owner,
                &group,
                RekeyScope::Base,
                Epoch(2),
                Epoch(1),
                &epoch_key_commitment(Epoch(1), &NEW_ROOT),
                &[blob],
                None,
                false,
                AT_MS / 1000,
            ))
            .expect("builds");
            store(&client, &wraps).await;

            let signer = UniversalSigner::new(me.clone());
            let adoptions = adopt(
                &client,
                &state,
                &CommunityRoles::default(),
                &signer,
                me.public_key(),
            )
            .await
            .expect("reads");

            assert!(adoptions.base.is_none(), "nothing to adopt");
            assert!(!adoptions.is_empty(), "the minter is still learned");
            assert!(adoptions.refounders.contains(&owner.public_key()));
        });
    }

    /// A rotation off a key we do not hold is a fork: adoptable by nobody here.
    #[test]
    fn a_rotation_that_does_not_extend_the_held_key_is_never_adopted() {
        smol::block_on(async {
            let client = client();
            let owner = Keys::generate();
            let me = Keys::generate();
            let id = CommunityId::from_bytes([0x42; 32]);
            let channel = ChannelId::from_bytes([0x9c; 32]);
            let state = state(owner.public_key(), id, channel);

            let blob = blob_for(&owner, &me, RekeyScope::Base, NEW_ROOT, None);
            let wraps = base_chunks(
                &owner,
                &id,
                &epoch_key_commitment(Epoch(0), &[0x99; 32]),
                &[blob],
            );
            store(&client, &wraps).await;

            let signer = UniversalSigner::new(me.clone());
            let roles = CommunityRoles::default();
            let adoptions = adopt(&client, &state, &roles, &signer, me.public_key())
                .await
                .expect("reads");

            assert!(adoptions.is_empty());
        });
    }

    /// Every chunk held, none carrying my blob, from a rotator who outranks me
    /// and published after I joined: I was excluded, not stranded.
    #[test]
    fn a_blobless_rotation_from_an_outranking_rotator_removes_the_member() {
        smol::block_on(async {
            let client = client();
            let owner = Keys::generate();
            let me = Keys::generate();
            let other = Keys::generate();
            let id = CommunityId::from_bytes([0x42; 32]);
            let channel = ChannelId::from_bytes([0x9c; 32]);
            let state = state(owner.public_key(), id, channel);

            let blob = blob_for(&owner, &other, RekeyScope::Base, NEW_ROOT, None);
            let wraps = base_chunks(&owner, &id, &epoch_key_commitment(Epoch(0), &ROOT), &[blob]);
            store(&client, &wraps).await;

            let signer = UniversalSigner::new(me.clone());
            let roles = CommunityRoles::default();
            let adoptions = adopt(&client, &state, &roles, &signer, me.public_key())
                .await
                .expect("reads");

            assert!(adoptions.base.is_none());
            assert_eq!(adoptions.removed_at, Some(Epoch(1)));
            assert!(!adoptions.stranded);
        });
    }

    #[test]
    fn a_channel_rotation_replaces_the_key_and_keeps_the_prior() {
        smol::block_on(async {
            let client = client();
            let owner = Keys::generate();
            let me = Keys::generate();
            let id = CommunityId::from_bytes([0x42; 32]);
            let channel = ChannelId::from_bytes([0x9c; 32]);
            let state = state(owner.public_key(), id, channel);

            let blob = blob_for(
                &owner,
                &me,
                RekeyScope::Channel(channel),
                NEW_CHANNEL_KEY,
                None,
            );
            let wraps = channel_chunks(
                &owner,
                &channel,
                &epoch_key_commitment(Epoch(0), &CHANNEL_KEY),
                &[blob],
            );
            store(&client, &wraps).await;

            let signer = UniversalSigner::new(me.clone());
            let roles = CommunityRoles::default();
            let adoptions = adopt(&client, &state, &roles, &signer, me.public_key())
                .await
                .expect("reads");

            assert_eq!(adoptions.channels.len(), 1);
            let adopted = &adoptions.channels[0];
            assert_eq!(adopted.channel, channel);
            assert_eq!(adopted.epoch, Epoch(1));
            assert_eq!(adopted.key, NEW_CHANNEL_KEY);
            assert_eq!(adopted.stepped.len(), 1);
            assert_eq!(adopted.stepped[0].epoch, Epoch(0));
            assert_eq!(adopted.stepped[0].key, CHANNEL_KEY);
            assert_eq!(
                adopted.stepped[0].retired_at,
                Some(Timestamp::from_secs(AT_MS / 1000))
            );
            assert!(adoptions.cuts.is_empty());
        });
    }
}
