#![cfg(not(target_arch = "wasm32"))]

use gpui::{App, AppContext, Context, Entity, Global, SharedString, Subscription, Window};
use gpui_updater::{EngineConfig, GitHubSource, UpdateStatus, Updater, Version};
use instant::{Duration, Instant};

const COOP_UPDATE_EXPLANATION: &str = "COOP_UPDATE_EXPLANATION";

fn get_github_repo_owner() -> String {
    std::env::var("COOP_GITHUB_REPO_OWNER").unwrap_or_else(|_| "reyakov".to_string())
}

fn get_github_repo_name() -> String {
    std::env::var("COOP_GITHUB_REPO_NAME").unwrap_or_else(|_| "coop".to_string())
}

fn is_flatpak_installation() -> bool {
    std::env::var("FLATPAK_ID").is_ok() || std::env::var(COOP_UPDATE_EXPLANATION).is_ok()
}

/// Initialize the auto-update system.
///
/// Skips initialization when running as a Flatpak (updates are handled by the
/// Flatpak distribution channel). Otherwise creates the global [`AutoUpdater`]
/// entity and schedules a check for updates after a 2-minute delay.
pub fn init(window: &mut Window, cx: &mut App) {
    if is_flatpak_installation() {
        log::info!("Skipping auto-update initialization: App is installed via Flatpak");
        return;
    }

    AutoUpdater::set_global(cx.new(|cx| AutoUpdater::new(window, cx)), cx);
}

struct GlobalAutoUpdater(Entity<AutoUpdater>);

impl Global for GlobalAutoUpdater {}

/// Observable auto-update status — re-exported from [`gpui_updater::UpdateStatus`].
pub use gpui_updater::UpdateStatus as AutoUpdateStatus;

/// The global auto-updater entity.
///
/// Wraps [`gpui_updater::Updater`] with Coop-specific configuration
/// (GitHub repo, Flatpak detection, delayed auto-check).
///
/// Retrieve the global instance via [`AutoUpdater::global`].
pub struct AutoUpdater {
    /// The underlying gpui-updater entity that does the heavy lifting.
    pub updater: Entity<Updater>,
    /// Currently running app version.
    pub version: Version,
    /// Keeps the observer subscription alive.
    _subscription: Subscription,
    /// When the last error was recorded, so we can reset to idle after 5s.
    error_time: Option<Instant>,
}

impl AutoUpdater {
    /// Retrieve the global auto updater instance.
    pub fn global(cx: &App) -> Entity<Self> {
        cx.global::<GlobalAutoUpdater>().0.clone()
    }

    fn set_global(state: Entity<Self>, cx: &mut App) {
        cx.set_global(GlobalAutoUpdater(state));
    }

    fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let version = Version::parse(env!("CARGO_PKG_VERSION")).unwrap();

        let repo_owner = get_github_repo_owner();
        let repo_name = get_github_repo_name();

        let source =
            GitHubSource::new(&repo_owner, &repo_name).asset_contains(match std::env::consts::OS {
                "macos" => "macos",
                "linux" => "linux",
                _ => "",
            });

        let updater: Entity<Updater> =
            cx.new(|cx| Updater::new(source, EngineConfig::new(version.clone()), cx));

        // When an update becomes available, automatically download and install it.
        let subscription = cx.observe(&updater, |this: &mut AutoUpdater, _updater, cx| {
            let status = this.updater.read(cx).status().clone();

            if matches!(status, UpdateStatus::Available(_)) {
                this.updater.update(cx, |updater, cx| {
                    updater.download_and_install(cx);
                });
            }

            if matches!(status, UpdateStatus::Errored(_)) {
                this.error_time = Some(Instant::now());
                cx.spawn(async move |this, cx| {
                    cx.background_executor().timer(Duration::from_secs(5)).await;
                    this.update(cx, |_this, cx| cx.notify()).ok();
                })
                .detach();
            } else {
                this.error_time = None;
            }

            cx.notify();
        });

        // Schedule an auto-check after a 2-minute delay (deferred to run at the
        // end of the current frame so the window is fully set up).
        cx.defer_in(window, |_this, _window, cx| {
            let duration = Duration::from_secs(120);
            cx.spawn(async move |this, cx| {
                cx.background_executor().timer(duration).await;
                this.update(cx, |this, cx| {
                    this.updater.update(cx, |updater, cx| {
                        updater.check(cx);
                    });
                })
                .ok();
            })
            .detach();
        });

        Self {
            updater,
            version,
            _subscription: subscription,
            error_time: None,
        }
    }

    pub fn idle(&self, cx: &App) -> bool {
        let status = self.updater.read(cx).status();
        if status == &UpdateStatus::Idle {
            return true;
        }
        if matches!(status, UpdateStatus::Errored(_))
            && self
                .error_time
                .is_some_and(|t| t.elapsed() >= Duration::from_secs(5))
        {
            return true;
        }
        false
    }

    pub fn status(&self, cx: &App) -> SharedString {
        let status = self.updater.read(cx).status();

        match status {
            UpdateStatus::Idle => "Up to date".into(),
            UpdateStatus::Checking => "Checking for updates…".into(),
            UpdateStatus::UpToDate => "Up to date".into(),
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
            UpdateStatus::Errored(msg) => {
                if self
                    .error_time
                    .is_some_and(|t| t.elapsed() >= Duration::from_secs(5))
                {
                    "Up to date".into()
                } else {
                    format!("Update failed: {msg}").into()
                }
            }
        }
    }
}
