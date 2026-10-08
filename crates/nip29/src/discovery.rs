use std::collections::{HashMap, HashSet};

use futures::future::join_all;
use nostr_sdk::prelude::*;

use crate::protocol::{GroupCandidate, GroupId, GroupKey, GroupList, GroupMetadata};
use crate::utils::parse_members;

#[derive(Default, Clone, Debug)]
pub struct Discovery {
    refs: HashMap<RelayUrl, HashSet<GroupId>>,
    discovered: Vec<GroupCandidate>,
    members: HashMap<GroupKey, (Timestamp, usize)>,
}

impl Discovery {
    /// The discovered groups, sorted by name.
    pub fn discovered(&self) -> &[GroupCandidate] {
        &self.discovered
    }

    /// The member count of a group, when its member list is known.
    pub fn members(&self, key: &GroupKey) -> Option<usize> {
        self.members.get(key).map(|(_, members)| *members)
    }

    /// The (relay, group) references, for asking the relays for metadata.
    pub(crate) fn refs(&self) -> &HashMap<RelayUrl, HashSet<GroupId>> {
        &self.refs
    }

    /// Absorb the references of a group list, skipping the joined groups.
    pub(crate) fn absorb_list(&mut self, list: &GroupList, joined: &HashSet<(RelayUrl, GroupId)>) {
        for entry in list.entries() {
            if joined.contains(&(entry.relay.clone(), entry.id.clone())) {
                continue;
            }

            self.refs
                .entry(entry.relay.clone())
                .or_default()
                .insert(entry.id.clone());
        }
    }

    /// Absorb the newest group list of every author.
    pub(crate) fn absorb_lists(
        &mut self,
        lists: Vec<Event>,
        joined: &HashSet<(RelayUrl, GroupId)>,
    ) {
        let newest: HashMap<PublicKey, Event> =
            lists.into_iter().fold(HashMap::new(), |mut newest, event| {
                match newest.get(&event.pubkey) {
                    Some(current) if current.created_at >= event.created_at => newest,
                    _ => {
                        newest.insert(event.pubkey, event);
                        newest
                    }
                }
            });

        for event in newest.into_values() {
            let Ok(list) = GroupList::parse(&event) else {
                log::warn!("ignoring a malformed group list from {}", event.pubkey);
                continue;
            };

            self.absorb_list(&list, joined);
        }
    }

    /// Add a discovered group, keeping the newest metadata.
    pub(crate) fn upsert(&mut self, candidate: GroupCandidate) {
        match self.discovered.iter_mut().find(|d| d.key == candidate.key) {
            Some(existing) if existing.created_at <= candidate.created_at => {
                *existing = candidate;
            }
            Some(_) => return,
            None => self.discovered.push(candidate),
        }

        self.discovered.sort_by(|a, b| {
            a.display_name()
                .to_lowercase()
                .cmp(&b.display_name().to_lowercase())
        });
    }

    /// Apply a member count, keeping the newest list.
    pub fn set_members(&mut self, key: GroupKey, created_at: Timestamp, members: usize) {
        if self
            .members
            .get(&key)
            .is_some_and(|(current, _)| *current > created_at)
        {
            return;
        }

        self.members.insert(key, (created_at, members));
    }

    /// Seed the discoveries from the cached group metadata.
    pub(crate) fn absorb_cached_previews(&mut self, previews: Vec<Event>) {
        let newest: HashMap<GroupId, (Timestamp, GroupMetadata)> =
            previews
                .into_iter()
                .fold(HashMap::new(), |mut newest, event| {
                    let metadata = match GroupMetadata::parse(&event) {
                        Ok(metadata) => metadata,
                        Err(error) => {
                            log::warn!("ignoring malformed cached group metadata: {error}");
                            return newest;
                        }
                    };

                    let Some(id) = metadata.id().cloned() else {
                        return newest;
                    };

                    match newest.get(&id) {
                        Some((created_at, _)) if *created_at >= event.created_at => newest,
                        _ => {
                            newest.insert(id, (event.created_at, metadata));
                            newest
                        }
                    }
                });

        for (relay, ids) in self.refs.clone() {
            for id in ids {
                let Some((created_at, metadata)) = newest.get(&id) else {
                    continue;
                };

                // Hidden groups are not discoverable (NIP-29).
                if metadata.hidden {
                    continue;
                }

                self.upsert(GroupCandidate {
                    key: GroupKey::new(relay.clone(), id.clone()),
                    created_at: *created_at,
                    metadata: metadata.clone(),
                });
            }
        }
    }

    /// Seed the member counts from the cached member lists.
    pub(crate) fn absorb_cached_members(&mut self, members: Vec<Event>) {
        let newest: HashMap<GroupId, (Timestamp, usize)> =
            members
                .into_iter()
                .fold(HashMap::new(), |mut newest, event| {
                    let Some((id, created_at, count)) = parse_members(&event) else {
                        return newest;
                    };

                    match newest.get(&id) {
                        Some((current, _)) if *current >= created_at => newest,
                        _ => {
                            newest.insert(id, (created_at, count));
                            newest
                        }
                    }
                });

        for (relay, ids) in self.refs.clone() {
            for id in ids {
                let Some((created_at, count)) = newest.get(&id) else {
                    continue;
                };

                self.set_members(
                    GroupKey::new(relay.clone(), id.clone()),
                    *created_at,
                    *count,
                );
            }
        }
    }

    /// Load the discoverable groups from the local database only.
    pub async fn cached(client: Client, joined: HashSet<(RelayUrl, GroupId)>) -> Self {
        let mut discovery = Self::default();

        // The group lists of every cached author.
        let lists: Vec<Event> = client
            .database()
            .query(Filter::new().kind(Kind::SimpleGroups))
            .await
            .map(|events| events.into_iter().collect())
            .unwrap_or_default();

        discovery.absorb_lists(lists, &joined);

        let refs = discovery.refs().clone();

        if refs.is_empty() {
            return discovery;
        }

        let ids: HashSet<&str> = refs.values().flatten().map(GroupId::as_str).collect();

        // The cached metadata of the referenced groups.
        let previews: Vec<Event> = client
            .database()
            .query(
                Filter::new()
                    .kind(Kind::GroupMetadata)
                    .identifiers(ids.clone()),
            )
            .await
            .map(|events| events.into_iter().collect())
            .unwrap_or_default();

        discovery.absorb_cached_previews(previews);

        // The cached member lists of the referenced groups.
        let members: Vec<Event> = client
            .database()
            .query(Filter::new().kind(Kind::GroupMembers).identifiers(ids))
            .await
            .map(|events| events.into_iter().collect())
            .unwrap_or_default();

        discovery.absorb_cached_members(members);

        discovery
    }

    /// Reconcile the discoverable groups with the relays.
    pub async fn sync(
        &mut self,
        client: Client,
        me: PublicKey,
        joined: HashSet<(RelayUrl, GroupId)>,
    ) {
        let filter = Filter::new().author(me).kind(Kind::ContactList).limit(1);

        let authors: HashSet<PublicKey> = client
            .database()
            .query(filter)
            .await
            .ok()
            .and_then(|events| events.into_iter().next())
            .map(|event| event.tags.public_keys().collect())
            .unwrap_or_default();

        let urls: Vec<RelayUrl> = client.relays().await.into_keys().collect();
        let filter = Filter::new().authors(authors).kind(Kind::SimpleGroups);
        client.sync(filter.clone()).with(urls).await.ok();

        let lists: Vec<Event> = client
            .database()
            .query(filter)
            .await
            .map(|events| events.into_iter().collect())
            .unwrap_or_default();

        self.absorb_lists(lists, &joined);

        // The pool-wide sweep of the group metadata and members.
        let urls: Vec<RelayUrl> = client.relays().await.into_keys().collect();
        let filter = Filter::new().kinds([Kind::GroupMetadata, Kind::GroupMembers]);
        client.sync(filter).with(urls).await.ok();

        // The previews cover the relays the lists just referenced.
        let fetches = self.refs.clone().into_iter().map(|(relay, ids)| {
            let client = client.clone();

            async move {
                client.add_relay(relay.clone()).and_connect().await.ok();

                let filter = Filter::new()
                    .kinds([Kind::GroupMetadata, Kind::GroupMembers])
                    .identifiers(ids.iter().map(GroupId::as_str));

                let urls = vec![relay];
                client.sync(filter).with(urls).await.ok();
            }
        });

        join_all(fetches).await;
    }
}
