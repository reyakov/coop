use std::rc::Rc;

use gpui::prelude::FluentBuilder as _;
use gpui::{
    AnyElement, App, ClickEvent, ElementId, InteractiveElement, Interactivity, IntoElement,
    ParentElement, RenderOnce, SharedString, StatefulInteractiveElement as _, StyleRefinement,
    Styled, Window, div, px,
};
use theme::{ActiveTheme, TextStyle, Typeset as _};

use crate::{Selectable, StyledExt, h_flex};

/// A single navigation entry in a sidebar.
#[allow(clippy::type_complexity)]
#[derive(IntoElement)]
pub struct NavItem {
    id: ElementId,
    interactivity: Interactivity,
    style: StyleRefinement,
    icon: AnyElement,
    label: SharedString,
    suffix: Option<AnyElement>,
    selected: bool,
    on_click: Option<Rc<dyn Fn(&ClickEvent, &mut Window, &mut App)>>,
}

impl NavItem {
    pub fn new(
        id: impl Into<ElementId>,
        label: impl Into<SharedString>,
        icon: impl IntoElement,
    ) -> Self {
        let id = id.into();
        let mut interactivity = Interactivity::default();
        interactivity.element_id = Some(id.clone());

        Self {
            id,
            interactivity,
            style: StyleRefinement::default(),
            icon: icon.into_any_element(),
            label: label.into(),
            suffix: None,
            selected: false,
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

impl Selectable for NavItem {
    fn selected(mut self, selected: bool) -> Self {
        self.selected = selected;
        self
    }

    fn is_selected(&self) -> bool {
        self.selected
    }
}

impl InteractiveElement for NavItem {
    fn interactivity(&mut self) -> &mut Interactivity {
        &mut self.interactivity
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
        let text_color = if self.selected {
            cx.theme().text
        } else {
            cx.theme().text_muted
        };

        h_flex()
            .id(self.id)
            .refine_style(&self.style)
            .px_2()
            .py_1()
            .w_full()
            .gap(px(theme::SPACE))
            .rounded(px(theme::button_radius()))
            .text_color(text_color)
            .child(self.icon)
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .text_style(TextStyle::Callout)
                    .child(self.label),
            )
            .when_some(self.suffix, |this, suffix| {
                this.child(div().flex_shrink_0().child(suffix))
            })
            .when(clickable, |this| {
                this.cursor_pointer()
                    .hover(|this| this.bg(cx.theme().element_hover))
                    .when(self.selected, |this| this.bg(cx.theme().element_active))
            })
            .when_some(self.on_click, |this, handler| {
                this.on_click(move |event, window, cx| handler(event, window, cx))
            })
    }
}
