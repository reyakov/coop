use gpui::WindowAppearance;
use serde::{Deserialize, Serialize};

/// Which appearance the palette is painted for.
#[derive(Debug, Clone, Copy, Default, PartialEq, PartialOrd, Eq, Hash, Deserialize, Serialize)]
pub enum ThemeMode {
    #[default]
    Dark,
    Light,
}

impl ThemeMode {
    pub fn is_dark(&self) -> bool {
        matches!(self, Self::Dark)
    }

    pub fn is_light(&self) -> bool {
        matches!(self, Self::Light)
    }

    /// Map a gpui window appearance onto ours.
    pub fn from_window(appearance: WindowAppearance) -> Self {
        Self::from(appearance)
    }

    /// Return theme name: `light`, `dark`.
    pub fn name(&self) -> &'static str {
        match self {
            ThemeMode::Light => "Light",
            ThemeMode::Dark => "Dark",
        }
    }
}

impl From<WindowAppearance> for ThemeMode {
    fn from(appearance: WindowAppearance) -> Self {
        match appearance {
            WindowAppearance::Dark | WindowAppearance::VibrantDark => Self::Dark,
            WindowAppearance::Light | WindowAppearance::VibrantLight => Self::Light,
        }
    }
}
