use std::path::PathBuf;
use std::sync::OnceLock;

/// Returns the path to the user's home directory.
fn home_dir() -> &'static PathBuf {
    static HOME_DIR: OnceLock<PathBuf> = OnceLock::new();
    HOME_DIR.get_or_init(|| dirs::home_dir().expect("failed to determine home directory"))
}

/// Returns the path to the user's download directory.
pub fn download_dir() -> &'static PathBuf {
    static DOWNLOAD_DIR: OnceLock<PathBuf> = OnceLock::new();
    DOWNLOAD_DIR
        .get_or_init(|| dirs::download_dir().expect("failed to determine download directory"))
}

/// Returns the path to the configuration directory used by Coop.
pub fn config_dir() -> &'static PathBuf {
    static CONFIG_DIR: OnceLock<PathBuf> = OnceLock::new();
    CONFIG_DIR.get_or_init(|| {
        if cfg!(target_os = "windows") {
            return dirs::config_dir()
                .expect("failed to determine RoamingAppData directory")
                .join("Coop");
        }

        if cfg!(any(target_os = "linux", target_os = "freebsd")) {
            return if let Ok(flatpak_xdg_config) = std::env::var("FLATPAK_XDG_CONFIG_HOME") {
                flatpak_xdg_config.into()
            } else {
                dirs::config_dir().expect("failed to determine XDG_CONFIG_HOME directory")
            }
            .join("coop");
        }

        home_dir().join(".config").join("coop")
    })
}
