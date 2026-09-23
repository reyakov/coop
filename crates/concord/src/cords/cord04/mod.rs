pub mod pins;
pub mod roles;

use std::cmp::Reverse;
use std::collections::BTreeMap;
use std::fmt;

use data_encoding::HEXLOWER;
use nostr_sdk::prelude::{EventId, PublicKey, Tag, UnsignedEvent};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::cord01::build_rumor_secs;
use crate::decode_hex_32;

pub const KIND_CONTROL: u16 = 3308;

const EDITION_LABEL: &[u8] = b"vector-community/v1/edition";

/// Entity types an edition can address.
pub mod vsk {
    pub const COMMUNITY_METADATA: &str = "0";
    pub const ROLE: &str = "1";
    pub const CHANNEL_METADATA: &str = "2";
    pub const GRANT: &str = "3";
    pub const BANLIST: &str = "4";
    pub const INVITE_LIVE: &str = "6";
    pub const INVITE_LINKS: &str = "8";
    pub const INVITE_REVOKED: &str = "9";
    pub const DISSOLVED: &str = "10";
    pub const PINS: &str = "11";
}

pub const TAG_SUBKIND: &str = "vsk";
pub const TAG_CITATION: &str = "vac";

const TAG_ENTITY: &str = "eid";
const TAG_VERSION: &str = "ev";
const TAG_PREV: &str = "ep";

#[derive(Debug)]
pub enum EditionError {
    BadKind(u16),
    BadField(&'static str),
    Duplicate(&'static str),
    Missing(&'static str),
}

impl fmt::Display for EditionError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            EditionError::BadKind(kind) => write!(f, "not an edition kind: {kind}"),
            EditionError::BadField(name) => write!(f, "malformed edition field: {name}"),
            EditionError::Duplicate(name) => write!(f, "duplicate edition field: {name}"),
            EditionError::Missing(name) => write!(f, "missing edition field: {name}"),
        }
    }
}

impl std::error::Error for EditionError {}

/// A `vac`: the Grant edition an actor claims rank under, pinned by coordinate, version and hash.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AuthorityCitation {
    pub entity: [u8; 32],
    pub version: u64,
    pub hash: [u8; 32],
}

#[derive(Debug, Clone)]
pub struct ParsedEdition {
    pub author: PublicKey,
    pub subkind: String,
    pub entity: [u8; 32],
    pub version: u64,
    pub prev: Option<[u8; 32]>,
    pub citation: Option<AuthorityCitation>,
    pub content: String,
    pub self_hash: [u8; 32],
    pub rumor_id: EventId,
}

pub struct EditionFields<'a> {
    pub author: PublicKey,
    pub subkind: &'a str,
    pub entity: [u8; 32],
    pub version: u64,
    pub prev: Option<[u8; 32]>,
    pub citation: Option<AuthorityCitation>,
    pub content: &'a str,
    pub at_secs: u64,
}

fn signing_bytes(
    entity: &[u8; 32],
    version: u64,
    prev: Option<&[u8; 32]>,
    content: &[u8],
) -> Vec<u8> {
    let mut bytes =
        Vec::with_capacity(8 + EDITION_LABEL.len() + 32 + 8 + 1 + 32 + 8 + content.len());

    bytes.extend_from_slice(&(EDITION_LABEL.len() as u64).to_be_bytes());
    bytes.extend_from_slice(EDITION_LABEL);
    bytes.extend_from_slice(entity);
    bytes.extend_from_slice(&version.to_be_bytes());

    match prev {
        Some(prev) => {
            bytes.push(1);
            bytes.extend_from_slice(prev);
        }
        None => {
            bytes.push(0);
            bytes.extend_from_slice(&[0u8; 32]);
        }
    }

    bytes.extend_from_slice(&(content.len() as u64).to_be_bytes());
    bytes.extend_from_slice(content);
    bytes
}

fn edition_hash(
    entity: &[u8; 32],
    version: u64,
    prev: Option<&[u8; 32]>,
    content: &[u8],
) -> [u8; 32] {
    Sha256::digest(signing_bytes(entity, version, prev, content)).into()
}

pub fn citation_tag(citation: &AuthorityCitation) -> Tag {
    Tag::custom(
        TAG_CITATION,
        [
            HEXLOWER.encode(&citation.entity),
            citation.version.to_string(),
            HEXLOWER.encode(&citation.hash),
        ],
    )
}

pub fn citation_from(fields: &[String]) -> Option<AuthorityCitation> {
    if fields.len() != 4 {
        return None;
    }

    Some(AuthorityCitation {
        entity: hex32(&fields[1], TAG_CITATION).ok()?,
        version: canonical_decimal(&fields[2])?,
        hash: hex32(&fields[3], TAG_CITATION).ok()?,
    })
}

pub fn build_edition(fields: EditionFields<'_>) -> UnsignedEvent {
    let mut tags = vec![
        Tag::custom(TAG_SUBKIND, [fields.subkind]),
        Tag::custom(TAG_ENTITY, [HEXLOWER.encode(&fields.entity)]),
        Tag::custom(TAG_VERSION, [fields.version.to_string()]),
    ];

    if let Some(prev) = fields.prev {
        tags.push(Tag::custom(TAG_PREV, [HEXLOWER.encode(&prev)]));
    }

    if let Some(citation) = fields.citation {
        tags.push(citation_tag(&citation));
    }

    build_rumor_secs(
        KIND_CONTROL,
        fields.author,
        fields.content,
        tags,
        fields.at_secs,
    )
}

pub fn parse_edition(rumor: &UnsignedEvent) -> Result<ParsedEdition, EditionError> {
    let kind = rumor.kind.as_u16();

    if kind != KIND_CONTROL {
        return Err(EditionError::BadKind(kind));
    }

    let subkind = value(rumor, TAG_SUBKIND)?
        .ok_or(EditionError::Missing(TAG_SUBKIND))?
        .to_owned();

    if canonical_decimal(&subkind).is_none() {
        return Err(EditionError::BadField(TAG_SUBKIND));
    }

    let entity = hex32(
        value(rumor, TAG_ENTITY)?.ok_or(EditionError::Missing(TAG_ENTITY))?,
        TAG_ENTITY,
    )?;

    let version =
        canonical_decimal(value(rumor, TAG_VERSION)?.ok_or(EditionError::Missing(TAG_VERSION))?)
            .ok_or(EditionError::BadField(TAG_VERSION))?;

    let prev = match value(rumor, TAG_PREV)? {
        Some(raw) => Some(hex32(raw, TAG_PREV)?),
        None => None,
    };

    let citation = match fields(rumor, TAG_CITATION)? {
        Some(fields) => Some(citation_from(fields).ok_or(EditionError::BadField(TAG_CITATION))?),
        None => None,
    };

    let self_hash = edition_hash(&entity, version, prev.as_ref(), rumor.content.as_bytes());

    Ok(ParsedEdition {
        author: rumor.pubkey,
        subkind,
        entity,
        version,
        prev,
        citation,
        content: rumor.content.clone(),
        self_hash,
        rumor_id: rumor.id.unwrap_or_else(|| rumor.compute_id()),
    })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EditionMeta {
    pub version: u64,
    pub self_hash: [u8; 32],
    pub prev: Option<[u8; 32]>,
    pub tiebreak_id: EventId,
}

impl From<&ParsedEdition> for EditionMeta {
    fn from(edition: &ParsedEdition) -> Self {
        Self {
            version: edition.version,
            self_hash: edition.self_hash,
            prev: edition.prev,
            tiebreak_id: edition.rumor_id,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
struct FoldResult {
    pub head: Option<usize>,
    pub gap: bool,
    pub anchored: bool,
}

/// The highest version whose chain is intact, given a held floor.
fn fold(editions: &[EditionMeta], floor: u64, floor_hash: Option<&[u8; 32]>) -> FoldResult {
    let mut by_version: BTreeMap<u64, usize> = BTreeMap::new();

    for (index, edition) in editions.iter().enumerate() {
        if edition.version < floor {
            continue;
        }

        match by_version.get(&edition.version) {
            Some(&current) if editions[current].tiebreak_id <= edition.tiebreak_id => {}
            _ => {
                by_version.insert(edition.version, index);
            }
        }
    }

    let Some((&lowest_version, &lowest_index)) = by_version.first_key_value() else {
        return FoldResult::default();
    };

    let lowest = editions[lowest_index];

    let anchored = if floor == 0 {
        lowest_version == 1 && lowest.prev.is_none()
    } else if lowest_version == floor {
        floor_hash == Some(&lowest.self_hash)
    } else if lowest_version == floor + 1 {
        floor_hash.is_some() && lowest.prev.as_ref() == floor_hash
    } else {
        false
    };

    let mut head = Some(lowest_index);
    let mut gap = !anchored;
    let mut previous_version = lowest_version;
    let mut previous_hash = lowest.self_hash;

    for (&version, &index) in by_version.range(lowest_version + 1..) {
        let edition = editions[index];

        if version == previous_version + 1 && edition.prev == Some(previous_hash) {
            head = Some(index);
            previous_version = version;
            previous_hash = edition.self_hash;
        } else {
            gap = true;
            break;
        }
    }

    FoldResult {
        head,
        gap,
        anchored,
    }
}

/// The highest version overall, ignoring contiguity.
fn bootstrap_head(editions: &[EditionMeta]) -> Option<usize> {
    editions
        .iter()
        .enumerate()
        .min_by_key(|(_, edition)| (Reverse(edition.version), edition.tiebreak_id))
        .map(|(index, _)| index)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct HeadSelection {
    pub head: Option<usize>,
    pub gap: bool,
}

/// The head to prefer for one entity, given what this client already committed to.
pub fn fold_head(editions: &[EditionMeta], floor: Option<&EntityHead>) -> HeadSelection {
    let Some(floor) = floor else {
        return HeadSelection {
            head: bootstrap_head(editions),
            gap: false,
        };
    };

    let anchored = fold(editions, floor.version, Some(&floor.self_hash));

    if anchored.anchored {
        return HeadSelection {
            head: anchored.head,
            gap: anchored.gap,
        };
    }

    if anchored.head.is_none() && !anchored.gap {
        return HeadSelection::default();
    }

    let fork = editions
        .iter()
        .enumerate()
        .filter(|(_, edition)| edition.version == floor.version)
        .min_by_key(|(_, edition)| edition.tiebreak_id);

    let winner = match fork {
        Some((_, edition))
            if edition.self_hash != floor.self_hash && edition.tiebreak_id < floor.rumor_id =>
        {
            edition.self_hash
        }
        _ => {
            return HeadSelection {
                head: None,
                gap: true,
            };
        }
    };

    let refolded = fold(editions, floor.version, Some(&winner));

    if refolded.anchored {
        HeadSelection {
            head: refolded.head,
            gap: refolded.gap,
        }
    } else {
        HeadSelection {
            head: None,
            gap: true,
        }
    }
}

/// A committed head, and the refuse-downgrade floor a later fold is judged against.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EntityHead {
    pub entity: [u8; 32],
    pub version: u64,
    pub self_hash: [u8; 32],
    pub rumor_id: EventId,
}

impl From<&ParsedEdition> for EntityHead {
    fn from(edition: &ParsedEdition) -> Self {
        Self {
            entity: edition.entity,
            version: edition.version,
            self_hash: edition.self_hash,
            rumor_id: edition.rumor_id,
        }
    }
}

/// Every entity's committed head, keyed by coordinate.
pub type Floors = BTreeMap<[u8; 32], EntityHead>;

pub(crate) fn canonical_decimal(raw: &str) -> Option<u64> {
    if raw.is_empty() || !raw.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }

    if raw.len() > 1 && raw.starts_with('0') {
        return None;
    }

    raw.parse().ok()
}

fn hex32(raw: &str, name: &'static str) -> Result<[u8; 32], EditionError> {
    decode_hex_32(raw).map_err(|_| EditionError::BadField(name))
}

fn fields<'a>(
    rumor: &'a UnsignedEvent,
    name: &'static str,
) -> Result<Option<&'a [String]>, EditionError> {
    let mut found: Option<&[String]> = None;

    for tag in rumor.tags.iter() {
        let tag_fields = tag.as_slice();

        if tag_fields.first().map(String::as_str) != Some(name) {
            continue;
        }

        if found.is_some() {
            return Err(EditionError::Duplicate(name));
        }

        found = Some(tag_fields);
    }

    Ok(found)
}

fn value<'a>(
    rumor: &'a UnsignedEvent,
    name: &'static str,
) -> Result<Option<&'a str>, EditionError> {
    match fields(rumor, name)? {
        Some(fields) if fields.len() == 2 => Ok(Some(fields[1].as_str())),
        Some(_) => Err(EditionError::BadField(name)),
        None => Ok(None),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn meta(version: u64, prev: Option<[u8; 32]>, hash: u8, tiebreak: u8) -> EditionMeta {
        EditionMeta {
            version,
            self_hash: [hash; 32],
            prev,
            tiebreak_id: EventId::from_byte_array([tiebreak; 32]),
        }
    }

    fn head(version: u64, hash: u8, rumor: u8) -> EntityHead {
        EntityHead {
            entity: [0x11; 32],
            version,
            self_hash: [hash; 32],
            rumor_id: EventId::from_byte_array([rumor; 32]),
        }
    }

    #[test]
    fn fold_picks_the_head_from_the_chain_and_the_floor() {
        let chain = [
            meta(1, None, 0xa1, 1),
            meta(2, Some([0xa1; 32]), 0xa2, 2),
            meta(3, Some([0xa2; 32]), 0xa3, 3),
        ];

        let folded = fold(&chain, 0, None);
        assert_eq!(folded.head, Some(2));
        assert!(!folded.gap && folded.anchored);

        // A missing link stops the walk at the last contiguous edition.
        let gapped = fold(&[chain[0], chain[2]], 0, None);
        assert_eq!(gapped.head, Some(0));
        assert!(gapped.gap && gapped.anchored);

        // Everything below the held floor is a stale relay, not a gap.
        let stale = fold(&chain[..2], 3, Some(&[0xa3; 32]));
        assert_eq!(stale.head, None);
        assert!(!stale.gap && !stale.anchored);

        // A fork at a version breaks on the lower inner rumor id, and the chain resumes.
        let fork = [meta(1, None, 0xb1, 9), meta(1, None, 0xa1, 1)];
        assert_eq!(
            fold(&fork, 0, None).head,
            Some(1),
            "the lower rumor id wins"
        );
        let forked = [fork[0], fork[1], chain[1], chain[2]];
        assert_eq!(fold(&forked, 0, None).head, Some(3));

        // A re-wrap onto the head we hold is the legitimate case; one whose `prev` no
        // longer resolves is a withholding.
        let rewrapped = meta(5, Some([0x99; 32]), 0xc5, 5);
        assert_eq!(
            fold_head(&[rewrapped], Some(&head(4, 0x99, 4))).head,
            Some(0)
        );
        let dangling = meta(5, Some([0x88; 32]), 0xc5, 5);
        let refused = fold_head(&[dangling], Some(&head(4, 0x99, 4)));
        assert_eq!(refused.head, None);
        assert!(refused.gap);

        // A bootstrap takes it anyway: a compaction would leave a joiner with nothing.
        assert_eq!(bootstrap_head(&[dangling]), Some(0));
        assert_eq!(fold_head(&[dangling], None).head, Some(0));

        // A fork at the floor's own version converges to the lower rumor id when that is
        // genuinely earlier than what we hold, and the chain above it re-anchors.
        let forked = [
            meta(2, Some([0xa1; 32]), 0xb2, 3),
            meta(3, Some([0xb2; 32]), 0xb3, 4),
        ];
        let converged = fold_head(&forked, Some(&head(2, 0xaa, 9)));
        assert_eq!(converged.head, Some(1));
        assert!(!converged.gap);

        // A fork that is not earlier than the held head is refused.
        assert_eq!(fold_head(&forked, Some(&head(2, 0xaa, 2))).head, None);
    }

    #[test]
    fn edition_hash_matches_the_cross_client_vector() {
        let entity = [0x11u8; 32];

        assert_eq!(
            HEXLOWER.encode(&edition_hash(&entity, 1, None, b"hello")),
            "2daf42e65a6bc259a4c99fac6df754a5d3d92310607cf13e2a1e8c94d42f6303"
        );

        // The golden vector only exercises the absent-prev encoding, so pin the flag.
        let bytes = signing_bytes(&entity, 1, Some(&entity), b"hello");
        assert_eq!(bytes[8 + EDITION_LABEL.len() + 32 + 8], 1);
    }
}
