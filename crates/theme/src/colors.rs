use gpui::Hsla;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::syntax::SyntaxPalette;
use crate::{ThemeMode, palette};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize, JsonSchema)]
#[serde(default)]
pub struct ThemeColors {
    /// The main content plane.
    pub bg: Hsla,
    /// The shell / sidebar plane.
    pub surface: Hsla,
    /// Opaque pills and chips raised above the panel.
    pub surface_raised: Hsla,
    /// An inline card resting on the main plane.
    pub surface_card: Hsla,
    /// A modal dialog, over the scrim.
    pub surface_dialog: Hsla,
    /// Popovers and menus — the highest plane.
    pub surface_overlay: Hsla,
    /// Row hover — translucent ink over whatever is beneath.
    pub element_hover: Hsla,
    /// Pressed or selected, one rung past [`ThemeColors::element_hover`].
    pub element_active: Hsla,

    /// The standard hairline border.
    pub border: Hsla,
    /// Dividers inside cards, nested frames.
    pub border_faint: Hsla,
    /// Focused or raised edges.
    pub border_strong: Hsla,
    /// The focus ring.
    pub ring: Hsla,

    /// Primary text.
    pub text: Hsla,
    /// Secondary text.
    pub text_muted: Hsla,
    /// Placeholders and disabled text.
    pub text_faint: Hsla,
    /// Diff file paths — one notch below [`ThemeColors::text_muted`].
    pub text_dim: Hsla,

    /// The text cursor.
    pub cursor: Hsla,
    /// The selected-text wash.
    pub selection: Hsla,

    /// The primary button plate — the maximum-contrast fill.
    pub solid: Hsla,
    /// Label ink on [`ThemeColors::solid`].
    pub on_solid: Hsla,

    /// Emphasis for text and icons; neutral by default.
    pub accent: Hsla,
    /// Accent fills carrying [`ThemeColors::on_accent`].
    pub accent_strong: Hsla,
    /// Label ink on [`ThemeColors::accent_strong`].
    pub on_accent: Hsla,

    /// Errors and destructive actions.
    pub danger: Hsla,
    /// Inline error copy.
    pub danger_muted: Hsla,
    /// The destructive plate — white ink in both appearances.
    pub danger_strong: Hsla,
    /// Warnings.
    pub warning: Hsla,
    /// The translucent warning wash behind chips and banners.
    pub warning_muted: Hsla,
    /// Success and online presence.
    pub success: Hsla,
    /// Text on success-tinted chips.
    pub success_muted: Hsla,
    /// Working or streaming states.
    pub busy: Hsla,

    /// Hover over [`ThemeColors::surface_raised`].
    pub surface_raised_hover: Hsla,
    /// A recessed header / footer strip.
    pub band: Hsla,
    /// Input backgrounds.
    pub input_bg: Hsla,
    /// The text caret.
    pub caret: Hsla,
    /// The line marking where a drag will land.
    pub drop_line: Hsla,
    /// The fill of the region a dragged pane would dock into.
    pub drop_target: Hsla,

    /// Inline-code text.
    pub code_text: Hsla,
    /// The wash behind [`ThemeColors::code_text`].
    pub code_wash: Hsla,
    /// The shared syntax palette, read at paint time.
    pub syntax: SyntaxPalette,
    /// Diff additions.
    pub diff_add: Hsla,
    /// Diff deletions.
    pub diff_del: Hsla,
    /// The diff hunk-header wash.
    pub diff_hunk_bg: Hsla,
}

impl Default for ThemeColors {
    fn default() -> Self {
        palette::dark()
    }
}

impl ThemeColors {
    /// Returns the default colors for an appearance.
    pub fn for_appearance(appearance: ThemeMode) -> Self {
        palette::for_appearance(appearance)
    }
}
