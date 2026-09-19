use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;

use anyhow::Result;
use concord::cord02::{ControlFold, ImageRef};
use concord::store::{ChannelKeyRef, CommunityState};
use concord::{ChannelId, CommunityId, Epoch};
use gpui::{AppContext, Context, EventEmitter, Task};
use nostr_sdk::prelude::*;
use state::NostrRegistry;

use crate::sync::{self, Snapshot};

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

    pub(crate) fn relays(&self) -> &[RelayUrl] {
        &self.relays
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
    icon: Option<PathBuf>,
    icon_ref: Option<ImageRef>,
    dirty: bool,
    refresh_task: Option<Task<Result<()>>>,
    icon_task: Option<Task<Result<()>>>,
}

impl EventEmitter<CommunityEvent> for Community {}

impl Community {
    pub fn new(state: CommunityState) -> Self {
        Self {
            state,
            control: ControlFold::default(),
            members: BTreeSet::new(),
            icon: None,
            icon_ref: None,
            dirty: false,
            refresh_task: None,
            icon_task: None,
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

    pub fn members(&self) -> &BTreeSet<PublicKey> {
        &self.members
    }

    pub fn channels(&self) -> &[ChannelKeyRef] {
        &self.state.channels
    }

    pub fn subscription_key(&self) -> SubscriptionKey {
        SubscriptionKey::of(&self.state)
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
                self.state = snapshot.state;
                self.control = snapshot.control;
                self.members = snapshot.members;
                self.load_icon(cx);
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

    /// Resolve the folded icon into a local file.
    fn load_icon(&mut self, cx: &mut Context<Self>) {
        let icon = self
            .control
            .community
            .as_ref()
            .and_then(|metadata| metadata.icon.clone());

        if self.icon_ref == icon {
            return;
        }

        self.icon_ref = icon.clone();
        self.icon = None;

        let Some(icon) = icon else {
            return;
        };

        self.icon_task = Some(cx.spawn(async move |this, cx| {
            match sync::resolve_icon(&icon, cx).await {
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
}
