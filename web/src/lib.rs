#![cfg(target_arch = "wasm32")]

use std::cell::RefCell;
use std::rc::Rc;
use std::sync::Arc;

use gpui::*;
use universal_time::{Instant, MonotonicClock, SystemTime, WallClock, define_time_provider};
use wasm_bindgen::prelude::*;

thread_local! {
    /// Dropping the handle releases the app, so the page's lifetime must own it.
    static APPLICATION: RefCell<Option<ApplicationHandle>> = const { RefCell::new(None) };
}

/// Serves `Date.now()` as the wall clock and `performance.now()` as the monotonic clock.
struct BrowserTimeProvider;

impl WallClock for BrowserTimeProvider {
    fn system_time(&self) -> SystemTime {
        SystemTime::from_unix_duration(core::time::Duration::from_secs_f64(
            js_sys::Date::now() / 1000.0,
        ))
    }
}

impl MonotonicClock for BrowserTimeProvider {
    fn instant(&self) -> Instant {
        Instant::from_ticks(core::time::Duration::from_secs_f64(instant::now() / 1000.0))
    }
}

define_time_provider!(BrowserTimeProvider);

fn init_logging() {
    // Called from both entry points; the first one wins
    static ONCE: std::sync::OnceLock<()> = std::sync::OnceLock::new();

    ONCE.get_or_init(|| {
        console_error_panic_hook::set_once();

        // Initialize logging to browser console
        console_log::init_with_level(log::Level::Info).ok();

        // Also initialize tracing for WASM
        tracing_wasm::set_as_global_default();
    });
}

/// Open the persistent nostr database before the app starts.
#[wasm_bindgen]
pub async fn open_database() -> Result<(), JsValue> {
    init_logging();

    state::web_database::open().await;

    Ok(())
}

#[wasm_bindgen]
pub fn run() -> Result<(), JsValue> {
    init_logging();

    // Sets the panic hook and GPUI's console logger
    gpui_platform::web_init();

    let platform = std::rc::Rc::new(gpui_web::WebPlatform::new_with_backend_and_font_fallback(
        false,
        gpui_web::WebBackendPreference::Auto,
        gpui_web::CanvasFontFallback::EmojiAndCjk,
    ));

    let http_client = Arc::new(platform.fetch_http_client());

    let app = Application::with_platform(platform)
        .with_http_client(http_client)
        .with_assets(assets::CombinedAssets);

    APPLICATION.with(|application| {
        *application.borrow_mut() = Some(app.run_embedded(|cx| {
            gpui_kit::init(cx);

            load_embedded_fonts(cx);
            apply_web_fonts(cx);

            // Initialize settings
            settings::init(cx);

            apply_appearance(settings::persisted_appearance_mode(), cx);
            settings::AppSettings::apply_theme_style(cx);

            // Initialize the nostr client
            state::init(cx, None);

            // Initialize person registry
            person::init(cx);

            // Initialize device signer
            //
            // NIP-4e: https://github.com/nostr-protocol/nips/blob/per-device-keys/4e.md
            nip4e::init(cx);

            // Initialize app registry
            chat::init(cx);

            // Initialize groups registry
            nip29::init(cx);

            gpui_kit::open_window(WindowOptions::default(), cx, |window, cx| {
                workspace::init(window, cx)
            })
            .expect("Failed to open window. Please restart the application.");

            cx.activate(true);
        }));
    });

    Ok(())
}

/// Register every bundled font with the text system before any text is laid out
fn load_embedded_fonts(cx: &mut App) {
    let font_paths = assets::Assets::iter()
        .filter(|path| path.starts_with("fonts/") && path.ends_with(".ttf"))
        .collect::<Vec<_>>();

    let fonts = font_paths
        .iter()
        .filter_map(|path| assets::Assets::get(path.as_ref()))
        .map(|font| font.data)
        .collect::<Vec<_>>();

    cx.text_system()
        .add_fonts(fonts)
        .expect("Failed to load bundled fonts");
}

fn apply_web_fonts(cx: &mut App) {
    use gpui_kit::component::Theme;

    const UI_FONT: &str = "Inter";

    Theme::update(cx, |theme| {
        theme.font_family = UI_FONT.into();
        theme.mono_font_family = UI_FONT.into();

        for config in [&mut theme.light_theme, &mut theme.dark_theme] {
            let config = Rc::make_mut(config);
            config.font_family = Some(UI_FONT.into());
            config.mono_font_family = Some(UI_FONT.into());
        }
    });
}

/// Honor the appearance mode against the default palettes
fn apply_appearance(appearance: settings::AppearanceMode, cx: &mut App) {
    use gpui_kit::component::{Theme, ThemeMode};

    match appearance {
        settings::AppearanceMode::System => Theme::sync_system_appearance(None, cx),
        settings::AppearanceMode::Light => Theme::change(ThemeMode::Light, None, cx),
        settings::AppearanceMode::Dark => Theme::change(ThemeMode::Dark, None, cx),
    }
}
