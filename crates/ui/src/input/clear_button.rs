use gpui::{App, Styled};
use theme::ActiveTheme;

use crate::button::{Button, ButtonVariants as _};
use crate::{Icon, IconName, Sizable as _};

#[inline]
pub(crate) fn clear_button(cx: &App) -> Button {
    Button::new("clean")
        .icon(Icon::new(IconName::CloseCircle))
        .ghost()
        .xsmall()
        .tab_stop(false)
        .text_color(cx.theme().icon_muted)
}
