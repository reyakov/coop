use std::borrow::Cow;

use anyhow::Context;
use gpui::{App, AssetSource, Result, SharedString};
use rust_embed::RustEmbed;

/// Native implementation using `rust-embed`: assets are embedded into the
/// binary at compile time.
#[derive(RustEmbed)]
#[folder = "../../assets"]
#[include = "fonts/**/*"]
#[include = "brand/**/*"]
#[include = "icons/**/*"]
#[include = "themes/**/*"]
#[exclude = "*.DS_Store"]
pub struct Assets;

impl Assets {
    /// Create a new Assets instance. The endpoint parameter is ignored for
    /// native builds.
    pub fn new(_endpoint: impl Into<SharedString>) -> Self {
        Self
    }
}

impl AssetSource for Assets {
    fn load(&self, path: &str) -> Result<Option<Cow<'static, [u8]>>> {
        Self::get(path)
            .map(|f| Some(f.data))
            .with_context(|| format!("loading asset at path {path:?}"))
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

impl Assets {
    /// Populate the [`TextSystem`] of the given [`AppContext`] with all `.ttf` fonts in the `fonts` directory.
    pub fn load_fonts(&self, cx: &App) -> anyhow::Result<()> {
        let font_paths = self.list("fonts")?;
        let mut embedded_fonts = Vec::new();
        for font_path in font_paths {
            if font_path.ends_with(".ttf") {
                let font_bytes = cx
                    .asset_source()
                    .load(&font_path)?
                    .expect("Assets should never return None");
                embedded_fonts.push(font_bytes);
            }
        }

        cx.text_system().add_fonts(embedded_fonts)
    }
}
