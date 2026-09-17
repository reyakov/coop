use std::collections::{BTreeMap, BTreeSet};
use std::fmt;

use anyhow::Result;
use data_encoding::HEXLOWER;
use nostr::nips::nip44::v2::ConversationKey;
use nostr_sdk::prelude::{Event, Keys, PublicKey, SecretKey, Tag, Timestamp, UnsignedEvent};
use serde::{Deserialize, Serialize};

use crate::control::CommunityIdentity;
use crate::derive::{
    base_rekey_group_key, channel_rekey_group_key, control_group_key, control_signer_group_key,
    dissolved_group_key, epoch_key_commitment, recipient_locator,
};
use crate::edition::{
    AuthorityCitation, KIND_CONTROL, TAG_SUBKIND, canonical_decimal, citation_from, citation_tag,
    vsk,
};
use crate::roles::CommunityRoles;
use crate::stream::{self, KIND_SEAL_PLAINTEXT, OpenedStream, SealForm, StreamError};
use crate::{ChannelId, CommunityId, Epoch, GroupKey, random_32};

pub const KIND_REKEY: u16 = 3303;
pub const MAX_REKEY_BLOBS_PER_EVENT: usize = 80;
pub const MAX_REKEY_BLOBS_RECEIVED: usize = 120;
pub const MAX_REKEY_EPOCH: u64 = 1 << 40;

const TAG_SCOPE: &str = "scope";
const TAG_NEW_EPOCH: &str = "newepoch";
const TAG_PREV_EPOCH: &str = "prevepoch";
const TAG_PREV_COMMIT: &str = "prevcommit";
const TAG_CHUNK: &str = "chunk";
const TAG_SEVER: &str = "sever";
const TAG_EID: &str = "eid";

const CHANNEL_BLOB_LEN: usize = 72;
const MEMBER_BASE_BLOB_LEN: usize = 104;
const STAFF_BASE_BLOB_LEN: usize = 136;

#[derive(Debug)]
pub enum RekeyError {
    Stream(StreamError),
    Crypto(String),
    Json(String),
    BadBlobLength(usize),
    BadBaseBlobWidth(usize),
    ControlPairMismatch,
    MisplacedControlKey,
    ScopeSplice,
    EpochSplice,
    NotARekey(u16),
    NotADissolution,
    BadTag(&'static str),
    NonMonotonicEpoch,
    EpochTooLarge(u64),
    BadChunkIndex,
    TooManyBlobs(usize),
}

impl fmt::Display for RekeyError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            RekeyError::Stream(error) => write!(f, "stream: {error}"),
            RekeyError::Crypto(error) => write!(f, "crypto: {error}"),
            RekeyError::Json(error) => write!(f, "json: {error}"),
            RekeyError::BadBlobLength(len) => {
                write!(f, "rekey blob plaintext is {len} bytes, expected 72")
            }
            RekeyError::BadBaseBlobWidth(len) => write!(
                f,
                "base rekey blob plaintext is {len} bytes, expected 72, 104 or 136"
            ),
            RekeyError::ControlPairMismatch => {
                write!(
                    f,
                    "base rekey blob control_root does not derive to its control_pk"
                )
            }
            RekeyError::MisplacedControlKey => {
                write!(f, "rekey blob carries a control key at the wrong width")
            }
            RekeyError::ScopeSplice => write!(f, "rekey blob scope does not match its coordinate"),
            RekeyError::EpochSplice => write!(f, "rekey blob epoch does not match its coordinate"),
            RekeyError::NotARekey(kind) => write!(f, "kind {kind} is not a rekey"),
            RekeyError::NotADissolution => write!(f, "not a dissolution tombstone"),
            RekeyError::BadTag(name) => write!(f, "missing, repeated or malformed tag: {name}"),
            RekeyError::NonMonotonicEpoch => write!(f, "a rotation must advance the epoch"),
            RekeyError::EpochTooLarge(epoch) => write!(f, "epoch {epoch} out of range"),
            RekeyError::BadChunkIndex => write!(f, "rekey chunk index out of range"),
            RekeyError::TooManyBlobs(count) => {
                write!(f, "rekey carries {count} blobs, over the cap")
            }
        }
    }
}

impl std::error::Error for RekeyError {}

impl From<StreamError> for RekeyError {
    fn from(error: StreamError) -> Self {
        RekeyError::Stream(error)
    }
}

/// What a rotation rotates: one private channel, or the whole community base.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RekeyScope {
    Channel(ChannelId),
    Base,
}

impl RekeyScope {
    /// The all-zero sentinel addresses the base; a channel id is random.
    pub fn id32(self) -> [u8; 32] {
        match self {
            RekeyScope::Channel(channel) => *channel.as_bytes(),
            RekeyScope::Base => [0u8; 32],
        }
    }

    fn to_hex(self) -> String {
        HEXLOWER.encode(&self.id32())
    }

    fn from_hex(raw: &str) -> Option<Self> {
        let bytes = crate::decode_hex_32(raw).ok()?;

        if bytes == [0u8; 32] {
            return Some(RekeyScope::Base);
        }

        Some(RekeyScope::Channel(ChannelId::from_bytes(bytes)))
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RekeyBlob {
    pub locator: String,
    pub wrapped: String,
}

/// The plaintext a blob delivered. The width declared which fields ride.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct KeyDelivery {
    pub new_key: [u8; 32],
    pub control_pk: Option<[u8; 32]>,
    pub control_root: Option<[u8; 32]>,
}

pub fn encode_blob_plaintext(
    scope: RekeyScope,
    epoch: Epoch,
    new_key: &[u8; 32],
    control_pk: Option<&[u8; 32]>,
    control_root: Option<&[u8; 32]>,
) -> Result<Vec<u8>, RekeyError> {
    if control_root.is_some() && control_pk.is_none() {
        return Err(RekeyError::MisplacedControlKey);
    }

    let mut bytes = Vec::with_capacity(STAFF_BASE_BLOB_LEN);
    bytes.extend_from_slice(&scope.id32());
    bytes.extend_from_slice(&epoch.0.to_be_bytes());
    bytes.extend_from_slice(new_key);

    match (scope, control_pk) {
        (RekeyScope::Base, Some(control_pk)) => {
            bytes.extend_from_slice(control_pk);

            if let Some(control_root) = control_root {
                bytes.extend_from_slice(control_root);
            }
        }
        (RekeyScope::Base | RekeyScope::Channel(_), None) => {}
        (RekeyScope::Channel(_), Some(_)) => return Err(RekeyError::MisplacedControlKey),
    }

    Ok(bytes)
}

pub fn parse_blob_plaintext(
    bytes: &[u8],
    scope: RekeyScope,
    epoch: Epoch,
    community_id: &CommunityId,
) -> Result<KeyDelivery, RekeyError> {
    if bytes.len() < CHANNEL_BLOB_LEN {
        return Err(RekeyError::BadBlobLength(bytes.len()));
    }

    if bytes[..32] != scope.id32() {
        return Err(RekeyError::ScopeSplice);
    }

    let mut epoch_be = [0u8; 8];
    epoch_be.copy_from_slice(&bytes[32..40]);

    if u64::from_be_bytes(epoch_be) != epoch.0 {
        return Err(RekeyError::EpochSplice);
    }

    let mut new_key = [0u8; 32];
    new_key.copy_from_slice(&bytes[40..CHANNEL_BLOB_LEN]);

    if let RekeyScope::Channel(_) = scope {
        if bytes.len() != CHANNEL_BLOB_LEN {
            return Err(RekeyError::BadBlobLength(bytes.len()));
        }

        return Ok(KeyDelivery {
            new_key,
            control_pk: None,
            control_root: None,
        });
    }

    let width = bytes.len();

    if width == CHANNEL_BLOB_LEN {
        return Ok(KeyDelivery {
            new_key,
            control_pk: None,
            control_root: None,
        });
    }

    // Between the frozen forms is malformed; wider is a future form, kept below.
    if (CHANNEL_BLOB_LEN + 1..MEMBER_BASE_BLOB_LEN).contains(&width)
        || (MEMBER_BASE_BLOB_LEN + 1..STAFF_BASE_BLOB_LEN).contains(&width)
    {
        return Err(RekeyError::BadBaseBlobWidth(width));
    }

    let mut control_pk = [0u8; 32];
    control_pk.copy_from_slice(&bytes[CHANNEL_BLOB_LEN..MEMBER_BASE_BLOB_LEN]);

    if width == MEMBER_BASE_BLOB_LEN {
        return Ok(KeyDelivery {
            new_key,
            control_pk: Some(control_pk),
            control_root: None,
        });
    }

    let mut control_root = [0u8; 32];
    control_root.copy_from_slice(&bytes[MEMBER_BASE_BLOB_LEN..STAFF_BASE_BLOB_LEN]);

    if control_signer_group_key(&control_root, community_id, epoch)
        .map_err(crypto_error)?
        .pk()
        .to_bytes()
        != control_pk
    {
        if width == STAFF_BASE_BLOB_LEN {
            return Err(RekeyError::ControlPairMismatch);
        }

        return Ok(KeyDelivery {
            new_key,
            control_pk: Some(control_pk),
            control_root: None,
        });
    }

    Ok(KeyDelivery {
        new_key,
        control_pk: Some(control_pk),
        control_root: Some(control_root),
    })
}

/// The rekey plane's address for a scope.
pub fn rekey_group(
    scope: RekeyScope,
    addressing_root: &[u8; 32],
    community_id: &CommunityId,
    new_epoch: Epoch,
) -> Result<GroupKey> {
    match scope {
        RekeyScope::Channel(channel) => {
            channel_rekey_group_key(addressing_root, &channel, new_epoch)
        }
        RekeyScope::Base => base_rekey_group_key(addressing_root, community_id, new_epoch),
    }
}

pub fn blob_locator(
    rotator: &PublicKey,
    recipient: &PublicKey,
    scope: RekeyScope,
    epoch: Epoch,
) -> String {
    HEXLOWER.encode(&recipient_locator(
        &rotator.to_bytes(),
        &recipient.to_bytes(),
        &scope.id32(),
        epoch,
    ))
}

pub fn build_blob(
    rotator: &Keys,
    recipient: &PublicKey,
    scope: RekeyScope,
    epoch: Epoch,
    new_key: &[u8; 32],
    control_pk: Option<&[u8; 32]>,
    control_root: Option<&[u8; 32]>,
) -> Result<RekeyBlob, RekeyError> {
    let plaintext = encode_blob_plaintext(scope, epoch, new_key, control_pk, control_root)?;

    Ok(RekeyBlob {
        locator: blob_locator(&rotator.public_key(), recipient, scope, epoch),
        wrapped: seal_to(rotator.secret_key(), recipient, &plaintext)?,
    })
}

pub fn open_blob(
    recipient: &Keys,
    rotator: &PublicKey,
    scope: RekeyScope,
    epoch: Epoch,
    blob: &RekeyBlob,
    community_id: &CommunityId,
) -> Result<KeyDelivery, RekeyError> {
    let conversation =
        ConversationKey::derive(recipient.secret_key(), rotator).map_err(crypto_error)?;
    let plaintext = stream::open_bytes(&conversation, &blob.wrapped)?;

    parse_blob_plaintext(&plaintext, scope, epoch, community_id)
}

pub fn find_my_blobs<'a>(
    blobs: &'a [RekeyBlob],
    rotator: &PublicKey,
    me: &PublicKey,
    scope: RekeyScope,
    epoch: Epoch,
) -> impl Iterator<Item = &'a RekeyBlob> {
    let wanted = blob_locator(rotator, me, scope, epoch);
    blobs.iter().filter(move |blob| blob.locator == wanted)
}

fn seal_to(
    secret: &SecretKey,
    recipient: &PublicKey,
    plaintext: &[u8],
) -> Result<String, RekeyError> {
    let conversation = ConversationKey::derive(secret, recipient).map_err(crypto_error)?;
    Ok(stream::seal_bytes(&conversation, plaintext)?)
}

#[derive(Debug, Clone)]
pub struct RekeyChunk {
    pub rotator: PublicKey,
    pub scope: RekeyScope,
    pub new_epoch: Epoch,
    pub prev_epoch: Epoch,
    pub prev_commit: [u8; 32],
    pub chunk: (u32, u32),
    pub blobs: Vec<RekeyBlob>,
    pub citation: Option<AuthorityCitation>,
    pub severed: bool,
}

/// The key that groups the chunks of one rotation.
pub type RotationKey = ([u8; 32], [u8; 32], u64, [u8; 32]);

impl RekeyChunk {
    pub fn correlation(&self) -> RotationKey {
        (
            self.rotator.to_bytes(),
            self.scope.id32(),
            self.new_epoch.0,
            self.prev_commit,
        )
    }
}

#[derive(Debug, Clone)]
pub struct Rotation {
    pub rotator: PublicKey,
    pub scope: RekeyScope,
    pub new_epoch: Epoch,
    pub prev_epoch: Epoch,
    pub prev_commit: [u8; 32],
    pub blobs: Vec<RekeyBlob>,
    pub declared: u32,
    pub held: BTreeSet<u32>,
    pub severed: bool,
    pub citation: Option<AuthorityCitation>,
}

impl Rotation {
    /// Every declared index held. A missing chunk is never a removal.
    pub fn is_complete(&self) -> bool {
        self.declared >= 1 && (1..=self.declared).all(|index| self.held.contains(&index))
    }

    pub fn continuity(&self, held_epoch: Epoch, held_key: &[u8; 32]) -> Continuity {
        continuity(self.prev_epoch, &self.prev_commit, held_epoch, held_key)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Continuity {
    Extends,
    Gap,
    Fork,
}

pub fn collect_rotations(chunks: &[RekeyChunk]) -> Vec<Rotation> {
    let mut by_key: BTreeMap<RotationKey, Rotation> = BTreeMap::new();

    for chunk in chunks {
        let rotation = by_key
            .entry(chunk.correlation())
            .or_insert_with(|| Rotation {
                rotator: chunk.rotator,
                scope: chunk.scope,
                new_epoch: chunk.new_epoch,
                prev_epoch: chunk.prev_epoch,
                prev_commit: chunk.prev_commit,
                blobs: Vec::new(),
                declared: chunk.chunk.1,
                held: BTreeSet::new(),
                severed: chunk.severed,
                citation: chunk.citation,
            });

        rotation.severed |= chunk.severed;
        rotation.held.insert(chunk.chunk.0);

        for blob in &chunk.blobs {
            if !rotation.blobs.iter().any(|held| held == blob) {
                rotation.blobs.push(blob.clone());
            }
        }
    }

    by_key.into_values().collect()
}

/// `None` until every chunk is held: an incomplete set is never a removal.
pub fn am_i_removed(rotation: &Rotation, me: &PublicKey) -> Option<bool> {
    if !rotation.is_complete() {
        return None;
    }

    Some(
        find_my_blobs(
            &rotation.blobs,
            &rotation.rotator,
            me,
            rotation.scope,
            rotation.new_epoch,
        )
        .next()
        .is_none(),
    )
}

fn continuity(
    prev_epoch: Epoch,
    prev_commit: &[u8; 32],
    held_epoch: Epoch,
    held_key: &[u8; 32],
) -> Continuity {
    if prev_epoch.0 == held_epoch.0 {
        return if epoch_key_commitment(held_epoch, held_key) == *prev_commit {
            Continuity::Extends
        } else {
            Continuity::Fork
        };
    }

    if prev_epoch.0 > held_epoch.0 {
        Continuity::Gap
    } else {
        Continuity::Fork
    }
}

pub fn fork_winner(held: Option<&[u8; 32]>, candidates: &[[u8; 32]]) -> Option<usize> {
    let (index, winner) = candidates.iter().enumerate().min_by_key(|(_, key)| **key)?;

    match held {
        Some(held) if winner >= held => None,
        _ => Some(index),
    }
}

pub fn rekey_authorized(
    roles: &CommunityRoles,
    owner: &PublicKey,
    rotator: &PublicKey,
    permission: u64,
    removed: &[PublicKey],
) -> bool {
    roles.is_authorized(rotator, owner, permission)
        && removed
            .iter()
            .all(|target| roles.can_act_on_member(rotator, owner, target, permission))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Refounding {
    pub epoch: Epoch,
    pub new_root: [u8; 32],
    pub new_control_root: [u8; 32],
}

impl Refounding {
    pub fn read(&self, community_id: &CommunityId) -> Result<GroupKey> {
        control_group_key(&self.new_root, community_id, self.epoch)
    }

    pub fn signer(&self, community_id: &CommunityId) -> Result<GroupKey> {
        control_signer_group_key(&self.new_control_root, community_id, self.epoch)
    }
}

pub fn plan_refounding(epoch: Epoch) -> Result<Refounding> {
    Ok(Refounding {
        epoch,
        new_root: random_32()?,
        new_control_root: random_32()?,
    })
}

/// Carries the settled heads across a refounding.
pub fn compact(
    seals: &[Event],
    read: &GroupKey,
    signer: &GroupKey,
    at_secs: u64,
) -> Result<Vec<Event>, RekeyError> {
    let at = Timestamp::from_secs(at_secs);
    let mut wraps = Vec::with_capacity(seals.len());

    for seal in seals {
        wraps.push(stream::rewrap_seal(seal, read, signer, at)?.0);
    }

    Ok(wraps)
}

#[allow(clippy::too_many_arguments)]
pub fn build_rekey_rumor(
    rotator: PublicKey,
    scope: RekeyScope,
    new_epoch: Epoch,
    prev_epoch: Epoch,
    prev_commit: &[u8; 32],
    blobs: &[RekeyBlob],
    chunk: (u32, u32),
    citation: Option<&AuthorityCitation>,
    severed: bool,
    at_secs: u64,
) -> Result<UnsignedEvent, RekeyError> {
    if new_epoch.0 <= prev_epoch.0 {
        return Err(RekeyError::NonMonotonicEpoch);
    }

    if new_epoch.0 > MAX_REKEY_EPOCH {
        return Err(RekeyError::EpochTooLarge(new_epoch.0));
    }

    if chunk.1 < 1 || chunk.0 < 1 || chunk.0 > chunk.1 {
        return Err(RekeyError::BadChunkIndex);
    }

    if blobs.len() > MAX_REKEY_BLOBS_PER_EVENT {
        return Err(RekeyError::TooManyBlobs(blobs.len()));
    }

    let content = serde_json::to_string(blobs).map_err(json_error)?;

    let mut tags = vec![
        Tag::custom(TAG_SCOPE, [scope.to_hex()]),
        Tag::custom(TAG_NEW_EPOCH, [new_epoch.0.to_string()]),
        Tag::custom(TAG_PREV_EPOCH, [prev_epoch.0.to_string()]),
        Tag::custom(TAG_PREV_COMMIT, [HEXLOWER.encode(prev_commit)]),
        Tag::custom(TAG_CHUNK, [chunk.0.to_string(), chunk.1.to_string()]),
    ];

    if let Some(citation) = citation {
        tags.push(citation_tag(citation));
    }

    if severed {
        tags.push(Tag::custom(TAG_SEVER, ["1"]));
    }

    Ok(stream::build_rumor_secs(
        KIND_REKEY, rotator, &content, tags, at_secs,
    ))
}

#[allow(clippy::too_many_arguments)]
pub fn build_rekey_chunks(
    rotator: &Keys,
    group: &GroupKey,
    scope: RekeyScope,
    new_epoch: Epoch,
    prev_epoch: Epoch,
    prev_commit: &[u8; 32],
    blobs: &[RekeyBlob],
    citation: Option<&AuthorityCitation>,
    severed: bool,
    at_secs: u64,
) -> Result<Vec<Event>, RekeyError> {
    let mut groups: Vec<&[RekeyBlob]> = blobs.chunks(MAX_REKEY_BLOBS_PER_EVENT).collect();

    if groups.is_empty() {
        groups.push(&[]);
    }

    let total = groups.len() as u32;
    let mut chunks = Vec::with_capacity(groups.len());

    for (index, group_blobs) in groups.into_iter().enumerate() {
        let rumor = build_rekey_rumor(
            rotator.public_key(),
            scope,
            new_epoch,
            prev_epoch,
            prev_commit,
            group_blobs,
            (index as u32 + 1, total),
            citation,
            severed,
            at_secs,
        )?;

        let seal = stream::build_seal(&rumor, SealForm::Encrypted, group, rotator)?;
        let (wrap, _) = stream::wrap_seal(
            &seal,
            group,
            stream::KIND_WRAP,
            Timestamp::from_secs(at_secs),
            &[],
        )?;

        chunks.push(wrap);
    }

    Ok(chunks)
}

pub fn parse_rekey_chunk(opened: &OpenedStream) -> Result<RekeyChunk, RekeyError> {
    if opened.seal_form != SealForm::Encrypted {
        // A plaintext-sealed rekey would be a public artifact anyone could lift.
        return Err(RekeyError::Stream(StreamError::BadSealKind(
            KIND_SEAL_PLAINTEXT,
        )));
    }

    let rumor = &opened.rumor;

    if rumor.kind.as_u16() != KIND_REKEY {
        return Err(RekeyError::NotARekey(rumor.kind.as_u16()));
    }

    let scope = tag(rumor, TAG_SCOPE)?
        .and_then(|fields| fields.get(1))
        .and_then(|raw| RekeyScope::from_hex(raw))
        .ok_or(RekeyError::BadTag(TAG_SCOPE))?;

    let new_epoch = Epoch(decimal(rumor, TAG_NEW_EPOCH)?);
    let prev_epoch = Epoch(decimal(rumor, TAG_PREV_EPOCH)?);

    if new_epoch.0 <= prev_epoch.0 {
        return Err(RekeyError::NonMonotonicEpoch);
    }

    if new_epoch.0 > MAX_REKEY_EPOCH {
        return Err(RekeyError::EpochTooLarge(new_epoch.0));
    }

    let prev_commit = tag(rumor, TAG_PREV_COMMIT)?
        .and_then(|fields| fields.get(1))
        .and_then(|raw| crate::decode_hex_32(raw).ok())
        .ok_or(RekeyError::BadTag(TAG_PREV_COMMIT))?;

    let blobs: Vec<RekeyBlob> =
        serde_json::from_str(&rumor.content).map_err(|_| RekeyError::BadTag("blobs"))?;

    if blobs.len() > MAX_REKEY_BLOBS_RECEIVED {
        return Err(RekeyError::TooManyBlobs(blobs.len()));
    }

    let severed = match tag(rumor, TAG_SEVER)? {
        None => false,
        Some(fields) if fields.get(1).map(String::as_str) == Some("1") => true,
        Some(_) => return Err(RekeyError::BadTag(TAG_SEVER)),
    };

    Ok(RekeyChunk {
        rotator: opened.author,
        scope,
        new_epoch,
        prev_epoch,
        prev_commit,
        chunk: parse_chunk(rumor)?,
        blobs,
        citation: tag(rumor, crate::edition::TAG_CITATION)?.and_then(citation_from),
        severed,
    })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DissolvedTombstone {
    pub owner: PublicKey,
}

pub fn dissolved_tombstone_rumor(
    owner: PublicKey,
    community_id: &CommunityId,
    at_secs: u64,
) -> UnsignedEvent {
    stream::build_rumor_secs(
        KIND_CONTROL,
        owner,
        "",
        vec![
            Tag::custom(TAG_SUBKIND, [vsk::DISSOLVED]),
            Tag::custom(TAG_EID, [HEXLOWER.encode(community_id.as_bytes())]),
        ],
        at_secs,
    )
}

pub fn seal_dissolved(
    rumor: &UnsignedEvent,
    community_id: &CommunityId,
    owner: &Keys,
    at_secs: u64,
) -> Result<Event, RekeyError> {
    let group = dissolved_group_key(community_id).map_err(crypto_error)?;
    let seal = stream::build_seal(rumor, SealForm::Plaintext, &group, owner)?;
    let (wrap, _) = stream::wrap_seal(
        &seal,
        &group,
        stream::KIND_WRAP,
        Timestamp::from_secs(at_secs),
        &[],
    )?;

    Ok(wrap)
}

/// Proves the seal signature and the tombstone shape, but not the owner.
pub fn open_dissolved(
    wrap: &Event,
    community_id: &CommunityId,
) -> Result<DissolvedTombstone, RekeyError> {
    let group = dissolved_group_key(community_id).map_err(crypto_error)?;
    let opened = stream::open_wrap(wrap, &group)?;

    if !is_tombstone(&opened.rumor, community_id) {
        return Err(RekeyError::NotADissolution);
    }

    Ok(DissolvedTombstone {
        owner: opened.author,
    })
}

/// Fail-closed: an unverifiable or foreign-signed tombstone is not death.
pub fn verify_dissolved(wrap: &Event, identity: &CommunityIdentity) -> bool {
    if !identity.verify() {
        return false;
    }

    matches!(
        open_dissolved(wrap, &identity.community_id),
        Ok(tombstone) if tombstone.owner == identity.owner
    )
}

fn is_tombstone(rumor: &UnsignedEvent, community_id: &CommunityId) -> bool {
    let eid = HEXLOWER.encode(community_id.as_bytes());

    rumor.kind.as_u16() == KIND_CONTROL
        && value(rumor, TAG_SUBKIND) == Some(vsk::DISSOLVED)
        && value(rumor, TAG_EID) == Some(eid.as_str())
}

fn value<'a>(rumor: &'a UnsignedEvent, name: &'static str) -> Option<&'a str> {
    tag(rumor, name)
        .ok()
        .flatten()
        .and_then(|fields| fields.get(1))
        .map(String::as_str)
}

fn tag<'a>(
    rumor: &'a UnsignedEvent,
    name: &'static str,
) -> Result<Option<&'a [String]>, RekeyError> {
    let mut found: Option<&[String]> = None;

    for candidate in rumor.tags.iter() {
        let fields = candidate.as_slice();

        if fields.first().map(String::as_str) != Some(name) {
            continue;
        }

        if found.is_some() {
            return Err(RekeyError::BadTag(name));
        }

        found = Some(fields);
    }

    Ok(found)
}

fn decimal(rumor: &UnsignedEvent, name: &'static str) -> Result<u64, RekeyError> {
    tag(rumor, name)?
        .and_then(|fields| fields.get(1))
        .and_then(|raw| canonical_decimal(raw))
        .ok_or(RekeyError::BadTag(name))
}

fn parse_chunk(rumor: &UnsignedEvent) -> Result<(u32, u32), RekeyError> {
    let fields = tag(rumor, TAG_CHUNK)?.ok_or(RekeyError::BadTag(TAG_CHUNK))?;

    let index = as_u32(fields.get(1), TAG_CHUNK)?;
    let total = as_u32(fields.get(2), TAG_CHUNK)?;

    if total < 1 || index < 1 || index > total {
        return Err(RekeyError::BadChunkIndex);
    }

    Ok((index, total))
}

fn as_u32(raw: Option<&String>, name: &'static str) -> Result<u32, RekeyError> {
    let value = raw
        .and_then(|raw| canonical_decimal(raw))
        .ok_or(RekeyError::BadTag(name))?;

    u32::try_from(value).map_err(|_| RekeyError::BadTag(name))
}

fn json_error(error: serde_json::Error) -> RekeyError {
    RekeyError::Json(error.to_string())
}

fn crypto_error(error: impl fmt::Display) -> RekeyError {
    RekeyError::Crypto(error.to_string())
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use super::*;
    use crate::control::{
        CommunityMetadata, ControlWriter, Edition, ROOT_EPOCH, fold_control, genesis, open_edition,
    };
    use crate::derive::{community_id_of, grant_locator};
    use crate::edition::{EditionFields, Floors, build_edition};
    use crate::roles::{Grant, Permissions, Role, RoleScope};
    use crate::stream::KIND_WRAP;
    use crate::{Extra, RoleId};

    const AT: u64 = 1_700_000_000;
    const ROOT: [u8; 32] = [0x55; 32];
    const PRIOR_KEY: [u8; 32] = [0xEE; 32];

    fn channel() -> ChannelId {
        ChannelId::from_bytes([0x42; 32])
    }

    fn community() -> CommunityId {
        CommunityId::from_bytes([0x77; 32])
    }

    fn chunk_at(
        rotator: &Keys,
        scope: RekeyScope,
        new_epoch: u64,
        prev_epoch: u64,
        prev_key: &[u8; 32],
        blobs: Vec<RekeyBlob>,
        chunk: (u32, u32),
    ) -> RekeyChunk {
        RekeyChunk {
            rotator: rotator.public_key(),
            scope,
            new_epoch: Epoch(new_epoch),
            prev_epoch: Epoch(prev_epoch),
            prev_commit: epoch_key_commitment(Epoch(prev_epoch), prev_key),
            chunk,
            blobs,
            citation: None,
            severed: false,
        }
    }

    #[test]
    fn a_blob_binds_its_scope_and_the_width_declares_the_base_form() {
        let rotator = Keys::generate();
        let recipient = Keys::generate();
        let community_id = community();
        let epoch = Epoch(3);
        let key = [0xABu8; 32];
        let scope = RekeyScope::Channel(channel());

        let open = |keys: &Keys, scope: RekeyScope, epoch: Epoch, blob: &RekeyBlob| {
            open_blob(
                keys,
                &rotator.public_key(),
                scope,
                epoch,
                blob,
                &community_id,
            )
        };
        let blob = build_blob(
            &rotator,
            &recipient.public_key(),
            scope,
            epoch,
            &key,
            None,
            None,
        )
        .expect("builds");

        assert_eq!(
            blob.locator,
            blob_locator(&rotator.public_key(), &recipient.public_key(), scope, epoch)
        );

        let delivery = open(&recipient, scope, epoch, &blob).expect("opens");
        assert_eq!(delivery.new_key, key);
        assert_eq!(delivery.control_pk, None);

        // The locator is a public lookup index, so an outsider computes it and
        // still cannot open: the pairwise decrypt is the gate.
        let outsider = Keys::generate();
        assert!(open(&outsider, scope, epoch, &blob).is_err());

        assert!(matches!(
            open(&recipient, RekeyScope::Base, epoch, &blob),
            Err(RekeyError::ScopeSplice)
        ));
        assert!(matches!(
            open(&recipient, scope, Epoch(4), &blob),
            Err(RekeyError::EpochSplice)
        ));

        let control_root = [0x5Cu8; 32];
        let control_pk = control_signer_group_key(&control_root, &community_id, epoch)
            .expect("derives")
            .pk()
            .to_bytes();

        let base = |pk: Option<&[u8; 32]>, root: Option<&[u8; 32]>| {
            build_blob(
                &rotator,
                &recipient.public_key(),
                RekeyScope::Base,
                epoch,
                &key,
                pk,
                root,
            )
            .expect("builds")
        };

        for (pk, root, expected_pk, expected_root) in [
            (None, None, None, None),
            (Some(&control_pk), None, Some(control_pk), None),
            (
                Some(&control_pk),
                Some(&control_root),
                Some(control_pk),
                Some(control_root),
            ),
        ] {
            let delivery =
                open(&recipient, RekeyScope::Base, epoch, &base(pk, root)).expect("opens");

            assert_eq!(delivery.new_key, key);
            assert_eq!(delivery.control_pk, expected_pk);
            assert_eq!(delivery.control_root, expected_root);
        }

        // A staff secret that does not derive to the pk beside it refuses the whole
        // blob, rather than adopting a control plane split from its readers.
        let forged = encode_blob_plaintext(
            RekeyScope::Base,
            epoch,
            &key,
            Some(&control_pk),
            Some(&[0x11; 32]),
        )
        .expect("encodes");
        assert!(matches!(
            parse_blob_plaintext(&forged, RekeyScope::Base, epoch, &community_id),
            Err(RekeyError::ControlPairMismatch)
        ));

        // A width between the defined forms fits no append-only extension.
        let staff = encode_blob_plaintext(
            RekeyScope::Base,
            epoch,
            &key,
            Some(&control_pk),
            Some(&control_root),
        )
        .expect("encodes");

        for width in [73usize, 105, 135] {
            let mut bytes = staff.clone();
            bytes.truncate(width);

            assert!(matches!(
                parse_blob_plaintext(&bytes, RekeyScope::Base, epoch, &community_id),
                Err(RekeyError::BadBaseBlobWidth(len)) if len == width
            ));
        }

        assert!(matches!(
            encode_blob_plaintext(RekeyScope::Base, epoch, &key, None, Some(&control_root)),
            Err(RekeyError::MisplacedControlKey)
        ));
    }

    #[test]
    fn a_rekey_round_trips_and_only_a_complete_rotation_concludes_a_removal() {
        let rotator = Keys::generate();
        let me = Keys::generate();
        let other = Keys::generate();
        let scope = RekeyScope::Channel(channel());
        let epoch = Epoch(1);
        let community_id = community();

        let blob_for = |recipient: &Keys, key: [u8; 32]| {
            build_blob(
                &rotator,
                &recipient.public_key(),
                scope,
                epoch,
                &key,
                None,
                None,
            )
            .expect("builds")
        };
        let mine = blob_for(&me, [0xAA; 32]);
        let theirs = blob_for(&other, [0xBB; 32]);

        let group = rekey_group(scope, &ROOT, &community_id, epoch).expect("derives");
        let prior_commit = epoch_key_commitment(Epoch(0), &PRIOR_KEY);
        let chunks = build_rekey_chunks(
            &rotator,
            &group,
            scope,
            epoch,
            Epoch(0),
            &prior_commit,
            &[mine.clone(), theirs.clone()],
            None,
            false,
            AT,
        )
        .expect("builds");
        assert_eq!(chunks.len(), 1);

        let opened = stream::open_wrap(&chunks[0], &group).expect("opens");
        let chunk = parse_rekey_chunk(&opened).expect("parses");
        assert_eq!(
            chunk.rotator,
            rotator.public_key(),
            "the seal names the rotator"
        );
        assert_eq!(chunk.scope, scope);
        assert_eq!((chunk.new_epoch, chunk.prev_epoch), (epoch, Epoch(0)));
        assert_eq!(chunk.prev_commit, prior_commit);
        assert_eq!(
            chunk.prev_commit,
            epoch_key_commitment(Epoch(0), &PRIOR_KEY)
        );
        assert_eq!(chunk.chunk, (1, 1));
        assert_eq!(chunk.blobs, vec![mine.clone(), theirs.clone()]);

        // One of two chunks held, and it lacks my blob: not answerable yet.
        let first = chunk_at(
            &rotator,
            scope,
            1,
            0,
            &PRIOR_KEY,
            vec![theirs.clone()],
            (1, 2),
        );
        let rotations = collect_rotations(std::slice::from_ref(&first));
        assert!(!rotations[0].is_complete());
        assert_eq!(am_i_removed(&rotations[0], &me.public_key()), None);

        // The second arrives with my blob: complete, and I am retained.
        let second = chunk_at(
            &rotator,
            scope,
            1,
            0,
            &PRIOR_KEY,
            vec![mine.clone()],
            (2, 2),
        );
        let rotations = collect_rotations(&[first, second]);
        assert!(rotations[0].is_complete());
        assert_eq!(am_i_removed(&rotations[0], &me.public_key()), Some(false));

        let located = find_my_blobs(
            &rotations[0].blobs,
            &rotator.public_key(),
            &me.public_key(),
            scope,
            epoch,
        )
        .next()
        .expect("located");
        assert_eq!(
            open_blob(
                &me,
                &rotator.public_key(),
                scope,
                epoch,
                located,
                &community_id
            )
            .expect("opens")
            .new_key,
            [0xAA; 32]
        );

        // A complete rotation carrying only someone else's blob is a removal.
        let alone = chunk_at(
            &rotator,
            scope,
            1,
            0,
            &PRIOR_KEY,
            vec![theirs.clone()],
            (1, 1),
        );
        assert_eq!(
            am_i_removed(&collect_rotations(&[alone])[0], &me.public_key()),
            Some(true)
        );

        // Two chunks claiming one index union their blobs. Dropping the loser's
        // blobs would delete its recipients from the union, and a recipient with no
        // blob reads as removed.
        let reclaim = chunk_at(&rotator, scope, 1, 0, &PRIOR_KEY, vec![theirs], (1, 1));
        let original = chunk_at(&rotator, scope, 1, 0, &PRIOR_KEY, vec![mine], (1, 1));

        for order in [
            vec![original.clone(), reclaim.clone()],
            vec![reclaim, original],
        ] {
            let rotations = collect_rotations(&order);
            assert_eq!(rotations.len(), 1);
            assert_eq!(
                am_i_removed(&rotations[0], &me.public_key()),
                Some(false),
                "an index collision must never fabricate a removal"
            );
        }
    }

    #[test]
    fn a_severed_rotation_is_not_laundered_by_an_unmarked_sibling() {
        let rotator = Keys::generate();
        let scope = RekeyScope::Base;
        let mut marked = chunk_at(&rotator, scope, 2, 1, &PRIOR_KEY, vec![], (1, 2));
        marked.severed = true;
        let unmarked = chunk_at(&rotator, scope, 2, 1, &PRIOR_KEY, vec![], (2, 2));

        for order in [
            vec![marked.clone(), unmarked.clone()],
            vec![unmarked, marked],
        ] {
            let rotations = collect_rotations(&order);
            assert_eq!(rotations.len(), 1);
            assert!(rotations[0].severed);
            assert!(rotations[0].is_complete());
        }

        // Two rotators racing one epoch, and one rotator over two scopes, never merge.
        let other = Keys::generate();
        let rotations = collect_rotations(&[
            chunk_at(&rotator, RekeyScope::Base, 2, 1, &PRIOR_KEY, vec![], (1, 1)),
            chunk_at(&other, RekeyScope::Base, 2, 1, &PRIOR_KEY, vec![], (1, 1)),
            chunk_at(
                &rotator,
                RekeyScope::Channel(channel()),
                2,
                1,
                &PRIOR_KEY,
                vec![],
                (1, 1),
            ),
        ]);
        assert_eq!(rotations.len(), 3);
    }

    #[test]
    fn continuity_gaps_and_forks_and_the_winner_heals_only_downward() {
        let rotator = Keys::generate();
        let held = [0x33u8; 32];

        let extends = chunk_at(&rotator, RekeyScope::Base, 3, 2, &held, vec![], (1, 1));
        assert_eq!(
            collect_rotations(&[extends])[0].continuity(Epoch(2), &held),
            Continuity::Extends
        );

        let ahead = chunk_at(&rotator, RekeyScope::Base, 5, 4, &held, vec![], (1, 1));
        assert_eq!(
            collect_rotations(&[ahead])[0].continuity(Epoch(2), &held),
            Continuity::Gap
        );

        // The same epoch with a different prior key, and a rotation older than where
        // I am, are both forks.
        let forked = chunk_at(
            &rotator,
            RekeyScope::Base,
            3,
            2,
            &[0x99; 32],
            vec![],
            (1, 1),
        );
        assert_eq!(
            collect_rotations(&[forked])[0].continuity(Epoch(2), &held),
            Continuity::Fork
        );

        let stale = chunk_at(&rotator, RekeyScope::Base, 2, 1, &held, vec![], (1, 1));
        assert_eq!(
            collect_rotations(&[stale])[0].continuity(Epoch(2), &held),
            Continuity::Fork
        );

        // The lowest key wins, and only when it strictly lowers a key already held,
        // so a settled epoch re-converges down and never re-forks upward.
        let candidates = [[0x03u8; 32], [0x01u8; 32], [0x02u8; 32]];
        assert_eq!(fork_winner(None, &candidates), Some(1));
        assert_eq!(fork_winner(Some(&[0x05u8; 32]), &candidates), Some(1));
        assert_eq!(fork_winner(Some(&[0x00u8; 32]), &candidates), None);
        assert_eq!(fork_winner(None, &[]), None);
    }

    fn publish_authority(
        writer: &ControlWriter,
        owner: &Keys,
        subkind: &'static str,
        entity: [u8; 32],
        content: String,
        at_secs: u64,
    ) -> Event {
        writer
            .publish(
                owner,
                Edition {
                    subkind,
                    entity,
                    content: &content,
                    head: None,
                    citation: None,
                },
                at_secs,
            )
            .expect("publishes")
            .0
    }

    #[test]
    fn a_rotation_needs_the_permission_and_must_strictly_outrank_every_target() {
        let owner = Keys::generate();
        let minted = genesis(&owner, &CommunityMetadata::default(), AT).expect("mints");
        let community_id = minted.identity.community_id;
        let read =
            control_group_key(&minted.community_root, &community_id, ROOT_EPOCH).expect("derives");
        let signer = control_signer_group_key(&minted.control_root, &community_id, ROOT_EPOCH)
            .expect("derives");
        let writer = ControlWriter {
            author: owner.public_key(),
            read: read.clone(),
            signer: signer.clone(),
        };

        let senior = Keys::generate();
        let junior = Keys::generate();
        let target = Keys::generate();
        let mut wraps = Vec::new();

        for (index, position, member, permissions) in [
            (
                0u64,
                1u32,
                &senior,
                Permissions::MANAGE_CHANNELS | Permissions::BAN,
            ),
            (1, 3, &junior, Permissions::MANAGE_CHANNELS),
        ] {
            let role_id = RoleId::from_bytes([index as u8 + 1; 32]);
            let role = Role {
                role_id,
                name: "role".to_owned(),
                position,
                permissions: Permissions(permissions),
                scope: RoleScope::Server,
                color: 0,
                extra: Extra::default(),
            };
            let grant = Grant {
                member: member.public_key(),
                role_ids: vec![role_id],
                control_wrap: None,
                extra: Extra::default(),
            };

            wraps.push(publish_authority(
                &writer,
                &owner,
                vsk::ROLE,
                *role_id.as_bytes(),
                role.to_content().expect("serializes"),
                AT + index,
            ));
            wraps.push(publish_authority(
                &writer,
                &owner,
                vsk::GRANT,
                grant_locator(&community_id, &member.public_key().to_bytes()),
                grant.to_content().expect("serializes"),
                AT + 10 + index,
            ));
        }

        let editions: Vec<_> = minted
            .wraps
            .iter()
            .chain(wraps.iter())
            .map(|wrap| open_edition(wrap, &read, &signer.pk(), true).expect("opens"))
            .collect();

        let roles = fold_control(
            &owner.public_key(),
            &community_id,
            &editions,
            &Floors::new(),
            &BTreeSet::new(),
        )
        .roles;
        let owner_pk = owner.public_key();
        let authorized = |rotator: &PublicKey, permission: u64, removed: &[PublicKey]| {
            rekey_authorized(&roles, &owner_pk, rotator, permission, removed)
        };
        let targets = [target.public_key()];

        // The owner needs no grant.
        assert!(authorized(&owner_pk, Permissions::BAN, &targets));

        // The senior may refound; the junior holds only MANAGE_CHANNELS.
        assert!(authorized(&senior.public_key(), Permissions::BAN, &targets));
        assert!(authorized(
            &junior.public_key(),
            Permissions::MANAGE_CHANNELS,
            &targets
        ));
        assert!(!authorized(
            &junior.public_key(),
            Permissions::BAN,
            &targets
        ));

        // Strictly outrank: equal or above cannot rotate the other out.
        assert!(!authorized(
            &junior.public_key(),
            Permissions::MANAGE_CHANNELS,
            &[senior.public_key()]
        ));

        // Holding a key is never authority.
        assert!(!authorized(
            &target.public_key(),
            Permissions::MANAGE_CHANNELS,
            &[]
        ));
    }

    #[test]
    fn a_full_send_chunk_stays_within_a_relay_event() {
        let rotator = Keys::generate();
        let community_id = community();
        let epoch = Epoch(1);
        let scope = RekeyScope::Base;
        let group = rekey_group(scope, &ROOT, &community_id, epoch).expect("derives");

        let blobs: Vec<RekeyBlob> = (0..MAX_REKEY_BLOBS_PER_EVENT)
            .map(|_| {
                let member = Keys::generate();

                build_blob(
                    &rotator,
                    &member.public_key(),
                    scope,
                    epoch,
                    &[0xCD; 32],
                    None,
                    None,
                )
                .expect("builds")
            })
            .collect();

        let chunks = build_rekey_chunks(
            &rotator,
            &group,
            scope,
            epoch,
            Epoch(0),
            &PRIOR_KEY,
            &blobs,
            None,
            false,
            AT,
        )
        .expect("builds");

        assert_eq!(chunks.len(), 1, "a full send chunk is one event");
        assert!(
            chunks[0].as_json().len() <= 65_536,
            "a full chunk must fit a 64 KB relay event"
        );

        let mut over = blobs;
        over.push(RekeyBlob {
            locator: "aa".repeat(32),
            wrapped: "x".to_owned(),
        });

        let chunks = build_rekey_chunks(
            &rotator,
            &group,
            scope,
            epoch,
            Epoch(0),
            &PRIOR_KEY,
            &over,
            None,
            false,
            AT,
        )
        .expect("builds");

        assert_eq!(chunks.len(), 2, "one over the cap splits across two events");
    }

    #[test]
    fn compaction_carries_a_settled_head_with_its_signature_intact() {
        let owner = Keys::generate();
        let community_id = community();
        let prior_read = control_group_key(&[0x01; 32], &community_id, Epoch(0)).expect("derives");

        let rumor = build_edition(EditionFields {
            author: owner.public_key(),
            subkind: vsk::COMMUNITY_METADATA,
            entity: *community_id.as_bytes(),
            version: 1,
            prev: None,
            citation: None,
            content: "{}",
            at_secs: AT,
        });
        let seal =
            stream::build_seal(&rumor, SealForm::Plaintext, &prior_read, &owner).expect("seals");

        let refounding = plan_refounding(Epoch(1)).expect("plans");
        let read = refounding.read(&community_id).expect("derives");
        let signer = refounding.signer(&community_id).expect("derives");
        assert_ne!(refounding.new_root, refounding.new_control_root);

        let compacted =
            compact(std::slice::from_ref(&seal), &read, &signer, AT + 1).expect("compacts");
        assert_eq!(compacted.len(), 1);

        let reopened = stream::open_wrap_at(&compacted[0], &signer.pk(), read.conversation(), true)
            .expect("opens");
        assert_eq!(
            reopened.seal.sig, seal.sig,
            "the refounder re-signs nothing: the original author's signature rides the new wrap"
        );
        assert_eq!(reopened.rumor_id, rumor.id.expect("has an id"));
        assert_eq!(reopened.author, owner.public_key());

        // Only a plaintext seal can be carried forward.
        let encrypted =
            stream::build_seal(&rumor, SealForm::Encrypted, &prior_read, &owner).expect("seals");
        assert!(matches!(
            compact(&[encrypted], &read, &signer, AT + 1),
            Err(RekeyError::Stream(StreamError::NotRewrappable))
        ));
    }

    #[test]
    fn a_foreign_eid_tombstone_cannot_seal_a_community() {
        let owner = Keys::generate();
        let salt = [0x33u8; 32];
        let community_id = community_id_of(&owner.public_key().to_bytes(), &salt);
        let identity = CommunityIdentity {
            community_id,
            owner: owner.public_key(),
            owner_salt: salt,
        };

        let rumor = dissolved_tombstone_rumor(owner.public_key(), &community_id, AT);
        let wrap = seal_dissolved(&rumor, &community_id, &owner, AT).expect("seals");

        assert!(verify_dissolved(&wrap, &identity));
        assert_eq!(
            open_dissolved(&wrap, &community_id).expect("opens").owner,
            owner.public_key()
        );

        // Anyone holding the community id finds the address, but only the committed
        // owner's signature counts.
        let impostor = Keys::generate();
        let forged = seal_dissolved(
            &dissolved_tombstone_rumor(impostor.public_key(), &community_id, AT),
            &community_id,
            &impostor,
            AT,
        )
        .expect("seals");
        assert!(!verify_dissolved(&forged, &identity));

        // The spec's all-zero `eid` is refused: it would let one owner's genuine
        // tombstone be re-wrapped at another of their communities and kill it.
        let zeroed = seal_dissolved(
            &stream::build_rumor_secs(
                KIND_CONTROL,
                owner.public_key(),
                "",
                vec![
                    Tag::custom(TAG_SUBKIND, [vsk::DISSOLVED]),
                    Tag::custom(TAG_EID, ["00".repeat(32)]),
                ],
                AT,
            ),
            &community_id,
            &owner,
            AT,
        )
        .expect("seals");
        assert!(matches!(
            open_dissolved(&zeroed, &community_id),
            Err(RekeyError::NotADissolution)
        ));
        assert!(!verify_dissolved(&zeroed, &identity));

        // One owner, two communities: lifting X's seal to Y's public address keeps
        // the signed `eid` naming X, so Y is not sealed.
        let other_salt = [0x44u8; 32];
        let other_id = community_id_of(&owner.public_key().to_bytes(), &other_salt);
        let other_identity = CommunityIdentity {
            community_id: other_id,
            owner: owner.public_key(),
            owner_salt: other_salt,
        };
        let seal = stream::open_wrap(&wrap, &dissolved_group_key(&community_id).expect("derives"))
            .expect("opens")
            .seal;
        let replayed = stream::wrap_seal(
            &seal,
            &dissolved_group_key(&other_id).expect("derives"),
            KIND_WRAP,
            Timestamp::from_secs(AT + 1),
            &[],
        )
        .expect("rewraps")
        .0;

        assert!(matches!(
            open_dissolved(&replayed, &other_id),
            Err(RekeyError::NotADissolution)
        ));
        assert!(!verify_dissolved(&replayed, &other_identity));
    }
}
