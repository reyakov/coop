use std::collections::HashSet;
use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};

use nostr_sdk::prelude::*;

use crate::{GroupId, GroupKey};

const LIST_SUBSCRIPTION: &str = "nip29-list";
const FORK_SUBSCRIPTION: &str = "nip29-forks";
const GROUP_PREFIX: &str = "grp-";
const BROWSE_PREFIX: &str = "brw-";
const PAGE_PREFIX: &str = "pg-";

pub(crate) fn list_subscription_id() -> SubscriptionId {
    SubscriptionId::new(LIST_SUBSCRIPTION)
}

pub(crate) fn fork_subscription_id() -> SubscriptionId {
    SubscriptionId::new(FORK_SUBSCRIPTION)
}

pub(crate) fn subscription_id(key: &GroupKey) -> SubscriptionId {
    SubscriptionId::new(format!("{GROUP_PREFIX}{:08x}", key.uniq_id() as u32))
}

pub(crate) fn browse_subscription(relay: &RelayUrl) -> SubscriptionId {
    let mut hasher = DefaultHasher::new();
    relay.hash(&mut hasher);

    SubscriptionId::new(format!("{BROWSE_PREFIX}{:08x}", hasher.finish() as u32))
}

pub(crate) fn page_subscription(key: &GroupKey) -> SubscriptionId {
    SubscriptionId::new(format!("{PAGE_PREFIX}{:08x}", key.uniq_id() as u32))
}

pub(crate) fn is_page(id: &SubscriptionId) -> bool {
    id.as_str().starts_with(PAGE_PREFIX)
}

pub(crate) fn is_own_subscription(id: &SubscriptionId) -> bool {
    let id = id.as_str();
    id == LIST_SUBSCRIPTION
        || id == FORK_SUBSCRIPTION
        || id.starts_with(GROUP_PREFIX)
        || id.starts_with(BROWSE_PREFIX)
        || id.starts_with(PAGE_PREFIX)
}

pub(crate) fn parse_members(event: &Event) -> Option<(GroupId, Timestamp, usize)> {
    if event.kind != Kind::GroupMembers {
        return None;
    }

    let id = GroupId::new(event.tags.identifier()?).ok()?;
    let members = event.tags.public_keys().collect::<HashSet<_>>().len();

    Some((id, event.created_at, members))
}

const ONSETS: [&str; 34] = [
    "b", "c", "d", "f", "g", "h", "j", "k", "l", "m", "n", "p", "r", "s", "t", "v", "br", "cr",
    "dr", "fr", "gr", "pr", "tr", "bl", "cl", "fl", "gl", "pl", "sl", "sh", "ch", "th", "st", "sk",
];
const NUCLEI: [&str; 9] = ["a", "e", "i", "o", "u", "ae", "ei", "io", "ou"];
const CODAS: [&str; 11] = ["", "", "", "n", "r", "s", "t", "d", "l", "m", "k"];

/// A short pronounceable id for new groups, seeded from a fresh key.
pub(crate) fn readable_id() -> String {
    let mut picks = SecretKey::generate().secret_bytes().into_iter();

    let mut id = String::with_capacity(18);

    for _ in 0..3 {
        let onset = ONSETS[usize::from(picks.next().unwrap_or(0)) % ONSETS.len()];
        let nucleus = NUCLEI[usize::from(picks.next().unwrap_or(0)) % NUCLEI.len()];
        let coda = CODAS[usize::from(picks.next().unwrap_or(0)) % CODAS.len()];

        id.push_str(onset);
        id.push_str(nucleus);
        id.push_str(coda);
    }

    id
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_readable_id() {
        for _ in 0..32 {
            let id = readable_id();

            assert!((6..=15).contains(&id.len()));
            assert!(id.chars().all(|character| character.is_ascii_lowercase()));
        }
    }
}
