use gpui::{AnyElement, App, IntoElement, ParentElement, SharedString, Styled, Window, img};
use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::v_flex;
use instant::Instant;

use crate::dialogs::import;

/// Brand backgrounds shown behind the signed-out screen; one is picked at random.
const SIGNED_OUT_BANNERS: [&str; 2] = ["brand/bg1.jpg", "brand/bg2.jpg"];

/// Pick one of the signed-out backgrounds at random.
pub(super) fn pick_banner() -> SharedString {
    let nanos = Instant::now().elapsed().subsec_nanos();
    let index = nanos as usize % SIGNED_OUT_BANNERS.len();
    SIGNED_OUT_BANNERS[index].into()
}

/// The headline and identity import shown while signed out.
pub(super) fn render_signed_out(_window: &mut Window, _cx: &mut App) -> AnyElement {
    v_flex()
        .size_full()
        .justify_end()
        .gap_4()
        .p_4()
        .child(img("brand/headline.png").max_w_48())
        .child(
            Button::new("import")
                .label("Import Identity")
                .primary()
                .on_click(|_, window, cx| {
                    import::open(window, cx);
                }),
        )
        .into_any_element()
}
