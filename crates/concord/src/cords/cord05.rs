use std::collections::BTreeMap;
use std::collections::btree_map::Entry;
use std::fmt;

use data_encoding::BASE64URL_NOPAD;
use nostr::nips::nip01::Coordinate;
use nostr::nips::nip19::{Nip19, Nip19Coordinate};
use nostr::nips::nip44::v2::ConversationKey;
use nostr::nips::nip59::{GiftWrapBuilder, UnwrappedGift};
use nostr_sdk::prelude::*;
use serde::{Deserialize, Serialize};

use crate::cord01::{self, NIP44_MAX_PLAINTEXT, StreamError};
use crate::cord02::{ImageRef, MAX_RELAYS};
use crate::cord04::{TAG_SUBKIND, vsk};
use crate::derive::{TOKEN_LEN, verify_community_id};
use crate::utils::{canonical, union};
use crate::{ChannelId, CommunityId, Epoch, Extra, decode_hex_32};

pub const KIND_BUNDLE: u16 = 33301;
pub const KIND_INVITE_LIST: u16 = 13303;
pub const KIND_DIRECT_INVITE: u16 = 3313;
pub const FRAGMENT_VERSION: u8 = 4;
pub const MAX_BUNDLE_CHANNELS: usize = 256;
pub const MAX_BOOTSTRAP_RELAYS: usize = 3;
pub const MAX_BUNDLE_EPOCH: u64 = 1 << 40;
pub const MAX_INVITE_ENTRIES: usize = 64;

const FLAG_STOCK_SET: u8 = 0x01;
const INVITE_PATH: &str = "/invite/";
const TAG_IDENTIFIER: &str = "d";
const TAG_EXPIRATION: &str = "expiration";

const RELAY_DICT: [&str; 4] = [
    "wss://jskitty.com/nostr",
    "wss://asia.vectorapp.io/nostr",
    "wss://relay.ditto.pub",
    "wss://relay.dreamith.to",
];

#[derive(Debug)]
pub enum InviteError {
    Stream(StreamError),
    Json(String),
    BadHex(&'static str),
    TooManyChannels(usize),
    TooManyInvites(usize),
    Oversize(usize),
    Kind(u16),
    EpochTooLarge(u64),
    OwnerMismatch,
    BadFragment(&'static str),
    BadVersion(u8),
    BadLink(&'static str),
    BadEvent(&'static str),
    Crypto(String),
}

impl fmt::Display for InviteError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            InviteError::Stream(error) => write!(f, "stream: {error}"),
            InviteError::Json(error) => write!(f, "json: {error}"),
            InviteError::BadHex(field) => write!(f, "{field} is not 32-byte lowercase hex"),
            InviteError::TooManyChannels(count) => {
                write!(
                    f,
                    "bundle carries {count} channels (cap {MAX_BUNDLE_CHANNELS})"
                )
            }
            InviteError::TooManyInvites(count) => {
                write!(
                    f,
                    "invite list carries {count} entries (cap {MAX_INVITE_ENTRIES})"
                )
            }
            InviteError::Oversize(len) => {
                write!(f, "invite list is {len} bytes (cap {NIP44_MAX_PLAINTEXT})")
            }
            InviteError::Kind(kind) => write!(f, "not an invite list kind: {kind}"),
            InviteError::EpochTooLarge(epoch) => write!(f, "epoch {epoch} out of range"),
            InviteError::OwnerMismatch => {
                write!(f, "bundle owner does not reproduce its community_id")
            }
            InviteError::BadFragment(why) => write!(f, "bad invite fragment: {why}"),
            InviteError::BadVersion(version) => {
                write!(f, "unsupported invite fragment version {version}")
            }
            InviteError::BadLink(why) => write!(f, "bad invite link: {why}"),
            InviteError::BadEvent(why) => write!(f, "bad invite bundle event: {why}"),
            InviteError::Crypto(error) => write!(f, "crypto: {error}"),
        }
    }
}

impl std::error::Error for InviteError {}

impl From<StreamError> for InviteError {
    fn from(error: StreamError) -> Self {
        InviteError::Stream(error)
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ChannelGrant {
    pub id: ChannelId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub key: Option<String>,
    pub epoch: Epoch,
    #[serde(default)]
    pub name: String,
    #[serde(flatten)]
    pub extra: Extra,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CommunityInvite {
    pub community_id: CommunityId,
    pub owner: PublicKey,
    pub owner_salt: String,
    pub community_root: String,
    pub root_epoch: Epoch,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub control_pk: Option<PublicKey>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub channels: Vec<ChannelGrant>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub relays: Vec<String>,
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub icon: Option<ImageRef>,
    /// Unix **ms**: past it the preview still renders, joining refuses.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expires_at: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub creator_npub: Option<PublicKey>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    #[serde(flatten)]
    pub extra: Extra,
}

impl CommunityInvite {
    pub fn from_bundle_json(json: &str) -> Result<Self, InviteError> {
        let mut invite: Self =
            serde_json::from_str(json).map_err(|error| InviteError::Json(error.to_string()))?;

        if invite.channels.len() > MAX_BUNDLE_CHANNELS {
            return Err(InviteError::TooManyChannels(invite.channels.len()));
        }

        invite.relays.truncate(MAX_RELAYS);
        invite.validate()?;

        Ok(invite)
    }

    pub fn validate(&self) -> Result<(), InviteError> {
        if self.channels.len() > MAX_BUNDLE_CHANNELS {
            return Err(InviteError::TooManyChannels(self.channels.len()));
        }

        for epoch in std::iter::once(self.root_epoch).chain(self.channels.iter().map(|c| c.epoch)) {
            if epoch.0 > MAX_BUNDLE_EPOCH {
                return Err(InviteError::EpochTooLarge(epoch.0));
            }
        }

        let owner_salt = hex32(&self.owner_salt, "owner_salt")?;
        hex32(&self.community_root, "community_root")?;

        for channel in &self.channels {
            if let Some(key) = &channel.key {
                hex32(key, "channel key")?;
            }
        }

        if !verify_community_id(&self.community_id, &self.owner.to_bytes(), &owner_salt) {
            return Err(InviteError::OwnerMismatch);
        }

        Ok(())
    }

    pub fn expired(&self, now_ms: u64) -> bool {
        self.expires_at.is_some_and(|expires| now_ms > expires)
    }
}

#[derive(Debug, Clone)]
pub enum BundleState {
    Live(Box<CommunityInvite>),
    Revoked,
}

pub fn build_bundle_event(
    link_signer: &Keys,
    invite: &CommunityInvite,
    bundle_key: &[u8; 32],
) -> Result<Event, InviteError> {
    invite.validate()?;

    let json = serde_json::to_string(invite).map_err(json_error)?;
    let content = seal_bundle(bundle_key, &json)?;

    EventBuilder::new(Kind::Custom(KIND_BUNDLE), content)
        .tags([empty_identifier(), subkind_tag(vsk::INVITE_LIVE)])
        .finalize(link_signer)
        .map_err(crypto_error)
}

pub fn build_revocation(link_signer: &Keys) -> Result<Event, InviteError> {
    EventBuilder::new(Kind::Custom(KIND_BUNDLE), "")
        .tags([empty_identifier(), subkind_tag(vsk::INVITE_REVOKED)])
        .finalize(link_signer)
        .map_err(crypto_error)
}

pub fn parse_bundle_event(
    event: &Event,
    expected_signer: &PublicKey,
    bundle_key: &[u8; 32],
) -> Result<BundleState, InviteError> {
    if event.kind.as_u16() != KIND_BUNDLE {
        return Err(InviteError::BadEvent("wrong kind"));
    }

    if event.pubkey != *expected_signer {
        return Err(InviteError::BadEvent("author is not the link signer"));
    }

    if first_tag(event, TAG_IDENTIFIER).is_some_and(|identifier| !identifier.is_empty()) {
        return Err(InviteError::BadEvent(
            "bundle is not at the link's coordinate",
        ));
    }

    event
        .verify()
        .map_err(|_| InviteError::BadEvent("signature invalid"))?;

    match first_tag(event, TAG_SUBKIND).as_deref() {
        Some(vsk::INVITE_REVOKED) => return Ok(BundleState::Revoked),
        Some(vsk::INVITE_LIVE) => {}
        _ => return Err(InviteError::BadEvent("unknown or missing bundle marker")),
    }

    let json = open_bundle(bundle_key, &event.content)?;

    Ok(BundleState::Live(Box::new(
        CommunityInvite::from_bundle_json(&json)?,
    )))
}

pub fn stock_relays() -> Vec<String> {
    RELAY_DICT.iter().map(|relay| relay.to_string()).collect()
}

pub fn encode_fragment(token: &[u8; TOKEN_LEN], relays: &[String]) -> Result<String, InviteError> {
    let stock = relays == RELAY_DICT;

    let mut bytes = Vec::with_capacity(2 + TOKEN_LEN + relays.len() * 8);
    bytes.push(FRAGMENT_VERSION);

    if stock {
        bytes.push(FLAG_STOCK_SET);
    } else {
        bytes.push(0x00);

        let bounded = &relays[..relays.len().min(MAX_BOOTSTRAP_RELAYS)];
        bytes.push(bounded.len() as u8);

        for relay in bounded {
            match dict_id(relay) {
                Some(id) => bytes.push(id),
                None => {
                    let (lead, literal) = match relay.strip_prefix("wss://") {
                        Some(host) => (0x00, host),
                        None => (0xff, relay.as_str()),
                    };

                    if literal.len() > u8::MAX as usize {
                        return Err(InviteError::BadFragment("relay too long"));
                    }

                    bytes.extend_from_slice(&[lead, literal.len() as u8]);
                    bytes.extend_from_slice(literal.as_bytes());
                }
            }
        }
    }

    bytes.extend_from_slice(token);

    Ok(BASE64URL_NOPAD.encode(&bytes))
}

pub fn decode_fragment(fragment: &str) -> Result<([u8; TOKEN_LEN], Vec<String>), InviteError> {
    let bytes = BASE64URL_NOPAD
        .decode(fragment.trim().as_bytes())
        .map_err(|_| InviteError::BadFragment("not base64url"))?;

    let version = *bytes.first().ok_or(InviteError::BadFragment("truncated"))?;

    if version != FRAGMENT_VERSION {
        return Err(InviteError::BadVersion(version));
    }

    let flags = *bytes.get(1).ok_or(InviteError::BadFragment("truncated"))?;

    let mut offset = 2;
    let mut relays = Vec::new();

    if flags & FLAG_STOCK_SET != 0 {
        relays = stock_relays();
    } else {
        let count = *bytes
            .get(offset)
            .ok_or(InviteError::BadFragment("truncated"))? as usize;
        offset += 1;

        if count > MAX_BOOTSTRAP_RELAYS {
            return Err(InviteError::BadFragment("too many bootstrap relays"));
        }

        for _ in 0..count {
            let lead = *bytes
                .get(offset)
                .ok_or(InviteError::BadFragment("truncated"))?;
            offset += 1;

            if (1..=254).contains(&lead) {
                if let Some(url) = dict_url(lead) {
                    relays.push(url.to_string());
                }

                continue;
            }

            let len = *bytes
                .get(offset)
                .ok_or(InviteError::BadFragment("truncated"))? as usize;
            offset += 1;

            let end = offset
                .checked_add(len)
                .ok_or(InviteError::BadFragment("truncated"))?;

            let raw = bytes
                .get(offset..end)
                .ok_or(InviteError::BadFragment("truncated"))?;

            let text = std::str::from_utf8(raw)
                .map_err(|_| InviteError::BadFragment("relay is not utf8"))?;

            relays.push(match lead {
                0x00 => format!("wss://{text}"),
                0xff => text.to_string(),
                _ => return Err(InviteError::BadFragment("unknown relay lead byte")),
            });

            offset = end;
        }
    }

    let end = offset
        .checked_add(TOKEN_LEN)
        .ok_or(InviteError::BadFragment("truncated"))?;

    let raw = bytes
        .get(offset..end)
        .ok_or(InviteError::BadFragment("truncated"))?;

    if end != bytes.len() {
        return Err(InviteError::BadFragment("trailing bytes"));
    }

    let mut token = [0u8; TOKEN_LEN];
    token.copy_from_slice(raw);

    Ok((token, relays))
}

pub fn bundle_naddr(link_signer: &PublicKey) -> Result<String, InviteError> {
    let coordinate = Coordinate {
        kind: Kind::Custom(KIND_BUNDLE),
        public_key: *link_signer,
        identifier: String::new(),
    };

    Nip19::Coordinate(Nip19Coordinate {
        coordinate,
        relays: Vec::new(),
    })
    .to_bech32()
    .map_err(|_| InviteError::BadLink("invalid naddr"))
}

pub fn build_invite_url(
    base: &str,
    link_signer: &PublicKey,
    token: &[u8; TOKEN_LEN],
    relays: &[String],
) -> Result<String, InviteError> {
    let naddr = bundle_naddr(link_signer)?;
    let fragment = encode_fragment(token, relays)?;

    Ok(format!(
        "{}{INVITE_PATH}{naddr}#{fragment}",
        base.trim_end_matches('/')
    ))
}

#[derive(Debug, Clone)]
pub struct ParsedInviteLink {
    /// The bundle coordinate's author.
    pub link_signer: PublicKey,
    pub token: [u8; TOKEN_LEN],
    pub bootstrap_relays: Vec<String>,
    /// The bare naddr as it appeared in the link, for the fetch.
    pub naddr: String,
}

pub fn parse_link(input: &str) -> Result<ParsedInviteLink, InviteError> {
    let (locator, fragment) = input
        .trim()
        .split_once('#')
        .ok_or(InviteError::BadLink("no fragment"))?;

    if fragment.is_empty() {
        return Err(InviteError::BadLink("empty fragment"));
    }

    let naddr = match locator.find(INVITE_PATH) {
        Some(index) => locator[index + INVITE_PATH.len()..].trim_end_matches('/'),
        None => locator.trim_start_matches("nostr:"),
    };

    let link_signer = signer_from_naddr(naddr)?;
    let (token, bootstrap_relays) = decode_fragment(fragment)?;

    Ok(ParsedInviteLink {
        link_signer,
        token,
        bootstrap_relays,
        naddr: naddr.to_string(),
    })
}

pub async fn build_direct_invite<S>(
    inviter: &S,
    recipient: &PublicKey,
    invite: &CommunityInvite,
) -> Result<Event, InviteError>
where
    S: AsyncGetPublicKey + AsyncSignEvent + AsyncNip44,
{
    invite.validate()?;

    let json = serde_json::to_string(invite).map_err(json_error)?;
    let author = inviter.get_public_key_async().await.map_err(crypto_error)?;
    let rumor = EventBuilder::new(Kind::Custom(KIND_DIRECT_INVITE), json).finalize_unsigned(author);

    let mut tags = vec![Tag::custom("k", [KIND_DIRECT_INVITE.to_string()])];

    if let Some(expires_at) = invite.expires_at {
        tags.push(Tag::custom(
            TAG_EXPIRATION,
            [(expires_at / 1000).to_string()],
        ));
    }

    GiftWrapBuilder::new(*recipient, rumor)
        .extra_tags(tags)
        .finalize_async(inviter)
        .await
        .map_err(crypto_error)
}

/// The NIP-59 unwrap is `Sized`-bounded in the SDK, so this stays `Sized` too.
pub async fn unwrap_direct_invite<S>(
    wrap: &Event,
    recipient: &S,
) -> Result<(PublicKey, CommunityInvite), InviteError>
where
    S: AsyncNip44,
{
    let unwrapped = UnwrappedGift::from_gift_wrap_async(recipient, wrap)
        .await
        .map_err(crypto_error)?;

    if unwrapped.rumor.kind.as_u16() != KIND_DIRECT_INVITE {
        return Err(InviteError::BadEvent("rumor is not a direct invite"));
    }

    Ok((
        unwrapped.sender,
        CommunityInvite::from_bundle_json(&unwrapped.rumor.content)?,
    ))
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct InviteEntry {
    /// The link's unlock secret, and its merge key.
    pub token: String,
    /// The `link_signer` secret: refreshing or retiring the bundle needs it.
    pub signer_sk: String,
    pub community_id: CommunityId,
    pub url: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    pub created_at: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expires_at: Option<u64>,
    #[serde(flatten)]
    pub extra: Extra,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct InviteTombstone {
    pub token: String,
    pub community_id: CommunityId,
    #[serde(flatten)]
    pub extra: Extra,
}

/// A creator's own link bookkeeping.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct InviteList {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub entries: Vec<InviteEntry>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tombstones: Vec<InviteTombstone>,
    #[serde(flatten)]
    pub extra: Extra,
}

impl InviteList {
    /// A tombstone beats an entry terminally, so a stale device can never resurrect a revoked link.
    pub fn is_live(&self, token: &str) -> bool {
        self.entries.iter().any(|entry| entry.token == token)
            && !self
                .tombstones
                .iter()
                .any(|tombstone| tombstone.token == token)
    }

    pub fn fits(&self) -> Result<(), InviteError> {
        if self.entries.len() > MAX_INVITE_ENTRIES {
            return Err(InviteError::TooManyInvites(self.entries.len()));
        }

        let json = serde_json::to_string(self).map_err(json_error)?;

        if json.len() > NIP44_MAX_PLAINTEXT {
            return Err(InviteError::Oversize(json.len()));
        }

        Ok(())
    }
}

pub fn merge_invite_lists(held: InviteList, incoming: InviteList) -> InviteList {
    let mut entries: BTreeMap<String, InviteEntry> = BTreeMap::new();

    for entry in held.entries.into_iter().chain(incoming.entries) {
        match entries.entry(entry.token.clone()) {
            Entry::Vacant(slot) => {
                slot.insert(entry);
            }
            Entry::Occupied(mut slot) => {
                let merged = merge_entry(slot.get(), &entry);
                *slot.get_mut() = merged;
            }
        }
    }

    let mut tombstones: BTreeMap<String, InviteTombstone> = BTreeMap::new();

    for tombstone in held.tombstones.into_iter().chain(incoming.tombstones) {
        match tombstones.entry(tombstone.token.clone()) {
            Entry::Vacant(slot) => {
                slot.insert(tombstone);
            }
            Entry::Occupied(mut slot) => {
                if canonical(&tombstone) < canonical(slot.get()) {
                    *slot.get_mut() = tombstone;
                }
            }
        }
    }

    let mut extra = held.extra;
    union(&mut extra, incoming.extra);

    InviteList {
        entries: entries.into_values().collect(),
        tombstones: tombstones.into_values().collect(),
        extra,
    }
}

pub async fn build_invite_list<S>(keys: &S, list: &InviteList) -> Result<Event, InviteError>
where
    S: AsyncGetPublicKey + AsyncSignEvent + AsyncNip44 + ?Sized,
{
    list.fits()?;

    let json = serde_json::to_string(list).map_err(json_error)?;
    let content = cord01::seal_to_self(keys, &json).await?;

    EventBuilder::new(Kind::Custom(KIND_INVITE_LIST), content)
        .finalize_async(keys)
        .await
        .map_err(crypto_error)
}

pub async fn parse_invite_list<S>(keys: &S, event: &Event) -> Result<InviteList, InviteError>
where
    S: AsyncGetPublicKey + AsyncNip44 + ?Sized,
{
    if event.kind.as_u16() != KIND_INVITE_LIST {
        return Err(InviteError::Kind(event.kind.as_u16()));
    }

    let json = cord01::open_to_self(keys, &event.content).await?;

    serde_json::from_str(&json).map_err(json_error)
}

/// An entry is immutable once minted, so two copies should agree.
fn merge_entry(held: &InviteEntry, incoming: &InviteEntry) -> InviteEntry {
    let (winner, loser) = if canonical(incoming) < canonical(held) {
        (incoming, held)
    } else {
        (held, incoming)
    };

    let mut merged = winner.clone();
    union(&mut merged.extra, loser.extra.clone());

    merged
}

fn seal_bundle(bundle_key: &[u8; 32], json: &str) -> Result<String, InviteError> {
    Ok(cord01::seal_bytes(
        &ConversationKey::new(*bundle_key),
        json.as_bytes(),
    )?)
}

fn open_bundle(bundle_key: &[u8; 32], content: &str) -> Result<String, InviteError> {
    let plaintext = cord01::open_bytes(&ConversationKey::new(*bundle_key), content)?;

    String::from_utf8(plaintext).map_err(|_| InviteError::BadFragment("bundle is not utf8"))
}

fn signer_from_naddr(naddr: &str) -> Result<PublicKey, InviteError> {
    match Nip19::from_bech32(naddr.trim_start_matches("nostr:")) {
        Ok(Nip19::Coordinate(coordinate))
            if coordinate.coordinate.kind.as_u16() == KIND_BUNDLE
                && coordinate.coordinate.identifier.is_empty() =>
        {
            Ok(coordinate.coordinate.public_key)
        }
        _ => Err(InviteError::BadLink(
            "naddr is not an invite-bundle coordinate",
        )),
    }
}

fn hex32(value: &str, field: &'static str) -> Result<[u8; 32], InviteError> {
    decode_hex_32(value).map_err(|_| InviteError::BadHex(field))
}

fn dict_id(relay: &str) -> Option<u8> {
    RELAY_DICT
        .iter()
        .position(|known| *known == relay)
        .map(|index| index as u8 + 1)
}

fn dict_url(id: u8) -> Option<&'static str> {
    RELAY_DICT.get(id.checked_sub(1)? as usize).copied()
}

fn empty_identifier() -> Tag {
    Tag::identifier("")
}

fn subkind_tag(value: &str) -> Tag {
    Tag::custom(TAG_SUBKIND, [value])
}

fn first_tag(event: &Event, name: &str) -> Option<String> {
    event.tags.iter().find_map(|tag| {
        let fields = tag.as_slice();

        (fields.len() >= 2 && fields[0] == name).then(|| fields[1].clone())
    })
}

fn json_error(error: serde_json::Error) -> InviteError {
    InviteError::Json(error.to_string())
}

fn crypto_error(error: impl fmt::Display) -> InviteError {
    InviteError::Crypto(error.to_string())
}

#[cfg(test)]
mod tests {
    use data_encoding::HEXLOWER;

    use super::*;
    use crate::derive::{community_id_of, invite_bundle_key};

    const SALT: [u8; 32] = [0x33u8; 32];

    fn bundle() -> CommunityInvite {
        let owner = Keys::generate();

        CommunityInvite {
            community_id: community_id_of(&owner.public_key().to_bytes(), &SALT),
            owner: owner.public_key(),
            owner_salt: HEXLOWER.encode(&SALT),
            community_root: "44".repeat(32),
            root_epoch: Epoch(0),
            control_pk: None,
            channels: vec![ChannelGrant {
                id: ChannelId::from_bytes([0x9cu8; 32]),
                key: Some("55".repeat(32)),
                epoch: Epoch(1),
                name: "lounge".to_owned(),
                extra: Extra::default(),
            }],
            relays: vec!["wss://relay.example".to_owned()],
            name: "Test community".to_owned(),
            icon: None,
            expires_at: None,
            creator_npub: None,
            label: None,
            extra: Extra::default(),
        }
    }

    fn token16() -> [u8; TOKEN_LEN] {
        std::array::from_fn(|i| i as u8)
    }

    #[test]
    fn fragment_goldens_pin_the_wire_layout() {
        let token = token16();

        // [04 version][01 stock flag][token 00..0f]
        let stock = encode_fragment(&token, &stock_relays()).expect("encodes");
        assert_eq!(stock, "BAEAAQIDBAUGBwgJCgsMDQ4P");
        assert_eq!(
            decode_fragment(&stock).expect("decodes"),
            (token, stock_relays())
        );

        // [04][00 flags][02 count][02 dict-id][04 dict-id][token 00..0f]
        let mixed = vec![RELAY_DICT[1].to_owned(), RELAY_DICT[3].to_owned()];
        let encoded = encode_fragment(&token, &mixed).expect("encodes");
        assert_eq!(encoded, "BAACAgQAAQIDBAUGBwgJCgsMDQ4P");
        assert_eq!(decode_fragment(&encoded).expect("decodes"), (token, mixed));

        // [04][00][01 count][ff verbatim lead][06 len]["ws://h"][token 00..0f]
        let verbatim = vec!["ws://h".to_owned()];
        let encoded = encode_fragment(&token, &verbatim).expect("encodes");
        assert_eq!(encoded, "BAAB_wZ3czovL2gAAQIDBAUGBwgJCgsMDQ4P");
        assert_eq!(
            decode_fragment(&encoded).expect("decodes"),
            (token, verbatim)
        );
    }

    #[test]
    fn a_fragment_is_strict_about_framing_and_counts() {
        let token = token16();

        for version in [3u8, 5] {
            let mut bytes = vec![version, FLAG_STOCK_SET];
            bytes.extend_from_slice(&token);
            let encoded = BASE64URL_NOPAD.encode(&bytes);
            assert!(
                matches!(decode_fragment(&encoded), Err(InviteError::BadVersion(v)) if v == version),
                "a legacy and a future version are both refused"
            );
        }

        let mut trailing = vec![FRAGMENT_VERSION, FLAG_STOCK_SET];
        trailing.extend_from_slice(&token);
        trailing.push(0xff);
        assert!(matches!(
            decode_fragment(&BASE64URL_NOPAD.encode(&trailing)),
            Err(InviteError::BadFragment(_))
        ));

        let mut over = vec![FRAGMENT_VERSION, 0x00, 0x04, 1, 2, 3, 4];
        over.extend_from_slice(&token);
        assert!(matches!(
            decode_fragment(&BASE64URL_NOPAD.encode(&over)),
            Err(InviteError::BadFragment(_))
        ));

        // An unknown dictionary id is skipped, not fatal, so the dictionary can grow.
        let mut unknown = vec![FRAGMENT_VERSION, 0x00, 0x01, 200];
        unknown.extend_from_slice(&token);
        let (decoded, relays) =
            decode_fragment(&BASE64URL_NOPAD.encode(&unknown)).expect("decodes");
        assert_eq!(decoded, token);
        assert!(relays.is_empty());
    }

    #[test]
    fn a_link_round_trips_and_refuses_a_non_invite() {
        let link_signer = Keys::generate();
        let token = token16();
        let relays = vec!["wss://a.example".to_owned()];

        let url = build_invite_url(
            "https://vectorapp.io/",
            &link_signer.public_key(),
            &token,
            &relays,
        )
        .expect("builds");

        let parsed = parse_link(&url).expect("parses");
        assert_eq!(parsed.link_signer, link_signer.public_key());
        assert_eq!(parsed.token, token);
        assert_eq!(parsed.bootstrap_relays, relays);

        let fragment = url.split('#').nth(1).expect("carries a fragment");
        let bare = format!("{}#{fragment}", parsed.naddr);
        let reparsed = parse_link(&bare).expect("parses the domain-agnostic form");
        assert_eq!(reparsed.link_signer, link_signer.public_key());
        assert_eq!(reparsed.token, token);

        assert!(
            parse_link("https://x/invite/#frag").is_err(),
            "the naddr is not optional"
        );
    }

    #[test]
    fn a_bundle_round_trips_while_a_revocation_reads_as_revoked() {
        let invite = bundle();
        let link_signer = Keys::generate();
        let key = invite_bundle_key(&[7u8; TOKEN_LEN]);

        let event = build_bundle_event(&link_signer, &invite, &key).expect("builds");
        assert_eq!(event.pubkey, link_signer.public_key());

        match parse_bundle_event(&event, &link_signer.public_key(), &key).expect("parses") {
            BundleState::Live(opened) => {
                assert_eq!(opened.community_id, invite.community_id);
                assert_eq!(opened.channels.len(), 1);
            }
            BundleState::Revoked => panic!("expected a live bundle"),
        }

        let revocation = build_revocation(&link_signer).expect("builds");
        assert!(matches!(
            parse_bundle_event(&revocation, &link_signer.public_key(), &key),
            Ok(BundleState::Revoked)
        ));

        // The token is the only way in, and a squatter is a different coordinate.
        assert!(
            parse_bundle_event(
                &event,
                &link_signer.public_key(),
                &invite_bundle_key(&[8u8; TOKEN_LEN])
            )
            .is_err()
        );
        let squatter = Keys::generate();
        assert!(matches!(
            parse_bundle_event(&event, &squatter.public_key(), &key),
            Err(InviteError::BadEvent(_))
        ));
    }

    #[test]
    fn a_bundle_off_its_coordinate_or_off_its_owner_is_refused() {
        let invite = bundle();
        let link_signer = Keys::generate();
        let key = invite_bundle_key(&[9u8; TOKEN_LEN]);
        let json = serde_json::to_string(&invite).expect("serializes");
        let content = seal_bundle(&key, &json).expect("seals");

        // The fetch filters on the author, so the empty `d` is pinned here: a
        // signature-valid event of the same author at another `d` is not the bundle.
        let elsewhere = EventBuilder::new(Kind::Custom(KIND_BUNDLE), content)
            .tags([Tag::identifier("elsewhere"), subkind_tag(vsk::INVITE_LIVE)])
            .finalize(&link_signer)
            .expect("signs");
        assert!(matches!(
            parse_bundle_event(&elsewhere, &link_signer.public_key(), &key),
            Err(InviteError::BadEvent(_))
        ));

        let mut forged = bundle();
        forged.owner = Keys::generate().public_key();
        assert!(matches!(forged.validate(), Err(InviteError::OwnerMismatch)));
        assert!(matches!(
            build_bundle_event(&link_signer, &forged, &key),
            Err(InviteError::OwnerMismatch)
        ));

        let mut malformed = bundle();
        malformed.community_root = "not hex".to_owned();
        assert!(matches!(malformed.validate(), Err(InviteError::BadHex(_))));

        let mut crowded = bundle();
        crowded.channels = (0..=MAX_BUNDLE_CHANNELS)
            .map(|_| ChannelGrant {
                id: ChannelId::from_bytes([0x01; 32]),
                key: None,
                epoch: Epoch(0),
                name: String::new(),
                extra: Extra::default(),
            })
            .collect();
        assert!(matches!(
            crowded.validate(),
            Err(InviteError::TooManyChannels(n)) if n == MAX_BUNDLE_CHANNELS + 1
        ));
    }

    #[test]
    fn a_direct_invite_round_trips_and_refuses_a_foreign_rumor() {
        let inviter = Keys::generate();
        let recipient = Keys::generate();
        let invite = bundle();

        let wrap = smol::block_on(build_direct_invite(
            &inviter,
            &recipient.public_key(),
            &invite,
        ))
        .expect("builds");
        assert_eq!(wrap.kind, Kind::GiftWrap);
        assert_ne!(
            wrap.pubkey,
            inviter.public_key(),
            "the wrap author is ephemeral"
        );
        assert!(
            wrap.tags.iter().any(|tag| tag.as_slice() == ["k", "3313"]),
            "the k tag is what makes an invite indexable"
        );

        let (sender, opened) =
            smol::block_on(unwrap_direct_invite(&wrap, &recipient)).expect("unwraps");
        assert_eq!(sender, inviter.public_key());
        assert_eq!(opened.community_id, invite.community_id);

        // Somebody else's wrap is not ours to open...
        let stranger = Keys::generate();
        assert!(smol::block_on(unwrap_direct_invite(&wrap, &stranger)).is_err());

        // ...and a wrap that opens to some other kind is not an invite.
        let rumor = EventBuilder::new(Kind::Custom(crate::cord03::KIND_MESSAGE), "hello")
            .finalize_unsigned(recipient.public_key());
        let wrap = GiftWrapBuilder::new(recipient.public_key(), rumor)
            .finalize(&recipient)
            .expect("wraps");
        assert!(matches!(
            smol::block_on(unwrap_direct_invite(&wrap, &recipient)),
            Err(InviteError::BadEvent(_))
        ));
    }
}
