use std::ops::{Deref, DerefMut};
use std::rc::Rc;

use gpui::{App, Global, Pixels, SharedString, Window, px};

mod colors;
mod geometry;
mod notification;
mod platform_kind;
mod registry;
mod scale;
mod scrollbar_mode;
mod theme;

pub use colors::*;
pub use geometry::*;
pub use notification::*;
pub use platform_kind::PlatformKind;
pub use registry::*;
pub use scale::*;
pub use scrollbar_mode::*;
pub use theme::*;

/// Defines window border radius for platforms that use client side decorations.
pub const CLIENT_SIDE_DECORATION_ROUNDING: Pixels = px(10.0);

/// Defines window shadow size for platforms that use client side decorations.
pub const CLIENT_SIDE_DECORATION_SHADOW: Pixels = px(10.0);

/// Defines window border size for platforms that use client side decorations.
pub const CLIENT_SIDE_DECORATION_BORDER: Pixels = px(1.0);

/// Defines window titlebar height
pub const TITLEBAR_HEIGHT: Pixels = px(36.0);

/// Defines workspace tabbar height
pub const TABBAR_HEIGHT: Pixels = px(44.0);

/// Defines default sidebar width
pub const SIDEBAR_WIDTH: Pixels = px(240.);

pub fn init(cx: &mut App) {
    registry::init(cx);

    Theme::sync_system_appearance(None, cx);
    Theme::sync_scrollbar_appearance(cx);
}

/// Mirror the active coop theme into the `gpui-base` global theme.
///
/// Base paints a few things from its own tokens -- the focus ring, the wash
/// behind selected text, scrollbars, and overlay backdrops -- so the two
/// globals have to agree or those details drift away from the palette.
///
/// Only roles base can act on are projected. Radius, spacing, typography sizes,
/// shadows, and scrollbar geometry keep their base defaults: coop has a single
/// `radius`/`radius_lg`/`font_size` where base has six-point scales, so any
/// mapping would be invented rather than derived.
///
/// This is a no-op before the coop theme global exists; [`Theme::change`] is the
/// authoritative hook that keeps the projection current.
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
    let font_family = theme.font_family.clone();

    let base = gpui_base::Theme::global_mut(cx);
    base.appearance = appearance;
    base.scrollbar = base.scrollbar.clone().with_mode(scrollbar_mode);
    base.tokens.typography.sans = font_family;

    let tokens = &mut base.tokens.colors;
    tokens.background = colors.background;
    tokens.foreground = colors.text;
    tokens.surface = colors.surface_background;
    tokens.surface_foreground = colors.text;
    tokens.primary = colors.element_background;
    tokens.primary_foreground = colors.element_foreground;
    tokens.secondary = colors.secondary_background;
    tokens.secondary_foreground = colors.secondary_foreground;
    tokens.muted = colors.ghost_element_background_alt;
    tokens.muted_foreground = colors.text_muted;
    tokens.accent = colors.ghost_element_hover;
    tokens.accent_foreground = colors.text;
    tokens.destructive = colors.danger_background;
    tokens.destructive_foreground = colors.danger_foreground;
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
    /// Theme colors
    pub colors: ThemeColors,

    /// Theme family
    pub theme: Rc<ThemeFamily>,

    /// The appearance of the theme (light or dark).
    pub mode: ThemeMode,

    /// The font family for the application.
    pub font_family: SharedString,

    /// The root font size for the application, default is 15px.
    pub font_size: Pixels,

    /// Radius for the general elements.
    pub radius: Pixels,

    /// Radius for the large elements, e.g.: modal, notification.
    pub radius_lg: Pixels,

    /// Enable shadow for the general elements. default is true
    pub shadow: bool,

    /// Show the scrollbar mode, default: scrolling
    pub scrollbar_mode: ScrollbarMode,

    /// Notification settings
    pub notification: NotificationSettings,

    /// Platform kind
    pub platform: PlatformKind,
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
    pub fn global_mut(cx: &mut App) -> &mut Theme {
        cx.global_mut::<Theme>()
    }

    /// Returns true if the theme is dark.
    pub fn is_dark(&self) -> bool {
        self.mode.is_dark()
    }

    /// Sync the theme with the system appearance
    pub fn sync_system_appearance(window: Option<&mut Window>, cx: &mut App) {
        let appearance = window
            .as_ref()
            .map(|window| window.appearance())
            .unwrap_or_else(|| cx.window_appearance());

        Self::change(appearance, window, cx);
    }

    /// Sync the Scrollbar showing behavior with the system
    pub fn sync_scrollbar_appearance(cx: &mut App) {
        Theme::global_mut(cx).scrollbar_mode = if cx.should_auto_hide_scrollbars() {
            ScrollbarMode::Scrolling
        } else {
            ScrollbarMode::Hover
        };
    }

    /// Apply a new theme to the application.
    pub fn apply_theme(new_theme: Rc<ThemeFamily>, window: Option<&mut Window>, cx: &mut App) {
        let theme = cx.global_mut::<Theme>();
        let mode = theme.mode;
        // Update the theme
        theme.theme = new_theme;
        // Emit a theme change event
        Self::change(mode, window, cx);
    }

    /// Change the app's appearance
    pub fn change<M>(mode: M, window: Option<&mut Window>, cx: &mut App)
    where
        M: Into<ThemeMode>,
    {
        if !cx.has_global::<Theme>() {
            let default_theme = ThemeFamily::default();
            let theme = Theme::from(default_theme);

            cx.set_global(theme);
        }

        let mode = mode.into();
        let theme = cx.global_mut::<Theme>();

        // Set the theme mode
        theme.mode = mode;

        // Set the theme colors
        if mode.is_dark() {
            theme.colors = *theme.theme.dark();
        } else {
            theme.colors = *theme.theme.light();
        }

        // Refresh the window if available
        if let Some(window) = window {
            window.refresh();
        }

        // Keep the base-layer projection in step with the coop palette
        sync_base(cx);
    }
}

impl From<ThemeFamily> for Theme {
    fn from(family: ThemeFamily) -> Self {
        let platform = PlatformKind::platform();
        let mode = ThemeMode::default();

        // Define the font family based on the platform.
        let font_family = match platform {
            PlatformKind::Linux => "Inter",
            _ => ".SystemUIFont",
        };

        // Define the theme colors based on the appearance
        let colors = match mode {
            ThemeMode::Light => family.light(),
            ThemeMode::Dark => family.dark(),
        };

        Theme {
            font_size: px(15.),
            font_family: font_family.into(),
            radius: px(6.),
            radius_lg: px(10.),
            shadow: true,
            scrollbar_mode: ScrollbarMode::default(),
            notification: NotificationSettings::default(),
            mode,
            colors: *colors,
            theme: Rc::new(family),
            platform,
        }
    }
}
