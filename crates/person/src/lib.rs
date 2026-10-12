use std::cmp::Reverse;
use std::collections::{HashMap, HashSet};
use std::sync::RwLock;

use anyhow::{Error, anyhow};
use common::{EventExt, emojis_from_tags};
use gpui::{App, AppContext, Context, Entity, EventEmitter, Global, Subscription, Task};
use instant::Duration;
use nostr_sdk::prelude::*;
use smallvec::{SmallVec, smallvec};
use state::{Announcement, BOOTSTRAP_RELAYS, NostrRegistry, TIMEOUT};

mod directory;
mod person;

pub use directory::*;
pub use person::*;

pub fn init(cx: &mut App) {
    PersonRegistry::set_global(cx.new(PersonRegistry::new), cx);
}

struct GlobalPersonRegistry(Entity<PersonRegistry>);

impl Global for GlobalPersonRegistry {}

/// A change to a person's data
#[derive(Debug, Clone)]
pub enum PersonEvent {
    Updated(Person),
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
    /// Collection of all persons (user profiles)
    persons: HashMap<PublicKey, Entity<Person>>,
    /// Set of public keys that have been seen
    seen: RwLock<HashSet<PublicKey>>,
    /// Public keys with a metadata request in flight
    in_flight: RwLock<HashSet<PublicKey>>,
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
        let entity = cx.entity().downgrade();

        // Channel for communication between nostr and gpui
        let (tx, rx) = flume::bounded::<Dispatch>(100);
        let (metadata_tx, metadata_rx) = flume::unbounded::<PublicKey>();

        let mut tasks = smallvec![];

        let client2 = client.clone();
        tasks.push(cx.background_spawn(async move {
            Self::handle_notifications(&client2, &tx).await;
        }));

        let client3 = client.clone();
        tasks.push(cx.background_spawn(async move {
            Self::handle_requests(&client3, &metadata_rx).await;
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
            }
        });

        // Load all user profiles from the database
        cx.defer(move |cx| {
            _ = entity.update(cx, |this, cx| {
                this.load(cx);
                this.load_emojis(cx);
            });
        });

        Self {
            persons: HashMap::new(),
            seen: RwLock::new(HashSet::new()),
            in_flight: RwLock::new(HashSet::new()),
            sender: metadata_tx,
            tasks,
            _subscriptions: smallvec![subscription],
        }
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
                    Kind::ContactList => {
                        let public_keys = event.extract_public_keys();
                        if let Err(e) = get_metadata(client, public_keys).await {
                            log::warn!("Failed to get metadata for contact list: {e}");
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
    async fn handle_requests(client: &Client, rx: &flume::Receiver<PublicKey>) {
        let mut batch: HashSet<PublicKey> = HashSet::new();

        loop {
            match flume::Selector::new()
                .recv(rx, |result| result.ok())
                .wait_timeout(Duration::from_secs(TIMEOUT))
            {
                Ok(Some(public_key)) => {
                    batch.insert(public_key);
                    // Process the batch if it's full
                    if batch.len() >= 20
                        && let Err(e) = get_metadata(client, std::mem::take(&mut batch)).await
                    {
                        log::warn!("Failed to get metadata batch: {e}");
                    }
                }
                _ => {
                    if !batch.is_empty()
                        && let Err(e) = get_metadata(client, std::mem::take(&mut batch)).await
                    {
                        log::warn!("Failed to get metadata batch: {e}");
                    }
                }
            }
        }
    }

    /// Load all user profiles from the database
    fn load(&mut self, cx: &mut Context<Self>) {
        let nostr = NostrRegistry::global(cx);
        let client = nostr.read(cx).client();

        let task: Task<Result<Vec<Person>, Error>> = cx.background_spawn(async move {
            let filter = Filter::new().kind(Kind::Metadata).limit(200);
            let events = client.database().query(filter).await?;
            let persons = events
                .into_iter()
                .map(|event| {
                    let metadata = Metadata::from_json(event.content).unwrap_or_default();
                    Person::new(event.pubkey, metadata)
                })
                .collect();

            Ok(persons)
        });

        self.tasks.push(cx.spawn(async move |this, cx| {
            if let Ok(persons) = task.await {
                this.update(cx, |this, cx| {
                    this.bulk_insert(persons, cx);
                })
                .ok();
            }
        }));
    }

    /// Load cached emoji lists from the database
    fn load_emojis(&mut self, cx: &mut Context<Self>) {
        let nostr = NostrRegistry::global(cx);
        let client = nostr.read(cx).client();

        type EmojiLists = Vec<(PublicKey, Vec<(String, Url)>)>;

        let task: Task<Result<EmojiLists, Error>> = cx.background_spawn(async move {
            let filter = Filter::new().kind(Kind::Emojis).limit(200);
            let mut events: Vec<Event> =
                client.database().query(filter).await?.into_iter().collect();

            // Keep only the newest list per author
            events.sort_by_key(|event| Reverse(event.created_at));
            let mut seen = HashSet::new();

            let lists = events
                .into_iter()
                .filter(|event| seen.insert(event.pubkey))
                .map(|event| (event.pubkey, emojis_from_tags(&event.tags)))
                .collect();

            Ok(lists)
        });

        self.tasks.push(cx.spawn(async move |this, cx| {
            if let Ok(lists) = task.await {
                this.update(cx, |this, cx| {
                    for (public_key, emojis) in lists {
                        this.set_emojis_for(public_key, emojis, cx);
                    }
                })
                .ok();
            }
        }));
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

        if let Some(person) = self.persons.get(&event.pubkey) {
            person.update(cx, |person, cx| {
                person.set_announcement(announcement);
                cx.notify();
            });
            cx.emit(PersonEvent::Updated(person.read(cx).clone()));
        } else {
            let person =
                Person::new(event.pubkey, Metadata::default()).with_announcement(announcement);
            self.insert(person, cx);
        }
    }

    /// Set messaging relays for a person
    fn set_messaging_relays(&mut self, event: &Event, cx: &mut Context<Self>) {
        let urls: Vec<RelayUrl> = nip17::extract_relay_list(event).collect();

        if let Some(person) = self.persons.get(&event.pubkey) {
            person.update(cx, |person, cx| {
                person.set_messaging_relays(urls);
                cx.notify();
            });
            cx.emit(PersonEvent::Updated(person.read(cx).clone()));
        } else {
            let person = Person::new(event.pubkey, Metadata::default()).with_messaging_relays(urls);
            self.insert(person, cx);
        }
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
        if let Some(person) = self.persons.get(&public_key) {
            person.update(cx, |person, cx| {
                person.set_emojis(emojis);
                cx.notify();
            });
            cx.emit(PersonEvent::Updated(person.read(cx).clone()));
        } else {
            let person = Person::new(public_key, Metadata::default()).with_emojis(emojis);
            self.insert(person, cx);
        }
    }

    /// Insert batch of persons
    fn bulk_insert(&mut self, persons: Vec<Person>, cx: &mut Context<Self>) {
        for person in persons {
            let public_key = person.public_key();

            if self.persons.contains_key(&public_key) {
                continue;
            }

            cx.emit(PersonEvent::Updated(person.clone()));
            self.persons.insert(public_key, cx.new(|_| person));
        }
        cx.notify();
    }

    /// Insert or update a person
    pub fn insert(&mut self, person: Person, cx: &mut Context<Self>) {
        let public_key = person.public_key();
        self.release_in_flight(&public_key);

        match self.persons.get(&public_key) {
            Some(this) => {
                this.update(cx, |this, cx| {
                    this.set_metadata(person.metadata());
                    cx.notify();
                });
                cx.emit(PersonEvent::Updated(this.read(cx).clone()));
            }
            None => {
                cx.emit(PersonEvent::Updated(person.clone()));
                self.persons.insert(public_key, cx.new(|_| person));
                // Views rendering a placeholder only observed this registry
                cx.notify();
            }
        }
    }

    /// Get single person by public key
    pub fn get(&self, public_key: &PublicKey, cx: &App) -> Person {
        if let Some(person) = self.persons.get(public_key) {
            return person.read(cx).clone();
        }

        let public_key = *public_key;

        if self.seen.write().unwrap().insert(public_key) {
            let sender = self.sender.clone();

            // Spawn background task to request metadata
            cx.background_spawn(async move {
                if let Err(e) = sender.send_async(public_key).await {
                    log::warn!("Failed to send public key for metadata request: {e}");
                }
            })
            .detach();
        }

        // Return a temporary profile with default metadata
        Person::new(public_key, Metadata::default())
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

    /// The signed-in user's custom emoji
    pub fn current_emojis(cx: &App) -> Vec<(String, Url)> {
        let registry = Self::global(cx);
        let nostr = NostrRegistry::global(cx);

        let Some(public_key) = nostr.read(cx).current_user() else {
            return Vec::new();
        };

        registry.read(cx).get(&public_key, cx).emojis().to_vec()
    }

    /// An `emoji` tag for every signed-in user's custom emoji referenced.
    pub fn emoji_tags(content: &str, cx: &App) -> Vec<Tag> {
        let registry = Self::global(cx);
        let nostr = NostrRegistry::global(cx);
        let Some(public_key) = nostr.read(cx).current_user() else {
            return Vec::new();
        };

        registry.read(cx).get(&public_key, cx).emoji_tags(content)
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
