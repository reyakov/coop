use std::sync::{Arc, Mutex};

use assets::Assets;
use gpui::{
    App, AppContext, Bounds, KeyBinding, Menu, MenuItem, SharedString, TitlebarOptions,
    WindowBackgroundAppearance, WindowBounds, WindowDecorations, WindowKind, WindowOptions,
    actions, point, px, size,
};
use gpui_platform::application;
use nostr_sdk::prelude::SecretKey;
use state::{APP_ID, CLIENT_NAME};
use theme::TABBAR_HEIGHT;
use ui::Root;

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
        .with_assets(Assets)
        .with_http_client(Arc::new(reqwest_client::ReqwestClient::new()))
        .run(move |cx| {
            // Initialize components
            ui::init(cx);

            // Initialize theme registry
            theme::init(cx);

            // Load embedded fonts in assets/fonts
            load_embedded_fonts(cx);

            // Set app identity
            cx.set_app_identity(APP_ID, CLIENT_NAME);

            // Register the `quit` function
            cx.on_action(quit);

            // Register the `quit` function with CMD+Q (macOS)
            #[cfg(target_os = "macos")]
            cx.bind_keys([KeyBinding::new("cmd-q", Quit, None)]);

            // Register the `quit` function with Super+Q (others)
            #[cfg(not(target_os = "macos"))]
            cx.bind_keys([KeyBinding::new("super-q", Quit, None)]);

            // Set menu items
            cx.set_menus(vec![Menu {
                name: "Coop".into(),
                items: vec![MenuItem::action("Quit", Quit)],
                disabled: false,
            }]);

            // Initialize settings
            settings::init(cx);

            // Initialize the nostr client
            state::init(cx, cli_key);

            // Initialize person registry
            person::init(cx);

            // Initialize device signer
            //
            // NIP-4e: https://github.com/nostr-protocol/nips/blob/per-device-keys/4e.md
            device::init(cx);

            // Initialize app registry
            chat::init(cx);

            // Initialize community registry
            community::init(cx);

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
                        px(TABBAR_HEIGHT / px(2.) - 14. / 2.),
                    )),
                    appears_transparent: true,
                }),
                app_owns_titlebar_drag: true,
                ..Default::default()
            };

            cx.open_window(opts, |window, cx| {
                cx.new(|cx| Root::new(workspace::init(window, cx).into(), window, cx))
            })
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
