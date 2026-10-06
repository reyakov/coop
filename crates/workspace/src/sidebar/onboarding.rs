use std::time::{SystemTime, UNIX_EPOCH};

use gpui::{AnyElement, App, IntoElement, ParentElement, SharedString, Styled, Window, img};
use ui::button::{Button, ButtonCustomVariant, ButtonVariants};
use ui::{Sizable, StyledExt, v_flex};

use crate::dialogs::import;

/// Brand backgrounds shown behind the signed-out screen; one is picked at random.
const SIGNED_OUT_BANNERS: [&str; 2] = ["brand/bg1.jpg", "brand/bg2.jpg"];

/// Pick one of the signed-out backgrounds at random.
pub(super) fn pick_banner() -> SharedString {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| elapsed.subsec_nanos())
        .unwrap_or_default();
    let index = nanos as usize % SIGNED_OUT_BANNERS.len();
    SIGNED_OUT_BANNERS[index].into()
}

/// The headline and identity import shown while signed out.
pub(super) fn render_signed_out(window: &mut Window, cx: &mut App) -> AnyElement {
    v_flex()
        .size_full()
        .justify_end()
        .gap_4()
        .p_4()
        .child(img("brand/headline.png").max_w_48())
        .child(
            Button::new("import")
                .label("Import Identity")
                .custom(
                    ButtonCustomVariant::new(window, cx)
                        .color(gpui::white())
                        .foreground(gpui::black())
                        .hover(gpui::white().opacity(0.9))
                        .active(gpui::white().opacity(0.8)),
                )
                .large()
                .font_semibold()
                .on_click(|_, window, cx| {
                    import::open(window, cx);
                }),
        )
        .into_any_element()
}
