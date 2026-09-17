pub mod chat;
pub mod control;
pub mod derive;
pub mod edition;
pub mod guestbook;
pub mod invite;
pub mod list;
pub mod pins;
pub mod rekey;
pub mod roles;
pub mod store;
pub mod stream;

use std::fmt;
use std::str::FromStr;

use anyhow::{Result, anyhow, bail};
use data_encoding::HEXLOWER;
pub use derive::GroupKey;
use rand::TryRng as _;
use rand::rngs::SysRng;
use serde::{Deserialize, Deserializer, Serialize, Serializer};

/// Unknown fields a content struct does not model, so a republish cannot wipe them.
pub(crate) type Extra = serde_json::Map<String, serde_json::Value>;

macro_rules! hex_id {
    ($(#[$meta:meta])* $name:ident) => {
        $(#[$meta])*
        #[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
        pub struct $name([u8; 32]);

        impl $name {
            pub fn from_bytes(bytes: [u8; 32]) -> Self {
                Self(bytes)
            }

            pub fn as_bytes(&self) -> &[u8; 32] {
                &self.0
            }

            pub fn to_hex(&self) -> String {
                HEXLOWER.encode(&self.0)
            }
        }

        impl From<[u8; 32]> for $name {
            fn from(bytes: [u8; 32]) -> Self {
                Self(bytes)
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str(&self.to_hex())
            }
        }

        impl fmt::Debug for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                write!(f, "{}({})", stringify!($name), self.to_hex())
            }
        }

        impl FromStr for $name {
            type Err = anyhow::Error;

            fn from_str(value: &str) -> Result<Self> {
                Ok(Self(decode_hex_32(value)?))
            }
        }

        impl Serialize for $name {
            fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
                serializer.serialize_str(&self.to_hex())
            }
        }

        impl<'de> Deserialize<'de> for $name {
            fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
                let value = String::deserialize(deserializer)?;
                value.parse().map_err(serde::de::Error::custom)
            }
        }
    };
}

hex_id! {
    /// A self-certifying commitment to the owner's key, never on the wire.
    CommunityId
}

hex_id! {
    ChannelId
}

hex_id! {
    /// Both a Role's entity coordinate and the field it repeats in its own content.
    RoleId
}

/// A key-rotation counter; it bumps only on a Rekey that removes somebody.
#[derive(
    Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug, Default, Serialize, Deserialize,
)]
pub struct Epoch(pub u64);

impl fmt::Display for Epoch {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// Uppercase and other non-canonical spellings are rejected.
pub(crate) fn decode_hex_32(value: &str) -> Result<[u8; 32]> {
    decode_hex_lower::<32>(value)
}

pub(crate) fn decode_hex_lower<const N: usize>(value: &str) -> Result<[u8; N]> {
    let bytes = HEXLOWER
        .decode(value.as_bytes())
        .map_err(|error| anyhow!("invalid hex: {error}"))?;

    let decoded: [u8; N] = bytes
        .as_slice()
        .try_into()
        .map_err(|_| anyhow!("expected {N} bytes, got {}", bytes.len()))?;

    if HEXLOWER.encode(&decoded) != value {
        bail!("hex must be lowercase and canonical");
    }

    Ok(decoded)
}

pub(crate) fn fill_random(bytes: &mut [u8]) -> Result<()> {
    SysRng
        .try_fill_bytes(bytes)
        .map_err(|error| anyhow!("os rng: {error}"))
}

pub(crate) fn random_32() -> Result<[u8; 32]> {
    let mut bytes = [0u8; 32];
    fill_random(&mut bytes)?;
    Ok(bytes)
}
