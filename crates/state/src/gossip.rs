use std::collections::{HashMap, HashSet};

use gpui::SharedString;
use nostr_sdk::prelude::*;

/// Gossip
#[derive(Debug, Clone, Default)]
pub struct Gossip {
    relays: HashMap<PublicKey, HashSet<(RelayUrl, Option<RelayMetadata>)>>,
}

impl Gossip {
    pub fn read_only_relays(&self, public_key: &PublicKey) -> Vec<SharedString> {
        self.relays
            .get(public_key)
            .map(|relays| {
                relays
                    .iter()
                    .map(|(url, _)| url.to_string().into())
                    .collect()
            })
            .unwrap_or_default()
    }

    /// Get read relays for a given public key
    pub fn read_relays(&self, public_key: &PublicKey) -> Vec<RelayUrl> {
        self.relays
            .get(public_key)
            .map(|relays| {
                relays
                    .iter()
                    .filter_map(|(url, metadata)| {
                        if metadata.is_none() || metadata == &Some(RelayMetadata::Read) {
                            Some(url.to_owned())
                        } else {
                            None
                        }
                    })
                    .collect()
            })
            .unwrap_or_default()
    }

    /// Get write relays for a given public key
    pub fn write_relays(&self, public_key: &PublicKey) -> Vec<RelayUrl> {
        self.relays
            .get(public_key)
            .map(|relays| {
                relays
                    .iter()
                    .filter_map(|(url, metadata)| {
                        if metadata.is_none() || metadata == &Some(RelayMetadata::Write) {
                            Some(url.to_owned())
                        } else {
                            None
                        }
                    })
                    .collect()
            })
            .unwrap_or_default()
    }

    /// Insert gossip relays for a public key
    pub fn insert_relays(&mut self, event: &Event) {
        self.relays.entry(event.pubkey).or_default().extend(
            event
                .tags
                .iter()
                .filter_map(|tag| {
                    if let Some(TagStandard::RelayMetadata {
                        relay_url,
                        metadata,
                    }) = tag.clone().to_standardized()
                    {
                        Some((relay_url, metadata))
                    } else {
                        None
                    }
                })
                .take(3),
        );
    }
}
