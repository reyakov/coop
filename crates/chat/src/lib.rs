use std::cmp::Reverse;
use std::collections::{BTreeSet, HashMap, HashSet, hash_map};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, LazyLock, RwLock};

use anyhow::{Error, anyhow};
use common::EventExt;
use fuzzy_matcher::FuzzyMatcher;
use fuzzy_matcher::skim::SkimMatcherV2;
use gpui::{
    App, AppContext, Context, Entity, EventEmitter, Global, SharedString, Subscription, Task,
    WeakEntity, Window,
};
use instant::Duration;
use nostr_sdk::prelude::*;
use smallvec::{SmallVec, smallvec};
use state::{DEVICE_GIFTWRAP, NostrRegistry, USER_GIFTWRAP, UniversalSigner};

mod message;
mod room;

pub use message::*;
pub use room::*;

/// A static keypair used only for signing locally-cached rumor events.
static LOCAL_KEYS: LazyLock<Keys> = LazyLock::new(Keys::generate);

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
    /// No Inbox Relays found, the app is not ready to subscribe messages
    InboxRelayNotFound,
    /// An error occurred
    Error(String),
}

/// Channel signal.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
enum Signal {
    /// Inbox Relays found, the app is ready to subscribe messages
    InboxReady,
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

    /// O(1) room lookup by room ID
    room_index: HashMap<u64, Entity<Room>>,

    /// Events that failed to unwrap for any reason
    trash: Entity<BTreeSet<FailedMessage>>,

    /// Tracking events seen on which relays in the current session
    seen: Arc<RwLock<HashMap<EventId, HashSet<RelayUrl>>>>,

    /// Mapping of unwrapped event ids to their gift wrap event ids
    event_map: Arc<RwLock<HashMap<EventId, EventId>>>,

    /// True while the initial event backlog is still loading
    tracking: Arc<AtomicBool>,

    /// Channel for sending signals to the UI.
    signal_tx: flume::Sender<Signal>,

    /// Channel for receiving signals from the UI.
    signal_rx: flume::Receiver<Signal>,

    /// Async tasks
    tasks: SmallVec<[Task<Result<(), Error>>; 2]>,

    /// Notification listener task (cancelled on signer change)
    notification_listener: Option<Task<Result<(), Error>>>,

    /// Signal consumer task (cancelled on signer change)
    signal_consumer: Option<Task<Result<(), Error>>>,

    /// Fuzzy matcher for room search (cached; intentionally excluded from Debug)
    #[allow(dead_code)]
    matcher: CachedMatcher,

    /// Subscriptions
    _subscriptions: SmallVec<[Subscription; 2]>,
}

/// Wrapper to provide Debug for SkimMatcherV2
struct CachedMatcher(SkimMatcherV2);

impl std::fmt::Debug for CachedMatcher {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("CachedMatcher { .. }")
    }
}

impl std::ops::Deref for CachedMatcher {
    type Target = SkimMatcherV2;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
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
            cx.subscribe(&nostr, |this, _nostr, event, cx| {
                if event.signer_changed() {
                    this.reset(cx);
                    this.handle_notifications(cx);
                    this.get_metadata(cx);
                    this.get_rooms(cx);
                };
            }),
        );

        // Run at the end of the current cycle
        cx.defer_in(window, |this, _window, cx| {
            this.get_rooms(cx);
        });

        Self {
            rooms: vec![],
            room_index: HashMap::new(),
            trash: cx.new(|_| BTreeSet::default()),
            seen: Arc::new(RwLock::new(HashMap::default())),
            event_map: Arc::new(RwLock::new(HashMap::default())),
            tracking: Arc::new(AtomicBool::new(true)),
            matcher: CachedMatcher(SkimMatcherV2::default()),
            signal_rx: rx,
            signal_tx: tx,
            tasks: smallvec![],
            notification_listener: None,
            signal_consumer: None,
            _subscriptions: subscriptions,
        }
    }

    /// Handle nostr notifications
    fn handle_notifications(&mut self, cx: &mut Context<Self>) {
        // Cancel previous notification tasks before spawning new ones
        self.notification_listener = None;
        self.signal_consumer = None;

        let nostr = NostrRegistry::global(cx);
        let client = nostr.read(cx).client();
        let signer = nostr.read(cx).signer();

        let seen = self.seen.clone();
        let event_map = self.event_map.clone();
        let trash = self.trash.downgrade();

        let sub_id1 = SubscriptionId::new(DEVICE_GIFTWRAP);
        let sub_id2 = SubscriptionId::new(USER_GIFTWRAP);

        // Channel for communication between nostr and gpui
        let tx = self.signal_tx.clone();
        let rx = self.signal_rx.clone();

        self.notification_listener = Some(cx.background_spawn(async move {
            let mut notifications = client.notifications();
            let mut processed_events = HashSet::new();
            const MAX_PROCESSED: usize = 10_000;

            while let Some(notification) = notifications.next().await {
                let ClientNotification::Message { message, relay_url } = notification else {
                    continue;
                };

                match *message {
                    RelayMessage::Event { event, .. } => {
                        // Prune the dedup set before it grows unbounded
                        if processed_events.len() >= MAX_PROCESSED {
                            processed_events.clear();
                        }
                        if !processed_events.insert(event.id) {
                            continue;
                        }

                        // Handle msg relays event to determine when the app is ready to subscribe
                        if event.kind == Kind::InboxRelays {
                            let current_user = signer.get_public_key_async().await?;
                            if event.pubkey == current_user {
                                tx.send_async(Signal::InboxReady).await.ok();
                            }
                        }

                        // Skip non-gift wrap events
                        if event.kind != Kind::GiftWrap {
                            continue;
                        }

                        // Keep track of which relays have seen this event
                        {
                            let mut seen = seen.write().unwrap();
                            seen.entry(event.id).or_default().insert(relay_url);
                        }

                        // Extract the rumor from the gift wrap event
                        match extract_rumor(&client, &signer, event.as_ref()).await {
                            Ok(rumor) => {
                                let Some(rumor_id) = rumor.id else {
                                    log::error!("Rumor missing id after ensure_id");
                                    continue;
                                };
                                {
                                    let mut event_map = event_map.write().unwrap();
                                    event_map.insert(rumor_id, event.id);
                                }

                                if rumor.tags.is_empty() {
                                    let signal = Signal::error(&event, "Recipient is missing");
                                    tx.send_async(signal).await.ok();
                                }

                                // Emit message for both new and backlog events
                                let signal = Signal::message(event.id, rumor);
                                tx.send_async(signal).await.ok();
                            }
                            Err(e) => {
                                let reason = format!("Failed to extract rumor: {e}");
                                let signal = Signal::error(event.as_ref(), reason);
                                tx.send_async(signal).await.ok();
                            }
                        }
                    }
                    RelayMessage::EndOfStoredEvents(id)
                        if (id.as_ref() == &sub_id1 || id.as_ref() == &sub_id2) =>
                    {
                        tx.send_async(Signal::Eose).await.ok();
                    }
                    _ => {}
                }
            }

            Ok(())
        }));

        self.signal_consumer = Some(cx.spawn(async move |this, cx| {
            while let Ok(message) = rx.recv_async().await {
                // `update_in` (rather than `update`) routes through a
                // try-borrow: on wasm a task poll that lands while the app
                // context is borrowed can't panic and kill this consumer
                // (which would stall all message delivery).
                this.update_in(cx, |this, _window, cx| {
                    // Drain the whole queue in a single update so a burst of
                    // events (e.g. history sync after login) collapses into
                    // one repaint instead of one per message (important on
                    // wasm, where everything runs on the main thread).
                    let mut batch = vec![message];
                    while let Ok(extra) = rx.try_recv() {
                        batch.push(extra);
                    }

                    for message in batch {
                        match message {
                            Signal::Message(message) => {
                                this.new_message(message, cx);
                            }
                            Signal::InboxReady => {
                                this.get_messages(cx);
                            }
                            Signal::Eose => {
                                this.tracking.store(false, Ordering::Release);
                                this.get_rooms(cx);
                            }
                            Signal::Error(failed) => {
                                let _ = trash.update(cx, |this, cx| {
                                    this.insert(failed);
                                    cx.notify();
                                });
                            }
                        };
                    }
                })?;
            }

            Ok(())
        }));
    }

    /// Get all necessary metadata from relays for current user
    pub fn get_metadata(&mut self, cx: &mut Context<Self>) {
        let nostr = NostrRegistry::global(cx);
        let client = nostr.read(cx).client();

        let Some(public_key) = nostr.read(cx).current_user() else {
            return;
        };

        self.tasks.push(cx.spawn(async move |this, cx| {
            // Subscribe to metadata from relays
            let opts = SubscribeAutoCloseOptions::default().exit_policy(ReqExitPolicy::ExitOnEOSE);

            let msg_relays = Filter::new()
                .kind(Kind::InboxRelays)
                .author(public_key)
                .limit(1);

            let contact_list = Filter::new()
                .kind(Kind::ContactList)
                .author(public_key)
                .limit(1);

            _ = client
                .subscribe(vec![msg_relays, contact_list])
                .close_on(opts)
                .await;

            // Give relays time to respond
            cx.background_executor().timer(Duration::from_secs(5)).await;

            // Verify inbox relays were received
            let filter = Filter::new()
                .kind(Kind::InboxRelays)
                .author(public_key)
                .limit(1);

            let found = client
                .database()
                .query(filter)
                .await
                .unwrap_or_default()
                .into_iter()
                .next()
                .is_some();

            if !found {
                this.update_in(cx, |_this, _window, cx| {
                    cx.emit(ChatEvent::InboxRelayNotFound);
                })?;
            }

            Ok(())
        }));
    }

    /// Get all messages for the provided signer
    fn get_messages(&mut self, cx: &mut Context<Self>) {
        let nostr = NostrRegistry::global(cx);
        let client = nostr.read(cx).client();
        let signer = nostr.read(cx).signer();

        self.tasks.push(cx.spawn(async move |this, cx| {
            let task: Task<Result<(), Error>> = cx.background_spawn(async move {
                let public_key = signer.get_public_key_async().await?;

                let filter = Filter::new()
                    .kind(Kind::InboxRelays)
                    .author(public_key)
                    .limit(1);

                let event = client
                    .database()
                    .query(filter)
                    .await?
                    .into_iter()
                    .next()
                    .ok_or(anyhow::anyhow!("No inbox relays found"))?;

                let relays: Vec<RelayUrl> = nip17::extract_relay_list(&event).collect();
                for url in relays.iter() {
                    client.add_relay(url).and_connect().await?;
                }

                let filter = Filter::new().kind(Kind::GiftWrap).pubkey(public_key);
                let id = SubscriptionId::new(USER_GIFTWRAP);

                let target: HashMap<RelayUrl, Filter> = relays
                    .into_iter()
                    .map(|relay| (relay, filter.clone()))
                    .collect();

                client.subscribe(target).with_id(id).await?;

                Ok(())
            });

            if let Err(e) = task.await {
                this.update_in(cx, |_this, _window, cx| {
                    cx.emit(ChatEvent::Error(e.to_string()));
                })?;
            }

            Ok(())
        }));
    }

    /// Get all messages for the provided signer
    /// Reload the chat registry, fetching messages and contact list from relays.
    pub fn reload(&mut self, cx: &mut Context<Self>) {
        self.reset(cx);
        self.get_metadata(cx);
        self.get_rooms(cx);
    }

    /// Get the loading status of the chat registry
    pub fn loading(&self) -> bool {
        self.tracking.load(Ordering::Acquire)
    }

    /// Get a weak reference to a room by its ID
    pub fn room(&self, id: &u64, _cx: &App) -> Option<WeakEntity<Room>> {
        self.room_index.get(id).map(|room| room.downgrade())
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
        self.seen
            .read()
            .unwrap()
            .values()
            .filter(|s| s.contains(relay_url))
            .count()
    }

    /// Count the number of trash messages.
    pub fn count_trash_messages(&self, cx: &App) -> usize {
        self.trash.read(cx).len()
    }

    /// Get the trash messages entity.
    pub fn trash(&self) -> Entity<BTreeSet<FailedMessage>> {
        self.trash.clone()
    }

    /// Get the relays that have seen a given rumor id.
    pub fn rumor_seen_on(&self, id: &EventId) -> Option<HashSet<RelayUrl>> {
        self.event_map
            .read()
            .unwrap()
            .get(id)
            .map(|id| self.seen_on(id))
    }

    /// Get the relays that have seen a given gift wrap id.
    pub fn seen_on(&self, id: &EventId) -> HashSet<RelayUrl> {
        self.seen
            .read()
            .unwrap()
            .get(id)
            .cloned()
            .unwrap_or_default()
    }

    /// Add a new room to the start of list.
    pub fn add_room<I>(&mut self, room: I, cx: &mut Context<Self>)
    where
        I: Into<Room>,
    {
        let nostr = NostrRegistry::global(cx);
        let Some(public_key) = nostr.read(cx).current_user() else {
            return;
        };

        let room: Room = room.into().organize(&public_key);
        let room_id = room.id;
        let entity = cx.new(|_| room);

        self.room_index.insert(room_id, entity.clone());
        self.rooms.insert(0, entity);

        cx.emit(ChatEvent::Ping);
        cx.notify();
    }

    /// Emit an open room event.
    ///
    /// If the room is new, add it to the registry.
    pub fn emit_room(&mut self, room: &Entity<Room>, window: &mut Window, cx: &mut Context<Self>) {
        // Get the room's ID.
        let id = room.read(cx).id;

        // If the room is new, add it to the registry and index.
        if let hash_map::Entry::Vacant(e) = self.room_index.entry(id) {
            let entity = room.to_owned();
            e.insert(entity.clone());
            self.rooms.insert(0, entity);
        }

        // Emit the open room event deferred to avoid re-entrant reads
        cx.defer_in(window, move |_this, _window, cx| {
            cx.emit(ChatEvent::OpenRoom(id));
        });
    }

    /// Close a room.
    pub fn close_room(&mut self, id: u64, window: &mut Window, cx: &mut Context<Self>) {
        if self.room_index.contains_key(&id) {
            self.room_index.remove(&id);
            self.rooms.retain(|r| r.read(cx).id != id);
            cx.defer_in(window, move |_this, _window, cx| {
                cx.emit(ChatEvent::CloseRoom(id));
            });
        }
    }

    /// Sort rooms by their created at. Only notifies if order changed.
    pub fn sort(&mut self, cx: &mut Context<Self>) {
        let before: Vec<_> = self.rooms.iter().map(|ev| ev.read(cx).id).collect();
        self.rooms.sort_by_key(|ev| Reverse(ev.read(cx).created_at));
        let after: Vec<_> = self.rooms.iter().map(|ev| ev.read(cx).id).collect();
        if before != after {
            cx.notify();
        }
    }

    /// Finding rooms based on a query.
    pub fn find(&self, query: &str, cx: &App) -> Vec<Entity<Room>> {
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
                    self.matcher
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
        self.room_index.clear();
        self.trash.update(cx, |this, cx| {
            this.clear();
            cx.notify();
        });
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
                let entity = cx.new(|_| new_room);
                self.room_index.insert(new_room_id, entity.clone());
                self.rooms.push(entity);

                let new_index = self.rooms.len();
                room_map.insert(new_room_id, new_index);
            }
        }
    }

    /// Load all rooms from the database.
    pub fn get_rooms(&mut self, cx: &mut Context<Self>) {
        let task = self.get_rooms_task(cx);

        self.tasks.push(cx.spawn(async move |this, cx| {
            match task.await {
                Ok(rooms) => {
                    this.update_in(cx, |this, _window, cx| {
                        this.extend_rooms(rooms, cx);
                        this.sort(cx);
                    })?;
                }
                Err(e) => {
                    this.update_in(cx, |_, _window, cx| {
                        cx.emit(ChatEvent::Error(e.to_string()));
                    })?;
                }
            };

            Ok(())
        }));
    }

    /// Create a task to load rooms from the database
    fn get_rooms_task(&self, cx: &App) -> Task<Result<HashSet<Room>, Error>> {
        let nostr = NostrRegistry::global(cx);
        let client = nostr.read(cx).client();
        let signer = nostr.read(cx).signer();

        cx.background_spawn(async move {
            let public_key = signer.get_public_key_async().await?;

            // Query the latest contact list (previously `NostrDatabaseExt::contacts_public_keys`)
            let filter = Filter::new()
                .author(public_key)
                .kind(Kind::ContactList)
                .limit(1);

            let contacts: HashSet<PublicKey> = client
                .database()
                .query(filter)
                .await
                .unwrap_or_default()
                .into_iter()
                .next()
                .map(|event| event.tags.public_keys().collect())
                .unwrap_or_default();

            let filter = Filter::new()
                .kind(Kind::ApplicationSpecificData)
                .custom_tag(SingleLetterTag::LOWERCASE_K, "14");

            let events = client.database().query(filter).await?;
            let mut grouped: HashMap<u64, Vec<UnsignedEvent>> = HashMap::new();

            for raw in events.into_iter() {
                if let Ok(rumor) = UnsignedEvent::from_json(&raw.content)
                    && rumor.tags.public_keys().next().is_some()
                {
                    if rumor.pubkey != public_key
                        && !rumor.tags.public_keys().any(|k| k == public_key)
                    {
                        continue;
                    }
                    grouped.entry(rumor.uniq_id()).or_default().push(rumor);
                }
            }

            let mut rooms = HashSet::with_capacity(grouped.len());

            for (_id, messages) in grouped.into_iter() {
                let latest = messages.iter().max_by_key(|m| m.created_at).unwrap();
                let room = Room::from(latest).organize(&public_key);

                let user_sent = messages.iter().any(|m| m.pubkey == public_key);
                let is_contact = room.members.iter().any(|k| contacts.contains(k));

                let room = if user_sent || is_contact {
                    room.kind(RoomKind::Ongoing)
                } else {
                    room
                };

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

        let Some(public_key) = nostr.read(cx).current_user() else {
            return;
        };

        match self.room_index.get(&message.room).cloned() {
            Some(room) => {
                room.update(cx, |this, cx| {
                    if this.kind == RoomKind::Request && message.rumor.pubkey == public_key {
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
    signer: &UniversalSigner,
    gift_wrap: &Event,
) -> Result<UnsignedEvent, Error> {
    // Try to get cached rumor first
    if let Ok(rumor) = get_rumor(client, gift_wrap.id).await {
        return Ok(rumor);
    }

    // Try to unwrap with the available signer
    let unwrapped = try_unwrap_with(signer, gift_wrap).await?;
    let mut rumor = unwrapped.rumor;

    // Verify rumor author matches the seal sender (as per mobile implementation)
    if rumor.pubkey != unwrapped.sender {
        return Err(anyhow!("Rumor author does not match seal sender"));
    }

    // Generate event id for the rumor if it doesn't have one
    rumor.ensure_id();

    // Cache the rumor
    if let Err(e) = set_rumor(client, gift_wrap.id, &rumor).await {
        log::error!("Failed to cache rumor: {e:?}");
    }

    Ok(rumor)
}

/// Attempts to unwrap a gift wrap event with a given signer.
async fn try_unwrap_with(
    signer: &UniversalSigner,
    gift_wrap: &Event,
) -> Result<UnwrappedGift, Error> {
    // Get the sealed event
    let seal = signer
        .nip44_decrypt_async(&gift_wrap.pubkey, &gift_wrap.content)
        .await?;

    // Verify the sealed event
    let seal: Event = Event::from_json(seal)?;
    seal.verify()?;

    // Get the rumor event
    let rumor = signer
        .nip44_decrypt_async(&seal.pubkey, &seal.content)
        .await?;

    let rumor = UnsignedEvent::from_json(rumor)?;

    Ok(UnwrappedGift {
        sender: seal.pubkey,
        rumor,
    })
}

/// Stores an unwrapped event in local database with reference to original
async fn set_rumor(client: &Client, id: EventId, rumor: &UnsignedEvent) -> Result<(), Error> {
    let room_id = rumor.uniq_id().to_string();

    let tags = vec![
        Tag::identifier(id),
        Tag::public_key(rumor.pubkey),
        Tag::custom("r", [room_id]),
        Tag::custom("k", ["14"]),
    ];

    let event = EventBuilder::new(Kind::ApplicationSpecificData, rumor.as_json())
        .tags(tags)
        .finalize_async(&*LOCAL_KEYS)
        .await?;

    client.database().save_event(&event).await?;

    Ok(())
}

/// Retrieves a previously unwrapped event from local database
async fn get_rumor(client: &Client, gift_wrap: EventId) -> Result<UnsignedEvent, Error> {
    let filter = Filter::new().identifier(gift_wrap).limit(1);

    if let Some(event) = client.database().query(filter).await?.into_iter().next() {
        UnsignedEvent::from_json(event.content).map_err(|e| anyhow!(e))
    } else {
        Err(anyhow!("Event is not cached yet."))
    }
}
