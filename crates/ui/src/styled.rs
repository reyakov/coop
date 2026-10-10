use gpui::{App, Div, Pixels, Refineable, StyleRefinement, Styled, div, px};
pub use gpui_base::component_traits::{Disableable, Selectable};
use theme::{ActiveTheme, TextStyle, Typeset as _, surface_radius, surface_shadows};

/// Returns a `Div` as horizontal flex layout.
pub fn h_flex() -> Div {
    div().h_flex()
}

/// Returns a `Div` as vertical flex layout.
pub fn v_flex() -> Div {
    div().v_flex()
}

/// Returns a `Div` as divider.
pub fn divider(cx: &App) -> Div {
    div().my_1().w_full().h_px().bg(cx.theme().border_faint)
}

macro_rules! font_weight {
    ($fn:ident, $const:ident) => {
        /// [docs](https://tailwindcss.com/docs/font-weight)
        fn $fn(self) -> Self {
            self.font_weight(gpui::FontWeight::$const)
        }
    };
}

/// Extends [`gpui::Styled`] with specific styling methods.
pub trait StyledExt: Styled + Sized {
    /// Refine the style of this element, applying the given style refinement.
    fn refine_style(mut self, style: &StyleRefinement) -> Self {
        self.style().refine(style);
        self
    }

    /// Apply self into a horizontal flex layout.
    #[inline]
    fn h_flex(self) -> Self {
        self.flex().flex_row().items_center()
    }

    /// Apply self into a vertical flex layout.
    #[inline]
    fn v_flex(self) -> Self {
        self.flex().flex_col()
    }

    font_weight!(font_normal, NORMAL);
    font_weight!(font_medium, MEDIUM);
    font_weight!(font_semibold, SEMIBOLD);

    /// Set as Popover style
    #[inline]
    fn popover_style(self, cx: &mut App) -> Self {
        let shadows = cx.theme().shadow.then(surface_shadows).unwrap_or_default();

        self.bg(cx.theme().surface_overlay)
            .border_1()
            .border_color(cx.theme().border)
            .shadow(shadows)
            .rounded(px(surface_radius()))
    }
}

impl<E: Styled> StyledExt for E {}

/// A size for elements.
#[derive(Clone, Default, Copy, PartialEq, Eq, Debug)]
pub enum Size {
    Size(Pixels),
    XSmall,
    Small,
    #[default]
    Medium,
    Large,
}

impl From<Pixels> for Size {
    fn from(size: Pixels) -> Self {
        Size::Size(size)
    }
}

/// A trait for setting the size of an element.
pub trait Sizable: Sized {
    /// Set the element's [`Size`], or a custom pixel size (`px(30.)`).
    fn with_size(self, size: impl Into<Size>) -> Self;

    /// Set to Size::XSmall
    fn xsmall(self) -> Self {
        self.with_size(Size::XSmall)
    }

    /// Set to Size::Small
    fn small(self) -> Self {
        self.with_size(Size::Small)
    }

    /// Set to Size::Medium
    fn medium(self) -> Self {
        self.with_size(Size::Medium)
    }

    /// Set to Size::Large
    fn large(self) -> Self {
        self.with_size(Size::Large)
    }
}

pub(crate) trait StyleSized<T: Styled> {
    fn input_px(self, size: Size) -> Self;
    fn input_py(self, size: Size) -> Self;
    fn input_h(self, size: Size) -> Self;
}

impl<T: Styled> StyleSized<T> for T {
    fn input_px(self, size: Size) -> Self {
        match size {
            Size::XSmall => self.px_1(),
            Size::Medium => self.px_3(),
            Size::Large => self.px_5(),
            _ => self.px_2(),
        }
    }

    fn input_py(self, size: Size) -> Self {
        match size {
            Size::XSmall => self.py_0p5(),
            Size::Medium => self.py_2(),
            Size::Large => self.py_1p5(),
            _ => self.py_1(),
        }
    }

    fn input_h(self, size: Size) -> Self {
        match size {
            Size::XSmall => self.h_6().text_style(TextStyle::Caption),
            Size::Small => self.h_7().text_style(TextStyle::Callout),
            Size::Medium => self.h_8().text_style(TextStyle::Body),
            Size::Large => self.h_10().text_style(TextStyle::Title3),
            Size::Size(size) => self.h(size).text_size(size * 0.5),
        }
    }
}
