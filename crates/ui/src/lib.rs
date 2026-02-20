pub use anchored::*;
pub use element_ext::ElementExt;
pub use event::InteractiveElementExt;
pub use focusable::FocusableCycle;
pub use geometry::*;
pub use icon::*;
pub use index_path::IndexPath;
pub use kbd::*;
pub use root::{window_paddings, Root};
pub use styled::*;
pub use window_ext::*;

pub use crate::Disableable;

pub mod actions;
pub mod animation;
pub mod avatar;
pub mod button;
pub mod checkbox;
pub mod divider;
pub mod dock_area;
pub mod history;
pub mod indicator;
pub mod input;
pub mod list;
pub mod menu;
pub mod modal;
pub mod notification;
pub mod popover;
pub mod resizable;
pub mod scroll;
pub mod skeleton;
pub mod switch;
pub mod tab;
pub mod tooltip;

mod anchored;
mod element_ext;
mod event;
mod focusable;
mod geometry;
mod icon;
mod index_path;
mod kbd;
mod root;
mod styled;
mod window_ext;

/// Initialize the UI module.
///
/// This must be called before using any of the UI components.
/// You can initialize the UI module at your application's entry point.
pub fn init(cx: &mut gpui::App) {
    input::init(cx);
    list::init(cx);
    modal::init(cx);
    popover::init(cx);
    menu::init(cx);
}
