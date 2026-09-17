pub use gpui_base::{ElementExt, IndexPath, InteractiveElementExt};
pub use icon::*;
pub use kbd::*;
pub use root::{Root, window_paddings};
pub use styled::*;
pub use title_bar::*;
pub use window_ext::*;

pub use crate::Disableable;

pub mod animation;
pub mod avatar;
pub mod button;
pub mod divider;
pub mod dock;
pub mod group_box;
pub mod indicator;
pub mod input;
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

mod icon;
mod kbd;
mod root;
mod styled;
mod title_bar;
mod window_ext;

/// Initialize the UI module.
///
/// This must be called before using any of the UI components.
/// You can initialize the UI module at your application's entry point.
pub fn init(cx: &mut gpui::App) {
    gpui_base::init(cx);
    theme::sync_base(cx);
    menu::init(cx);
}
