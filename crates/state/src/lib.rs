use std::collections::HashMap;

use anyhow::{Error, anyhow};
#[cfg(not(target_arch = "wasm32"))]
use browser_signer_proxy::prelude::*;
use common::config_dir;
use gpui::{App, AppContext, Context, Entity, EventEmitter, Global, Task, TaskExt};
use gpui_tokio::Tokio;
use instant::Duration;
use nostr_connect::prelude::*;
use nostr_gossip_memory::prelude::*;
#[cfg(not(target_arch = "wasm32"))]
use nostr_lmdb::prelude::*;
#[cfg(target_arch = "wasm32")]
use nostr_memory::prelude::*;
use nostr_sdk::prelude::*;

mod blossom;
mod constants;
mod file;
mod nip05;
mod nip4e;
mod signer;

pub use blossom::*;
pub use constants::*;
pub use file::*;
pub use nip4e::*;
pub use nip05::*;
pub use signer::{CoopAuthUrlHandler, UniversalSigner};

pub fn init(cx: &mut App, cli_key: Option<SecretKey>) {
    #[cfg(not(target_arch = "wasm32"))]
    rustls::crypto::aws_lc_rs::default_provider()
        .install_default()
        .ok();

    // Initialize the tokio runtime
    #[cfg(not(target_arch = "wasm32"))]
    gpui_tokio::init(cx);

    NostrRegistry::set_global(cx.new(|cx| NostrRegistry::new(cx, cli_key)), cx);
}

struct GlobalNostrRegistry(Entity<NostrRegistry>);

impl Global for GlobalNostrRegistry {}

/// Signer event.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub enum StateEvent {
    /// User has no signer
    NoSigner,
    /// The signer has changed
    SignerChanged,
    /// A stored credential needs a passphrase to unlock
    PassphraseRequired,
    /// An error occurred
    Error(String),
}

impl StateEvent {
    pub fn signer_changed(&self) -> bool {
        matches!(self, StateEvent::SignerChanged)
    }

    pub(crate) fn error<T>(error: T) -> Self
    where
        T: Into<String>,
    {
        Self::Error(error.into())
    }
}

/// Nostr Registry
#[derive(Debug)]
pub struct NostrRegistry {
    /// Nostr client
    client: Client,
    /// Universal signer
    signer: UniversalSigner,
    /// Current user's public key
    current_user: Option<PublicKey>,
    /// Whether the stored credential needs a passphrase to unlock
    passphrase_required: bool,
    /// Whether the initial credential check has concluded
    ready: bool,
    /// Tasks for asynchronous operations
    tasks: Vec<Task<Result<(), Error>>>,
}

impl EventEmitter<StateEvent> for NostrRegistry {}

impl NostrRegistry {
    /// Retrieve the global nostr state
    pub fn global(cx: &App) -> Entity<Self> {
        cx.global::<GlobalNostrRegistry>().0.clone()
    }

    /// Set the global nostr instance
    fn set_global(state: Entity<Self>, cx: &mut App) {
        cx.set_global(GlobalNostrRegistry(state));
    }

    /// Create a new nostr instance
    fn new(cx: &mut Context<Self>, cli_key: Option<SecretKey>) -> Self {
        let entity = cx.entity().downgrade();
        let signer = UniversalSigner::new(Keys::generate());
        let authenticator = SignerAuthenticator::new(signer.clone());

        // Construct the nostr lmdb instance
        #[cfg(not(target_arch = "wasm32"))]
        let database = cx.foreground_executor().block_on(async move {
            NostrLmdb::open(config_dir().join("nostr"))
                .await
                .expect("Failed to initialize database")
        });

        #[cfg(target_arch = "wasm32")]
        let database = MemoryDatabase::unbounded();

        // Construct the nostr client
        let client = ClientBuilder::default()
            .database(database)
            .authenticator(authenticator)
            .gossip(NostrGossipMemory::unbounded())
            .gossip_config(GossipConfig::default().no_background_refresh())
            .connect_timeout(Duration::from_secs(10))
            .verify_subscriptions(true)
            .ban_relay_on_mismatch(true)
            .sleep_when_idle(SleepWhenIdle::Enabled {
                timeout: Duration::from_secs(600),
            })
            .build();

        // Connect to bootstrap relays once the registry has been returned to the app
        cx.defer(move |cx| {
            let _ = entity.update(cx, |this, cx| {
                this.connect_bootstrap_relays(cx);

                if cfg!(target_arch = "wasm32") {
                    this.mark_ready(cx);
                    cx.emit(StateEvent::NoSigner);
                } else if let Some(secret) = cli_key {
                    // Use CLI-provided key -- same path as restore_session
                    let keys = Keys::new(secret);
                    this.set_signer(keys, cx);
                } else {
                    this.restore_session(cx);
                }
            });
        });

        Self {
            client,
            signer,
            current_user: None,
            passphrase_required: false,
            ready: false,
            tasks: vec![],
        }
    }

    /// Get the nostr client
    pub fn client(&self) -> Client {
        self.client.clone()
    }

    /// Get the current signer
    pub fn signer(&self) -> UniversalSigner {
        self.signer.clone()
    }

    /// Get the current user's public key
    pub fn current_user(&self) -> Option<PublicKey> {
        self.current_user
    }

    fn mark_ready(&mut self, cx: &mut Context<Self>) {
        if self.ready {
            return;
        }

        self.ready = true;
        cx.notify();
    }

    /// Update the signer
    fn set_signer<T>(&mut self, new_signer: T, cx: &mut Context<Self>)
    where
        T: AsyncGetPublicKey + AsyncSignEvent + AsyncNip44 + 'static,
        <T as AsyncGetPublicKey>::Error: std::error::Error + Send + Sync + 'static,
        <T as AsyncSignEvent>::Error: std::error::Error + Send + Sync + 'static,
        <T as AsyncNip44>::Error: std::error::Error + Send + Sync + 'static,
    {
        let task = cx.spawn(async move |this, cx| {
            match new_signer.get_public_key_async().await {
                Ok(public_key) => {
                    this.update(cx, |this, cx| {
                        this.signer.swap_inner(new_signer);
                        this.current_user = Some(public_key);
                        this.passphrase_required = false;
                        this.mark_ready(cx);
                        cx.emit(StateEvent::SignerChanged);
                        cx.notify();
                    })?;
                }
                Err(e) => {
                    this.update(cx, |_this, cx| {
                        cx.emit(StateEvent::error(e.to_string()));
                    })?;
                }
            };

            Ok(())
        });
        self.tasks.push(task);
    }

    /// Connect to the bootstrapping relays
    fn connect_bootstrap_relays(&mut self, cx: &mut Context<Self>) {
        let client = self.client();

        let task: Task<Result<(), Error>> = cx.background_spawn(async move {
            // Add indexer relay to the relay pool
            for url in INDEXER_RELAYS.into_iter() {
                client
                    .add_relay(url)
                    .capabilities(RelayCapabilities::DISCOVERY)
                    .await?;
            }

            // Add bootstrap relay to the relay pool
            for url in BOOTSTRAP_RELAYS.into_iter() {
                client.add_relay(url).await?;
            }

            // Connect to all added relays
            client.connect().await;

            Ok(())
        });

        self.tasks.push(cx.spawn(async move |this, cx| {
            if let Err(e) = task.await {
                this.update(cx, |_this, cx| {
                    cx.emit(StateEvent::error(e.to_string()));
                })?;
            }
            Ok(())
        }));
    }

    /// Restore the signer from the stored credential
    fn restore_session(&mut self, cx: &mut Context<Self>) {
        let user_keyring = cx.read_credentials(USER_KEYRING);

        self.tasks.push(cx.spawn(async move |this, cx| {
            let result = async {
                match user_keyring.await {
                    Ok(Some((_username, secret))) => {
                        let content = String::from_utf8(secret)?;

                        if content.starts_with("nsec1") {
                            let keys = Keys::new(SecretKey::parse(&content)?);
                            this.update(cx, |this, cx| {
                                this.set_signer(keys, cx);
                            })?;
                        } else if content.starts_with("bunker://") {
                            // The master key used for Nostr Connect is embedded in
                            // the stored URI so reconnects reuse the same local key
                            let (base, keys) = extract_master_key(&content);
                            let uri = NostrConnectUri::parse(base)?;

                            // Construct the nostr connect signer
                            let mut signer = NostrConnect::new(
                                uri,
                                keys,
                                Duration::from_secs(NOSTR_CONNECT_TIMEOUT),
                                None,
                            )?;

                            // Handle auth url with the default browser
                            signer.auth_url_handler(CoopAuthUrlHandler);

                            this.update(cx, |this, cx| {
                                this.set_signer(signer, cx);
                            })?;
                        } else if content.starts_with("ncryptsec1") {
                            this.update(cx, |this, cx| {
                                this.passphrase_required = true;
                                this.mark_ready(cx);
                                cx.emit(StateEvent::PassphraseRequired);
                            })?;
                        } else if content == "proxy" {
                            #[cfg(not(target_arch = "wasm32"))]
                            this.update(cx, |this, cx| {
                                this.mark_ready(cx);
                                this.connect_proxy(cx);
                            })?;
                        } else {
                            this.update(cx, |this, cx| {
                                this.mark_ready(cx);
                                cx.emit(StateEvent::NoSigner);
                            })?;
                        }
                    }
                    _ => {
                        this.update(cx, |this, cx| {
                            this.mark_ready(cx);
                            cx.emit(StateEvent::NoSigner);
                        })?;
                    }
                }

                Ok::<(), Error>(())
            }
            .await;

            if let Err(e) = result {
                this.update(cx, |this, cx| {
                    this.mark_ready(cx);
                    cx.emit(StateEvent::error(e.to_string()));
                    cx.emit(StateEvent::NoSigner);
                })?;
            }

            Ok(())
        }));
    }

    /// Unlock the stored encrypted key with its passphrase
    pub fn restore_with_passphrase(
        &mut self,
        password: &str,
        cx: &mut Context<Self>,
    ) -> Task<Result<PublicKey, Error>> {
        let password = password.to_owned();
        let user_keyring = cx.read_credentials(USER_KEYRING);

        cx.spawn(async move |this, cx| {
            let content = user_keyring
                .await?
                .map(|(_username, secret)| String::from_utf8(secret))
                .transpose()?
                .ok_or_else(|| anyhow!("no stored credential; nothing to unlock"))?;

            if !content.starts_with("ncryptsec1") {
                return Err(anyhow!("stored credential is not passphrase-encrypted"));
            }

            let decrypt_task = cx.background_spawn(async move {
                let encrypted = EncryptedSecretKey::from_bech32(&content)?;
                let secret = encrypted.decrypt(&password)?;
                Ok::<_, Error>(Keys::new(secret))
            });

            let keys = decrypt_task.await?;
            let public_key = keys.public_key();

            this.update(cx, |this, cx| {
                this.set_signer(keys, cx);
            })?;

            Ok(public_key)
        })
    }

    /// Import an `nsec1...` secret key, storing it NIP-49 encrypted with `password`
    pub fn import_nsec(
        &mut self,
        nsec: &str,
        password: &str,
        cx: &mut Context<Self>,
    ) -> Task<Result<PublicKey, Error>> {
        let keys = match SecretKey::parse(nsec.trim()) {
            Ok(secret) => Keys::new(secret),
            Err(e) => return Task::ready(Err(anyhow!(e))),
        };

        if password.is_empty() {
            return Task::ready(Err(anyhow!("Passphrase must not be empty")));
        }

        let password = password.to_owned();

        cx.spawn(async move |this, cx| {
            let job = cx.background_spawn(async move {
                let encrypted =
                    EncryptedSecretKey::new(keys.secret_key(), &password, 16, KeySecurity::Medium)?;
                let ncryptsec = encrypted.to_bech32()?;
                Ok::<_, Error>((keys, ncryptsec))
            });

            let (keys, ncryptsec) = job.await?;
            let public_key = keys.public_key();

            let write = cx.update(|cx| {
                cx.write_credentials(USER_KEYRING, &public_key.to_hex(), ncryptsec.as_bytes())
            });
            write.await?;

            this.update(cx, |this, cx| {
                this.set_signer(keys, cx);
            })?;

            Ok(public_key)
        })
    }

    /// Import an NIP-49 encrypted secret key (`ncryptsec1...`), decrypting it with `password`
    pub fn import_ncryptsec(
        &mut self,
        ncryptsec: &str,
        password: &str,
        cx: &mut Context<Self>,
    ) -> Task<Result<PublicKey, Error>> {
        let ncryptsec = ncryptsec.trim().to_owned();

        if password.is_empty() {
            return Task::ready(Err(anyhow!("Passphrase must not be empty")));
        }

        let password = password.to_owned();

        cx.spawn(async move |this, cx| {
            let stored = ncryptsec.clone();
            let decrypt_task = cx.background_spawn(async move {
                let encrypted = EncryptedSecretKey::from_bech32(&ncryptsec)?;
                let secret = encrypted.decrypt(&password)?;
                Ok::<_, Error>(Keys::new(secret))
            });

            let keys = decrypt_task.await?;
            let public_key = keys.public_key();

            let write = cx.update(|cx| {
                cx.write_credentials(USER_KEYRING, &public_key.to_hex(), stored.as_bytes())
            });
            write.await?;

            this.update(cx, |this, cx| {
                this.set_signer(keys, cx);
            })?;

            Ok(public_key)
        })
    }

    /// Import a `bunker://...` URI (NIP-46), connecting to the remote signer
    pub fn import_bunker(
        &mut self,
        uri: &str,
        cx: &mut Context<Self>,
    ) -> Task<Result<PublicKey, Error>> {
        let uri_string = uri.trim().to_owned();

        let connect_uri = match NostrConnectUri::parse(&uri_string) {
            Ok(uri) => uri,
            Err(e) => return Task::ready(Err(anyhow!(e))),
        };

        let keys = Keys::generate();
        let credential = with_master_key(&uri_string, &keys);
        let write = cx.write_credentials(USER_KEYRING, "bunker", credential.as_bytes());

        cx.spawn(async move |this, cx| {
            // Construct the nostr connect signer
            let mut signer = NostrConnect::new(
                connect_uri,
                keys,
                Duration::from_secs(NOSTR_CONNECT_TIMEOUT),
                None,
            )?;

            // Handle auth url with the default browser
            signer.auth_url_handler(CoopAuthUrlHandler);

            // Verify the bunker responds before persisting the credential
            let public_key = signer.get_public_key_async().await?;
            write.await?;

            this.update(cx, |this, cx| {
                this.set_signer(signer, cx);
            })?;

            Ok(public_key)
        })
    }

    /// Sign out and remove the stored credential
    pub fn sign_out(&mut self, cx: &mut Context<Self>) {
        self.current_user = None;
        self.passphrase_required = false;

        cx.delete_credentials(USER_KEYRING).detach_and_log_err(cx);
        cx.emit(StateEvent::SignerChanged);
        cx.emit(StateEvent::NoSigner);
        cx.notify();
    }

    /// Start the browser proxy
    #[cfg(not(target_arch = "wasm32"))]
    pub fn connect_proxy(&mut self, cx: &mut Context<Self>) {
        let proxy = BrowserSignerProxy::new(BrowserSignerProxyOptions::default());
        let (tx, rx) = flume::bounded::<String>(1);

        self.tasks.push(Tokio::spawn_result(cx, {
            let proxy = proxy.clone();
            async move {
                // Start the proxy and get the web url
                proxy.start().await?;
                // Notify GPUI
                let url = proxy.url();
                tx.send(url).ok();
                Ok(())
            }
        }));

        self.tasks.push(Tokio::spawn_result(cx, {
            let proxy = proxy.clone();
            async move {
                loop {
                    if proxy.is_session_active() {
                        break;
                    }
                    smol::Timer::after(Duration::from_secs(1)).await;
                }
                Ok(())
            }
        }));

        self.tasks.push(cx.spawn({
            let proxy = proxy.clone();
            async move |this, cx| {
                while let Ok(url) = rx.recv_async().await {
                    this.update(cx, |this, cx| {
                        let save = cx.write_credentials(USER_KEYRING, "proxy", b"proxy");
                        cx.background_spawn(async move { save.await.ok() }).detach();
                        cx.open_url(&url);
                        this.set_signer(proxy.clone(), cx);
                    })?;
                }
                Ok(())
            }
        }));

        // Monitor the session, if the browser disconnects, notify user to reconnect
        self.tasks.push(cx.spawn({
            let proxy = proxy.clone();
            let executor = cx.background_executor().clone();
            async move |this, cx| {
                // Wait for the signer to be confirmed (timeout is 30s)
                executor.timer(Duration::from_secs(30)).await;

                loop {
                    executor.timer(Duration::from_secs(5)).await;
                    if !proxy.is_session_active() {
                        _ = this.update(cx, |this, cx| {
                            // Only notify if this proxy is still the active signer
                            if this.current_user.is_some() {
                                this.signer.swap_inner(Keys::generate());
                                this.current_user = None;
                                cx.emit(StateEvent::NoSigner);
                                cx.notify();
                            }
                        });
                        break;
                    }
                }

                Ok(())
            }
        }));
    }

    /// Get the public key of a NIP-05 address
    pub fn query_address(&self, addr: Nip05Address, cx: &App) -> Task<Result<PublicKey, Error>> {
        let client = self.client();
        let http_client = cx.http_client();

        cx.background_spawn(async move {
            let profile = addr.profile(&http_client).await?;
            let public_key = profile.public_key;

            let opts = SubscribeAutoCloseOptions::default()
                .exit_policy(ReqExitPolicy::ExitOnEOSE)
                .timeout(Some(Duration::from_secs(3)));

            // Construct the filter for the metadata event
            let filter = Filter::new()
                .kind(Kind::Metadata)
                .author(public_key)
                .limit(1);

            // Construct target for subscription
            let target: HashMap<&str, Vec<Filter>> = BOOTSTRAP_RELAYS
                .into_iter()
                .map(|relay| (relay, vec![filter.clone()]))
                .collect();

            client.subscribe(target).close_on(opts).await?;

            Ok(public_key)
        })
    }

    /// Perform a NIP-50 global search for user profiles based on a given query
    pub fn search(&self, query: &str, cx: &App) -> Task<Result<Vec<PublicKey>, Error>> {
        let client = self.client();
        let query = query.to_string();

        // Get the address task if the query is a valid NIP-05 address
        let address_task = if let Ok(addr) = Nip05Address::parse(&query) {
            Some(self.query_address(addr, cx))
        } else {
            None
        };

        cx.background_spawn(async move {
            let mut results: Vec<PublicKey> = Vec::with_capacity(FIND_LIMIT);

            // Return early if the query is a valid NIP-05 address
            if let Some(task) = address_task
                && let Ok(public_key) = task.await
            {
                results.push(public_key);
                return Ok(results);
            }

            // Add search relay to the relay pool
            for url in SEARCH_RELAYS.into_iter() {
                if client.relay(url).await.is_ok() {
                    client
                        .add_relay(url)
                        .capabilities(RelayCapabilities::READ)
                        .await?;
                } else {
                    return Err(anyhow!("Failed to add search relay: {}", url));
                }
            }

            // Return early if the query is a valid public key
            if let Ok(public_key) = PublicKey::parse(&query) {
                results.push(public_key);
                return Ok(results);
            }

            // Construct the filter for the search query
            let filter = Filter::new()
                .search(query.to_lowercase())
                .kind(Kind::Metadata)
                .limit(FIND_LIMIT);

            // Construct target for subscription
            let target: HashMap<&str, Vec<Filter>> = SEARCH_RELAYS
                .into_iter()
                .map(|relay| (relay, vec![filter.clone()]))
                .collect();

            // Stream events from the search relays
            let mut stream = client
                .stream_events(target)
                .timeout(Duration::from_secs(TIMEOUT))
                .await?;

            // Collect the results
            while let Some((_url, res)) = stream.next().await {
                if let Ok(event) = res {
                    results.push(event.pubkey);
                }
            }

            if results.is_empty() {
                return Err(anyhow!("No results for query {query}"));
            }

            Ok(results)
        })
    }
}

// Extract the base URI and the master key from a bunker credential with `master=<nsec>`
fn extract_master_key(credential: &str) -> (&str, Keys) {
    match credential.split_once("master=") {
        Some((base, nsec)) => {
            let keys = SecretKey::parse(nsec)
                .map(Keys::new)
                .unwrap_or_else(|_| Keys::generate());
            (base.trim_end_matches(['?', '&']), keys)
        }
        None => (credential, Keys::generate()),
    }
}

// Embed the master key in a bunker URI so reconnects reuse the same local key
fn with_master_key(uri: &str, keys: &Keys) -> String {
    let separator = if uri.contains('?') { '&' } else { '?' };
    let nsec = keys.secret_key().to_bech32().expect("infallible");
    format!("{uri}{separator}master={nsec}")
}
