use std::rc::Rc;

use gpui::{
    Anchor, AnyElement, Context, DismissEvent, ElementId, Entity, Focusable, InteractiveElement,
    IntoElement, MouseButton, RenderOnce, SharedString, StyleRefinement, Styled, Window,
};

use crate::Selectable;
use crate::avatar::Avatar;
use crate::button::Button;
use crate::menu::PopupMenu;
use crate::popover::{Popover, PopoverState};

/// Builds the items of a popup menu on each render.
type MenuBuilder = dyn Fn(PopupMenu, &mut Window, &mut Context<PopupMenu>) -> PopupMenu;

/// A dropdown menu trait for buttons and other interactive elements
pub trait DropdownMenu: Styled + Selectable + InteractiveElement + IntoElement + 'static {
    /// Create a dropdown menu with the given items, anchored to the TopLeft corner
    fn dropdown_menu(
        self,
        f: impl Fn(PopupMenu, &mut Window, &mut Context<PopupMenu>) -> PopupMenu + 'static,
    ) -> DropdownMenuPopover<Self> {
        self.dropdown_menu_with_anchor(Anchor::TopLeft, f)
    }

    /// Create a dropdown menu with the given items, anchored to the given corner
    fn dropdown_menu_with_anchor(
        mut self,
        anchor: impl Into<Anchor>,
        f: impl Fn(PopupMenu, &mut Window, &mut Context<PopupMenu>) -> PopupMenu + 'static,
    ) -> DropdownMenuPopover<Self> {
        let style = self.style().clone();
        let id = self.interactivity().element_id.clone();

        DropdownMenuPopover::new(id.unwrap_or(0.into()), anchor, self, f).trigger_style(style)
    }
}

impl DropdownMenu for Button {}

impl DropdownMenu for Avatar {}

#[derive(IntoElement)]
pub struct DropdownMenuPopover<T: Selectable + IntoElement + 'static> {
    id: ElementId,
    style: StyleRefinement,
    anchor: Anchor,
    trigger: T,
    builder: Rc<MenuBuilder>,
}

impl<T> DropdownMenuPopover<T>
where
    T: Selectable + IntoElement + 'static,
{
    fn new(
        id: ElementId,
        anchor: impl Into<Anchor>,
        trigger: T,
        builder: impl Fn(PopupMenu, &mut Window, &mut Context<PopupMenu>) -> PopupMenu + 'static,
    ) -> Self {
        Self {
            id: SharedString::from(format!("dropdown-menu:{:?}", id)).into(),
            style: StyleRefinement::default(),
            anchor: anchor.into(),
            trigger,
            builder: Rc::new(builder),
        }
    }

    /// Set the anchor corner for the dropdown menu popover.
    pub fn anchor(mut self, anchor: impl Into<Anchor>) -> Self {
        self.anchor = anchor.into();
        self
    }

    /// Set the style refinement for the dropdown menu trigger.
    fn trigger_style(mut self, style: StyleRefinement) -> Self {
        self.style = style;
        self
    }
}

/// Opens a [`PopupMenu`] when its child is clicked with a mouse button
/// (right by default), keeping the child's own click handler intact.
#[derive(IntoElement)]
pub struct ContextMenu {
    id: ElementId,
    anchor: Anchor,
    mouse_button: MouseButton,
    child: AnyElement,
    builder: Rc<MenuBuilder>,
}

impl ContextMenu {
    pub fn new(
        id: impl Into<ElementId>,
        child: impl IntoElement,
        builder: impl Fn(PopupMenu, &mut Window, &mut Context<PopupMenu>) -> PopupMenu + 'static,
    ) -> Self {
        Self {
            id: id.into(),
            anchor: Anchor::TopLeft,
            mouse_button: MouseButton::Right,
            child: child.into_any_element(),
            builder: Rc::new(builder),
        }
    }

    /// Set the anchor corner of the menu, default is `Anchor::TopLeft`.
    pub fn anchor(mut self, anchor: impl Into<Anchor>) -> Self {
        self.anchor = anchor.into();
        self
    }

    /// Set the mouse button that opens the menu, default is `MouseButton::Right`.
    pub fn mouse_button(mut self, mouse_button: MouseButton) -> Self {
        self.mouse_button = mouse_button;
        self
    }
}

#[derive(Default)]
struct MenuState {
    menu: Option<Entity<PopupMenu>>,
}

/// Builds the menu once and reuses it until it is dismissed.
///
/// The popover content closure runs on every render, so rebuilding the menu
/// entity each time would drop its focus and selection state.
fn cached_menu(
    menu_state: &Entity<MenuState>,
    builder: Rc<MenuBuilder>,
    window: &mut Window,
    cx: &mut Context<PopoverState>,
) -> Entity<PopupMenu> {
    if let Some(menu) = menu_state.read(cx).menu.clone() {
        return menu;
    }

    let menu = PopupMenu::build(window, cx, move |menu, window, cx| {
        builder(menu, window, cx)
    });
    menu_state.update(cx, |state, _| {
        state.menu = Some(menu.clone());
    });
    menu.focus_handle(cx).focus(window, cx);

    let popover_state = cx.entity();
    window
        .subscribe(&menu, cx, {
            let menu_state = menu_state.clone();
            move |_, _: &DismissEvent, window, cx| {
                popover_state.update(cx, |state, cx| state.dismiss(window, cx));
                menu_state.update(cx, |state, _| {
                    state.menu = None;
                });
            }
        })
        .detach();

    menu
}

impl<T> RenderOnce for DropdownMenuPopover<T>
where
    T: Selectable + IntoElement + 'static,
{
    fn render(self, window: &mut Window, cx: &mut gpui::App) -> impl IntoElement {
        let builder = self.builder.clone();
        let menu_state = window.use_keyed_state(self.id.clone(), cx, |_, _| MenuState::default());

        Popover::new(SharedString::from(format!("popover:{}", self.id)))
            .appearance(false)
            .overlay_closable(false)
            .trigger(self.trigger)
            .trigger_style(self.style)
            .anchor(self.anchor)
            .content(move |_, window, cx| cached_menu(&menu_state, builder.clone(), window, cx))
    }
}

impl RenderOnce for ContextMenu {
    fn render(self, window: &mut Window, cx: &mut gpui::App) -> impl IntoElement {
        let builder = self.builder.clone();
        let menu_state = window.use_keyed_state(self.id.clone(), cx, |_, _| MenuState::default());

        Popover::new(SharedString::from(format!("context-menu:{}", self.id)))
            .appearance(false)
            .overlay_closable(false)
            .anchor(self.anchor)
            .mouse_button(self.mouse_button)
            .trigger_with(move |_open, _window, _cx| self.child)
            .content(move |_, window, cx| cached_menu(&menu_state, builder.clone(), window, cx))
    }
}
