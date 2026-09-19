pub mod base64url;
pub mod derive;

use anyhow::{Result, anyhow, bail};
use data_encoding::HEXLOWER;
use rand::TryRng as _;
use rand::rngs::SysRng;
use serde::Serialize;

use crate::Extra;

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

/// Hex to unpadded base64url for one 32-byte §8 value.
pub(crate) fn hex32_to_base64(value: &str) -> Result<String> {
    Ok(base64url::encode(&decode_hex_32(value)?))
}

/// Unpadded base64url to lowercase hex for one 32-byte §8 value.
pub(crate) fn base64_to_hex32(value: &str) -> Result<String> {
    Ok(HEXLOWER.encode(&base64url::decode_32(value)?))
}

/// Canonical JSON bytes: the total-order tie-break every content merge uses.
pub(crate) fn canonical<T: Serialize>(value: &T) -> String {
    serde_json::to_string(value).unwrap_or_default()
}

/// Unions an unknown-field map. Where both sides carry a key, the
/// lexicographically lowest canonical bytes win, so two devices converge
/// instead of flapping.
pub(crate) fn union(into: &mut Extra, other: Extra) {
    for (key, value) in other {
        let replace = match into.get(&key) {
            Some(existing) => canonical(&value) < canonical(existing),
            None => true,
        };

        if replace {
            into.insert(key, value);
        }
    }
}
