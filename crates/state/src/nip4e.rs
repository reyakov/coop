use nostr_sdk::prelude::*;

/// Announcement
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Announcement {
    /// The public key of the device that created this announcement.
    public_key: PublicKey,
    /// The id of the announcement event, for ordering
    event_id: EventId,
    /// The creation time of the announcement event
    created_at: Timestamp,
}

impl Announcement {
    pub fn from_event(event: &Event) -> Option<Self> {
        let public_key = event
            .tags
            .iter()
            .find(|tag| tag.kind() == "n")
            .and_then(|tag| tag.content())
            .and_then(|c| PublicKey::parse(c).ok())?;

        Some(Self {
            public_key,
            event_id: event.id,
            created_at: event.created_at,
        })
    }

    /// Returns the public key of the device that created this announcement.
    pub fn public_key(&self) -> PublicKey {
        self.public_key
    }

    pub fn is_newer_than(&self, other: &Self) -> bool {
        self.created_at > other.created_at
            || (self.created_at == other.created_at && self.event_id < other.event_id)
    }
}
