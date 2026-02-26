use std::collections::{HashMap, HashSet};
use std::fmt::Display;

use anyhow::{anyhow, Error};
use common::config_dir;
use gpui::{App, AppContext, Context, Entity, Global, Subscription, Task};
use nostr_sdk::prelude::*;
use serde::{Deserialize, Serialize};
use smallvec::{smallvec, SmallVec};

pub fn init(cx: &mut App) {
    AppSettings::set_global(cx.new(AppSettings::new), cx)
}

macro_rules! setting_accessors {
    ($(pub $field:ident: $type:ty),* $(,)?) => {
        impl AppSettings {
            $(
                paste::paste! {
                    pub fn [<get_ $field>](cx: &App) -> $type {
                        Self::global(cx).read(cx).values.$field.clone()
                    }

                    pub fn [<update_ $field>](value: $type, cx: &mut App) {
                        Self::global(cx).update(cx, |this, cx| {
                            this.values.$field = value;
                            cx.notify();
                        });
                    }
                }
            )*
        }
    };
}

setting_accessors! {
    pub hide_avatar: bool,
    pub screening: bool,
    pub auth_mode: AuthMode,
    pub trusted_relays: HashSet<RelayUrl>,
    pub room_configs: HashMap<u64, RoomConfig>,
    pub file_server: Url,
}

/// Authentication mode
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
pub enum AuthMode {
    #[default]
    Auto,
    Manual,
}

impl Display for AuthMode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            AuthMode::Auto => write!(f, "Auto authentication"),
            AuthMode::Manual => write!(f, "Ask every time"),
        }
    }
}

/// Signer kind
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
pub enum SignerKind {
    #[default]
    Auto,
    User,
    Encryption,
}

impl SignerKind {
    pub fn auto(&self) -> bool {
        matches!(self, SignerKind::Auto)
    }

    pub fn user(&self) -> bool {
        matches!(self, SignerKind::User)
    }

    pub fn encryption(&self) -> bool {
        matches!(self, SignerKind::Encryption)
    }
}

/// Room configuration
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
pub struct RoomConfig {
    backup: bool,
    signer_kind: SignerKind,
}

impl RoomConfig {
    /// Get backup config
    pub fn backup(&self) -> bool {
        self.backup
    }

    /// Get signer kind config
    pub fn signer_kind(&self) -> &SignerKind {
        &self.signer_kind
    }

    /// Set backup config
    pub fn set_backup(&mut self, backup: bool) {
        self.backup = backup;
    }

    /// Set signer kind config
    pub fn set_signer_kind(&mut self, kind: &SignerKind) {
        self.signer_kind = kind.to_owned();
    }
}

/// Settings
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Settings {
    /// Hide user avatars
    pub hide_avatar: bool,

    /// Enable screening for unknown chat requests
    pub screening: bool,

    /// Authentication mode
    pub auth_mode: AuthMode,

    /// Trusted relays; Coop will automatically authenticate with these relays
    pub trusted_relays: HashSet<RelayUrl>,

    /// Configuration for each chat room
    pub room_configs: HashMap<u64, RoomConfig>,

    /// Server for blossom media attachments
    pub file_server: Url,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            hide_avatar: false,
            screening: true,
            auth_mode: AuthMode::default(),
            trusted_relays: HashSet::default(),
            room_configs: HashMap::default(),
            file_server: Url::parse("https://blossom.band/").unwrap(),
        }
    }
}

impl AsRef<Settings> for Settings {
    fn as_ref(&self) -> &Settings {
        self
    }
}

struct GlobalAppSettings(Entity<AppSettings>);

impl Global for GlobalAppSettings {}

/// Application settings
pub struct AppSettings {
    /// Settings
    values: Settings,

    /// Event subscriptions
    _subscriptions: SmallVec<[Subscription; 2]>,
}

impl AppSettings {
    /// Retrieve the global settings instance
    pub fn global(cx: &App) -> Entity<Self> {
        cx.global::<GlobalAppSettings>().0.clone()
    }

    /// Set the global settings instance
    fn set_global(state: Entity<Self>, cx: &mut App) {
        cx.set_global(GlobalAppSettings(state));
    }

    fn new(cx: &mut Context<Self>) -> Self {
        let mut subscriptions = smallvec![];

        subscriptions.push(
            // Observe and automatically save settings on changes
            cx.observe_self(|this, cx| {
                this.save(cx);
            }),
        );

        cx.defer(|cx| {
            let settings = AppSettings::global(cx);

            settings.update(cx, |this, cx| {
                this.load(cx);
            });
        });

        Self {
            values: Settings::default(),
            _subscriptions: subscriptions,
        }
    }

    /// Update settings
    fn set_settings(&mut self, settings: Settings, cx: &mut Context<Self>) {
        self.values = settings;
        cx.notify();
    }

    /// Load settings
    fn load(&mut self, cx: &mut Context<Self>) {
        let task: Task<Result<Settings, Error>> = cx.background_spawn(async move {
            let path = config_dir().join(".settings");

            if let Ok(content) = smol::fs::read_to_string(&path).await {
                Ok(serde_json::from_str(&content)?)
            } else {
                Err(anyhow!("Not found"))
            }
        });

        cx.spawn(async move |this, cx| {
            let settings = task.await.unwrap_or(Settings::default());

            // Update settings
            this.update(cx, |this, cx| {
                this.set_settings(settings, cx);
            })
            .ok();
        })
        .detach();
    }

    /// Save settings
    pub fn save(&mut self, cx: &mut Context<Self>) {
        let settings = self.values.clone();

        let task: Task<Result<(), Error>> = cx.background_spawn(async move {
            let path = config_dir().join(".settings");
            let content = serde_json::to_string(&settings)?;

            // Write settings to file
            smol::fs::write(&path, content).await?;

            Ok(())
        });

        task.detach();
    }

    /// Check if the given relay is already authenticated
    pub fn trusted_relay(&self, url: &RelayUrl, _cx: &App) -> bool {
        self.values.trusted_relays.iter().any(|relay| {
            relay.as_str_without_trailing_slash() == url.as_str_without_trailing_slash()
        })
    }

    /// Add a relay to the trusted list
    pub fn add_trusted_relay(&mut self, url: &RelayUrl, cx: &mut Context<Self>) {
        self.values.trusted_relays.insert(url.clone());
        cx.notify();
    }

    /// Add a room configuration
    pub fn add_room_config(&mut self, id: u64, config: RoomConfig, cx: &mut Context<Self>) {
        self.values
            .room_configs
            .entry(id)
            .and_modify(|this| *this = config)
            .or_default();
        cx.notify();
    }
}
