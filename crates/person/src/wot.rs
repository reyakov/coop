use std::collections::{HashMap, HashSet};

use gpui::{Context, EventEmitter, Task};
use instant::Duration;
use nostr_sdk::prelude::*;
use smallvec::{SmallVec, smallvec};
use state::NostrRegistry;

/// Disable scoring when the follow set exceeds this size
const MAX_FOLLOWS: usize = 2000;
/// Cap a followed author's contact list to bound hostile publishers
const MAX_TAGS_PER_LIST: usize = 5000;
/// Time between reconciliations with the relays
const SYNC_INTERVAL: u64 = 15 * 60;
/// Longest wait between retries while the graph has no data yet
const MAX_RETRY_WAIT: u64 = 60;

/// The web of trust graph changed
#[derive(Debug, Clone)]
pub struct WoTEvent;

/// A built graph: the follow set and, per public key, who follows it
#[derive(Debug)]
struct Snapshot {
    follows: HashSet<PublicKey>,
    scores: HashMap<PublicKey, usize>,
    mutuals: HashMap<PublicKey, Vec<PublicKey>>,
    disabled: bool,
}

impl Snapshot {
    /// Build the snapshot from the newest stored contact list of every author
    fn build(self_key: PublicKey, follows: HashSet<PublicKey>, events: Vec<Event>) -> Self {
        let disabled = follows.len() > MAX_FOLLOWS;

        let mut snapshot = Self {
            follows,
            scores: HashMap::new(),
            mutuals: HashMap::new(),
            disabled,
        };

        if snapshot.disabled {
            return snapshot;
        }

        // Keep the newest contact list per author
        let mut latest: HashMap<PublicKey, Event> = HashMap::new();

        for event in events {
            if event.pubkey == self_key || !snapshot.follows.contains(&event.pubkey) {
                continue;
            }

            match latest.get(&event.pubkey) {
                Some(current) if current.created_at > event.created_at => continue,
                _ => {
                    latest.insert(event.pubkey, event);
                }
            }
        }

        for (author, event) in latest {
            for key in event.tags.public_keys().take(MAX_TAGS_PER_LIST) {
                if key == author || key == self_key {
                    continue;
                }

                *snapshot.scores.entry(key).or_default() += 1;
                snapshot.mutuals.entry(key).or_default().push(author);
            }
        }

        for contacts in snapshot.mutuals.values_mut() {
            contacts.sort();
        }

        snapshot
    }
}

impl EventEmitter<WoTEvent> for WebOfTrust {}

/// Web of trust
///
/// Scores public keys by how many of the current user's contacts follow them.
#[derive(Debug)]
pub struct WebOfTrust {
    /// Current user's public key
    self_key: Option<PublicKey>,
    /// Latest built snapshot, none until the first refresh completes
    snapshot: Option<Snapshot>,
    /// In-flight refresh, replaced by newer ones
    refresh_task: Option<Task<()>>,
    /// Keeps the periodic reconciliation loop alive
    _tasks: SmallVec<[Task<()>; 4]>,
}

impl WebOfTrust {
    /// Create a new web of trust instance
    pub(crate) fn new(cx: &mut Context<Self>) -> Self {
        let entity = cx.entity().downgrade();
        let mut tasks = smallvec![];

        tasks.push(cx.spawn(async move |this, cx| {
            let mut delay = Duration::from_secs(5);

            loop {
                cx.background_executor().timer(delay).await;

                let mut has_data = false;

                // Ignore errors, the next refresh will retry
                let _ = this.update(cx, |this, cx| {
                    this.refresh(cx);
                    has_data = this.has_data();
                });

                delay = if has_data {
                    Duration::from_secs(SYNC_INTERVAL)
                } else {
                    (delay * 2).min(Duration::from_secs(MAX_RETRY_WAIT))
                };
            }
        }));

        cx.defer(move |cx| {
            let _ = entity.update(cx, |this, cx| {
                this.hydrate(cx);
            });
        });

        Self {
            self_key: None,
            snapshot: None,
            refresh_task: None,
            _tasks: tasks,
        }
    }

    /// Load the graph for the current account
    pub(crate) fn hydrate(&mut self, cx: &mut Context<Self>) {
        let nostr = NostrRegistry::global(cx);
        let Some(self_key) = nostr.read(cx).current_user() else {
            return;
        };

        self.self_key = Some(self_key);
        self.refresh(cx);
    }

    /// Clear the graph on sign out or account switch
    pub(crate) fn reset(&mut self, cx: &mut Context<Self>) {
        self.self_key = None;
        self.snapshot = None;
        self.refresh_task = None;

        cx.emit(WoTEvent);
        cx.notify();
    }

    /// Reconcile the follow set's contact lists with the relays and rebuild.
    fn refresh(&mut self, cx: &mut Context<Self>) {
        let nostr = NostrRegistry::global(cx);
        let client = nostr.read(cx).client();

        let Some(self_key) = nostr.read(cx).current_user() else {
            return;
        };

        self.refresh_task = Some(cx.spawn(async move |this, cx| {
            // Scope the reconcile to the follow set known so far
            let known: HashSet<PublicKey> = stored_contact_list(&client, self_key)
                .await
                .map(|event| event.tags.public_keys().take(MAX_FOLLOWS + 1).collect())
                .unwrap_or_default();

            let mut authors: Vec<PublicKey> = known.iter().copied().collect();
            authors.push(self_key);

            // Negentropy transfers only the lists the relays hold
            // and the database is missing
            let filter = Filter::new().kind(Kind::ContactList).authors(authors);
            if let Err(error) = client.sync(filter).await {
                log::warn!("Failed to reconcile contact lists: {error}");
            }

            // The follow set after the reconcile
            let Some(own) = stored_contact_list(&client, self_key).await else {
                return;
            };

            let follows: HashSet<PublicKey> =
                own.tags.public_keys().take(MAX_FOLLOWS + 1).collect();

            let mut authors: Vec<PublicKey> = follows.iter().copied().collect();
            authors.push(self_key);

            let events: Vec<Event> = if follows.is_empty() {
                Vec::new()
            } else {
                let filter = Filter::new().kind(Kind::ContactList).authors(authors);
                client
                    .database()
                    .query(filter)
                    .await
                    .unwrap_or_default()
                    .into_iter()
                    .collect()
            };

            let snapshot = Snapshot::build(self_key, follows.clone(), events);
            log::debug!(
                "Web of trust rebuilt: {} follows, {} scored keys",
                snapshot.follows.len(),
                snapshot.scores.len()
            );

            // Newly followed authors were not part of the reconcile
            let grew = !known.is_superset(&follows);

            this.update(cx, |this, cx| {
                this.snapshot = Some(snapshot);
                cx.emit(WoTEvent);
                cx.notify();
            })
            .ok();

            if grew {
                this.update(cx, |this, cx| {
                    this.refresh(cx);
                })
                .ok();
            }
        }));
    }

    /// How many of the current user's contacts follow the public key
    pub fn score(&self, public_key: &PublicKey) -> usize {
        self.snapshot.as_ref().map_or(0, |snapshot| {
            snapshot.scores.get(public_key).copied().unwrap_or(0)
        })
    }

    /// Whether the public key is followed by the user or by any of their contacts
    pub fn is_in_network(&self, public_key: &PublicKey) -> bool {
        self.snapshot
            .as_ref()
            .is_some_and(|snapshot| snapshot.follows.contains(public_key))
            || self.score(public_key) > 0
    }

    /// Whether the graph is loaded and the public key is outside the network
    pub fn is_outside(&self, public_key: &PublicKey) -> bool {
        self.is_loaded() && !self.is_in_network(public_key)
    }

    /// The current user's contacts who follow the public key
    pub fn mutuals(&self, public_key: &PublicKey) -> Vec<PublicKey> {
        self.snapshot.as_ref().map_or_else(Vec::new, |snapshot| {
            snapshot
                .mutuals
                .get(public_key)
                .cloned()
                .unwrap_or_default()
        })
    }

    /// Whether the graph is loaded and score queries are meaningful
    pub fn is_loaded(&self) -> bool {
        self.snapshot
            .as_ref()
            .is_some_and(|snapshot| !snapshot.disabled)
    }

    /// Whether the snapshot covers a follow set
    fn has_data(&self) -> bool {
        self.snapshot
            .as_ref()
            .is_some_and(|snapshot| !snapshot.follows.is_empty())
    }
}

/// The newest stored contact list of the public key
async fn stored_contact_list(client: &Client, self_key: PublicKey) -> Option<Event> {
    let filter = Filter::new()
        .author(self_key)
        .kind(Kind::ContactList)
        .limit(1);

    client
        .database()
        .query(filter)
        .await
        .unwrap_or_default()
        .into_iter()
        .next()
}
