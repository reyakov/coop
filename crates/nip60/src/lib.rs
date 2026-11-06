#![cfg(not(target_arch = "wasm32"))]

use gpui::App;
use nostr::prelude::*;

/// Initialize the wallet registry.
pub fn init(_cx: &mut App) {}

/// Compile-only phase 0 spike: proves the `nostr::nips::nip60` types, kinds,
/// and the cdk crates resolve against the workspace's git nostr. Replaced by
/// the real implementation.
#[allow(dead_code)]
fn spike() {
    let _kinds = (
        Kind::CashuWallet,
        Kind::CashuWalletUnspentProof,
        Kind::CashuWalletSpendingHistory,
        Kind::CashuWalletQuote,
    );

    let _history =
        nostr::nips::nip60::SpendingHistory::new(nostr::nips::nip60::TransactionDirection::In, 1);
    let _builder = cdk::wallet::WalletBuilder::new();
    let _store: Option<cdk_redb::WalletRedbDatabase> = None;
    let _inbox: Option<cdk_nostr::inbox::NostrInbox> = None;

    if let Ok(mint) = nostr::types::Url::parse("https://mint.example.com") {
        let proof = nostr::nips::nip60::CashuProof {
            id: String::new(),
            amount: 1,
            secret: String::new(),
            c: String::new(),
        };
        let _wallet_event = nostr::nips::nip60::WalletEvent::new("privkey", vec![mint.clone()]);
        let _token_event =
            nostr::nips::nip60::TokenEvent::new(mint, vec![proof]).destroyed("id".to_string());
    }
}

/// Only compiles when `nostr` and `cdk_nostr` resolve to the same crate.
#[allow(dead_code)]
fn spike_unified(key: nostr::key::SecretKey) -> cdk_nostr::nostr::key::SecretKey {
    key
}

#[allow(dead_code)]
fn spike_sdk_unified(client: nostr_sdk::client::Client) -> cdk_nostr::nostr_sdk::client::Client {
    client
}
