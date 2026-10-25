use std::collections::{HashMap, HashSet};
use std::sync::RwLock;

use anyhow::{Error, anyhow};
use common::emojis_from_tags;
use futures::{FutureExt, pin_mut};
use gpui::{
    App, AppContext, BackgroundExecutor, Context, Entity, EventEmitter, Global, Subscription, Task,
};
use instant::Duration;
use nostr_sdk::prelude::*;
use smallvec::{SmallVec, smallvec};
use state::{Announcement, BOOTSTRAP_RELAYS, NostrRegistry, TIMEOUT};

mod directory;
mod person;
mod wot;

pub use directory::*;
pub use person::*;
pub use wot::*;

pub fn init(cx: &mut App) {
    PersonRegistry::set_global(cx.new(PersonRegistry::new), cx);
}

struct GlobalPersonRegistry(Entity<PersonRegistry>);

impl Global for GlobalPersonRegistry {}

/// A change to a person's data
#[derive(Debug, Clone)]
pub enum PersonEvent {
    /// Profile metadata
    Metadata(Box<Person>),
    /// Custom emoji list (NIP-30)
    Emojis {
        public_key: PublicKey,
        emojis: Vec<(String, Url)>,
    },
    /// Profile encryption keys announcement (NIP-4e)
    Announcement {
        public_key: PublicKey,
        announcement: Announcement,
    },
    /// Messaging relay list (NIP-65)
    MessagingRelays {
        public_key: PublicKey,
        relays: Vec<RelayUrl>,
    },
}

impl EventEmitter<PersonEvent> for PersonRegistry {}

#[derive(Debug, Clone)]
enum Dispatch {
    Person(Person),
    Announcement(Event),
    Relays(Event),
    Emojis(Event),
}

/// Person Registry
#[derive(Debug)]
pub struct PersonRegistry {
    /// Public keys with a metadata request in flight
    in_flight: RwLock<HashSet<PublicKey>>,
    /// Web of trust graph of the signed-in user
    wot: Entity<WebOfTrust>,
    sender: flume::Sender<PublicKey>,
    tasks: SmallVec<[Task<()>; 4]>,
    _subscriptions: SmallVec<[Subscription; 1]>,
}

impl PersonRegistry {
    /// Retrieve the global person registry
    pub fn global(cx: &App) -> Entity<Self> {
        cx.global::<GlobalPersonRegistry>().0.clone()
    }

    /// Set the global person registry instance
    fn set_global(state: Entity<Self>, cx: &mut App) {
        cx.set_global(GlobalPersonRegistry(state));
    }

    /// Create a new person registry instance
    fn new(cx: &mut Context<Self>) -> Self {
        let nostr = NostrRegistry::global(cx);
        let client = nostr.read(cx).client();
        let wot = cx.new(WebOfTrust::new);

        // Channel for communication between nostr and gpui
        let (tx, rx) = flume::bounded::<Dispatch>(100);
        let (metadata_tx, metadata_rx) = flume::unbounded::<PublicKey>();

        let mut tasks = smallvec![];

        let client2 = client.clone();
        tasks.push(cx.background_spawn(async move {
            Self::handle_notifications(&client2, &tx).await;
        }));

        let client3 = client.clone();
        let executor = cx.background_executor().clone();
        tasks.push(cx.background_spawn(async move {
            Self::handle_requests(&client3, &metadata_rx, executor).await;
        }));

        tasks.push(cx.spawn(async move |this, cx| {
            while let Ok(event) = rx.recv_async().await {
                this.update(cx, |this, cx| {
                    match event {
                        Dispatch::Person(person) => {
                            this.insert(person, cx);
                        }
                        Dispatch::Announcement(event) => {
                            this.set_announcement(&event, cx);
                        }
                        Dispatch::Relays(event) => {
                            this.set_messaging_relays(&event, cx);
                        }
                        Dispatch::Emojis(event) => {
                            this.set_emojis(&event, cx);
                        }
                    };
                })
                .ok();
            }
        }));

        let subscription = cx.subscribe(&nostr, |this, _nostr, event, cx| {
            if event.signer_changed() {
                this.get_emojis(cx);

                this.wot.update(cx, |wot, cx| {
                    wot.reset(cx);
                    wot.hydrate(cx);
                });
            }
        });

        Self {
            in_flight: RwLock::new(HashSet::new()),
            wot,
            sender: metadata_tx,
            tasks,
            _subscriptions: smallvec![subscription],
        }
    }

    /// The web of trust graph of the signed-in user
    pub fn wot(&self) -> Entity<WebOfTrust> {
        self.wot.clone()
    }

    /// Handle nostr notifications
    async fn handle_notifications(client: &Client, tx: &flume::Sender<Dispatch>) {
        let mut notifications = client.notifications();
        let mut processed: HashSet<EventId> = HashSet::new();

        while let Some(notification) = notifications.next().await {
            let ClientNotification::Message { message, .. } = notification else {
                // Skip if the notification is not a message
                continue;
            };

            if let RelayMessage::Event { event, .. } = *message {
                // Skip if the event has already been processed
                if !processed.insert(event.id) {
                    continue;
                }

                match event.kind {
                    Kind::Metadata => {
                        let metadata = Metadata::from_json(&event.content).unwrap_or_default();
                        let person = Person::new(event.pubkey, metadata);

                        if tx.send_async(Dispatch::Person(person)).await.is_err() {
                            log::warn!("Channel closed, dropping metadata event");
                        }
                    }
                    Kind::InboxRelays => {
                        _ = tx.send_async(Dispatch::Relays(event.into_owned())).await;
                    }
                    Kind::Emojis => {
                        _ = tx.send_async(Dispatch::Emojis(event.into_owned())).await;
                    }
                    Kind::Custom(10044) => {
                        _ = tx
                            .send_async(Dispatch::Announcement(event.into_owned()))
                            .await;
                    }
                    _ => {}
                }
            }
        }
    }

    /// Handle request for metadata
    async fn handle_requests(
        client: &Client,
        rx: &flume::Receiver<PublicKey>,
        executor: BackgroundExecutor,
    ) {
        let mut batch: HashSet<PublicKey> = HashSet::new();

        loop {
            let timer = executor.timer(Duration::from_secs(TIMEOUT)).fuse();
            pin_mut!(timer);

            let request = futures::select! {
                result = rx.recv_async().fuse() => match result {
                    Ok(public_key) => public_key,
                    Err(_) => {
                        if !batch.is_empty()
                            && let Err(e) = get_metadata(client, std::mem::take(&mut batch)).await
                        {
                            log::warn!("Failed to get metadata batch: {e}");
                        }
                        return;
                    }
                },
                () = timer => {
                    if !batch.is_empty()
                        && let Err(e) = get_metadata(client, std::mem::take(&mut batch)).await
                    {
                        log::warn!("Failed to get metadata batch: {e}");
                    }
                    continue;
                }
            };

            batch.insert(request);
            // Process the batch if it's full
            if batch.len() >= 20
                && let Err(e) = get_metadata(client, std::mem::take(&mut batch)).await
            {
                log::warn!("Failed to get metadata batch: {e}");
            }
        }
    }

    /// Subscribe to the signed-in user's emoji list
    fn get_emojis(&mut self, cx: &mut Context<Self>) {
        let nostr = NostrRegistry::global(cx);
        let client = nostr.read(cx).client();
        let Some(public_key) = nostr.read(cx).current_user() else {
            return;
        };

        self.tasks.push(cx.background_spawn(async move {
            let opts = SubscribeAutoCloseOptions::default().exit_policy(ReqExitPolicy::ExitOnEOSE);
            let filter = Filter::new().kind(Kind::Emojis).author(public_key).limit(1);

            if let Err(e) = client.subscribe(vec![filter]).close_on(opts).await {
                log::warn!("Failed to subscribe to emoji list: {e}");
            }
        }));
    }

    /// Set profile encryption keys announcement
    fn set_announcement(&mut self, event: &Event, cx: &mut Context<Self>) {
        let Some(announcement) = Announcement::from_event(event) else {
            log::warn!("Ignoring announcement with missing or invalid key tag");
            return;
        };

        cx.emit(PersonEvent::Announcement {
            public_key: event.pubkey,
            announcement,
        });
    }

    /// Set messaging relays for a person
    fn set_messaging_relays(&mut self, event: &Event, cx: &mut Context<Self>) {
        cx.emit(PersonEvent::MessagingRelays {
            public_key: event.pubkey,
            relays: nip17::extract_relay_list(event).collect(),
        });
    }

    /// Set custom emoji for a person from their emoji list event
    fn set_emojis(&mut self, event: &Event, cx: &mut Context<Self>) {
        self.set_emojis_for(event.pubkey, emojis_from_tags(&event.tags), cx);
    }

    /// Set custom emoji for a person
    fn set_emojis_for(
        &mut self,
        public_key: PublicKey,
        emojis: Vec<(String, Url)>,
        cx: &mut Context<Self>,
    ) {
        cx.emit(PersonEvent::Emojis { public_key, emojis });
    }

    /// Publish a person's profile metadata to every directory
    pub fn insert(&mut self, person: Person, cx: &mut Context<Self>) {
        let public_key = person.public_key();
        self.release_in_flight(&public_key);

        cx.emit(PersonEvent::Metadata(Box::new(person)));
    }

    /// Queue a batched metadata request for keys without one in flight
    pub(crate) fn fetch<I>(&self, keys: I)
    where
        I: IntoIterator<Item = PublicKey>,
    {
        let mut queue = Vec::new();
        {
            let mut in_flight = match self.in_flight.write() {
                Ok(in_flight) => in_flight,
                Err(poisoned) => poisoned.into_inner(),
            };

            for key in keys {
                if in_flight.insert(key) {
                    queue.push(key);
                }
            }
        }

        for public_key in queue {
            if let Err(error) = self.sender.send(public_key) {
                log::warn!("Failed to queue metadata request: {error}");
            }
        }
    }

    /// Allow future fetches for a key once its metadata has arrived
    fn release_in_flight(&self, public_key: &PublicKey) {
        let mut in_flight = match self.in_flight.write() {
            Ok(in_flight) => in_flight,
            Err(poisoned) => poisoned.into_inner(),
        };
        in_flight.remove(public_key);
    }
}

/// Get metadata for all public keys in a event
async fn get_metadata<I>(client: &Client, public_keys: I) -> Result<(), Error>
where
    I: IntoIterator<Item = PublicKey>,
{
    let authors: Vec<PublicKey> = public_keys.into_iter().collect();
    let limit = authors.len();

    if authors.is_empty() {
        return Err(anyhow!("You need at least one public key"));
    }

    // Construct the subscription option
    let opts = SubscribeAutoCloseOptions::default()
        .exit_policy(ReqExitPolicy::ExitOnEOSE)
        .timeout(Some(Duration::from_secs(TIMEOUT)));

    // Construct the filter for metadata
    let filter = Filter::new()
        .kind(Kind::Metadata)
        .authors(authors)
        .limit(limit);

    // Construct target for subscription
    let target: HashMap<&str, Vec<Filter>> = BOOTSTRAP_RELAYS
        .into_iter()
        .map(|relay| (relay, vec![filter.clone()]))
        .collect();

    client.subscribe(target).close_on(opts).await?;

    Ok(())
}
