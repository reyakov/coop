use std::collections::{HashMap, HashSet};

use anyhow::{anyhow, Error};
use gpui::{App, AppContext, Context, Entity, Global, Subscription, Task};
use nostr_sdk::prelude::*;
use serde::{Deserialize, Serialize};
use smallvec::{smallvec, SmallVec};
use state::NostrRegistry;

const SETTINGS_IDENTIFIER: &str = "coop:settings";

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
    Manual,
    Auto,
}

/// Signer kind
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
pub enum SignerKind {
    #[default]
    Auto,
    User,
    Device,
}

/// Room configuration
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
pub struct RoomConfig {
    backup: bool,
    signer_kind: SignerKind,
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

    /// File server for NIP-96 media attachments
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
            file_server: Url::parse("https://nostrmedia.com").unwrap(),
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
    _subscriptions: SmallVec<[Subscription; 1]>,

    /// Background tasks
    _tasks: SmallVec<[Task<()>; 1]>,
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
        let load_settings = Self::get_from_database(false, cx);

        let mut tasks = smallvec![];
        let mut subscriptions = smallvec![];

        subscriptions.push(
            // Observe and automatically save settings on changes
            cx.observe_self(|this, cx| {
                this.save(cx);
            }),
        );

        tasks.push(
            // Load the initial settings
            cx.spawn(async move |this, cx| {
                if let Ok(settings) = load_settings.await {
                    this.update(cx, |this, cx| {
                        this.values = settings;
                        cx.notify();
                    })
                    .ok();
                }
            }),
        );

        Self {
            values: Settings::default(),
            _subscriptions: subscriptions,
            _tasks: tasks,
        }
    }

    /// Get settings from the database
    ///
    /// If `current_user` is true, the settings will be retrieved for current user.
    /// Otherwise, Coop will load the latest settings from the database.
    fn get_from_database(current_user: bool, cx: &App) -> Task<Result<Settings, Error>> {
        let nostr = NostrRegistry::global(cx);
        let client = nostr.read(cx).client();

        cx.background_spawn(async move {
            // Construct a filter to get the latest settings
            let mut filter = Filter::new()
                .kind(Kind::ApplicationSpecificData)
                .identifier(SETTINGS_IDENTIFIER)
                .limit(1);

            if current_user {
                let signer = client.signer().await?;
                let public_key = signer.get_public_key().await?;

                // Push author to the filter
                filter = filter.author(public_key);
            }

            if let Some(event) = client.database().query(filter).await?.first_owned() {
                Ok(serde_json::from_str(&event.content).unwrap_or(Settings::default()))
            } else {
                Err(anyhow!("Not found"))
            }
        })
    }

    /// Load settings
    pub fn load(&mut self, cx: &mut Context<Self>) {
        let task = Self::get_from_database(true, cx);

        self._tasks.push(
            // Run task in the background
            cx.spawn(async move |this, cx| {
                if let Ok(settings) = task.await {
                    this.update(cx, |this, cx| {
                        this.values = settings;
                        cx.notify();
                    })
                    .ok();
                }
            }),
        );
    }

    /// Save settings
    pub fn save(&mut self, cx: &mut Context<Self>) {
        let nostr = NostrRegistry::global(cx);
        let client = nostr.read(cx).client();

        if let Ok(content) = serde_json::to_string(&self.values) {
            let task: Task<Result<(), Error>> = cx.background_spawn(async move {
                let signer = client.signer().await?;
                let public_key = signer.get_public_key().await?;

                let event = EventBuilder::new(Kind::ApplicationSpecificData, content)
                    .tag(Tag::identifier(SETTINGS_IDENTIFIER))
                    .build(public_key)
                    .sign(&Keys::generate())
                    .await?;

                client.database().save_event(&event).await?;

                Ok(())
            });

            task.detach();
        }
    }

    /// Check if the given relay is trusted
    pub fn is_trusted_relay(&self, url: &RelayUrl, _cx: &App) -> bool {
        self.values.trusted_relays.contains(url)
    }

    /// Add a relay to the trusted list
    pub fn add_trusted_relay(&mut self, url: RelayUrl, cx: &mut Context<Self>) {
        self.values.trusted_relays.insert(url);
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
