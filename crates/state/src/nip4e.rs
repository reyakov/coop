use nostr_sdk::prelude::*;

/// Announcement
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Announcement {
    /// The public key of the device that created this announcement.
    public_key: PublicKey,
}

impl Announcement {
    pub fn from_event(event: &Event) -> Option<Self> {
        let public_key = event
            .tags
            .iter()
            .find(|tag| tag.kind() == "n")
            .and_then(|tag| tag.content())
            .and_then(|c| PublicKey::parse(c).ok())?;

        Some(Self { public_key })
    }

    /// Returns the public key of the device that created this announcement.
    pub fn public_key(&self) -> PublicKey {
        self.public_key
    }
}
