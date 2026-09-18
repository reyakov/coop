use gpui::{App, Window};

mod community;
mod sync;

pub use community::*;
pub use sync::*;

pub fn init(_window: &mut Window, _cx: &mut App) {}
