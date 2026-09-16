#![cfg(not(target_arch = "wasm32"))]

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use gpui::{App, AppContext, Context, Entity, Global, SharedString, Task, Window};
use gpui_updater_core::{EngineConfig, Release, UpdateEngine, UpdateStatus, Verification, Version};
use instant::Duration;

use crate::source::{AssetFilter, GiteaSource, asset_filter_for};

mod source;

pub use gpui_updater_core::UpdateStatus as AutoUpdateStatus;

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
        || std::env::var(COOP_BUNDLE_TYPE).is_ok_and(|value| value == "snap")
}

/// Initialize the auto-update system.
pub fn init(window: &mut Window, cx: &mut App) {
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

    AutoUpdater::set_global(
        cx.new(|cx| AutoUpdater::new(window, version, filter, cx)),
        cx,
    );
}

struct GlobalAutoUpdater(Entity<AutoUpdater>);

impl Global for GlobalAutoUpdater {}

pub struct AutoUpdater {
    /// The blocking engine, driven on the background executor.
    engine: Arc<UpdateEngine<GiteaSource>>,
    status: UpdateStatus,
    /// The newer release found by the last successful check, if any.
    available: Option<Release>,
    /// Currently running app version.
    pub version: Version,
    /// The in-flight check or download, if any.
    task: Option<Task<()>>,
}

impl AutoUpdater {
    /// Whether auto-update is available for this installation.
    pub fn is_available(cx: &App) -> bool {
        cx.try_global::<GlobalAutoUpdater>().is_some()
    }

    /// Retrieve the global auto updater instance, if one was initialized.
    pub fn try_global(cx: &App) -> Option<Entity<Self>> {
        cx.try_global::<GlobalAutoUpdater>()
            .map(|global| global.0.clone())
    }

    /// Retrieve the global auto updater instance.
    pub fn global(cx: &App) -> Entity<Self> {
        cx.global::<GlobalAutoUpdater>().0.clone()
    }

    fn set_global(state: Entity<Self>, cx: &mut App) {
        cx.set_global(GlobalAutoUpdater(state));
    }

    fn new(
        window: &mut Window,
        version: Version,
        filter: AssetFilter,
        cx: &mut Context<Self>,
    ) -> Self {
        let source = GiteaSource::new(GITEA_API_BASE, GITEA_REPO_OWNER, GITEA_REPO_NAME, filter);
        let config = EngineConfig::new(version.clone()).verification(Verification::Checksum);
        let engine = Arc::new(UpdateEngine::new(source, config));

        // Schedule an auto-check after a 2-minute delay
        cx.defer_in(window, |_this, _window, cx| {
            cx.spawn(async move |this, cx| {
                cx.background_executor().timer(AUTO_CHECK_DELAY).await;
                this.update(cx, |this, cx| this.check(cx)).ok();
            })
            .detach();
        });

        Self {
            engine,
            status: UpdateStatus::Idle,
            available: None,
            version,
            task: None,
        }
    }

    /// Whether nothing is happening, so the UI can hide the status line.
    pub fn idle(&self) -> bool {
        matches!(self.status, UpdateStatus::Idle)
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
        if self.status.is_busy() {
            return;
        }
        self.set_status(UpdateStatus::Checking, cx);

        let engine = self.engine.clone();

        self.task = Some(cx.spawn(async move |this, cx| {
            let result = cx
                .background_executor()
                .spawn(async move { engine.check() })
                .await;

            this.update(cx, |this, cx| {
                this.task = None;
                match result {
                    Ok(Some(release)) => {
                        log::info!("Update {} is available", release.version);
                        let version = release.version.clone();
                        this.available = Some(release);
                        this.set_status(UpdateStatus::Available(version), cx);
                        this.download_and_install(cx);
                    }
                    Ok(None) => this.set_status(UpdateStatus::UpToDate, cx),
                    Err(error) => {
                        log::warn!("Update check failed: {error}");
                        this.set_status(UpdateStatus::Errored(error.to_string()), cx);
                    }
                }
            })
            .ok();
        }));
    }

    /// Download the available update, verify it, and swap it into place.
    fn download_and_install(&mut self, cx: &mut Context<Self>) {
        if self.status.is_busy() {
            return;
        }
        let Some(release) = self.available.clone() else {
            return;
        };

        let engine = self.engine.clone();
        self.set_status(
            UpdateStatus::Downloading {
                downloaded: 0,
                total: None,
            },
            cx,
        );

        self.task = Some(cx.spawn(async move |this, cx| {
            let downloaded = Arc::new(AtomicU64::new(0));
            let total = Arc::new(AtomicU64::new(0)); // 0 = unknown
            let done = Arc::new(AtomicBool::new(false));

            let download_task = {
                let (engine, release) = (engine.clone(), release.clone());
                let (downloaded, total, done) = (downloaded.clone(), total.clone(), done.clone());
                cx.background_executor().spawn(async move {
                    let result = engine.download(&release, |got, expected| {
                        downloaded.store(got, Ordering::Relaxed);
                        total.store(expected.unwrap_or(0), Ordering::Relaxed);
                    });
                    done.store(true, Ordering::Relaxed);
                    result
                })
            };

            loop {
                let got = downloaded.load(Ordering::Relaxed);
                let total = total.load(Ordering::Relaxed);
                this.update(cx, |this, cx| {
                    this.set_status(
                        UpdateStatus::Downloading {
                            downloaded: got,
                            total: (total != 0).then_some(total),
                        },
                        cx,
                    );
                })
                .ok();
                if done.load(Ordering::Relaxed) {
                    break;
                }
                cx.background_executor()
                    .timer(Duration::from_millis(120))
                    .await;
            }

            let artifact = match download_task.await {
                Ok(artifact) => artifact,
                Err(error) => {
                    log::warn!("Update download failed: {error}");
                    this.update(cx, |this, cx| {
                        this.task = None;
                        this.set_status(UpdateStatus::Errored(error.to_string()), cx);
                    })
                    .ok();
                    return;
                }
            };

            let _ = this.update(cx, |this, cx| this.set_status(UpdateStatus::Installing, cx));

            let installed = {
                let engine = engine.clone();
                cx.background_executor()
                    .spawn(async move { engine.install(&artifact) })
                    .await
            };

            this.update(cx, |this, cx| {
                this.task = None;
                match installed {
                    Ok(installed) => {
                        if let Some(path) = installed.restart_path {
                            cx.set_restart_path(path);
                        }
                        let version = release.version.clone();
                        this.set_status(UpdateStatus::Staged(version), cx);
                    }
                    Err(error) => {
                        this.set_status(UpdateStatus::Errored(error.to_string()), cx);
                    }
                }
            })
            .ok();
        }));
    }

    /// Relaunch into the staged update.
    pub fn restart(&mut self, cx: &mut Context<Self>) {
        if !self.staged() {
            log::warn!("Ignoring restart request: no update is staged");
            return;
        }
        cx.restart();
    }

    fn set_status(&mut self, status: UpdateStatus, cx: &mut Context<Self>) {
        let errored = matches!(status, UpdateStatus::Errored(_));
        self.status = status;

        if errored {
            cx.spawn(async move |this, cx| {
                cx.background_executor().timer(ERROR_DISPLAY_DURATION).await;
                this.update(cx, |this, cx| {
                    this.set_status(UpdateStatus::Idle, cx);
                })
                .ok();
            })
            .detach();
        }

        cx.notify();
    }
}
