use gpui::*;
use wasm_bindgen::prelude::*;

#[wasm_bindgen]
pub fn run() -> Result<(), JsValue> {
    console_error_panic_hook::set_once();

    // Initialize logging to browser console
    console_log::init_with_level(log::Level::Info).expect("Failed to initialize logger");

    // Also initialize tracing for WASM
    tracing_wasm::set_as_global_default();

    #[cfg(target_family = "wasm")]
    gpui_platform::web_init();

    #[cfg(not(target_family = "wasm"))]
    let app = gpui_platform::application();

    #[cfg(target_family = "wasm")]
    let app = gpui_platform::single_threaded_web();

    app.run(|_cx| {});

    Ok(())
}
