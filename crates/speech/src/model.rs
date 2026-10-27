use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::{fs, io};

use anyhow::{Result, bail};
use serde::Deserialize;

/// How the model is currently available.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Status {
    NotInstalled,
    Downloading { downloaded: u64, total: u64 },
    Installed,
}

impl Status {
    pub fn is_downloading(self) -> bool {
        matches!(self, Status::Downloading { .. })
    }
}

#[derive(Debug, Deserialize)]
struct Manifest {
    repository: String,
    revision: String,
    files: Vec<Artifact>,
}

#[derive(Debug, Deserialize)]
struct Artifact {
    name: String,
    size: u64,
    sha256: String,
}

fn manifest() -> &'static Manifest {
    static MANIFEST: OnceLock<Manifest> = OnceLock::new();

    MANIFEST.get_or_init(|| {
        serde_json::from_str(include_str!("model.json")).expect("pinned model manifest")
    })
}

/// The directory the model artifacts are downloaded into.
pub fn dir() -> PathBuf {
    common::config_dir()
        .join("speech")
        .join("parakeet-tdt-0.6b-v3")
}

/// Total download size in bytes.
pub fn total_size() -> u64 {
    manifest().files.iter().map(|file| file.size).sum()
}

/// The revision of the installed model, if its artifacts are complete.
fn installed_revision() -> Option<String> {
    let manifest = manifest();
    let dir = dir();

    let complete = manifest.files.iter().all(|file| {
        fs::metadata(dir.join(&file.name)).is_ok_and(|metadata| metadata.len() == file.size)
    });

    if complete
        && fs::read_to_string(dir.join("verified"))
            .is_ok_and(|revision| revision == manifest.revision)
    {
        Some(manifest.revision.clone())
    } else {
        None
    }
}

/// Whether the model is ready to load.
pub fn installed() -> bool {
    status() == Status::Installed
}

/// The current status, reconciling the download state with the disk.
pub fn status() -> Status {
    let state = state().lock().unwrap_or_else(|error| error.into_inner());

    if let Download::Running {
        downloaded, total, ..
    } = state.download
    {
        return Status::Downloading { downloaded, total };
    }

    if installed_revision().is_some() {
        Status::Installed
    } else {
        Status::NotInstalled
    }
}

/// Take the last download failure, if it has not been reported yet.
pub fn take_error() -> Option<String> {
    let mut state = state().lock().unwrap_or_else(|error| error.into_inner());
    state.error.take()
}

/// Start downloading the model, unless a download is already running.
pub fn start_download() {
    let mut state = state().lock().unwrap_or_else(|error| error.into_inner());

    if matches!(state.download, Download::Running { .. }) {
        return;
    }

    let cancel = Arc::new(AtomicBool::new(false));
    state.error = None;
    state.download = Download::Running {
        downloaded: 0,
        total: total_size(),
        cancel: cancel.clone(),
    };

    if let Err(error) = std::thread::Builder::new()
        .name("speech-model-download".into())
        .spawn(move || download(cancel))
    {
        state.download = Download::Idle;
        state.error = Some(error.to_string());
    }
}

/// Request cancellation of the running download.
pub fn cancel_download() {
    let state = state().lock().unwrap_or_else(|error| error.into_inner());

    if let Download::Running { cancel, .. } = &state.download {
        cancel.store(true, Ordering::Release);
    }
}

/// Delete the downloaded model and any partial download.
pub fn delete() {
    super::parakeet::unload();
    cancel_download();

    if let Err(error) = fs::remove_dir_all(dir())
        && error.kind() != io::ErrorKind::NotFound
    {
        log::warn!("failed to remove the speech model: {error}");
    }
}

fn download(cancel: Arc<AtomicBool>) {
    let result = download_inner(&cancel);

    let mut state = state().lock().unwrap_or_else(|error| error.into_inner());
    state.download = Download::Idle;

    // A cancelled download must not report a spurious failure.
    if !cancelled(&cancel)
        && let Err(error) = result
    {
        state.error = Some(format!("{error:#}"));
    }
}

fn download_inner(cancel: &AtomicBool) -> Result<()> {
    let manifest = manifest();
    let dir = dir();
    let client = reqwest::blocking::Client::builder()
        .connect_timeout(std::time::Duration::from_secs(20))
        .build()?;

    fs::create_dir_all(&dir)?;

    for file in &manifest.files {
        if cancelled(cancel) {
            bail!("Cancelled");
        }

        let staging = dir.join(format!("{}.part", file.name));
        let result = download_file(&client, manifest, file, &staging, cancel);

        if result.is_err() || cancelled(cancel) {
            let _unused = fs::remove_file(&staging);
            result?;
            bail!("Cancelled");
        }
    }

    fs::write(dir.join("verified"), &manifest.revision)?;
    Ok(())
}

fn download_file(
    client: &reqwest::blocking::Client,
    manifest: &Manifest,
    file: &Artifact,
    staging: &Path,
    cancel: &AtomicBool,
) -> Result<()> {
    use std::io::{Read as _, Write as _};

    use sha2::Digest as _;

    let url = format!(
        "https://huggingface.co/{}/resolve/{}/{}",
        manifest.repository, manifest.revision, file.name
    );

    let mut response = client.get(url).send()?.error_for_status()?;
    let mut out = fs::File::create(staging)?;
    let mut hash = sha2::Sha256::new();
    let mut size = 0u64;

    loop {
        if cancelled(cancel) {
            bail!("Cancelled");
        }

        let mut chunk = [0u8; 64 * 1024];
        let read = response.read(&mut chunk)?;

        if read == 0 {
            break;
        }

        size += read as u64;
        if size > file.size {
            bail!("Unexpected model size");
        }

        hash.update(&chunk[..read]);
        out.write_all(&chunk[..read])?;

        let mut state = state().lock().unwrap_or_else(|error| error.into_inner());
        if let Download::Running { downloaded, .. } = &mut state.download {
            *downloaded += read as u64;
        }
    }

    if size != file.size || format!("{:x}", hash.finalize()) != file.sha256 {
        bail!("Model checksum verification failed");
    }

    out.sync_all()?;
    fs::rename(staging, dir().join(&file.name))?;
    Ok(())
}

fn cancelled(cancel: &AtomicBool) -> bool {
    cancel.load(Ordering::Acquire)
}

enum Download {
    Idle,
    Running {
        downloaded: u64,
        total: u64,
        cancel: Arc<AtomicBool>,
    },
}

struct State {
    download: Download,
    error: Option<String>,
}

fn state() -> &'static Mutex<State> {
    static STATE: Mutex<State> = Mutex::new(State {
        download: Download::Idle,
        error: None,
    });

    &STATE
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn manifest_pins_v3_artifacts_with_exact_sizes() {
        let manifest = manifest();

        assert_eq!(manifest.repository, "istupakov/parakeet-tdt-0.6b-v3-onnx");
        assert_eq!(manifest.revision.len(), 40);
        assert_eq!(manifest.files.len(), 3);
        assert_eq!(total_size(), 670_479_942);
        assert!(
            manifest
                .files
                .iter()
                .all(|file| file.sha256.len() == 64 && !file.name.contains('/'))
        );
    }
}
