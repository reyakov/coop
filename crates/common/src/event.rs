use std::hash::{DefaultHasher, Hash, Hasher};

use itertools::Itertools;
use nostr_sdk::prelude::*;

pub trait EventExt {
    fn uniq_id(&self) -> u64;
    fn extract_public_keys(&self) -> Vec<PublicKey>;
}

impl EventExt for Event {
    fn uniq_id(&self) -> u64 {
        let mut hasher = DefaultHasher::new();
        let mut pubkeys: Vec<PublicKey> = self.extract_public_keys();
        pubkeys.sort();
        pubkeys.hash(&mut hasher);
        hasher.finish()
    }

    fn extract_public_keys(&self) -> Vec<PublicKey> {
        let mut public_keys: Vec<PublicKey> = self.tags.public_keys().collect();
        public_keys.push(self.pubkey);

        public_keys.into_iter().unique().collect()
    }
}

impl EventExt for UnsignedEvent {
    fn uniq_id(&self) -> u64 {
        let mut hasher = DefaultHasher::new();
        let mut pubkeys: Vec<PublicKey> = vec![];

        // Add all public keys from event
        pubkeys.push(self.pubkey);
        pubkeys.extend(self.tags.public_keys().collect::<Vec<_>>());

        // Generate unique hash
        pubkeys
            .into_iter()
            .unique()
            .sorted()
            .collect::<Vec<_>>()
            .hash(&mut hasher);

        hasher.finish()
    }

    fn extract_public_keys(&self) -> Vec<PublicKey> {
        let mut public_keys: Vec<PublicKey> = self.tags.public_keys().collect();
        public_keys.push(self.pubkey);
        public_keys.into_iter().unique().sorted().collect()
    }
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
}
