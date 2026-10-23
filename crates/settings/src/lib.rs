use std::path::PathBuf;

use anyhow::{Error, anyhow};
use common::config_dir;
use gpui::{App, AppContext, Context, Entity, Global, ImageSource, Subscription, Task};
use nostr_sdk::prelude::*;
use serde::{Deserialize, Serialize};
use smallvec::{SmallVec, smallvec};

/// The user's appearance preference, persisted with the rest of the settings.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum AppearanceMode {
    /// Follow the OS.
    System,
    Light,
    /// Paint dark regardless of the OS.
    #[default]
    Dark,
}

impl AppearanceMode {
    pub const ALL: [Self; 3] = [Self::System, Self::Light, Self::Dark];

    /// Menu/label text.
    pub fn label(self) -> &'static str {
        match self {
            Self::System => "System",
            Self::Light => "Light",
            Self::Dark => "Dark",
        }
    }
}

pub fn init(cx: &mut App) {
    AppSettings::set_global(cx.new(AppSettings::new), cx)
}

/// Read the persisted appearance mode synchronously.
///
/// The async load fills the global settings later; the native theme must be
/// applied before the first window opens, so boot reads the mode straight
/// from the settings file instead.
pub fn persisted_appearance_mode() -> AppearanceMode {
    #[cfg(not(target_arch = "wasm32"))]
    {
        let path = config_dir().join(".settings");
        if let Ok(content) = std::fs::read_to_string(path)
            && let Ok(settings) = serde_json::from_str::<Settings>(&content)
        {
            return settings.appearance_mode;
        }
    }
    AppearanceMode::default()
}

const DEFAULT_FILE_SERVER: &str = "https://nostr.download/";
const LEGACY_FILE_SERVER: &str = "blossom.band";

/// Built-in image shown behind chat and group panels
pub const DEFAULT_BACKGROUND: &str = "brand/default-bg.jpg";

macro_rules! setting_accessors {
    ($(pub $field:ident: $type:ty),* $(,)?) => {
        impl AppSettings {
            $(
                paste::paste! {
                    pub fn [<get_ $field>](cx: &App) -> $type {
                        Self::global(cx).read(cx).inner.read(cx).$field.clone()
                    }

                    pub fn [<update_ $field>](value: $type, cx: &mut App) {
                        Self::global(cx).update(cx, |this, cx| {
                            this.inner.update(cx, |inner, cx| {
                                inner.$field = value;
                                cx.notify();
                            });
                        });
                    }
                }
            )*
        }
    };
}

macro_rules! setting_getters {
    ($(pub $field:ident: $type:ty),* $(,)?) => {
        impl AppSettings {
            $(
                paste::paste! {
                    pub fn [<get_ $field>](cx: &App) -> $type {
                        Self::global(cx).read(cx).inner.read(cx).$field.clone()
                    }
                }
            )*
        }
    };
}

setting_accessors! {
    pub appearance_mode: AppearanceMode,
    pub hide_avatar: bool,
    pub screening: bool,
    pub wot_auto_trust: bool,
    pub nip4e: bool,
    pub hide_group_notice: bool,
    pub file_server: Url,
    pub panel_background: bool,
    pub panel_background_path: Option<PathBuf>,
}

setting_getters! {
    pub pinned_rooms: Vec<u64>,
    pub pinned_groups: Vec<String>,
}

/// Signer kind
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
pub enum SignerKind {
    Auto,
    Encryption,
    #[default]
    User,
}

impl SignerKind {
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
    pub fn new() -> Self {
        Self {
            backup: true,
            signer_kind: SignerKind::default(),
        }
    }

    /// Get backup config
    pub fn backup(&self) -> bool {
        self.backup
    }

    /// Set backup config
    pub fn toggle_backup(&mut self) {
        self.backup = !self.backup;
    }

    /// Get signer kind config
    pub fn signer_kind(&self) -> &SignerKind {
        &self.signer_kind
    }

    /// Set signer kind config
    pub fn set_signer_kind(&mut self, kind: &SignerKind) {
        self.signer_kind = kind.to_owned();
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Settings {
    #[serde(default, alias = "theme_mode")]
    pub appearance_mode: AppearanceMode,
    /// Hide user avatars
    pub hide_avatar: bool,
    /// Enable screening for unknown chat requests
    pub screening: bool,
    /// Promote chat requests from people followed by the user's contacts
    pub wot_auto_trust: bool,
    /// Enable decoupling encryption key
    pub nip4e: bool,
    /// Hide the notice that messages in public groups are not encrypted
    #[serde(default)]
    pub hide_group_notice: bool,
    /// Show an image behind chat and group panels
    #[serde(default)]
    pub panel_background: bool,
    /// Custom image for the panel background; the built-in one when unset
    #[serde(default)]
    pub panel_background_path: Option<PathBuf>,
    /// Trusted relays; Coop will automatically authenticate with these relays
    pub trusted_relays: Vec<String>,
    /// Server for blossom media attachments
    pub file_server: Url,
    /// Pinned chat rooms, by conversation ID
    #[serde(default)]
    pub pinned_rooms: Vec<u64>,
    /// Pinned groups, by cache tag
    #[serde(default)]
    pub pinned_groups: Vec<String>,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            appearance_mode: AppearanceMode::default(),
            hide_avatar: false,
            screening: true,
            wot_auto_trust: true,
            nip4e: false,
            hide_group_notice: false,
            panel_background: true,
            panel_background_path: None,
            trusted_relays: vec![],
            file_server: Url::parse(DEFAULT_FILE_SERVER).unwrap(),
            pinned_rooms: vec![],
            pinned_groups: vec![],
        }
    }
}

struct GlobalAppSettings(Entity<AppSettings>);

impl Global for GlobalAppSettings {}

/// Application settings
pub struct AppSettings {
    /// Settings
    inner: Entity<Settings>,
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
        let entity = cx.entity().downgrade();
        let inner = cx.new(|_| Settings::default());
        let mut subscriptions = smallvec![];

        subscriptions.push(
            // Observe and automatically save settings on changes
            cx.observe(&inner, |this, _inner, cx| {
                this.save(cx);
            }),
        );

        // Run at the end of current cycle
        cx.defer(move |cx| {
            entity.update(cx, |this, cx| this.load(cx)).ok();
        });

        Self {
            inner,
            _subscriptions: subscriptions,
        }
    }

    /// Update settings
    fn set_settings(&mut self, settings: Settings, cx: &mut Context<Self>) {
        self.inner.update(cx, |this, cx| {
            *this = settings;
            cx.notify();
        });
    }

    /// Load settings
    fn load(&mut self, cx: &mut Context<Self>) {
        let task: Task<Result<Settings, Error>> = cx.background_spawn(async move {
            #[cfg(not(target_arch = "wasm32"))]
            {
                let path = config_dir().join(".settings");
                if let Ok(content) = smol::fs::read_to_string(&path).await {
                    return Ok(serde_json::from_str(&content)?);
                }
            }
            Err(anyhow!("Not found"))
        });

        cx.spawn(async move |this, cx| {
            let mut settings = task.await.unwrap_or(Settings::default());

            // Move settings still pointed at the old default file server over to the new one
            if settings.file_server.host_str() == Some(LEGACY_FILE_SERVER) {
                settings.file_server = Url::parse(DEFAULT_FILE_SERVER).unwrap();
            }

            // Update settings
            this.update(cx, |this, cx| {
                this.set_settings(settings, cx);
                this.apply_theme(cx);
                cx.refresh_windows();
            })
            .ok();
        })
        .detach();
    }

    /// Save settings
    pub fn save(&mut self, cx: &mut Context<Self>) {
        let settings = self.inner.read(cx);
        if let Ok(content) = serde_json::to_string(&settings) {
            #[cfg(not(target_arch = "wasm32"))]
            cx.background_spawn(async move {
                let path = config_dir().join(".settings");
                smol::fs::write(&path, content).await.ok();
            })
            .detach();
        }
    }

    /// Apply the persisted appearance mode to the app
    pub fn apply_theme(&mut self, cx: &mut Context<Self>) {
        use gpui_kit::component::Theme;

        let mode = self.inner.read(cx).appearance_mode;
        match mode {
            AppearanceMode::System => Theme::sync_system_appearance(None, cx),
            AppearanceMode::Light => Theme::change(gpui_kit::component::ThemeMode::Light, None, cx),
            AppearanceMode::Dark => Theme::change(gpui_kit::component::ThemeMode::Dark, None, cx),
        }
        cx.refresh_windows();
    }

    /// Check if decoupling encryption key is enabled
    pub fn is_nip4e_enabled(&self, cx: &App) -> bool {
        self.inner.read(cx).nip4e
    }

    /// The image to show behind chat and group panels, when enabled
    pub fn panel_background_source(cx: &App) -> Option<ImageSource> {
        if !Self::get_panel_background(cx) {
            return None;
        }
        Some(match Self::get_panel_background_path(cx) {
            Some(path) => path.into(),
            None => DEFAULT_BACKGROUND.into(),
        })
    }

    /// Check if the chat room is pinned
    pub fn pinned_room(&self, id: u64, cx: &App) -> bool {
        self.inner.read(cx).pinned_rooms.contains(&id)
    }

    /// Pin or unpin the chat room
    pub fn toggle_pinned_room(&mut self, id: u64, cx: &mut Context<Self>) {
        self.inner.update(cx, |this, cx| {
            if let Some(index) = this.pinned_rooms.iter().position(|pinned| *pinned == id) {
                this.pinned_rooms.remove(index);
            } else {
                this.pinned_rooms.push(id);
            }
            cx.notify();
        });
    }

    /// Check if the group is pinned by its cache tag
    pub fn pinned_group(&self, tag: &str, cx: &App) -> bool {
        self.inner
            .read(cx)
            .pinned_groups
            .iter()
            .any(|pinned| pinned == tag)
    }

    /// Pin or unpin the group by its cache tag
    pub fn toggle_pinned_group(&mut self, tag: &str, cx: &mut Context<Self>) {
        self.inner.update(cx, |this, cx| {
            if let Some(index) = this.pinned_groups.iter().position(|pinned| pinned == tag) {
                this.pinned_groups.remove(index);
            } else {
                this.pinned_groups.push(tag.to_owned());
            }
            cx.notify();
        });
    }
}
