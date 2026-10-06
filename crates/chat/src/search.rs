use std::cmp::Reverse;
use std::collections::BTreeMap;
use std::sync::Arc;

use common::EventExt;
use nostr_sdk::prelude::*;

/// A chat message kept in memory for content search.
#[derive(Debug, Clone)]
pub struct SearchMessage {
    /// ID of the room the message belongs to
    pub room: u64,
    /// Author of the message
    pub author: PublicKey,
    /// The content of the message
    pub content: Arc<str>,
    /// When the message was created
    pub created_at: Timestamp,
}

#[derive(Debug)]
struct IndexedMessage {
    message: SearchMessage,
    /// Lowercase content so repeated searches don't re-allocate per message
    normalized: Arc<str>,
}

/// In-memory index of chat message rumors, keyed by room and rumor id.
#[derive(Debug, Default)]
pub struct MessageSearchIndex(BTreeMap<u64, BTreeMap<EventId, Arc<IndexedMessage>>>);

impl MessageSearchIndex {
    /// Index a rumor for search, ignoring reactions and empty content.
    pub fn insert(&mut self, rumor: &UnsignedEvent) {
        if !is_chat(rumor.kind) || rumor.content.trim().is_empty() {
            return;
        }

        let Some(id) = rumor.id else { return };

        let indexed = Arc::new(IndexedMessage {
            message: SearchMessage {
                room: rumor.uniq_id(),
                author: rumor.pubkey,
                content: rumor.content.as_str().into(),
                created_at: rumor.created_at,
            },
            normalized: rumor.content.to_lowercase().into(),
        });

        self.0
            .entry(rumor.uniq_id())
            .or_default()
            .entry(id)
            .or_insert(indexed);
    }

    /// Find messages whose content contains all of the terms, newest first.
    pub fn search(&self, terms: &[String], limit: usize) -> Vec<SearchMessage> {
        let mut matches: Vec<&Arc<IndexedMessage>> = self
            .0
            .values()
            .flat_map(|messages| messages.values())
            .filter(|indexed| terms.iter().all(|term| indexed.normalized.contains(term)))
            .collect();

        matches.sort_by_key(|indexed| Reverse(indexed.message.created_at));
        matches.truncate(limit);

        matches
            .into_iter()
            .map(|indexed| indexed.message.clone())
            .collect()
    }

    /// Drop all indexed messages.
    pub fn clear(&mut self) {
        self.0.clear();
    }
}

fn is_chat(kind: Kind) -> bool {
    kind == Kind::PrivateDirectMessage || kind == Kind::Custom(15)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rumor(kind: Kind, content: &str, created_at: u64) -> UnsignedEvent {
        let sender = Keys::generate();
        let peer = Keys::generate().public_key();

        let mut rumor = EventBuilder::new(kind, content)
            .tag(Tag::public_key(peer))
            .finalize_unsigned(sender.public_key());
        rumor.ensure_id();
        rumor.created_at = Timestamp::from(created_at);
        rumor
    }

    #[test]
    fn search_matches_all_terms_newest_first_and_deduplicates() {
        let mut index = MessageSearchIndex::default();
        let first = rumor(Kind::PrivateDirectMessage, "Hello ALICE", 10);
        let second = rumor(Kind::PrivateDirectMessage, "hello again", 20);
        let reaction = rumor(Kind::Reaction, "hello world", 30);

        index.insert(&first);
        index.insert(&first);
        index.insert(&second);
        index.insert(&reaction);

        let hits = index.search(&["hello".to_owned()], 10);
        assert_eq!(hits.len(), 2);
        assert_eq!(&*hits[0].content, "hello again");
        assert_eq!(hits[0].created_at, Timestamp::from(20));

        let hits = index.search(&["hello".to_owned(), "again".to_owned()], 10);
        assert_eq!(hits.len(), 1);

        let hits = index.search(&["hello".to_owned()], 1);
        assert_eq!(hits.len(), 1);
    }

    #[test]
    fn empty_content_is_not_indexed() {
        let mut index = MessageSearchIndex::default();
        index.insert(&rumor(Kind::PrivateDirectMessage, "   ", 10));

        assert!(index.search(&["hello".to_owned()], 10).is_empty());
    }
}
