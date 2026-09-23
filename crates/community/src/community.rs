use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::PathBuf;
use std::time::{Duration, Instant};

use anyhow::Result;
use concord::cord02::{ControlFold, ImageRef};
use concord::cord03::{self, ChatMessage, ReplyRef};
use concord::cord04::AuthorityCitation;
use concord::cord04::roles::{Permissions, citation_ok};
use concord::cord06::RekeyScope;
use concord::derive::{channel_group_key, grant_locator};
use concord::state::{ChannelCursor, ChannelKeyRef, CommunityState, HeldKey, HeldRoot};
use concord::{ChannelId, CommunityId, Epoch};
use gpui::{App, AppContext, Context, EventEmitter, Task};
use nostr_sdk::prelude::*;
use smallvec::{SmallVec, smallvec};
use state::NostrRegistry;

use crate::cache;
use crate::history::{self, PageRegistry, Window, WrapPage};
use crate::rekey::{self, Adoptions};
use crate::sync::{self, Snapshot};

/// Wraps one relay returns for one page request.
const PAGE_WRAPS: usize = 50;
/// Pages a catch-up round walks down a channel's history on its own.
const CATCH_UP_PAGES: usize = 20;
/// Pages one explicit "load older" fetches from the relays.
pub const LOAD_OLDER_PAGES: usize = 6;
/// Rows one timeline read returns before the caller asks for more.
pub const TIMELINE_PAGE: usize = 100;
/// Side events read per row, so a reaction flood cannot displace the rows it decorates.
const SIDE_EVENT_FACTOR: usize = 4;
/// The shortest gap between two automatic catch-up rounds for one channel.
pub const MIN_ROUND_INTERVAL: Duration = Duration::from_secs(30);
/// How long a channel may go unsynced before the scheduler repairs it.
const STALE_AFTER: Duration = Duration::from_secs(300);

/// Which direction a sync round reads.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Intent {
    CatchUp,
    Older { pages: usize },
}

/// What one round saw, for the caller to report and act on.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Progress {
    /// Wraps the round reached that no key this client holds can open.
    pub unreadable: usize,
    /// A relay refused or never answered a page.
    pub failed: bool,
    /// How many relays failed the round.
    pub errors: usize,
}

/// A channel's timeline, folded from the local cache.
#[derive(Debug, Clone, Default)]
pub struct Timeline {
    /// Oldest first, ready for a bottom-aligned list.
    pub messages: Vec<ChatMessage>,
    /// The cache holds rows older than `messages`.
    pub has_more: bool,
}

/// One channel's round in flight, coalescing requests that arrive while it runs.
#[derive(Default)]
struct Round {
    running: bool,
    /// The next round to run once this one lands.
    queued: Option<Intent>,
    /// Everyone waiting on the outcome.
    waiters: Vec<flume::Sender<Result<Progress, String>>>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SubscriptionKey {
    control_pks: BTreeMap<u64, PublicKey>,
    channels: Vec<(ChannelId, Epoch, bool)>,
    roots: Vec<(u64, [u8; 32])>,
    relays: Vec<RelayUrl>,
}

impl SubscriptionKey {
    fn of(state: &CommunityState) -> Self {
        Self {
            control_pks: state.control_pks.clone(),
            channels: state
                .channels
                .iter()
                .map(|channel| (channel.id, channel.epoch, channel.private))
                .collect(),
            roots: state
                .roots()
                .into_iter()
                .map(|root| (root.epoch.0, root.key))
                .collect(),
            relays: state.relays.clone(),
        }
    }

    pub(crate) fn relays(&self) -> &[RelayUrl] {
        &self.relays
    }
}

#[derive(Debug, Clone)]
pub enum CommunityEvent {
    Updated(CommunityId),
    Open(CommunityId),
    Close(CommunityId),
    Channel(CommunityId, ChannelId),
    /// History exists here that no held key can open.
    Unreadable(CommunityId),
    /// The last round could not reach the community's relays.
    Failed(CommunityId),
    Error(String),
}

pub struct Community {
    state: CommunityState,
    control: ControlFold,
    members: BTreeSet<PublicKey>,
    active: Option<ChannelId>,
    icon: Option<PathBuf>,
    icon_ref: Option<ImageRef>,
    banner: Option<PathBuf>,
    banner_ref: Option<ImageRef>,
    dirty: bool,
    refresh_task: Option<Task<Result<()>>>,
    icon_task: Option<Task<Result<()>>>,
    banner_task: Option<Task<Result<()>>>,
    rounds: HashMap<ChannelId, Round>,
    /// The last completed round per channel, for the panel's honest states.
    progress: HashMap<ChannelId, Progress>,
    /// Wraps held per channel that no key we hold can open.
    unreadable: BTreeMap<ChannelId, usize>,
    /// When each channel's last round started, which paces the automatic ones.
    last_round: HashMap<ChannelId, Instant>,
    pages: PageRegistry,
    rekey_task: Option<Task<Result<()>>>,
    rekey_dirty: bool,
    /// Spawned folds, round bookkeeping and publishes, cancelled on drop
    tasks: SmallVec<[Task<Result<()>>; 2]>,
}

impl EventEmitter<CommunityEvent> for Community {}

impl Community {
    pub fn new(state: CommunityState, pages: PageRegistry) -> Self {
        Self {
            state,
            control: ControlFold::default(),
            members: BTreeSet::new(),
            active: None,
            icon: None,
            icon_ref: None,
            banner: None,
            banner_ref: None,
            dirty: false,
            refresh_task: None,
            icon_task: None,
            banner_task: None,
            rounds: HashMap::new(),
            progress: HashMap::new(),
            unreadable: BTreeMap::new(),
            last_round: HashMap::new(),
            pages,
            rekey_task: None,
            rekey_dirty: false,
            tasks: smallvec![],
        }
    }

    pub fn id(&self) -> CommunityId {
        self.state.id
    }

    pub fn state(&self) -> &CommunityState {
        &self.state
    }

    pub fn name(&self) -> String {
        if let Some(metadata) = &self.control.community {
            return metadata.name.clone();
        }

        self.state
            .name
            .clone()
            .unwrap_or_else(|| self.state.id.to_hex())
    }

    pub fn control(&self) -> &ControlFold {
        &self.control
    }

    /// The community's icon, once downloaded and decrypted into a cache file.
    pub fn icon(&self) -> Option<PathBuf> {
        self.icon.clone()
    }

    /// The community's banner, once downloaded and decrypted into a cache file.
    pub fn banner(&self) -> Option<PathBuf> {
        self.banner.clone()
    }

    /// The channel the sidebar and panel show, defaulting to the first one.
    pub fn active_channel(&self) -> Option<ChannelId> {
        self.active
            .or_else(|| self.state.channels.first().map(|channel| channel.id))
    }

    /// Mark `channel` as the one the sidebar and panel show.
    pub fn set_active_channel(&mut self, channel: ChannelId, cx: &mut Context<Self>) {
        if self.active == Some(channel) {
            return;
        }

        self.active = Some(channel);
        cx.emit(CommunityEvent::Channel(self.state.id, channel));
    }

    pub fn members(&self) -> &BTreeSet<PublicKey> {
        &self.members
    }

    /// The base epoch a rotation excluded us at: readable history, no writes.
    pub fn removed_at(&self) -> Option<Epoch> {
        self.state.removed_at
    }

    /// A complete rotation ahead of our epoch predates our join and
    /// carries no blob for us, so the invite landed us on a superseded epoch.
    pub fn stranded(&self) -> bool {
        self.state.stranded
    }

    /// Wraps held here that no key we hold can open.
    pub fn unreadable(&self, channel: &ChannelId) -> usize {
        self.unreadable.get(channel).copied().unwrap_or(0)
    }

    /// The last round's outcome for `channel`, when one has run.
    pub fn progress(&self, channel: &ChannelId) -> Option<Progress> {
        self.progress.get(channel).copied()
    }

    /// The epoch a private channel's key is missing for, when we hold none.
    pub fn missing_key(&self, channel: &ChannelId) -> Option<Epoch> {
        self.state
            .channels
            .iter()
            .find(|held| held.id == *channel && held.private && held.key.is_none())
            .map(|held| held.epoch)
    }

    /// The epoch a channel rotation removed us at, when it removed us.
    pub fn channel_removed_at(&self, channel: &ChannelId) -> Option<Epoch> {
        self.state.channel_cuts.get(channel).copied()
    }

    /// Whether an automatic catch-up for `channel` is worth asking for yet.
    pub fn due(&self, channel: &ChannelId) -> bool {
        self.last_round
            .get(channel)
            .is_none_or(|at| at.elapsed() >= MIN_ROUND_INTERVAL)
    }

    /// Whether `channel` has been synced before and has since gone stale.
    fn stale(&self, channel: &ChannelId) -> bool {
        self.last_round
            .get(channel)
            .is_some_and(|at| at.elapsed() >= STALE_AFTER)
    }

    pub fn channels(&self) -> &[ChannelKeyRef] {
        &self.state.channels
    }

    pub fn subscription_key(&self) -> SubscriptionKey {
        SubscriptionKey::of(&self.state)
    }

    /// A public channel derives its write plane from the community root.
    fn channel_secret(&self, channel: &ChannelId) -> Option<(Epoch, [u8; 32])> {
        if self.state.removed_at.is_some() || self.state.stranded {
            return None;
        }

        let held = self
            .state
            .channels
            .iter()
            .find(|held| held.id == *channel)?;

        if self.state.channel_cut(channel, held.epoch) {
            return None;
        }

        held.current().or_else(|| {
            (!held.private).then_some((self.state.root_epoch, self.state.community_root))
        })
    }

    /// Every secret the client holds for a channel, newest epoch first.
    fn held_keys(&self, channel: &ChannelId) -> Vec<HeldKey> {
        self.state.held_keys(channel)
    }

    pub fn sync_channel(
        &mut self,
        channel: &ChannelId,
        intent: Intent,
        cx: &mut Context<Self>,
    ) -> Task<Result<Progress>> {
        let channel = *channel;
        let (sender, receiver) = flume::bounded(1);

        let start = {
            let round = self.rounds.entry(channel).or_default();
            round.waiters.push(sender);

            let start = !round.running;

            if !start {
                round.queued = Some(intent);
            }

            start
        };

        if start {
            self.start_round(channel, intent, cx);
        }

        cx.background_spawn(async move {
            let progress = match receiver.recv_async().await {
                Ok(progress) => progress,
                Err(error) => {
                    log::warn!("community: channel round cancelled: {error}");
                    return Ok(Progress {
                        failed: true,
                        ..Progress::default()
                    });
                }
            };
            progress.map_err(anyhow::Error::msg)
        })
    }

    fn start_round(&mut self, channel: ChannelId, intent: Intent, cx: &mut Context<Self>) {
        let client = NostrRegistry::global(cx).read(cx).client();
        let held = self.held_keys(&channel);
        let relays = self.state.relays.clone();
        let pages = self.pages.clone();

        let saved = clamped(
            self.state
                .cursors
                .get(&channel)
                .copied()
                .unwrap_or_default(),
            Timestamp::now(),
        );

        if let Some(round) = self.rounds.get_mut(&channel) {
            round.running = true;
            round.queued = None;
        }

        self.last_round.insert(channel, Instant::now());

        let round = cx.background_spawn(async move {
            sync_round(&client, &pages, &channel, &held, &relays, saved, intent).await
        });

        let finisher = cx.spawn(async move |this, cx| {
            let result = round.await;

            if let Err(error) = this.update(cx, |this, cx| this.finish_round(channel, result, cx)) {
                log::warn!("community: a channel round outlived its community: {error}");
            }

            Ok(())
        });

        self.tasks.push(finisher);
    }

    fn finish_round(
        &mut self,
        channel: ChannelId,
        result: Result<RoundOutcome>,
        cx: &mut Context<Self>,
    ) {
        let outcome = match result {
            Ok((progress, cursor)) => {
                self.merge_cursor(channel, cursor, cx);
                Ok(progress)
            }
            Err(error) => Err(error.to_string()),
        };

        let Some(round) = self.rounds.get_mut(&channel) else {
            return;
        };

        round.running = false;
        let queued = round.queued.take();
        let waiters = std::mem::take(&mut round.waiters);

        for waiter in waiters {
            if let Err(error) = waiter.try_send(outcome.clone()) {
                log::warn!("community: a channel round result was not delivered: {error}");
            }
        }

        if let Some(intent) = queued {
            self.start_round(channel, intent, cx);
        }

        if let Ok(progress) = &outcome {
            self.progress.insert(channel, *progress);
            self.record_unreadable(channel, progress.unreadable);

            if progress.failed && progress.errors > 0 {
                cx.emit(CommunityEvent::Failed(self.state.id));
            }

            if progress.unreadable > 0 {
                cx.emit(CommunityEvent::Unreadable(self.state.id));
            }
        }

        cx.emit(CommunityEvent::Updated(self.state.id));
    }

    /// Remember the wraps no held key could open.
    fn record_unreadable(&mut self, channel: ChannelId, count: usize) {
        if count == 0 {
            return;
        }

        let seen = self.unreadable.entry(channel).or_default();
        *seen = (*seen).max(count);
    }

    fn missing_authority(&self) -> bool {
        self.state.refounders.is_empty() && self.state.roots().len() > 1
    }

    pub(crate) fn tick(&mut self, cx: &mut Context<Self>) {
        if self.missing_authority() {
            self.rekey(cx);
        }

        let Some(channel) = self.active_channel() else {
            return;
        };

        if !self.stale(&channel) {
            return;
        }

        self.refresh(cx);

        let catch_up = self.sync_channel(&channel, Intent::CatchUp, cx);

        let task = cx.spawn(async move |_this, _cx| {
            if let Err(error) = catch_up.await {
                log::warn!("community: the scheduler's catch-up failed: {error}");
            }

            Ok(())
        });

        self.tasks.push(task);
    }

    /// Fold a round's cursor findings in, monotonically, and persist them.
    fn merge_cursor(&mut self, channel: ChannelId, cursor: ChannelCursor, cx: &mut Context<Self>) {
        let held = self
            .state
            .cursors
            .get(&channel)
            .copied()
            .unwrap_or_default();

        let merged = clamped(held.merge(cursor), Timestamp::now());

        if merged == held {
            return;
        }

        self.state.cursors.insert(channel, merged);
        self.persist(cx);
    }

    /// Write the local state document out.
    fn persist(&mut self, cx: &Context<Self>) {
        let client = NostrRegistry::global(cx).read(cx).client();
        let state = self.state.clone();

        let task = cx.background_spawn(async move {
            if let Err(error) = cache::save_state(&client, &state).await {
                log::warn!("community: failed to persist the local state: {error}");
            }

            Ok(())
        });

        self.tasks.push(task);
    }

    /// The channel's timeline, folded from the local cache, oldest first.
    pub fn timeline(
        &self,
        channel: &ChannelId,
        before_ms: Option<u64>,
        limit: usize,
        cx: &App,
    ) -> Task<Result<Timeline>> {
        let client = NostrRegistry::global(cx).read(cx).client();
        let channel = *channel;
        let owner = self.state.owner;
        let community_id = self.state.id;
        let floors = self.state.floors();
        let roles = self.control.roles.clone();

        cx.background_spawn(async move {
            let until = before_ms.map(|before_ms| Timestamp::from_secs(before_ms / 1000));

            let rows = cache::query_rumors(
                &client,
                &channel,
                until,
                limit.saturating_add(1),
                Some(&cord03::ROW_KINDS),
            )
            .await?;

            let has_more = rows.len() > limit;

            let sides = cache::query_rumors(
                &client,
                &channel,
                until,
                limit.saturating_mul(SIDE_EVENT_FACTOR),
                Some(&cord03::SIDE_KINDS),
            )
            .await?;

            let mut rumors = Vec::with_capacity(rows.len() + sides.len());

            for rumor in rows.iter().take(limit).chain(sides.iter()) {
                match cord03::parse_rumor(rumor) {
                    Ok(chat) => rumors.push(chat),
                    Err(error) => {
                        log::warn!("community: skipping an unreadable cached rumor: {error}")
                    }
                }
            }

            let mut messages =
                cord03::fold(&rumors, Timestamp::now(), |actor, citation, author| {
                    citation_ok(&owner, &community_id, actor, citation, &floors)
                        && roles.can_act_on_member(
                            actor,
                            &owner,
                            author,
                            Permissions::MANAGE_MESSAGES,
                        )
                });

            messages.reverse();

            Ok(Timeline { messages, has_more })
        })
    }

    /// Seal a message to the channel plane, cache it, then publish it to the relays.
    pub fn send(
        &self,
        channel: &ChannelId,
        content: &str,
        reply_to: Option<ReplyRef>,
        cx: &App,
    ) -> Option<Task<Result<EventId>>> {
        let (epoch, secret) = self.channel_secret(channel)?;

        let nostr = NostrRegistry::global(cx);
        let client = nostr.read(cx).client();
        let signer = nostr.read(cx).signer();
        let author = nostr.read(cx).current_user()?;

        let channel = *channel;
        let relays = self.state.relays.clone();
        let timer = self
            .control
            .community
            .as_ref()
            .and_then(|metadata| metadata.message_expiration);
        let content = content.to_owned();

        Some(cx.background_spawn(async move {
            let group = channel_group_key(&secret, &channel, epoch)?;
            let at_ms = now_ms()?;

            let rumor = cord03::build_message(
                author,
                &channel,
                epoch,
                &content,
                reply_to.as_ref(),
                at_ms,
                timer,
            );
            let (wrap, _) = cord03::seal_rumor(&rumor, &group, &signer, false).await?;

            let (opened, _) = cord03::open(&wrap, &group, &channel, epoch)?;
            cache::cache_rumor(&client, &channel, &opened).await?;

            sync::connect_relays(&client, &relays).await;
            sync::publish_wrap(&client, &wrap, &relays).await;

            Ok(opened.rumor_id)
        }))
    }

    /// Adopt plane material the account's list now carries.
    ///
    /// The caller re-folds afterwards; this only seeds the new planes.
    pub(crate) fn adopt(&mut self, state: CommunityState) {
        // A fold already in flight would write its pre-adoption state back.
        self.refresh_task = None;
        self.dirty = false;
        self.state = state;
    }

    /// Adopt whatever the rekey watch has delivered, then re-page what it moved.
    pub fn rekey(&mut self, cx: &mut Context<Self>) {
        if self.rekey_task.is_some() {
            self.rekey_dirty = true;
            return;
        }

        let nostr = NostrRegistry::global(cx);
        let Some(me) = nostr.read(cx).current_user() else {
            return;
        };

        let client = nostr.read(cx).client();
        let signer = nostr.read(cx).signer();
        let state = self.state.clone();
        let roles = self.control.roles.clone();

        let task =
            cx.background_spawn(
                async move { rekey::adopt(&client, &state, &roles, &signer, me).await },
            );

        self.rekey_task = Some(cx.spawn(async move |this, cx| {
            let result = task.await;
            this.update(cx, |this, cx| this.apply_rekey(result, cx))?;
            Ok(())
        }));
    }

    /// Rotate one scope to its next epoch, cutting off `excluded`.
    pub fn rotate(
        &mut self,
        scope: RekeyScope,
        recipients: &[PublicKey],
        excluded: &[PublicKey],
        cx: &mut Context<Self>,
    ) -> Option<Task<Result<Epoch>>> {
        let nostr = NostrRegistry::global(cx);
        let me = nostr.read(cx).current_user()?;

        let client = nostr.read(cx).client();
        let signer = nostr.read(cx).signer();

        if self.state.removed_at.is_some() || self.state.stranded || self.state.banned.contains(&me)
        {
            return None;
        }

        let rewrite = rekey::Rewrite {
            scope,
            recipients: recipients.to_vec(),
            excluded: excluded.to_vec(),
            citation: self.citation(&me),
        };

        if !rewrite.authorized(&self.control.roles, &self.state.owner, &me) {
            return None;
        }

        let state = self.state.clone();
        let roles = self.control.roles.clone();

        Some(cx.spawn(async move |this, cx| {
            let epoch = rekey::rotate(
                &client,
                &state,
                &roles,
                &signer,
                me,
                &rewrite,
                Timestamp::now(),
            )
            .await?;

            let adoptions = rekey::adopt(&client, &state, &roles, &signer, me).await?;

            this.update(cx, |this, cx| {
                if !adoptions.is_empty() {
                    this.merge_adoptions(adoptions, cx);
                }
            })?;

            Ok(epoch)
        }))
    }

    /// The rank this client cites when it acts.
    fn citation(&self, me: &PublicKey) -> Option<AuthorityCitation> {
        let entity = grant_locator(&self.state.id, &me.to_bytes());

        self.state
            .heads
            .iter()
            .find(|head| head.entity == entity)
            .map(|head| AuthorityCitation {
                entity: head.entity,
                version: head.version,
                hash: head.self_hash,
            })
    }

    fn apply_rekey(&mut self, result: Result<Adoptions>, cx: &mut Context<Self>) {
        self.rekey_task = None;

        match result {
            Ok(adoptions) if !adoptions.is_empty() => self.merge_adoptions(adoptions, cx),
            Ok(_) => {}
            Err(error) => cx.emit(CommunityEvent::Error(error.to_string())),
        }

        if self.rekey_dirty {
            self.rekey_dirty = false;
            self.rekey(cx);
        }
    }

    /// Fold an adoption into the held state, persist it, and re-page what moved.
    fn merge_adoptions(&mut self, adoptions: Adoptions, cx: &mut Context<Self>) {
        let mut touched: Vec<ChannelId> = Vec::new();

        if let Some(base) = adoptions.base {
            let mut held = Vec::with_capacity(base.stepped.len() + self.state.held_roots.len());

            for key in base.stepped {
                let known = self
                    .state
                    .held_roots
                    .iter()
                    .chain(held.iter())
                    .any(|root| root.epoch == key.epoch && root.key == key.key);

                if known {
                    continue;
                }

                held.push(HeldRoot {
                    epoch: key.epoch,
                    key: key.key,
                    control_pk: self.state.control_pks.get(&key.epoch.0).copied(),
                    retired_at: key.retired_at,
                });
            }

            for root in &self.state.held_roots {
                if held
                    .iter()
                    .any(|kept| kept.epoch == root.epoch && kept.key == root.key)
                {
                    continue;
                }

                held.push(*root);
            }

            self.state.held_roots = held;

            if let Some(control_pk) = base.control_pk {
                self.state.control_pks.insert(base.epoch.0, control_pk);
            }

            // The rotation is authoritative about the new epoch's signing root
            self.state.control_root = base.control_root;
            self.state.community_root = base.key;
            self.state.root_epoch = base.epoch;
            self.state.removed_at = None;
            self.state.stranded = false;

            // Every channel's plane moved with the root.
            touched.extend(self.state.channels.iter().map(|channel| channel.id));
        }

        for channel in adoptions.channels {
            let Some(held) = self
                .state
                .channels
                .iter_mut()
                .find(|held| held.id == channel.channel)
            else {
                continue;
            };

            // `stepped` carries the key held before the walk plus every epoch it
            // passed through, each with the cutoff its superseding rotation set.
            for key in channel.stepped {
                if !held.priors.iter().any(|prior| prior.epoch == key.epoch) {
                    held.priors.push(key);
                }
            }

            held.key = Some(channel.key);
            held.epoch = channel.epoch;
            held.private = true;
            touched.push(channel.channel);
        }

        for (channel, epoch) in adoptions.cuts {
            self.state.channels.retain(|held| held.id != channel);
            self.state.channel_cuts.insert(channel, epoch);
            self.state.cursors.remove(&channel);
        }

        // A rotation that delivered us no key still names the npub that minted the epoch.
        let learned = adoptions
            .refounders
            .into_iter()
            .filter(|refounder| self.state.refounders.insert(*refounder))
            .count();

        if let Some(epoch) = adoptions.removed_at {
            self.state.removed_at = Some(epoch);
        }

        if adoptions.stranded {
            self.state.stranded = true;
        }

        for channel in &touched {
            if let Some(cursor) = self.state.cursors.get_mut(channel) {
                cursor.exhausted = false;
            }
        }

        self.persist(cx);
        cx.notify();
        cx.emit(CommunityEvent::Updated(self.state.id));

        if learned > 0 {
            self.refresh(cx);
        }

        let Some(channel) = self
            .active
            .or_else(|| self.state.channels.first().map(|channel| channel.id))
        else {
            return;
        };

        let catch_up = self.sync_channel(&channel, Intent::CatchUp, cx);

        let task = cx.spawn(async move |_this, _cx| {
            if let Err(error) = catch_up.await {
                log::warn!("community: the catch-up after a rekey failed: {error}");
            }

            Ok(())
        });

        self.tasks.push(task);
    }

    /// Rebuilds the community from the wraps in the local database.
    pub fn refresh(&mut self, cx: &mut Context<Self>) {
        if self.refresh_task.is_some() {
            self.dirty = true;
            return;
        }

        let nostr = NostrRegistry::global(cx);
        let client = nostr.read(cx).client();

        let state = self.state.clone();
        let folded = cx.background_spawn(async move { sync::fold(&client, &state).await });

        self.refresh_task = Some(cx.spawn(async move |this, cx| {
            let result = folded.await;
            this.update(cx, |this, cx| this.apply(result, cx))?;
            Ok(())
        }));
    }

    fn apply(&mut self, result: Result<Option<Snapshot>>, cx: &mut Context<Self>) {
        self.refresh_task = None;

        match result {
            Ok(Some(snapshot)) => {
                let mut state = snapshot.state;
                state.cursors = std::mem::take(&mut self.state.cursors);
                self.state = state;
                self.control = snapshot.control;
                self.members = snapshot.members;
                self.load_images(cx);

                // A fold reads every plane, so its count is the truth for every
                // channel rather than a sample of the region one round read.
                // Replacing it is what lets the count fall again once a key is
                // adopted; a partial round only ever raises it.
                let reported = !snapshot.unreadable.is_empty();
                self.unreadable = snapshot.unreadable;

                if reported {
                    cx.emit(CommunityEvent::Unreadable(self.state.id));
                }

                cx.emit(CommunityEvent::Updated(self.state.id));
                cx.notify();
            }
            Ok(None) => {}
            Err(error) => cx.emit(CommunityEvent::Error(error.to_string())),
        }

        if self.dirty {
            self.dirty = false;
            self.refresh(cx);
        }
    }

    /// Resolve the folded icon and banner into local files.
    fn load_images(&mut self, cx: &mut Context<Self>) {
        let (icon, banner) = match self.control.community.as_ref() {
            Some(metadata) => (metadata.icon.clone(), metadata.banner.clone()),
            None => (None, None),
        };

        self.load_icon(icon, cx);
        self.load_banner(banner, cx);
    }

    fn load_icon(&mut self, icon: Option<ImageRef>, cx: &mut Context<Self>) {
        if self.icon_ref == icon {
            return;
        }

        self.icon_ref = icon.clone();
        self.icon = None;

        let Some(icon) = icon else {
            return;
        };

        self.icon_task = Some(cx.spawn(async move |this, cx| {
            match sync::resolve_image(&icon, cx).await {
                Ok(path) => {
                    this.update(cx, |this, cx| {
                        this.icon = Some(path);
                        cx.notify();
                    })?;
                }
                Err(error) => log::warn!("community icon: {error}"),
            }
            Ok(())
        }));
    }

    fn load_banner(&mut self, banner: Option<ImageRef>, cx: &mut Context<Self>) {
        if self.banner_ref == banner {
            return;
        }

        self.banner_ref = banner.clone();
        self.banner = None;

        let Some(banner) = banner else {
            return;
        };

        self.banner_task = Some(cx.spawn(async move |this, cx| {
            match sync::resolve_image(&banner, cx).await {
                Ok(path) => {
                    this.update(cx, |this, cx| {
                        this.banner = Some(path);
                        cx.notify();
                    })?;
                }
                Err(error) => log::warn!("community banner: {error}"),
            }
            Ok(())
        }));
    }
}

/// One round's progress and the cursor material it earned.
type RoundOutcome = (Progress, ChannelCursor);

/// A cursor as a filter bound may use it.
fn clamped(cursor: ChannelCursor, now: Timestamp) -> ChannelCursor {
    ChannelCursor {
        newest: cursor.newest.map(|newest| newest.min(now)),
        ..cursor
    }
}

/// The wall clock in epoch milliseconds: a rumor's `ms` tag carries the part of
/// the second the message was written in, and the fold orders rows by it.
fn now_ms() -> Result<u64> {
    let elapsed = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_err(|error| anyhow::anyhow!("the system clock is before the Unix epoch: {error}"))?;

    u64::try_from(elapsed.as_millis())
        .map_err(|error| anyhow::anyhow!("the system clock is out of range: {error}"))
}

/// Read a channel's history from the community's relays, in three passes.
async fn sync_round(
    client: &Client,
    pages: &PageRegistry,
    channel: &ChannelId,
    held: &[HeldKey],
    relays: &[RelayUrl],
    saved: ChannelCursor,
    intent: Intent,
) -> Result<RoundOutcome> {
    let mut progress = Progress::default();
    let mut round = ChannelCursor::default();

    // Expired rows drop at fold time otherwise, never from disk.
    let purged = cache::purge_expired(client, channel, Timestamp::now()).await?;

    if purged > 0 {
        log::debug!(
            "community: purged {purged} expired rumor(s) from {}",
            channel.to_hex()
        );
    }

    let newest = match intent {
        Intent::CatchUp => {
            let page = history::page(
                client,
                pages,
                channel,
                held,
                relays,
                Window::opening(saved),
                1,
                PAGE_WRAPS,
            )
            .await?;
            absorb(&mut progress, &page);
            page
        }
        Intent::Older { .. } => WrapPage::default(),
    };

    let bridge = match (intent, newest.oldest, saved.newest) {
        (Intent::CatchUp, Some(oldest), Some(saved_newest)) if oldest > saved_newest => {
            let page = history::page(
                client,
                pages,
                channel,
                held,
                relays,
                Window::between(saved_newest, oldest),
                CATCH_UP_PAGES,
                PAGE_WRAPS,
            )
            .await?;
            absorb(&mut progress, &page);
            page
        }
        // Nothing to bridge, so the newest region is already complete.
        _ => WrapPage {
            exhausted: true,
            ..WrapPage::default()
        },
    };

    let resume = match intent {
        Intent::CatchUp => saved.oldest.or(newest.oldest),
        Intent::Older { .. } => saved.oldest,
    };

    let budget = match intent {
        Intent::CatchUp => CATCH_UP_PAGES,
        Intent::Older { pages } => pages,
    };

    // A channel already swept to the bottom has nothing older to ask for.
    let older = match (saved.exhausted, resume) {
        (true, _) => WrapPage {
            exhausted: true,
            ..WrapPage::default()
        },
        (false, Some(until)) => {
            let page = history::page(
                client,
                pages,
                channel,
                held,
                relays,
                Window::older_than(until),
                budget,
                PAGE_WRAPS,
            )
            .await?;
            absorb(&mut progress, &page);
            page
        }
        (false, None) => WrapPage::default(),
    };

    if intent == Intent::CatchUp {
        let complete = !newest.failed && bridge.exhausted;
        let top = newest
            .newest
            .unwrap_or_default()
            .max(bridge.newest.unwrap_or_default());

        if complete && !top.is_zero() {
            round.newest = Some(top);
        }
    }

    round.oldest = older.oldest.or(newest.oldest);
    round.exhausted = older.exhausted;

    Ok((progress, round))
}

fn absorb(progress: &mut Progress, page: &WrapPage) {
    progress.unreadable += page.unreadable;
    progress.failed |= page.failed;
    progress.errors += page.errors;
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A stamp ahead of the local clock must never become a cursor.
    ///
    /// A durable `newest` past `now` bounds every later REQ below a region that
    /// has not happened yet, so the channel receives nothing at all — not live,
    /// not by round — while its older history keeps working, which is exactly
    /// what a peer with a fast clock (or a hostile stamp) would cause.
    #[test]
    fn a_future_stamp_cannot_push_a_cursor_past_now() {
        let now = Timestamp::now();
        let held = ChannelCursor {
            newest: Some(now - Duration::from_secs(30)),
            oldest: Some(now - Duration::from_secs(90)),
            exhausted: false,
        };
        let round = ChannelCursor {
            newest: Some(now + Duration::from_secs(86_400)),
            oldest: None,
            exhausted: true,
        };

        let merged = clamped(held.merge(round), now);

        assert_eq!(merged.newest, Some(now), "the frontier stops at the clock");
        assert_eq!(merged.oldest, Some(now - Duration::from_secs(90)));
        assert!(merged.exhausted, "the round's other findings still land");

        // A cursor already stored past `now` heals instead of staying deaf.
        let poisoned = ChannelCursor {
            newest: Some(now + Duration::from_secs(86_400)),
            ..ChannelCursor::default()
        };

        assert_eq!(clamped(poisoned, now).newest, Some(now));
    }

    /// One public channel under a held root, and nothing else.
    fn state(channel: ChannelId) -> CommunityState {
        CommunityState {
            id: CommunityId::from_bytes([0x42; 32]),
            name: None,
            owner: Keys::generate().public_key(),
            owner_salt: [0x01; 32],
            community_root: [0x02; 32],
            root_epoch: Epoch(0),
            control_root: None,
            control_pks: BTreeMap::new(),
            channels: vec![ChannelKeyRef {
                id: channel,
                name: "general".to_owned(),
                private: false,
                epoch: Epoch(0),
                key: None,
                priors: Vec::new(),
            }],
            relays: Vec::new(),
            heads: Vec::new(),
            banned: BTreeSet::new(),
            cursors: BTreeMap::new(),
            held_roots: Vec::new(),
            channel_cuts: BTreeMap::new(),
            refounders: BTreeSet::new(),
            removed_at: None,
            stranded: false,
            dissolved: false,
            added_at_ms: 0,
        }
    }

    fn community(state: CommunityState) -> Community {
        Community::new(state, PageRegistry::default())
    }

    /// A rotation that excluded us leaves no key to write under: a wrap sealed
    /// with the retired root would reach nobody who rotated with it.
    #[test]
    fn a_removed_member_holds_no_write_key() {
        let channel = ChannelId::from_bytes([0x9c; 32]);

        // A public channel writes from the community root while we hold it.
        assert_eq!(
            community(state(channel)).channel_secret(&channel),
            Some((Epoch(0), [0x02; 32]))
        );

        // A base removal closes the community...
        let mut removed = state(channel);
        removed.removed_at = Some(Epoch(1));
        assert_eq!(community(removed).channel_secret(&channel), None);

        // ...a strand closes it too, because the rotation moved past our epoch...
        let mut stranded = state(channel);
        stranded.stranded = true;
        assert_eq!(community(stranded).channel_secret(&channel), None);

        // ...and a channel cut closes the one channel.
        let mut cut = state(channel);
        cut.channel_cuts.insert(channel, Epoch(0));
        assert_eq!(community(cut).channel_secret(&channel), None);
    }
}
