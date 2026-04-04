use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context as AnyhowContext, Error, anyhow};
use common::config_dir;
use gpui::{App, AppContext, Context, Entity, EventEmitter, Global, SharedString, Task, Window};
use nostr_connect::prelude::*;
use nostr_gossip_memory::prelude::*;
use nostr_lmdb::prelude::*;
use nostr_sdk::prelude::*;

mod blossom;
mod constants;
mod device;
mod nip05;
mod signer;

pub use blossom::*;
pub use constants::*;
pub use device::*;
pub use nip05::*;
pub use signer::*;

pub fn init(window: &mut Window, cx: &mut App) {
    // rustls uses the `aws_lc_rs` provider by default
    // This only errors if the default provider has already
    // been installed. We can ignore this `Result`.
    rustls::crypto::aws_lc_rs::default_provider()
        .install_default()
        .ok();

    // Initialize the tokio runtime
    gpui_tokio::init(cx);

    NostrRegistry::set_global(cx.new(|cx| NostrRegistry::new(window, cx)), cx);
}

struct GlobalNostrRegistry(Entity<NostrRegistry>);

impl Global for GlobalNostrRegistry {}

/// Signer event.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub enum StateEvent {
    /// Connecting to the bootstrapping relay
    Connecting,
    /// Connected to the bootstrapping relay
    Connected,
    /// Creating the signer
    Creating,
    /// Show the identity dialog
    Show,
    /// A new signer has been set
    SignerSet,
    /// An error occurred
    Error(SharedString),
}

impl StateEvent {
    pub fn error<T>(error: T) -> Self
    where
        T: Into<SharedString>,
    {
        Self::Error(error.into())
    }
}

/// Nostr Registry
#[derive(Debug)]
pub struct NostrRegistry {
    /// Nostr client
    client: Client,

    /// Nostr signer
    signer: Arc<CoopSigner>,

    /// All local stored identities
    npubs: Entity<Vec<PublicKey>>,

    /// Keys directory
    key_dir: PathBuf,

    /// Master app keys used for various operations.
    ///
    /// Example: Nostr Connect and NIP-4e operations
    app_keys: Keys,

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
    fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let key_dir = config_dir().join("keys");
        let app_keys = get_or_init_app_keys(cx).unwrap_or(Keys::generate());

        // Construct the nostr signer
        let signer = Arc::new(CoopSigner::new(app_keys.clone()));

        // Get all local stored npubs
        let npubs = cx.new(|_| match Self::discover(&key_dir) {
            Ok(npubs) => npubs,
            Err(e) => {
                log::error!("Failed to discover npubs: {e}");
                vec![]
            }
        });

        // Construct the nostr lmdb instance
        let lmdb = cx.foreground_executor().block_on(async move {
            NostrLmdb::open(config_dir().join("nostr"))
                .await
                .expect("Failed to initialize database")
        });

        // Construct the nostr client
        let client = ClientBuilder::default()
            .signer(signer.clone())
            .database(lmdb)
            .gossip(NostrGossipMemory::unbounded())
            .automatic_authentication(false)
            .connect_timeout(Duration::from_secs(10))
            .sleep_when_idle(SleepWhenIdle::Enabled {
                timeout: Duration::from_secs(600),
            })
            .build();

        // Run at the end of current cycle
        cx.defer_in(window, |this, _window, cx| {
            this.connect(cx);
            // Create an identity if none exists
            if this.npubs.read(cx).is_empty() {
                this.create_identity(cx);
            } else {
                // Show the identity dialog
                cx.emit(StateEvent::Show);
            }
        });

        Self {
            client,
            signer,
            npubs,
            key_dir,
            app_keys,
            tasks: vec![],
        }
    }

    /// Get the nostr client
    pub fn client(&self) -> Client {
        self.client.clone()
    }

    /// Get the nostr signer
    pub fn signer(&self) -> Arc<CoopSigner> {
        self.signer.clone()
    }

    /// Get the npubs entity
    pub fn npubs(&self) -> Entity<Vec<PublicKey>> {
        self.npubs.clone()
    }

    /// Get the app keys
    pub fn keys(&self) -> Keys {
        self.app_keys.clone()
    }

    /// Discover all npubs in the keys directory
    fn discover(dir: &PathBuf) -> Result<Vec<PublicKey>, Error> {
        // Ensure keys directory exists
        std::fs::create_dir_all(dir)?;

        let files = std::fs::read_dir(dir)?;
        let mut entries = Vec::new();
        let mut npubs: Vec<PublicKey> = Vec::new();

        for file in files.flatten() {
            let metadata = file.metadata()?;
            let modified_time = metadata.modified()?;
            let name = file.file_name().into_string().unwrap().replace(".npub", "");
            entries.push((modified_time, name));
        }

        // Sort by modification time (most recent first)
        entries.sort_by(|a, b| b.0.cmp(&a.0));

        for (_, name) in entries {
            let public_key = PublicKey::parse(&name)?;
            npubs.push(public_key);
        }

        Ok(npubs)
    }

    /// Connect to the bootstrapping relays
    fn connect(&mut self, cx: &mut Context<Self>) {
        let client = self.client();

        let task: Task<Result<(), Error>> = cx.background_spawn(async move {
            // Add search relay to the relay pool
            for url in SEARCH_RELAYS.into_iter() {
                client
                    .add_relay(url)
                    .capabilities(RelayCapabilities::READ)
                    .await?;
            }

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
            client
                .connect()
                .and_wait(Duration::from_secs(TIMEOUT))
                .await;

            Ok(())
        });

        // Emit connecting event
        cx.emit(StateEvent::Connecting);

        self.tasks.push(cx.spawn(async move |this, cx| {
            if let Err(e) = task.await {
                this.update(cx, |_this, cx| {
                    cx.emit(StateEvent::error(e.to_string()));
                })?;
            } else {
                this.update(cx, |_this, cx| {
                    cx.emit(StateEvent::Connected);
                })?;
            }

            Ok(())
        }));
    }

    /// Get the secret for a given npub.
    pub fn get_secret(
        &self,
        public_key: PublicKey,
        cx: &App,
    ) -> Task<Result<Arc<dyn NostrSigner>, Error>> {
        let npub = public_key.to_bech32().unwrap();
        let key_path = self.key_dir.join(format!("{}.npub", npub));
        let app_keys = self.app_keys.clone();

        if let Ok(payload) = std::fs::read_to_string(key_path) {
            if !payload.is_empty() {
                cx.background_spawn(async move {
                    let decrypted = app_keys.nip44_decrypt(&public_key, &payload).await?;
                    let secret = SecretKey::parse(&decrypted)?;
                    let keys = Keys::new(secret);

                    Ok(keys.into_nostr_signer())
                })
            } else {
                self.get_secret_keyring(&npub, cx)
            }
        } else {
            self.get_secret_keyring(&npub, cx)
        }
    }

    /// Get the secret for a given npub in the OS credentials store.
    #[deprecated = "Use get_secret instead"]
    fn get_secret_keyring(
        &self,
        user: &str,
        cx: &App,
    ) -> Task<Result<Arc<dyn NostrSigner>, Error>> {
        let read = cx.read_credentials(user);
        let app_keys = self.app_keys.clone();

        cx.background_spawn(async move {
            let (_, secret) = read
                .await
                .map_err(|_| anyhow!("Failed to get signer. Please re-import the secret key"))?
                .ok_or_else(|| anyhow!("Failed to get signer. Please re-import the secret key"))?;

            // Try to parse as a direct secret key first
            if let Ok(secret_key) = SecretKey::from_slice(&secret) {
                return Ok(Keys::new(secret_key).into_nostr_signer());
            }

            // Convert the secret into string
            let sec = String::from_utf8(secret)
                .map_err(|_| anyhow!("Failed to parse secret as UTF-8"))?;

            // Try to parse as a NIP-46 URI
            let uri =
                NostrConnectUri::parse(&sec).map_err(|_| anyhow!("Failed to parse NIP-46 URI"))?;

            let timeout = Duration::from_secs(NOSTR_CONNECT_TIMEOUT);
            let mut nip46 = NostrConnect::new(uri, app_keys, timeout, None)?;

            // Set the auth URL handler
            nip46.auth_url_handler(CoopAuthUrlHandler);

            Ok(nip46.into_nostr_signer())
        })
    }

    /// Add a new npub to the keys directory
    fn write_secret(
        &self,
        public_key: PublicKey,
        secret: String,
        cx: &App,
    ) -> Task<Result<(), Error>> {
        let npub = public_key.to_bech32().unwrap();
        let key_path = self.key_dir.join(format!("{}.npub", npub));
        let app_keys = self.app_keys.clone();

        cx.background_spawn(async move {
            // If the secret starts with "bunker://" (nostr connect), use it directly; otherwise, encrypt it
            let content = if secret.starts_with("bunker://") {
                secret
            } else {
                app_keys.nip44_encrypt(&public_key, &secret).await?
            };

            // Write the encrypted secret to the keys directory
            smol::fs::write(key_path, &content).await?;

            Ok(())
        })
    }

    /// Remove a secret
    pub fn remove_secret(&mut self, public_key: &PublicKey, cx: &mut Context<Self>) {
        let public_key = public_key.to_owned();
        let npub = public_key.to_bech32().unwrap();

        let keys_dir = config_dir().join("keys");
        let key_path = keys_dir.join(format!("{}.npub", npub));

        // Remove the secret file from the keys directory
        std::fs::remove_file(key_path).ok();

        self.npubs.update(cx, |this, cx| {
            this.retain(|k| k != &public_key);
            cx.notify();
        });
    }

    /// Create a new identity
    pub fn create_identity(&mut self, cx: &mut Context<Self>) {
        let client = self.client();
        let keys = Keys::generate();
        let async_keys = keys.clone();

        // Emit creating event
        cx.emit(StateEvent::Creating);

        // Create the write secret task
        let write_secret =
            self.write_secret(keys.public_key(), keys.secret_key().to_secret_hex(), cx);

        // Run async tasks in background
        let task: Task<Result<(), Error>> = cx.background_spawn(async move {
            let signer = async_keys.into_nostr_signer();

            // Construct relay list event
            let relay_list = default_relay_list();
            let event = EventBuilder::relay_list(relay_list).sign(&signer).await?;

            // Publish relay list
            client
                .send_event(&event)
                .to(BOOTSTRAP_RELAYS)
                .ack_policy(AckPolicy::none())
                .await?;

            // Construct the default metadata
            let name = petname::petname(2, "-").unwrap_or("Cooper".to_string());
            let avatar = Url::parse(&format!("https://avatar.vercel.sh/{name}")).unwrap();
            let metadata = Metadata::new().display_name(&name).picture(avatar);
            let event = EventBuilder::metadata(&metadata).sign(&signer).await?;

            // Publish metadata event
            client
                .send_event(&event)
                .to_nip65()
                .ack_policy(AckPolicy::none())
                .await?;

            // Construct the default contact list
            let contacts = vec![Contact::new(PublicKey::parse(COOP_PUBKEY).unwrap())];
            let event = EventBuilder::contact_list(contacts).sign(&signer).await?;

            // Publish contact list event
            client
                .send_event(&event)
                .to_nip65()
                .ack_policy(AckPolicy::none())
                .await?;

            // Construct the default messaging relay list
            let relays = default_messaging_relays();
            let event = EventBuilder::nip17_relay_list(relays).sign(&signer).await?;

            // Publish messaging relay list event
            client.send_event(&event).to_nip65().await?;

            // Write user's credentials to the system keyring
            write_secret.await?;

            Ok(())
        });

        self.tasks.push(cx.spawn(async move |this, cx| {
            match task.await {
                Ok(_) => {
                    this.update(cx, |this, cx| {
                        this.set_signer(keys, cx);
                    })?;
                }
                Err(e) => {
                    this.update(cx, |_this, cx| {
                        cx.emit(StateEvent::error(e.to_string()));
                    })?;
                }
            };

            Ok(())
        }));
    }

    /// Set the signer for the nostr client and verify the public key
    pub fn set_signer<T>(&mut self, new: T, cx: &mut Context<Self>)
    where
        T: NostrSigner + 'static,
    {
        let client = self.client();
        let signer = self.signer();

        // Create a task to update the signer and verify the public key
        let task: Task<Result<PublicKey, Error>> = cx.background_spawn(async move {
            // Update signer and unsubscribe
            signer.switch(new).await;
            client.unsubscribe_all().await?;

            // Verify and get public key
            let signer = client.signer().context("Signer not found")?;
            let public_key = signer.get_public_key().await?;

            log::info!("Signer's public key: {}", public_key);
            Ok(public_key)
        });

        self.tasks.push(cx.spawn(async move |this, cx| {
            match task.await {
                Ok(public_key) => {
                    this.update(cx, |this, cx| {
                        // Add public key to npubs if not already present
                        this.npubs.update(cx, |this, cx| {
                            if !this.contains(&public_key) {
                                this.push(public_key);
                                cx.notify();
                            }
                        });

                        // Emit signer changed event
                        cx.emit(StateEvent::SignerSet);
                    })?;
                }
                Err(e) => {
                    this.update(cx, |_this, cx| {
                        cx.emit(StateEvent::error(e.to_string()));
                    })?;
                }
            };

            Ok(())
        }));
    }

    /// Add a key signer to keyring
    pub fn add_key_signer(&mut self, keys: &Keys, cx: &mut Context<Self>) {
        let keys = keys.clone();
        let write_secret =
            self.write_secret(keys.public_key(), keys.secret_key().to_secret_hex(), cx);

        self.tasks.push(cx.spawn(async move |this, cx| {
            match write_secret.await {
                Ok(_) => {
                    this.update(cx, |this, cx| {
                        this.set_signer(keys, cx);
                    })?;
                }
                Err(e) => {
                    this.update(cx, |_this, cx| {
                        cx.emit(StateEvent::error(e.to_string()));
                    })?;
                }
            };

            Ok(())
        }));
    }

    /// Add a nostr connect signer to keyring
    pub fn add_nip46_signer(&mut self, nip46: &NostrConnect, cx: &mut Context<Self>) {
        let nip46 = nip46.clone();
        let async_nip46 = nip46.clone();

        // Connect and verify the remote signer
        let task: Task<Result<(PublicKey, NostrConnectUri), Error>> =
            cx.background_spawn(async move {
                let uri = async_nip46.bunker_uri().await?;
                let public_key = async_nip46.get_public_key().await?;

                Ok((public_key, uri))
            });

        self.tasks.push(cx.spawn(async move |this, cx| {
            match task.await {
                Ok((public_key, uri)) => {
                    // Create the write secret task
                    let write_secret = this.read_with(cx, |this, cx| {
                        this.write_secret(public_key, uri.to_string(), cx)
                    })?;

                    match write_secret.await {
                        Ok(_) => {
                            this.update(cx, |this, cx| {
                                this.set_signer(nip46, cx);
                            })?;
                        }
                        Err(e) => {
                            this.update(cx, |_this, cx| {
                                cx.emit(StateEvent::error(e.to_string()));
                            })?;
                        }
                    }
                }
                Err(e) => {
                    this.update(cx, |_this, cx| {
                        cx.emit(StateEvent::error(e.to_string()));
                    })?;
                }
            };

            Ok(())
        }));
    }

    /// Get the public key of a NIP-05 address
    pub fn get_address(&self, addr: Nip05Address, cx: &App) -> Task<Result<PublicKey, Error>> {
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
            Some(self.get_address(addr, cx))
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

    /// Perform a WoT (via Vertex) search for a given query.
    pub fn wot_search(&self, query: &str, cx: &App) -> Task<Result<Vec<PublicKey>, Error>> {
        let client = self.client();
        let query = query.to_string();

        cx.background_spawn(async move {
            // Construct a vertex request event
            let builder = EventBuilder::new(Kind::Custom(5315), "").tags(vec![
                Tag::custom(TagKind::custom("param"), vec!["search", &query]),
                Tag::custom(TagKind::custom("param"), vec!["limit", "10"]),
            ]);
            let event = client.sign_event_builder(builder).await?;

            // Send the event to vertex relays
            let output = client.send_event(&event).to(WOT_RELAYS).await?;

            // Construct a filter to get the response or error from vertex
            let filter = Filter::new()
                .kinds(vec![Kind::Custom(6315), Kind::Custom(7000)])
                .event(output.id().to_owned());

            // Construct target for subscription
            let target: HashMap<&str, Vec<Filter>> = WOT_RELAYS
                .into_iter()
                .map(|relay| (relay, vec![filter.clone()]))
                .collect();

            // Stream events from the wot relays
            let mut stream = client
                .stream_events(target)
                .timeout(Duration::from_secs(TIMEOUT))
                .await?;

            while let Some((_url, res)) = stream.next().await {
                if let Ok(event) = res {
                    match event.kind {
                        Kind::Custom(6315) => {
                            let content: serde_json::Value = serde_json::from_str(&event.content)?;
                            let pubkeys: Vec<PublicKey> = content
                                .as_array()
                                .into_iter()
                                .flatten()
                                .filter_map(|item| item.as_object())
                                .filter_map(|obj| obj.get("pubkey").and_then(|v| v.as_str()))
                                .filter_map(|pubkey_str| PublicKey::parse(pubkey_str).ok())
                                .collect();

                            return Ok(pubkeys);
                        }
                        Kind::Custom(7000) => {
                            return Err(anyhow!("Search error"));
                        }
                        _ => {}
                    }
                }
            }

            Err(anyhow!("No results for query: {query}"))
        })
    }
}

/// Get or create new app keys
fn get_or_init_app_keys(cx: &App) -> Result<Keys, Error> {
    let read = cx.read_credentials(CLIENT_NAME);
    let stored_keys: Option<Keys> = cx.foreground_executor().block_on(async move {
        if let Ok(Some((_, secret))) = read.await {
            SecretKey::from_slice(&secret).map(Keys::new).ok()
        } else {
            None
        }
    });

    if let Some(keys) = stored_keys {
        Ok(keys)
    } else {
        let keys = Keys::generate();
        let user = keys.public_key().to_hex();
        let secret = keys.secret_key().to_secret_bytes();
        let write = cx.write_credentials(CLIENT_NAME, &user, &secret);

        cx.foreground_executor().block_on(async move {
            if let Err(e) = write.await {
                log::error!("Keyring not available or panic: {e}")
            }
        });

        Ok(keys)
    }
}

fn default_relay_list() -> Vec<(RelayUrl, Option<RelayMetadata>)> {
    vec![
        (
            RelayUrl::parse("wss://relay.nostr.net").unwrap(),
            Some(RelayMetadata::Write),
        ),
        (
            RelayUrl::parse("wss://relay.primal.net").unwrap(),
            Some(RelayMetadata::Write),
        ),
        (
            RelayUrl::parse("wss://relay.damus.io").unwrap(),
            Some(RelayMetadata::Read),
        ),
        (
            RelayUrl::parse("wss://nos.lol").unwrap(),
            Some(RelayMetadata::Read),
        ),
        (
            RelayUrl::parse("wss://nostr.superfriends.online").unwrap(),
            None,
        ),
    ]
}

fn default_messaging_relays() -> Vec<RelayUrl> {
    vec![
        RelayUrl::parse("wss://nos.lol").unwrap(),
        RelayUrl::parse("wss://nip17.com").unwrap(),
        RelayUrl::parse("wss://auth.nostr1.com").unwrap(),
    ]
}

#[derive(Debug, Clone)]
pub struct CoopAuthUrlHandler;

impl AuthUrlHandler for CoopAuthUrlHandler {
    #[allow(mismatched_lifetime_syntaxes)]
    fn on_auth_url(&self, auth_url: Url) -> BoxedFuture<Result<()>> {
        Box::pin(async move {
            webbrowser::open(auth_url.as_str())?;
            Ok(())
        })
    }
}
