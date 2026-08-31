use std::cmp::Ordering;
use std::hash::{Hash, Hasher};

use gpui::SharedString;
use nostr_sdk::prelude::*;
use state::Announcement;

/// Person
#[derive(Debug, Clone)]
pub struct Person {
    /// Public Key
    public_key: PublicKey,

    /// Metadata (profile)
    metadata: Metadata,

    /// Dekey (NIP-4e) announcement
    announcement: Option<Announcement>,

    /// Messaging relays
    messaging_relays: Vec<RelayUrl>,
}

impl PartialEq for Person {
    fn eq(&self, other: &Self) -> bool {
        self.public_key == other.public_key
    }
}

impl Eq for Person {}

impl PartialOrd for Person {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for Person {
    fn cmp(&self, other: &Self) -> Ordering {
        self.name().cmp(&other.name())
    }
}

impl Hash for Person {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.public_key.hash(state)
    }
}

impl From<PublicKey> for Person {
    fn from(public_key: PublicKey) -> Self {
        Self::new(public_key, Metadata::default())
    }
}

impl Person {
    pub fn new(public_key: PublicKey, metadata: Metadata) -> Self {
        Self {
            public_key,
            metadata,
            announcement: None,
            messaging_relays: vec![],
        }
    }

    /// Build profile encryption keys announcement
    pub fn with_announcement(mut self, announcement: Announcement) -> Self {
        self.announcement = Some(announcement);
        self
    }

    /// Build profile messaging relays
    pub fn with_messaging_relays<I>(mut self, relays: I) -> Self
    where
        I: IntoIterator<Item = RelayUrl>,
    {
        self.messaging_relays = relays.into_iter().collect();
        self
    }

    /// Get profile public key
    pub fn public_key(&self) -> PublicKey {
        self.public_key
    }

    /// Get profile metadata
    pub fn metadata(&self) -> Metadata {
        self.metadata.clone()
    }

    /// Get profile encryption keys announcement
    pub fn announcement(&self) -> Option<Announcement> {
        self.announcement.clone()
    }

    /// Get profile messaging relays
    pub fn messaging_relays(&self) -> &Vec<RelayUrl> {
        &self.messaging_relays
    }

    /// Get relay hint for messaging relay list
    pub fn messaging_relay_hint(&self) -> Option<RelayUrl> {
        self.messaging_relays.first().cloned()
    }

    /// Get profile avatar
    pub fn avatar(&self) -> SharedString {
        // On web, the browser blocks cross-origin image fetches unless the
        // picture host sends CORS headers, so remote avatars can never load
        // there. Each doomed fetch still triggers a full-window repaint when
        // it fails, which is very costly on wasm's single main thread when a
        // list renders many avatars at once. Fall back to the bundled avatar
        // for remote pictures on web.
        #[cfg(target_arch = "wasm32")]
        if let Some(picture) = self.metadata.picture.as_ref()
            && !picture.is_empty()
            && url::Url::parse(picture).is_ok_and(|url| matches!(url.scheme(), "http" | "https"))
        {
            return "brand/avatar.png".into();
        }

        self.metadata
            .picture
            .as_ref()
            .filter(|picture| !picture.is_empty())
            .map(|picture| picture.into())
            .unwrap_or_else(|| "brand/avatar.png".into())
    }

    /// Get profile name
    pub fn name(&self) -> SharedString {
        if let Some(display_name) = self.metadata.display_name.as_ref()
            && !display_name.is_empty()
        {
            return SharedString::from(display_name.trim());
        }

        if let Some(name) = self.metadata.name.as_ref()
            && !name.is_empty()
        {
            return SharedString::from(name.trim());
        }

        SharedString::from(shorten_pubkey(self.public_key(), 4))
    }

    /// Set profile metadata
    pub fn set_metadata(&mut self, metadata: Metadata) {
        self.metadata = metadata;
    }

    /// Set profile encryption keys announcement
    pub fn set_announcement(&mut self, announcement: Announcement) {
        self.announcement = Some(announcement);
    }

    /// Set profile messaging relays
    pub fn set_messaging_relays<I>(&mut self, relays: I)
    where
        I: IntoIterator<Item = RelayUrl>,
    {
        self.messaging_relays = relays.into_iter().collect();
    }
}

/// Shorten a [`PublicKey`] to a string with the first and last `len` characters
///
/// Ex. `00000000:00000002`
pub fn shorten_pubkey(public_key: PublicKey, len: usize) -> String {
    let Ok(pubkey) = public_key.to_bech32();

    format!(
        "{}...{}",
        &pubkey[0..(len + 1)],
        &pubkey[pubkey.len() - len..]
    )
}
