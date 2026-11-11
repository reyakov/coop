use std::sync::{Arc, Mutex};

use chat_ui::ToggleDictation;
use gpui::{
    App, Bounds, KeyBinding, Menu, MenuItem, SharedString, TitlebarOptions,
    WindowBackgroundAppearance, WindowBounds, WindowDecorations, WindowKind, WindowOptions,
    actions, point, px, size,
};
use gpui_platform::application;
use nostr_sdk::prelude::SecretKey;
use state::{APP_ID, CLIENT_NAME};
use workspace::TAB_BAR_HEIGHT;

actions!(coop, [Quit]);

fn main() {
    // Initialize logging
    tracing_subscriber::fmt::init();

    // Parse CLI arguments for --sec <nsec1>
    let cli_key = parse_cli_key();
    if let Err(ref e) = cli_key {
        eprintln!("Failed to parse --sec argument: {e}");
        std::process::exit(1);
    }
    let cli_key = cli_key.unwrap();

    // Run application
    application()
        .with_assets(assets::CombinedAssets)
        .with_http_client(Arc::new(reqwest_client::ReqwestClient::new()))
        .run(move |cx| {
            // Initialize the framework before anything renders
            gpui_kit::init(cx);

            // Register the bundled gpui-kit themes before anything renders
            settings::init_themes(cx);

            // Load embedded fonts in assets/fonts
            load_embedded_fonts(cx);

            // Set app identity
            cx.set_app_identity(APP_ID, CLIENT_NAME);

            // Register the `quit` function
            cx.on_action(quit);

            // Register the `quit` function with CMD+Q (macOS) and dictation with CMD+D
            #[cfg(target_os = "macos")]
            cx.bind_keys([
                KeyBinding::new("cmd-q", Quit, None),
                KeyBinding::new("cmd-d", ToggleDictation, None),
            ]);

            // Register the `quit` function with Super+Q (others) and dictation with Ctrl+D
            #[cfg(not(target_os = "macos"))]
            cx.bind_keys([
                KeyBinding::new("super-q", Quit, None),
                KeyBinding::new("ctrl-d", ToggleDictation, None),
            ]);

            // Set menu items
            cx.set_menus(vec![Menu {
                name: "Coop".into(),
                items: vec![MenuItem::action("Quit", Quit)],
                disabled: false,
            }]);

            // Initialize settings
            settings::init(cx);

            // Apply the persisted appearance, theme selection and tuning
            settings::apply_persisted_theme(cx);

            // Initialize the nostr client
            state::init(cx, cli_key);

            // Initialize person registry
            person::init(cx);

            // Initialize device signer
            //
            // NIP-4e: https://github.com/nostr-protocol/nips/blob/per-device-keys/4e.md
            nip4e::init(cx);

            // Initialize cashu wallet registry
            //
            // NIP-60: https://github.com/nostr-protocol/nips/blob/master/60.md
            nip60::init(cx);

            // Initialize app registry
            chat::init(cx);

            // Initialize groups registry
            nip29::init(cx);

            // Initialize auto update
            auto_update::init(cx);

            // Set up the window bounds
            let bounds = Bounds::centered(None, size(px(960.0), px(720.0)), cx);

            // Set up the window options
            let opts = WindowOptions {
                window_background: WindowBackgroundAppearance::Opaque,
                window_decorations: Some(WindowDecorations::Client),
                window_bounds: Some(WindowBounds::Windowed(bounds)),
                kind: WindowKind::Normal,
                app_id: Some(APP_ID.to_owned()),
                titlebar: Some(TitlebarOptions {
                    title: Some(SharedString::new_static(CLIENT_NAME)),
                    traffic_light_position: Some(point(
                        px(9.0),
                        px(TAB_BAR_HEIGHT / px(2.) - 14. / 2.),
                    )),
                    appears_transparent: true,
                }),
                app_owns_titlebar_drag: true,
                ..Default::default()
            };

            gpui_kit::open_window(opts, cx, workspace::init)
                .expect("Failed to open window. Please restart the application.");

            cx.activate(true);
        });
}

fn load_embedded_fonts(cx: &App) {
    let asset_source = cx.asset_source();
    let font_paths = asset_source.list("fonts").unwrap();
    let embedded_fonts = Mutex::new(vec![]);
    let executor = cx.background_executor();

    cx.foreground_executor().block_on(executor.scoped(|scope| {
        for font_path in &font_paths {
            if !font_path.ends_with(".ttf") {
                continue;
            }

            scope.spawn(async {
                let font_bytes = asset_source.load(font_path.as_str()).unwrap().unwrap();
                embedded_fonts.lock().unwrap().push(font_bytes);
            });
        }
    }));

    cx.text_system()
        .add_fonts(embedded_fonts.into_inner().unwrap())
        .unwrap();
}

fn parse_cli_key() -> Result<Option<SecretKey>, String> {
    let args: Vec<String> = std::env::args().collect();
    let mut i = 0;
    while i < args.len() {
        if args[i] == "--sec" {
            if i + 1 < args.len() {
                let nsec = &args[i + 1];
                return SecretKey::parse(nsec)
                    .map(Some)
                    .map_err(|e| format!("Invalid nsec key '{nsec}': {e}"));
            } else {
                return Err("--sec requires a value (nsec1...)".to_string());
            }
        }
        i += 1;
    }
    Ok(None)
}

fn quit(_ev: &Quit, cx: &mut App) {
    log::info!("Gracefully quitting the application . . .");
    cx.quit();
}
