//! Context-free paint helpers, ported from bezel
//! (`crates/theme/src/paint.rs`, MIT License, Copyright (c) 2026 bezel
//! contributors): the process-wide appearance mirror and the free functions
//! ([`ink`], [`hairline`], [`wash`], …) that element builders call without a
//! `cx` in scope.

use std::sync::atomic::{AtomicU8, Ordering};

use gpui::{BoxShadow, Hsla, hsla, point, px};

use crate::ThemeMode;

/// Process-wide mirror of the installed theme's appearance.
///
/// The paint helpers are free functions called from deep inside element
/// builders that have no `cx` in scope, so they read the appearance from here
/// instead of the gpui global. Appearance is genuinely process-wide — one
/// setting for every window — so a single mirror is sound; [`Theme::change`]
/// is the only writer outside tests.
static CURRENT_APPEARANCE: AtomicU8 = AtomicU8::new(0);

/// The appearance the context-free paint helpers are painting for.
pub fn current_appearance() -> ThemeMode {
    match CURRENT_APPEARANCE.load(Ordering::Relaxed) {
        1 => ThemeMode::Light,
        _ => ThemeMode::Dark,
    }
}

/// Point the context-free paint helpers at an appearance. Called by
/// [`Theme::change`]; exposed for tests that build a palette without an `App`.
pub fn set_current_appearance(appearance: ThemeMode) {
    let encoded = match appearance {
        ThemeMode::Dark => 0,
        ThemeMode::Light => 1,
    };
    CURRENT_APPEARANCE.store(encoded, Ordering::Relaxed);
}

/// Light-mode alpha multiplier for **fills** (hover/active washes, chip and pill
/// backgrounds).
///
/// The established light-UI scales (Primer, Radix) land subtle ≈ 3–4%, hover ≈ 8%,
/// selected ≈ 14% black — which is where the dark palette's white alphas already
/// sit. So the honest multiplier is 1: the same number in both appearances, with
/// only the *tone* flipping. Any per-state correction belongs in that state's
/// token, not in a blanket multiplier.
pub const INK_FILL_SCALE: f32 = 1.0;

/// Light-mode alpha multiplier for **hairlines** (borders, dividers, rings).
/// Opposite of fills: a 1px edge has to hold its own against a bright surround,
/// and the dark palette's white hairlines are deliberately faint. Scaling up
/// keeps separators legible instead of dissolving into the panel.
pub const INK_HAIRLINE_SCALE: f32 = 1.35;

/// Lightness of [`hairline`] ink, dark then light.
const HAIRLINE_TONE: (f32, f32) = (0.75, 0.25);

/// Translucent **fill** ink for interactive states and chip plates: soft-white on
/// dark, soft-black on light at [`INK_FILL_SCALE`] of the alpha.
///
/// Alphas are quoted in *dark-mode terms* at every call site — the dark theme is
/// the tuned one — and the light value is derived. Callers keep one number and
/// both appearances stay in the relationship the dark tuning established.
pub fn ink(alpha: f32) -> Hsla {
    ink_for(current_appearance(), alpha)
}

pub(crate) fn ink_for(appearance: ThemeMode, alpha: f32) -> Hsla {
    match appearance {
        ThemeMode::Dark => hsla(0.0, 0.0, 1.0, alpha),
        ThemeMode::Light => hsla(0.0, 0.0, 0.0, alpha * INK_FILL_SCALE),
    }
}

/// Translucent **hairline** ink for borders, dividers and rings: a light grey on
/// dark, a dark grey on light at [`INK_HAIRLINE_SCALE`] of the alpha. Never
/// pure white or black.
///
/// Separate from [`ink`] because edges and fills scale in opposite directions
/// when the field brightens — a 1px line needs *more* ink on white, a plate needs
/// less.
pub fn hairline(alpha: f32) -> Hsla {
    hairline_for(current_appearance(), alpha)
}

pub(crate) fn hairline_for(appearance: ThemeMode, alpha: f32) -> Hsla {
    match appearance {
        ThemeMode::Dark => hsla(0.0, 0.0, HAIRLINE_TONE.0, alpha),
        ThemeMode::Light => hsla(
            0.0,
            0.0,
            HAIRLINE_TONE.1,
            (alpha * INK_HAIRLINE_SCALE).min(0.5),
        ),
    }
}

/// Interactive-state wash: a softened [`ink`] that stops short of pure black or
/// white so hover plates read as tinted glass rather than paint.
pub fn wash(alpha: f32) -> Hsla {
    wash_for(current_appearance(), alpha)
}

pub(crate) fn wash_for(appearance: ThemeMode, alpha: f32) -> Hsla {
    match appearance {
        ThemeMode::Dark => hsla(0.0, 0.0, 0.92, alpha),
        ThemeMode::Light => hsla(0.0, 0.0, 0.10, alpha * INK_FILL_SCALE),
    }
}

/// Alpha of the standard modal backdrop in dark mode. Call sites that need a
/// heavier or lighter scrim pass their own dark-mode alpha to [`scrim`].
pub const SCRIM_ALPHA_DARK: f32 = 0.60;

/// Modal backdrop at `alpha_dark` (quoted, as everywhere, in dark-mode terms).
///
/// Black in both appearances — a scrim's job is to darken what is behind it, and
/// a "light scrim" of white would wash the modal out rather than seat it. What
/// changes is strength: on a bright field a dark-mode-weight scrim reads as a
/// blackout, so light mode scales to roughly half.
pub fn scrim(alpha_dark: f32) -> Hsla {
    scrim_for(current_appearance(), alpha_dark)
}

pub(crate) fn scrim_for(appearance: ThemeMode, alpha_dark: f32) -> Hsla {
    match appearance {
        ThemeMode::Dark => hsla(0.0, 0.0, 0.0, alpha_dark),
        ThemeMode::Light => hsla(0.0, 0.0, 0.0, 0.32 * (alpha_dark / SCRIM_ALPHA_DARK)),
    }
}

/// Recessed band behind a picker header or footer strip.
///
/// A free function as well as a token so the chrome that paints it can be built
/// from context-free helpers; both resolve to the same value.
pub fn band() -> Hsla {
    band_for(current_appearance())
}

pub(crate) fn band_for(appearance: ThemeMode) -> Hsla {
    match appearance {
        ThemeMode::Dark => hsla(0.0, 0.0, 0.0, 0.16),
        // A recessed strip on white needs far less ink than on near-black; the
        // dark 16% would read as a bruise.
        ThemeMode::Light => hsla(0.0, 0.0, 0.0, 0.045),
    }
}

/// The elevation shadow a floating surface casts. Painted cut to outside the
/// shape, because the same drop shadow under a translucent fill reads as a grey
/// plate.
pub fn surface_shadows() -> Vec<BoxShadow> {
    vec![
        BoxShadow {
            color: hsla(0.0, 0.0, 0.0, 0.1),
            offset: point(px(0.0), px(10.0)),
            blur_radius: px(15.0),
            spread_radius: px(-3.0),
            inset: false,
        },
        BoxShadow {
            color: hsla(0.0, 0.0, 0.0, 0.1),
            offset: point(px(0.0), px(4.0)),
            blur_radius: px(6.0),
            spread_radius: px(-4.0),
            inset: false,
        },
    ]
}

/// Hover over an opaque plate: brighten the plate in dark mode, darken it in
/// light — never swap an opaque plate for a translucent wash.
pub(crate) fn plate_hover_for(appearance: ThemeMode, plate: gpui::Hsla) -> gpui::Hsla {
    crate::color::flatten(ink_for(appearance, 0.10), plate)
}

/// Pressed over an opaque plate, one rung above [`plate_hover_for`].
pub(crate) fn plate_active_for(appearance: ThemeMode, plate: gpui::Hsla) -> gpui::Hsla {
    crate::color::flatten(ink_for(appearance, 0.16), plate)
}

/// Hover over an opaque plate, for call sites outside this crate.
pub fn plate_hover(plate: gpui::Hsla) -> gpui::Hsla {
    plate_hover_for(current_appearance(), plate)
}

/// Pressed over an opaque plate, for call sites outside this crate.
pub fn plate_active(plate: gpui::Hsla) -> gpui::Hsla {
    plate_active_for(current_appearance(), plate)
}
