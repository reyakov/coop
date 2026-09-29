use std::time::{SystemTime, UNIX_EPOCH};

use gpui::{AnyElement, App, IntoElement, ParentElement, SharedString, Styled, px};
use theme::ActiveTheme;
use ui::{Icon, IconName, Sizable, h_flex};

pub(crate) fn nav_icon(icon: IconName, cx: &App) -> AnyElement {
    h_flex()
        .flex_shrink_0()
        .w(px(20.))
        .justify_center()
        .text_color(cx.theme().icon_muted)
        .child(Icon::new(icon).small())
        .into_any_element()
}

/// Brand backgrounds shown behind the signed-out screen; one is picked at random.
const SIGNED_OUT_BANNERS: [&str; 2] = ["brand/bg1.jpg", "brand/bg2.jpg"];

/// Pick one of the signed-out backgrounds at random.
pub(crate) fn pick_banner() -> SharedString {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| elapsed.subsec_nanos())
        .unwrap_or_default();
    let index = nanos as usize % SIGNED_OUT_BANNERS.len();
    SIGNED_OUT_BANNERS[index].into()
}
