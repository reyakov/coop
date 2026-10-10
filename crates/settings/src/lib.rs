use anyhow::{Error, anyhow};
use common::config_dir;
use gpui::{App, AppContext, Context, Entity, Global, Subscription, Task};
use nostr_sdk::prelude::*;
use serde::{Deserialize, Serialize};
use smallvec::{SmallVec, smallvec};
use theme::{AppExt as _, AppearanceMode};

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
    pub nip4e: bool,
    pub file_server: Url,
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

/// Settings
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Settings {
    /// Which appearance to paint: follow the OS, or pin light or dark.
    /// `alias` reads the pre-bezel `theme_mode` key so old config files keep
    /// their pinned choice.
    #[serde(default, alias = "theme_mode")]
    pub appearance_mode: AppearanceMode,

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
            nip4e: false,
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
        let mode = self.inner.read(cx).appearance_mode;
        cx.set_appearance_mode(mode);
    }

    /// Check if decoupling encryption key is enabled
    pub fn is_nip4e_enabled(&self, cx: &App) -> bool {
        self.inner.read(cx).nip4e
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

#[cfg(test)]
mod tests {
    use super::*;

    fn json_with(partial: &str) -> Result<Settings, serde_json::Error> {
        // Every field the persisted format has always required; `partial`
        // contributes the fields under test.
        serde_json::from_str(&format!(
            r#"{{ {} "hide_avatar": false, "screening": true, "nip4e": false,
                 "trusted_relays": [], "file_server": "https://nostr.download/" }}"#,
            partial
        ))
    }

    #[test]
    fn old_theme_mode_key_keeps_its_pinned_choice() {
        let settings = json_with(r#""theme_mode": "Dark", "#).unwrap();
        assert_eq!(settings.appearance_mode, AppearanceMode::Dark);
    }

    #[test]
    fn missing_appearance_mode_falls_back_to_dark() {
        let settings = json_with("").unwrap();
        assert_eq!(settings.appearance_mode, AppearanceMode::Dark);
    }
}
