#![cfg(not(target_arch = "wasm32"))]

use std::time::Duration;

use gpui::{App, AppContext, Context, Entity, Global, SharedString, Task};
use gpui_updater_pre::{EngineConfig, UpdateStatus, Updater, Verification, Version};

use crate::source::{AssetFilter, GiteaSource, asset_filter_for};

mod source;

const GITEA_API_BASE: &str = "https://git.reya.info/api/v1";
const GITEA_REPO_OWNER: &str = "reya";
const GITEA_REPO_NAME: &str = "coop";

/// Delay before the automatic check that runs on startup.
const AUTO_CHECK_DELAY: Duration = Duration::from_secs(120);
/// How long a failure stays visible before the status reverts to "Up to date".
const ERROR_DISPLAY_DURATION: Duration = Duration::from_secs(5);

const COOP_UPDATE_EXPLANATION: &str = "COOP_UPDATE_EXPLANATION";
const COOP_BUNDLE_TYPE: &str = "COOP_BUNDLE_TYPE";

fn uses_managed_updates() -> bool {
    // The Flatpak runtime exports `FLATPAK_ID` inside the sandbox.
    std::env::var("FLATPAK_ID").is_ok()
        // Allow opting out of in-app updates via an explicit environment variable.
        || std::env::var(COOP_UPDATE_EXPLANATION).is_ok()
        // The Snap package sets `COOP_BUNDLE_TYPE=snap` (see snapcraft.yaml.in).
        || std::env::var(COOP_BUNDLE_TYPE).is_ok_and(|value | value == "snap")
}

/// Initialize the auto-update system.
pub fn init(cx: &mut App) {
    if uses_managed_updates() {
        log::info!(
            "Skipping auto-update initialization: updates are managed by the installed distribution channel (Flatpak/Snap)"
        );
        return;
    }

    let (os, arch) = (std::env::consts::OS, std::env::consts::ARCH);

    let Some(filter) = asset_filter_for(os, arch) else {
        log::info!(
            "Skipping auto-update initialization: no installable release artifact is published for {os}/{arch}"
        );
        return;
    };

    let Ok(version) = Version::parse(env!("CARGO_PKG_VERSION")) else {
        log::error!(
            "Skipping auto-update initialization: crate version {:?} is not valid semver",
            env!("CARGO_PKG_VERSION")
        );
        return;
    };

    AutoUpdater::set_global(cx.new(|cx| AutoUpdater::new(version, filter, cx)), cx);
}

struct GlobalAutoUpdater(Entity<AutoUpdater>);

impl Global for GlobalAutoUpdater {}

pub struct AutoUpdater {
    updater: Entity<Updater>,
    status: UpdateStatus,
    /// The pending revert of an `Errored` status back to `Idle`, if any.
    error_revert: Option<Task<()>>,
}

impl AutoUpdater {
    /// Retrieve the global auto updater instance, if one was initialized.
    pub fn try_global(cx: &App) -> Option<Entity<Self>> {
        cx.try_global::<GlobalAutoUpdater>()
            .map(|global| global.0.clone())
    }

    fn set_global(state: Entity<Self>, cx: &mut App) {
        cx.set_global(GlobalAutoUpdater(state));
    }

    fn new(version: Version, filter: AssetFilter, cx: &mut Context<Self>) -> Self {
        let source = GiteaSource::new(
            GITEA_API_BASE,
            GITEA_REPO_OWNER,
            GITEA_REPO_NAME,
            filter,
            cx.http_client(),
        );
        let config = EngineConfig::new(version).verification(Verification::Checksum);
        let updater = cx.new(|cx| Updater::new(source, config, cx));

        cx.observe(&updater, |this, _, cx| this.observe_status(cx))
            .detach();

        // Schedule an auto-check after a 2-minute delay
        let entity = cx.entity().downgrade();
        cx.defer(move |cx| {
            cx.spawn(async move |cx| {
                cx.background_executor().timer(AUTO_CHECK_DELAY).await;
                entity.update(cx, |this, cx| this.check(cx)).ok();
            })
            .detach();
        });

        Self {
            updater,
            status: UpdateStatus::Idle,
            error_revert: None,
        }
    }

    fn observe_status(&mut self, cx: &mut Context<Self>) {
        let status = self.updater.read(cx).status().clone();
        self.status = status.clone();

        self.error_revert = if matches!(status, UpdateStatus::Errored(_)) {
            Some(cx.spawn(async move |this, cx| {
                cx.background_executor().timer(ERROR_DISPLAY_DURATION).await;
                this.update(cx, |this, cx| {
                    if matches!(this.status, UpdateStatus::Errored(_)) {
                        this.status = UpdateStatus::Idle;
                        cx.notify();
                    }
                })
                .ok();
            }))
        } else {
            None
        };

        // The adapter stops at `Available`; continue into the download.
        if matches!(status, UpdateStatus::Available(_)) {
            self.updater.update(cx, |updater, cx| {
                updater.download_and_install(cx);
            });
        }

        cx.notify();
    }

    /// Whether the running version is the newest release, so the status line can be hidden.
    pub fn up_to_date(&self) -> bool {
        matches!(self.status, UpdateStatus::Idle | UpdateStatus::UpToDate)
    }

    /// Whether a verified update is installed and waiting for a restart.
    pub fn staged(&self) -> bool {
        matches!(self.status, UpdateStatus::Staged(_))
    }

    /// A short, human-readable description of the current status.
    pub fn status(&self) -> SharedString {
        match &self.status {
            UpdateStatus::Idle | UpdateStatus::UpToDate => "Up to date".into(),
            UpdateStatus::Checking => "Checking for updates…".into(),
            UpdateStatus::Available(version) => format!("Version {version} available").into(),
            UpdateStatus::Downloading { downloaded, total } => {
                let total_mb = total.map(|t| t as f64 / 1_048_576.0);
                let downloaded_mb = *downloaded as f64 / 1_048_576.0;
                match total_mb {
                    Some(t) => format!("Downloading {downloaded_mb:.1} / {t:.1} MB").into(),
                    None => format!("Downloading {downloaded_mb:.1} MB").into(),
                }
            }
            UpdateStatus::Installing => "Installing update…".into(),
            UpdateStatus::Staged(version) => {
                format!("Version {version} ready — restart to apply").into()
            }
            UpdateStatus::Errored(message) => format!("Update failed: {message}").into(),
        }
    }

    /// Check the release host for a newer version, then download and install it.
    pub fn check(&mut self, cx: &mut Context<Self>) {
        self.updater.update(cx, |updater, cx| {
            updater.check(cx);
        });
    }

    /// Relaunch into the staged update.
    pub fn restart(&mut self, cx: &mut Context<Self>) {
        if !self.staged() {
            log::warn!("Ignoring restart request: no update is staged");
            return;
        }
        self.updater.update(cx, |updater, cx| {
            updater.restart(cx);
        });
    }
}
