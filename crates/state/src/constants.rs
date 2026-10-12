/// Client name (Application name)
pub const CLIENT_NAME: &str = "Coop";

/// App ID
pub const APP_ID: &str = "su.reya.coop";

/// Keyring name
pub const USER_KEYRING: &str = "Coop Safe Storage";

/// Default timeout for subscription
pub const TIMEOUT: u64 = 2;

/// Default timeout for Nostr Connect
pub const NOSTR_CONNECT_TIMEOUT: u64 = 60;

/// Default delay for searching
pub const FIND_DELAY: u64 = 600;

/// Default limit for searching
pub const FIND_LIMIT: usize = 20;

/// Default subscription id for device gift wrap events
pub const DEVICE_GIFTWRAP: &str = "device-gift-wraps";

/// Default subscription id for user gift wrap events
pub const USER_GIFTWRAP: &str = "user-gift-wraps";

/// Default search relays
pub(crate) const INDEXER_RELAYS: [&str; 2] =
    ["wss://indexer.coracle.social", "wss://user.kindpag.es"];

/// Default search relays
pub(crate) const SEARCH_RELAYS: [&str; 2] = ["wss://antiprimal.net", "wss://search.nos.today"];

/// Default bootstrap relays
pub const BOOTSTRAP_RELAYS: [&str; 3] = [
    "wss://relay.ditto.pub",
    "wss://relay.primal.net",
    "wss://profiles.nostr1.com",
];
