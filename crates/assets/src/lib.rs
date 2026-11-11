use gpui::{AssetSource, Result, SharedString};
use rust_embed::RustEmbed;

/// The Lucide icons the app and the kit components draw, vendored so they ship
/// with the binary and resolve like any other coop asset.
#[derive(RustEmbed)]
#[folder = "../../assets"]
#[include = "fonts/**/*"]
#[include = "brand/**/*"]
#[include = "icons/**/*"]
#[include = "themes/**/*"]
#[exclude = "*.DS_Store"]
pub struct Assets;

/// Coop's bundled assets, with the GPUI Kit component icon bundle as the
/// fallback for any icon not vendored here.
pub struct CombinedAssets;

impl AssetSource for CombinedAssets {
    fn load(&self, path: &str) -> Result<Option<std::borrow::Cow<'static, [u8]>>> {
        if let Ok(Some(data)) = Assets.load(path) {
            return Ok(Some(data));
        }

        #[cfg(target_arch = "wasm32")]
        {
            // The kit bundle fetches icons over HTTP on the web; a single
            // instance keeps its download cache alive across loads.
            use std::sync::OnceLock;
            static KIT_ASSETS: OnceLock<gpui_kit::assets::Assets> = OnceLock::new();
            KIT_ASSETS
                .get_or_init(gpui_kit::assets::Assets::default)
                .load(path)
        }

        #[cfg(not(target_arch = "wasm32"))]
        gpui_kit::assets::Assets.load(path)
    }

    fn list(&self, path: &str) -> Result<Vec<SharedString>> {
        let mut paths = Assets.list(path).unwrap_or_default();

        #[cfg(target_arch = "wasm32")]
        {
            use std::sync::OnceLock;
            static KIT_ASSETS: OnceLock<gpui_kit::assets::Assets> = OnceLock::new();
            paths.extend(
                KIT_ASSETS
                    .get_or_init(gpui_kit::assets::Assets::default)
                    .list(path)?,
            );
        }

        #[cfg(not(target_arch = "wasm32"))]
        paths.extend(gpui_kit::assets::Assets.list(path)?);

        Ok(paths)
    }
}

impl AssetSource for Assets {
    fn load(&self, path: &str) -> Result<Option<std::borrow::Cow<'static, [u8]>>> {
        Ok(Self::get(path).map(|f| f.data))
    }

    fn list(&self, path: &str) -> Result<Vec<SharedString>> {
        Ok(Self::iter()
            .filter_map(|p| {
                if p.starts_with(path) {
                    Some(p.into())
                } else {
                    None
                }
            })
            .collect())
    }
}

#[cfg(test)]
mod tests {
    use gpui_kit::component::ThemeSet;

    use super::*;

    /// Every bundled theme file must parse against gpui-kit's schema.
    #[test]
    fn bundled_themes_parse() {
        let mut count = 0;

        for entry in Assets::iter().filter(|p| p.starts_with("themes/")) {
            let file = Assets::get(&entry).expect("theme file exists");
            let content = std::str::from_utf8(&file.data).expect("theme is UTF-8");
            let set: ThemeSet = serde_json::from_str(content).expect("theme parses");
            assert!(!set.themes.is_empty(), "{entry} has no themes");
            count += set.themes.len();
        }

        assert!(
            count >= 36,
            "expected the bundled gpui-kit themes, got {count}"
        );
    }
}
