pub mod derive;

use anyhow::{Result, anyhow, bail};
use data_encoding::HEXLOWER;
use rand::TryRng as _;
use rand::rngs::SysRng;

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
