use gpui::{Hsla, Rgba, hsla};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::scale::{ColorScale, ColorScaleSet};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize, Serialize, JsonSchema)]
pub struct ThemeColors {
    // Surface colors
    pub background: Hsla,
    pub surface_background: Hsla,
    pub elevated_surface_background: Hsla,
    pub panel_background: Hsla,
    pub overlay: Hsla,
    pub title_bar: Hsla,
    pub title_bar_inactive: Hsla,
    pub window_border: Hsla,

    // Border colors
    pub border: Hsla,
    pub border_variant: Hsla,
    pub border_focused: Hsla,
    pub border_selected: Hsla,
    pub border_transparent: Hsla,
    pub border_disabled: Hsla,
    pub ring: Hsla,

    // Text colors
    pub text: Hsla,
    pub text_muted: Hsla,
    pub text_placeholder: Hsla,
    pub text_accent: Hsla,
    pub text_danger: Hsla,
    pub text_warning: Hsla,

    // Icon colors
    pub icon: Hsla,
    pub icon_muted: Hsla,
    pub icon_accent: Hsla,

    // Element colors
    pub element_foreground: Hsla,
    pub element_background: Hsla,
    pub element_hover: Hsla,
    pub element_active: Hsla,
    pub element_selected: Hsla,
    pub element_disabled: Hsla,

    // Secondary element colors
    pub secondary_foreground: Hsla,
    pub secondary_background: Hsla,
    pub secondary_hover: Hsla,
    pub secondary_active: Hsla,
    pub secondary_selected: Hsla,
    pub secondary_disabled: Hsla,

    // Danger element colors
    pub danger_foreground: Hsla,
    pub danger_background: Hsla,
    pub danger_hover: Hsla,
    pub danger_active: Hsla,
    pub danger_selected: Hsla,
    pub danger_disabled: Hsla,

    // Warning element colors
    pub warning_foreground: Hsla,
    pub warning_background: Hsla,
    pub warning_hover: Hsla,
    pub warning_active: Hsla,
    pub warning_selected: Hsla,
    pub warning_disabled: Hsla,

    // Ghost element colors
    pub ghost_element_background: Hsla,
    pub ghost_element_background_alt: Hsla,
    pub ghost_element_hover: Hsla,
    pub ghost_element_active: Hsla,
    pub ghost_element_selected: Hsla,
    pub ghost_element_disabled: Hsla,

    // Tab colors
    pub tab_background: Hsla,
    pub tab_foreground: Hsla,
    pub tab_hover_background: Hsla,
    pub tab_active_background: Hsla,
    pub tab_active_foreground: Hsla,

    // Scrollbar colors
    pub scrollbar_thumb_background: Hsla,
    pub scrollbar_thumb_hover_background: Hsla,
    pub scrollbar_thumb_border: Hsla,
    pub scrollbar_track_background: Hsla,
    pub scrollbar_track_border: Hsla,

    // Interactive colors
    pub drop_target_background: Hsla,
    pub cursor: Hsla,
    pub selection: Hsla,
}

/// The default colors for the theme.
///
/// Themes that do not specify all colors are refined off of these defaults.
impl ThemeColors {
    /// Returns the default colors for light themes.
    ///
    /// Themes that do not specify all colors are refined off of these defaults.
    pub(crate) fn light() -> Self {
        Self {
            background: neutral().light().step_1(),
            surface_background: neutral().light().step_2(),
            elevated_surface_background: neutral().light().step_3(),
            panel_background: neutral().light().step_1(),
            overlay: neutral().light_alpha().step_3(),
            title_bar: neutral().light().step_2(),
            title_bar_inactive: neutral().light().step_1(),
            window_border: hsl(240.0, 5.9, 78.0),

            border: neutral().light().step_6(),
            border_variant: neutral().light().step_4(),
            border_focused: brand().light().step_7(),
            border_selected: brand().light().step_7(),
            border_transparent: gpui::transparent_black(),
            border_disabled: neutral().light().step_3(),
            ring: brand().light().step_8(),

            text: neutral().light().step_12(),
            text_muted: neutral().light().step_11(),
            text_placeholder: neutral().light().step_10(),
            text_accent: brand().light().step_9(),
            text_danger: danger().light().step_9(),
            text_warning: warning().light().step_9(),

            icon: neutral().light().step_11(),
            icon_muted: neutral().light().step_10(),
            icon_accent: brand().light().step_11(),

            element_foreground: brand().light().step_12(),
            element_background: brand().light().step_9(),
            element_hover: brand().light_alpha().step_10(),
            element_active: brand().light().step_10(),
            element_selected: brand().light().step_11(),
            element_disabled: brand().light_alpha().step_3(),

            secondary_foreground: brand().light().step_11(),
            secondary_background: brand().light().step_3(),
            secondary_hover: brand().light_alpha().step_4(),
            secondary_active: brand().light().step_5(),
            secondary_selected: brand().light().step_5(),
            secondary_disabled: brand().light_alpha().step_3(),

            danger_foreground: danger().light().step_12(),
            danger_background: danger().light().step_3(),
            danger_hover: danger().light_alpha().step_4(),
            danger_active: danger().light().step_5(),
            danger_selected: danger().light().step_5(),
            danger_disabled: danger().light_alpha().step_3(),

            warning_foreground: warning().light().step_12(),
            warning_background: warning().light().step_3(),
            warning_hover: warning().light_alpha().step_4(),
            warning_active: warning().light().step_5(),
            warning_selected: warning().light().step_5(),
            warning_disabled: warning().light_alpha().step_3(),

            ghost_element_background: gpui::transparent_black(),
            ghost_element_background_alt: neutral().light().step_3(),
            ghost_element_hover: neutral().light_alpha().step_4(),
            ghost_element_active: neutral().light().step_5(),
            ghost_element_selected: neutral().light().step_5(),
            ghost_element_disabled: neutral().light_alpha().step_2(),

            tab_background: neutral().light().step_3(),
            tab_foreground: neutral().light().step_11(),
            tab_hover_background: neutral().light_alpha().step_4(),
            tab_active_background: neutral().light().step_1(),
            tab_active_foreground: neutral().light().step_12(),

            scrollbar_thumb_background: neutral().light_alpha().step_3(),
            scrollbar_thumb_hover_background: neutral().light_alpha().step_4(),
            scrollbar_thumb_border: gpui::transparent_black(),
            scrollbar_track_background: gpui::transparent_black(),
            scrollbar_track_border: gpui::transparent_black(),

            drop_target_background: brand().light_alpha().step_2(),
            cursor: hsl(200., 100., 50.),
            selection: hsl(200., 100., 50.).alpha(0.25),
        }
    }

    /// Returns the default colors for dark themes.
    ///
    /// Themes that do not specify all colors are refined off of these defaults.
    pub(crate) fn dark() -> Self {
        Self {
            background: neutral().dark().step_1(),
            surface_background: neutral().dark().step_2(),
            elevated_surface_background: neutral().dark().step_3(),
            panel_background: neutral().dark().step_1(),
            overlay: neutral().dark_alpha().step_3(),
            title_bar: neutral().dark().step_2(),
            title_bar_inactive: neutral().dark().step_1(),
            window_border: hsl(240.0, 3.7, 28.0),

            border: neutral().dark().step_6(),
            border_variant: neutral().dark().step_4(),
            border_focused: brand().dark().step_7(),
            border_selected: brand().dark().step_7(),
            border_transparent: gpui::transparent_black(),
            border_disabled: neutral().dark().step_3(),
            ring: brand().dark().step_8(),

            text: neutral().dark().step_12(),
            text_muted: neutral().dark().step_11(),
            text_placeholder: neutral().dark().step_10(),
            text_accent: brand().dark().step_9(),
            text_danger: danger().dark().step_9(),
            text_warning: warning().dark().step_9(),

            icon: neutral().dark().step_11(),
            icon_muted: neutral().dark().step_10(),
            icon_accent: brand().dark().step_11(),

            element_foreground: brand().dark().step_1(),
            element_background: brand().dark().step_9(),
            element_hover: brand().dark_alpha().step_10(),
            element_active: brand().dark().step_10(),
            element_selected: brand().dark().step_11(),
            element_disabled: brand().dark_alpha().step_3(),

            secondary_foreground: brand().dark().step_12(),
            secondary_background: brand().dark().step_3(),
            secondary_hover: brand().dark_alpha().step_4(),
            secondary_active: brand().dark().step_5(),
            secondary_selected: brand().dark().step_5(),
            secondary_disabled: brand().dark_alpha().step_3(),

            danger_foreground: danger().dark().step_12(),
            danger_background: danger().dark().step_3(),
            danger_hover: danger().dark_alpha().step_4(),
            danger_active: danger().dark().step_5(),
            danger_selected: danger().dark().step_5(),
            danger_disabled: danger().dark_alpha().step_3(),

            warning_foreground: warning().dark().step_12(),
            warning_background: warning().dark().step_3(),
            warning_hover: warning().dark_alpha().step_4(),
            warning_active: warning().dark().step_5(),
            warning_selected: warning().dark().step_5(),
            warning_disabled: warning().dark_alpha().step_3(),

            ghost_element_background: gpui::transparent_black(),
            ghost_element_background_alt: neutral().dark().step_3(),
            ghost_element_hover: neutral().dark_alpha().step_4(),
            ghost_element_active: neutral().dark().step_5(),
            ghost_element_selected: neutral().dark().step_5(),
            ghost_element_disabled: neutral().dark_alpha().step_2(),

            tab_background: neutral().dark().step_3(),
            tab_foreground: neutral().dark().step_11(),
            tab_hover_background: neutral().dark_alpha().step_4(),
            tab_active_background: neutral().dark().step_1(),
            tab_active_foreground: neutral().dark().step_12(),

            scrollbar_thumb_background: neutral().dark_alpha().step_3(),
            scrollbar_thumb_hover_background: neutral().dark_alpha().step_4(),
            scrollbar_thumb_border: gpui::transparent_black(),
            scrollbar_track_background: gpui::transparent_black(),
            scrollbar_track_border: gpui::transparent_black(),

            drop_target_background: brand().dark_alpha().step_2(),
            cursor: hsl(200., 100., 50.),
            selection: hsl(200., 100., 50.).alpha(0.25),
        }
    }
}

fn neutral() -> ColorScaleSet {
    gray()
}

fn brand() -> ColorScaleSet {
    yellow()
}

fn danger() -> ColorScaleSet {
    tomato()
}

fn warning() -> ColorScaleSet {
    orange()
}

/// Make a [gpui::Hsla] color.
///
/// - h: 0..360.0
/// - s: 0.0..100.0
/// - l: 0.0..100.0
pub(crate) fn hsl(h: f32, s: f32, l: f32) -> Hsla {
    hsla(h / 360., s / 100.0, l / 100.0, 1.0)
}

type StaticColorScale = [&'static str; 12];

struct StaticColorScaleSet {
    light: StaticColorScale,
    light_alpha: StaticColorScale,
    dark: StaticColorScale,
    dark_alpha: StaticColorScale,
}

impl TryFrom<StaticColorScaleSet> for ColorScaleSet {
    type Error = anyhow::Error;

    fn try_from(value: StaticColorScaleSet) -> Result<Self, Self::Error> {
        fn to_color_scale(scale: StaticColorScale) -> Result<ColorScale, anyhow::Error> {
            scale
                .into_iter()
                .map(|color| Rgba::try_from(color).map(Hsla::from))
                .collect::<Result<Vec<_>, _>>()
                .map(ColorScale::from_iter)
        }

        Ok(Self::new(
            to_color_scale(value.light)?,
            to_color_scale(value.light_alpha)?,
            to_color_scale(value.dark)?,
            to_color_scale(value.dark_alpha)?,
        ))
    }
}

pub(crate) fn gray() -> ColorScaleSet {
    StaticColorScaleSet {
        light: [
            "#fcfcfcff",
            "#f9f9f9ff",
            "#f0f0f0ff",
            "#e8e8e8ff",
            "#e0e0e0ff",
            "#d9d9d9ff",
            "#cececeff",
            "#bbbbbbff",
            "#8d8d8dff",
            "#838383ff",
            "#646464ff",
            "#202020ff",
        ],
        light_alpha: [
            "#00000003",
            "#00000006",
            "#0000000f",
            "#00000017",
            "#0000001f",
            "#00000026",
            "#00000031",
            "#00000044",
            "#00000072",
            "#0000007c",
            "#0000009b",
            "#000000df",
        ],
        dark: [
            "#111111ff",
            "#191919ff",
            "#222222ff",
            "#2a2a2aff",
            "#313131ff",
            "#3a3a3aff",
            "#484848ff",
            "#606060ff",
            "#6e6e6eff",
            "#7b7b7bff",
            "#b4b4b4ff",
            "#eeeeeeff",
        ],
        dark_alpha: [
            "#00000000",
            "#ffffff09",
            "#ffffff12",
            "#ffffff1b",
            "#ffffff22",
            "#ffffff2c",
            "#ffffff3b",
            "#ffffff55",
            "#ffffff64",
            "#ffffff72",
            "#ffffffaf",
            "#ffffffed",
        ],
    }
    .try_into()
    .unwrap()
}

pub(crate) fn yellow() -> ColorScaleSet {
    StaticColorScaleSet {
        light: [
            "#fdfdf9ff",
            "#fefce9ff",
            "#fffab8ff",
            "#fff394ff",
            "#ffe770ff",
            "#f3d768ff",
            "#e4c767ff",
            "#d5ae39ff",
            "#ffe629ff",
            "#ffdc00ff",
            "#9e6c00ff",
            "#473b1fff",
        ],
        light_alpha: [
            "#aaaa0006",
            "#f4dd0016",
            "#ffee0047",
            "#ffe3016b",
            "#ffd5008f",
            "#ebbc0097",
            "#d2a10098",
            "#c99700c6",
            "#ffe100d6",
            "#ffdc00ff",
            "#9e6c00ff",
            "#2e2000e0",
        ],
        dark: [
            "#14120bff",
            "#1b180fff",
            "#2d2305ff",
            "#362b00ff",
            "#433500ff",
            "#524202ff",
            "#665417ff",
            "#836a21ff",
            "#ffe629ff",
            "#ffff57ff",
            "#f5e147ff",
            "#f6eeb4ff",
        ],
        dark_alpha: [
            "#d1510004",
            "#f9b4000b",
            "#ffaa001e",
            "#fdb70028",
            "#febb0036",
            "#fec40046",
            "#fdcb225c",
            "#fdca327b",
            "#ffe629ff",
            "#ffff57ff",
            "#fee949f5",
            "#fef6baf6",
        ],
    }
    .try_into()
    .unwrap()
}

pub(crate) fn orange() -> ColorScaleSet {
    StaticColorScaleSet {
        light: [
            "#fefcfbff",
            "#fff7edff",
            "#ffefd6ff",
            "#ffdfb5ff",
            "#ffd19aff",
            "#ffc182ff",
            "#f5ae73ff",
            "#ec9455ff",
            "#f76b15ff",
            "#ef5f00ff",
            "#cc4e00ff",
            "#582d1dff",
        ],
        light_alpha: [
            "#c0400004",
            "#ff8e0012",
            "#ff9c0029",
            "#ff91014a",
            "#ff8b0065",
            "#ff81007d",
            "#ed6c008c",
            "#e35f00aa",
            "#f65e00ea",
            "#ef5f00ff",
            "#cc4e00ff",
            "#431200e2",
        ],
        dark: [
            "#17120eff",
            "#1e160fff",
            "#331e0bff",
            "#462100ff",
            "#562800ff",
            "#66350cff",
            "#7e451dff",
            "#a35829ff",
            "#f76b15ff",
            "#ff801fff",
            "#ffa057ff",
            "#ffe0c2ff",
        ],
        dark_alpha: [
            "#ec360007",
            "#fe6d000e",
            "#fb6a0025",
            "#ff590039",
            "#ff61004a",
            "#fd75045c",
            "#ff832c75",
            "#fe84389d",
            "#fe6d15f7",
            "#ff801fff",
            "#ffa057ff",
            "#ffe0c2ff",
        ],
    }
    .try_into()
    .unwrap()
}

pub(crate) fn tomato() -> ColorScaleSet {
    StaticColorScaleSet {
        light: [
            "#fffcfcff",
            "#fff8f7ff",
            "#feebe7ff",
            "#ffdcd3ff",
            "#ffcdc2ff",
            "#fdbdafff",
            "#f5a898ff",
            "#ec8e7bff",
            "#e54d2eff",
            "#dd4425ff",
            "#d13415ff",
            "#5c271fff",
        ],
        light_alpha: [
            "#ff000003",
            "#ff200008",
            "#f52b0018",
            "#ff35002c",
            "#ff2e003d",
            "#f92d0050",
            "#e7280067",
            "#db250084",
            "#df2600d1",
            "#d72400da",
            "#cd2200ea",
            "#460900e0",
        ],
        dark: [
            "#181111ff",
            "#1f1513ff",
            "#391714ff",
            "#4e1511ff",
            "#5e1c16ff",
            "#6e2920ff",
            "#853a2dff",
            "#ac4d39ff",
            "#e54d2eff",
            "#ec6142ff",
            "#ff977dff",
            "#fbd3cbff",
        ],
        dark_alpha: [
            "#f1121208",
            "#ff55330f",
            "#ff35232b",
            "#fd201142",
            "#fe332153",
            "#ff4f3864",
            "#fd644a7d",
            "#fe6d4ea7",
            "#fe5431e4",
            "#ff6847eb",
            "#ff977dff",
            "#ffd6cefb",
        ],
    }
    .try_into()
    .unwrap()
}
