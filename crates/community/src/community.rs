use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use anyhow::Result;
use concord::cord02::ControlFold;
use concord::store::{ChannelKeyRef, CommunityState};
use concord::{ChannelId, CommunityId, Epoch};
use gpui::{AppContext, Context, EventEmitter, Task};
use nostr_sdk::prelude::*;

use crate::sync::{self, Snapshot};

/// Everything that decides which planes a community is subscribed to. The
/// registry re-subscribes only when this changes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SubscriptionKey {
    control_pks: BTreeMap<u64, PublicKey>,
    channels: Vec<(ChannelId, Epoch, bool)>,
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
            relays: state.relays.clone(),
        }
    }
}

#[derive(Debug, Clone)]
pub enum CommunityEvent {
    Updated(CommunityId),
    Error(String),
}

pub struct Community {
    state: CommunityState,
    control: ControlFold,
    members: BTreeSet<PublicKey>,
    database: Arc<dyn NostrDatabase>,
    dirty: bool,
    refresh_task: Option<Task<Result<()>>>,
}

impl EventEmitter<CommunityEvent> for Community {}

impl Community {
    pub fn new(state: CommunityState, database: Arc<dyn NostrDatabase>) -> Self {
        Self {
            state,
            control: ControlFold::default(),
            members: BTreeSet::new(),
            database,
            dirty: false,
            refresh_task: None,
        }
    }

    pub fn id(&self) -> CommunityId {
        self.state.id
    }

    pub fn state(&self) -> &CommunityState {
        &self.state
    }

    pub fn control(&self) -> &ControlFold {
        &self.control
    }

    pub fn members(&self) -> &BTreeSet<PublicKey> {
        &self.members
    }

    pub fn channels(&self) -> &[ChannelKeyRef] {
        &self.state.channels
    }

    pub fn subscription_key(&self) -> SubscriptionKey {
        SubscriptionKey::of(&self.state)
    }

    /// Rebuilds the community from the wraps in the local database. A burst of
    /// signals produces at most two folds: one running, one owed.
    pub fn refresh(&mut self, cx: &mut Context<Self>) {
        if self.refresh_task.is_some() {
            self.dirty = true;
            return;
        }

        let database = self.database.clone();
        let state = self.state.clone();
        let folded =
            cx.background_spawn(async move { sync::fold(database.as_ref(), &state).await });

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
                self.state = snapshot.state;
                self.control = snapshot.control;
                self.members = snapshot.members;
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
}

#[cfg(test)]
mod tests {
    use concord::cord02::GENERAL_CHANNEL;
    use concord::cord02::guestbook::{build_join, build_leave, seal_rumor};
    use concord::derive::guestbook_group_key;
    use concord::store::{load_state, save_state};
    use nostr_memory::MemoryDatabase;

    use super::*;
    use crate::sync::fixtures::{AT_MS, community};

    #[test]
    fn genesis_folds_into_metadata_channels_and_the_owner() {
        smol::block_on(async {
            let owner = Keys::generate();
            let (genesis, state) = community(&owner);
            let database = MemoryDatabase::unbounded();

            for wrap in &genesis.wraps {
                database.save_event(wrap).await.expect("saves wrap");
            }
            save_state(&database, &state).await.expect("saves state");

            let snapshot = sync::fold(&database, &state)
                .await
                .expect("folds")
                .expect("genesis is a control edition");

            let metadata = snapshot.control.community.as_ref().expect("metadata");
            assert_eq!(metadata.name, "Room");

            let channel = snapshot
                .control
                .channels
                .get(&genesis.channel_id)
                .expect("general channel");
            assert_eq!(channel.name, GENERAL_CHANNEL);
            assert!(!channel.private);

            assert_eq!(snapshot.members, BTreeSet::from([owner.public_key()]));

            let persisted = load_state(&database, &state.id)
                .await
                .expect("loads")
                .expect("persisted");
            assert_eq!(persisted.id, state.id);
        });
    }

    #[test]
    fn a_join_adds_a_member_and_a_later_leave_removes_them() {
        smol::block_on(async {
            let owner = Keys::generate();
            let member = Keys::generate();
            let (genesis, state) = community(&owner);
            let database = MemoryDatabase::unbounded();

            for wrap in &genesis.wraps {
                database.save_event(wrap).await.expect("saves wrap");
            }
            save_state(&database, &state).await.expect("saves state");

            let guestbook = guestbook_group_key(&state.community_root, &state.id, state.root_epoch)
                .expect("guestbook key");

            let join = seal_rumor(
                &build_join(member.public_key(), None, AT_MS + 1_000),
                &guestbook,
                &member,
            )
            .expect("seals join")
            .0;
            database.save_event(&join).await.expect("saves join");

            let joined = sync::fold(&database, &state)
                .await
                .expect("folds")
                .expect("control is still held");
            assert!(joined.members.contains(&member.public_key()));
            assert_eq!(joined.members.len(), 2);

            let leave = seal_rumor(
                &build_leave(member.public_key(), AT_MS + 2_000),
                &guestbook,
                &member,
            )
            .expect("seals leave")
            .0;
            database.save_event(&leave).await.expect("saves leave");

            let left = sync::fold(&database, &joined.state)
                .await
                .expect("folds")
                .expect("control is still held");
            assert!(!left.members.contains(&member.public_key()));
            assert_eq!(left.members, BTreeSet::from([owner.public_key()]));
        });
    }
}
