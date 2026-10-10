use std::ops::{Deref, DerefMut};

use gpui::{App, Global, Pixels, SharedString, Window, px};

mod brand;
mod color;
mod colors;
mod geometry;
mod layout;
mod notification;
mod paint;
mod palette;
mod platform_kind;
mod scrollbar_mode;
mod syntax;
mod theme;
mod typography;

pub mod appearance;

pub use appearance::AppearanceMode;
pub use brand::{BASE_COLORS, Brand, Ink, TextInk, Tint};
pub use color::{
    contrast_ratio, flatten, grey, hsl_to_rgb, lightness, mix, neutral, oklch, oklch_to_srgb,
    relative_luminance, rgb_to_hsl, tint,
};
pub use colors::*;
pub use geometry::*;
pub use layout::{
    BASE_RADIUS, BUTTON_HEIGHT, CONTENT_MARGIN, CONTROL_HEIGHT_SMALL, EDGE, SPACE, bubble_radius,
    button_radius, control_radius, inset_radius, panel_radius, surface_radius,
};
pub use notification::*;
pub use paint::{
    INK_FILL_SCALE, INK_HAIRLINE_SCALE, SCRIM_ALPHA_DARK, band, current_appearance, hairline, ink,
    plate_active, plate_hover, scrim, set_current_appearance, surface_shadows, wash,
};
pub use platform_kind::PlatformKind;
pub use scrollbar_mode::*;
pub use syntax::{HighlightKind, SyntaxPalette};
pub use theme::*;
pub use typography::{
    DEFAULT_BASE_TEXT_SIZE, Metrics, TextStyle, Typeset, base_text_size, set_base_text_size,
};

/// Which appearance the app is painting. Bezel names this enum `Appearance`;
/// coop's `ThemeMode` predates the port and carries the same two variants.
pub type Appearance = ThemeMode;

/// Defines window border radius for platforms that use client side decorations.
pub const CLIENT_SIDE_DECORATION_ROUNDING: Pixels = px(10.0);

/// Defines window shadow size for platforms that use client side decorations.
pub const CLIENT_SIDE_DECORATION_SHADOW: Pixels = px(10.0);

/// Defines window border size for platforms that use client side decorations.
pub const CLIENT_SIDE_DECORATION_BORDER: Pixels = px(1.0);

/// Defines window titlebar height
pub(crate) const TITLEBAR_HEIGHT: Pixels = px(36.0);

/// Defines workspace tabbar height
pub const TABBAR_HEIGHT: Pixels = px(36.0);

/// Defines default sidebar width
pub const SIDEBAR_WIDTH: Pixels = px(240.);

pub fn init(cx: &mut App) {
    appearance::init(AppearanceMode::default(), cx);
    Theme::sync_scrollbar_appearance(cx);
}

/// An alternative palette source.
pub type PaletteBuilder = fn(ThemeMode) -> ThemeColors;

struct GlobalPaletteBuilder(Option<PaletteBuilder>);

impl Global for GlobalPaletteBuilder {}

/// Install a brand or an alternative palette on the app.
pub trait AppExt {
    fn set_brand(&mut self, brand: Brand);
    fn set_palette(&mut self, build: PaletteBuilder);
    fn set_appearance_mode(&mut self, mode: AppearanceMode);
    fn set_base_text_size(&mut self, points: f32);
}

impl AppExt for App {
    fn set_brand(&mut self, brand: Brand) {
        self.set_global(brand);
        reinstall(self);
    }

    fn set_palette(&mut self, build: PaletteBuilder) {
        self.set_global(GlobalPaletteBuilder(Some(build)));
        reinstall(self);
    }

    fn set_appearance_mode(&mut self, mode: AppearanceMode) {
        appearance::set_mode(mode, self);
    }

    fn set_base_text_size(&mut self, points: f32) {
        typography::set_base_text_size(points);
        self.refresh_windows();
    }
}

/// Re-install the palette for the current appearance.
fn reinstall(cx: &mut App) {
    let mode = cx
        .try_global::<Theme>()
        .map(|theme| theme.mode)
        .unwrap_or_default();
    Theme::change(mode, None, cx);
    cx.refresh_windows();
}

/// Mirror the active coop theme into the `gpui-base` global theme.
pub fn sync_base(cx: &mut App) {
    let Some(theme) = cx.try_global::<Theme>() else {
        return;
    };

    let appearance = if theme.mode.is_dark() {
        gpui_base::ThemeAppearance::Dark
    } else {
        gpui_base::ThemeAppearance::Light
    };
    let scrollbar_mode = match theme.scrollbar_mode {
        ScrollbarMode::Scrolling => gpui_base::ScrollbarMode::Scrolling,
        ScrollbarMode::Hover => gpui_base::ScrollbarMode::Hover,
        ScrollbarMode::Always => gpui_base::ScrollbarMode::Always,
    };
    let colors = theme.colors;
    let font_sans = theme.font_sans.clone();

    let base = gpui_base::Theme::global_mut(cx);
    base.appearance = appearance;
    base.scrollbar = base.scrollbar.clone().with_mode(scrollbar_mode);
    base.tokens.typography.sans = font_sans;

    let tokens = &mut base.tokens.colors;
    tokens.background = colors.bg;
    tokens.foreground = colors.text;
    tokens.surface = colors.surface;
    tokens.surface_foreground = colors.text;
    tokens.primary = colors.solid;
    tokens.primary_foreground = colors.on_solid;
    tokens.secondary = colors.surface_raised;
    tokens.secondary_foreground = colors.text;
    tokens.muted = colors.surface_card;
    tokens.muted_foreground = colors.text_muted;
    tokens.accent = colors.element_hover;
    tokens.accent_foreground = colors.text;
    tokens.destructive = colors.danger_strong;
    tokens.destructive_foreground = gpui::white();
    tokens.border = colors.border;
    tokens.input = colors.border;
    tokens.ring = colors.ring;
    tokens.selection = colors.selection;
}

pub trait ActiveTheme {
    fn theme(&self) -> &Theme;
}

impl ActiveTheme for App {
    #[inline(always)]
    fn theme(&self) -> &Theme {
        Theme::global(self)
    }
}

#[derive(Debug, Clone)]
pub struct Theme {
    pub colors: ThemeColors,
    pub mode: ThemeMode,
    pub font_sans: SharedString,
    pub font_mono: SharedString,
    pub shadow: bool,
    pub scrollbar_mode: ScrollbarMode,
    pub notification: NotificationSettings,
}

impl Deref for Theme {
    type Target = ThemeColors;

    fn deref(&self) -> &Self::Target {
        &self.colors
    }
}

impl DerefMut for Theme {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.colors
    }
}

impl Global for Theme {}

impl Theme {
    /// Returns the global theme reference
    pub fn global(cx: &App) -> &Theme {
        cx.global::<Theme>()
    }

    /// Returns the global theme mutable reference
    pub(crate) fn global_mut(cx: &mut App) -> &mut Theme {
        cx.global_mut::<Theme>()
    }

    /// Returns true if the theme is dark.
    pub fn is_dark(&self) -> bool {
        self.mode.is_dark()
    }

    /// Sync the theme with the system appearance
    pub fn sync_system_appearance(window: Option<&mut Window>, cx: &mut App) {
        appearance::record_system_appearance(window.as_deref(), cx);
    }

    /// Sync the Scrollbar showing behavior with the system
    pub fn sync_scrollbar_appearance(cx: &mut App) {
        Theme::global_mut(cx).scrollbar_mode = if cx.should_auto_hide_scrollbars() {
            ScrollbarMode::Scrolling
        } else {
            ScrollbarMode::Hover
        };
    }

    /// Change the app's appearance
    pub fn change<M>(mode: M, window: Option<&mut Window>, cx: &mut App)
    where
        M: Into<ThemeMode>,
    {
        if !cx.has_global::<Theme>() {
            cx.set_global(Theme::default());
        }

        let mode = mode.into();
        let builder = cx
            .try_global::<GlobalPaletteBuilder>()
            .and_then(|builder| builder.0);
        let mut colors = match builder {
            Some(build) => build(mode),
            None => ThemeColors::for_appearance(mode),
        };

        // A default brand changes nothing; anything else rotates the tokens.
        let installed_brand = brand::installed(cx);
        if !installed_brand.is_default() {
            installed_brand.apply(mode, &mut colors);
        }

        let theme = Theme::global_mut(cx);

        // Set the theme mode
        theme.mode = mode;

        // Set the theme colors
        theme.colors = colors;

        // Refresh the window if available
        if let Some(window) = window {
            window.refresh();
        }

        // Keep the context-free paint helpers and the radius accessors in step
        paint::set_current_appearance(mode);
        layout::set_base_radius(installed_brand.radius);

        // Keep the base-layer projection in step with the coop palette
        sync_base(cx);
    }
}

impl Default for Theme {
    fn default() -> Self {
        let platform = PlatformKind::platform();
        let mode = ThemeMode::default();

        // The system UI font, by platform.
        let font_sans = match platform {
            PlatformKind::Linux => "Inter",
            _ => ".SystemUIFont",
        };

        Self {
            font_sans: font_sans.into(),
            font_mono: system_mono().into(),
            shadow: true,
            scrollbar_mode: ScrollbarMode::default(),
            notification: NotificationSettings::default(),
            mode,
            colors: ThemeColors::for_appearance(mode),
        }
    }
}

/// The mono face has no alias of its own, so each backend is named here.
fn system_mono() -> &'static str {
    if cfg!(target_family = "wasm") {
        ".ZedMono"
    } else if cfg!(target_os = "macos") {
        ".AppleSystemUIFontMonospaced"
    } else if cfg!(target_os = "windows") {
        "Cascadia Mono"
    } else {
        "DejaVu Sans Mono"
    }
}
