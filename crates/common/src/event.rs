use itertools::Itertools;
use nostr_sdk::prelude::*;
use sha2::{Digest, Sha256};

const HEX_DIGITS: [u8; 16] = *b"0123456789abcdef";

pub trait EventExt {
    /// Id over the event's participants, stable across Rust versions.
    fn uniq_id(&self) -> u64;
    fn extract_public_keys(&self) -> Vec<PublicKey>;
}

impl EventExt for Event {
    fn uniq_id(&self) -> u64 {
        pubkey_set_uniq_id(&self.extract_public_keys())
    }

    fn extract_public_keys(&self) -> Vec<PublicKey> {
        let mut public_keys: Vec<PublicKey> = self.tags.public_keys().collect();
        public_keys.push(self.pubkey);

        public_keys.into_iter().unique().collect()
    }
}

impl EventExt for UnsignedEvent {
    fn uniq_id(&self) -> u64 {
        pubkey_set_uniq_id(&self.extract_public_keys())
    }

    fn extract_public_keys(&self) -> Vec<PublicKey> {
        let mut public_keys: Vec<PublicKey> = self.tags.public_keys().collect();
        public_keys.push(self.pubkey);

        public_keys.into_iter().unique().sorted().collect()
    }
}

/// Digest over NUL-separated parts, so part boundaries stay unambiguous.
fn parts_digest(parts: &[&str]) -> [u8; 32] {
    let mut hasher = Sha256::new();

    for part in parts {
        hasher.update(part.as_bytes());
        hasher.update([0]);
    }

    hasher.finalize().into()
}

/// Stable id over parts: hex-encoded SHA-256 truncated to 128 bits.
pub fn stable_id(parts: &[&str]) -> String {
    let digest = parts_digest(parts);

    let mut id = String::with_capacity(32);
    for byte in &digest[..16] {
        id.push(HEX_DIGITS[usize::from(byte >> 4)] as char);
        id.push(HEX_DIGITS[usize::from(byte & 0x0f)] as char);
    }

    id
}

/// Stable 64-bit id over parts, for callers that key ids on `u64`.
pub fn stable_id_u64(parts: &[&str]) -> u64 {
    let digest = parts_digest(parts);

    let mut lead = [0u8; 8];
    for (slot, byte) in lead.iter_mut().zip(digest.iter()) {
        *slot = *byte;
    }

    u64::from_be_bytes(lead)
}

/// Hashes the sorted hex forms of the participant set.
fn pubkey_set_uniq_id(public_keys: &[PublicKey]) -> u64 {
    let mut parts: Vec<String> = public_keys.iter().map(|key| key.to_string()).collect();
    parts.sort();
    let parts: Vec<&str> = parts.iter().map(String::as_str).collect();
    stable_id_u64(&parts)
}

/// Custom emoji (NIP-30) declared by `emoji` tags, first per shortcode wins
pub fn emojis_from_tags(tags: &Tags) -> Vec<(String, Url)> {
    let mut emojis: Vec<(String, Url)> = Vec::new();

    for tag in tags.iter() {
        let Ok(Nip30Tag::Emoji {
            shortcode,
            image_url,
            ..
        }) = Nip30Tag::try_from(tag)
        else {
            continue;
        };

        if !emojis.iter().any(|(existing, _)| *existing == shortcode) {
            emojis.push((shortcode, image_url));
        }
    }

    emojis
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn emojis_from_tags_parses_and_deduplicates() {
        let tags = Tags::from_list(vec![
            Tag::parse(vec!["emoji", "soapbox", "https://example.com/soapbox.png"]).unwrap(),
            Tag::parse(vec!["emoji", "soapbox", "https://example.com/other.png"]).unwrap(),
            Tag::parse(vec![
                "emoji",
                "bad shortcode",
                "https://example.com/bad.png",
            ])
            .unwrap(),
        ]);

        let emojis = emojis_from_tags(&tags);

        assert_eq!(emojis.len(), 1);
        assert_eq!(emojis[0].0, "soapbox");
        assert_eq!(
            emojis[0].1,
            Url::parse("https://example.com/soapbox.png").unwrap()
        );
    }

    fn event_with_participants(public_key: PublicKey, participants: Vec<PublicKey>) -> Event {
        let tags = Tags::from_list(participants.into_iter().map(Tag::public_key).collect());

        Event::new(
            EventId::from_byte_array([0; 32]),
            public_key,
            Timestamp::from(0u64),
            Kind::TextNote,
            tags,
            "content".to_string(),
            Signature::from_byte_array([0; 64]),
        )
    }

    #[test]
    fn uniq_id_ignores_participant_order() {
        let me = Keys::new(SecretKey::generate()).public_key();
        let alice = Keys::new(SecretKey::generate()).public_key();
        let bob = Keys::new(SecretKey::generate()).public_key();

        let first = event_with_participants(me, vec![alice, bob]);
        let second = event_with_participants(me, vec![bob, alice]);

        assert_eq!(first.uniq_id(), second.uniq_id());
    }

    #[test]
    fn uniq_id_tracks_participants() {
        let me = Keys::new(SecretKey::generate()).public_key();
        let alice = Keys::new(SecretKey::generate()).public_key();
        let bob = Keys::new(SecretKey::generate()).public_key();

        let with_alice = event_with_participants(me, vec![alice]);
        let with_bob = event_with_participants(me, vec![bob]);

        assert_ne!(with_alice.uniq_id(), with_bob.uniq_id());
    }

    #[test]
    fn stable_id_is_deterministic_128_bit_hex() {
        let id = stable_id(&["alpha", "beta"]);

        assert_eq!(id.len(), 32);
        assert!(id.chars().all(|character| character.is_ascii_hexdigit()));
        assert_eq!(id, stable_id(&["alpha", "beta"]));
    }
}
