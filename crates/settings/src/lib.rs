use std::path::PathBuf;
use std::rc::Rc;

use anyhow::{Error, anyhow};
#[cfg(not(target_arch = "wasm32"))]
use common::config_dir;
use gpui::{App, AppContext, Context, Entity, Global, ImageSource, Subscription, Task, px};
use gpui_kit::component::{Theme, ThemeConfig, ThemeMode, ThemeRegistry};
use nostr_sdk::prelude::*;
use serde::{Deserialize, Serialize};
use smallvec::{SmallVec, smallvec};

/// Browser storage key for the settings JSON
#[cfg(target_arch = "wasm32")]
const SETTINGS_KEY: &str = "coop.settings";

/// The default base font size for the interface
pub const DEFAULT_FONT_SIZE: f32 = 16.0;

const DEFAULT_FILE_SERVER: &str = "https://nostr.download/";
const LEGACY_FILE_SERVER: &str = "blossom.band";
/// Minimum days between automatic encryption key rotations
pub const NIP4E_ROTATION_MIN_DAYS: u32 = 1;
/// Maximum days between automatic encryption key rotations
pub const NIP4E_ROTATION_MAX_DAYS: u32 = 90;
/// Days between automatic encryption key rotations when enabled
pub const NIP4E_ROTATION_DEFAULT_DAYS: u32 = 30;
/// Built-in image shown behind chat and group panels
pub const DEFAULT_BACKGROUND: &str = "brand/default-bg.jpg";

fn default_onboarded() -> bool {
    true
}

fn default_shadow() -> bool {
    true
}

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

/// User tuning applied on top of the native OS theme
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct ThemeSettings {
    /// The base UI font size in pixels
    pub font_size: f32,
    /// Draw a ring around focused controls
    pub focus_ring: bool,
    /// Corner radius for general elements; follows the OS theme when unset
    pub radius: Option<f32>,
    /// Corner radius for large elements like dialogs; follows the OS theme when unset
    pub radius_lg: Option<f32>,
    /// Draw soft shadows on inputs, buttons and dialogs
    #[serde(default = "default_shadow")]
    pub shadow: bool,
}

impl Default for ThemeSettings {
    fn default() -> Self {
        Self {
            font_size: DEFAULT_FONT_SIZE,
            focus_ring: false,
            radius: None,
            radius_lg: None,
            shadow: true,
        }
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
    /// Days between automatic encryption key rotations; None disables rotation
    #[serde(default)]
    pub nip4e_rotation_days: Option<u32>,
    /// Hide the notice that messages in public groups are not encrypted
    #[serde(default)]
    pub hide_group_notice: bool,
    /// Enable the NIP-60 wallet
    #[serde(default)]
    pub wallet: bool,
    /// Whether the first-run onboarding has been completed
    #[serde(default = "default_onboarded")]
    pub onboarded: bool,
    /// Tuning applied on top of the native OS theme
    #[serde(default)]
    pub theme: ThemeSettings,
    /// Bundled theme applied in light appearance; None uses the default light theme
    #[serde(default)]
    pub theme_light: Option<String>,
    /// Bundled theme applied in dark appearance; None uses the default dark theme
    #[serde(default)]
    pub theme_dark: Option<String>,
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
            wot_auto_trust: false,
            nip4e: false,
            nip4e_rotation_days: None,
            hide_group_notice: false,
            wallet: false,
            onboarded: false,
            theme: ThemeSettings::default(),
            theme_light: None,
            theme_dark: None,
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
    /// The pending save task, so writes to the settings file complete in order
    #[cfg(not(target_arch = "wasm32"))]
    save_task: Option<Task<()>>,
    /// Set after a failed load so its own save cannot overwrite the file with defaults
    suppress_next_save: bool,
    /// Event subscriptions
    _subscriptions: SmallVec<[Subscription; 2]>,
}

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
                            // Notify the handle too, so observers of AppSettings
                            // react to the change
                            cx.notify();
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
    pub nip4e_rotation_days: Option<u32>,
    pub hide_group_notice: bool,
    pub file_server: Url,
    pub panel_background: bool,
    pub panel_background_path: Option<PathBuf>,
    pub theme_light: Option<String>,
    pub theme_dark: Option<String>,
    pub wallet: bool,
    pub onboarded: bool,
}

setting_getters! {
    pub theme: ThemeSettings,
    pub pinned_rooms: Vec<u64>,
    pub pinned_groups: Vec<String>,
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
            if let Err(error) = entity.update(cx, |this, cx| this.load(cx)) {
                log::error!("failed to load settings: {error:#}");
            }
        });

        Self {
            inner,
            #[cfg(not(target_arch = "wasm32"))]
            save_task: None,
            suppress_next_save: false,
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

    /// Edit the theme tuning
    pub fn update_theme(edit: impl FnOnce(&mut ThemeSettings), cx: &mut App) {
        Self::global(cx).update(cx, |this, cx| {
            this.inner.update(cx, |inner, cx| {
                edit(&mut inner.theme);
                cx.notify();
            });
            // Notify the handle too, so observers of AppSettings react
            cx.notify();
        });
    }

    pub fn apply_theme_style(cx: &mut App) {
        Self::apply_theme_tuning(Self::get_theme(cx), cx);
    }

    fn apply_theme_tuning(tuning: ThemeSettings, cx: &mut App) {
        {
            let styled = Theme::global_mut(cx);
            styled.font_size = px(tuning.font_size);
            styled.focus_ring = tuning.focus_ring;
            styled.shadow = tuning.shadow;
            if let Some(radius) = tuning.radius {
                styled.radius = px(radius);
            }
            if let Some(radius_lg) = tuning.radius_lg {
                styled.radius_lg = px(radius_lg);
            }
        }

        Theme::update(cx, |styled| {
            let background = styled.background;
            let foreground = styled.foreground;
            let (hover_alpha, active_alpha) = if styled.is_dark() {
                (0.08, 0.12)
            } else {
                (0.04, 0.06)
            };
            if styled.list_hover == background {
                styled.list_hover = foreground.alpha(hover_alpha);
            }
            if styled.tab_active == background {
                styled.tab_active = foreground.alpha(active_alpha);
            }
        });
    }

    /// Load settings
    fn load(&mut self, cx: &mut Context<Self>) {
        let task: Task<Result<Option<Settings>, Error>> = cx.background_spawn(async move {
            #[cfg(not(target_arch = "wasm32"))]
            {
                let path = config_dir().join(".settings");
                match smol::fs::read_to_string(&path).await {
                    Ok(content) => {
                        let settings: Settings =
                            serde_json::from_str(&content).map_err(|error| {
                                anyhow!("failed to parse {}: {error}", path.display())
                            })?;
                        return Ok(Some(settings));
                    }
                    // No settings file yet on a fresh install, so keep the defaults
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                    Err(error) => {
                        return Err(anyhow!("failed to read {}: {error}", path.display()));
                    }
                }
            }

            #[cfg(target_arch = "wasm32")]
            {
                if let Some(content) = common::web_store::get(SETTINGS_KEY)? {
                    let settings: Settings = serde_json::from_str(&content)
                        .map_err(|error| anyhow!("failed to parse saved settings: {error}"))?;
                    return Ok(Some(settings));
                }
            }

            Ok(None)
        });

        cx.spawn(async move |this, cx| {
            let (mut settings, load_failed) = match task.await {
                Ok(settings) => (settings.unwrap_or_default(), false),
                Err(error) => {
                    log::error!("failed to load settings, keeping defaults: {error:#}");
                    (Settings::default(), true)
                }
            };

            // Move settings still pointed at the old default file server over to the new one
            if settings.file_server.host_str() == Some(LEGACY_FILE_SERVER) {
                settings.file_server = Url::parse(DEFAULT_FILE_SERVER).unwrap();
            }

            // Update settings
            if let Err(error) = this.update(cx, |this, cx| {
                // A failed load must not trigger the save that would overwrite the file
                this.suppress_next_save = load_failed;
                this.set_settings(settings, cx);
                this.apply_theme(cx);
                cx.refresh_windows();
            }) {
                log::error!("failed to apply loaded settings: {error:#}");
            }
        })
        .detach();
    }

    /// Save settings
    pub fn save(&mut self, cx: &mut Context<Self>) {
        // Consume the failed-load suppression, so the next user change saves again
        if self.suppress_next_save {
            self.suppress_next_save = false;
            return;
        }

        let settings = self.inner.read(cx);
        let content = match serde_json::to_string(settings) {
            Ok(content) => content,
            Err(error) => {
                log::error!("failed to serialize settings: {error:#}");
                return;
            }
        };

        #[cfg(not(target_arch = "wasm32"))]
        {
            let previous_task = self.save_task.take();
            self.save_task = Some(cx.background_spawn(async move {
                // Await the previous write, so saves to the file complete in order
                if let Some(previous_task) = previous_task {
                    previous_task.await;
                }

                let path = config_dir().join(".settings");
                let temporary_path = config_dir().join(".settings.tmp");

                let result: std::io::Result<()> = async {
                    smol::fs::write(&temporary_path, &content).await?;
                    smol::fs::rename(&temporary_path, &path).await
                }
                .await;

                if let Err(error) = result {
                    log::error!("failed to save settings to {}: {error}", path.display());
                }
            }));
        }

        #[cfg(target_arch = "wasm32")]
        if let Err(error) = common::web_store::set(SETTINGS_KEY, &content) {
            log::error!("failed to persist settings: {error:#}");
        }
    }

    /// Apply the persisted appearance mode and theme selection to the app
    pub fn apply_theme(&mut self, cx: &mut Context<Self>) {
        let appearance = self.inner.read(cx).appearance_mode;
        match appearance {
            AppearanceMode::System => Theme::sync_system_appearance(None, cx),
            AppearanceMode::Light => Theme::change(ThemeMode::Light, None, cx),
            AppearanceMode::Dark => Theme::change(ThemeMode::Dark, None, cx),
        }

        let (light, dark) = {
            let inner = self.inner.read(cx);
            (inner.theme_light.clone(), inner.theme_dark.clone())
        };
        apply_theme_selection(light.as_deref(), dark.as_deref(), cx);

        let tuning = self.inner.read(cx).theme;
        Self::apply_theme_tuning(tuning, cx);
    }

    /// Select a registered theme by name.
    pub fn select_theme(name: &str, cx: &mut App) {
        let Some(config) = registered_theme(name, cx) else {
            log::warn!("theme is not registered: {name}");
            return;
        };

        let dark = config.mode.is_dark();

        if dark {
            Self::update_theme_dark(Some(name.to_string()), cx);
            Self::update_appearance_mode(AppearanceMode::Dark, cx);
        } else {
            Self::update_theme_light(Some(name.to_string()), cx);
            Self::update_appearance_mode(AppearanceMode::Light, cx);
        }

        Theme::change(
            if dark {
                ThemeMode::Dark
            } else {
                ThemeMode::Light
            },
            None,
            cx,
        );
        Self::reapply_theme(cx);
    }

    /// Select the theme applied in light appearance; `None` restores the default
    pub fn set_theme_light(name: Option<String>, cx: &mut App) {
        Self::update_theme_light(name, cx);
        Self::reapply_theme(cx);
    }

    /// Select the theme applied in dark appearance; `None` restores the default
    pub fn set_theme_dark(name: Option<String>, cx: &mut App) {
        Self::update_theme_dark(name, cx);
        Self::reapply_theme(cx);
    }

    /// Re-apply the selected themes and tuning from the current settings
    fn reapply_theme(cx: &mut App) {
        let (light, dark, tuning) = AppSettings::global(cx).read_with(cx, |this, cx| {
            let inner = this.inner.read(cx);
            (
                inner.theme_light.clone(),
                inner.theme_dark.clone(),
                inner.theme,
            )
        });

        apply_theme_selection(light.as_deref(), dark.as_deref(), cx);
        Self::apply_theme_tuning(tuning, cx);
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

pub fn init(cx: &mut App) {
    AppSettings::set_global(cx.new(AppSettings::new), cx)
}

/// Register the bundled gpui-kit themes with the theme registry
pub fn init_themes(cx: &mut App) {
    let assets = cx.asset_source().clone();

    for path in assets.list("themes").unwrap_or_default() {
        let Some(content) = assets.load(&path).unwrap_or_default() else {
            log::warn!("bundled theme {path} is listed but missing");
            continue;
        };

        let Ok(content) = std::str::from_utf8(&content) else {
            log::warn!("bundled theme {path} is not valid UTF-8");
            continue;
        };

        if let Err(error) = ThemeRegistry::global_mut(cx).load_themes_from_str(content) {
            log::warn!("failed to load bundled theme {path} ({error})");
        }
    }
}

/// Read the persisted appearance mode synchronously.
fn persisted_settings() -> Option<Settings> {
    let content = persisted_content();
    content.and_then(|content| serde_json::from_str(&content).ok())
}

/// The persisted settings JSON, from the config file or browser storage
#[cfg(not(target_arch = "wasm32"))]
fn persisted_content() -> Option<String> {
    let path = config_dir().join(".settings");
    std::fs::read_to_string(path).ok()
}

#[cfg(target_arch = "wasm32")]
fn persisted_content() -> Option<String> {
    common::web_store::get(SETTINGS_KEY).ok().flatten()
}

/// Read the persisted onboarding flag synchronously.
pub fn persisted_onboarded() -> bool {
    persisted_settings().is_some_and(|settings| settings.onboarded)
}

pub fn persisted_appearance_mode() -> AppearanceMode {
    persisted_settings()
        .map(|settings| settings.appearance_mode)
        .unwrap_or_default()
}

/// Read the persisted theme tuning synchronously.
pub fn persisted_theme() -> ThemeSettings {
    persisted_settings()
        .map(|settings| settings.theme)
        .unwrap_or_default()
}

/// Read the persisted theme selection for light appearance.
pub fn persisted_theme_light() -> Option<String> {
    persisted_settings().and_then(|settings| settings.theme_light)
}

/// Read the persisted theme selection for dark appearance.
pub fn persisted_theme_dark() -> Option<String> {
    persisted_settings().and_then(|settings| settings.theme_dark)
}

/// The registered theme for a persisted theme name, if any
fn registered_theme(name: &str, cx: &App) -> Option<Rc<ThemeConfig>> {
    ThemeRegistry::global(cx).themes().get(name).cloned()
}

/// Swap the selected themes into the light/dark slots and reload the active mode.
/// An unset or unknown theme falls back to the registry's default theme.
fn apply_theme_selection(light: Option<&str>, dark: Option<&str>, cx: &mut App) {
    let light_config = light
        .and_then(|name| registered_theme(name, cx))
        .or_else(|| Some(ThemeRegistry::global(cx).default_light_theme().clone()));
    let dark_config = dark
        .and_then(|name| registered_theme(name, cx))
        .or_else(|| Some(ThemeRegistry::global(cx).default_dark_theme().clone()));

    {
        let theme = Theme::global_mut(cx);
        if let Some(config) = light_config {
            theme.light_theme = config;
        }
        if let Some(config) = dark_config {
            theme.dark_theme = config;
        }
    }

    let mode = Theme::global(cx).mode;
    Theme::change(mode, None, cx);
}

/// Apply the persisted appearance, theme selection and tuning synchronously.
pub fn apply_persisted_theme(cx: &mut App) {
    match persisted_appearance_mode() {
        AppearanceMode::System => Theme::sync_system_appearance(None, cx),
        AppearanceMode::Light => Theme::change(ThemeMode::Light, None, cx),
        AppearanceMode::Dark => Theme::change(ThemeMode::Dark, None, cx),
    }

    let light = persisted_theme_light();
    let dark = persisted_theme_dark();
    apply_theme_selection(light.as_deref(), dark.as_deref(), cx);

    AppSettings::apply_theme_tuning(persisted_theme(), cx);
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Settings written before theme tuning existed must keep loading.
    #[test]
    fn settings_without_theme_keep_defaults() {
        let settings: Settings = serde_json::from_str(
            r#"{
                "appearance_mode": "Dark",
                "hide_avatar": false,
                "screening": true,
                "wot_auto_trust": true,
                "nip4e": false,
                "trusted_relays": [],
                "file_server": "https://nostr.download/"
            }"#,
        )
        .unwrap();

        assert_eq!(settings.theme, ThemeSettings::default());
        assert_eq!(settings.theme_light, None);
        assert_eq!(settings.theme_dark, None);

        // Files written before onboarding and the wallet flag existed count
        // as onboarded with the wallet off
        assert!(settings.onboarded);
        assert!(!settings.wallet);
    }

    /// Fresh installs start un-onboarded with optional features off, and the
    /// flags round-trip through serde
    #[test]
    fn onboarding_and_wallet_persist() {
        let settings = Settings::default();
        assert!(!settings.onboarded);
        assert!(!settings.wallet);
        assert!(!settings.wot_auto_trust);
        assert!(settings.theme.shadow);

        let settings: Settings = serde_json::from_str(
            r#"{
                "appearance_mode": "Dark",
                "hide_avatar": false,
                "screening": true,
                "wot_auto_trust": true,
                "nip4e": false,
                "trusted_relays": [],
                "file_server": "https://nostr.download/",
                "onboarded": true,
                "wallet": true
            }"#,
        )
        .unwrap();

        assert!(settings.onboarded);
        assert!(settings.wallet);
    }

    /// The theme selection round-trips through serde.
    #[test]
    fn theme_selection_persists() {
        let settings: Settings = serde_json::from_str(
            r#"{
                "appearance_mode": "Dark",
                "hide_avatar": false,
                "screening": true,
                "wot_auto_trust": true,
                "nip4e": false,
                "trusted_relays": [],
                "file_server": "https://nostr.download/",
                "theme_light": "Solarized Light",
                "theme_dark": "Tokyo Night"
            }"#,
        )
        .unwrap();

        assert_eq!(settings.theme_light.as_deref(), Some("Solarized Light"));
        assert_eq!(settings.theme_dark.as_deref(), Some("Tokyo Night"));
    }
}
