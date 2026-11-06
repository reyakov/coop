#![cfg(not(target_arch = "wasm32"))]

mod keys;

use std::collections::{HashMap, HashSet};
use std::str::FromStr;
use std::sync::Arc;

use anyhow::Error;
use cdk::Amount;
use cdk::cdk_database::WalletDatabase;
use cdk::mint_url::MintUrl;
use cdk::nuts::{CurrencyUnit, Id, Proof, State};
use cdk::secret::Secret;
use cdk::wallet::types::ProofInfo;
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

pub struct WalletRegistry {
    wallet: Option<keys::WalletData>,
    tokens: HashMap<EventId, TokenEvent>,
    /// Token events killed by a kind 5 delete or superseded by a `del` chain
    stale: HashSet<EventId>,
    store: Option<Arc<WalletRedbDatabase>>,
    wallets: HashMap<MintUrl, Wallet>,
    loading: bool,
    store_opening: bool,
    importing: bool,
    import_queued: bool,
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
            store: None,
            wallets: HashMap::new(),
            loading: false,
            store_opening: false,
            importing: false,
            import_queued: false,
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
        self.store = None;
        self.wallets.clear();
        self.loading = false;
        self.store_opening = false;
        self.importing = false;
        self.import_queued = false;
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
                this.schedule_import(cx);
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
            self.schedule_import(cx);
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
            self.schedule_import(cx);

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
                        this.schedule_import(cx);
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

    /// Mirror the active token events into the cdk store.
    fn schedule_import(&mut self, cx: &mut Context<Self>) {
        if self.importing {
            self.import_queued = true;
            return;
        }

        let Some(store) = self.store.clone() else {
            return;
        };

        if self.wallets.is_empty() {
            return;
        }

        let mut active: Vec<(MintUrl, Vec<Proof>)> = Vec::new();
        for token in self.tokens.values() {
            let Ok(mint_url) = MintUrl::from_str(token.mint.as_str()) else {
                continue;
            };

            let mut proofs = Vec::new();
            for proof in &token.proofs {
                let Ok(keyset_id) = Id::from_str(&proof.id) else {
                    continue;
                };

                let Ok(c) = cdk::nuts::PublicKey::from_str(&proof.c) else {
                    continue;
                };

                proofs.push(Proof::new(
                    Amount::from(proof.amount),
                    keyset_id,
                    Secret::new(proof.secret.clone()),
                    c,
                ));
            }

            active.push((mint_url, proofs));
        }

        self.importing = true;

        self.tasks.push(cx.spawn(async move |this, cx| {
            let result = Tokio::spawn_result(cx, async move {
                let mut added = Vec::new();
                let mut ys = HashSet::new();

                for (mint_url, proofs) in active {
                    for proof in proofs {
                        let info = match ProofInfo::new(
                            proof,
                            mint_url.clone(),
                            State::Unspent,
                            CurrencyUnit::Sat,
                        ) {
                            Ok(info) => info,
                            Err(error) => {
                                log::warn!("skipping proof: {error:#}");
                                continue;
                            }
                        };
                        if ys.insert(info.y) {
                            added.push(info);
                        }
                    }
                }

                let existing = store
                    .get_proofs(
                        None,
                        Some(CurrencyUnit::Sat),
                        Some(vec![State::Unspent]),
                        None,
                    )
                    .await?;

                let removed = existing
                    .iter()
                    .filter(|info| !ys.contains(&info.y))
                    .map(|info| info.y)
                    .collect();

                store.update_proofs(added, removed).await?;

                Ok(())
            })
            .await;

            this.update(cx, |this, cx| {
                this.importing = false;

                if this.import_queued {
                    this.import_queued = false;
                    this.schedule_import(cx);
                }

                if let Err(error) = result {
                    this.emit_error(format!("{error:#}"), cx);
                }
            })
            .ok();
        }));
    }

    fn apply_loaded(&mut self, state: LoadedState, cx: &mut Context<Self>) {
        self.wallet = state.wallet;
        self.tokens = state.tokens;
        self.stale = state.stale;
        cx.notify();
        self.ensure_wallets(cx);
        self.schedule_import(cx);
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

    let recovery = wallets.clone();
    tokio::spawn(async move {
        for (_, wallet) in recovery {
            if let Err(error) = wallet.recover_incomplete_sagas().await {
                log::warn!("saga recovery failed: {error:#}");
            }
            if let Err(error) = wallet.mint_unissued_quotes().await {
                log::warn!("minting unissued quotes failed: {error:#}");
            }
        }
    });

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
