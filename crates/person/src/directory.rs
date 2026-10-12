use std::cmp::Reverse;
use std::collections::{HashMap, HashSet};
use std::sync::RwLock;

use anyhow::Error;
use common::emojis_from_tags;
use gpui::{App, AppContext, Context, Entity, Subscription, Task};
use nostr_sdk::prelude::*;
use smallvec::{SmallVec, smallvec};
use state::NostrRegistry;

use crate::{Person, PersonEvent, PersonRegistry};

/// Directory of persons tracked by a view
#[derive(Debug)]
pub struct Directory {
    persons: HashMap<PublicKey, Person>,
    /// Keys awaiting their first update, guarded for render-time access
    pending: RwLock<HashSet<PublicKey>>,
    tasks: SmallVec<[Task<()>; 2]>,
    _subscriptions: SmallVec<[Subscription; 1]>,
}

impl Directory {
    /// Create a directory following the registry's events
    pub fn new(cx: &mut App) -> Entity<Self> {
        let registry = PersonRegistry::global(cx);

        cx.new(|cx| {
            let subscription = cx.subscribe(&registry, |this: &mut Self, _registry, event, cx| {
                this.handle_event(event.clone(), cx);
            });

            Self {
                persons: HashMap::new(),
                pending: RwLock::new(HashSet::new()),
                tasks: SmallVec::new(),
                _subscriptions: smallvec![subscription],
            }
        })
    }

    /// Load known profiles from the database, request the rest
    pub fn track(&mut self, keys: impl IntoIterator<Item = PublicKey>, cx: &mut Context<Self>) {
        /// Profiles and emoji lists loaded from the database
        type Loaded = (Vec<Person>, Vec<(PublicKey, Vec<(String, Url)>)>);

        let missing: Vec<PublicKey> = keys
            .into_iter()
            .filter(|key| {
                !self.persons.contains_key(key)
                    && !self.with_pending(|pending| pending.contains(key))
            })
            .collect();

        if missing.is_empty() {
            return;
        }

        // Accept events that arrive while the database query is running
        self.with_pending(|pending| {
            pending.extend(missing.iter().copied());
        });

        let nostr = NostrRegistry::global(cx);
        let client = nostr.read(cx).client();
        let authors = missing.clone();

        let task: Task<Result<Loaded, Error>> = cx.background_spawn(async move {
            let mut metadata_events: Vec<Event> = client
                .database()
                .query(Filter::new().kind(Kind::Metadata).authors(authors.clone()))
                .await?
                .into_iter()
                .collect();

            // Keep only the newest event per author
            metadata_events.sort_by_key(|event| Reverse(event.created_at));
            let mut seen = HashSet::new();

            let persons = metadata_events
                .into_iter()
                .filter(|event| seen.insert(event.pubkey))
                .map(|event| {
                    let metadata = Metadata::from_json(event.content).unwrap_or_default();
                    Person::new(event.pubkey, metadata)
                })
                .collect();

            let mut emoji_events: Vec<Event> = client
                .database()
                .query(Filter::new().kind(Kind::Emojis).authors(authors))
                .await?
                .into_iter()
                .collect();

            // Keep only the newest event per author
            emoji_events.sort_by_key(|event| Reverse(event.created_at));
            let mut seen = HashSet::new();

            let emojis = emoji_events
                .into_iter()
                .filter(|event| seen.insert(event.pubkey))
                .map(|event| (event.pubkey, emojis_from_tags(&event.tags)))
                .collect();

            Ok((persons, emojis))
        });

        self.tasks.push(cx.spawn(async move |this, cx| {
            let loaded = task.await;

            this.update(cx, |this, cx| {
                let remaining = match loaded {
                    Ok((persons, emojis)) => {
                        let found: HashSet<PublicKey> =
                            persons.iter().map(Person::public_key).collect();
                        this.bulk_insert(persons, cx);

                        // Emojis apply on top of loaded or already received data
                        let mut updated = false;

                        for (public_key, emojis) in emojis {
                            if emojis.is_empty() {
                                continue;
                            }

                            let person = this
                                .persons
                                .entry(public_key)
                                .or_insert_with(|| Person::new(public_key, Metadata::default()));
                            person.set_emojis(emojis);
                            updated = true;
                        }

                        if updated {
                            cx.notify();
                        }

                        missing
                            .into_iter()
                            .filter(|key| !found.contains(key))
                            .collect()
                    }
                    Err(e) => {
                        log::warn!("Failed to load persons from the database: {e}");
                        missing
                    }
                };

                if !remaining.is_empty() {
                    let registry = PersonRegistry::global(cx);
                    registry.read(cx).fetch(remaining);
                }
            })
            .ok();
        }));
    }

    /// The person for a key, or a placeholder that queues a request
    pub fn person(&self, public_key: &PublicKey, cx: &App) -> Person {
        if let Some(person) = self.persons.get(public_key) {
            return person.clone();
        }

        if self.with_pending(|pending| pending.insert(*public_key)) {
            let registry = PersonRegistry::global(cx);
            registry.read(cx).fetch([*public_key]);
        }

        Person::new(*public_key, Metadata::default())
    }

    /// Merge event data into the tracked person
    fn handle_event(&mut self, event: PersonEvent, cx: &mut Context<Self>) {
        let public_key = match &event {
            PersonEvent::Metadata(person) => person.public_key(),
            PersonEvent::Emojis { public_key, .. }
            | PersonEvent::Announcement { public_key, .. }
            | PersonEvent::MessagingRelays { public_key, .. } => *public_key,
        };

        if !self.persons.contains_key(&public_key)
            && !self.with_pending(|pending| pending.remove(&public_key))
        {
            return;
        }

        let person = self
            .persons
            .entry(public_key)
            .or_insert_with(|| Person::new(public_key, Metadata::default()));

        match event {
            PersonEvent::Metadata(updated) => person.set_metadata(updated.metadata()),
            PersonEvent::Emojis { emojis, .. } => person.set_emojis(emojis),
            PersonEvent::Announcement { announcement, .. } => {
                person.set_announcement(announcement);
            }
            PersonEvent::MessagingRelays { relays, .. } => person.set_messaging_relays(relays),
        }

        cx.notify();
    }

    /// Insert loaded persons, keeping data already received from relays
    fn bulk_insert(&mut self, persons: Vec<Person>, cx: &mut Context<Self>) {
        let mut inserted = false;

        for person in persons {
            let public_key = person.public_key();

            if self.persons.contains_key(&public_key) {
                continue;
            }

            self.persons.insert(public_key, person);
            inserted = true;
        }

        if inserted {
            cx.notify();
        }
    }

    /// Run `then` with the pending set, tolerating poisoning: it is only a dedupe cache
    fn with_pending<R>(&self, then: impl FnOnce(&mut HashSet<PublicKey>) -> R) -> R {
        let mut pending = match self.pending.write() {
            Ok(pending) => pending,
            Err(poisoned) => poisoned.into_inner(),
        };
        then(&mut pending)
    }
}
