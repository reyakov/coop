use std::time::{SystemTime, UNIX_EPOCH};

use gpui::prelude::FluentBuilder as _;
use gpui::{AnyElement, App, ImageSource, IntoElement, ParentElement, SharedString, Styled, px};
use settings::AppSettings;
use theme::ActiveTheme;
use ui::avatar::{Avatar, PixelAvatar};
use ui::{Icon, IconName, Sizable, h_flex};

pub(crate) fn nav_avatar(
    seed: Option<impl Into<SharedString>>,
    picture: Option<impl Into<ImageSource>>,
    cx: &App,
) -> Option<AnyElement> {
    if AppSettings::get_hide_avatar(cx) {
        return None;
    }

    match (seed.map(Into::into), picture.map(Into::into)) {
        (None, None) => None,
        (seed, Some(picture)) => Some(
            Avatar::from_source(picture)
                .when_some(seed, |avatar, seed| avatar.seed(seed))
                .small()
                .flex_shrink_0()
                .into_any_element(),
        ),
        (Some(seed), None) => Some(
            PixelAvatar::new(seed)
                .small()
                .flex_shrink_0()
                .into_any_element(),
        ),
    }
}

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
