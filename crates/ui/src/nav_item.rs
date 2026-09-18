use std::rc::Rc;

use gpui::prelude::FluentBuilder as _;
use gpui::{
    AnyElement, App, ClickEvent, ElementId, InteractiveElement, IntoElement, ParentElement,
    RenderOnce, SharedString, StatefulInteractiveElement as _, StyleRefinement, Styled, Window,
    div,
};
use theme::ActiveTheme;

use crate::{StyledExt, h_flex};

/// A single navigation entry in a sidebar.
///
/// It has an arbitrary leading element, such as an icon or avatar, and a text
/// label. It can carry an optional trailing suffix, such as a status icon, and
/// an optional click handler. Rows with a click handler are highlighted on
/// hover and show a pointer cursor.
#[allow(clippy::type_complexity)]
#[derive(IntoElement)]
pub struct NavItem {
    id: ElementId,
    style: StyleRefinement,
    icon: AnyElement,
    label: SharedString,
    /// Trailing element at the right edge of the row, after the ellipsized label.
    suffix: Option<AnyElement>,
    on_click: Option<Rc<dyn Fn(&ClickEvent, &mut Window, &mut App)>>,
}

impl NavItem {
    pub fn new(
        id: impl Into<ElementId>,
        label: impl Into<SharedString>,
        icon: impl IntoElement,
    ) -> Self {
        Self {
            id: id.into(),
            style: StyleRefinement::default(),
            icon: icon.into_any_element(),
            label: label.into(),
            suffix: None,
            on_click: None,
        }
    }

    pub fn suffix(mut self, suffix: impl IntoElement) -> Self {
        self.suffix = Some(suffix.into_any_element());
        self
    }

    pub fn on_click(
        mut self,
        handler: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
    ) -> Self {
        self.on_click = Some(Rc::new(handler));
        self
    }
}

impl Styled for NavItem {
    fn style(&mut self) -> &mut StyleRefinement {
        &mut self.style
    }
}

impl RenderOnce for NavItem {
    fn render(self, _window: &mut Window, cx: &mut App) -> impl IntoElement {
        let clickable = self.on_click.is_some();

        h_flex()
            .id(self.id)
            .refine_style(&self.style)
            .px_2()
            .py_1()
            .w_full()
            .gap_2()
            .rounded(cx.theme().radius)
            .text_color(cx.theme().text)
            .child(self.icon)
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .text_sm()
                    .child(self.label),
            )
            .when_some(self.suffix, |this, suffix| {
                this.child(div().flex_shrink_0().child(suffix))
            })
            .when(clickable, |this| {
                this.cursor_pointer()
                    .hover(|this| this.bg(cx.theme().ghost_element_hover))
            })
            .when_some(self.on_click, |this, handler| {
                this.on_click(move |event, window, cx| handler(event, window, cx))
            })
    }
}
