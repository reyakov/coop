#![cfg(not(target_arch = "wasm32"))]

mod keys;
mod ops;

use std::collections::{HashMap, HashSet};
use std::str::FromStr;
use std::sync::Arc;

use anyhow::{Error, anyhow};
use cdk::mint_url::MintUrl;
use cdk::nuts::{CurrencyUnit, Proof, Proofs, SpendingConditions};
use cdk::wallet::{Wallet, WalletBuilder};
use cdk_redb::WalletRedbDatabase;
use common::config_dir;
use gpui::{App, AppContext, Context, Entity, EventEmitter, Global, Subscription, Task};
use gpui_tokio::Tokio;
use nostr::nips::nip60::TokenEvent;
use nostr_sdk::prelude::*;
use smallvec::{SmallVec, smallvec};
use state::NostrRegistry;

pub fn init(cx: &mut App) {
    WalletRegistry::set_global(cx.new(WalletRegistry::new), cx);
}

struct GlobalWalletRegistry(Entity<WalletRegistry>);

impl Global for GlobalWalletRegistry {}

#[derive(Debug, Clone)]
pub enum WalletEvent {
    Updated,
    Error(String),
}

impl EventEmitter<WalletEvent> for WalletRegistry {}

/// A pending lightning invoice waiting to be paid and claimed.
#[derive(Debug, Clone)]
pub struct MintQuoteEntry {
    pub quote_id: String,
    pub request: String,
    pub amount: u64,
    pub mint: MintUrl,
}

/// One spending history entry, newest first in `WalletRegistry::history`.
#[derive(Debug, Clone)]
pub struct HistoryEntry {
    pub id: EventId,
    pub direction: TransactionDirection,
    pub amount: u64,
    pub created_at: Timestamp,
}

/// How many times a failed nutzap redemption is retried before giving up
const MAX_REDEEM_ATTEMPTS: u32 = 3;

pub struct WalletRegistry {
    wallet: Option<keys::WalletData>,
    tokens: HashMap<EventId, TokenEvent>,
    /// Token events killed by a kind 5 delete or superseded by a `del` chain
    stale: HashSet<EventId>,
    quotes: Vec<MintQuoteEntry>,
    /// Spending history entries, newest first
    history: Vec<HistoryEntry>,
    /// NIP-61 nutzap events already claimed, recorded by our 7376 events
    redeemed: HashSet<EventId>,
    /// Nutzaps seen but not yet redeemed (wallet was not ready)
    pending_nutzaps: Vec<Event>,
    /// Nutzaps with a redemption job in flight
    redeeming: HashSet<EventId>,
    /// Redemption attempts per nutzap, reset on success
    redeem_attempts: HashMap<EventId, u32>,
    /// A job errored after possibly mutating the store
    publish_failed: bool,
    store: Option<Arc<WalletRedbDatabase>>,
    wallets: HashMap<MintUrl, Wallet>,
    loading: bool,
    store_opening: bool,
    job_running: bool,
    job_queued: Vec<ops::Job>,
    notification_listener: Option<Task<()>>,
    tasks: SmallVec<[Task<()>; 2]>,
    _subscriptions: SmallVec<[Subscription; 2]>,
}

struct LoadedState {
    wallet: Option<keys::WalletData>,
    tokens: HashMap<EventId, TokenEvent>,
    stale: HashSet<EventId>,
    history: Vec<HistoryEntry>,
    redeemed: HashSet<EventId>,
    nutzaps: Vec<Event>,
}

impl WalletRegistry {
    pub fn global(cx: &App) -> Entity<Self> {
        cx.global::<GlobalWalletRegistry>().0.clone()
    }

    fn set_global(state: Entity<Self>, cx: &mut App) {
        cx.set_global(GlobalWalletRegistry(state));
    }

    fn new(cx: &mut Context<Self>) -> Self {
        let nostr = NostrRegistry::global(cx);
        let mut subscriptions = smallvec![];

        subscriptions.push(cx.subscribe(&nostr, |this, _nostr, event, cx| {
            if event.signer_changed() {
                this.reset(cx);
                this.handle_notifications(cx);
                this.load(cx);
            }
        }));

        Self {
            wallet: None,
            tokens: HashMap::new(),
            stale: HashSet::new(),
            quotes: Vec::new(),
            history: Vec::new(),
            redeemed: HashSet::new(),
            pending_nutzaps: Vec::new(),
            redeeming: HashSet::new(),
            redeem_attempts: HashMap::new(),
            publish_failed: false,
            store: None,
            wallets: HashMap::new(),
            loading: false,
            store_opening: false,
            job_running: false,
            job_queued: Vec::new(),
            notification_listener: None,
            tasks: smallvec![],
            _subscriptions: subscriptions,
        }
    }

    pub fn has_wallet(&self) -> bool {
        self.wallet.is_some()
    }

    /// Spendable balance in satoshis across the active token events.
    pub fn balance(&self) -> u64 {
        self.tokens
            .values()
            .map(|token| token.proofs.iter().map(|proof| proof.amount).sum::<u64>())
            .sum()
    }

    /// Lightning invoices waiting to be paid and claimed.
    pub fn mint_quotes(&self) -> &[MintQuoteEntry] {
        &self.quotes
    }

    /// Spending history, newest first.
    pub fn history(&self) -> &[HistoryEntry] {
        &self.history
    }

    /// Create a lightning invoice that funds the wallet once paid.
    pub fn create_mint_quote(&mut self, amount: u64, cx: &mut Context<Self>) {
        let Some(mint_url) = self.primary_mint(cx) else {
            return;
        };

        self.schedule_job(
            ops::Job::CreateQuote {
                mint: mint_url,
                amount,
            },
            cx,
        );
    }

    /// Check a mint quote with the mint and claim it once it is paid.
    pub fn claim_mint_quote(&mut self, quote_id: String, cx: &mut Context<Self>) {
        let Some(entry) = self.quotes.iter().find(|quote| quote.quote_id == quote_id) else {
            self.emit_error("Unknown mint quote".to_string(), cx);
            return;
        };

        self.schedule_job(
            ops::Job::ClaimQuote {
                mint: entry.mint.clone(),
                quote_id,
            },
            cx,
        );
    }

    /// Drop a pending invoice at the user's request.
    pub fn dismiss_mint_quote(&mut self, quote_id: String, cx: &mut Context<Self>) {
        if !self.quotes.iter().any(|quote| quote.quote_id == quote_id) {
            return;
        }

        self.quotes.retain(|quote| quote.quote_id != quote_id);
        self.emit_updated(cx);
    }

    /// Send a NIP-61 nutzap to a peer, optionally nutzapping one of their events.
    pub fn send_nutzap(
        &mut self,
        recipient: PublicKey,
        amount: u64,
        target: Option<EventId>,
        comment: Option<String>,
        cx: &mut Context<Self>,
    ) {
        if self.wallet.is_none() {
            self.emit_error("No wallet is set up".to_string(), cx);
            return;
        }

        self.schedule_job(
            ops::Job::SendNutzap {
                recipient,
                amount,
                target,
                comment,
            },
            cx,
        );
    }

    /// The first wallet mint; the UI is effectively single-mint in v1.
    fn primary_mint(&self, cx: &mut Context<Self>) -> Option<MintUrl> {
        let Some(mint) = self.wallet.as_ref().and_then(|wallet| wallet.mints.first()) else {
            self.emit_error("No wallet is set up".to_string(), cx);
            return None;
        };

        match MintUrl::from_str(mint.as_str()) {
            Ok(mint_url) => Some(mint_url),
            Err(error) => {
                self.emit_error(format!("invalid mint url: {error}"), cx);
                None
            }
        }
    }

    /// Receive a cashu token created elsewhere.
    pub fn receive_token(&mut self, token: String, cx: &mut Context<Self>) {
        let mint = match cdk::nuts::Token::from_str(&token) {
            Ok(token) => token.mint_url(),
            Err(_) => {
                self.emit_error("Invalid token".to_string(), cx);
                return;
            }
        };

        let mint = match mint {
            Ok(mint) => mint,
            Err(_) => {
                self.emit_error("Token has no mint".to_string(), cx);
                return;
            }
        };

        if !self.wallets.contains_key(&mint) {
            self.emit_error("Token mint is not in the wallet".to_string(), cx);
            return;
        }

        self.schedule_job(ops::Job::Receive { mint, token }, cx);
    }

    /// Create the wallet (first mint) or add a mint to an existing one.
    pub fn setup(&mut self, mint: String, cx: &mut Context<Self>) {
        if self.loading {
            self.emit_error("Wallet is still syncing, try again".to_string(), cx);
            return;
        }

        let mint = match Url::parse(&mint) {
            Ok(mint) => mint,
            Err(_) => {
                self.emit_error("Invalid mint URL".to_string(), cx);
                return;
            }
        };

        let mut data = match self.wallet.clone() {
            Some(mut data) => {
                if data.mints.contains(&mint) {
                    return;
                }
                data.mints.push(mint.clone());
                data
            }
            None => keys::WalletData::generate(mint.clone()),
        };

        let nostr = NostrRegistry::global(cx);
        let client = nostr.read(cx).client().clone();
        let signer = nostr.read(cx).signer().clone();

        let publish = cx.background_spawn(async move {
            let me = signer.get_public_key_async().await?;

            let event = data.to_event(&signer, &me).await?;
            client.send_event(&event).await?;

            let filter = Filter::new().kind(Kind::RelayList).author(me).limit(1);
            let relays = match client.database().query(filter).await?.into_iter().next() {
                Some(list) => nip65::extract_relay_list(&list)
                    .map(|(relay, _)| relay)
                    .collect::<Vec<_>>(),
                None => Vec::new(),
            };

            let info = keys::build_nutzap_info_event(
                &signer,
                &data.mints,
                &relays,
                Keys::new(data.privkey.clone()).public_key(),
            )
            .await?;
            client.send_event(&info).await?;

            data.event_id = event.id;
            data.created_at = event.created_at;

            Ok::<_, Error>(data)
        });

        self.tasks.push(cx.spawn(async move |this, cx| {
            let data = match publish.await {
                Ok(data) => data,
                Err(error) => {
                    this.update(cx, |this, cx| {
                        this.emit_error(format!("{error:#}"), cx);
                    })
                    .ok();
                    return;
                }
            };

            this.update(cx, |this, cx| {
                // Our event wins ties so a relay echo of the same event is a no-op
                let take_over = match &this.wallet {
                    Some(current) => data.created_at >= current.created_at,
                    None => true,
                };

                if take_over {
                    this.wallet = Some(data);
                    this.ensure_wallets(cx);
                    this.emit_updated(cx);
                }
            })
            .ok();
        }));
    }

    fn reset(&mut self, cx: &mut Context<Self>) {
        self.wallet = None;
        self.tokens.clear();
        self.stale.clear();
        self.quotes.clear();
        self.history.clear();
        self.redeemed.clear();
        self.pending_nutzaps.clear();
        self.redeeming.clear();
        self.redeem_attempts.clear();
        self.publish_failed = false;
        self.store = None;
        self.wallets.clear();
        self.loading = false;
        self.store_opening = false;
        self.job_running = false;
        self.job_queued.clear();
        self.notification_listener = None;
        self.tasks.clear();
        cx.notify();
    }

    fn handle_notifications(&mut self, cx: &mut Context<Self>) {
        self.notification_listener = None;

        let nostr = NostrRegistry::global(cx);
        let client = nostr.read(cx).client().clone();
        let signer = nostr.read(cx).signer().clone();

        let (tx, rx) = flume::bounded::<Event>(100);
        let consumer_signer = signer.clone();

        self.notification_listener = Some(cx.background_spawn(async move {
            const MAX_PROCESSED: usize = 1000;

            let me = signer.get_public_key_async().await.ok();
            let mut notifications = client.notifications();
            let mut processed_events = HashSet::new();

            while let Some(notification) = notifications.next().await {
                if let ClientNotification::Message { message, .. } = notification
                    && let RelayMessage::Event { event, .. } = *message
                {
                    let ours = me == Some(event.pubkey);
                    let is_cashu = matches!(
                        event.kind,
                        Kind::CashuWallet
                            | Kind::CashuWalletUnspentProof
                            | Kind::CashuWalletSpendingHistory
                    );

                    let wallet_event = ours && (is_cashu || is_token_deletion(&event));

                    let nutzap = event.kind == Kind::Custom(9321)
                        && me.is_some_and(|me| event.tags.public_keys().any(|key| key == me));

                    if !wallet_event && !nutzap {
                        continue;
                    }

                    // Prune the dedup set before it grows unbounded
                    if processed_events.len() >= MAX_PROCESSED {
                        processed_events.clear();
                    }

                    if !processed_events.insert(event.id) {
                        continue;
                    }

                    if tx.send_async(event.into_owned()).await.is_err() {
                        break;
                    }
                }
            }
        }));

        self.tasks.push(cx.spawn(async move |this, cx| {
            let me = consumer_signer.get_public_key_async().await.ok();
            while let Ok(event) = rx.recv_async().await {
                this.update(cx, |this, cx| {
                    this.absorb(event, me, cx);
                })
                .ok();
            }
        }));
    }

    fn absorb(&mut self, event: Event, me: Option<PublicKey>, cx: &mut Context<Self>) {
        match event.kind {
            Kind::CashuWallet => self.absorb_wallet(event, cx),
            Kind::CashuWalletUnspentProof => self.absorb_token(event, cx),
            Kind::CashuWalletSpendingHistory => self.absorb_history(event, cx),
            Kind::EventDeletion => self.absorb_deletion(event, me, cx),
            Kind::Custom(9321) => self.absorb_nutzap(event, cx),
            _ => {}
        }
    }

    fn absorb_wallet(&mut self, event: Event, cx: &mut Context<Self>) {
        let newer = match &self.wallet {
            Some(current) => event.created_at > current.created_at,
            None => true,
        };

        if !newer {
            return;
        }

        let nostr = NostrRegistry::global(cx);
        let signer = nostr.read(cx).signer().clone();

        let decrypted = cx.background_spawn(async move {
            let me = signer.get_public_key_async().await?;
            keys::WalletData::decrypt(&signer, &me, &event).await
        });

        self.tasks.push(cx.spawn(async move |this, cx| {
            let data = match decrypted.await {
                Ok(data) => data,
                Err(error) => {
                    this.update(cx, |this, cx| {
                        this.emit_error(format!("{error:#}"), cx);
                    })
                    .ok();
                    return;
                }
            };

            this.update(cx, |this, cx| {
                // A stale decrypt may land after a newer wallet event
                let take_over = match &this.wallet {
                    Some(current) => data.created_at >= current.created_at,
                    None => true,
                };

                if take_over {
                    this.wallet = Some(data);
                    this.ensure_wallets(cx);
                    this.emit_updated(cx);
                }
            })
            .ok();
        }));
    }

    fn absorb_token(&mut self, event: Event, cx: &mut Context<Self>) {
        if self.stale.contains(&event.id) {
            return;
        }

        let nostr = NostrRegistry::global(cx);
        let signer = nostr.read(cx).signer().clone();
        let event_id = event.id;

        let decrypted = cx.background_spawn(async move {
            let me = signer.get_public_key_async().await?;
            keys::decrypt_token_event(&signer, &me, &event).await
        });

        self.tasks.push(cx.spawn(async move |this, cx| {
            let token = match decrypted.await {
                Ok(token) => token,
                Err(error) => {
                    log::warn!("skipping unreadable token event {event_id}: {error:#}");
                    return;
                }
            };

            this.update(cx, |this, cx| {
                // The replacement may arrive before the tokens it replaces
                for old in &token.del {
                    if let Ok(old_id) = EventId::from_hex(old) {
                        this.tokens.remove(&old_id);
                        this.stale.insert(old_id);
                    }
                }
                this.tokens.insert(event_id, token);
                this.emit_updated(cx);
                this.schedule_job(ops::Job::Reconcile, cx);
            })
            .ok();
        }));
    }

    fn absorb_deletion(&mut self, event: Event, me: Option<PublicKey>, cx: &mut Context<Self>) {
        if me != Some(event.pubkey) {
            log::warn!(
                "ignoring kind 5 deletion {} from a foreign author",
                event.id
            );
            return;
        }

        let mut removed_any = false;
        for tag in event.tags.iter() {
            if tag.kind() != "e" {
                continue;
            }
            let Some(hex) = tag.content() else {
                continue;
            };
            if let Ok(id) = EventId::from_hex(hex) {
                if self.tokens.remove(&id).is_some() {
                    removed_any = true;
                }
                self.stale.insert(id);
            }
        }
        if removed_any {
            self.emit_updated(cx);
            self.schedule_job(ops::Job::Reconcile, cx);
        }
    }

    fn absorb_history(&mut self, event: Event, cx: &mut Context<Self>) {
        let event_id = event.id;
        let created_at = event.created_at;

        if self.history.iter().any(|entry| entry.id == event_id) {
            return;
        }

        let nostr = NostrRegistry::global(cx);
        let signer = nostr.read(cx).signer().clone();

        let decrypted = cx.background_spawn(async move {
            let me = signer.get_public_key_async().await?;
            keys::decrypt_history_event(&signer, &me, &event).await
        });

        self.tasks.push(cx.spawn(async move |this, cx| {
            let payload = match decrypted.await {
                Ok(payload) => payload,
                Err(error) => {
                    log::warn!("skipping unreadable history event {event_id}: {error:#}");
                    return;
                }
            };

            this.update(cx, |this, cx| {
                this.history.push(HistoryEntry {
                    id: event_id,
                    direction: payload.direction,
                    amount: payload.amount,
                    created_at,
                });
                this.history
                    .sort_by_key(|entry| std::cmp::Reverse(entry.created_at));
                this.emit_updated(cx);
            })
            .ok();
        }));
    }

    /// Claim a NIP-61 nutzap.
    fn absorb_nutzap(&mut self, event: Event, cx: &mut Context<Self>) {
        if self.redeemed.contains(&event.id) || self.redeeming.contains(&event.id) {
            return;
        }

        let Some(wallet) = self.wallet.as_ref() else {
            log::warn!("dropping nutzap {}: no wallet is set up", event.id);
            return;
        };

        if self.store.is_none() || self.wallets.is_empty() {
            self.pending_nutzaps.push(event);
            return;
        }

        let target = format!(
            "02{}",
            Keys::new(wallet.privkey.clone()).public_key().to_hex()
        );
        let nutzap = match Nutzap::from_event(&event, &target) {
            Ok(nutzap) => nutzap,
            Err(error) => {
                log::warn!("skipping nutzap {}: {error:#}", event.id);
                return;
            }
        };

        if !self.wallets.contains_key(&nutzap.mint) {
            log::warn!(
                "nutzap {} is on an unlisted mint: {}",
                event.id,
                nutzap.mint
            );
            return;
        }

        self.redeeming.insert(event.id);
        self.schedule_job(
            ops::Job::RedeemNutzap {
                mint: nutzap.mint,
                proofs: nutzap.proofs,
                nutzap_id: event.id,
                sender: event.pubkey,
            },
            cx,
        );
    }

    fn drain_pending_nutzaps(&mut self, cx: &mut Context<Self>) {
        for event in std::mem::take(&mut self.pending_nutzaps) {
            self.absorb_nutzap(event, cx);
        }
    }

    /// Open the cdk store and build a wallet per mint that is not open yet.
    fn ensure_wallets(&mut self, cx: &mut Context<Self>) {
        let Some(data) = self.wallet.clone() else {
            return;
        };

        let pending: Vec<Url> = data
            .mints
            .iter()
            .filter(|mint| {
                MintUrl::from_str(mint.as_str())
                    .map(|mint_url| !self.wallets.contains_key(&mint_url))
                    .unwrap_or(true)
            })
            .cloned()
            .collect();

        if pending.is_empty() {
            return;
        }

        if let Some(store) = self.store.clone() {
            let seed = match keys::WalletData::derive_seed(&data.privkey) {
                Ok(seed) => seed,
                Err(error) => {
                    self.emit_error(format!("{error:#}"), cx);
                    return;
                }
            };
            for mint in pending {
                let Ok(mint_url) = MintUrl::from_str(mint.as_str()) else {
                    continue;
                };

                match WalletBuilder::new()
                    .mint_url(mint_url.clone())
                    .unit(CurrencyUnit::Sat)
                    .localstore(store.clone())
                    .seed(seed)
                    .build()
                {
                    Ok(wallet) => {
                        self.wallets.insert(mint_url, wallet);
                    }
                    Err(error) => self.emit_error(format!("{error:#}"), cx),
                }
            }

            cx.notify();
            self.drain_pending_nutzaps(cx);
            self.schedule_job(ops::Job::Reconcile, cx);

            return;
        }

        if self.store_opening {
            return;
        }
        self.store_opening = true;

        let privkey = data.privkey.clone();
        let mints = data.mints.clone();

        self.tasks.push(cx.spawn(async move |this, cx| {
            match Tokio::spawn_result(cx, Self::open_store(privkey, mints)).await {
                Ok((store, wallets)) => {
                    this.update(cx, |this, cx| {
                        if this.store.is_none() {
                            this.store = Some(store);
                        }

                        for (mint_url, wallet) in wallets {
                            this.wallets.insert(mint_url, wallet);
                        }

                        this.store_opening = false;
                        this.drain_pending_nutzaps(cx);
                        cx.notify();
                        this.schedule_job(ops::Job::Reconcile, cx);
                    })
                    .ok();
                }
                Err(error) => {
                    this.update(cx, |this, cx| {
                        this.store_opening = false;
                        this.emit_error(format!("{error:#}"), cx);
                    })
                    .ok();
                }
            }
        }));
    }

    /// Queue a store-touching job.
    fn schedule_job(&mut self, job: ops::Job, cx: &mut Context<Self>) {
        if let Some(queued) = self.job_queued.last_mut()
            && matches!(queued, ops::Job::Reconcile)
            && matches!(job, ops::Job::Reconcile)
        {
            return;
        }

        if self.job_running {
            self.job_queued.push(job);
            return;
        }

        self.start_job(job, cx);
    }

    fn start_job(&mut self, job: ops::Job, cx: &mut Context<Self>) {
        if self.store.is_none() || self.wallets.is_empty() {
            self.job_queued.push(job);
            return;
        }

        let wallets = self.wallets.clone();
        let tokens = self.tokens.clone();

        let nostr = NostrRegistry::global(cx);
        let signer = nostr.read(cx).signer().clone();
        let client = nostr.read(cx).client().clone();

        let wallet_key = match self
            .wallet
            .as_ref()
            .map(|wallet| cdk::nuts::SecretKey::from_str(&wallet.privkey.to_secret_hex()))
        {
            Some(Ok(key)) => key,
            Some(Err(error)) => {
                self.emit_error(format!("invalid wallet key: {error}"), cx);
                return;
            }
            None => {
                self.emit_error("Wallet is not ready".to_string(), cx);
                return;
            }
        };

        self.job_running = true;

        let job_handle = job.clone();
        let publish_failed = self.publish_failed;

        self.tasks.push(cx.spawn(async move |this, cx| {
            let result = Tokio::spawn_result(cx, async move {
                let ctx = ops::JobCtx {
                    wallets,
                    wallet_key,
                    signer,
                    client,
                    tokens,
                    publish_failed,
                };

                job.run(ctx).await
            })
            .await;

            this.update(cx, |this, cx| {
                this.job_running = false;
                this.apply_outcome(job_handle, result, cx);

                for job in std::mem::take(&mut this.job_queued) {
                    this.schedule_job(job, cx);
                }
            })
            .ok();
        }));
    }

    fn apply_outcome(
        &mut self,
        job: ops::Job,
        result: Result<ops::Outcome, Error>,
        cx: &mut Context<Self>,
    ) {
        let outcome = match result {
            Ok(outcome) => outcome,
            Err(error) => {
                if !matches!(job, ops::Job::CreateQuote { .. }) {
                    self.publish_failed = true;
                }

                if !matches!(job, ops::Job::CreateQuote { .. } | ops::Job::Reconcile) {
                    self.schedule_job(ops::Job::Reconcile, cx);
                }

                if let ops::Job::RedeemNutzap { nutzap_id, .. } = &job {
                    self.redeeming.remove(nutzap_id);

                    let attempts = self.redeem_attempts.entry(*nutzap_id).or_default();
                    *attempts += 1;

                    if *attempts <= MAX_REDEEM_ATTEMPTS {
                        self.job_queued.push(job.clone());
                    } else {
                        log::warn!(
                            "giving up redeeming nutzap {nutzap_id} after {MAX_REDEEM_ATTEMPTS} attempts"
                        );
                    }
                }

                self.emit_error(format!("{error:#}"), cx);
                return;
            }
        };

        match outcome {
            ops::Outcome::Idle => {
                if matches!(job, ops::Job::ClaimQuote { .. }) {
                    self.emit_error("Invoice is not paid yet".to_string(), cx);
                }
            }
            ops::Outcome::Reconciled { updates } => {
                self.publish_failed = false;
                let changed = !updates.is_empty();

                for update in updates {
                    self.apply_token_update(update);
                }

                if changed {
                    self.emit_updated(cx);
                }
            }
            ops::Outcome::QuoteCreated {
                mint,
                quote_id,
                request,
                amount,
            } => {
                self.quotes.push(MintQuoteEntry {
                    quote_id,
                    request,
                    amount,
                    mint,
                });
                self.emit_updated(cx);
            }
            ops::Outcome::QuoteMinted {
                quote_id,
                update,
                amount,
            } => {
                self.quotes.retain(|quote| quote.quote_id != quote_id);
                self.apply_token_update(update);
                self.emit_updated(cx);
                log::info!("minted {amount} sats from quote {quote_id}");
            }
            ops::Outcome::NutzapSent { update, amount } => {
                self.apply_token_update(update);
                self.emit_updated(cx);
                log::info!("sent {amount} sats as a nutzap");
            }
            ops::Outcome::Received { update, amount } => {
                self.apply_token_update(update);
                self.emit_updated(cx);
                log::info!("received {amount} sats");
            }
            ops::Outcome::NutzapRedeemed {
                update,
                nutzap_id,
                amount,
            } => {
                self.redeeming.remove(&nutzap_id);
                self.redeem_attempts.remove(&nutzap_id);
                self.redeemed.insert(nutzap_id);
                self.apply_token_update(update);
                self.emit_updated(cx);
                log::info!("redeemed nutzap {nutzap_id} for {amount} sats");
            }
        }
    }

    fn apply_token_update(&mut self, update: ops::TokenUpdate) {
        for id in &update.destroyed {
            self.tokens.remove(id);
            self.stale.insert(*id);
        }
        self.tokens.insert(update.event_id, update.token);
    }

    fn apply_loaded(&mut self, state: LoadedState, cx: &mut Context<Self>) {
        self.wallet = state.wallet;
        self.tokens = state.tokens;
        self.stale = state.stale;
        self.history = state.history;
        self.redeemed = state.redeemed;
        cx.notify();
        self.ensure_wallets(cx);

        // Claimed once the wallets are open; live redemptions continue
        // through the notification listener
        for event in state.nutzaps {
            self.pending_nutzaps.push(event);
        }
    }

    fn load(&mut self, cx: &mut Context<Self>) {
        if self.loading {
            return;
        }
        self.loading = true;

        let nostr = NostrRegistry::global(cx);
        let client = nostr.read(cx).client().clone();
        let signer = nostr.read(cx).signer().clone();

        let subscribe_client = client.clone();
        let cached = cx.background_spawn(async move {
            let me = signer.get_public_key_async().await?;
            let state = LoadedState::load(me, &client, &signer).await?;
            Ok::<_, Error>((me, state))
        });

        self.tasks.push(cx.spawn(async move |this, cx| {
            let (me, state) = match cached.await {
                Ok(loaded) => loaded,
                Err(error) => {
                    this.update(cx, |this, cx| {
                        this.emit_error(format!("{error:#}"), cx);
                    })
                    .ok();
                    this.update(cx, |this, _cx| {
                        this.loading = false;
                    })
                    .ok();
                    return;
                }
            };

            this.update(cx, |this, cx| {
                this.apply_loaded(state, cx);
            })
            .ok();

            this.update(cx, |this, _cx| {
                this.loading = false;
            })
            .ok();

            let subscribed = cx.background_spawn(async move {
                subscribe_client.subscribe(Self::wallet_filter(me)).await?;
                subscribe_client.subscribe(Self::nutzap_filter(me)).await
            });

            if let Err(error) = subscribed.await {
                log::warn!("wallet subscription failed: {error:#}");
            }
        }));
    }

    fn emit_updated(&self, cx: &mut Context<Self>) {
        cx.emit(WalletEvent::Updated);
        cx.notify();
    }

    fn emit_error(&self, message: String, cx: &mut Context<Self>) {
        log::error!("wallet: {message}");
        cx.emit(WalletEvent::Error(message));
    }

    /// Open the cdk store and build a wallet per mint.
    async fn open_store(
        privkey: SecretKey,
        mints: Vec<Url>,
    ) -> Result<(Arc<WalletRedbDatabase>, HashMap<MintUrl, Wallet>), Error> {
        let dir = config_dir()
            .join("cashu")
            .join(Keys::new(privkey.clone()).public_key().to_hex());
        std::fs::create_dir_all(&dir)?;

        let store = Arc::new(WalletRedbDatabase::new(&dir.join("wallet.redb"))?);
        let seed = keys::WalletData::derive_seed(&privkey)?;

        let mut wallets = HashMap::new();
        for mint in mints {
            let mint_url = MintUrl::from_str(mint.as_str())?;
            let wallet = WalletBuilder::new()
                .mint_url(mint_url.clone())
                .unit(CurrencyUnit::Sat)
                .localstore(store.clone())
                .seed(seed)
                .build()?;
            wallets.insert(mint_url, wallet);
        }

        Ok((store, wallets))
    }

    fn wallet_filter(author: PublicKey) -> Filter {
        Filter::new().author(author).kinds([
            Kind::CashuWallet,
            Kind::CashuWalletUnspentProof,
            Kind::CashuWalletSpendingHistory,
            Kind::EventDeletion,
        ])
    }

    /// NIP-61 nutzaps: public kind 9321 events p-tagging our identity pubkey
    fn nutzap_filter(me: PublicKey) -> Filter {
        Filter::new()
            .kinds([Kind::Custom(9321)])
            .custom_tag(SingleLetterTag::LOWERCASE_P, me.to_hex())
    }
}

impl LoadedState {
    async fn load<S>(me: PublicKey, client: &Client, signer: &S) -> Result<Self, Error>
    where
        S: AsyncNip44 + ?Sized,
    {
        let events = client
            .database()
            .query(WalletRegistry::wallet_filter(me))
            .await?;
        let nutzap_events = client
            .database()
            .query(WalletRegistry::nutzap_filter(me))
            .await?;

        let mut wallet_event: Option<&Event> = None;
        let mut token_events = Vec::new();
        let mut history_events = Vec::new();
        let mut deletions = Vec::new();

        for event in &events {
            match event.kind {
                Kind::CashuWallet => {
                    let replace = match &wallet_event {
                        Some(current) => event.created_at > current.created_at,
                        None => true,
                    };

                    if replace {
                        wallet_event = Some(event);
                    }
                }
                Kind::CashuWalletUnspentProof => token_events.push(event),
                Kind::CashuWalletSpendingHistory => history_events.push(event),
                Kind::EventDeletion if is_token_deletion(event) => deletions.push(event),
                _ => {}
            }
        }

        // Nutzap events tagged redeemed in our history events are already claimed
        let mut redeemed = HashSet::new();
        for event in &history_events {
            for id in redeemed_ids(event) {
                redeemed.insert(id);
            }
        }

        let mut history = Vec::new();
        for event in &history_events {
            let payload = match keys::decrypt_history_event(signer, &me, event).await {
                Ok(payload) => payload,
                Err(error) => {
                    log::warn!("skipping unreadable history event {}: {error:#}", event.id);
                    continue;
                }
            };

            history.push(HistoryEntry {
                id: event.id,
                direction: payload.direction,
                amount: payload.amount,
                created_at: event.created_at,
            });
        }
        history.sort_by_key(|entry| std::cmp::Reverse(entry.created_at));

        let nutzaps = nutzap_events
            .into_iter()
            .filter(|event| !redeemed.contains(&event.id))
            .collect();

        let mut stale = HashSet::new();
        for event in deletions {
            for tag in event.tags.iter() {
                if tag.kind() != "e" {
                    continue;
                }

                let Some(hex) = tag.content() else {
                    continue;
                };

                if let Ok(id) = EventId::from_hex(hex) {
                    stale.insert(id);
                }
            }
        }

        let mut tokens = HashMap::new();
        for event in token_events {
            if stale.contains(&event.id) {
                continue;
            }
            let token = match keys::decrypt_token_event(signer, &me, event).await {
                Ok(token) => token,
                Err(error) => {
                    log::warn!("skipping unreadable token event {}: {error:#}", event.id);
                    continue;
                }
            };
            tokens.insert(event.id, token);
        }

        let superseded: HashSet<EventId> = tokens
            .values()
            .flat_map(|token| token.del.iter())
            .filter_map(|hex| EventId::from_hex(hex).ok())
            .collect();

        tokens.retain(|id, _| !stale.contains(id) && !superseded.contains(id));
        stale.extend(superseded);

        let wallet = match wallet_event {
            Some(event) => match keys::WalletData::decrypt(signer, &me, event).await {
                Ok(wallet) => Some(wallet),
                Err(error) => {
                    log::warn!("skipping wallet event {}: {error:#}", event.id);
                    None
                }
            },
            None => None,
        };

        Ok(Self {
            wallet,
            tokens,
            stale,
            history,
            redeemed,
            nutzaps,
        })
    }
}

fn is_token_deletion(event: &Event) -> bool {
    event.kind == Kind::EventDeletion
        && event
            .tags
            .iter()
            .any(|tag| tag.kind() == "k" && tag.content() == Some("7375"))
}

/// Nutzap event ids our history events marked as redeemed.
fn redeemed_ids(event: &Event) -> Vec<EventId> {
    event
        .tags
        .iter()
        .filter(|tag| {
            let slice = tag.as_slice();
            slice.len() >= 4 && slice[0] == "e" && slice[3] == "redeemed"
        })
        .filter_map(|tag| tag.content())
        .filter_map(|hex| EventId::from_hex(hex).ok())
        .collect()
}

/// A NIP-61 nutzap's proofs, validated against the wallet pubkey.
struct Nutzap {
    mint: MintUrl,
    proofs: Proofs,
}

impl Nutzap {
    fn from_event(event: &Event, wallet_target: &str) -> Result<Self, Error> {
        let mint = event
            .tags
            .iter()
            .find(|tag| tag.kind() == "u" && tag.content().is_some())
            .and_then(|tag| tag.content())
            .ok_or_else(|| anyhow!("nutzap is missing the mint tag"))?;
        let mint = MintUrl::from_str(mint).map_err(|error| anyhow!("invalid mint url: {error}"))?;

        let mut proofs = Vec::new();
        for tag in event.tags.iter() {
            if tag.kind() != "proof" {
                continue;
            }

            let Some(value) = tag.content() else {
                continue;
            };

            let proof: Proof = serde_json::from_str(value)
                .map_err(|error| anyhow!("invalid proof tag: {error}"))?;
            proofs.push(proof);
        }

        if proofs.is_empty() {
            return Err(anyhow!("nutzap has no proofs"));
        }

        let proofs = Proofs::from(proofs);
        let locked = proofs.iter().all(|proof| locked_to(proof, wallet_target));
        if !locked {
            return Err(anyhow!("proofs are not locked to the wallet key"));
        }

        Ok(Self { mint, proofs })
    }
}

/// NIP-61 requires nutzap proofs to be P2PK-locked to the wallet pubkey.
fn locked_to(proof: &Proof, target: &str) -> bool {
    let Ok(conditions) = SpendingConditions::try_from(&proof.secret) else {
        return false;
    };

    conditions
        .pubkeys()
        .is_some_and(|keys| keys.iter().any(|key| key.to_string() == target))
}
