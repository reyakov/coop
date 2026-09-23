use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::future::Future;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant};

use anyhow::{Result, bail};
use concord::cord01::{KIND_WRAP_EPHEMERAL, OpenedStream};
use concord::cord03::{self, ChatRumor};
use concord::derive::channel_group_key;
use concord::state::{ChannelCursor, HeldKey};
use concord::{ChannelId, GroupKey};
use futures::future::{Either, select};
use nostr_sdk::prelude::*;

use crate::cache::cache_rumor;
use crate::sync::connect_relays;

/// How long one relay is given to answer one page of history.
const PAGE_TIMEOUT: Duration = Duration::from_secs(10);
/// How far below a cursor a warm window reaches back.
pub const CURSOR_OVERLAP: Duration = Duration::from_secs(60);

/// The region of history to read.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Window {
    pub until: Option<Timestamp>,
    pub since: Option<Timestamp>,
}

impl Window {
    /// The newest wraps, with no older bound.
    pub fn newest() -> Self {
        Self::default()
    }

    /// The wraps strictly older than `oldest`.
    pub fn older_than(oldest: Timestamp) -> Self {
        Self {
            until: Some(oldest - 1u64),
            since: None,
        }
    }

    /// The region between `since` and `oldest`, both inclusive.
    pub fn between(since: Timestamp, oldest: Timestamp) -> Self {
        Self {
            until: Some(oldest - 1u64),
            since: Some(since),
        }
    }

    /// The window a channel is opened with.
    pub fn opening(cursor: ChannelCursor) -> Self {
        match cursor.newest {
            Some(newest) => Self {
                since: Some(newest - CURSOR_OVERLAP),
                until: None,
            },
            None => Self::default(),
        }
    }
}

/// What a relay said about one page subscription.
#[derive(Debug, Clone)]
pub enum Settled {
    Replayed,
    Refused(String),
}

/// One relay's verdict on one page subscription.
#[derive(Debug, Clone)]
pub struct PageReport {
    pub relay: RelayUrl,
    pub outcome: Settled,
}

/// The page subscriptions in flight, by subscription id.
pub struct PageRegistry {
    pages: Arc<Mutex<HashMap<SubscriptionId, flume::Sender<PageReport>>>>,
}

impl Default for PageRegistry {
    fn default() -> Self {
        Self {
            pages: Arc::new(Mutex::new(HashMap::new())),
        }
    }
}

impl Clone for PageRegistry {
    fn clone(&self) -> Self {
        Self {
            pages: Arc::clone(&self.pages),
        }
    }
}

impl PageRegistry {
    pub fn register(&self, id: SubscriptionId, sender: flume::Sender<PageReport>) {
        self.lock().insert(id, sender);
    }

    pub fn unregister(&self, id: &SubscriptionId) {
        self.lock().remove(id);
    }

    /// Forget every page still in flight, because its round is gone.
    pub fn clear(&self) {
        self.lock().clear();
    }

    /// Hand a relay's verdict to the page that owns `id`, when it is still waiting.
    pub fn deliver(&self, id: &SubscriptionId, relay: RelayUrl, outcome: Settled) {
        let sender = self.lock().get(id).cloned();

        let Some(sender) = sender else {
            return;
        };

        if let Err(error) = sender.try_send(PageReport { relay, outcome }) {
            log::debug!("community: a page report was not delivered: {error}");
        }
    }

    fn lock(&self) -> MutexGuard<'_, HashMap<SubscriptionId, flume::Sender<PageReport>>> {
        match self.pages.lock() {
            Ok(pages) => pages,
            Err(poisoned) => poisoned.into_inner(),
        }
    }
}

/// What one paged fetch saw.
#[derive(Debug, Clone, Default)]
pub struct WrapPage {
    pub opened: Vec<ChatRumor>,
    pub raw: usize,
    /// Wraps that reached us under a held plane but that no held key could open.
    pub unreadable: usize,
    pub newest: Option<Timestamp>,
    pub oldest: Option<Timestamp>,
    pub exhausted: bool,
    pub failed: bool,
    pub errors: usize,
}

/// Walks a channel's history back over the community's own relays.
#[allow(clippy::too_many_arguments)]
pub async fn page(
    client: &Client,
    pages: &PageRegistry,
    channel: &ChannelId,
    held: &[HeldKey],
    relays: &[RelayUrl],
    window: Window,
    max_pages: usize,
    limit: usize,
) -> Result<WrapPage> {
    let planes: Vec<(HeldKey, GroupKey)> = held
        .iter()
        .map(|key| Ok((*key, channel_group_key(&key.key, channel, key.epoch)?)))
        .collect::<Result<Vec<_>>>()?;
    let authors: Vec<PublicKey> = planes.iter().map(|(_, group)| group.pk()).collect();

    if authors.is_empty() || relays.is_empty() || limit == 0 {
        return Ok(WrapPage {
            failed: true,
            ..WrapPage::default()
        });
    }

    // A REQ can only target a relay the pool already knows about.
    connect_relays(client, relays).await;

    let mut walk = Walk::new(relays, window);
    let mut opened = Vec::new();

    for _ in 0..max_pages {
        if walk.is_done() {
            break;
        }

        let filter = wrap_filter(&authors, walk.region(), limit);
        let asked: Vec<(usize, RelayUrl)> = walk
            .live()
            .map(|index| (index, walk.url(index).clone()))
            .collect();

        let answers = ask_page(client, pages, &asked, &filter).await;

        for (index, url) in &asked {
            match answers.get(url) {
                Some(Settled::Replayed) => {}
                Some(Settled::Refused(reason)) => {
                    log::warn!("community: relay {url} refused a history page: {reason}");
                    walk.reject(*index);
                }
                None => {
                    log::warn!("community: relay {url} did not finish a history page");
                    walk.reject(*index);
                }
            }
        }

        let answered = client.database().query(filter).await?;

        for wrap in walk.accept(answered, limit) {
            let Some((held, group)) = planes.iter().find(|(_, group)| group.pk() == wrap.pubkey)
            else {
                continue;
            };

            let Ok((stream, rumor)) = read_under(&wrap, held, group, channel) else {
                walk.unreadable += 1;
                continue;
            };

            if cache_rumor(client, channel, &stream).await? {
                opened.push(rumor);
            }
        }
    }

    Ok(walk.finish(opened))
}

/// Opens one wrap under a held key.
fn read_under(
    wrap: &Event,
    held: &HeldKey,
    group: &GroupKey,
    channel: &ChannelId,
) -> Result<(OpenedStream, ChatRumor)> {
    if held
        .retired_at
        .is_some_and(|retired| wrap.created_at > retired)
    {
        bail!("sealed after the key that reads it was retired");
    }

    Ok(cord03::open(wrap, group, channel, held.epoch)?)
}

/// The one filter a page is asked for.
fn wrap_filter(authors: &[PublicKey], window: Window, limit: usize) -> Filter {
    let mut filter = Filter::new()
        .kinds([Kind::GiftWrap, Kind::Custom(KIND_WRAP_EPHEMERAL)])
        .authors(authors.iter().copied())
        .limit(limit);

    if let Some(until) = window.until {
        filter = filter.until(until);
    }

    if let Some(since) = window.since {
        filter = filter.since(since);
    }

    filter
}

/// One page's verdicts, by relay.
type PageAnswers = BTreeMap<RelayUrl, Settled>;

async fn ask_page(
    client: &Client,
    pages: &PageRegistry,
    asked: &[(usize, RelayUrl)],
    filter: &Filter,
) -> PageAnswers {
    let mut answers = PageAnswers::new();
    let distinct: BTreeSet<RelayUrl> = asked.iter().map(|(_, url)| url.clone()).collect();
    let mut targets: Vec<(RelayUrl, Vec<Filter>)> = Vec::with_capacity(distinct.len());

    for url in &distinct {
        match client.relay(url).await {
            Ok(Some(_)) => targets.push((url.clone(), vec![filter.clone()])),
            Ok(None) => {
                log::warn!("community: relay {url} is not in the pool for a history page");
                answers.insert(
                    url.clone(),
                    Settled::Refused("not in the relay pool".to_owned()),
                );
            }
            Err(error) => {
                log::warn!("community: relay {url} could not be looked up: {error}");
                answers.insert(url.clone(), Settled::Refused(error.to_string()));
            }
        }
    }

    if targets.is_empty() {
        return answers;
    }

    let id = history_subscription();
    let (sender, receiver) = flume::bounded(distinct.len());
    pages.register(id.clone(), sender);

    let options = SubscribeAutoCloseOptions::default()
        .exit_policy(ReqExitPolicy::ExitOnEOSE)
        .timeout(Some(PAGE_TIMEOUT));

    match client
        .subscribe(ReqTarget::manual(targets))
        .with_id(id.clone())
        .close_on(options)
        .await
    {
        Ok(output) => {
            for (url, reason) in output.failed {
                answers.insert(url, Settled::Refused(reason));
            }

            let deadline = Instant::now() + PAGE_TIMEOUT;

            while answers.len() < distinct.len() {
                let remaining = deadline.saturating_duration_since(Instant::now());

                let Some(report) = within(remaining, receiver.recv_async()).await else {
                    break;
                };

                match report {
                    Ok(report) => {
                        answers.insert(report.relay, report.outcome);
                    }
                    Err(error) => {
                        log::warn!("community: a history page's reports were lost: {error}");
                        break;
                    }
                }
            }
        }
        Err(error) => {
            log::warn!("community: a history page REQ was refused: {error}");
        }
    }

    pages.unregister(&id);

    answers
}

pub(crate) fn auth_required(reason: &str) -> bool {
    matches!(
        MachineReadablePrefix::parse(reason),
        Some(MachineReadablePrefix::AuthRequired)
    )
}

fn history_subscription() -> SubscriptionId {
    static NEXT: AtomicU64 = AtomicU64::new(0);
    SubscriptionId::new(format!("history-{}", NEXT.fetch_add(1, Ordering::Relaxed)))
}

async fn within<F>(limit: Duration, future: F) -> Option<F::Output>
where
    F: Future,
{
    let future = std::pin::pin!(future);
    let deadline = std::pin::pin!(smol::Timer::after(limit));

    match select(future, deadline).await {
        Either::Left((output, _)) => Some(output),
        Either::Right(_) => None,
    }
}

/// One pass over a channel's history, page by page.
#[derive(Debug)]
struct Walk {
    relays: Vec<Walker>,
    since: Option<Timestamp>,
    /// The inclusive upper bound of the next page.
    cursor: Option<Timestamp>,
    seen: BTreeSet<EventId>,
    newest: Option<Timestamp>,
    oldest: Option<Timestamp>,
    raw: usize,
    errors: usize,
    /// Wraps the caller could not read under any held key.
    unreadable: usize,
    /// A short page ended the walk.
    bottom: bool,
}

/// One relay's standing in a walk.
#[derive(Debug)]
struct Walker {
    url: RelayUrl,
    dead: bool,
}

impl Walk {
    fn new(relays: &[RelayUrl], window: Window) -> Self {
        Self {
            relays: relays
                .iter()
                .cloned()
                .map(|url| Walker { url, dead: false })
                .collect(),
            since: window.since,
            cursor: window.until,
            seen: BTreeSet::new(),
            newest: None,
            oldest: None,
            raw: 0,
            errors: 0,
            unreadable: 0,
            bottom: false,
        }
    }

    fn is_done(&self) -> bool {
        self.bottom || self.relays.iter().all(|walker| walker.dead)
    }

    /// The region the next page asks for.
    fn region(&self) -> Window {
        Window {
            until: self.cursor,
            since: self.since,
        }
    }

    fn live(&self) -> impl Iterator<Item = usize> + '_ {
        (0..self.relays.len()).filter(|&index| !self.relays[index].dead)
    }

    fn url(&self, index: usize) -> &RelayUrl {
        &self.relays[index].url
    }

    fn reject(&mut self, index: usize) {
        self.relays[index].dead = true;
        self.errors += 1;
    }

    fn accept(&mut self, page: BTreeSet<Event>, limit: usize) -> Vec<Event> {
        if page.len() < limit {
            self.bottom = true;
        }

        let mut oldest: Option<Timestamp> = None;
        let mut events = Vec::with_capacity(page.len());

        for event in page {
            let at = event.created_at;
            self.newest = Some(self.newest.map_or(at, |newest| newest.max(at)));
            oldest = Some(oldest.map_or(at, |oldest| oldest.min(at)));

            if self.seen.insert(event.id) {
                self.raw += 1;
                events.push(event);
            }
        }

        // The walk's floor, which the round resumes the older pass from.
        if let Some(oldest) = oldest {
            self.oldest = Some(self.oldest.map_or(oldest, |held| held.min(oldest)));
        }

        match oldest {
            Some(oldest) if !oldest.is_zero() => self.cursor = Some(oldest - 1u64),
            Some(_) => self.bottom = true,
            None => {}
        }

        events
    }

    fn finish(self, opened: Vec<ChatRumor>) -> WrapPage {
        let swept = self.bottom && self.errors == 0;

        WrapPage {
            opened,
            raw: self.raw,
            unreadable: self.unreadable,
            newest: self.newest,
            oldest: self.oldest,
            exhausted: swept && self.raw > 0,
            failed: self.errors > 0 || (self.bottom && self.raw == 0),
            errors: self.errors,
        }
    }
}

#[cfg(test)]
mod tests {
    use std::cmp::Reverse;

    use concord::Epoch;
    use concord::cord03::{build_message, seal_rumor};
    use concord::derive::channel_group_key;

    use super::*;

    const SECRET: [u8; 32] = [0x07u8; 32];
    const NEXT_SECRET: [u8; 32] = [0x11u8; 32];

    fn serve_page(database: &BTreeSet<Event>, window: Window, limit: usize) -> BTreeSet<Event> {
        let mut events: Vec<Event> = database
            .iter()
            .filter(|event| {
                window.until.is_none_or(|until| event.created_at <= until)
                    && window.since.is_none_or(|since| event.created_at >= since)
            })
            .cloned()
            .collect();

        events.sort_by_key(|event| Reverse(event.created_at));
        events.truncate(limit);
        events.into_iter().collect()
    }

    fn relay_url(host: &str) -> RelayUrl {
        RelayUrl::parse(&format!("wss://{host}.example.com")).expect("parses")
    }

    #[test]
    fn a_walk_pages_back_across_a_rekey() {
        let channel = ChannelId::from_bytes([0x9cu8; 32]);
        let author = Keys::generate();
        let held = [
            HeldKey {
                epoch: Epoch(0),
                key: SECRET,
                retired_at: None,
            },
            HeldKey {
                epoch: Epoch(1),
                key: NEXT_SECRET,
                retired_at: None,
            },
        ];
        let planes: Vec<(HeldKey, GroupKey)> = held
            .iter()
            .map(|key| {
                (
                    *key,
                    channel_group_key(&key.key, &channel, key.epoch).expect("derives"),
                )
            })
            .collect();

        // Three messages a second apart: a page boundary falls between each.
        let base = 1_700_000_000_000;
        let mut relay: BTreeSet<Event> = BTreeSet::new();

        for (content, secret, epoch, at_ms) in [
            ("before the rekey", &SECRET, Epoch(0), base),
            ("still before", &SECRET, Epoch(0), base + 1_000),
            ("after the rekey", &NEXT_SECRET, Epoch(1), base + 2_000),
        ] {
            let group = channel_group_key(secret, &channel, epoch).expect("derives");
            let rumor = build_message(
                author.public_key(),
                &channel,
                epoch,
                content,
                None,
                at_ms,
                None,
            );
            relay.insert(
                smol::block_on(seal_rumor(&rumor, &group, &author, false))
                    .expect("seals")
                    .0,
            );
        }

        let mut walk = Walk::new(&[relay_url("history")], Window::newest());
        let mut found = Vec::new();
        let mut pages = 0;

        while !walk.is_done() && pages < 10 {
            pages += 1;
            let page = serve_page(&relay, walk.region(), 2);

            for wrap in walk.accept(page, 2) {
                let Some((held, group)) =
                    planes.iter().find(|(_, group)| group.pk() == wrap.pubkey)
                else {
                    continue;
                };

                let (_, rumor) = cord03::open(&wrap, group, &channel, held.epoch).expect("opens");
                found.push(rumor);
            }
        }

        found.sort_by_key(|rumor| (Reverse(rumor.at_ms), rumor.id));

        let contents: Vec<&str> = found.iter().map(|rumor| rumor.content.as_str()).collect();
        assert_eq!(
            contents,
            ["after the rekey", "still before", "before the rekey"]
        );

        let page = walk.finish(Vec::new());
        assert!(page.exhausted);
        assert!(!page.failed);
        assert_eq!(page.raw, 3);
        assert_eq!(page.newest, Some(Timestamp::from_secs(1_700_000_002)));
        assert_eq!(
            page.oldest,
            Some(Timestamp::from_secs(1_700_000_000)),
            "the walk reports its floor, which the older pass resumes below"
        );
    }

    /// A wrap that reaches us and still will not open is history we cannot read,
    /// not history that does not exist.
    #[test]
    fn a_wrap_no_held_key_can_open_reads_as_unreadable() {
        let channel = ChannelId::from_bytes([0x9cu8; 32]);
        let other = ChannelId::from_bytes([0x9du8; 32]);
        let author = Keys::generate();
        let group = channel_group_key(&SECRET, &channel, Epoch(0)).expect("derives");
        let held = HeldKey {
            epoch: Epoch(0),
            key: SECRET,
            retired_at: Some(Timestamp::from_secs(1_000)),
        };

        let wrap_at = |channel: &ChannelId, at_ms: u64| {
            let rumor = build_message(
                author.public_key(),
                channel,
                Epoch(0),
                "sealed",
                None,
                at_ms,
                None,
            );
            smol::block_on(seal_rumor(&rumor, &group, &author, false))
                .expect("seals")
                .0
        };

        // Sealed before the rotation superseded this key, so it still reads.
        let before = wrap_at(&channel, 999_000);
        assert!(read_under(&before, &held, &group, &channel).is_ok());

        // Sealed after the cutoff the rotation set on that key.
        let after = wrap_at(&channel, 1_001_000);
        assert!(read_under(&after, &held, &group, &channel).is_err());

        // Sealed to this plane but bound to another channel.
        let misbound = wrap_at(&other, 999_000);
        assert!(read_under(&misbound, &held, &group, &channel).is_err());
    }

    #[test]
    fn an_empty_answer_never_seals_the_channel() {
        let database: BTreeSet<Event> = BTreeSet::new();
        let mut walk = Walk::new(&[relay_url("history")], Window::newest());

        let page = serve_page(&database, walk.region(), 50);
        assert!(walk.accept(page, 50).is_empty());

        let page = walk.finish(Vec::new());
        assert!(page.failed);
        assert!(!page.exhausted);
        assert_eq!(page.raw, 0);
        assert_eq!(page.oldest, None);
    }

    #[test]
    fn a_silent_relay_blocks_the_bottom() {
        let database: BTreeSet<Event> = BTreeSet::new();
        let mut walk = Walk::new(
            &[relay_url("history"), relay_url("archive")],
            Window::newest(),
        );

        // One relay answered the empty page; the other never answered at all, so
        // its share of the region was never read and the walk must not seal.
        walk.reject(1);
        let page = serve_page(&database, walk.region(), 50);
        assert!(walk.accept(page, 50).is_empty());

        let page = walk.finish(Vec::new());
        assert!(page.failed);
        assert!(!page.exhausted);
        assert_eq!(page.errors, 1);
    }

    #[test]
    fn a_page_boundary_is_exclusive() {
        let database = BTreeSet::from([
            event_at(Timestamp::from_secs(1_700_000_000)),
            event_at(Timestamp::from_secs(1_700_000_001)),
        ]);
        let mut walk = Walk::new(&[relay_url("history")], Window::newest());

        let first = walk.accept(serve_page(&database, walk.region(), 1), 1);
        let second = walk.accept(serve_page(&database, walk.region(), 1), 1);

        assert_eq!(first.len(), 1);
        assert_eq!(second.len(), 1);
        assert_ne!(first[0].id, second[0].id);

        let oldest = second
            .iter()
            .map(|event| event.created_at)
            .min()
            .expect("one wrap");
        assert_eq!(oldest, Timestamp::from_secs(1_700_000_000));
    }

    fn event_at(at: Timestamp) -> Event {
        let keys = Keys::generate();
        EventBuilder::new(Kind::TextNote, "page")
            .custom_created_at(at)
            .finalize(&keys)
            .expect("signs")
    }

    /// A cold channel asks wide; a warm one resumes at the overlap above its
    /// cursor, and `older_than` never includes the boundary event itself.
    #[test]
    fn a_cold_window_is_open_and_a_warm_one_resumes_at_the_overlap() {
        assert_eq!(Window::opening(ChannelCursor::default()), Window::default());

        let warm = Window::opening(ChannelCursor {
            newest: Some(Timestamp::from_secs(2_000_000)),
            oldest: Some(Timestamp::from_secs(1_000)),
            exhausted: false,
        });
        assert_eq!(
            warm,
            Window {
                since: Some(Timestamp::from_secs(2_000_000) - CURSOR_OVERLAP),
                until: None,
            }
        );

        assert_eq!(
            Window::older_than(Timestamp::from_secs(1_000)),
            Window {
                since: None,
                until: Some(Timestamp::from_secs(999)),
            }
        );
    }

    /// A page whose round has moved on is simply a report nobody reads.
    #[test]
    fn a_page_that_moved_on_receives_nothing() {
        let pages = PageRegistry::default();
        let id = SubscriptionId::new("concord-history-9");
        let (sender, receiver) = flume::bounded(1);

        pages.register(id.clone(), sender);
        pages.unregister(&id);
        pages.deliver(&id, relay_url("history"), Settled::Replayed);

        assert!(receiver.try_recv().is_err());
    }
}
