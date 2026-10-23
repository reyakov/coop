use std::rc::Rc;

use gpui::prelude::FluentBuilder as _;
use gpui::{
    AnyElement, App, ClickEvent, ElementId, InteractiveElement, Interactivity, IntoElement,
    ParentElement, RenderOnce, SharedString, StatefulInteractiveElement as _, StyleRefinement,
    Styled, Window, div,
};
use gpui_kit::component::{ActiveTheme, Selectable, StyledExt as _, h_flex};

/// A single navigation row, such as an entry in a sidebar list.
#[derive(IntoElement)]
pub struct Nav {
    id: ElementId,
    interactivity: Interactivity,
    style: StyleRefinement,
    prefix: Option<AnyElement>,
    label: SharedString,
    suffix: Option<AnyElement>,
    selected: bool,
    clickable: bool,
    center_prefix: bool,
    #[allow(clippy::type_complexity)]
    on_click: Option<Rc<dyn Fn(&ClickEvent, &mut Window, &mut App)>>,
}

impl Nav {
    pub fn new(id: impl Into<ElementId>) -> Self {
        let id = id.into();
        let mut interactivity = Interactivity::default();
        interactivity.element_id = Some(id.clone());

        Self {
            id,
            interactivity,
            style: StyleRefinement::default(),
            prefix: None,
            label: SharedString::default(),
            suffix: None,
            selected: false,
            clickable: false,
            center_prefix: false,
            on_click: None,
        }
    }

    /// Marks the row as clickable even without an `on_click` handler, keeping the hover affordance.
    pub fn clickable(mut self, clickable: bool) -> Self {
        self.clickable = clickable;
        self
    }

    /// Whether to center the prefix in the avatar-width column.
    pub fn center_prefix(mut self, center: bool) -> Self {
        self.center_prefix = center;
        self
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

impl InteractiveElement for Nav {
    fn interactivity(&mut self) -> &mut Interactivity {
        &mut self.interactivity
    }
}

impl Styled for Nav {
    fn style(&mut self) -> &mut StyleRefinement {
        &mut self.style
    }
}

impl RenderOnce for Nav {
    fn render(self, _window: &mut Window, cx: &mut App) -> impl IntoElement {
        let clickable = self.on_click.is_some() || self.clickable;

        h_flex()
            .id(self.id)
            .h_8()
            .w_full()
            .px_2()
            .gap_2()
            .rounded(cx.theme().radius)
            .when_some(self.prefix, |this, prefix| {
                this.child(
                    h_flex()
                        .flex_shrink_0()
                        .when(self.center_prefix, |this| this.w_6().justify_center())
                        .child(prefix),
                )
            })
            .child(div().flex_1().min_w_0().truncate().child(self.label))
            .when_some(self.suffix, |this, suffix| {
                this.child(div().flex_shrink_0().child(suffix))
            })
            .when(clickable, |this| {
                this.cursor_pointer()
                    .hover(|this| this.bg(cx.theme().list_hover))
                    .when(self.selected, |this| this.bg(cx.theme().list_active))
            })
            .when_some(self.on_click, |this, handler| {
                this.on_click(move |event, window, cx| handler(event, window, cx))
            })
            .refine_style(&self.style)
    }
}
