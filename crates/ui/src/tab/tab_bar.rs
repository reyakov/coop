use std::rc::Rc;

use gpui::prelude::FluentBuilder as _;
use gpui::{
    AnyElement, App, Div, ElementId, InteractiveElement, IntoElement, ParentElement, RenderOnce,
    ScrollHandle, Stateful, StatefulInteractiveElement as _, StyleRefinement, Styled, Window, div,
    px,
};
use smallvec::SmallVec;
use theme::ActiveTheme;

use super::Tab;
use crate::{Selectable, StyledExt, h_flex};

/// A TabBar element that contains multiple [`Tab`] items.
#[derive(IntoElement)]
pub struct TabBar {
    base: Stateful<Div>,
    style: StyleRefinement,
    scroll_handle: Option<ScrollHandle>,
    prefix: Option<AnyElement>,
    suffix: Option<AnyElement>,
    children: SmallVec<[Tab; 2]>,
    last_empty_space: AnyElement,
    selected_index: Option<usize>,
    segmented: bool,
    #[allow(clippy::type_complexity)]
    on_click: Option<Rc<dyn Fn(&usize, &mut Window, &mut App) + 'static>>,
}

impl TabBar {
    /// Create a new TabBar.
    pub fn new(id: impl Into<ElementId>) -> Self {
        Self {
            base: div().id(id).px(px(-1.)),
            style: StyleRefinement::default(),
            children: SmallVec::new(),
            scroll_handle: None,
            prefix: None,
            suffix: None,
            last_empty_space: div().w_3().into_any_element(),
            selected_index: None,
            on_click: None,
            segmented: false,
        }
    }

    /// Render the tabs as a segmented control inside a pill-shaped track.
    pub fn segmented(mut self, segmented: bool) -> Self {
        self.segmented = segmented;
        self
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

    /// Add children of the TabBar.
    pub fn children(mut self, children: impl IntoIterator<Item = impl Into<Tab>>) -> Self {
        self.children.extend(children.into_iter().map(Into::into));
        self
    }

    /// Add child of the TabBar.
    pub fn child(mut self, child: impl Into<Tab>) -> Self {
        self.children.push(child.into());
        self
    }

    /// Set the selected index of the TabBar.
    pub fn selected_index(mut self, index: usize) -> Self {
        self.selected_index = Some(index);
        self
    }

    /// Set the last empty space element of the TabBar.
    pub fn last_empty_space(mut self, last_empty_space: impl IntoElement) -> Self {
        self.last_empty_space = last_empty_space.into_any_element();
        self
    }

    /// Set the on_click callback of the TabBar, the first parameter is the index of the clicked tab.
    ///
    /// When this is set, the children's on_click will be ignored.
    pub fn on_click<F>(mut self, on_click: F) -> Self
    where
        F: Fn(&usize, &mut Window, &mut App) + 'static,
    {
        self.on_click = Some(Rc::new(on_click));
        self
    }
}

impl Styled for TabBar {
    fn style(&mut self) -> &mut StyleRefinement {
        &mut self.style
    }
}

impl RenderOnce for TabBar {
    fn render(self, _: &mut Window, cx: &mut App) -> impl IntoElement {
        let segmented = self.segmented;
        let has_prefix = self.prefix.is_some();

        self.base
            .group("tab-bar")
            .relative()
            .flex()
            .items_center()
            .min_w_0()
            .refine_style(&self.style)
            .when(segmented, |this| {
                this.bg(cx.theme().tab_background)
                    .p_0p5()
                    .rounded(cx.theme().radius)
            })
            .when_some(self.prefix, |this, prefix| this.child(prefix))
            .child(
                h_flex()
                    .id("tabs")
                    .flex_1()
                    .when(!segmented, |this| {
                        this.overflow_x_scroll()
                            .when(!has_prefix, |this| this.pl_2())
                    })
                    .when_some(self.scroll_handle, |this, scroll_handle| {
                        this.track_scroll(&scroll_handle)
                    })
                    .gap_1()
                    .children(self.children.into_iter().enumerate().map(|(ix, child)| {
                        let tab_bar_prefix = child.tab_bar_prefix.unwrap_or(true);
                        child
                            .ix(ix)
                            .tab_bar_prefix(tab_bar_prefix)
                            .segmented(segmented)
                            .when_some(self.selected_index, |this, selected_ix| {
                                this.selected(selected_ix == ix)
                            })
                            .when_some(self.on_click.clone(), move |this, on_click| {
                                this.on_click(move |_, window, cx| on_click(&ix, window, cx))
                            })
                    }))
                    .when(self.suffix.is_some(), |this| {
                        this.child(self.last_empty_space)
                    }),
            )
            .when_some(self.suffix, |this, suffix| this.child(suffix))
    }
}
