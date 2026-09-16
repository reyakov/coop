use gpui_updater_core::{Asset, Error, Release, Result, UpdateSource, parse_tag};
use serde::Deserialize;

const CHECKSUMS_ASSET: &str = "SHA256SUMS";
const RELEASE_PAGE_SIZE: usize = 20;

/// Which published artifact belongs to a target platform.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AssetFilter {
    extension: &'static str,
    arch: &'static str,
}

impl AssetFilter {
    /// Whether `name` is the installable artifact for this target.
    fn matches(&self, name: &str) -> bool {
        let name = name.to_ascii_lowercase();
        name.ends_with(self.extension) && name.contains(self.arch)
    }
}

pub fn asset_filter_for(os: &str, arch: &str) -> Option<AssetFilter> {
    let extension = match os {
        "macos" => ".dmg",
        "linux" => ".tar.gz",
        _ => return None,
    };

    let arch = match (os, arch) {
        // cargo-packager names the disk images `aarch64`/`x64`.
        ("macos", "aarch64") => "aarch64",
        ("macos", "x86_64") => "x64",
        // `script/bundle-linux` names the tarballs `aarch64`/`x86_64`.
        ("linux", "aarch64") => "aarch64",
        ("linux", "x86_64") => "x86_64",
        _ => return None,
    };

    Some(AssetFilter { extension, arch })
}

/// Reads releases from a Gitea repository's Releases.
pub struct GiteaSource {
    api_base: String,
    owner: String,
    repo: String,
    filter: AssetFilter,
}

impl GiteaSource {
    /// Build a source for `owner/repo` on the Gitea instance at `api_base`
    /// (e.g. `https://git.reya.info/api/v1`).
    pub fn new(
        api_base: impl Into<String>,
        owner: impl Into<String>,
        repo: impl Into<String>,
        filter: AssetFilter,
    ) -> Self {
        Self {
            api_base: api_base.into().trim_end_matches('/').to_string(),
            owner: owner.into(),
            repo: repo.into(),
            filter,
        }
    }

    fn releases_url(&self) -> String {
        format!(
            "{}/repos/{}/{}/releases?limit={RELEASE_PAGE_SIZE}",
            self.api_base, self.owner, self.repo
        )
    }
}

impl UpdateSource for GiteaSource {
    fn fetch_latest(&self) -> Result<Release> {
        let releases: Vec<GiteaRelease> = http::get_json(&self.releases_url())?;
        let release = newest_published(&releases)
            .ok_or_else(|| Error::Parse("repository has no published releases".to_string()))?;

        let asset = release
            .assets
            .iter()
            .find(|asset| self.filter.matches(&asset.name))
            .ok_or(Error::NoMatchingAsset {
                target_os: std::env::consts::OS,
                target_arch: std::env::consts::ARCH,
            })?;

        // Resolve the published checksum so the engine can reject a truncated or substituted download.
        let sha256 = release
            .assets
            .iter()
            .find(|candidate| candidate.name.eq_ignore_ascii_case(CHECKSUMS_ASSET))
            .map(|sums| http::get_string(&sums.browser_download_url))
            .transpose()?
            .and_then(|sums| sha256_for(&sums, &asset.name));

        Ok(Release {
            version: parse_tag(&release.tag_name)?,
            notes: release
                .body
                .clone()
                .filter(|body| !body.trim().is_empty())
                .or_else(|| release.name.clone()),
            asset: Asset {
                name: asset.name.clone(),
                url: asset.browser_download_url.clone(),
                size: asset.size,
            },
            signature: None,
            signature_url: None,
            sha256,
        })
    }
}

fn newest_published(releases: &[GiteaRelease]) -> Option<&GiteaRelease> {
    releases
        .iter()
        .filter(|release| !release.draft && !release.prerelease)
        .filter_map(|release| {
            parse_tag(&release.tag_name)
                .ok()
                .map(|version| (version, release))
        })
        .max_by(|(left, _), (right, _)| left.cmp(right))
        .map(|(_, release)| release)
}

/// The SHA-256 recorded for `asset_name` in a `shasum`-style checksums file.
fn sha256_for(sums: &str, asset_name: &str) -> Option<String> {
    sums.lines().find_map(|line| {
        let mut parts = line.split_whitespace();
        let (hash, path) = (parts.next()?, parts.next()?);
        let path = path.strip_prefix('*').unwrap_or(path);
        let base = path.rsplit(['/', '\\']).next().unwrap_or(path);
        (base == asset_name).then(|| hash.to_ascii_lowercase())
    })
}

/// A release as returned by the Gitea API.
#[derive(Debug, Deserialize)]
struct GiteaRelease {
    tag_name: String,
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    body: Option<String>,
    #[serde(default)]
    draft: bool,
    #[serde(default)]
    prerelease: bool,
    #[serde(default)]
    assets: Vec<GiteaAsset>,
}

/// A release asset as returned by the Gitea API.
#[derive(Debug, Deserialize)]
struct GiteaAsset {
    name: String,
    browser_download_url: String,
    #[serde(default)]
    size: u64,
}

/// Blocking HTTP helpers for release metadata.
mod http {
    use std::time::Duration;

    use gpui_updater_core::{Error, Result};
    use serde::de::DeserializeOwned;
    use ureq::Agent;
    use ureq::tls::{RootCerts, TlsConfig};

    const USER_AGENT: &str = concat!("coop-updater/", env!("CARGO_PKG_VERSION"));
    const CONNECT_TIMEOUT: Duration = Duration::from_secs(15);
    const RESPONSE_TIMEOUT: Duration = Duration::from_secs(30);

    fn agent() -> Agent {
        Agent::config_builder()
            .user_agent(USER_AGENT)
            .tls_config(
                TlsConfig::builder()
                    .root_certs(RootCerts::PlatformVerifier)
                    .build(),
            )
            .timeout_resolve(Some(CONNECT_TIMEOUT))
            .timeout_connect(Some(CONNECT_TIMEOUT))
            .timeout_recv_response(Some(RESPONSE_TIMEOUT))
            .build()
            .into()
    }

    fn get_bytes(url: &str) -> Result<Vec<u8>> {
        let mut response = agent().get(url).call().map_err(|error| match error {
            ureq::Error::StatusCode(code) => Error::Http(format!("GET {url} -> {code}")),
            other => Error::Http(other.to_string()),
        })?;

        response
            .body_mut()
            .read_to_vec()
            .map_err(|error| Error::Http(format!("GET {url} -> {error}")))
    }

    pub(super) fn get_json<T: DeserializeOwned>(url: &str) -> Result<T> {
        serde_json::from_slice(&get_bytes(url)?).map_err(|error| Error::Parse(error.to_string()))
    }

    pub(super) fn get_string(url: &str) -> Result<String> {
        String::from_utf8(get_bytes(url)?).map_err(|error| Error::Parse(error.to_string()))
    }
}

#[cfg(test)]
mod tests {
    use gpui_updater_core::Version;

    use super::*;

    const PUBLISHED: &[&str] = &[
        "Coop_1.0.1_aarch64.dmg",
        "Coop_1.0.1_x64.dmg",
        "coop-linux-aarch64.tar.gz",
        "coop-linux-x86_64.tar.gz",
        "coop_1.0.1_aarch64.snap",
        "coop_1.0.1_arm64-setup.exe",
        "coop_1.0.1_x64-setup.exe",
        "coop_1.0.1_x86_64.snap",
        "su.reya.coop_aarch64.flatpak",
        "su.reya.coop_x86_64.flatpak",
    ];

    fn selected(os: &str, arch: &str) -> Option<&'static str> {
        let filter = asset_filter_for(os, arch)?;
        PUBLISHED.iter().copied().find(|name| filter.matches(name))
    }

    #[test]
    fn picks_the_artifact_matching_os_and_architecture() {
        assert_eq!(selected("macos", "aarch64"), Some("Coop_1.0.1_aarch64.dmg"));
        assert_eq!(selected("macos", "x86_64"), Some("Coop_1.0.1_x64.dmg"));
        assert_eq!(
            selected("linux", "aarch64"),
            Some("coop-linux-aarch64.tar.gz")
        );
        assert_eq!(
            selected("linux", "x86_64"),
            Some("coop-linux-x86_64.tar.gz")
        );
    }

    #[test]
    fn has_no_target_for_windows_or_unknown_platforms() {
        assert_eq!(asset_filter_for("windows", "x86_64"), None);
        assert_eq!(asset_filter_for("freebsd", "x86_64"), None);
        assert_eq!(asset_filter_for("macos", "riscv64"), None);
    }

    #[test]
    fn ignores_package_formats_and_sidecars_that_are_not_the_artifact() {
        let macos = asset_filter_for("macos", "aarch64").unwrap();
        assert!(!macos.matches("coop_1.0.1_aarch64.snap"));
        assert!(!macos.matches("su.reya.coop_aarch64.flatpak"));
        assert!(!macos.matches("Coop_1.0.1_aarch64.dmg.minisig"));

        let linux = asset_filter_for("linux", "x86_64").unwrap();
        assert!(!linux.matches("coop_1.0.1_x64-setup.exe"));
        assert!(!linux.matches("coop_1.0.1_x86_64.snap"));
    }

    #[test]
    fn reads_checksums_by_basename_ignoring_directory_prefix() {
        let sums = "\
abcdef  macos-arm64-artifacts/Coop_1.0.1_aarch64.dmg
123456  *linux-x64-artifacts/coop-linux-x86_64.tar.gz
789abc  SHA256SUMS
";
        assert_eq!(
            sha256_for(sums, "Coop_1.0.1_aarch64.dmg").as_deref(),
            Some("abcdef")
        );
        assert_eq!(
            sha256_for(sums, "coop-linux-x86_64.tar.gz").as_deref(),
            Some("123456")
        );
        assert_eq!(sha256_for(sums, "coop_1.0.1_x64-setup.exe"), None);
    }

    #[test]
    fn newest_published_skips_drafts_prereleases_and_bad_tags() {
        let releases: Vec<GiteaRelease> = serde_json::from_str(
            r#"[
                {
                    "tag_name": "v1.0.2",
                    "draft": true,
                    "assets": []
                },
                {
                    "tag_name": "v2.0.0-rc.1",
                    "prerelease": true,
                    "assets": []
                },
                {
                    "tag_name": "nightly",
                    "assets": []
                },
                {
                    "tag_name": "v1.0.0",
                    "assets": [
                        {
                            "name": "coop-linux-x86_64.tar.gz",
                            "browser_download_url": "https://git.reya.info/reya/coop/releases/download/v1.0.0/coop-linux-x86_64.tar.gz",
                            "size": 26160329
                        }
                    ]
                },
                {
                    "tag_name": "v1.0.1",
                    "name": "v1.0.1",
                    "body": "Fixed app panic on flatpak installations",
                    "assets": [
                        {
                            "name": "coop-linux-x86_64.tar.gz",
                            "browser_download_url": "https://git.reya.info/reya/coop/releases/download/v1.0.1/coop-linux-x86_64.tar.gz",
                            "size": 26160329
                        }
                    ]
                }
            ]"#,
        )
        .unwrap();

        let newest = newest_published(&releases).unwrap();
        assert_eq!(newest.tag_name, "v1.0.1");
        assert_eq!(parse_tag(&newest.tag_name).unwrap().to_string(), "1.0.1");
        assert_eq!(newest.assets.len(), 1);
        assert_eq!(newest.assets[0].size, 26160329);
    }

    #[test]
    #[ignore = "requires network access to the release host"]
    fn live_release_source_resolves_the_running_platform() {
        let filter = asset_filter_for(std::env::consts::OS, std::env::consts::ARCH)
            .expect("this platform should be supported");
        let source = GiteaSource::new("https://git.reya.info/api/v1", "reya", "coop", filter);

        let release = source
            .fetch_latest()
            .expect("release lookup should succeed");

        assert!(
            release.version >= Version::new(1, 0, 0),
            "unexpected version {}",
            release.version
        );
        assert!(
            source.filter.matches(&release.asset.name),
            "unexpected artifact {}",
            release.asset.name
        );
        assert!(
            release.asset.url.starts_with("https://"),
            "{} ",
            release.asset.url
        );
    }
}
