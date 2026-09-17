use std::panic::Location;

use gpui::prelude::FluentBuilder;
use gpui::{
    App, Div, Element, ElementId, InteractiveElement, IntoElement, ParentElement, RenderOnce,
    ScrollHandle, Stateful, StatefulInteractiveElement, StyleRefinement, Styled, Window, div, px,
};
use gpui_base::{Scrollbar, ScrollbarAxis, ScrollbarHandle};
use theme::ActiveTheme as _;

use crate::StyledExt as _;

/// A trait for elements that can be made scrollable with scrollbars.
pub trait ScrollableElement: InteractiveElement + Styled + ParentElement + Element {
    /// Adds a scrollbar to the element.
    #[track_caller]
    fn scrollbar<H: ScrollbarHandle + Clone>(
        self,
        scroll_handle: &H,
        axis: impl Into<ScrollbarAxis>,
    ) -> Self {
        self.child(ScrollbarLayer {
            id: "scrollbar_layer".into(),
            axis: axis.into(),
            scroll_handle: scroll_handle.clone(),
        })
    }

    /// Adds a vertical scrollbar to the element.
    #[track_caller]
    fn vertical_scrollbar<H: ScrollbarHandle + Clone>(self, scroll_handle: &H) -> Self {
        self.scrollbar(scroll_handle, ScrollbarAxis::Vertical)
    }
    /// Adds a horizontal scrollbar to the element.
    #[track_caller]
    fn horizontal_scrollbar<H: ScrollbarHandle + Clone>(self, scroll_handle: &H) -> Self {
        self.scrollbar(scroll_handle, ScrollbarAxis::Horizontal)
    }

    /// Almost equivalent to [`StatefulInteractiveElement::overflow_scroll`], but adds scrollbars.
    #[track_caller]
    fn overflow_scrollbar(self) -> Scrollable<Self> {
        Scrollable::new(self, ScrollbarAxis::Both)
    }

    /// Almost equivalent to [`StatefulInteractiveElement::overflow_x_scroll`], but adds Horizontal scrollbar.
    #[track_caller]
    fn overflow_x_scrollbar(self) -> Scrollable<Self> {
        Scrollable::new(self, ScrollbarAxis::Horizontal)
    }

    /// Almost equivalent to [`StatefulInteractiveElement::overflow_y_scroll`], but adds Vertical scrollbar.
    #[track_caller]
    fn overflow_y_scrollbar(self) -> Scrollable<Self> {
        Scrollable::new(self, ScrollbarAxis::Vertical)
    }
}

/// The scrollbar the application's theme describes: a 10px rail holding a 6px
/// rounded thumb that grows to 8px under the pointer.
fn scrollbar<H: ScrollbarHandle + Clone>(
    scroll_handle: &H,
    axis: ScrollbarAxis,
    cx: &App,
) -> Scrollbar {
    let theme = cx.theme();
    let (thumb_width, thumb_radius) = if theme.scrollbar_mode.is_scrolling() {
        (px(6.), px(3.))
    } else {
        (px(8.), px(4.))
    };

    Scrollbar::new(scroll_handle).axis(axis).styles(|styles| {
        styles
            .track(|track| {
                track
                    .bg(theme.scrollbar_track_background)
                    .border_color(theme.scrollbar_thumb_border)
                    .width(px(10.))
            })
            .track_hover(|track| track.bg(theme.scrollbar_thumb_background).width(px(10.)))
            .thumb(|thumb| {
                thumb
                    .bg(theme.scrollbar_thumb_background)
                    .width(thumb_width)
                    .inset(px(1.))
                    .radius(thumb_radius)
                    .min_length(px(48.))
            })
            .thumb_hover(|thumb| {
                thumb
                    .bg(theme.scrollbar_thumb_hover_background)
                    .width(px(8.))
                    .inset(px(1.))
                    .radius(px(4.))
            })
            .thumb_active(|thumb| {
                thumb
                    .bg(theme.scrollbar_thumb_hover_background)
                    .width(px(8.))
                    .inset(px(1.))
                    .radius(px(4.))
            })
    })
}

/// A scrollbar child that resolves the theme while rendering, which the
/// [`ScrollableElement`] helpers cannot do at the call site.
#[derive(IntoElement)]
struct ScrollbarLayer<H: ScrollbarHandle + Clone> {
    id: ElementId,
    axis: ScrollbarAxis,
    scroll_handle: H,
}

impl<H> RenderOnce for ScrollbarLayer<H>
where
    H: ScrollbarHandle + Clone + 'static,
{
    fn render(self, _: &mut Window, cx: &mut App) -> impl IntoElement {
        scrollbar(&self.scroll_handle, self.axis, cx).id(self.id)
    }
}

/// A scrollable element wrapper that adds scrollbars to an interactive element.
#[derive(IntoElement)]
pub struct Scrollable<E: InteractiveElement + Styled + ParentElement + Element> {
    id: ElementId,
    element: E,
    axis: ScrollbarAxis,
}

impl<E> Scrollable<E>
where
    E: InteractiveElement + Styled + ParentElement + Element,
{
    #[track_caller]
    fn new(element: E, axis: impl Into<ScrollbarAxis>) -> Self {
        let caller = Location::caller();
        Self {
            id: ElementId::CodeLocation(*caller),
            element,
            axis: axis.into(),
        }
    }
}

impl<E> Styled for Scrollable<E>
where
    E: InteractiveElement + Styled + ParentElement + Element,
{
    fn style(&mut self) -> &mut StyleRefinement {
        self.element.style()
    }
}

impl<E> ParentElement for Scrollable<E>
where
    E: InteractiveElement + Styled + ParentElement + Element,
{
    fn extend(&mut self, elements: impl IntoIterator<Item = gpui::AnyElement>) {
        self.element.extend(elements)
    }
}

impl InteractiveElement for Scrollable<Div> {
    fn interactivity(&mut self) -> &mut gpui::Interactivity {
        self.element.interactivity()
    }
}

impl InteractiveElement for Scrollable<Stateful<Div>> {
    fn interactivity(&mut self) -> &mut gpui::Interactivity {
        self.element.interactivity()
    }
}

impl<E> RenderOnce for Scrollable<E>
where
    E: InteractiveElement + Styled + ParentElement + Element + 'static,
{
    fn render(mut self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let scroll_handle = window
            .use_keyed_state(self.id.clone(), cx, |_, _| ScrollHandle::default())
            .read(cx)
            .clone();

        // Inherit the size from the element style.
        let style = StyleRefinement {
            size: self.element.style().size.clone(),
            ..Default::default()
        };

        div()
            .id(self.id)
            .size_full()
            .refine_style(&style)
            .relative()
            .child(
                div()
                    .id("scroll-area")
                    .flex()
                    .size_full()
                    .track_scroll(&scroll_handle)
                    .map(|this| match self.axis {
                        ScrollbarAxis::Vertical => this.flex_col().overflow_y_scroll(),
                        ScrollbarAxis::Horizontal => this.flex_row().overflow_x_scroll(),
                        ScrollbarAxis::Both => this.overflow_scroll(),
                    })
                    .child(
                        self.element
                            // Refine element size to `flex_1`.
                            .size_auto()
                            .flex_1(),
                    ),
            )
            .child(scrollbar(&scroll_handle, self.axis, cx).id("scrollbar"))
    }
}

impl ScrollableElement for Div {}

impl<E> ScrollableElement for Stateful<E>
where
    E: ParentElement + Styled + Element,
    Self: InteractiveElement,
{
}
