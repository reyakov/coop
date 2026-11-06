#![cfg(not(target_arch = "wasm32"))]

mod keys;
mod ops;

use std::collections::{HashMap, HashSet};
use std::str::FromStr;
use std::sync::Arc;

use anyhow::Error;
use cdk::mint_url::MintUrl;
use cdk::nuts::CurrencyUnit;
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
    TokenCreated(String),
    Error(String),
}

impl EventEmitter<WalletEvent> for WalletRegistry {}

/// A pending lightning invoice waiting to be paid and claimed.
#[derive(Debug, Clone)]
pub struct MintQuoteEntry {
    pub quote_id: String,
    pub request: String,
    pub amount: u64,
    pub mint: Url,
}

pub struct WalletRegistry {
    wallet: Option<keys::WalletData>,
    tokens: HashMap<EventId, TokenEvent>,
    /// Token events killed by a kind 5 delete or superseded by a `del` chain
    stale: HashSet<EventId>,
    quotes: Vec<MintQuoteEntry>,
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

    /// Create a lightning invoice that funds the wallet once paid.
    pub fn create_mint_quote(&mut self, amount: u64, cx: &mut Context<Self>) {
        let Some(mint) = self
            .wallet
            .as_ref()
            .and_then(|wallet| wallet.mints.first().cloned())
        else {
            self.emit_error("No wallet is set up".to_string(), cx);
            return;
        };

        let Ok(mint_url) = MintUrl::from_str(mint.as_str()) else {
            self.emit_error("Invalid mint URL".to_string(), cx);
            return;
        };

        let Some(wallet) = self.wallets.get(&mint_url).cloned() else {
            self.emit_error("Wallet is not ready".to_string(), cx);
            return;
        };

        let created = cx.background_spawn(async move {
            let quote = wallet
                .mint_quote(
                    cdk::nuts::PaymentMethod::Known(cdk::nuts::nut00::KnownMethod::Bolt11),
                    Some(cdk::Amount::from(amount)),
                    None,
                    None,
                )
                .await?;

            Ok::<_, Error>((quote.id, quote.request))
        });

        self.tasks.push(cx.spawn(async move |this, cx| {
            let (quote_id, request) = match created.await {
                Ok(quote) => quote,
                Err(error) => {
                    this.update(cx, |this, cx| {
                        this.emit_error(format!("{error:#}"), cx);
                    })
                    .ok();
                    return;
                }
            };

            this.update(cx, |this, cx| {
                this.quotes.push(MintQuoteEntry {
                    quote_id,
                    request,
                    amount,
                    mint: mint.clone(),
                });
                this.emit_updated(cx);
            })
            .ok();
        }));
    }

    /// Check a mint quote with the mint and claim it once it is paid.
    pub fn claim_mint_quote(&mut self, quote_id: String, cx: &mut Context<Self>) {
        let Some(entry) = self.quotes.iter().find(|quote| quote.quote_id == quote_id) else {
            self.emit_error("Unknown mint quote".to_string(), cx);
            return;
        };

        let Ok(mint_url) = MintUrl::from_str(entry.mint.as_str()) else {
            self.emit_error("Invalid mint URL".to_string(), cx);
            return;
        };

        self.schedule_job(
            ops::Job::ClaimQuote {
                mint: mint_url,
                quote_id,
            },
            cx,
        );
    }

    /// Pay a lightning invoice from the wallet.
    pub fn melt(&mut self, invoice: String, cx: &mut Context<Self>) {
        let Some(mint) = self
            .wallet
            .as_ref()
            .and_then(|wallet| wallet.mints.first().cloned())
        else {
            self.emit_error("No wallet is set up".to_string(), cx);
            return;
        };

        let Ok(mint_url) = MintUrl::from_str(mint.as_str()) else {
            self.emit_error("Invalid mint URL".to_string(), cx);
            return;
        };

        self.schedule_job(
            ops::Job::Melt {
                mint: mint_url,
                invoice,
            },
            cx,
        );
    }

    /// Send ecash as a token string the recipient can paste anywhere.
    pub fn send_token(&mut self, amount: u64, cx: &mut Context<Self>) {
        let Some(mint) = self
            .wallet
            .as_ref()
            .and_then(|wallet| wallet.mints.first().cloned())
        else {
            self.emit_error("No wallet is set up".to_string(), cx);
            return;
        };

        let Ok(mint_url) = MintUrl::from_str(mint.as_str()) else {
            self.emit_error("Invalid mint URL".to_string(), cx);
            return;
        };

        self.schedule_job(
            ops::Job::SendToken {
                mint: mint_url,
                amount,
            },
            cx,
        );
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

        let (privkey, mints) = match self
            .wallet
            .as_ref()
            .map(|wallet| (wallet.privkey.clone(), wallet.mints.clone()))
        {
            Some((privkey, mut mints)) => {
                if mints.contains(&mint) {
                    return;
                }
                mints.push(mint.clone());
                (privkey, mints)
            }
            None => (keys::generate_privkey(), vec![mint.clone()]),
        };

        let nostr = NostrRegistry::global(cx);
        let client = nostr.read(cx).client().clone();
        let signer = nostr.read(cx).signer().clone();

        let publish = cx.background_spawn(async move {
            let me = signer.get_public_key_async().await?;
            let event = keys::build_wallet_event(&signer, &me, &privkey, &mints).await?;
            client.send_event(&event).await?;
            client.database().save_event(&event).await?;

            Ok::<_, Error>(keys::WalletData {
                event_id: event.id,
                created_at: event.created_at,
                privkey,
                mints,
            })
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

        self.notification_listener = Some(cx.background_spawn(async move {
            let me = signer.get_public_key_async().await.ok();
            let mut notifications = client.notifications();

            const MAX_PROCESSED: usize = 1000;
            let mut processed_events = HashSet::new();

            while let Some(notification) = notifications.next().await {
                if let ClientNotification::Message { message, .. } = notification
                    && let RelayMessage::Event { event, .. } = *message
                    && me == Some(event.pubkey)
                    && (matches!(
                        event.kind,
                        Kind::CashuWallet
                            | Kind::CashuWalletUnspentProof
                            | Kind::CashuWalletSpendingHistory
                    ) || is_token_deletion(&event))
                {
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
            while let Ok(event) = rx.recv_async().await {
                this.update(cx, |this, cx| {
                    this.absorb(event, cx);
                })
                .ok();
            }
        }));
    }

    fn absorb(&mut self, event: Event, cx: &mut Context<Self>) {
        match event.kind {
            Kind::CashuWallet => self.absorb_wallet(event, cx),
            Kind::CashuWalletUnspentProof => self.absorb_token(event, cx),
            Kind::EventDeletion => self.absorb_deletion(event, cx),
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
            keys::decrypt_wallet_event(&signer, &me, &event).await
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
                this.wallet = Some(data);
                this.ensure_wallets(cx);
                this.emit_updated(cx);
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
                    this.update(cx, |this, cx| {
                        this.emit_error(format!("{error:#}"), cx);
                    })
                    .ok();
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

    fn absorb_deletion(&mut self, event: Event, cx: &mut Context<Self>) {
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
            let seed = match keys::derive_seed(&data.privkey) {
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
            match Tokio::spawn_result(cx, open_store(privkey, mints)).await {
                Ok((store, wallets)) => {
                    this.update(cx, |this, cx| {
                        if this.store.is_none() {
                            this.store = Some(store);
                        }

                        for (mint_url, wallet) in wallets {
                            this.wallets.insert(mint_url, wallet);
                        }

                        this.store_opening = false;
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
            self.emit_error("Wallet is not ready".to_string(), cx);
            return;
        }

        let wallets = self.wallets.clone();
        let tokens = self.tokens.clone();

        let signer = NostrRegistry::global(cx).read(cx).signer().clone();
        let client = NostrRegistry::global(cx).read(cx).client().clone();
        self.job_running = true;

        self.tasks.push(cx.spawn(async move |this, cx| {
            let result = Tokio::spawn_result(cx, async move {
                let ctx = ops::JobCtx {
                    wallets,
                    signer,
                    client,
                    tokens,
                };

                ops::run_job(job, ctx).await
            })
            .await;

            this.update(cx, |this, cx| {
                this.job_running = false;
                this.apply_outcome(result, cx);

                for job in std::mem::take(&mut this.job_queued) {
                    this.schedule_job(job, cx);
                }
            })
            .ok();
        }));
    }

    fn apply_outcome(&mut self, result: Result<ops::Outcome, Error>, cx: &mut Context<Self>) {
        let outcome = match result {
            Ok(outcome) => outcome,
            Err(error) => {
                self.emit_error(format!("{error:#}"), cx);
                return;
            }
        };

        match outcome {
            ops::Outcome::Idle => {}
            ops::Outcome::Reconciled { updates } => {
                let changed = !updates.is_empty();

                for update in updates {
                    self.apply_token_update(update);
                }

                if changed {
                    self.emit_updated(cx);
                }
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
            ops::Outcome::Melted { update, amount } => {
                self.apply_token_update(update);
                self.emit_updated(cx);
                log::info!("melted {amount} sats");
            }
            ops::Outcome::Sent { update, token } => {
                self.apply_token_update(update);
                cx.emit(WalletEvent::TokenCreated(token));
                self.emit_updated(cx);
            }
            ops::Outcome::Received { update, amount } => {
                self.apply_token_update(update);
                self.emit_updated(cx);
                log::info!("received {amount} sats");
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
        cx.notify();
        self.ensure_wallets(cx);
        self.schedule_job(ops::Job::Reconcile, cx);
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
            let state = load_inner(&me, &client, &signer).await?;
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

            // Subscribe only after the cache snapshot; from then on the state
            // advances exclusively through the notification listener.
            let subscribed =
                cx.background_spawn(
                    async move { subscribe_client.subscribe(wallet_filter(me)).await },
                );
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
}

async fn load_inner<S>(me: &PublicKey, client: &Client, signer: &S) -> Result<LoadedState, Error>
where
    S: AsyncNip44 + ?Sized,
{
    let events = client.database().query(wallet_filter(*me)).await?;

    let mut wallet_event: Option<&Event> = None;
    let mut token_events = Vec::new();
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
            Kind::EventDeletion if is_token_deletion(event) => deletions.push(event),
            _ => {}
        }
    }

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
        let token = match keys::decrypt_token_event(signer, me, event).await {
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
        Some(event) => match keys::decrypt_wallet_event(signer, me, event).await {
            Ok(wallet) => Some(wallet),
            Err(error) => {
                log::warn!("skipping wallet event {}: {error:#}", event.id);
                None
            }
        },
        None => None,
    };

    Ok(LoadedState {
        wallet,
        tokens,
        stale,
    })
}

async fn open_store(
    privkey: SecretKey,
    mints: Vec<Url>,
) -> Result<(Arc<WalletRedbDatabase>, HashMap<MintUrl, Wallet>), Error> {
    let dir = config_dir()
        .join("cashu")
        .join(Keys::new(privkey.clone()).public_key().to_hex());
    std::fs::create_dir_all(&dir)?;

    let store = Arc::new(WalletRedbDatabase::new(&dir.join("wallet.redb"))?);
    let seed = keys::derive_seed(&privkey)?;

    let mut wallets = HashMap::new();
    for mint in &mints {
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

fn is_token_deletion(event: &Event) -> bool {
    event.kind == Kind::EventDeletion
        && event
            .tags
            .iter()
            .any(|tag| tag.kind() == "k" && tag.content() == Some("7375"))
}
