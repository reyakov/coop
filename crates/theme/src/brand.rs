//! Brand, ported from bezel (`crates/theme/src/brand.rs`, MIT License,
//! Copyright (c) 2026 bezel contributors): one hue for the greys, one for the
//! accent, one radius, and the text ink.
//!
//! A brand moves hue alone: every lightness in `palette.rs` stays as the
//! palette set it.

use gpui::{App, Global, Hsla};

use crate::{ThemeColors, ThemeMode, color, layout};

/// A hue and how much of it, in oklch terms. `chroma: 0.0` is the shipped
/// neutral, so [`Brand::default`] reproduces the built-in palette exactly.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct Tint {
    /// oklch hue, in degrees.
    pub hue: f32,
    /// oklch chroma. Neutral ramps live near 0.01–0.05; an accent carries more.
    pub chroma: f32,
}

impl Tint {
    pub const NONE: Self = Self {
        hue: 0.0,
        chroma: 0.0,
    };

    pub const fn new(hue: f32, chroma: f32) -> Self {
        Self { hue, chroma }
    }
}

/// The greys a UI is built on, as oklch hue and chroma.
///
/// Tailwind's five neutral families at their 500 step (tailwindcss.com/docs/colors)
/// — the same list shadcn offers as its base colour, and the reason these are
/// quoted rather than invented: a neutral that carries hue is a judgement
/// someone else has already made five times.
pub const BASE_COLORS: [(&str, Tint); 5] = [
    ("Neutral", Tint::NONE),
    ("Stone", Tint::new(58.071, 0.013)),
    ("Zinc", Tint::new(285.938, 0.016)),
    ("Gray", Tint::new(264.364, 0.027)),
    ("Slate", Tint::new(257.417, 0.046)),
];

/// The alpha of each text role over the appearance's ink — white in dark,
/// black in light.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TextInk {
    /// [`ThemeColors::text`].
    pub text: f32,
    /// [`ThemeColors::text_muted`].
    pub muted: f32,
    /// [`ThemeColors::text_faint`].
    pub faint: f32,
}

impl TextInk {
    /// The role at `alpha` over `appearance`'s ink.
    pub fn color(appearance: ThemeMode, alpha: f32) -> Hsla {
        match appearance {
            ThemeMode::Dark => gpui::hsla(0.0, 0.0, 1.0, alpha),
            ThemeMode::Light => gpui::hsla(0.0, 0.0, 0.0, alpha),
        }
    }

    /// Writes [`ThemeColors::text`], [`ThemeColors::text_muted`] and
    /// [`ThemeColors::text_faint`] for the appearance.
    pub fn paint(&self, appearance: ThemeMode, colors: &mut ThemeColors) {
        colors.text = Self::color(appearance, self.text);
        colors.text_muted = Self::color(appearance, self.muted);
        colors.text_faint = Self::color(appearance, self.faint);
    }
}

/// Text ink for both appearances.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Ink {
    pub dark: TextInk,
    pub light: TextInk,
}

impl Ink {
    /// Text and muted at 4.5:1 or more and faint at 3:1 or more, on
    /// [`ThemeColors::bg`] and [`ThemeColors::surface`] in both appearances.
    pub const ACCESSIBLE: Self = Self {
        dark: TextInk {
            text: 0.85,
            muted: 0.52,
            faint: 0.40,
        },
        light: TextInk {
            text: 0.85,
            muted: 0.56,
            faint: 0.43,
        },
    };
    /// AppKit's label, secondary and tertiary label colours. Muted is under
    /// 4.5:1 on white, and faint is under 3:1 in both appearances.
    pub const APPKIT: Self = Self {
        dark: TextInk {
            text: 0.85,
            muted: 0.55,
            faint: 0.25,
        },
        light: TextInk {
            text: 0.85,
            muted: 0.50,
            faint: 0.26,
        },
    };

    pub fn for_appearance(&self, appearance: ThemeMode) -> TextInk {
        match appearance {
            ThemeMode::Dark => self.dark,
            ThemeMode::Light => self.light,
        }
    }
}

/// What an app changes about the shipped palette without redesigning it.
///
/// Installed as a gpui [`Global`] via [`crate::AppExt::set_brand`]; every
/// palette install applies it, so it survives a light/dark switch and composes
/// with [`crate::AppExt::set_palette`] rather than competing with it.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Brand {
    /// The hue every grey in the palette carries.
    pub tint: Tint,
    /// The emphasis hue. Left neutral, the accent follows [`Self::tint`] like
    /// any other grey — which is what the shipped palette already is.
    pub accent: Tint,
    /// The base corner radius; every other corner is a ratio of it. See
    /// [`crate::BASE_RADIUS`].
    pub radius: f32,
    /// The text roles' alphas. [`Ink::APPKIT`] is what the palettes ship.
    pub ink: Ink,
}

impl Global for Brand {}

impl Default for Brand {
    fn default() -> Self {
        Self {
            tint: Tint::NONE,
            accent: Tint::NONE,
            radius: layout::BASE_RADIUS,
            ink: Ink::APPKIT,
        }
    }
}

impl Brand {
    /// Whether applying this brand would change nothing — the shipped palette
    /// already carries these values, so a JSON theme's own text colors must be
    /// left alone.
    pub fn is_default(&self) -> bool {
        *self == Self::default()
    }

    /// The shipped palette for an appearance, rotated onto this brand.
    pub fn branded(&self, appearance: ThemeMode) -> ThemeColors {
        let mut colors = ThemeColors::for_appearance(appearance);
        // apply() re-asserts this brand's ink, so the default brand — whose
        // ink is the shipped one — must skip it to stay a strict no-op.
        if !self.is_default() {
            self.apply(appearance, &mut colors);
        }
        colors
    }

    /// Rotate a palette onto this brand's hues.
    pub fn apply(&self, appearance: ThemeMode, colors: &mut ThemeColors) {
        self.ink
            .for_appearance(appearance)
            .paint(appearance, colors);
        // Every colour token, with the rule doing the choosing: a token that is
        // already grey takes the tint, and one that already carries a hue —
        // danger, warning, success — is semantic and keeps it. Translucent ink
        // is skipped because it paints over whatever is beneath it, which is
        // tinted already.
        let tokens: [&mut Hsla; 41] = [
            &mut colors.bg,
            &mut colors.surface,
            &mut colors.surface_raised,
            &mut colors.surface_card,
            &mut colors.surface_dialog,
            &mut colors.surface_overlay,
            &mut colors.element_hover,
            &mut colors.element_active,
            &mut colors.border_faint,
            &mut colors.border,
            &mut colors.border_strong,
            &mut colors.text,
            &mut colors.text_muted,
            &mut colors.text_faint,
            &mut colors.text_dim,
            &mut colors.solid,
            &mut colors.on_solid,
            &mut colors.accent,
            &mut colors.accent_strong,
            &mut colors.on_accent,
            &mut colors.danger,
            &mut colors.danger_muted,
            &mut colors.warning,
            &mut colors.warning_muted,
            &mut colors.success,
            &mut colors.busy,
            &mut colors.success_muted,
            &mut colors.surface_raised_hover,
            &mut colors.band,
            &mut colors.input_bg,
            &mut colors.selection,
            &mut colors.cursor,
            &mut colors.caret,
            &mut colors.ring,
            &mut colors.drop_target,
            &mut colors.danger_strong,
            &mut colors.code_text,
            &mut colors.code_wash,
            &mut colors.diff_add,
            &mut colors.diff_del,
            &mut colors.diff_hunk_bg,
        ];
        let syntax: [&mut Hsla; 24] = [
            &mut colors.syntax.comment,
            &mut colors.syntax.keyword,
            &mut colors.syntax.string,
            &mut colors.syntax.string_special,
            &mut colors.syntax.escape,
            &mut colors.syntax.number,
            &mut colors.syntax.boolean,
            &mut colors.syntax.type_name,
            &mut colors.syntax.type_builtin,
            &mut colors.syntax.constructor,
            &mut colors.syntax.function,
            &mut colors.syntax.function_builtin,
            &mut colors.syntax.macro_name,
            &mut colors.syntax.property,
            &mut colors.syntax.constant,
            &mut colors.syntax.variable,
            &mut colors.syntax.variable_special,
            &mut colors.syntax.parameter,
            &mut colors.syntax.operator,
            &mut colors.syntax.punctuation,
            &mut colors.syntax.tag,
            &mut colors.syntax.attribute,
            &mut colors.syntax.label,
            &mut colors.syntax.invalid,
        ];
        for slot in tokens.into_iter().chain(syntax) {
            if slot.a == 1.0 && slot.s <= f32::EPSILON {
                *slot = color::tint(*slot, self.tint.hue, self.tint.chroma);
            }
        }

        if self.accent.chroma > 0.0 {
            let (accent_l, plate_l) = if appearance.is_light() {
                (ACCENT_L.1, PLATE_L.1)
            } else {
                (ACCENT_L.0, PLATE_L.0)
            };
            colors.accent = color::oklch(accent_l, self.accent.chroma, self.accent.hue);
            colors.accent_strong = color::oklch(plate_l, self.accent.chroma, self.accent.hue);
            // Whichever label the plate can actually hold. The shipped accent is
            // the maximum-contrast neutral, where the answer is always the
            // inverse; a chromatic plate at a yellow hue is bright enough that
            // the inverse would be the unreadable one.
            colors.on_accent = label_on(colors.accent_strong, colors);
        }
    }
}

/// The accent's lightness in each appearance — indigo-400's and indigo-600's,
/// the two steps `palette.rs` picked so an accent clears WCAG AA on its own
/// background rather than glowing on one and vanishing on the other.
const ACCENT_L: (f32, f32) = (0.673, 0.511);

/// The lightness of a *plate* carrying [`ThemeColors::on_accent`], taken from
/// `danger_strong` — the palette's existing chromatic plate, already tuned to
/// hold a label in both appearances.
const PLATE_L: (f32, f32) = (0.58, 0.51);

/// Whichever of the palette's two extremes the plate can actually hold.
fn label_on(plate: Hsla, colors: &ThemeColors) -> Hsla {
    let (a, b) = (colors.solid, colors.on_solid);
    if color::contrast_ratio(plate, a) >= color::contrast_ratio(plate, b) {
        a
    } else {
        b
    }
}

/// Read the installed brand (the default before one is set).
pub fn installed(cx: &App) -> Brand {
    cx.try_global::<Brand>().copied().unwrap_or_default()
}
