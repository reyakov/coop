use std::time::{SystemTime, UNIX_EPOCH};

use gpui::SharedString;

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
