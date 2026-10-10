use std::rc::Rc;

use gpui::prelude::FluentBuilder as _;
use gpui::{
    Anchor, AnyElement, App, Context, Div, ElementId, InteractiveElement as _, IntoElement,
    MouseButton, ParentElement, RenderOnce, Stateful, StyleRefinement, Styled, Window,
};
use gpui_base::Popover as BasePopover;
pub(crate) use gpui_base::PopoverState;

use crate::{Selectable, StyledExt as _, v_flex};

/// A popover element that can be triggered by a button or any other element.
#[derive(IntoElement)]
pub(crate) struct Popover {
    id: ElementId,
    style: StyleRefinement,
    anchor: Anchor,
    #[allow(clippy::type_complexity)]
    trigger: Option<Box<dyn FnOnce(bool, &Window, &App) -> AnyElement + 'static>>,
    #[allow(clippy::type_complexity)]
    content: Option<
        Rc<
            dyn Fn(&mut PopoverState, &mut Window, &mut Context<PopoverState>) -> AnyElement
                + 'static,
        >,
    >,
    children: Vec<AnyElement>,
    /// Style refinement applied to the trigger element.
    trigger_style: Option<StyleRefinement>,
    mouse_button: MouseButton,
    appearance: bool,
    overlay_closable: bool,
}

impl Popover {
    /// Creates a new Popover with the given id.
    pub(crate) fn new(id: impl Into<ElementId>) -> Self {
        Self {
            id: id.into(),
            style: StyleRefinement::default(),
            anchor: Anchor::TopLeft,
            trigger: None,
            trigger_style: None,
            content: None,
            children: vec![],
            mouse_button: MouseButton::Left,
            appearance: true,
            overlay_closable: true,
        }
    }

    /// Sets the anchor corner of the popover, default is `Anchor::TopLeft`.
    pub(crate) fn anchor(mut self, anchor: impl Into<Anchor>) -> Self {
        self.anchor = anchor.into();
        self
    }

    /// Sets the mouse button that triggers the popover, default is `MouseButton::Left`.
    pub(crate) fn mouse_button(mut self, mouse_button: MouseButton) -> Self {
        self.mouse_button = mouse_button;
        self
    }

    /// Sets the trigger element, marked as selected while the popover is open.
    pub(crate) fn trigger<T>(mut self, trigger: T) -> Self
    where
        T: Selectable + IntoElement + 'static,
    {
        self.trigger = Some(Box::new(|is_open, _, _| {
            let selected = trigger.is_selected();
            trigger.selected(selected || is_open).into_any_element()
        }));
        self
    }

    /// Sets the trigger from a builder, for elements that have no selected state.
    pub(crate) fn trigger_with<F>(mut self, trigger: F) -> Self
    where
        F: FnOnce(bool, &Window, &App) -> AnyElement + 'static,
    {
        self.trigger = Some(Box::new(trigger));
        self
    }

    /// Sets the style for the trigger element.
    pub(crate) fn trigger_style(mut self, style: StyleRefinement) -> Self {
        self.trigger_style = Some(style);
        self
    }

    /// Sets whether clicking outside the popover dismisses it, default is `true`.
    pub(crate) fn overlay_closable(mut self, closable: bool) -> Self {
        self.overlay_closable = closable;
        self
    }

    /// Sets the content builder, called on every render of the popover.
    pub(crate) fn content<F, E>(mut self, content: F) -> Self
    where
        E: IntoElement,
        F: Fn(&mut PopoverState, &mut Window, &mut Context<PopoverState>) -> E + 'static,
    {
        self.content = Some(Rc::new(move |state, window, cx| {
            content(state, window, cx).into_any_element()
        }));
        self
    }

    /// Sets whether the popover renders with default styling, default is `true`.
    pub(crate) fn appearance(mut self, appearance: bool) -> Self {
        self.appearance = appearance;
        self
    }

    fn render_popover_content(
        anchor: Anchor,
        appearance: bool,
        _: &mut Window,
        cx: &mut App,
    ) -> Stateful<Div> {
        v_flex()
            .id("content")
            .occlude()
            .tab_group()
            .when(appearance, |this| this.popover_style(cx).p_3())
            .map(|this| match anchor {
                Anchor::TopLeft | Anchor::TopCenter | Anchor::TopRight => this.top_1(),
                Anchor::BottomLeft | Anchor::BottomCenter | Anchor::BottomRight => this.bottom_1(),
                Anchor::LeftCenter | Anchor::RightCenter => this.top_1(), // Fallback for centered
            })
    }
}

impl ParentElement for Popover {
    fn extend(&mut self, elements: impl IntoIterator<Item = AnyElement>) {
        self.children.extend(elements);
    }
}

impl Styled for Popover {
    fn style(&mut self) -> &mut StyleRefinement {
        &mut self.style
    }
}

impl RenderOnce for Popover {
    fn render(self, _: &mut Window, _: &mut App) -> impl IntoElement {
        let anchor = self.anchor;
        let appearance = self.appearance;
        let style = self.style;
        let children = self.children;
        let content = self.content;

        BasePopover::new(self.id)
            .anchor(anchor)
            .mouse_button(self.mouse_button)
            .overlay_closable(self.overlay_closable)
            .content(move |state, window, cx| {
                Self::render_popover_content(anchor, appearance, window, cx)
                    .when_some(content, |this, content| {
                        this.child((content)(state, window, cx))
                    })
                    .children(children)
                    .refine_style(&style)
            })
            .when_some(self.trigger, |this, trigger| this.trigger_with(trigger))
            .into_any_element()
    }
}
