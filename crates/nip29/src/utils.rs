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
