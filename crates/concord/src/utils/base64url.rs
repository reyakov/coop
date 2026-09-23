use anyhow::{Result, anyhow};
use base64::engine::{DecodePaddingMode, GeneralPurpose, GeneralPurposeConfig};
use base64::{Engine as _, alphabet};

/// Unpadded base64url (RFC 4648 §5), 43 characters for 32 bytes: §8's value
/// encoding at any depth.
///
/// The reader tolerates non-zero trailing bits; the writer never emits them.
/// The spec's own worked example (`examples.md` §6.2) contains five such
/// values, and a reader cannot tell a mis-encoded named field from a correctly
/// encoded one, so the boundary is the writer's alone.
const BASE64URL: GeneralPurpose = GeneralPurpose::new(
    &alphabet::URL_SAFE,
    GeneralPurposeConfig::new()
        .with_encode_padding(false)
        .with_decode_padding_mode(DecodePaddingMode::RequireNone)
        .with_decode_allow_trailing_bits(true),
);

pub(crate) fn encode(bytes: &[u8]) -> String {
    BASE64URL.encode(bytes)
}

/// Decodes one 32-byte value, the width every §8 field has.
pub(crate) fn decode_32(value: &str) -> Result<[u8; 32]> {
    let bytes = BASE64URL
        .decode(value.trim())
        .map_err(|error| anyhow!("invalid base64url: {error}"))?;

    bytes
        .as_slice()
        .try_into()
        .map_err(|_| anyhow!("expected 32 bytes, got {}", bytes.len()))
}
