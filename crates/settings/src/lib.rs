use std::rc::Rc;

use anyhow::{Error, anyhow};
use common::config_dir;
use gpui::{App, AppContext, Context, Entity, Global, Subscription, Task, Window};
use nostr_sdk::prelude::*;
use serde::{Deserialize, Serialize};
use smallvec::{SmallVec, smallvec};
use theme::{Theme, ThemeFamily, ThemeMode};

pub fn init(cx: &mut App) {
    AppSettings::set_global(cx.new(AppSettings::new), cx)
}

const DEFAULT_FILE_SERVER: &str = "https://nostr.download/";
const LEGACY_FILE_SERVER: &str = "blossom.band";

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

setting_accessors! {
    pub theme: Option<String>,
    pub theme_mode: ThemeMode,
    pub hide_avatar: bool,
    pub screening: bool,
    pub nip4e: bool,
    pub trusted_relays: Vec<String>,
    pub file_server: Url,
    pub pinned_rooms: Vec<u64>,
    pub expanded_sections: Option<Vec<String>>,
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

/// Settings
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Settings {
    /// Theme
    pub theme: Option<String>,

    /// Theme mode
    pub theme_mode: ThemeMode,

    /// Hide user avatars
    pub hide_avatar: bool,

    /// Enable screening for unknown chat requests
    pub screening: bool,

    /// Enable decoupling encryption key
    pub nip4e: bool,

    /// Trusted relays; Coop will automatically authenticate with these relays
    pub trusted_relays: Vec<String>,

    /// Server for blossom media attachments
    pub file_server: Url,

    /// Pinned sidebar room ids, in pin order
    #[serde(default)]
    pub pinned_rooms: Vec<u64>,

    /// Expanded sidebar tree sections; `None` means the default sections
    #[serde(default)]
    pub expanded_sections: Option<Vec<String>>,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            theme: None,
            theme_mode: ThemeMode::default(),
            hide_avatar: false,
            screening: true,
            nip4e: false,
            trusted_relays: vec![],
            file_server: Url::parse(DEFAULT_FILE_SERVER).unwrap(),
            pinned_rooms: vec![],
            expanded_sections: None,
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
    inner: Entity<Settings>,

    /// Event subscriptions
    _subscriptions: SmallVec<[Subscription; 2]>,
}

impl AppSettings {
    /// Retrieve the global settings instance
    pub fn global(cx: &App) -> Entity<Self> {
        cx.global::<GlobalAppSettings>().0.clone()
    }

    /// The underlying settings entity, which notifies whenever any field changes.
    /// Settings load asynchronously, so observers can watch it to pick up values
    /// that arrive after construction.
    pub fn entity(&self) -> &Entity<Settings> {
        &self.inner
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
                this.apply_theme(None, cx);
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

    /// Set theme
    pub fn set_theme<T>(&mut self, theme: T, window: &mut Window, cx: &mut Context<Self>)
    where
        T: Into<String>,
    {
        // Update settings
        self.inner.update(cx, |this, cx| {
            this.theme = Some(theme.into());
            cx.notify();
        });

        // Apply the new theme
        self.apply_theme(Some(window), cx);
    }

    /// Reset theme
    pub fn reset_theme(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.inner.update(cx, |this, cx| {
            this.theme = None;
            cx.notify();
        });
        self.apply_theme(Some(window), cx);
    }

    /// Apply theme
    pub fn apply_theme(&mut self, mut window: Option<&mut Window>, cx: &mut Context<Self>) {
        if let Some(name) = self.inner.read(cx).theme.as_ref() {
            let mode = self.inner.read(cx).theme_mode;

            if let Ok(new_theme) = ThemeFamily::from_assets(name) {
                Theme::apply_theme(Rc::new(new_theme), window.as_deref_mut(), cx);
                Theme::change(mode, window, cx);
            } else {
                log::info!("Failed to load theme: {name}");
            }
        } else {
            Theme::apply_theme(Rc::new(ThemeFamily::default()), window, cx);
        }
    }

    /// Check if decoupling encryption key is enabled
    pub fn is_nip4e_enabled(&self, cx: &App) -> bool {
        self.inner.read(cx).nip4e
    }

    /// Check if the given relay is already authenticated
    pub fn trusted_relay(&self, url: &RelayUrl, cx: &App) -> bool {
        self.inner
            .read(cx)
            .trusted_relays
            .iter()
            .any(|relay| relay == url.as_str_without_trailing_slash())
    }

    /// Add a relay to the trusted list
    pub fn add_trusted_relay(&mut self, url: &RelayUrl, cx: &mut Context<Self>) {
        self.inner.update(cx, |this, cx| {
            if !this
                .trusted_relays
                .iter()
                .any(|relay| relay == url.as_str_without_trailing_slash())
            {
                this.trusted_relays
                    .push(url.as_str_without_trailing_slash().to_string());
                cx.notify();
            }
        });
    }
}
