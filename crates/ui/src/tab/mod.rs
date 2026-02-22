use gpui::prelude::FluentBuilder;
use gpui::{
    div, px, AnyElement, App, Div, InteractiveElement, IntoElement, MouseButton, ParentElement,
    RenderOnce, StatefulInteractiveElement, Styled, Window,
};
use theme::{ActiveTheme, TABBAR_HEIGHT};

use crate::{Selectable, Sizable, Size};

pub mod tab_bar;

#[derive(IntoElement)]
pub struct Tab {
    ix: usize,
    base: Div,
    label: Option<AnyElement>,
    prefix: Option<AnyElement>,
    suffix: Option<AnyElement>,
    disabled: bool,
    selected: bool,
    size: Size,
}

impl Tab {
    pub fn new() -> Self {
        Self {
            ix: 0,
            base: div(),
            label: None,
            disabled: false,
            selected: false,
            prefix: None,
            suffix: None,
            size: Size::default(),
        }
    }

    /// Set label for the tab.
    pub fn label(mut self, label: impl Into<AnyElement>) -> Self {
        self.label = Some(label.into());
        self
    }

    /// Set the left side of the tab
    pub fn prefix(mut self, prefix: impl Into<AnyElement>) -> Self {
        self.prefix = Some(prefix.into());
        self
    }

    /// Set the right side of the tab
    pub fn suffix(mut self, suffix: impl Into<AnyElement>) -> Self {
        self.suffix = Some(suffix.into());
        self
    }

    /// Set disabled state to the tab
    pub fn disabled(mut self, disabled: bool) -> Self {
        self.disabled = disabled;
        self
    }

    /// Set index to the tab.
    pub fn ix(mut self, ix: usize) -> Self {
        self.ix = ix;
        self
    }
}

impl Default for Tab {
    fn default() -> Self {
        Self::new()
    }
}

impl Selectable for Tab {
    fn selected(mut self, selected: bool) -> Self {
        self.selected = selected;
        self
    }

    fn is_selected(&self) -> bool {
        self.selected
    }
}

impl InteractiveElement for Tab {
    fn interactivity(&mut self) -> &mut gpui::Interactivity {
        self.base.interactivity()
    }
}

impl StatefulInteractiveElement for Tab {}

impl Styled for Tab {
    fn style(&mut self) -> &mut gpui::StyleRefinement {
        self.base.style()
    }
}

impl Sizable for Tab {
    fn with_size(mut self, size: impl Into<Size>) -> Self {
        self.size = size.into();
        self
    }
}

impl RenderOnce for Tab {
    fn render(self, _window: &mut Window, cx: &mut App) -> impl IntoElement {
        let (text_color, hover_text_color, bg_color, border_color) =
            match (self.selected, self.disabled) {
                (true, false) => (
                    cx.theme().tab_active_foreground,
                    cx.theme().tab_hover_foreground,
                    cx.theme().tab_active_background,
                    cx.theme().border,
                ),
                (false, false) => (
                    cx.theme().tab_inactive_foreground,
                    cx.theme().tab_hover_foreground,
                    cx.theme().ghost_element_background,
                    cx.theme().border_transparent,
                ),
                (true, true) => (
                    cx.theme().tab_inactive_foreground,
                    cx.theme().tab_hover_foreground,
                    cx.theme().ghost_element_background,
                    cx.theme().border_disabled,
                ),
                (false, true) => (
                    cx.theme().tab_inactive_foreground,
                    cx.theme().tab_hover_foreground,
                    cx.theme().ghost_element_background,
                    cx.theme().border_disabled,
                ),
            };

        self.base
            .id(self.ix)
            .h(TABBAR_HEIGHT)
            .px_4()
            .relative()
            .flex()
            .items_center()
            .flex_shrink_0()
            .cursor_pointer()
            .overflow_hidden()
            .text_xs()
            .text_ellipsis()
            .text_color(text_color)
            .bg(bg_color)
            .border_l(px(1.))
            .border_r(px(1.))
            .border_color(border_color)
            .when(!self.selected && !self.disabled, |this| {
                this.hover(|this| this.text_color(hover_text_color))
            })
            .when_some(self.prefix, |this, prefix| {
                this.child(prefix).text_color(text_color)
            })
            .when_some(self.label, |this, label| this.child(label))
            .when_some(self.suffix, |this, suffix| this.child(suffix))
            .on_mouse_down(MouseButton::Left, |_ev, _window, cx| {
                cx.stop_propagation();
            })
    }
}
