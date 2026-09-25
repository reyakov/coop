use std::collections::{BTreeSet, HashMap};
use std::time::{Duration, Instant};

use anyhow::Result;
pub use concord::cord02::CommunityMetadata;
pub use concord::cord03::{ChatMessage, ReplyRef};
use concord::state::CommunityState;
pub use concord::{ChannelId, CommunityId, Epoch};
use futures::future::{Either, select};
use gpui::{App, AppContext, Context, Entity, EventEmitter, Global, Subscription, Task};
use nostr_sdk::prelude::*;
use smallvec::{SmallVec, smallvec};
use state::NostrRegistry;

use crate::history::{PageRegistry, Settled, auth_required};
use crate::rekey::WatchRegistry;

pub mod cache;
mod community;
pub mod history;
mod rekey;
mod sync;

pub use community::*;
pub use sync::*;

/// How long a burst of relay notifications is collected before it is folded.
const PUMP_WINDOW: Duration = Duration::from_millis(200);
/// How long a community's relays may deliver nothing before
/// its standing subscription is torn down and re-issued.
const LIVE_ROTATE: Duration = Duration::from_secs(90);

pub fn init(cx: &mut App) {
    CommunityRegistry::set_global(cx.new(CommunityRegistry::new), cx);
}

struct GlobalCommunityRegistry(Entity<CommunityRegistry>);

impl Global for GlobalCommunityRegistry {}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Signal {
    Event(CommunityId),
    List,
    Rekey(CommunityId),
}

/// Which standing subscription an id belongs to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Route {
    List,
    Community(CommunityId),
}

fn route_of(id: &SubscriptionId) -> Option<Route> {
    if sync::is_list_subscription(id) {
        return Some(Route::List);
    }

    sync::community_of(id).map(Route::Community)
}

/// What a window of relay notifications saw, waiting to be folded once.
#[derive(Default)]
struct Batch {
    list: bool,
    communities: BTreeSet<CommunityId>,
    rekeys: BTreeSet<CommunityId>,
}

/// Whether the pump should keep listening.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Flow {
    Continue,
    Stop,
}

/// Fold one notification into the window, or settle a page it belongs to.
fn route(
    notification: ClientNotification,
    pages: &PageRegistry,
    watches: &WatchRegistry,
    batch: &mut Batch,
) -> Flow {
    match notification {
        ClientNotification::Event {
            subscription_id, ..
        } => match route_of(&subscription_id) {
            Some(Route::List) => batch.list = true,
            Some(Route::Community(id)) => {
                batch.communities.insert(id);
            }
            None => {
                if let Some(id) = watches.community_of(&subscription_id) {
                    batch.rekeys.insert(id);
                }
            }
        },
        ClientNotification::Message { relay_url, message } => match *message {
            RelayMessage::EndOfStoredEvents(id) => {
                pages.deliver(&id, relay_url, Settled::Replayed);
            }
            RelayMessage::Closed {
                subscription_id,
                message,
            } if !auth_required(&message) => {
                pages.deliver(
                    &subscription_id,
                    relay_url,
                    Settled::Refused(message.into_owned()),
                );
            }
            _ => {}
        },
        ClientNotification::Shutdown => return Flow::Stop,
    }

    Flow::Continue
}

/// Hand the window's signals to the foreground consumer, one per community.
async fn flush(tx: &flume::Sender<Signal>, batch: &mut Batch) -> Result<()> {
    for id in std::mem::take(&mut batch.communities) {
        tx.send_async(Signal::Event(id)).await?;
    }

    for id in std::mem::take(&mut batch.rekeys) {
        tx.send_async(Signal::Rekey(id)).await?;
    }

    if std::mem::take(&mut batch.list) {
        tx.send_async(Signal::List).await?;
    }

    Ok(())
}

impl EventEmitter<CommunityEvent> for CommunityRegistry {}

pub struct CommunityRegistry {
    communities: Vec<Entity<Community>>,
    index: HashMap<CommunityId, Entity<Community>>,
    /// The plane set each community was last subscribed with
    synced: HashMap<CommunityId, SubscriptionKey>,
    /// When a relay last delivered something for a community,
    /// which is the only evidence the standing subscription is alive.
    last_event: HashMap<CommunityId, Instant>,
    /// One observer per tracked community, dropped on reset
    observers: HashMap<CommunityId, Subscription>,
    signal_tx: flume::Sender<Signal>,
    signal_rx: flume::Receiver<Signal>,
    /// The page subscriptions in flight, shared with the notification pump.
    pages: PageRegistry,
    /// The rekey watch subscriptions, resolved to their community.
    watches: WatchRegistry,
    tasks: SmallVec<[Task<Result<()>>; 2]>,
    /// Notification listener task (cancelled on signer change)
    notification_listener: Option<Task<Result<()>>>,
    /// Signal consumer task (cancelled on signer change)
    signal_consumer: Option<Task<Result<()>>>,
    /// The round scheduler (cancelled on signer change)
    scheduler: Option<Task<Result<()>>>,
    _subscriptions: SmallVec<[Subscription; 2]>,
}

impl CommunityRegistry {
    pub fn global(cx: &App) -> Entity<Self> {
        cx.global::<GlobalCommunityRegistry>().0.clone()
    }

    fn set_global(state: Entity<Self>, cx: &mut App) {
        cx.set_global(GlobalCommunityRegistry(state));
    }

    fn new(cx: &mut Context<Self>) -> Self {
        let entity = cx.entity().downgrade();
        let nostr = NostrRegistry::global(cx);
        let (tx, rx) = flume::bounded::<Signal>(256);
        let mut subscriptions = smallvec![];

        subscriptions.push(cx.subscribe(&nostr, |this, _nostr, event, cx| {
            if event.signer_changed() {
                this.reset(cx);
                this.handle_notifications(cx);
                this.subscribe_list(cx);
                this.load(cx);
            }
        }));

        cx.defer(move |cx| {
            entity
                .update(cx, |this, cx| {
                    this.handle_notifications(cx);
                    if nostr.read(cx).current_user().is_some() {
                        this.subscribe_list(cx);
                        this.load(cx);
                    }
                })
                .ok();
        });

        Self {
            communities: Vec::new(),
            index: HashMap::new(),
            synced: HashMap::new(),
            last_event: HashMap::new(),
            observers: HashMap::new(),
            signal_tx: tx,
            signal_rx: rx,
            pages: PageRegistry::default(),
            watches: WatchRegistry::default(),
            tasks: smallvec![],
            notification_listener: None,
            signal_consumer: None,
            scheduler: None,
            _subscriptions: subscriptions,
        }
    }

    pub fn communities(&self) -> &[Entity<Community>] {
        &self.communities
    }

    pub fn community(&self, id: &CommunityId) -> Option<Entity<Community>> {
        self.index.get(id).cloned()
    }

    /// Create a community owned by the current account and begin tracking it.
    pub fn create(&mut self, metadata: CommunityMetadata, cx: &mut Context<Self>) {
        let nostr = NostrRegistry::global(cx);
        let current_user = nostr.read(cx).current_user();

        if current_user.is_none() {
            cx.emit(CommunityEvent::Error(
                "cannot create a community without an account".to_owned(),
            ));
            return;
        }

        let signer = nostr.read(cx).signer();
        let client = nostr.read(cx).client();

        let task =
            cx.background_spawn(async move { sync::create(&client, &signer, &metadata).await });

        self.tasks.push(cx.spawn(async move |this, cx| {
            match task.await {
                Ok(_state) => this.update(cx, |this, cx| this.load(cx))?,
                Err(error) => {
                    this.update(cx, |_this, cx| {
                        cx.emit(CommunityEvent::Error(error.to_string()));
                    })?;
                }
            }

            Ok(())
        }));
    }

    /// Forget the current account and cancel everything in flight.
    pub fn reset(&mut self, cx: &mut Context<Self>) {
        self.notification_listener = None;
        self.signal_consumer = None;
        self.scheduler = None;
        self.tasks.clear();
        self.observers.clear();
        self.pages.clear();
        self.watches.clear();

        let nostr = NostrRegistry::global(cx);
        let client = nostr.read(cx).client();
        let ids: Vec<CommunityId> = self.index.keys().copied().collect();

        for id in ids {
            let client = client.clone();
            let subscription = sync::subscription_id(&id);
            let rekey = rekey::subscription_id(&id);

            self.tasks.push(cx.background_spawn(async move {
                client.unsubscribe(&subscription).await?;
                client.unsubscribe(&rekey).await?;
                Ok(())
            }));
        }

        self.communities.clear();
        self.index.clear();
        self.synced.clear();
        self.last_event.clear();

        cx.notify();
    }

    /// Subscribe to the account's community list.
    fn subscribe_list(&mut self, cx: &mut Context<Self>) {
        let nostr = NostrRegistry::global(cx);
        let signer = nostr.read(cx).signer();
        let client = nostr.read(cx).client();

        self.tasks.push(cx.spawn(async move |this, cx| {
            let self_pk = signer.get_public_key_async().await?;

            if let Err(error) = sync::subscribe_list(&client, self_pk).await {
                this.update(cx, |_this, cx| {
                    cx.emit(CommunityEvent::Error(error.to_string()));
                })?;
            }

            Ok(())
        }));
    }

    /// Discover the account's communities in the local database.
    fn load(&mut self, cx: &mut Context<Self>) {
        let nostr = NostrRegistry::global(cx);
        let signer = nostr.read(cx).signer();
        let client = nostr.read(cx).client();

        let task = cx.background_spawn(async move {
            let self_pk = signer.get_public_key_async().await?;
            sync::load(&client, &signer, self_pk).await
        });

        self.tasks.push(cx.spawn(async move |this, cx| {
            match task.await {
                Ok(states) => {
                    this.update(cx, |this, cx| this.track(states, cx))?;
                }
                Err(error) => {
                    this.update(cx, |_this, cx| {
                        cx.emit(CommunityEvent::Error(error.to_string()));
                    })?;
                }
            }

            Ok(())
        }));
    }

    /// Replace the tracked communities with a freshly loaded set.
    ///
    /// A community that survives the reload keeps its entity, so an open panel
    /// and a browsing sidebar stay pointed at a live community.
    fn track(&mut self, states: Vec<CommunityState>, cx: &mut Context<Self>) {
        let mut communities = Vec::with_capacity(states.len());

        for state in states {
            let id = state.id;

            let community = match self.index.remove(&id) {
                Some(community) => {
                    // The list can carry plane material the store does not.
                    if community.read(cx).state() != &state {
                        community.update(cx, |community, _cx| community.adopt(state));
                    }

                    community
                }
                None => {
                    let community = cx.new(|_| Community::new(state, self.pages.clone()));

                    self.observers.insert(
                        id,
                        cx.observe(&community, |this, _community, cx| {
                            this.sync_subscriptions(cx);
                            cx.notify();
                        }),
                    );

                    community
                }
            };

            communities.push((id, community));
        }

        // Whatever the index still holds is no longer in the list.
        let dropped: Vec<CommunityId> = self.index.keys().copied().collect();

        for id in dropped {
            self.observers.remove(&id);
            self.synced.remove(&id);
            self.last_event.remove(&id);
        }

        self.communities = communities
            .iter()
            .map(|(_, community)| community.clone())
            .collect();
        self.index = communities.into_iter().collect();

        self.sync_subscriptions(cx);

        // A backlog already in the database produces no notification, so fold it once.
        for community in self.communities.clone() {
            community.update(cx, |community, cx| community.refresh(cx));
        }

        cx.notify();
    }

    fn refresh(&mut self, id: CommunityId, cx: &mut Context<Self>) {
        let Some(community) = self.index.get(&id).cloned() else {
            return;
        };

        community.update(cx, |community, cx| community.refresh(cx));
    }

    /// Adopt whatever a community's rekey watch has delivered.
    fn rekey(&mut self, id: CommunityId, cx: &mut Context<Self>) {
        let Some(community) = self.index.get(&id).cloned() else {
            return;
        };

        community.update(cx, |community, cx| community.rekey(cx));
    }

    /// One scheduler pass: every community repairs itself if it has gone stale,
    /// and any whose relays have gone quiet is re-subscribed.
    fn tick(&mut self, cx: &mut Context<Self>) {
        for community in self.communities.clone() {
            community.update(cx, |community, cx| community.tick(cx));
        }
        self.rotate_quiet(cx);
    }

    /// Re-issue the standing subscription of every community that has been quiet for `LIVE_ROTATE`
    fn rotate_quiet(&mut self, cx: &mut Context<Self>) {
        let now = Instant::now();
        let ids: Vec<CommunityId> = self.index.keys().copied().collect();
        let mut rotated = false;

        for id in ids {
            let quiet = self
                .last_event
                .get(&id)
                .is_none_or(|at| now.duration_since(*at) >= LIVE_ROTATE);

            if !quiet {
                continue;
            }

            self.synced.remove(&id);
            rotated = true;
        }

        if rotated {
            self.sync_subscriptions(cx);
        }
    }

    /// Re-subscribe every community whose held planes moved.
    fn sync_subscriptions(&mut self, cx: &mut Context<Self>) {
        let nostr = NostrRegistry::global(cx);

        for community in self.communities.clone() {
            let client = nostr.read(cx).client();

            let (id, key, state) = {
                let community = community.read(cx);
                (
                    community.id(),
                    community.subscription_key(),
                    community.state().clone(),
                )
            };

            if self.synced.get(&id) == Some(&key) {
                continue;
            }

            let planes = match sync::planes(&state) {
                Ok(planes) => planes,
                Err(error) => {
                    cx.emit(CommunityEvent::Error(error.to_string()));
                    continue;
                }
            };

            let subscription = sync::subscription_id(&id);
            let filter = sync::live_filter(&planes, sync::live_window(&state, Timestamp::now()));
            let relays = key.relays().to_vec();

            // A fresh REQ counts as evidence of life for this community.
            self.last_event.insert(id, Instant::now());

            // The rekey watch is a second standing REQ over the same relays.
            let watch = match rekey::watches(&state) {
                Ok(watches) => rekey::watch_filter(&watches),
                Err(error) => {
                    cx.emit(CommunityEvent::Error(error.to_string()));
                    continue;
                }
            };

            let rekey_subscription = rekey::subscription_id(&id);
            self.watches.register(rekey_subscription.clone(), id);

            self.synced.insert(id, key);

            self.tasks.push(cx.spawn(async move |this, cx| {
                if let Err(error) = subscribe(&client, &subscription, &relays, filter).await {
                    this.update(cx, |_this, cx| {
                        cx.emit(CommunityEvent::Error(error.to_string()));
                    })?;
                }

                if let Err(error) = subscribe(&client, &rekey_subscription, &relays, watch).await {
                    this.update(cx, |_this, cx| {
                        cx.emit(CommunityEvent::Error(error.to_string()));
                    })?;
                }

                Ok(())
            }));
        }
    }

    fn handle_notifications(&mut self, cx: &mut Context<Self>) {
        self.notification_listener = None;
        self.signal_consumer = None;
        self.scheduler = None;

        let nostr = NostrRegistry::global(cx);
        let client = nostr.read(cx).client();

        let tx = self.signal_tx.clone();
        let rx = self.signal_rx.clone();
        let pages = self.pages.clone();
        let watches = self.watches.clone();
        let executor = cx.background_executor().clone();

        self.notification_listener = Some(cx.background_spawn(async move {
            let mut notifications = client.notifications();
            let mut batch = Batch::default();

            'outer: loop {
                match notifications.next().await {
                    Some(notification) => {
                        if route(notification, &pages, &watches, &mut batch) == Flow::Stop {
                            flush(&tx, &mut batch).await?;
                            break 'outer;
                        }
                    }
                    None => break 'outer,
                }

                let deadline = Instant::now() + PUMP_WINDOW;

                loop {
                    let now = Instant::now();

                    if now >= deadline {
                        break;
                    }

                    let timer = executor.timer(deadline - now);
                    let next = notifications.next();
                    futures::pin_mut!(timer);
                    futures::pin_mut!(next);

                    match select(next, timer).await {
                        Either::Left((Some(notification), _)) => {
                            if route(notification, &pages, &watches, &mut batch) == Flow::Stop {
                                flush(&tx, &mut batch).await?;
                                break 'outer;
                            }
                        }
                        Either::Left((None, _)) => break 'outer,
                        Either::Right(_) => break,
                    }
                }

                flush(&tx, &mut batch).await?;
            }

            Ok(())
        }));

        self.signal_consumer = Some(cx.spawn(async move |this, cx| {
            while let Ok(signal) = rx.recv_async().await {
                match signal {
                    Signal::Event(id) => this.update(cx, |this, cx| {
                        // The only proof a community's subscription is still
                        // delivering anything.
                        this.last_event.insert(id, Instant::now());
                        this.refresh(id, cx);
                    })?,
                    Signal::Rekey(id) => this.update(cx, |this, cx| this.rekey(id, cx))?,
                    Signal::List => this.update(cx, |this, cx| this.load(cx))?,
                }
            }
            Ok(())
        }));

        self.scheduler = Some(cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(MIN_ROUND_INTERVAL).await;

                if let Some(registry) = this.upgrade() {
                    registry.update(cx, |this, cx| {
                        this.tick(cx);
                    });
                } else {
                    break;
                }
            }

            Ok(())
        }));
    }
}

async fn subscribe(
    client: &Client,
    id: &SubscriptionId,
    relays: &[RelayUrl],
    filter: Filter,
) -> Result<()> {
    client.unsubscribe(id).await?;

    if relays.is_empty() {
        log::warn!("community {id}: no relay to subscribe to");
        return Ok(());
    }

    let mut targets: Vec<(RelayUrl, Vec<Filter>)> = Vec::with_capacity(relays.len());

    for url in relays {
        if let Err(error) = client.add_relay(url).and_connect().await {
            log::warn!("community {id}: failed to add relay {url}: {error}");
        }

        match client.relay(url).await {
            Ok(Some(_)) => targets.push((url.clone(), vec![filter.clone()])),
            Ok(None) => log::warn!("community {id}: relay {url} is not in the pool"),
            Err(error) => log::warn!("community {id}: relay {url} could not be looked up: {error}"),
        }
    }

    if targets.is_empty() {
        log::warn!("community {id}: no relay accepted the standing subscription");
        return Ok(());
    }

    let output = client
        .subscribe(ReqTarget::manual(targets))
        .with_id(id.clone())
        .await?;

    if !output.failed.is_empty() {
        log::warn!(
            "community {id}: {} relay(s) rejected the subscription",
            output.failed.len()
        );
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn relay() -> RelayUrl {
        RelayUrl::parse("wss://relay.example").expect("a url")
    }

    fn event(subscription_id: SubscriptionId) -> ClientNotification {
        let keys = Keys::generate();
        let event = EventBuilder::new(Kind::TextNote, "hi")
            .finalize(&keys)
            .expect("signs");

        ClientNotification::Event {
            relay_url: relay(),
            subscription_id,
            event: Box::new(event),
        }
    }

    fn message(message: RelayMessage<'static>) -> ClientNotification {
        ClientNotification::Message {
            relay_url: relay(),
            message: Box::new(message),
        }
    }

    /// A burst within one window folds each community once, and the list once,
    /// however many events arrived.
    #[test]
    fn a_burst_collapses_to_one_signal_per_community() {
        let pages = PageRegistry::default();
        let watches = WatchRegistry::default();
        let community = CommunityId::from_bytes([0x42; 32]);
        let mut batch = Batch::default();

        let plane = event(sync::subscription_id(&community));
        for _ in 0..50 {
            assert_eq!(
                route(plane.clone(), &pages, &watches, &mut batch),
                Flow::Continue
            );
        }

        route(
            event(sync::list_subscription_id()),
            &pages,
            &watches,
            &mut batch,
        );

        // A page's event is folded from the database later, not routed here.
        route(
            event(SubscriptionId::new("concord-history-7")),
            &pages,
            &watches,
            &mut batch,
        );

        assert!(batch.list);
        assert_eq!(batch.communities, BTreeSet::from([community]));
        assert!(batch.rekeys.is_empty());
    }

    /// A rekey watch's wraps put themselves in the database; the pump's only job
    /// is to wake the adoption pass, and it resolves the community by id.
    #[test]
    fn a_rekey_watch_event_wakes_its_community() {
        let pages = PageRegistry::default();
        let watches = WatchRegistry::default();
        let community = CommunityId::from_bytes([0x42; 32]);
        let id = rekey::subscription_id(&community);
        watches.register(id.clone(), community);

        let mut batch = Batch::default();
        route(event(id), &pages, &watches, &mut batch);

        assert_eq!(batch.rekeys, BTreeSet::from([community]));
        assert!(batch.communities.is_empty());
    }

    #[test]
    fn an_eose_settles_only_the_page_that_owns_the_id() {
        let pages = PageRegistry::default();
        let mine = SubscriptionId::new("concord-history-1");
        let other = SubscriptionId::new("concord-history-2");
        let (mine_tx, mine_rx) = flume::bounded(1);
        let (other_tx, other_rx) = flume::bounded(1);
        pages.register(mine.clone(), mine_tx);
        pages.register(other, other_tx);

        let mut batch = Batch::default();
        let flow = route(
            message(RelayMessage::eose(mine)),
            &pages,
            &WatchRegistry::default(),
            &mut batch,
        );

        assert_eq!(flow, Flow::Continue);
        assert!(matches!(
            mine_rx.try_recv().expect("a report").outcome,
            Settled::Replayed
        ));
        assert!(other_rx.try_recv().is_err());
    }

    #[test]
    fn a_refused_page_settles_its_relay_as_refused() {
        let pages = PageRegistry::default();
        let id = SubscriptionId::new("concord-history-1");
        let (sender, receiver) = flume::bounded(1);
        pages.register(id.clone(), sender);

        let mut batch = Batch::default();
        route(
            message(RelayMessage::closed(id, "blocked: not allowed")),
            &pages,
            &WatchRegistry::default(),
            &mut batch,
        );

        match receiver.try_recv().expect("a report").outcome {
            Settled::Refused(reason) => assert!(reason.contains("blocked")),
            other => panic!("expected a refusal, got {other:?}"),
        }
    }

    /// The SDK re-issues an `auth-required` REQ under the same id after AUTH, so
    /// the page keeps waiting rather than writing the relay off.
    #[test]
    fn an_auth_required_close_settles_nothing() {
        let pages = PageRegistry::default();
        let id = SubscriptionId::new("concord-history-1");
        let (sender, receiver) = flume::bounded(1);
        pages.register(id.clone(), sender);

        let mut batch = Batch::default();
        route(
            message(RelayMessage::closed(
                id,
                "auth-required: please authenticate",
            )),
            &pages,
            &WatchRegistry::default(),
            &mut batch,
        );

        assert!(receiver.try_recv().is_err());
    }

    #[test]
    fn a_shutdown_stops_the_pump() {
        let pages = PageRegistry::default();
        let mut batch = Batch::default();

        assert_eq!(
            route(
                ClientNotification::Shutdown,
                &pages,
                &WatchRegistry::default(),
                &mut batch
            ),
            Flow::Stop
        );
    }
}
