use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use anyhow::{anyhow, Context as AnyhowContext, Error};
use gpui::http_client::{AsyncBody, HttpClient};
use gpui::{
    App, AppContext, AsyncApp, BackgroundExecutor, Context, Entity, Global, Subscription, Task,
};
use semver::Version;
use serde::Deserialize;
use smallvec::{smallvec, SmallVec};
use smol::fs::File;
use smol::process::Command;

const GITHUB_API_URL: &str = "https://api.github.com";
const COOP_UPDATE_EXPLANATION: &str = "COOP_UPDATE_EXPLANATION";

fn get_github_repo_owner() -> String {
    std::env::var("COOP_GITHUB_REPO_OWNER").unwrap_or_else(|_| "your-username".to_string())
}

fn get_github_repo_name() -> String {
    std::env::var("COOP_GITHUB_REPO_NAME").unwrap_or_else(|_| "your-repo".to_string())
}

fn is_flatpak_installation() -> bool {
    // Check if app is installed via Flatpak
    std::env::var("FLATPAK_ID").is_ok() || std::env::var(COOP_UPDATE_EXPLANATION).is_ok()
}

pub fn init(cx: &mut App) {
    // Skip auto-update initialization if installed via Flatpak
    if is_flatpak_installation() {
        log::info!("Skipping auto-update initialization: App is installed via Flatpak");
        return;
    }

    AutoUpdater::set_global(cx.new(AutoUpdater::new), cx);
}

struct GlobalAutoUpdater(Entity<AutoUpdater>);

impl Global for GlobalAutoUpdater {}

#[cfg(not(target_os = "windows"))]
struct InstallerDir(tempfile::TempDir);

#[cfg(not(target_os = "windows"))]
impl InstallerDir {
    async fn new() -> Result<Self, Error> {
        Ok(Self(
            tempfile::Builder::new()
                .prefix("coop-auto-update")
                .tempdir()?,
        ))
    }

    fn path(&self) -> &Path {
        self.0.path()
    }
}

#[cfg(target_os = "windows")]
struct InstallerDir(PathBuf);

#[cfg(target_os = "windows")]
impl InstallerDir {
    async fn new() -> Result<Self, Error> {
        let installer_dir = std::env::current_exe()?
            .parent()
            .context("No parent dir for Coop.exe")?
            .join("updates");

        if smol::fs::metadata(&installer_dir).await.is_ok() {
            smol::fs::remove_dir_all(&installer_dir).await?;
        }

        smol::fs::create_dir(&installer_dir).await?;

        Ok(Self(installer_dir))
    }

    fn path(&self) -> &Path {
        self.0.as_path()
    }
}

struct MacOsUnmounter<'a> {
    mount_path: PathBuf,
    background_executor: &'a BackgroundExecutor,
}

impl Drop for MacOsUnmounter<'_> {
    fn drop(&mut self) {
        let mount_path = std::mem::take(&mut self.mount_path);

        self.background_executor
            .spawn(async move {
                let unmount_output = Command::new("hdiutil")
                    .args(["detach", "-force"])
                    .arg(&mount_path)
                    .output()
                    .await;

                match unmount_output {
                    Ok(output) if output.status.success() => {
                        log::info!("Successfully unmounted the disk image");
                    }
                    Ok(output) => {
                        log::error!(
                            "Failed to unmount disk image: {:?}",
                            String::from_utf8_lossy(&output.stderr)
                        );
                    }
                    Err(error) => {
                        log::error!("Error while trying to unmount disk image: {:?}", error);
                    }
                }
            })
            .detach();
    }
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub enum AutoUpdateStatus {
    Idle,
    Checking,
    Checked { download_url: String },
    Installing,
    Updated,
    Errored { msg: Box<String> },
}

impl AsRef<AutoUpdateStatus> for AutoUpdateStatus {
    fn as_ref(&self) -> &AutoUpdateStatus {
        self
    }
}

impl AutoUpdateStatus {
    pub fn is_updating(&self) -> bool {
        matches!(self, Self::Checked { .. } | Self::Installing)
    }

    pub fn is_updated(&self) -> bool {
        matches!(self, Self::Updated)
    }

    pub fn checked(download_url: String) -> Self {
        Self::Checked { download_url }
    }

    pub fn error(e: String) -> Self {
        Self::Errored { msg: Box::new(e) }
    }
}

#[derive(Debug, Deserialize)]
pub struct GitHubRelease {
    pub tag_name: String,
    pub assets: Vec<GitHubAsset>,
}

#[derive(Debug, Deserialize)]
pub struct GitHubAsset {
    pub name: String,
    pub browser_download_url: String,
}

#[derive(Debug)]
pub struct AutoUpdater {
    /// Current status of the auto updater
    pub status: AutoUpdateStatus,

    /// Current version of the application
    pub version: Version,

    /// Event subscriptions
    _subscriptions: SmallVec<[Subscription; 1]>,

    /// Background tasks
    _tasks: SmallVec<[Task<()>; 2]>,
}

impl AutoUpdater {
    /// Retrieve the global auto updater instance
    pub fn global(cx: &App) -> Entity<Self> {
        cx.global::<GlobalAutoUpdater>().0.clone()
    }

    /// Set the global auto updater instance
    fn set_global(state: Entity<Self>, cx: &mut App) {
        cx.set_global(GlobalAutoUpdater(state));
    }

    fn new(cx: &mut Context<Self>) -> Self {
        let version = Version::parse(env!("CARGO_PKG_VERSION")).unwrap();
        let async_version = version.clone();

        let mut subscriptions = smallvec![];
        let mut tasks = smallvec![];

        tasks.push(
            // Check for updates after 2 minutes
            cx.spawn(async move |this, cx| {
                cx.background_executor()
                    .timer(Duration::from_secs(120))
                    .await;

                // Update the status to checking
                this.update(cx, |this, cx| {
                    this.set_status(AutoUpdateStatus::Checking, cx);
                })
                .ok();

                match Self::check_for_updates(async_version, cx).await {
                    Ok(download_url) => {
                        // Update the status to checked with download URL
                        this.update(cx, |this, cx| {
                            this.set_status(AutoUpdateStatus::checked(download_url), cx);
                        })
                        .ok();
                    }
                    Err(e) => {
                        log::warn!("Failed to check for updates: {e}");
                        this.update(cx, |this, cx| {
                            this.set_status(AutoUpdateStatus::Idle, cx);
                        })
                        .ok();
                    }
                }
            }),
        );

        subscriptions.push(
            // Observe the status
            cx.observe_self(|this, cx| {
                if let AutoUpdateStatus::Checked { download_url } = this.status.clone() {
                    this.download_and_install(&download_url, cx);
                }
            }),
        );

        Self {
            status: AutoUpdateStatus::Idle,
            version,
            _subscriptions: subscriptions,
            _tasks: tasks,
        }
    }

    fn set_status(&mut self, status: AutoUpdateStatus, cx: &mut Context<Self>) {
        self.status = status;
        cx.notify();
    }

    fn check_for_updates(version: Version, cx: &AsyncApp) -> Task<Result<String, Error>> {
        cx.background_spawn(async move {
            let client = reqwest::Client::new();
            let repo_owner = get_github_repo_owner();
            let repo_name = get_github_repo_name();
            let url = format!(
                "{}/repos/{}/{}/releases/latest",
                GITHUB_API_URL, repo_owner, repo_name
            );

            let response = client
                .get(&url)
                .header("User-Agent", "Coop-Auto-Updater")
                .send()
                .await
                .context("Failed to fetch GitHub releases")?;

            if !response.status().is_success() {
                return Err(anyhow!("GitHub API returned error: {}", response.status()));
            }

            let release: GitHubRelease = response
                .json()
                .await
                .context("Failed to parse GitHub release")?;

            // Parse version from tag (remove 'v' prefix if present)
            let tag_version = release.tag_name.trim_start_matches('v');
            let new_version = Version::parse(tag_version).context(format!(
                "Failed to parse version from tag: {}",
                release.tag_name
            ))?;

            if new_version > version {
                // Find the appropriate asset for the current platform
                let current_os = std::env::consts::OS;
                let asset_name = match current_os {
                    "macos" => "Coop.dmg",
                    "linux" => "coop.tar.gz",
                    "windows" => "Coop.exe",
                    _ => return Err(anyhow!("Unsupported OS: {}", current_os)),
                };

                let download_url = release
                    .assets
                    .iter()
                    .find(|asset| asset.name == asset_name)
                    .map(|asset| asset.browser_download_url.clone())
                    .context(format!(
                        "No {} asset found in release {}",
                        asset_name, release.tag_name
                    ))?;

                Ok(download_url)
            } else {
                Err(anyhow!(
                    "No update available. Current: {}, Latest: {}",
                    version,
                    new_version
                ))
            }
        })
    }

    fn download_and_install(&mut self, download_url: &str, cx: &mut Context<Self>) {
        let http_client = cx.http_client();
        let download_url = download_url.to_string();

        let task: Task<Result<(InstallerDir, PathBuf), Error>> = cx.background_spawn(async move {
            let installer_dir = InstallerDir::new().await?;
            let target_path = Self::target_path(&installer_dir).await?;

            // Download the release
            download(&download_url, &target_path, http_client).await?;

            Ok((installer_dir, target_path))
        });

        self._tasks.push(
            // Install the new release
            cx.spawn(async move |this, cx| {
                _ = this.update(cx, |this, cx| {
                    this.set_status(AutoUpdateStatus::Installing, cx);
                });

                match task.await {
                    Ok((installer_dir, target_path)) => {
                        if Self::install(installer_dir, target_path, cx).await.is_ok() {
                            // Update the status to updated
                            _ = this.update(cx, |this, cx| {
                                this.set_status(AutoUpdateStatus::Updated, cx);
                            });
                        }
                    }
                    Err(e) => {
                        // Update the status to error including the error message
                        _ = this.update(cx, |this, cx| {
                            this.set_status(AutoUpdateStatus::error(e.to_string()), cx);
                        });
                    }
                }
            }),
        );
    }

    async fn target_path(installer_dir: &InstallerDir) -> Result<PathBuf, Error> {
        let filename = match std::env::consts::OS {
            "macos" => anyhow::Ok("Coop.dmg"),
            "linux" => Ok("coop.tar.gz"),
            "windows" => Ok("Coop.exe"),
            unsupported_os => anyhow::bail!("not supported: {unsupported_os}"),
        }?;

        Ok(installer_dir.path().join(filename))
    }

    async fn install(
        installer_dir: InstallerDir,
        target_path: PathBuf,
        cx: &AsyncApp,
    ) -> Result<(), Error> {
        match std::env::consts::OS {
            "macos" => install_release_macos(&installer_dir, target_path, cx).await,
            "linux" => install_release_linux(&installer_dir, target_path, cx).await,
            "windows" => install_release_windows(target_path).await,
            unsupported_os => anyhow::bail!("Not supported: {unsupported_os}"),
        }
    }
}

async fn download(
    url: &str,
    target_path: &std::path::Path,
    client: Arc<dyn HttpClient>,
) -> Result<(), Error> {
    let body = AsyncBody::default();
    let mut target_file = File::create(&target_path).await?;
    let mut response = client.get(url, body, true).await?;

    // Copy the response body to the target file
    smol::io::copy(response.body_mut(), &mut target_file).await?;

    Ok(())
}

async fn install_release_macos(
    temp_dir: &InstallerDir,
    downloaded_dmg: PathBuf,
    cx: &AsyncApp,
) -> Result<(), Error> {
    let running_app_path = cx.update(|cx| cx.app_path())?;
    let running_app_filename = running_app_path
        .file_name()
        .with_context(|| format!("invalid running app path {running_app_path:?}"))?;

    let mount_path = temp_dir.path().join("Coop");
    let mut mounted_app_path: OsString = mount_path.join(running_app_filename).into();

    mounted_app_path.push("/");

    let output = Command::new("hdiutil")
        .args(["attach", "-nobrowse"])
        .arg(&downloaded_dmg)
        .arg("-mountroot")
        .arg(temp_dir.path())
        .output()
        .await?;

    anyhow::ensure!(
        output.status.success(),
        "failed to mount: {:?}",
        String::from_utf8_lossy(&output.stderr)
    );

    // Create an MacOsUnmounter that will be dropped (and thus unmount the disk) when this function exits
    let _unmounter = MacOsUnmounter {
        mount_path: mount_path.clone(),
        background_executor: cx.background_executor(),
    };

    let output = Command::new("rsync")
        .args(["-av", "--delete"])
        .arg(&mounted_app_path)
        .arg(&running_app_path)
        .output()
        .await?;

    anyhow::ensure!(
        output.status.success(),
        "failed to copy app: {:?}",
        String::from_utf8_lossy(&output.stderr)
    );

    Ok(())
}

async fn install_release_linux(
    temp_dir: &InstallerDir,
    downloaded_tar_gz: PathBuf,
    cx: &AsyncApp,
) -> Result<(), Error> {
    let running_app_path = cx.update(|cx| cx.app_path())?;

    // Extract the tar.gz file
    let extracted = temp_dir.path().join("coop");
    smol::fs::create_dir_all(&extracted)
        .await
        .context("failed to create directory to extract update")?;

    let output = Command::new("tar")
        .arg("-xzf")
        .arg(&downloaded_tar_gz)
        .arg("-C")
        .arg(&extracted)
        .output()
        .await?;

    anyhow::ensure!(
        output.status.success(),
        "failed to extract {:?} to {:?}: {:?}",
        downloaded_tar_gz,
        extracted,
        String::from_utf8_lossy(&output.stderr)
    );

    // Find the extracted app directory
    let mut entries = smol::fs::read_dir(&extracted).await?;
    let mut app_dir = None;

    use smol::stream::StreamExt;

    while let Some(entry) = entries.next().await {
        let entry = entry?;
        let path = entry.path();
        if path.is_dir() {
            app_dir = Some(path);
            break;
        }
    }

    let from = app_dir.context("No app directory found in archive")?;

    // Copy to the current installation directory
    let output = Command::new("rsync")
        .args(["-av", "--delete"])
        .arg(&from)
        .arg(
            running_app_path
                .parent()
                .context("No parent directory for app")?,
        )
        .output()
        .await?;

    anyhow::ensure!(
        output.status.success(),
        "failed to copy app from {:?} to {:?}: {:?}",
        from,
        running_app_path.parent(),
        String::from_utf8_lossy(&output.stderr)
    );

    Ok(())
}

async fn install_release_windows(downloaded_installer: PathBuf) -> Result<(), Error> {
    //const CREATE_NO_WINDOW: u32 = 0x08000000;

    let system_root = std::env::var("SYSTEMROOT");
    let powershell_path = system_root.as_ref().map_or_else(
        |_| "powershell.exe".to_string(),
        |p| format!("{p}\\System32\\WindowsPowerShell\\v1.0\\powershell.exe"),
    );

    let mut installer_path = std::ffi::OsString::new();
    installer_path.push("\"");
    installer_path.push(&downloaded_installer);
    installer_path.push("\"");

    let output = Command::new(powershell_path)
        //.creation_flags(CREATE_NO_WINDOW)
        .args(["-NoProfile", "-WindowStyle", "Hidden"])
        .args(["Start-Process"])
        .arg(installer_path)
        .arg("-ArgumentList")
        .args(["/P", "/R"])
        .output()
        .await?;

    anyhow::ensure!(
        output.status.success(),
        "failed to start installer: {:?}",
        String::from_utf8_lossy(&output.stderr)
    );

    Ok(())
}
