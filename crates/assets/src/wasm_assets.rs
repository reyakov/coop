use std::borrow::Cow;
use std::collections::HashMap;
use std::sync::{Arc, RwLock};

use gpui::{AssetSource, Result, SharedString};
use wasm_bindgen_futures::spawn_local;

// Compile-time manifest of every asset file served on wasm (see build.rs).
include!(concat!(env!("OUT_DIR"), "/wasm_assets.rs"));

/// Path prefixes that the wasm loader serves. Fonts and themes are not
/// downloaded on web: the web platform bundles its own fonts, and the theme
/// registry falls back to the built-in default theme.
const SERVED_PREFIXES: [&str; 2] = ["icons/", "brand/"];

/// WASM implementation - download assets on demand.
///
/// Assets are fetched from `{endpoint}/assets/{path}` and cached in memory
/// after the first successful download. This keeps the WASM bundle small
/// while still providing the full asset set at runtime.
pub struct Assets {
    endpoint: SharedString,
    cache: Arc<RwLock<HashMap<String, Vec<u8>>>>,
    pending: Arc<RwLock<HashMap<String, bool>>>,
}

impl Assets {
    /// Create a new Assets instance backed by the given endpoint.
    ///
    /// Assets are resolved as `{endpoint}/assets/{path}`. An empty endpoint
    /// resolves against the current page origin (e.g. `/assets/icons/foo.svg`).
    pub fn new(endpoint: impl Into<SharedString>) -> Self {
        Self {
            endpoint: endpoint.into(),
            cache: Arc::new(RwLock::new(HashMap::new())),
            pending: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    /// Absolute URL of the given asset path.
    ///
    /// `reqwest` requires absolute URLs, so a relative endpoint is resolved
    /// against the current page origin.
    fn asset_url(&self, path: &str) -> String {
        let endpoint = if self.endpoint.is_empty() {
            web_sys::window()
                .and_then(|window| window.location().origin().ok())
                .unwrap_or_default()
        } else {
            self.endpoint.to_string()
        };
        format!("{endpoint}/assets/{path}")
    }

    /// Download every asset in [`WASM_ASSETS`] into the cache, in parallel,
    /// before the app starts.
    ///
    /// Preloading is required for two reasons:
    /// - Assets loaded through GPUI's [`gpui::Asset`] machinery (e.g. `img()`)
    ///   cache failed loads and never retry them.
    /// - SVG painting only re-attempts an empty load on the next repaint, so
    ///   an icon would stay invisible until the window happens to redraw.
    pub async fn preload(&self) {
        let downloads = WASM_ASSETS.iter().map(|path| async move {
            let result = reqwest::get(self.asset_url(path)).await;
            match result {
                Ok(response) if response.status().is_success() => match response.bytes().await {
                    Ok(bytes) => {
                        if let Ok(mut cache) = self.cache.write() {
                            cache.insert(path.to_string(), bytes.to_vec());
                        }
                    }
                    Err(e) => {
                        log::warn!("Failed to read asset {}: {}", path, e);
                    }
                },
                Ok(response) => {
                    log::warn!(
                        "Failed to download asset {}: HTTP {}",
                        path,
                        response.status()
                    );
                }
                Err(e) => {
                    log::warn!("Failed to fetch asset {}: {}", path, e);
                }
            }
        });
        futures::future::join_all(downloads).await;
    }
}

impl AssetSource for Assets {
    fn load(&self, path: &str) -> Result<Option<Cow<'static, [u8]>>> {
        if path.is_empty() {
            return Ok(None);
        }

        // Only serve paths the web build actually ships.
        if !SERVED_PREFIXES
            .iter()
            .any(|prefix| path.starts_with(prefix))
        {
            return Ok(None);
        }

        // Serve from the in-memory cache when available.
        if let Ok(cache) = self.cache.read() {
            if let Some(data) = cache.get(path) {
                return Ok(Some(Cow::Owned(data.clone())));
            }
        }

        // Kick off a single download per path; concurrent requests for the
        // same path share it.
        let is_pending = self
            .pending
            .read()
            .map(|pending| pending.contains_key(path))
            .unwrap_or(false);

        if !is_pending {
            if let Ok(mut pending) = self.pending.write() {
                pending.insert(path.to_string(), true);
            }

            let url = self.asset_url(path);
            let path_clone = path.to_string();
            let cache = self.cache.clone();
            let pending = self.pending.clone();

            spawn_local(async move {
                match reqwest::get(&url).await {
                    Ok(response) if response.status().is_success() => {
                        match response.bytes().await {
                            Ok(bytes) => {
                                if let Ok(mut cache) = cache.write() {
                                    cache.insert(path_clone.clone(), bytes.to_vec());
                                }
                            }
                            Err(e) => {
                                log::warn!("Failed to read asset {}: {}", path_clone, e);
                            }
                        }
                    }
                    Ok(response) => {
                        log::warn!(
                            "Failed to download asset {}: HTTP {}",
                            path_clone,
                            response.status()
                        );
                    }
                    Err(e) => {
                        log::warn!("Failed to fetch asset {}: {}", path_clone, e);
                    }
                }

                // Allow retrying failed downloads on subsequent requests.
                if let Ok(mut pending) = pending.write() {
                    pending.remove(&path_clone);
                }
            });
        }

        // The asset is not available yet. GPUI's SVG atlas does not cache
        // empty loads, so the next repaint will call `load` again and find
        // the asset in the cache once the download completes.
        Ok(None)
    }

    fn list(&self, _path: &str) -> Result<Vec<SharedString>> {
        // The asset manifest is not available at runtime on web; embedded
        // directories are not listed.
        Ok(Vec::new())
    }
}
