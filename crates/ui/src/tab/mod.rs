use std::rc::Rc;

use gpui::prelude::FluentBuilder as _;
use gpui::{
    AnyElement, App, ClickEvent, Div, InteractiveElement, IntoElement, MouseButton, ParentElement,
    RenderOnce, SharedString, StatefulInteractiveElement, Styled, Window, div, px, relative,
};
use theme::{ActiveTheme, TABBAR_HEIGHT};

use crate::{Icon, IconName, Selectable, h_flex};

pub mod tab_bar;

#[allow(clippy::type_complexity)]
/// A Tab element for the [`super::TabBar`].
#[derive(IntoElement)]
pub struct Tab {
    ix: usize,
    base: Div,
    pub(super) label: Option<SharedString>,
    icon: Option<Icon>,
    prefix: Option<AnyElement>,
    pub(super) tab_bar_prefix: Option<bool>,
    suffix: Option<AnyElement>,
    children: Vec<AnyElement>,
    pub(super) disabled: bool,
    pub(super) selected: bool,
    on_click: Option<Rc<dyn Fn(&ClickEvent, &mut Window, &mut App) + 'static>>,
}

impl From<&'static str> for Tab {
    fn from(label: &'static str) -> Self {
        Self::new().label(label)
    }
}

impl From<String> for Tab {
    fn from(label: String) -> Self {
        Self::new().label(label)
    }
}

impl From<SharedString> for Tab {
    fn from(label: SharedString) -> Self {
        Self::new().label(label)
    }
}

impl From<Icon> for Tab {
    fn from(icon: Icon) -> Self {
        Self::default().icon(icon)
    }
}

impl From<IconName> for Tab {
    fn from(icon_name: IconName) -> Self {
        Self::default().icon(Icon::new(icon_name))
    }
}

impl Default for Tab {
    fn default() -> Self {
        Self {
            ix: 0,
            base: div(),
            label: None,
            icon: None,
            tab_bar_prefix: None,
            children: Vec::new(),
            disabled: false,
            selected: false,
            prefix: None,
            suffix: None,
            on_click: None,
        }
    }
}

impl Tab {
    /// Create a new tab with a label.
    pub fn new() -> Self {
        Self::default()
    }

    /// Set label for the tab.
    pub fn label(mut self, label: impl Into<SharedString>) -> Self {
        self.label = Some(label.into());
        self
    }

    /// Set icon for the tab.
    pub fn icon(mut self, icon: impl Into<Icon>) -> Self {
        self.icon = Some(icon.into());
        self
    }

    /// Set the left side of the tab
    pub fn prefix(mut self, prefix: impl IntoElement) -> Self {
        self.prefix = Some(prefix.into_any_element());
        self
    }

    /// Set the right side of the tab
    pub fn suffix(mut self, suffix: impl IntoElement) -> Self {
        self.suffix = Some(suffix.into_any_element());
        self
    }

    /// Set disabled state to the tab, default false.
    pub fn disabled(mut self, disabled: bool) -> Self {
        self.disabled = disabled;
        self
    }

    /// Set the click handler for the tab.
    pub fn on_click(
        mut self,
        on_click: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
    ) -> Self {
        self.on_click = Some(Rc::new(on_click));
        self
    }

    /// Set index to the tab.
    pub(crate) fn ix(mut self, ix: usize) -> Self {
        self.ix = ix;
        self
    }

    /// Set if the tab bar has a prefix.
    pub(crate) fn tab_bar_prefix(mut self, tab_bar_prefix: bool) -> Self {
        self.tab_bar_prefix = Some(tab_bar_prefix);
        self
    }
}

impl ParentElement for Tab {
    fn extend(&mut self, elements: impl IntoIterator<Item = AnyElement>) {
        self.children.extend(elements);
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

impl RenderOnce for Tab {
    fn render(self, _: &mut Window, cx: &mut App) -> impl IntoElement {
        let fg = if self.disabled {
            cx.theme().text_muted
        } else if self.selected {
            cx.theme().tab_active_foreground
        } else {
            cx.theme().tab_foreground
        };

        self.base
            .id(self.ix)
            .flex()
            .flex_wrap()
            .gap_1()
            .items_center()
            .flex_shrink_0()
            .h(TABBAR_HEIGHT)
            .overflow_hidden()
            .text_color(fg)
            .text_sm()
            .when(!self.selected && !self.disabled, |this| {
                this.hover(|this| this.text_color(cx.theme().secondary_foreground))
            })
            .when_some(self.prefix, |this, prefix| this.child(prefix))
            .child(
                h_flex()
                    .flex_1()
                    .h(px(30.))
                    .line_height(relative(1.))
                    .whitespace_nowrap()
                    .items_center()
                    .justify_center()
                    .overflow_hidden()
                    .flex_shrink_0()
                    .px_3()
                    .map(|this| match self.icon {
                        Some(icon) => this.w(px(38.)).child(icon.size_4()),
                        None => this
                            .map(|this| match self.label {
                                Some(label) => this.child(label),
                                None => this,
                            })
                            .children(self.children),
                    }),
            )
            .when_some(self.suffix, |this, suffix| {
                this.child(div().pr_2().child(suffix))
            })
            .on_mouse_down(MouseButton::Left, |_ev, _window, cx| {
                cx.stop_propagation();
            })
            .when(!self.disabled, |this| {
                this.when_some(self.on_click.clone(), |this, on_click| {
                    this.on_click(move |event, window, cx| on_click(event, window, cx))
                })
            })
    }
}
