use gpui::prelude::FluentBuilder as _;
#[cfg(not(target_os = "windows"))]
use gpui::Pixels;
use gpui::{
    div, px, AnyElement, App, Div, InteractiveElement, IntoElement, ParentElement, RenderOnce,
    ScrollHandle, StatefulInteractiveElement as _, StyleRefinement, Styled, Window,
};
use smallvec::SmallVec;
use theme::ActiveTheme;

use crate::{h_flex, Sizable, Size, StyledExt};

#[derive(IntoElement)]
pub struct TabBar {
    base: Div,
    style: StyleRefinement,
    scroll_handle: Option<ScrollHandle>,
    prefix: Option<AnyElement>,
    suffix: Option<AnyElement>,
    last_empty_space: AnyElement,
    children: SmallVec<[AnyElement; 2]>,
    size: Size,
}

impl TabBar {
    pub fn new() -> Self {
        Self {
            base: h_flex().px(px(-1.)),
            style: StyleRefinement::default(),
            scroll_handle: None,
            children: SmallVec::new(),
            prefix: None,
            suffix: None,
            size: Size::default(),
            last_empty_space: div().w_3().into_any_element(),
        }
    }

    /// Track the scroll of the TabBar.
    pub fn track_scroll(mut self, scroll_handle: &ScrollHandle) -> Self {
        self.scroll_handle = Some(scroll_handle.clone());
        self
    }

    /// Set the prefix element of the TabBar
    pub fn prefix(mut self, prefix: impl IntoElement) -> Self {
        self.prefix = Some(prefix.into_any_element());
        self
    }

    /// Set the suffix element of the TabBar
    pub fn suffix(mut self, suffix: impl IntoElement) -> Self {
        self.suffix = Some(suffix.into_any_element());
        self
    }

    /// Set the last empty space element of the TabBar.
    pub fn last_empty_space(mut self, last_empty_space: impl IntoElement) -> Self {
        self.last_empty_space = last_empty_space.into_any_element();
        self
    }

    #[cfg(not(target_os = "windows"))]
    pub fn height(window: &mut Window) -> Pixels {
        (1.75 * window.rem_size()).max(px(36.))
    }
}

impl Default for TabBar {
    fn default() -> Self {
        Self::new()
    }
}

impl ParentElement for TabBar {
    fn extend(&mut self, elements: impl IntoIterator<Item = AnyElement>) {
        self.children.extend(elements)
    }
}

impl Styled for TabBar {
    fn style(&mut self) -> &mut StyleRefinement {
        &mut self.style
    }
}

impl Sizable for TabBar {
    fn with_size(mut self, size: impl Into<Size>) -> Self {
        self.size = size.into();
        self
    }
}

impl RenderOnce for TabBar {
    fn render(self, _window: &mut Window, cx: &mut App) -> impl IntoElement {
        self.base
            .group("tab-bar")
            .relative()
            .refine_style(&self.style)
            .bg(cx.theme().surface_background)
            .child(
                div()
                    .id("border-bottom")
                    .absolute()
                    .left_0()
                    .bottom_0()
                    .size_full()
                    .border_b_1()
                    .border_color(cx.theme().border),
            )
            .text_color(cx.theme().text)
            .when_some(self.prefix, |this, prefix| this.child(prefix))
            .child(
                h_flex()
                    .id("tabs")
                    .flex_grow()
                    .overflow_x_scroll()
                    .when_some(self.scroll_handle, |this, scroll_handle| {
                        this.track_scroll(&scroll_handle)
                    })
                    .children(self.children)
                    .when(self.suffix.is_some(), |this| {
                        this.child(self.last_empty_space)
                    }),
            )
            .when_some(self.suffix, |this, suffix| this.child(suffix))
    }
}
