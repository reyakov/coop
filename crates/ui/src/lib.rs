pub(crate) use gpui_base::{ElementExt, InteractiveElementExt};
pub use icon::*;
pub use root::Root;
pub use styled::*;
pub use title_bar::{TRAFFIC_LIGHT_PADDING, title_bar_drag_handlers};
pub use window_ext::*;

pub mod avatar;
pub mod button;
pub mod dialog;
pub mod dock;
pub mod group_box;
pub mod indicator;
pub mod input;
pub mod markdown;
pub mod menu;
pub mod message;
pub mod nav;
pub mod nav_item;
pub mod notification;
pub mod scroll;
pub mod switch;
pub mod tab;
pub mod tooltip;

mod icon;
mod kbd;
pub mod motion;
mod popover;
mod resizable;
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
    #[cfg(not(target_os = "macos"))]
    cx.bind_keys([gpui::KeyBinding::new(
        "shift-insert",
        gpui_base::input::Paste,
        Some("Input"),
    )]);
    theme::sync_base(cx);
    menu::init(cx);
}
