use std::rc::Rc;

use gpui::prelude::FluentBuilder as _;
use gpui::{App, InteractiveElement as _, IntoElement, Pixels, Styled as _, Window, div, px};
pub(crate) use gpui_base::resize_handle;
pub use gpui_base::{
    ResizablePanel, ResizablePanelEvent, ResizablePanelGroup, ResizableState, h_resizable,
    resizable_panel, v_resizable,
};
use gpui_base::{ResizeHandleContext, ResizeHandleRenderer};
use theme::{ActiveTheme as _, AxisExt as _};

const HANDLE_SIZE: Pixels = px(1.);

pub(crate) fn resize_handle_appearance() -> ResizeHandleRenderer {
    Rc::new(
        |context: &ResizeHandleContext, _: &mut Window, cx: &mut App| {
            let color = if context.is_active() {
                cx.theme().border_selected
            } else {
                cx.theme().border
            };
            let axis = context.axis();

            Some(
                div()
                    .group_hover("handle", move |this| this.bg(color))
                    .when(axis.is_horizontal(), |this| this.h_full().w(HANDLE_SIZE))
                    .when(axis.is_vertical(), |this| this.w_full().h(HANDLE_SIZE))
                    .into_any_element(),
            )
        },
    )
}
