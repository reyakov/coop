use gpui::{hsla, rgb};

use crate::paint::{band_for, hairline_for};
use crate::syntax::SyntaxPalette;
use crate::{Ink, TextInk, ThemeColors, ThemeMode, color};

pub(crate) fn dark() -> ThemeColors {
    let warning = color::oklch(0.828, 0.189, 84.429);
    ThemeColors {
        bg: color::grey(6),
        surface: color::grey(13),
        surface_raised: color::neutral(0.235),
        surface_card: color::grey(0x0e),
        surface_dialog: color::grey(0x10),
        surface_overlay: color::grey(0x16),
        element_hover: hsla(0.0, 0.0, 1.0, 0.08),
        element_active: hsla(0.0, 0.0, 1.0, 0.12),

        border_faint: hairline_for(ThemeMode::Dark, 0.06),
        border: hairline_for(ThemeMode::Dark, 0.08),
        border_strong: hairline_for(ThemeMode::Dark, 0.14),

        text: TextInk::color(ThemeMode::Dark, Ink::APPKIT.dark.text),
        text_muted: TextInk::color(ThemeMode::Dark, Ink::APPKIT.dark.muted),
        text_faint: TextInk::color(ThemeMode::Dark, Ink::APPKIT.dark.faint),
        text_dim: color::grey(0x98),

        // The restored Radix yellow brand: #ffe629 plate, #14120b label.
        solid: color::oklch(0.918, 0.184, 100.9),
        on_solid: color::grey(0x0e),

        // The restored Radix yellow brand: #f5e147 ink, #ffe629 plate.
        accent: color::oklch(0.900, 0.166, 101.7),
        accent_strong: color::oklch(0.918, 0.184, 100.9),
        on_accent: color::grey(0x0e),

        danger: color::oklch(0.704, 0.191, 22.216),
        danger_muted: color::oklch(0.808, 0.114, 19.571),
        danger_strong: color::oklch(0.58, 0.16, 25.0),
        success: color::oklch(0.765, 0.177, 163.223),
        success_muted: color::oklch(0.845, 0.143, 164.978),
        busy: color::oklch(0.718, 0.202, 349.761),

        surface_raised_hover: color::neutral(0.29),
        band: band_for(ThemeMode::Dark),
        input_bg: hsla(0.0, 0.0, 1.0, 0.03),
        selection: rgb(0x3f638b).into(),
        cursor: color::neutral(0.94),
        caret: color::neutral(0.673),
        ring: hairline_for(ThemeMode::Dark, 0.35),
        drop_line: hairline_for(ThemeMode::Dark, 0.35),
        drop_target: hsla(0.0, 0.0, 1.0, 0.10),

        code_text: color::neutral(0.94),
        code_wash: hsla(0.0, 0.0, 1.0, 0.08),
        syntax: SyntaxPalette::dark(
            color::neutral(0.922),
            color::neutral(0.60),
            color::oklch(0.704, 0.191, 22.216),
        ),
        diff_add: color::oklch(0.765, 0.177, 163.223),
        diff_del: color::oklch(0.704, 0.191, 22.216),
        diff_hunk_bg: hsla(0.6, 0.35, 0.6, 0.05),

        warning,
        warning_muted: warning.alpha(0.16),
    }
}

pub(crate) fn light() -> ThemeColors {
    let warning = color::oklch(0.555, 0.163, 48.998);
    ThemeColors {
        bg: color::grey(0xff),
        surface: color::neutral(0.968),
        surface_raised: color::neutral(0.940),
        surface_card: color::grey(0xff),
        surface_dialog: color::grey(0xff),
        surface_overlay: color::grey(0xff),
        element_hover: hsla(0.0, 0.0, 0.0, 0.04),
        element_active: hsla(0.0, 0.0, 0.0, 0.06),

        border_faint: hairline_for(ThemeMode::Light, 0.06),
        border: hairline_for(ThemeMode::Light, 0.08),
        border_strong: hairline_for(ThemeMode::Light, 0.14),

        text: TextInk::color(ThemeMode::Light, Ink::APPKIT.light.text),
        text_muted: color::neutral(0.45),
        text_faint: TextInk::color(ThemeMode::Light, Ink::APPKIT.light.faint),
        text_dim: color::neutral(0.50),

        // The restored Radix yellow brand: #ffe629 plate, #473b1f label.
        solid: color::oklch(0.918, 0.184, 100.9),
        on_solid: color::oklch(0.358, 0.046, 86.9),

        // The restored Radix yellow brand: #9e6c00 ink, #ffe629 plate.
        accent: color::oklch(0.569, 0.119, 76.8),
        accent_strong: color::oklch(0.918, 0.184, 100.9),
        on_accent: color::neutral(0.205),

        danger: color::oklch(0.577, 0.245, 27.325),
        danger_muted: color::oklch(0.505, 0.213, 27.518),
        danger_strong: color::oklch(0.51, 0.20, 25.0),
        warning,
        warning_muted: warning.alpha(0.10),
        success: color::oklch(0.596, 0.145, 163.225),
        success_muted: color::oklch(0.508, 0.118, 165.612),
        busy: color::oklch(0.592, 0.249, 0.584),

        surface_raised_hover: color::neutral(0.900),
        band: band_for(ThemeMode::Light),
        input_bg: color::neutral(0.950),
        selection: rgb(0xb3d7ff).into(),
        cursor: color::neutral(0.205),
        caret: color::neutral(0.511),
        ring: hairline_for(ThemeMode::Light, 0.35),
        drop_line: hairline_for(ThemeMode::Light, 0.35),
        drop_target: hsla(0.0, 0.0, 0.0, 0.06),

        code_text: color::neutral(0.18),
        code_wash: hsla(0.0, 0.0, 0.0, 0.06),
        syntax: SyntaxPalette::light(
            color::neutral(0.25),
            color::neutral(0.48),
            color::oklch(0.505, 0.213, 27.518),
        ),
        diff_add: color::oklch(0.596, 0.145, 163.225),
        diff_del: color::oklch(0.577, 0.245, 27.325),
        diff_hunk_bg: hsla(0.6, 0.35, 0.35, 0.07),
    }
}

pub(crate) fn for_appearance(appearance: ThemeMode) -> ThemeColors {
    match appearance {
        ThemeMode::Dark => dark(),
        ThemeMode::Light => light(),
    }
}
