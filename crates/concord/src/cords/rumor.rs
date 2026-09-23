use std::fmt;

use nostr_sdk::prelude::*;

use crate::cord01::StreamError;
use crate::cord04::{AuthorityCitation, TAG_CITATION, citation_from};
use crate::decode_hex_32;

#[derive(Debug)]
pub enum RumorError {
    Stream(StreamError),
    NotEncryptedSealed,
    UnknownKind(u16),
    MissingTag(&'static str),
    DuplicateTag(&'static str),
    BadTag(&'static str),
    /// Neither a delete nor a timer notice may be erased by the policy it carries.
    ExemptExpiration,
}

impl fmt::Display for RumorError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            RumorError::Stream(error) => write!(f, "stream: {error}"),
            RumorError::NotEncryptedSealed => write!(f, "rumor must ride an encrypted seal"),
            RumorError::UnknownKind(kind) => write!(f, "not a rumor kind: {kind}"),
            RumorError::MissingTag(name) => write!(f, "missing tag: {name}"),
            RumorError::DuplicateTag(name) => write!(f, "duplicate tag: {name}"),
            RumorError::BadTag(name) => write!(f, "malformed tag: {name}"),
            RumorError::ExemptExpiration => {
                write!(f, "a delete or timer notice must not carry an expiration")
            }
        }
    }
}

impl std::error::Error for RumorError {}

impl From<StreamError> for RumorError {
    fn from(error: StreamError) -> Self {
        RumorError::Stream(error)
    }
}

pub fn tag<'a>(
    rumor: &'a UnsignedEvent,
    name: &'static str,
) -> Result<Option<&'a [String]>, RumorError> {
    let mut found: Option<&[String]> = None;

    for candidate in rumor.tags.iter() {
        let fields = candidate.as_slice();

        if fields.first().map(String::as_str) != Some(name) {
            continue;
        }

        if found.is_some() {
            return Err(RumorError::DuplicateTag(name));
        }

        found = Some(fields);
    }

    Ok(found)
}

pub fn required<'a>(
    rumor: &'a UnsignedEvent,
    name: &'static str,
) -> Result<&'a [String], RumorError> {
    tag(rumor, name)?.ok_or(RumorError::MissingTag(name))
}

pub fn value<'a>(fields: &'a [String], name: &'static str) -> Result<&'a str, RumorError> {
    fields
        .get(1)
        .map(String::as_str)
        .ok_or(RumorError::BadTag(name))
}

pub fn pubkey(hex: &str, name: &'static str) -> Result<PublicKey, RumorError> {
    let bytes = decode_hex_32(hex).map_err(|_| RumorError::BadTag(name))?;

    PublicKey::from_slice(&bytes).map_err(|_| RumorError::BadTag(name))
}

pub fn optional_citation(rumor: &UnsignedEvent) -> Result<Option<AuthorityCitation>, RumorError> {
    let Some(fields) = tag(rumor, TAG_CITATION)? else {
        return Ok(None);
    };

    citation_from(fields)
        .map(Some)
        .ok_or(RumorError::BadTag(TAG_CITATION))
}
