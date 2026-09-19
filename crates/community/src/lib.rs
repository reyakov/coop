use std::collections::HashMap;

use anyhow::Result;
use concord::CommunityId;
use concord::cord01::KIND_WRAP;
pub use concord::cord02::CommunityMetadata;
use concord::store::CommunityState;
use gpui::{App, AppContext, Context, Entity, EventEmitter, Global, Subscription, Task};
use nostr_sdk::prelude::*;
use smallvec::{SmallVec, smallvec};
use state::NostrRegistry;

mod community;
mod sync;

pub use community::*;
pub use sync::*;

pub fn init(cx: &mut App) {
    CommunityRegistry::set_global(cx.new(CommunityRegistry::new), cx);
}

struct GlobalCommunityRegistry(Entity<CommunityRegistry>);

impl Global for GlobalCommunityRegistry {}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Signal {
    Event(CommunityId),
    List,
}

impl EventEmitter<CommunityEvent> for CommunityRegistry {}

pub struct CommunityRegistry {
    communities: Vec<Entity<Community>>,
    index: HashMap<CommunityId, Entity<Community>>,
    /// The plane set each community was last subscribed with
    synced: HashMap<CommunityId, SubscriptionKey>,
    /// One observer per tracked community, dropped on reset
    observers: Vec<Subscription>,
    signal_tx: flume::Sender<Signal>,
    signal_rx: flume::Receiver<Signal>,
    tasks: SmallVec<[Task<Result<()>>; 2]>,
    /// Notification listener task (cancelled on signer change)
    notification_listener: Option<Task<Result<()>>>,
    /// Signal consumer task (cancelled on signer change)
    signal_consumer: Option<Task<Result<()>>>,
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
            observers: Vec::new(),
            signal_tx: tx,
            signal_rx: rx,
            tasks: smallvec![],
            notification_listener: None,
            signal_consumer: None,
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
        self.tasks.clear();
        self.observers.clear();

        let nostr = NostrRegistry::global(cx);
        let client = nostr.read(cx).client();
        let ids: Vec<CommunityId> = self.index.keys().copied().collect();

        for id in ids {
            let client = client.clone();
            let subscription = sync::subscription_id(&id);

            self.tasks.push(cx.background_spawn(async move {
                client.unsubscribe(&subscription).await?;
                Ok(())
            }));
        }

        self.communities.clear();
        self.index.clear();
        self.synced.clear();
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
    fn track(&mut self, states: Vec<CommunityState>, cx: &mut Context<Self>) {
        self.observers.clear();
        self.communities.clear();
        self.index.clear();
        self.synced.clear();

        for state in states {
            let id = state.id;
            let community = cx.new(|_| Community::new(state));

            self.observers
                .push(cx.observe(&community, |this, _community, cx| {
                    this.sync_subscriptions(cx);
                    cx.notify();
                }));

            self.index.insert(id, community.clone());
            self.communities.push(community);
        }

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
            let filter = sync::subscription_filter(&planes);
            let relays = key.relays().to_vec();

            self.synced.insert(id, key);

            self.tasks.push(cx.spawn(async move |this, cx| {
                if let Err(error) = subscribe(&client, &subscription, &relays, filter).await {
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

        let nostr = NostrRegistry::global(cx);
        let client = nostr.read(cx).client();

        let tx = self.signal_tx.clone();
        let rx = self.signal_rx.clone();

        self.notification_listener = Some(cx.background_spawn(async move {
            let mut notifications = client.notifications();

            while let Some(notification) = notifications.next().await {
                let ClientNotification::Event {
                    subscription_id,
                    event,
                    ..
                } = notification
                else {
                    continue;
                };

                if sync::is_list_subscription(&subscription_id) {
                    tx.send_async(Signal::List).await?;
                    continue;
                }

                if event.kind != Kind::from(KIND_WRAP) {
                    continue;
                }

                let Some(id) = sync::community_of(&subscription_id) else {
                    continue;
                };

                tx.send_async(Signal::Event(id)).await?;
            }

            Ok(())
        }));

        self.signal_consumer = Some(cx.spawn(async move |this, cx| {
            while let Ok(signal) = rx.recv_async().await {
                match signal {
                    Signal::Event(id) => this.update(cx, |this, cx| this.refresh(id, cx))?,
                    Signal::List => this.update(cx, |this, cx| this.load(cx))?,
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

    for url in relays {
        if let Err(error) = client.add_relay(url).and_connect().await {
            log::warn!("community {id}: failed to add relay {url}: {error}");
        }
    }

    let target = if relays.is_empty() {
        ReqTarget::auto(vec![filter])
    } else {
        ReqTarget::manual(
            relays
                .iter()
                .map(|url| (url.clone(), vec![filter.clone()]))
                .collect::<Vec<_>>(),
        )
    };

    let output = client.subscribe(target).with_id(id.clone()).await?;

    if !output.failed.is_empty() {
        log::warn!(
            "community {id}: {} relay(s) rejected the subscription",
            output.failed.len()
        );
    }

    Ok(())
}
