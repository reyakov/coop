use std::borrow::Cow;
use std::cell::RefCell;

use gpui::*;
use theme::{Theme, ThemeMode};
use ui::Root;
use universal_time::{Instant, MonotonicClock, SystemTime, WallClock, define_time_provider};
use wasm_bindgen::prelude::*;

struct CustomTimeProvider;

impl WallClock for CustomTimeProvider {
    fn system_time(&self) -> SystemTime {
        // Browser wall clock: milliseconds since the Unix epoch.
        let millis = js_sys::Date::now();
        SystemTime::from_unix_duration(instant::Duration::from_millis(millis as u64))
    }
}

impl MonotonicClock for CustomTimeProvider {
    fn instant(&self) -> Instant {
        // `performance.now()` is monotonic; fall back to the wall clock if
        // it's unavailable.
        let millis = web_sys::window()
            .and_then(|window| window.performance())
            .map(|performance| performance.now())
            .unwrap_or_else(js_sys::Date::now);
        Instant::from_ticks(instant::Duration::from_millis(millis as u64))
    }
}

define_time_provider!(CustomTimeProvider);

/// Returns the preferred WebGPU/WebGL backend, selected with
/// `?backend=webgpu` / `?backend=webgl` in the page URL.
#[cfg(target_family = "wasm")]
fn requested_backend() -> gpui_platform::WebBackendPreference {
    let search = web_sys::window()
        .and_then(|window| window.location().search().ok())
        .unwrap_or_default();
    if search
        .trim_start_matches('?')
        .split('&')
        .any(|parameter| parameter == "backend=webgpu")
    {
        gpui_platform::WebBackendPreference::WebGpu
    } else if search
        .trim_start_matches('?')
        .split('&')
        .any(|parameter| parameter == "backend=webgl")
    {
        gpui_platform::WebBackendPreference::WebGl
    } else {
        gpui_platform::WebBackendPreference::Auto
    }
}

thread_local! {
    static APPLICATION: RefCell<Option<ApplicationHandle>> = const { RefCell::new(None) };
}

/// Applies a theme mode and restores the bundled web fonts.
///
/// `Theme::change` reapplies the theme config, which can carry its own font
/// family; host system fonts are unavailable in wasm, so the bundled Inter
/// fonts are put back afterwards.
fn apply_theme(mode: ThemeMode, cx: &mut App) {
    Theme::change(mode, None, cx);
    Theme::global_mut(cx).font_family = "Inter".into();
}

/// Switches the app between light and dark after it is running.
///
/// The embedding page calls this to keep the app in sync with its own
/// appearance.
#[cfg(target_family = "wasm")]
#[wasm_bindgen]
pub fn set_theme(dark: bool) {
    let mode = if dark {
        ThemeMode::Dark
    } else {
        ThemeMode::Light
    };
    APPLICATION.with(|application| {
        if let Some(handle) = application.borrow().as_ref() {
            handle.update(|cx| {
                apply_theme(mode, cx);
                cx.refresh_windows();
            });
        }
    });
}

#[wasm_bindgen]
pub async fn run() -> Result<(), JsValue> {
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
    let app = {
        // Assets are not embedded in the WASM bundle; they are served from
        // the `/assets/...` URL prefix (see `web/www/vite.config.js`) and
        // downloaded by the `assets` crate.
        let assets = assets::Assets::new("");

        // Download every icon and brand asset before the first frame: brand
        // images are loaded through GPUI's image cache, which does not retry
        // failed loads, and pre-caching the icons lets them render
        // immediately instead of waiting for a repaint.
        assets.preload().await;

        // Multithreaded web: background work (image decoding, network
        // fetches, CPU-heavy tasks) runs on real worker threads instead of
        // the main thread, so the UI stays responsive.
        gpui_platform::application_with_web_backend(requested_backend()).with_assets(assets)
    };

    let launch = move |cx: &mut App| {
        // Load the embedded Inter font stack for WASM, where host system
        // fonts are unavailable. Inter is the app's UI font on Linux; the
        // wasm build reuses it so the web app matches the desktop look.
        let inter_regular =
            Cow::Borrowed(include_bytes!("../../assets/fonts/Inter/Inter-Regular.ttf").as_slice());
        let inter_italic =
            Cow::Borrowed(include_bytes!("../../assets/fonts/Inter/Inter-Italic.ttf").as_slice());
        let inter_medium =
            Cow::Borrowed(include_bytes!("../../assets/fonts/Inter/Inter-Medium.ttf").as_slice());
        let inter_medium_italic = Cow::Borrowed(
            include_bytes!("../../assets/fonts/Inter/Inter-MediumItalic.ttf").as_slice(),
        );
        let inter_semibold =
            Cow::Borrowed(include_bytes!("../../assets/fonts/Inter/Inter-SemiBold.ttf").as_slice());
        let inter_semibold_italic = Cow::Borrowed(
            include_bytes!("../../assets/fonts/Inter/Inter-SemiBoldItalic.ttf").as_slice(),
        );
        let inter_bold =
            Cow::Borrowed(include_bytes!("../../assets/fonts/Inter/Inter-Bold.ttf").as_slice());
        let inter_bold_italic = Cow::Borrowed(
            include_bytes!("../../assets/fonts/Inter/Inter-BoldItalic.ttf").as_slice(),
        );

        cx.text_system()
            .add_fonts(vec![
                inter_regular,
                inter_italic,
                inter_medium,
                inter_medium_italic,
                inter_semibold,
                inter_semibold_italic,
                inter_bold,
                inter_bold_italic,
            ])
            .expect("Failed to load fonts");

        // Apply the system appearance before the first frame, so the app
        // never flashes the default light theme.
        apply_theme(cx.window_appearance().into(), cx);

        // Open the root window
        cx.open_window(WindowOptions::default(), |window, cx| {
            // Initialize components
            ui::init(cx);

            // Initialize theme registry
            theme::init(cx);

            // Initialize settings
            settings::init(window, cx);

            // Initialize the nostr client
            state::init(window, cx, None);

            // Initialize person registry
            person::init(window, cx);

            // Initialize device signer
            //
            // NIP-4e: https://github.com/nostr-protocol/nips/blob/per-device-keys/4e.md
            device::init(window, cx);

            // Initialize app registry
            chat::init(window, cx);

            // Root view
            cx.new(|cx| Root::new(workspace::init(window, cx).into(), window, cx))
        })
        .expect("Failed to open window. Please restart the application.");

        cx.activate(true);
    };

    #[cfg(target_family = "wasm")]
    APPLICATION.with(|application| {
        *application.borrow_mut() = Some(app.run_embedded(launch));
    });

    #[cfg(not(target_family = "wasm"))]
    app.run(launch);

    Ok(())
}
