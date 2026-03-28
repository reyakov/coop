use std::cmp::Reverse;
use std::collections::{BTreeSet, HashMap, HashSet};
use std::hash::{DefaultHasher, Hash, Hasher};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use anyhow::{Context as AnyhowContext, Error, anyhow};
use common::EventUtils;
use fuzzy_matcher::FuzzyMatcher;
use fuzzy_matcher::skim::SkimMatcherV2;
use gpui::{
    App, AppContext, Context, Entity, EventEmitter, Global, SharedString, Subscription, Task,
    WeakEntity, Window,
};
use nostr_sdk::prelude::*;
use smallvec::{SmallVec, smallvec};
use smol::lock::RwLock;
use state::{CoopSigner, DEVICE_GIFTWRAP, NostrRegistry, StateEvent, TIMEOUT, USER_GIFTWRAP};

mod message;
mod room;

pub use message::*;
pub use room::*;

pub fn init(window: &mut Window, cx: &mut App) {
    ChatRegistry::set_global(cx.new(|cx| ChatRegistry::new(window, cx)), cx);
}

struct GlobalChatRegistry(Entity<ChatRegistry>);

impl Global for GlobalChatRegistry {}

/// Chat event.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub enum ChatEvent {
    /// An event to open a room by its ID
    OpenRoom(u64),
    /// An event to close a room by its ID
    CloseRoom(u64),
    /// An event to notify UI about a new chat request
    Ping,
    /// An event to notify UI that the chat registry has subscribed to messaging relays
    Subscribed,
    /// An error occurred
    Error(SharedString),
}

/// Channel signal.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
enum Signal {
    /// Message received from relay pool
    Message(NewMessage),
    /// Eose received from relay pool
    Eose,
    /// An error occurred
    Error(FailedMessage),
}

impl Signal {
    pub fn message(gift_wrap: EventId, rumor: UnsignedEvent) -> Self {
        Self::Message(NewMessage::new(gift_wrap, rumor))
    }

    pub fn eose() -> Self {
        Self::Eose
    }

    pub fn error<T>(event: &Event, reason: T) -> Self
    where
        T: Into<SharedString>,
    {
        Self::Error(FailedMessage::new(event, reason))
    }
}

/// Chat Registry
#[derive(Debug)]
pub struct ChatRegistry {
    /// Chat rooms
    rooms: Vec<Entity<Room>>,

    /// Events that failed to unwrap for any reason
    trashes: Entity<BTreeSet<FailedMessage>>,

    /// Tracking events seen on which relays in the current session
    seens: Arc<RwLock<HashMap<EventId, HashSet<RelayUrl>>>>,

    /// Tracking the status of unwrapping gift wrap events.
    tracking_flag: Arc<AtomicBool>,

    /// Channel for sending signals to the UI.
    signal_tx: flume::Sender<Signal>,

    /// Channel for receiving signals from the UI.
    signal_rx: flume::Receiver<Signal>,

    /// Async tasks
    tasks: SmallVec<[Task<Result<(), Error>>; 2]>,

    /// Subscriptions
    _subscriptions: SmallVec<[Subscription; 1]>,
}

impl EventEmitter<ChatEvent> for ChatRegistry {}

impl ChatRegistry {
    /// Retrieve the global chat registry state
    pub fn global(cx: &App) -> Entity<Self> {
        cx.global::<GlobalChatRegistry>().0.clone()
    }

    /// Set the global chat registry instance
    fn set_global(state: Entity<Self>, cx: &mut App) {
        cx.set_global(GlobalChatRegistry(state));
    }

    /// Create a new chat registry instance
    fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let nostr = NostrRegistry::global(cx);
        let (tx, rx) = flume::unbounded::<Signal>();
        let mut subscriptions = smallvec![];

        subscriptions.push(
            // Subscribe to the signer event
            cx.subscribe(&nostr, |this, _state, event, cx| {
                match event {
                    StateEvent::SignerSet => {
                        this.reset(cx);
                        this.get_rooms(cx);
                    }
                    StateEvent::RelayConnected => {
                        this.get_contact_list(cx);
                        this.get_messages(cx)
                    }
                    _ => {}
                };
            }),
        );

        // Run at the end of the current cycle
        cx.defer_in(window, |this, _window, cx| {
            this.get_rooms(cx);
            this.handle_notifications(cx);
            this.tracking(cx);
        });

        Self {
            rooms: vec![],
            trashes: cx.new(|_| BTreeSet::default()),
            seens: Arc::new(RwLock::new(HashMap::default())),
            tracking_flag: Arc::new(AtomicBool::new(false)),
            signal_rx: rx,
            signal_tx: tx,
            tasks: smallvec![],
            _subscriptions: subscriptions,
        }
    }

    /// Handle nostr notifications
    fn handle_notifications(&mut self, cx: &mut Context<Self>) {
        let nostr = NostrRegistry::global(cx);
        let client = nostr.read(cx).client();
        let signer = nostr.read(cx).signer();
        let status = self.tracking_flag.clone();
        let seens = self.seens.clone();
        let trashes = self.trashes.downgrade();

        let initialized_at = Timestamp::now();
        let sub_id1 = SubscriptionId::new(DEVICE_GIFTWRAP);
        let sub_id2 = SubscriptionId::new(USER_GIFTWRAP);

        // Channel for communication between nostr and gpui
        let tx = self.signal_tx.clone();
        let rx = self.signal_rx.clone();

        self.tasks.push(cx.background_spawn(async move {
            let mut notifications = client.notifications();
            let mut processed_events = HashSet::new();

            while let Some(notification) = notifications.next().await {
                let ClientNotification::Message { message, relay_url } = notification else {
                    // Skip non-message notifications
                    continue;
                };

                match *message {
                    RelayMessage::Event { event, .. } => {
                        // Keep track of which relays have seen this event
                        {
                            let mut seens = seens.write().await;
                            seens.entry(event.id).or_default().insert(relay_url);
                        }

                        // De-duplicate events by their ID
                        if !processed_events.insert(event.id) {
                            continue;
                        }

                        // Skip non-gift wrap events
                        if event.kind != Kind::GiftWrap {
                            continue;
                        }

                        // Extract the rumor from the gift wrap event
                        match extract_rumor(&client, &signer, event.as_ref()).await {
                            Ok(rumor) => {
                                if rumor.tags.is_empty() {
                                    let signal =
                                        Signal::error(event.as_ref(), "Recipient is missing");
                                    tx.send_async(signal).await?;

                                    continue;
                                }

                                if rumor.created_at >= initialized_at {
                                    let signal = Signal::message(event.id, rumor);
                                    tx.send_async(signal).await?;
                                } else {
                                    status.store(true, Ordering::Release);
                                }
                            }
                            Err(e) => {
                                let reason = format!("Failed to extract rumor: {e}");
                                let signal = Signal::error(event.as_ref(), reason);
                                tx.send_async(signal).await?;
                            }
                        }
                    }
                    RelayMessage::EndOfStoredEvents(id) => {
                        if id.as_ref() == &sub_id1 || id.as_ref() == &sub_id2 {
                            tx.send_async(Signal::eose()).await?;
                        }
                    }
                    _ => {}
                }
            }

            Ok(())
        }));

        self.tasks.push(cx.spawn(async move |this, cx| {
            while let Ok(message) = rx.recv_async().await {
                match message {
                    Signal::Message(message) => {
                        this.update(cx, |this, cx| {
                            this.new_message(message, cx);
                        })?;
                    }
                    Signal::Eose => {
                        this.update(cx, |this, cx| {
                            this.get_rooms(cx);
                        })?;
                    }
                    Signal::Error(trash) => {
                        trashes.update(cx, |this, cx| {
                            this.insert(trash);
                            cx.notify();
                        })?;
                    }
                };
            }

            Ok(())
        }));
    }

    /// Tracking the status of unwrapping gift wrap events.
    fn tracking(&mut self, cx: &mut Context<Self>) {
        let status = self.tracking_flag.clone();
        let tx = self.signal_tx.clone();

        self.tasks.push(cx.background_spawn(async move {
            let loop_duration = Duration::from_secs(15);

            loop {
                if status.load(Ordering::Acquire) {
                    _ = status.compare_exchange(true, false, Ordering::Release, Ordering::Relaxed);
                    _ = tx.send_async(Signal::Eose).await;
                } else {
                    _ = tx.send_async(Signal::Eose).await;
                }
                smol::Timer::after(loop_duration).await;
            }
        }));
    }

    /// Get contact list from relays
    pub fn get_contact_list(&mut self, cx: &mut Context<Self>) {
        let nostr = NostrRegistry::global(cx);
        let client = nostr.read(cx).client();
        let signer = nostr.read(cx).signer();

        let Some(public_key) = signer.public_key() else {
            return;
        };

        let task: Task<Result<(), Error>> = cx.background_spawn(async move {
            let id = SubscriptionId::new("contact-list");
            let opts = SubscribeAutoCloseOptions::default()
                .exit_policy(ReqExitPolicy::ExitOnEOSE)
                .timeout(Some(Duration::from_secs(TIMEOUT)));

            // Construct filter for inbox relays
            let filter = Filter::new()
                .kind(Kind::ContactList)
                .author(public_key)
                .limit(1);

            // Subscribe
            client.subscribe(filter).close_on(opts).with_id(id).await?;

            Ok(())
        });

        self.tasks.push(task);
    }

    /// Get all messages for current user
    pub fn get_messages(&mut self, cx: &mut Context<Self>) {
        let task = self.subscribe(cx);

        self.tasks.push(cx.spawn(async move |this, cx| {
            match task.await {
                Ok(_) => {
                    this.update(cx, |_this, cx| {
                        cx.emit(ChatEvent::Subscribed);
                    })?;
                }
                Err(e) => {
                    this.update(cx, |_this, cx| {
                        cx.emit(ChatEvent::Error(SharedString::from(e.to_string())));
                    })?;
                }
            }
            Ok(())
        }));
    }

    // Get messaging relay list for current user
    fn get_messaging_relays(&self, cx: &App) -> Task<Result<Vec<RelayUrl>, Error>> {
        let nostr = NostrRegistry::global(cx);
        let client = nostr.read(cx).client();
        let signer = nostr.read(cx).signer();

        cx.background_spawn(async move {
            let public_key = signer.get_public_key().await?;

            // Construct filter for inbox relays
            let filter = Filter::new()
                .kind(Kind::InboxRelays)
                .author(public_key)
                .limit(1);

            // Stream events from user's write relays
            let mut stream = client
                .stream_events(filter)
                .timeout(Duration::from_secs(TIMEOUT))
                .await?;

            while let Some((_url, res)) = stream.next().await {
                if let Ok(event) = res {
                    log::debug!("Got event: {:?}", event);
                    let urls: Vec<RelayUrl> = nip17::extract_owned_relay_list(event).collect();
                    return Ok(urls);
                }
            }

            Err(anyhow!("Messaging Relays not found"))
        })
    }

    /// Continuously get gift wrap events for the current user in their messaging relays
    fn subscribe(&self, cx: &App) -> Task<Result<(), Error>> {
        let nostr = NostrRegistry::global(cx);
        let client = nostr.read(cx).client();
        let signer = nostr.read(cx).signer();
        let urls = self.get_messaging_relays(cx);

        cx.background_spawn(async move {
            let urls = urls.await?;
            let public_key = signer.get_public_key().await?;
            let filter = Filter::new().kind(Kind::GiftWrap).pubkey(public_key);
            let id = SubscriptionId::new(USER_GIFTWRAP);

            // Ensure relay connections
            for url in urls.iter() {
                client.add_relay(url).and_connect().await?;
            }

            // Construct target for subscription
            let target: HashMap<RelayUrl, Filter> = urls
                .into_iter()
                .map(|relay| (relay, filter.clone()))
                .collect();

            let output = client.subscribe(target).with_id(id).await?;

            log::info!(
                "Successfully subscribed to gift-wrap messages on: {:?}",
                output.success
            );

            Ok(())
        })
    }

    /// Get the loading status of the chat registry
    pub fn loading(&self) -> bool {
        self.tracking_flag.load(Ordering::Acquire)
    }

    /// Get a weak reference to a room by its ID.
    pub fn room(&self, id: &u64, cx: &App) -> Option<WeakEntity<Room>> {
        self.rooms
            .iter()
            .find(|this| &this.read(cx).id == id)
            .map(|this| this.downgrade())
    }

    /// Get all rooms based on the filter.
    pub fn rooms(&self, filter: &RoomKind, cx: &App) -> Vec<Entity<Room>> {
        self.rooms
            .iter()
            .filter(|room| &room.read(cx).kind == filter)
            .cloned()
            .collect()
    }

    /// Count the number of rooms based on the filter.
    pub fn count(&self, filter: &RoomKind, cx: &App) -> usize {
        self.rooms
            .iter()
            .filter(|room| &room.read(cx).kind == filter)
            .count()
    }

    /// Count the number of messages seen by a given relay.
    pub fn count_messages(&self, relay_url: &RelayUrl) -> usize {
        self.seens
            .read_blocking()
            .values()
            .filter(|seen| seen.contains(relay_url))
            .count()
    }

    /// Get the relays that have seen a given message.
    pub fn seen_on(&self, id: &EventId) -> HashSet<RelayUrl> {
        self.seens
            .read_blocking()
            .get(id)
            .cloned()
            .unwrap_or_default()
    }

    /// Add a new room to the start of list.
    pub fn add_room<I>(&mut self, room: I, cx: &mut Context<Self>)
    where
        I: Into<Room> + 'static,
    {
        let nostr = NostrRegistry::global(cx);
        let client = nostr.read(cx).client();

        cx.spawn(async move |this, cx| {
            let signer = client.signer()?;
            let public_key = signer.get_public_key().await.ok()?;
            let room: Room = room.into().organize(&public_key);

            this.update(cx, |this, cx| {
                this.rooms.insert(0, cx.new(|_| room));
                cx.emit(ChatEvent::Ping);
                cx.notify();
            })
            .ok()
        })
        .detach();
    }

    /// Emit an open room event.
    ///
    /// If the room is new, add it to the registry.
    pub fn emit_room(&mut self, room: &Entity<Room>, cx: &mut Context<Self>) {
        // Get the room's ID.
        let id = room.read(cx).id;

        // If the room is new, add it to the registry.
        if !self.rooms.iter().any(|r| r.read(cx).id == id) {
            self.rooms.insert(0, room.to_owned());
        }

        // Emit the open room event.
        cx.emit(ChatEvent::OpenRoom(id));
    }

    /// Close a room.
    pub fn close_room(&mut self, id: u64, cx: &mut Context<Self>) {
        if self.rooms.iter().any(|r| r.read(cx).id == id) {
            cx.emit(ChatEvent::CloseRoom(id));
        }
    }

    /// Sort rooms by their created at.
    pub fn sort(&mut self, cx: &mut Context<Self>) {
        self.rooms.sort_by_key(|ev| Reverse(ev.read(cx).created_at));
        cx.notify();
    }

    /// Finding rooms based on a query.
    pub fn find(&self, query: &str, cx: &App) -> Vec<Entity<Room>> {
        let matcher = SkimMatcherV2::default();

        if let Ok(public_key) = PublicKey::parse(query) {
            self.rooms
                .iter()
                .filter(|room| room.read(cx).members.contains(&public_key))
                .cloned()
                .collect()
        } else {
            self.rooms
                .iter()
                .filter(|room| {
                    matcher
                        .fuzzy_match(room.read(cx).display_name(cx).as_ref(), query)
                        .is_some()
                })
                .cloned()
                .collect()
        }
    }

    /// Reset the registry.
    pub fn reset(&mut self, cx: &mut Context<Self>) {
        self.rooms.clear();
        cx.notify();
    }

    /// Extend the registry with new rooms.
    fn extend_rooms(&mut self, rooms: HashSet<Room>, cx: &mut Context<Self>) {
        let mut room_map: HashMap<u64, usize> = self
            .rooms
            .iter()
            .enumerate()
            .map(|(idx, room)| (room.read(cx).id, idx))
            .collect();

        for new_room in rooms.into_iter() {
            // Check if we already have a room with this ID
            if let Some(&index) = room_map.get(&new_room.id) {
                self.rooms[index].update(cx, |this, cx| {
                    if new_room.created_at > this.created_at {
                        *this = new_room;
                        cx.notify();
                    }
                });
            } else {
                let new_room_id = new_room.id;
                self.rooms.push(cx.new(|_| new_room));

                let new_index = self.rooms.len();
                room_map.insert(new_room_id, new_index);
            }
        }
    }

    /// Load all rooms from the database.
    pub fn get_rooms(&mut self, cx: &mut Context<Self>) {
        let task = self.get_rooms_from_database(cx);

        self.tasks.push(cx.spawn(async move |this, cx| {
            match task.await {
                Ok(rooms) => {
                    this.update(cx, |this, cx| {
                        this.extend_rooms(rooms, cx);
                        this.sort(cx);
                    })?;
                }
                Err(e) => {
                    log::error!("Failed to load rooms: {}", e);
                }
            };

            Ok(())
        }));
    }

    /// Create a task to load rooms from the database
    fn get_rooms_from_database(&self, cx: &App) -> Task<Result<HashSet<Room>, Error>> {
        let nostr = NostrRegistry::global(cx);
        let client = nostr.read(cx).client();

        cx.background_spawn(async move {
            let signer = client.signer().context("Signer not found")?;
            let public_key = signer.get_public_key().await?;

            // Get contacts
            let contacts = client
                .database()
                .contacts_public_keys(public_key)
                .await
                .unwrap_or_default();

            // Construct authored filter
            let authored_filter = Filter::new()
                .kind(Kind::ApplicationSpecificData)
                .custom_tag(SingleLetterTag::lowercase(Alphabet::A), public_key);

            // Get all authored events
            let authored = client.database().query(authored_filter).await?;

            // Construct addressed filter
            let addressed_filter = Filter::new()
                .kind(Kind::ApplicationSpecificData)
                .custom_tag(SingleLetterTag::lowercase(Alphabet::P), public_key);

            // Get all addressed events
            let addressed = client.database().query(addressed_filter).await?;

            // Merge authored and addressed events
            let events = authored.merge(addressed);

            // Collect results
            let mut rooms: HashSet<Room> = HashSet::new();
            let mut grouped: HashMap<u64, Vec<UnsignedEvent>> = HashMap::new();

            // Process each event and group by room hash
            for raw in events.into_iter() {
                if let Ok(rumor) = UnsignedEvent::from_json(&raw.content)
                    && rumor.tags.public_keys().peekable().peek().is_some()
                {
                    grouped.entry(rumor.uniq_id()).or_default().push(rumor);
                }
            }

            for (_id, mut messages) in grouped.into_iter() {
                messages.sort_by_key(|m| Reverse(m.created_at));

                // Always use the latest message
                let Some(latest) = messages.first() else {
                    continue;
                };

                // Construct the room from the latest message.
                //
                // Call `.organize` to ensure the current user is at the end of the list.
                let mut room = Room::from(latest).organize(&public_key);

                // Check if the user has responded to the room
                let user_sent = messages.iter().any(|m| m.pubkey == public_key);

                // Check if public keys are from the user's contacts
                let is_contact = room.members.iter().any(|k| contacts.contains(k));

                // Set the room's kind based on status
                if user_sent || is_contact {
                    room = room.kind(RoomKind::Ongoing);
                }

                rooms.insert(room);
            }

            Ok(rooms)
        })
    }

    /// Parse a nostr event into a message and push it to the belonging room
    ///
    /// If the room doesn't exist, it will be created.
    /// Updates room ordering based on the most recent messages.
    pub fn new_message(&mut self, message: NewMessage, cx: &mut Context<Self>) {
        let nostr = NostrRegistry::global(cx);
        let signer = nostr.read(cx).signer();

        match self.rooms.iter().find(|e| e.read(cx).id == message.room) {
            Some(room) => {
                room.update(cx, |this, cx| {
                    if this.kind == RoomKind::Request
                        && let Some(public_key) = signer.public_key()
                        && message.rumor.pubkey == public_key
                    {
                        this.set_ongoing(cx);
                    }
                    this.push_message(message, cx);
                });
                self.sort(cx);
            }
            None => {
                // Push the new room to the front of the list
                self.add_room(message.rumor, cx);
            }
        }
    }

    /// Trigger a refresh of the opened chat rooms by their IDs
    pub fn refresh_rooms(&mut self, ids: &[u64], cx: &mut Context<Self>) {
        for room in self.rooms.iter() {
            if ids.contains(&room.read(cx).id) {
                room.update(cx, |this, cx| {
                    this.emit_refresh(cx);
                });
            }
        }
    }
}

/// Unwraps a gift-wrapped event and processes its contents.
async fn extract_rumor(
    client: &Client,
    signer: &Arc<CoopSigner>,
    gift_wrap: &Event,
) -> Result<UnsignedEvent, Error> {
    // Try to get cached rumor first
    if let Ok(rumor) = get_rumor(client, gift_wrap.id).await {
        return Ok(rumor);
    }

    // Try to unwrap with the available signer
    let unwrapped = try_unwrap(signer, gift_wrap).await?;
    let mut rumor = unwrapped.rumor;

    // Generate event id for the rumor if it doesn't have one
    rumor.ensure_id();

    // Cache the rumor
    if let Err(e) = set_rumor(client, gift_wrap.id, &rumor).await {
        log::error!("Failed to cache rumor: {e:?}");
    }

    Ok(rumor)
}

/// Helper method to try unwrapping with different signers
async fn try_unwrap(signer: &Arc<CoopSigner>, gift_wrap: &Event) -> Result<UnwrappedGift, Error> {
    // Try with the device signer first
    if let Some(signer) = signer.get_encryption_signer().await {
        log::info!("trying with encryption key");
        if let Ok(unwrapped) = try_unwrap_with(gift_wrap, &signer).await {
            return Ok(unwrapped);
        }
    }

    // Fallback to the user's signer
    let user_signer = signer.get().await;
    let unwrapped = try_unwrap_with(gift_wrap, &user_signer).await?;

    Ok(unwrapped)
}

/// Attempts to unwrap a gift wrap event with a given signer.
async fn try_unwrap_with<T>(gift_wrap: &Event, signer: &T) -> Result<UnwrappedGift, Error>
where
    T: NostrSigner + 'static,
{
    // Get the sealed event
    let seal = signer
        .nip44_decrypt(&gift_wrap.pubkey, &gift_wrap.content)
        .await?;

    // Verify the sealed event
    let seal: Event = Event::from_json(seal)?;
    seal.verify_with_ctx(&SECP256K1)?;

    // Get the rumor event
    let rumor = signer.nip44_decrypt(&seal.pubkey, &seal.content).await?;
    let rumor = UnsignedEvent::from_json(rumor)?;

    Ok(UnwrappedGift {
        sender: seal.pubkey,
        rumor,
    })
}

/// Stores an unwrapped event in local database with reference to original
async fn set_rumor(client: &Client, id: EventId, rumor: &UnsignedEvent) -> Result<(), Error> {
    let rumor_id = rumor.id.context("Rumor is missing an event id")?;
    let author = rumor.pubkey;
    let conversation = conversation_id(rumor);

    let mut tags = rumor.tags.clone().to_vec();

    // Add a unique identifier
    tags.push(Tag::identifier(id));

    // Add a reference to the rumor's author
    tags.push(Tag::custom(
        TagKind::SingleLetter(SingleLetterTag::lowercase(Alphabet::A)),
        [author],
    ));

    // Add a conversation id
    tags.push(Tag::custom(
        TagKind::SingleLetter(SingleLetterTag::lowercase(Alphabet::C)),
        [conversation.to_string()],
    ));

    // Add a reference to the rumor's id
    tags.push(Tag::event(rumor_id));

    // Add references to the rumor's participants
    for receiver in rumor.tags.public_keys().copied() {
        tags.push(Tag::custom(
            TagKind::SingleLetter(SingleLetterTag::lowercase(Alphabet::P)),
            [receiver],
        ));
    }

    // Convert rumor to json
    let content = rumor.as_json();

    // Construct the event
    let event = EventBuilder::new(Kind::ApplicationSpecificData, content)
        .tags(tags)
        .sign(&Keys::generate())
        .await?;

    // Save the event to the database
    client.database().save_event(&event).await?;

    Ok(())
}

/// Retrieves a previously unwrapped event from local database
async fn get_rumor(client: &Client, gift_wrap: EventId) -> Result<UnsignedEvent, Error> {
    let filter = Filter::new()
        .kind(Kind::ApplicationSpecificData)
        .identifier(gift_wrap)
        .limit(1);

    if let Some(event) = client.database().query(filter).await?.first_owned() {
        UnsignedEvent::from_json(event.content).map_err(|e| anyhow!(e))
    } else {
        Err(anyhow!("Event is not cached yet."))
    }
}

/// Get the conversation ID for a given rumor (message).
fn conversation_id(rumor: &UnsignedEvent) -> u64 {
    let mut hasher = DefaultHasher::new();
    let mut pubkeys: Vec<PublicKey> = rumor.tags.public_keys().copied().collect();
    pubkeys.push(rumor.pubkey);
    pubkeys.sort();
    pubkeys.dedup();
    pubkeys.hash(&mut hasher);

    hasher.finish()
}
