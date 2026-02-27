use std::collections::{HashMap, HashSet};
use std::os::unix::fs::PermissionsExt;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{anyhow, Context as AnyhowContext, Error};
use common::config_dir;
use gpui::{App, AppContext, Context, Entity, Global, SharedString, Task, Window};
use nostr_connect::prelude::*;
use nostr_lmdb::prelude::*;
use nostr_sdk::prelude::*;

mod blossom;
mod constants;
mod device;
mod gossip;
mod nip05;
mod signer;

pub use blossom::*;
pub use constants::*;
pub use device::*;
pub use gossip::*;
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

/// Nostr Registry
#[derive(Debug)]
pub struct NostrRegistry {
    /// Nostr client
    client: Client,

    /// Nostr signer
    signer: Arc<CoopSigner>,

    /// App keys
    ///
    /// Used for Nostr Connect and NIP-4e operations
    app_keys: Keys,

    /// Custom gossip implementation
    gossip: Entity<Gossip>,

    /// Relay list state
    relay_list_state: RelayState,

    /// Whether Coop is connected to all bootstrap relays
    connected: bool,

    /// Whether Coop is creating a new signer
    creating: bool,

    /// Tasks for asynchronous operations
    tasks: Vec<Task<Result<(), Error>>>,
}

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
        // Construct the nostr signer
        let app_keys = get_or_init_app_keys().unwrap_or(Keys::generate());
        let signer = Arc::new(CoopSigner::new(app_keys.clone()));

        // Construct the gossip entity
        let gossip = cx.new(|_| Gossip::default());

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
            .automatic_authentication(false)
            .verify_subscriptions(false)
            .connect_timeout(Duration::from_secs(TIMEOUT))
            .sleep_when_idle(SleepWhenIdle::Enabled {
                timeout: Duration::from_secs(600),
            })
            .build();

        // Run at the end of current cycle
        cx.defer_in(window, |this, _window, cx| {
            this.connect(cx);
            this.handle_notifications(cx);
        });

        Self {
            client,
            signer,
            app_keys,
            gossip,
            relay_list_state: RelayState::Idle,
            connected: false,
            creating: false,
            tasks: vec![],
        }
    }

    fn connect(&mut self, cx: &mut Context<Self>) {
        let client = self.client();

        self.tasks.push(cx.spawn(async move |this, cx| {
            cx.background_executor()
                .await_on_background(async move {
                    // Add search relay to the relay pool
                    for url in SEARCH_RELAYS.into_iter() {
                        client.add_relay(url).await.ok();
                    }

                    // Add bootstrap relay to the relay pool
                    for url in BOOTSTRAP_RELAYS.into_iter() {
                        client.add_relay(url).await.ok();
                    }

                    // Connect to all added relays
                    client.connect().and_wait(Duration::from_secs(5)).await;
                })
                .await;

            // Update the state
            this.update(cx, |this, cx| {
                this.set_connected(cx);
                this.get_signer(cx);
            })?;

            Ok(())
        }));
    }

    /// Handle nostr notifications
    fn handle_notifications(&mut self, cx: &mut Context<Self>) {
        let client = self.client();
        let gossip = self.gossip.downgrade();

        // Channel for communication between nostr and gpui
        let (tx, rx) = flume::bounded::<Event>(2048);

        let task: Task<Result<(), Error>> = cx.background_spawn(async move {
            // Handle nostr notifications
            let mut notifications = client.notifications();
            let mut processed_events = HashSet::new();

            while let Some(notification) = notifications.next().await {
                if let ClientNotification::Message {
                    message: RelayMessage::Event { event, .. },
                    ..
                } = notification
                {
                    // Skip if the event has already been processed
                    if processed_events.insert(event.id) {
                        match event.kind {
                            Kind::RelayList => {
                                tx.send_async(event.into_owned()).await?;
                            }
                            Kind::InboxRelays => {
                                tx.send_async(event.into_owned()).await?;
                            }
                            _ => {}
                        }
                    }
                }
            }

            Ok(())
        });

        // Run task in the background
        task.detach();

        self.tasks.push(cx.spawn(async move |_this, cx| {
            while let Ok(event) = rx.recv_async().await {
                if let Kind::RelayList = event.kind {
                    gossip.update(cx, |this, cx| {
                        this.insert_relays(&event);
                        cx.notify();
                    })?;
                }
            }

            Ok(())
        }));
    }

    /// Get the nostr client
    pub fn client(&self) -> Client {
        self.client.clone()
    }

    /// Get the nostr signer
    pub fn signer(&self) -> Arc<CoopSigner> {
        self.signer.clone()
    }

    /// Get the app keys
    pub fn app_keys(&self) -> &Keys {
        &self.app_keys
    }

    /// Get the connected status of the client
    pub fn connected(&self) -> bool {
        self.connected
    }

    /// Get the creating status
    pub fn creating(&self) -> bool {
        self.creating
    }

    /// Get the relay list state
    pub fn relay_list_state(&self) -> RelayState {
        self.relay_list_state.clone()
    }

    /// Get all relays for a given public key without ensuring connections
    pub fn read_only_relays(&self, public_key: &PublicKey, cx: &App) -> Vec<SharedString> {
        self.gossip.read(cx).read_only_relays(public_key)
    }

    /// Ensure write relays for a given public key
    pub fn ensure_write_relays(&self, public_key: &PublicKey, cx: &App) -> Task<Vec<RelayUrl>> {
        let client = self.client();
        let public_key = *public_key;

        cx.background_spawn(async move {
            let mut relays = vec![];

            let filter = Filter::new()
                .kind(Kind::RelayList)
                .author(public_key)
                .limit(1);

            // Construct target for subscription
            let target: HashMap<&str, Vec<Filter>> = BOOTSTRAP_RELAYS
                .into_iter()
                .map(|relay| (relay, vec![filter.clone()]))
                .collect();

            if let Ok(mut stream) = client
                .stream_events(target)
                .timeout(Duration::from_secs(TIMEOUT))
                .await
            {
                while let Some((_url, res)) = stream.next().await {
                    match res {
                        Ok(event) => {
                            // Extract relay urls
                            relays.extend(nip65::extract_owned_relay_list(event).filter_map(
                                |(url, metadata)| {
                                    if metadata.is_none() || metadata == Some(RelayMetadata::Write)
                                    {
                                        Some(url)
                                    } else {
                                        None
                                    }
                                },
                            ));

                            // Ensure connections
                            for url in relays.iter() {
                                client.add_relay(url).and_connect().await.ok();
                            }

                            return relays;
                        }
                        Err(e) => {
                            log::error!("Failed to receive relay list event: {e}");
                        }
                    }
                }
            }

            relays
        })
    }

    /// Get a list of write relays for a given public key
    pub fn write_relays(&self, public_key: &PublicKey, cx: &App) -> Task<Vec<RelayUrl>> {
        let client = self.client();
        let relays = self.gossip.read(cx).write_relays(public_key);

        cx.background_spawn(async move {
            // Ensure relay connections
            for url in relays.iter() {
                client.add_relay(url).and_connect().await.ok();
            }

            relays
        })
    }

    /// Get a list of read relays for a given public key
    pub fn read_relays(&self, public_key: &PublicKey, cx: &App) -> Task<Vec<RelayUrl>> {
        let client = self.client();
        let relays = self.gossip.read(cx).read_relays(public_key);

        cx.background_spawn(async move {
            // Ensure relay connections
            for url in relays.iter() {
                client.add_relay(url).and_connect().await.ok();
            }

            relays
        })
    }

    /// Set the connected status of the client
    fn set_connected(&mut self, cx: &mut Context<Self>) {
        self.connected = true;
        cx.notify();
    }

    /// Get local stored signer
    fn get_signer(&mut self, cx: &mut Context<Self>) {
        let read_credential = cx.read_credentials(KEYRING);

        self.tasks.push(cx.spawn(async move |this, cx| {
            match read_credential.await {
                Ok(Some((_user, secret))) => {
                    let secret = SecretKey::from_slice(&secret)?;
                    let keys = Keys::new(secret);

                    this.update(cx, |this, cx| {
                        this.set_signer(keys, false, cx);
                    })?;
                }
                _ => {
                    this.update(cx, |this, cx| {
                        this.get_bunker(cx);
                    })?;
                }
            }

            Ok(())
        }));
    }

    /// Get local stored bunker connection
    fn get_bunker(&mut self, cx: &mut Context<Self>) {
        let client = self.client();
        let app_keys = self.app_keys().clone();
        let timeout = Duration::from_secs(NOSTR_CONNECT_TIMEOUT);

        let task: Task<Result<NostrConnect, Error>> = cx.background_spawn(async move {
            log::info!("Getting bunker connection");

            let filter = Filter::new()
                .kind(Kind::ApplicationSpecificData)
                .identifier("coop:account")
                .limit(1);

            if let Some(event) = client.database().query(filter).await?.first_owned() {
                let uri = NostrConnectUri::parse(event.content)?;
                let signer = NostrConnect::new(uri.clone(), app_keys.clone(), timeout, None)?;

                Ok(signer)
            } else {
                Err(anyhow!("No account found"))
            }
        });

        self.tasks.push(cx.spawn(async move |this, cx| {
            match task.await {
                Ok(signer) => {
                    this.update(cx, |this, cx| {
                        this.set_signer(signer, true, cx);
                    })
                    .ok();
                }
                Err(e) => {
                    log::warn!("Failed to get bunker: {e}");
                    // Create a new identity if no stored bunker exists
                    this.update(cx, |this, cx| {
                        this.set_default_signer(cx);
                    })
                    .ok();
                }
            }

            Ok(())
        }));
    }

    /// Set the signer for the nostr client and verify the public key
    pub fn set_signer<T>(&mut self, new: T, owned: bool, cx: &mut Context<Self>)
    where
        T: NostrSigner + 'static,
    {
        let client = self.client();
        let signer = self.signer();

        // Create a task to update the signer and verify the public key
        let task: Task<Result<(), Error>> = cx.background_spawn(async move {
            // Update signer
            signer.switch(new, owned).await;

            // Unsubscribe from all subscriptions
            client.unsubscribe_all().await?;

            // Verify signer
            let signer = client.signer().context("Signer not found")?;
            let public_key = signer.get_public_key().await?;
            log::info!("Signer's public key: {}", public_key);

            Ok(())
        });

        self.tasks.push(cx.spawn(async move |this, cx| {
            // set signer
            task.await?;

            // Update states
            this.update(cx, |this, cx| {
                this.ensure_relay_list(cx);
            })?;

            Ok(())
        }));
    }

    /// Create a new identity
    fn set_default_signer(&mut self, cx: &mut Context<Self>) {
        let client = self.client();
        let keys = Keys::generate();
        let async_keys = keys.clone();

        // Create a write credential task
        let write_credential = cx.write_credentials(
            KEYRING,
            &keys.public_key().to_hex(),
            &keys.secret_key().to_secret_bytes(),
        );

        // Set the creating signer status
        self.set_creating_signer(true, cx);

        // Run async tasks in background
        let task: Task<Result<(), Error>> = cx.background_spawn(async move {
            let signer = async_keys.into_nostr_signer();

            // Get default relay list
            let relay_list = default_relay_list();

            // Publish relay list event
            let event = EventBuilder::relay_list(relay_list).sign(&signer).await?;
            client
                .send_event(&event)
                .ok_timeout(Duration::from_secs(TIMEOUT))
                .await?;

            // Construct the default metadata
            let name = petname::petname(2, "-").unwrap_or("Cooper".to_string());
            let avatar = Url::parse(&format!("https://avatar.vercel.sh/{name}")).unwrap();
            let metadata = Metadata::new().display_name(&name).picture(avatar);

            // Publish metadata event
            let event = EventBuilder::metadata(&metadata).sign(&signer).await?;
            client
                .send_event(&event)
                .ok_timeout(Duration::from_secs(TIMEOUT))
                .ack_policy(AckPolicy::none())
                .await?;

            // Construct the default contact list
            let contacts = vec![Contact::new(PublicKey::parse(COOP_PUBKEY).unwrap())];

            // Publish contact list event
            let event = EventBuilder::contact_list(contacts).sign(&signer).await?;
            client
                .send_event(&event)
                .ok_timeout(Duration::from_secs(TIMEOUT))
                .ack_policy(AckPolicy::none())
                .await?;

            // Construct the default messaging relay list
            let relays = default_messaging_relays();

            // Publish messaging relay list event
            let event = EventBuilder::nip17_relay_list(relays).sign(&signer).await?;
            client
                .send_event(&event)
                .ok_timeout(Duration::from_secs(TIMEOUT))
                .ack_policy(AckPolicy::none())
                .await?;

            // Write user's credentials to the system keyring
            write_credential.await?;

            Ok(())
        });

        self.tasks.push(cx.spawn(async move |this, cx| {
            // Wait for the task to complete
            task.await?;

            this.update(cx, |this, cx| {
                this.set_creating_signer(false, cx);
                this.set_signer(keys, false, cx);
            })?;

            Ok(())
        }));
    }

    /// Set whether Coop is creating a new signer
    fn set_creating_signer(&mut self, creating: bool, cx: &mut Context<Self>) {
        self.creating = creating;
        cx.notify();
    }

    /// Set the state of the relay list
    fn set_relay_state(&mut self, state: RelayState, cx: &mut Context<Self>) {
        self.relay_list_state = state;
        cx.notify();
    }

    pub fn ensure_relay_list(&mut self, cx: &mut Context<Self>) {
        let task = self.verify_relay_list(cx);

        // Set the state to idle before starting the task
        self.set_relay_state(RelayState::default(), cx);

        self.tasks.push(cx.spawn(async move |this, cx| {
            let result = task.await?;

            // Update state
            this.update(cx, |this, cx| {
                this.relay_list_state = result;
                cx.notify();
            })?;

            Ok(())
        }));
    }

    // Verify relay list for current user
    fn verify_relay_list(&mut self, cx: &mut Context<Self>) -> Task<Result<RelayState, Error>> {
        let client = self.client();

        cx.background_spawn(async move {
            let signer = client.signer().context("Signer not found")?;
            let public_key = signer.get_public_key().await?;

            let filter = Filter::new()
                .kind(Kind::RelayList)
                .author(public_key)
                .limit(1);

            // Construct target for subscription
            let target: HashMap<&str, Vec<Filter>> = BOOTSTRAP_RELAYS
                .into_iter()
                .map(|relay| (relay, vec![filter.clone()]))
                .collect();

            // Stream events from the bootstrap relays
            let mut stream = client
                .stream_events(target)
                .timeout(Duration::from_secs(TIMEOUT))
                .await?;

            while let Some((_url, res)) = stream.next().await {
                match res {
                    Ok(event) => {
                        log::info!("Received relay list event: {event:?}");
                        return Ok(RelayState::Configured);
                    }
                    Err(e) => {
                        log::error!("Failed to receive relay list event: {e}");
                    }
                }
            }

            Ok(RelayState::NotConfigured)
        })
    }

    /// Generate a direct nostr connection initiated by the client
    pub fn client_connect(&self, relay: Option<RelayUrl>) -> (NostrConnect, NostrConnectUri) {
        let app_keys = self.app_keys();
        let timeout = Duration::from_secs(NOSTR_CONNECT_TIMEOUT);

        // Determine the relay will be used for Nostr Connect
        let relay = match relay {
            Some(relay) => relay,
            None => RelayUrl::parse(NOSTR_CONNECT_RELAY).unwrap(),
        };

        // Generate the nostr connect uri
        let uri = NostrConnectUri::client(app_keys.public_key(), vec![relay], CLIENT_NAME);

        // Generate the nostr connect
        let mut signer = NostrConnect::new(uri.clone(), app_keys.clone(), timeout, None).unwrap();

        // Handle the auth request
        signer.auth_url_handler(CoopAuthUrlHandler);

        (signer, uri)
    }

    /// Store the bunker connection for the next login
    pub fn persist_bunker(&mut self, uri: NostrConnectUri, cx: &mut App) {
        let client = self.client();
        let rng_keys = Keys::generate();

        self.tasks.push(cx.background_spawn(async move {
            // Construct the event for application-specific data
            let event = EventBuilder::new(Kind::ApplicationSpecificData, uri.to_string())
                .tag(Tag::identifier("coop:account"))
                .sign(&rng_keys)
                .await?;

            // Store the event in the database
            client.database().save_event(&event).await?;

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
            if let Some(task) = address_task {
                if let Ok(public_key) = task.await {
                    results.push(public_key);
                    return Ok(results);
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
            let target = WOT_RELAYS
                .into_iter()
                .map(|relay| (relay, vec![filter.clone()]))
                .collect::<HashMap<_, _>>();

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

/// Get or create a new app keys
fn get_or_init_app_keys() -> Result<Keys, Error> {
    let dir = config_dir().join(".app_keys");

    let content = match std::fs::read(&dir) {
        Ok(content) => content,
        Err(_) => {
            // Generate new keys if file doesn't exist
            let keys = Keys::generate();
            let secret_key = keys.secret_key();

            // Create directory and write secret key
            std::fs::create_dir_all(dir.parent().unwrap())?;
            std::fs::write(&dir, secret_key.to_secret_bytes())?;

            // Set permissions to readonly
            let mut perms = std::fs::metadata(&dir)?.permissions();
            perms.set_mode(0o400);
            std::fs::set_permissions(&dir, perms)?;

            return Ok(keys);
        }
    };

    let secret_key = SecretKey::from_slice(&content)?;
    let keys = Keys::new(secret_key);

    Ok(keys)
}

fn default_relay_list() -> Vec<(RelayUrl, Option<RelayMetadata>)> {
    vec![
        (
            RelayUrl::parse("wss://relay.gulugulu.moe").unwrap(),
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
        RelayUrl::parse("wss://relay.0xchat.com").unwrap(),
    ]
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum RelayState {
    #[default]
    Idle,
    Checking,
    NotConfigured,
    Configured,
}

impl RelayState {
    pub fn idle(&self) -> bool {
        matches!(self, RelayState::Idle)
    }

    pub fn checking(&self) -> bool {
        matches!(self, RelayState::Checking)
    }

    pub fn not_configured(&self) -> bool {
        matches!(self, RelayState::NotConfigured)
    }

    pub fn configured(&self) -> bool {
        matches!(self, RelayState::Configured)
    }
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
