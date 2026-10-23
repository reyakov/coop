use gpui::AssetSource;

#[test]
fn icons_resolve_from_the_vendored_folder() {
    // Icons coop's code and the kit components draw, served by the vendored
    // folder (signed's set, filled with the kit bundle where coop needs more).
    let vendored = [
        "icons/search.svg",
        "icons/close.svg",
        "icons/panel-left.svg",
        "icons/window-minimize.svg",
        "icons/settings-2.svg",
        "icons/x.svg",
        "icons/compass.svg",
        "icons/radio-tower.svg",
    ];
    for path in vendored {
        let own = assets::Assets
            .load(path)
            .unwrap_or_else(|e| panic!("{path} errored in own embed: {e}"));
        assert!(own.is_some(), "own embed is missing {path}");

        let combined = assets::CombinedAssets.load(path).unwrap();
        assert!(combined.is_some(), "CombinedAssets fails to serve {path}");
    }

    // Coop's own files still load through the combined source.
    let own = [
        "brand/coop.svg",
        "brand/bg1.jpg",
        "fonts/Inter/Inter-Medium.ttf",
    ];
    for path in own {
        assert!(
            matches!(assets::CombinedAssets.load(path), Ok(Some(_))),
            "CombinedAssets fails to serve coop asset {path}"
        );
    }
}
