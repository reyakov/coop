use gpui::{div, px, App, Div, Pixels, Refineable, StyleRefinement, Styled};
use serde::{Deserialize, Serialize};
use theme::ActiveTheme;

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
    div().my_2().w_full().h_px().bg(cx.theme().border_variant)
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

    font_weight!(font_thin, THIN);
    font_weight!(font_extralight, EXTRA_LIGHT);
    font_weight!(font_light, LIGHT);
    font_weight!(font_normal, NORMAL);
    font_weight!(font_medium, MEDIUM);
    font_weight!(font_semibold, SEMIBOLD);
    font_weight!(font_bold, BOLD);
    font_weight!(font_extrabold, EXTRA_BOLD);
    font_weight!(font_black, BLACK);

    /// Set as Popover style
    #[inline]
    fn popover_style(self, cx: &mut App) -> Self {
        self.bg(cx.theme().background)
            .border_1()
            .border_color(cx.theme().border)
            .shadow_lg()
            .rounded(cx.theme().radius_lg)
    }
}

impl<E: Styled> StyledExt for E {}

/// A size for elements.
#[derive(Clone, Default, Copy, PartialEq, Eq, Debug, Deserialize, Serialize)]
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

/// A trait for defining element that can be selected.
pub trait Selectable: Sized {
    /// Set the selected state of the element.
    fn selected(self, selected: bool) -> Self;

    /// Returns true if the element is selected.
    fn is_selected(&self) -> bool;

    /// Set is the element mouse right clicked, default do nothing.
    fn secondary_selected(self, _: bool) -> Self {
        self
    }
}

/// A trait for defining element that can be disabled.
pub trait Disableable {
    /// Set the disabled state of the element.
    fn disabled(self, disabled: bool) -> Self;
}

/// A trait for setting the size of an element.
pub trait Sizable: Sized {
    /// Set the ui::Size of this element.
    ///
    /// Also can receive a `ButtonSize` to convert to `IconSize`,
    /// Or a `Pixels` to set a custom size: `px(30.)`
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

#[allow(unused)]
pub trait StyleSized<T: Styled> {
    fn input_font_size(self, size: Size) -> Self;
    fn input_size(self, size: Size) -> Self;
    fn input_pl(self, size: Size) -> Self;
    fn input_pr(self, size: Size) -> Self;
    fn input_px(self, size: Size) -> Self;
    fn input_py(self, size: Size) -> Self;
    fn input_h(self, size: Size) -> Self;
    fn list_size(self, size: Size) -> Self;
    fn list_px(self, size: Size) -> Self;
    fn list_py(self, size: Size) -> Self;
    /// Apply size with the given `Size`.
    fn size_with(self, size: Size) -> Self;
}

impl<T: Styled> StyleSized<T> for T {
    fn input_font_size(self, size: Size) -> Self {
        match size {
            Size::XSmall => self.text_xs(),
            Size::Small => self.text_sm(),
            Size::Medium => self.text_base(),
            Size::Large => self.text_lg(),
            Size::Size(size) => self.text_size(size),
        }
    }

    fn input_size(self, size: Size) -> Self {
        self.input_px(size).input_py(size).input_h(size)
    }

    fn input_pl(self, size: Size) -> Self {
        match size {
            Size::XSmall => self.pl_1(),
            Size::Medium => self.pl_3(),
            Size::Large => self.pl_5(),
            _ => self.pl_2(),
        }
    }

    fn input_pr(self, size: Size) -> Self {
        match size {
            Size::XSmall => self.pr_1(),
            Size::Medium => self.pr_3(),
            Size::Large => self.pr_5(),
            _ => self.pr_2(),
        }
    }

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
            Size::Large => self.py_5(),
            _ => self.py_1(),
        }
    }

    fn input_h(self, size: Size) -> Self {
        match size {
            Size::XSmall => self.h_6(),
            Size::Small => self.h_8(),
            Size::Medium => self.h_9(),
            Size::Large => self.h_12(),
            _ => self.h(px(24.)),
        }
        .input_font_size(size)
    }

    fn list_size(self, size: Size) -> Self {
        self.list_px(size).list_py(size).input_font_size(size)
    }

    fn list_px(self, size: Size) -> Self {
        match size {
            Size::Small => self.px_2(),
            _ => self.px_3(),
        }
    }

    fn list_py(self, size: Size) -> Self {
        match size {
            Size::Large => self.py_2(),
            Size::Medium => self.py_1(),
            Size::Small => self.py_0p5(),
            _ => self.py_1(),
        }
    }

    fn size_with(self, size: Size) -> Self {
        match size {
            Size::Large => self.size_11(),
            Size::Medium => self.size_8(),
            Size::Small => self.size_5(),
            Size::XSmall => self.size_4(),
            Size::Size(size) => self.size(size),
        }
    }
}

/// A trait for defining element that can be collapsed.
pub trait Collapsible {
    fn collapsed(self, collapsed: bool) -> Self;
    fn is_collapsed(&self) -> bool;
}
