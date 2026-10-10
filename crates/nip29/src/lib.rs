use std::collections::{HashMap, HashSet};
use std::sync::LazyLock;
use std::time::Duration;

use anyhow::{Error, Result};
use gpui::{App, AppContext, Context, Entity, EventEmitter, Global, Subscription, Task};
use nostr_sdk::prelude::*;
use smallvec::{SmallVec, smallvec};
use state::{NostrRegistry, UniversalSigner};

mod discovery;
mod group;
mod protocol;
mod utils;

pub use discovery::Discovery;
pub use group::*;
pub use protocol::*;
pub(crate) use utils::*;

pub(crate) static LOCAL_KEYS: LazyLock<Keys> = LazyLock::new(Keys::generate);

const BROWSE_LIMIT: usize = 500;
const BROWSE_TIMEOUT: Duration = Duration::from_secs(10);
const FORK_INTERVAL: Duration = Duration::from_secs(15 * 60);
const CONFIRM_POLLS: usize = 20;
const CONFIRM_INTERVAL: Duration = Duration::from_millis(250);
const METADATA_WARNING: &str =
    "Group created, but the relay did not accept its name. Edit the group to set it.";

pub fn init(cx: &mut App) {
    GroupsRegistry::set_global(cx.new(GroupsRegistry::new), cx);
}

struct GlobalGroupsRegistry(Entity<GroupsRegistry>);

impl Global for GlobalGroupsRegistry {}

#[derive(Debug, Clone)]
pub enum GroupsEvent {
    Updated,
    Error(String),
    Browsed(Box<GroupCandidate>),
    BrowseSettled(RelayUrl),
    /// A browsed group announced its member list.
    Members {
        key: GroupKey,
        created_at: Timestamp,
        members: usize,
    },
    /// The discovery synced new groups into the local database.
    Synced,
    /// A relay accepted a new group; the warning carries a non-fatal issue.
    Created {
        key: GroupKey,
        warning: Option<String>,
    },
}

#[derive(Debug)]
enum Signal {
    Event(SubscriptionId, Box<Event>),
    Eose(SubscriptionId),
    Closed(SubscriptionId, String),
    BrowseSettled(SubscriptionId),
    /// A reconciliation started on a relay.
    Syncing(RelayUrl),
    /// A relay finished answering a reconciliation.
    Synced(RelayUrl),
    Forks,
}

impl EventEmitter<GroupsEvent> for GroupsRegistry {}

pub struct GroupsRegistry {
    groups: Vec<Entity<Group>>,
    previews: HashMap<GroupKey, Entity<Group>>,
    list: GroupList,
    browse: HashMap<SubscriptionId, RelayUrl>,
    routes: HashMap<SubscriptionId, GroupKey>,
    observers: HashMap<GroupKey, Subscription>,
    forks: HashMap<PublicKey, (Timestamp, GroupList)>,
    fork_authors: HashSet<PublicKey>,
    /// The relays with a discovery reconciliation in flight.
    syncing: HashSet<RelayUrl>,
    signal_tx: flume::Sender<Signal>,
    signal_rx: flume::Receiver<Signal>,
    notification_listener: Option<Task<()>>,
    signal_consumer: Option<Task<()>>,
    tasks: SmallVec<[Task<()>; 2]>,
    _subscriptions: SmallVec<[Subscription; 2]>,
}

impl GroupsRegistry {
    pub fn global(cx: &App) -> Entity<Self> {
        cx.global::<GlobalGroupsRegistry>().0.clone()
    }

    fn set_global(state: Entity<Self>, cx: &mut App) {
        cx.set_global(GlobalGroupsRegistry(state));
    }

    fn new(cx: &mut Context<Self>) -> Self {
        let nostr = NostrRegistry::global(cx);
        let this = cx.entity().downgrade();

        let (tx, rx) = flume::unbounded::<Signal>();
        let mut subscriptions = smallvec![];

        subscriptions.push(cx.subscribe(&nostr, |this, _nostr, event, cx| {
            if event.signer_changed() {
                this.reset(cx);
                this.handle_notifications(cx);
                this.load_cached(cx);
                this.load(cx);
            }
        }));

        cx.defer(move |cx| {
            this.update(cx, |this, cx| {
                this.handle_notifications(cx);
                this.load_cached(cx);

                if nostr.read(cx).current_user().is_some() {
                    this.load(cx);
                }
            })
            .ok();
        });

        Self {
            groups: Vec::new(),
            previews: HashMap::new(),
            list: GroupList::default(),
            browse: HashMap::new(),
            routes: HashMap::new(),
            observers: HashMap::new(),
            forks: HashMap::new(),
            fork_authors: HashSet::new(),
            syncing: HashSet::new(),
            signal_tx: tx,
            signal_rx: rx,
            notification_listener: None,
            signal_consumer: None,
            tasks: smallvec![],
            _subscriptions: subscriptions,
        }
    }

    pub fn groups(&self) -> &[Entity<Group>] {
        &self.groups
    }

    pub fn group(&self, key: &GroupKey, cx: &App) -> Option<Entity<Group>> {
        self.groups
            .iter()
            .find(|group| group.read(cx).key() == key)
            .cloned()
            .or_else(|| self.previews.get(key).cloned())
    }

    pub fn join(&mut self, reference: GroupReference, cx: &mut Context<Self>) {
        self.join_key(reference.key(), reference.invite().map(str::to_owned), cx);
    }

    pub fn join_key(&mut self, key: GroupKey, invite: Option<String>, cx: &mut Context<Self>) {
        self.ensure(key.clone(), cx);

        let Some(group) = self.group(&key, cx) else {
            return;
        };

        self.upsert_entry(&key, None, false);
        self.publish_list(cx);

        cx.emit(GroupsEvent::Updated);
        cx.notify();

        let builder = key.id().join_request(invite.as_deref(), "");
        let sent = group.read(cx).publish(builder, cx);

        group.update(cx, |group, cx| {
            group.mark_pending(Timestamp::now(), cx);
        });

        self.tasks.push(cx.spawn(async move |this, cx| {
            if let Err(error) = sent.await {
                let reason = error.to_string();

                if let Err(error) = this.update(cx, |this, cx| {
                    if let Some(group) = this.group(&key, cx) {
                        group.update(cx, |group, cx| {
                            group.mark_refused(reason, cx);
                        });
                    }
                }) {
                    log::warn!("marking the refused join failed: {error}");
                }
            }
        }));
    }

    pub fn create(&mut self, relay: RelayUrl, options: GroupCreateOptions, cx: &mut Context<Self>) {
        let name = options.name.trim().to_owned();

        if name.is_empty() {
            cx.emit(GroupsEvent::Error("group name is empty".to_owned()));
            return;
        }

        let raw = options.custom_id.as_deref().unwrap_or_default();
        let id = if raw.is_empty() {
            readable_id()
        } else {
            raw.trim().to_lowercase()
        };

        let id = match GroupId::new(id) {
            Ok(id) => id,
            Err(error) => {
                cx.emit(GroupsEvent::Error(error.to_string()));
                return;
            }
        };

        let key = GroupKey::new(relay, id);
        self.ensure(key.clone(), cx);

        let Some(group) = self.group(&key, cx) else {
            return;
        };

        let metadata = options.metadata(name.clone());
        let list_private = options.list_private;
        let builder = key.id().create_group(None);
        let published = group.read(cx).publish(builder, cx);
        let created = key;

        self.tasks.push(cx.spawn(async move |this, cx| {
            if let Err(error) = published.await {
                this.update(cx, |this, cx| {
                    this.forget(created, cx);
                    cx.emit(GroupsEvent::Error(error.to_string()));
                })
                .ok();
                return;
            }

            // Give the relay time to publish the group's metadata event,
            // which settles the group id (the relay may override the suggested one).
            for _ in 0..CONFIRM_POLLS {
                if group.read_with(cx, |group, _cx| group.metadata().is_some()) {
                    break;
                }
                cx.background_executor().timer(CONFIRM_INTERVAL).await;
            }

            // The relay can reject the metadata while the admin grant from the
            // create is still settling, one delayed retry covers that race.
            let mut warning = None;

            for attempt in 0..2 {
                if attempt > 0 {
                    cx.background_executor().timer(Duration::from_secs(1)).await;
                }

                let published = group.update(cx, |group, cx| group.edit_metadata(&metadata, cx));
                let accepted = published.await.is_ok();

                if accepted {
                    break;
                }

                if attempt == 1 {
                    warning = Some(METADATA_WARNING.to_owned());
                }
            }

            this.update(cx, |this, cx| {
                this.upsert_entry(&created, Some(name), list_private);
                this.publish_list(cx);

                cx.emit(GroupsEvent::Updated);
                cx.emit(GroupsEvent::Created {
                    key: created,
                    warning,
                });
                cx.notify();
            })
            .ok();
        }));
    }

    pub fn browse(&mut self, relay: RelayUrl, cx: &mut Context<Self>) {
        let nostr = NostrRegistry::global(cx);
        let client = nostr.read(cx).client();

        let id = browse_subscription(&relay);
        let tx = self.signal_tx.clone();

        self.browse.insert(id.clone(), relay.clone());

        self.tasks.push(cx.background_spawn(async move {
            client.add_relay(relay.clone()).and_connect().await.ok();

            let metadata = Filter::new().kind(Kind::GroupMetadata).limit(BROWSE_LIMIT);
            let members = Filter::new().kind(Kind::GroupMembers).limit(BROWSE_LIMIT);

            let opts = SubscribeAutoCloseOptions::default()
                .exit_policy(ReqExitPolicy::ExitOnEOSE)
                .timeout(Some(BROWSE_TIMEOUT));

            if let Err(error) = client
                .subscribe(ReqTarget::single(relay.clone(), [metadata, members]))
                .with_id(id.clone())
                .close_on(opts)
                .await
            {
                log::warn!("browsing {relay} failed: {error}");
            }

            if let Err(error) = tx.send_async(Signal::BrowseSettled(id)).await {
                log::warn!("the browse settle signal was dropped: {error}");
            }
        }));
    }

    fn settle_browse(&mut self, id: SubscriptionId, cx: &mut Context<Self>) {
        let Some(relay) = self.browse.get(&id).cloned() else {
            return;
        };
        cx.emit(GroupsEvent::BrowseSettled(relay));
    }

    /// The (relay, group) pairs of every joined group.
    fn joined_keys(&self, cx: &App) -> HashSet<(RelayUrl, GroupId)> {
        self.groups
            .iter()
            .map(|group| {
                let key = group.read(cx).key();
                (key.relay().clone(), key.id().clone())
            })
            .collect()
    }

    /// Whether the group is one of the user's groups, not a preview.
    fn joined(&self, key: &GroupKey, cx: &App) -> bool {
        self.groups.iter().any(|group| group.read(cx).key() == key)
    }

    /// Load the discoverable groups from the local database only.
    pub fn cached_discovery(&self, cx: &App) -> Task<Discovery> {
        let client = NostrRegistry::global(cx).read(cx).client();
        let joined = self.joined_keys(cx);

        cx.background_spawn(Discovery::cached(client, joined))
    }

    /// Reconcile the discoverable groups with the relays.
    pub fn discover(&self, cx: &App) -> Task<()> {
        let nostr = NostrRegistry::global(cx);
        let client = nostr.read(cx).client();
        let signer = nostr.read(cx).signer();
        let joined = self.joined_keys(cx);

        if nostr.read(cx).current_user().is_none() {
            return Task::ready(());
        }

        cx.background_spawn(async move {
            let Ok(me) = signer.get_public_key_async().await else {
                return;
            };
            Discovery::default().sync(client, me, joined).await;
        })
    }

    pub fn load_more(&mut self, key: &GroupKey, cx: &mut Context<Self>) {
        let Some(group) = self.group(key, cx) else {
            return;
        };

        if group.read(cx).loading_more() || group.read(cx).oldest().is_none() {
            return;
        }

        self.routes.insert(page_subscription(key), key.clone());

        group.update(cx, |group, cx| {
            group.load_page(cx);
        });
    }

    pub fn leave(&mut self, key: &GroupKey, cx: &mut Context<Self>) {
        let Some(group) = self.group(key, cx) else {
            return;
        };

        let builder = key.id().leave_request("");
        let sent = group.read_with(cx, |group, cx| group.publish(builder, cx));
        let key = key.clone();

        self.tasks.push(cx.spawn(async move |this, cx| {
            if let Err(error) = sent.await {
                log::warn!("leaving {} failed: {error}", key.id());
                return;
            }
            this.update(cx, |this, cx| {
                this.forget(key, cx);
            })
            .ok();
        }));
    }

    pub(crate) fn reset(&mut self, cx: &mut Context<Self>) {
        self.notification_listener = None;
        self.signal_consumer = None;
        self.tasks.clear();

        let routes = std::mem::take(&mut self.routes);

        self.groups.clear();
        self.previews.clear();
        self.observers.clear();
        self.forks.clear();
        self.fork_authors.clear();
        self.syncing.clear();
        self.list = GroupList::default();

        let nostr = NostrRegistry::global(cx);
        let client = nostr.read(cx).client();

        let mut ids: Vec<SubscriptionId> = routes.into_keys().collect();
        ids.push(list_subscription_id());
        ids.push(fork_subscription_id());
        ids.extend(self.browse.keys().cloned());

        self.browse.clear();

        self.tasks.push(cx.background_spawn(async move {
            for id in ids {
                if let Err(error) = client.unsubscribe(&id).await {
                    log::warn!("unsubscribing {id} failed: {error}");
                }
            }
        }));

        cx.notify();
    }

    fn load_cached(&mut self, cx: &mut Context<Self>) {
        let nostr = NostrRegistry::global(cx);

        if nostr.read(cx).current_user().is_none() {
            return;
        }

        let client = nostr.read(cx).client();
        let signer = nostr.read(cx).signer();

        // Show the cached groups before the relays answer.
        let task: Task<Result<GroupList, Error>> = cx.background_spawn(async move {
            let me = signer.get_public_key_async().await?;
            let filter = Filter::new().author(me).kind(Kind::SimpleGroups).limit(1);

            match client.database().query(filter).await?.into_iter().next() {
                Some(event) => {
                    let list = GroupList::parse(&event)?;
                    Ok(Self::hydrate_list(list, &signer, &me).await)
                }
                None => Ok(GroupList::default()),
            }
        });

        self.tasks
            .push(cx.spawn(async move |this, cx| match task.await {
                Ok(list) => {
                    this.update(cx, |this, cx| {
                        this.merge(list, cx);
                    })
                    .ok();
                }
                Err(error) => {
                    log::warn!("loading the cached group list failed: {error}");
                }
            }));
    }

    fn load(&mut self, cx: &mut Context<Self>) {
        let nostr = NostrRegistry::global(cx);
        let client = nostr.read(cx).client();
        let signer = nostr.read(cx).signer();

        cx.background_spawn(async move {
            let me = signer.get_public_key_async().await?;
            let opts = SubscribeAutoCloseOptions::default().exit_policy(ReqExitPolicy::ExitOnEOSE);
            let filter = Filter::new().author(me).kind(Kind::SimpleGroups).limit(1);

            client
                .subscribe(ReqTarget::auto([filter]))
                .with_id(list_subscription_id())
                .close_on(opts)
                .await?;

            Ok::<(), anyhow::Error>(())
        })
        .detach();

        self.watch_forks(cx);
    }

    fn merge(&mut self, list: GroupList, cx: &mut Context<Self>) {
        // Carry the event's private section so a later publish can preserve it.
        self.list
            .set_content(list.content().map(str::to_owned), list.content_readable());

        let entries = list.entries().to_vec();

        for entry in entries {
            self.list.upsert(entry.clone());
            self.ensure(GroupKey::new(entry.relay.clone(), entry.id.clone()), cx);
        }

        cx.emit(GroupsEvent::Updated);
        cx.notify();
    }

    /// Decrypt a list event's private section into its entries.
    async fn hydrate_list(
        mut list: GroupList,
        signer: &UniversalSigner,
        me: &PublicKey,
    ) -> GroupList {
        let Some(content) = list.content().map(str::to_owned) else {
            return list;
        };

        match signer.nip44_decrypt(me, &content).await {
            Ok(plaintext) => match parse_private_section(&plaintext) {
                Some(entries) => {
                    list.merge_private(entries);
                    list.mark_readable();
                }
                None => log::warn!("ignoring a malformed private group list section"),
            },
            Err(error) => log::warn!("decrypting the private group list failed: {error}"),
        }

        list
    }

    /// Absorb a list event, decrypting its private section off the hot path.
    fn absorb_list(&mut self, event: Event, cx: &mut Context<Self>) {
        let list = match GroupList::parse(&event) {
            Ok(list) => list,
            Err(error) => {
                log::warn!("ignoring a malformed group list: {error}");
                return;
            }
        };

        if list.content().is_none() {
            self.merge(list, cx);
            return;
        }

        let nostr = NostrRegistry::global(cx);
        let signer = nostr.read(cx).signer();
        let Some(me) = nostr.read(cx).current_user() else {
            self.merge(list, cx);
            return;
        };

        self.tasks.push(cx.spawn(async move |this, cx| {
            let list = Self::hydrate_list(list, &signer, &me).await;
            this.update(cx, |this, cx| this.merge(list, cx)).ok();
        }));
    }

    fn watch_forks(&mut self, cx: &mut Context<Self>) {
        let executor = cx.background_executor().clone();
        let tx = self.signal_tx.clone();

        self.tasks.push(cx.background_spawn(async move {
            loop {
                executor.timer(FORK_INTERVAL).await;
                if tx.send_async(Signal::Forks).await.is_err() {
                    break;
                }
            }
        }));
    }

    fn admin_keys(&self, cx: &App) -> HashSet<PublicKey> {
        let mut keys = HashSet::new();

        for group in &self.groups {
            for (admin, _) in group.read(cx).admins().entries() {
                keys.insert(*admin);
            }
        }

        keys
    }

    fn sync_forks(&mut self, force: bool, cx: &mut Context<Self>) {
        let authors = self.admin_keys(cx);

        if authors.is_empty() || (!force && authors == self.fork_authors) {
            return;
        }

        let nostr = NostrRegistry::global(cx);
        let client = nostr.read(cx).client();
        let filter = Filter::new()
            .authors(authors.iter().copied())
            .kind(Kind::SimpleGroups);

        self.fork_authors = authors;

        self.tasks.push(cx.background_spawn(async move {
            if let Err(error) = client
                .subscribe(ReqTarget::auto([filter]))
                .with_id(fork_subscription_id())
                .await
            {
                log::warn!("subscribing to the admins' group lists failed: {error}");
            }
        }));
    }

    fn absorb_fork_list(&mut self, event: &Event, cx: &mut Context<Self>) {
        let Ok(list) = GroupList::parse(event) else {
            log::warn!("ignoring a malformed admin group list");
            return;
        };

        let author = event.pubkey;

        if let Some((seen, _)) = self.forks.get(&author)
            && *seen >= event.created_at
        {
            return;
        }

        self.forks.insert(author, (event.created_at, list));
        self.evaluate_forks(cx);
    }

    fn evaluate_forks(&mut self, cx: &mut Context<Self>) {
        let groups = self.groups.clone();

        for group in groups {
            let elsewhere = {
                let group = group.read(cx);
                let key = group.key();

                group
                    .admins()
                    .entries()
                    .iter()
                    .filter_map(|(admin, _)| self.forks.get(admin))
                    .flat_map(|(_, list)| list.entries())
                    .find(|entry| entry.id == *key.id() && entry.relay != *key.relay())
                    .map(|entry| GroupKey::new(entry.relay.clone(), entry.id.clone()))
            };

            group.update(cx, |group, cx| {
                group.set_elsewhere(elsewhere, cx);
            });
        }
    }

    fn ensure(&mut self, key: GroupKey, cx: &mut Context<Self>) {
        // A previewed group becomes one of the user's groups, keeping its state.
        if let Some(group) = self.previews.remove(&key) {
            self.groups.push(group);
            return;
        }

        if self.group(&key, cx).is_some() {
            return;
        }

        let group = cx.new(|_| Group::new(key.clone()));
        self.groups.push(group.clone());

        self.observe_group(key.clone(), &group, cx);
        self.subscribe(key.clone(), cx);
        self.load_state(key, cx);
    }

    /// Track a group opened for preview: connected, but not joined.
    pub fn open(&mut self, key: GroupKey, cx: &mut Context<Self>) -> Entity<Group> {
        if let Some(group) = self.group(&key, cx) {
            return group;
        }

        let group = cx.new(|_| Group::new(key.clone()));
        self.previews.insert(key.clone(), group.clone());

        self.observe_group(key.clone(), &group, cx);
        self.subscribe(key.clone(), cx);
        self.load_state(key, cx);

        group
    }

    fn observe_group(&mut self, key: GroupKey, group: &Entity<Group>, cx: &mut Context<Self>) {
        self.observers.insert(
            key,
            cx.subscribe(group, |this, group, event, cx| match event {
                GroupEvent::Sent(event) => {
                    if event.kind == Kind::GroupDeleteEvent {
                        this.forget_event(group, event.clone(), cx);
                    } else {
                        let key = group.read(cx).key().clone();
                        this.reload(key, Some(event.clone()), cx);
                    }
                }
                GroupEvent::Updated => {
                    let key = group.read(cx).key().clone();

                    // Only the user's groups belong in their group list.
                    if this.joined(&key, cx) {
                        this.refresh_entry(group, cx);
                    }

                    this.sync_forks(false, cx);

                    cx.emit(GroupsEvent::Updated);
                    cx.notify();
                }
                _ => {}
            }),
        );
    }

    fn subscribe(&mut self, key: GroupKey, cx: &mut Context<Self>) {
        let Some(group) = self.group(&key, cx) else {
            return;
        };

        self.routes.insert(subscription_id(&key), key);

        group.update(cx, |group, cx| {
            group.connect(cx);
        });
    }

    fn forget(&mut self, key: GroupKey, cx: &mut Context<Self>) {
        let id = subscription_id(&key);
        let page = page_subscription(&key);
        let cache_tag = key.cache_tag();

        self.routes.remove(&id);
        self.routes.remove(&page);
        self.observers.remove(&key);
        self.groups.retain(|group| group.read(cx).key() != &key);
        self.previews.remove(&key);
        self.list.remove(&key);

        let nostr = NostrRegistry::global(cx);
        let client = nostr.read(cx).client();

        self.tasks.push(cx.background_spawn(async move {
            for id in [id, page] {
                if let Err(error) = client.unsubscribe(&id).await {
                    log::warn!("unsubscribing {id} failed: {error}");
                }
            }

            let filter = Filter::new()
                .kind(Kind::ApplicationSpecificData)
                .custom_tag(SingleLetterTag::LOWERCASE_R, cache_tag);

            if let Err(error) = client.database().delete(filter).await {
                log::warn!("clearing the group cache failed: {error}");
            }
        }));

        self.publish_list(cx);

        cx.emit(GroupsEvent::Updated);
        cx.notify();
    }

    fn entry_name(&self, key: &GroupKey) -> Option<String> {
        self.list
            .entries()
            .iter()
            .find(|entry| entry.id == *key.id() && entry.relay == *key.relay())
            .and_then(|entry| entry.name.clone())
    }

    fn upsert_entry(&mut self, key: &GroupKey, name: Option<String>, private: bool) {
        let name = name.or_else(|| self.entry_name(key));

        self.list.upsert(GroupListEntry {
            id: key.id().clone(),
            relay: key.relay().clone(),
            name,
            private,
        });
    }

    fn refresh_entry(&mut self, group: Entity<Group>, cx: &mut Context<Self>) {
        let (key, Some(name)) = ({
            let group = group.read(cx);

            (
                group.key().clone(),
                group
                    .metadata()
                    .and_then(|metadata| metadata.name().map(str::to_owned)),
            )
        }) else {
            return;
        };

        if self.entry_name(&key).as_deref() == Some(name.as_str()) {
            return;
        }

        let private = self.list.entry(&key).is_some_and(|entry| entry.private);

        self.upsert_entry(&key, Some(name), private);
        self.publish_list(cx);
    }

    fn publish_list(&mut self, cx: &mut Context<Self>) {
        let nostr = NostrRegistry::global(cx);
        let client = nostr.read(cx).client();
        let signer = nostr.read(cx).signer();
        let me = nostr.read(cx).current_user();
        let list = self.list.clone();

        self.tasks.push(cx.spawn(async move |_this, _cx| {
            // Re-encrypt the private section when it carries local entries; a
            // section this session could not read is carried verbatim.
            let content = match (list.has_private(), list.content_readable()) {
                (true, _) => match list.encode_private().zip(me) {
                    Some((plaintext, me)) => match signer.nip44_encrypt(&me, &plaintext).await {
                        Ok(content) => Some(content),
                        Err(error) => {
                            log::warn!("encrypting the private group list failed: {error}");
                            list.content().map(str::to_owned)
                        }
                    },
                    None => None,
                },
                (false, false) => list.content().map(str::to_owned),
                (false, true) => None,
            };

            let builder = EventBuilder::new(Kind::SimpleGroups, content.unwrap_or_default())
                .tags(list.public_tags());

            let event = match builder.finalize_async(&signer).await {
                Ok(event) => event,
                Err(error) => {
                    log::warn!("signing the group list failed: {error}");
                    return;
                }
            };

            match client.send_event(&event).to_nip65().await {
                Ok(output) if output.failed.is_empty() => {}
                Ok(output) => {
                    log::warn!("{} relay(s) rejected the group list", output.failed.len())
                }
                Err(error) => log::warn!("publishing the group list failed: {error}"),
            }
        }));
    }

    fn reload(&mut self, key: GroupKey, event: Option<Event>, cx: &mut Context<Self>) {
        let nostr = NostrRegistry::global(cx);

        let Some(group) = self.group(&key, cx) else {
            return;
        };

        let Some(me) = nostr.read(cx).current_user() else {
            return;
        };

        let logged = key.clone();
        let load = group.read(cx).load_envelopes(cx, false);

        self.tasks.push(cx.spawn(async move |_this, cx| {
            if let Some(event) = event
                && let Err(error) = group
                    .read_with(cx, |group, cx| group.save_envelope(event, cx))
                    .await
            {
                log::warn!("caching failed: {error}");
                return;
            }

            match load.await {
                Ok(events) => {
                    group.update(cx, |group, cx| {
                        group.hydrate(events, me, cx);
                    });
                }
                Err(error) => {
                    log::warn!("reloading {} failed: {error}", logged.id());
                }
            }
        }));
    }

    /// Load only the group state (metadata, members, membership) from the cache.
    fn load_state(&mut self, key: GroupKey, cx: &mut Context<Self>) {
        let Some(group) = self.group(&key, cx) else {
            return;
        };

        let Some(me) = NostrRegistry::global(cx).read(cx).current_user() else {
            return;
        };

        let logged = key.clone();
        let load = group.read(cx).load_envelopes(cx, true);

        self.tasks
            .push(cx.spawn(async move |_this, cx| match load.await {
                Ok(events) => {
                    group.update(cx, |group, cx| {
                        group.hydrate(events, me, cx);
                    });
                }
                Err(error) => {
                    log::warn!("loading {} state failed: {error}", logged.id());
                }
            }));
    }

    /// Cache and absorb a single event without rebuilding the group.
    fn absorb(&mut self, key: GroupKey, event: Event, cx: &mut Context<Self>) {
        let Some(group) = self.group(&key, cx) else {
            return;
        };

        let Some(me) = NostrRegistry::global(cx).read(cx).current_user() else {
            return;
        };

        self.tasks.push(cx.spawn(async move |_this, cx| {
            if let Err(error) = group
                .read_with(cx, |group, cx| group.save_envelope(event.clone(), cx))
                .await
            {
                log::warn!("caching failed: {error}");
                return;
            }

            group.update(cx, |group, cx| {
                group.hydrate(vec![event], me, cx);
            });
        }));
    }

    /// Load the full group content on first open.
    pub fn activate(&mut self, key: &GroupKey, cx: &mut Context<Self>) {
        let Some(group) = self.group(key, cx) else {
            return;
        };

        if group.read(cx).is_active() {
            return;
        }

        group.update(cx, |group, _cx| group.activate());
        self.reload(key.clone(), None, cx);
    }

    fn forget_event(&mut self, group: Entity<Group>, event: Event, cx: &mut Context<Self>) {
        let Some(target) = event.tags.event_ids().next() else {
            return;
        };

        let key = group.read(cx).key().clone();
        let cache_tag = key.cache_tag();

        let nostr = NostrRegistry::global(cx);
        let client = nostr.read(cx).client();

        let purge = cx.background_spawn(async move {
            let filter = Filter::new()
                .kind(Kind::ApplicationSpecificData)
                .identifier(target.to_hex())
                .custom_tag(SingleLetterTag::LOWERCASE_R, cache_tag);

            client.database().delete(filter).await
        });

        self.tasks.push(cx.spawn(async move |this, cx| {
            if let Err(error) = purge.await {
                log::warn!("dropping the deleted envelope failed: {error}");
            }
            this.update(cx, |this, cx| {
                group.update(cx, |group, cx| {
                    group.forget(target, cx);
                });
                this.reload(key, Some(event), cx);
            })
            .ok();
        }));
    }

    fn handle_event(&mut self, id: SubscriptionId, event: Event, cx: &mut Context<Self>) {
        if id == list_subscription_id() {
            self.absorb_list(event, cx);
            return;
        }

        if id == fork_subscription_id() {
            self.absorb_fork_list(&event, cx);
            return;
        }

        if self.browse.contains_key(&id) {
            self.absorb_candidate(&id, &event, cx);
            return;
        }

        let Some(key) = self.routes.get(&id).cloned() else {
            return;
        };

        let active = self
            .group(&key, cx)
            .is_some_and(|group| group.read(cx).is_active());

        if active {
            self.reload(key, Some(event), cx);
        } else {
            self.absorb(key, event, cx);
        }
    }

    fn absorb_candidate(&mut self, id: &SubscriptionId, event: &Event, cx: &mut Context<Self>) {
        let Some(relay) = self.browse.get(id).cloned() else {
            return;
        };

        if let Some((group, created_at, members)) = parse_members(event) {
            cx.emit(GroupsEvent::Members {
                key: GroupKey::new(relay, group),
                created_at,
                members,
            });
            return;
        }

        let Ok(metadata) = GroupMetadata::parse(event) else {
            log::warn!("ignoring malformed group metadata from {relay}");
            return;
        };

        let Some(group) = metadata.id().cloned() else {
            return;
        };

        cx.emit(GroupsEvent::Browsed(Box::new(GroupCandidate {
            key: GroupKey::new(relay, group),
            created_at: event.created_at,
            metadata,
        })));
    }

    fn handle_eose(&mut self, id: SubscriptionId, cx: &mut Context<Self>) {
        let Some(key) = self.routes.get(&id).cloned() else {
            return;
        };

        if is_page(&id) {
            self.routes.remove(&id);

            if let Some(group) = self.group(&key, cx) {
                group.update(cx, |group, cx| {
                    group.page_finished(cx);
                });
            }

            return;
        }

        let Some(group) = self.group(&key, cx) else {
            return;
        };

        group.update(cx, |group, cx| {
            group.eose(cx);
        });

        cx.notify();
    }

    fn handle_closed(&mut self, id: SubscriptionId, reason: String, cx: &mut Context<Self>) {
        if let Some(relay) = self.browse.get(&id) {
            log::warn!("browsing {relay} was refused: {reason}");
            return;
        }

        if id == fork_subscription_id() {
            log::debug!("the admins' group lists were refused: {reason}");
            return;
        }

        let Some(key) = self.routes.get(&id).cloned() else {
            return;
        };

        if is_page(&id) {
            self.routes.remove(&id);
            log::warn!(
                "loading older messages in {} was refused: {reason}",
                key.id()
            );

            if let Some(group) = self.group(&key, cx) {
                group.update(cx, |group, cx| {
                    group.page_finished(cx);
                });
            }

            return;
        }

        let Some(group) = self.group(&key, cx) else {
            return;
        };

        group.update(cx, |group, cx| {
            group.refuse(reason, cx);
        });

        cx.notify();
    }

    fn handle_notifications(&mut self, cx: &mut Context<Self>) {
        self.notification_listener = None;
        self.signal_consumer = None;

        let nostr = NostrRegistry::global(cx).read(cx);
        let client = nostr.client();

        let tx = self.signal_tx.clone();
        let rx = self.signal_rx.clone();

        self.notification_listener = Some(cx.background_spawn(async move {
            let mut notifications = client.notifications();
            let mut processed = HashSet::new();
            const MAX_PROCESSED: usize = 10_000;

            while let Some(notification) = notifications.next().await {
                let ClientNotification::Message { relay_url, message } = notification else {
                    continue;
                };

                match *message {
                    RelayMessage::Event {
                        subscription_id,
                        event,
                    } if is_own_subscription(&subscription_id) => {
                        if processed.len() >= MAX_PROCESSED {
                            processed.clear();
                        }

                        if processed.insert(event.id) {
                            let signal = Signal::Event(
                                subscription_id.into_owned(),
                                Box::new(event.into_owned()),
                            );

                            tx.send_async(signal).await.ok();
                        }
                    }
                    RelayMessage::EndOfStoredEvents(id) if is_own_subscription(&id) => {
                        tx.send_async(Signal::Eose(id.into_owned())).await.ok();
                    }
                    RelayMessage::Closed {
                        subscription_id,
                        message,
                    } if is_own_subscription(&subscription_id) => {
                        let signal =
                            Signal::Closed(subscription_id.into_owned(), message.into_owned());
                        tx.send_async(signal).await.ok();
                    }
                    RelayMessage::NegMsg { .. } => {
                        tx.send_async(Signal::Syncing(relay_url)).await.ok();
                    }
                    RelayMessage::NegErr { .. } | RelayMessage::EndOfStoredEvents(_) => {
                        tx.send_async(Signal::Synced(relay_url)).await.ok();
                    }
                    _ => {}
                }
            }
        }));

        self.signal_consumer = Some(cx.spawn(async move |this, cx| {
            while let Ok(signal) = rx.recv_async().await {
                this.update(cx, |this, cx| match signal {
                    Signal::Event(id, event) => {
                        this.handle_event(id, *event, cx);
                    }
                    Signal::Eose(id) => {
                        this.handle_eose(id, cx);
                    }
                    Signal::Closed(id, reason) => {
                        this.handle_closed(id, reason, cx);
                    }
                    Signal::BrowseSettled(id) => {
                        this.settle_browse(id, cx);
                    }
                    Signal::Syncing(relay) => {
                        this.syncing.insert(relay);
                    }
                    Signal::Synced(relay) => {
                        if this.syncing.remove(&relay) {
                            cx.emit(GroupsEvent::Synced);
                        }
                    }
                    Signal::Forks => {
                        this.sync_forks(true, cx);
                    }
                })
                .ok();
            }
        }));
    }
}
