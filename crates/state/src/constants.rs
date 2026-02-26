use std::sync::OnceLock;

/// Client name (Application name)
pub const CLIENT_NAME: &str = "Coop";

/// COOP's public key
pub const COOP_PUBKEY: &str = "npub1j3rz3ndl902lya6ywxvy5c983lxs8mpukqnx4pa4lt5wrykwl5ys7wpw3x";

/// App ID
pub const APP_ID: &str = "su.reya.coop";

/// Keyring name
pub const KEYRING: &str = "Coop Safe Storage";

/// Default timeout in second for subscription
pub const TIMEOUT: u64 = 2;

/// Default delay for searching
pub const FIND_DELAY: u64 = 600;

/// Default limit for searching
pub const FIND_LIMIT: usize = 20;

/// Default timeout for Nostr Connect
pub const NOSTR_CONNECT_TIMEOUT: u64 = 200;

/// Default Nostr Connect relay
pub const NOSTR_CONNECT_RELAY: &str = "wss://relay.nsec.app";

/// Default subscription id for device gift wrap events
pub const DEVICE_GIFTWRAP: &str = "device-gift-wraps";

/// Default subscription id for user gift wrap events
pub const USER_GIFTWRAP: &str = "user-gift-wraps";

/// Default vertex relays
pub const WOT_RELAYS: [&str; 1] = ["wss://relay.vertexlab.io"];

/// Default search relays
pub const SEARCH_RELAYS: [&str; 1] = ["wss://antiprimal.net"];

/// Default bootstrap relays
pub const BOOTSTRAP_RELAYS: [&str; 3] = [
    "wss://relay.damus.io",
    "wss://nos.lol",
    "wss://user.kindpag.es",
];

static APP_NAME: OnceLock<String> = OnceLock::new();

/// Get the app name
pub fn app_name() -> &'static String {
    APP_NAME.get_or_init(|| {
        let devicename = whoami::devicename();
        let platform = whoami::platform();

        format!("{CLIENT_NAME} on {platform} ({devicename})")
    })
}
