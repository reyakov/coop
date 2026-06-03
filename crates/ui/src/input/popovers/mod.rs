mod code_action_menu;
mod completion_menu;
mod context_menu;
mod diagnostic_popover;
mod hover_popover;

pub(crate) use code_action_menu::*;
pub(crate) use completion_menu::*;
pub(crate) use context_menu::*;
pub(crate) use diagnostic_popover::*;
use gpui::{
    App, Div, ElementId, Entity, InteractiveElement as _, IntoElement, SharedString, Stateful,
    StyleRefinement, Styled as _, Window, div, px, rems,
};
pub(crate) use hover_popover::*;

use crate::StyledExt as _;

pub(crate) enum ContextMenu {
    Completion(Entity<CompletionMenu>),
    CodeAction(Entity<CodeActionMenu>),
    RightClick(Entity<InputContextMenu>),
}

impl ContextMenu {
    pub(crate) fn is_open(&self, cx: &App) -> bool {
        match self {
            ContextMenu::Completion(menu) => menu.read(cx).is_open(),
            ContextMenu::CodeAction(menu) => menu.read(cx).is_open(),
            ContextMenu::RightClick(menu) => menu.read(cx).is_open(),
        }
    }

    pub(crate) fn render(&self) -> impl IntoElement {
        match self {
            ContextMenu::Completion(menu) => menu.clone().into_any_element(),
            ContextMenu::CodeAction(menu) => menu.clone().into_any_element(),
            ContextMenu::RightClick(menu) => menu.clone().into_any_element(),
        }
    }
}
