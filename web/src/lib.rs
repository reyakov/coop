use gpui::*;
use ui::Root;
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

    gpui_platform::application().run(|cx| {
        cx.open_window(WindowOptions::default(), |window, cx| {
            cx.new(|cx| {
                // Initialize components
                ui::init(cx);

                // Initialize theme registry
                theme::init(cx);

                // Initialize settings
                settings::init(window, cx);

                // Initialize the nostr client
                state::init(window, cx);

                // Initialize person registry
                person::init(window, cx);

                // Initialize device signer
                //
                // NIP-4e: https://github.com/nostr-protocol/nips/blob/per-device-keys/4e.md
                device::init(window, cx);

                // Initialize app registry
                chat::init(window, cx);

                // Root Entity
                Root::new(workspace::init(window, cx).into(), window, cx)
            })
        })
        .expect("Failed to open window. Please restart the application.");
        cx.activate(true);
    });

    Ok(())
}
