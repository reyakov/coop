use std::rc::Rc;

use gpui::prelude::FluentBuilder as _;
use gpui::{
    AnyElement, App, ClickEvent, ElementId, InteractiveElement, IntoElement, ParentElement,
    RenderOnce, SharedString, StatefulInteractiveElement as _, StyleRefinement, Styled, Window,
    div,
};
use theme::ActiveTheme;

use crate::{Selectable, StyledExt, h_flex, v_flex};

/// A single navigation row, such as an entry in a sidebar list.
#[derive(IntoElement)]
pub struct Nav {
    id: ElementId,
    style: StyleRefinement,
    prefix: Option<AnyElement>,
    label: SharedString,
    suffix: Option<AnyElement>,
    selected: bool,
    #[allow(clippy::type_complexity)]
    on_click: Option<Rc<dyn Fn(&ClickEvent, &mut Window, &mut App)>>,
}

impl Nav {
    pub fn new(id: impl Into<ElementId>) -> Self {
        Self {
            id: id.into(),
            style: StyleRefinement::default(),
            prefix: None,
            label: SharedString::default(),
            suffix: None,
            selected: false,
            on_click: None,
        }
    }

    /// Sets the element shown before the label, such as an avatar or icon.
    pub fn prefix(mut self, prefix: impl IntoElement) -> Self {
        self.prefix = Some(prefix.into_any_element());
        self
    }

    /// Sets the row's label.
    pub fn label(mut self, label: impl Into<SharedString>) -> Self {
        self.label = label.into();
        self
    }

    /// Sets the element shown after the label, such as a timestamp or badge.
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

impl Selectable for Nav {
    fn selected(mut self, selected: bool) -> Self {
        self.selected = selected;
        self
    }

    fn is_selected(&self) -> bool {
        self.selected
    }
}

impl Styled for Nav {
    fn style(&mut self) -> &mut StyleRefinement {
        &mut self.style
    }
}

impl RenderOnce for Nav {
    fn render(self, _window: &mut Window, cx: &mut App) -> impl IntoElement {
        let clickable = self.on_click.is_some();

        v_flex()
            .id(format!("nav-{}", self.id))
            .w_full()
            .h_10()
            .child(
                h_flex()
                    .id(self.id)
                    .h_9()
                    .w_full()
                    .px_1p5()
                    .gap_1p5()
                    .rounded(cx.theme().radius_lg)
                    .when_some(self.prefix, |this, prefix| this.child(prefix))
                    .child(
                        h_flex()
                            .gap_1()
                            .flex_1()
                            .child(div().truncate().min_w_0().child(self.label))
                            .child(div().flex_1())
                            .when_some(self.suffix, |this, suffix| {
                                this.child(div().flex_shrink_0().child(suffix))
                            }),
                    )
                    .when(clickable, |this| {
                        this.cursor_pointer()
                            .hover(|this| this.bg(cx.theme().ghost_element_hover))
                            .when(self.selected, |this| {
                                this.bg(cx.theme().ghost_element_active)
                            })
                    })
                    .when_some(self.on_click, |this, handler| {
                        this.on_click(move |event, window, cx| handler(event, window, cx))
                    })
                    .refine_style(&self.style),
            )
    }
}
