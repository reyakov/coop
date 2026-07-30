pub(super) const MASK_CHAR: char = '*';

mod blink_cursor;
mod change;
mod clear_button;
mod cursor;
mod display_map;
mod element;
mod indent;
#[allow(clippy::module_inception)]
mod input;
mod mask_pattern;
mod mode;
mod movement;
mod rope_ext;
mod selection;
mod state;

pub(crate) use clear_button::*;
pub use cursor::*;
pub use display_map::DisplayMap;
pub use indent::TabSize;
pub use input::*;
pub use mask_pattern::MaskPattern;
pub use rope_ext::{InputEdit, Point, RopeExt, RopeLines};
pub use ropey::Rope;
pub use state::*;
