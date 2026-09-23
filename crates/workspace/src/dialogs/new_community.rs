use community::{CommunityMetadata, CommunityRegistry};
use gpui::{App, AppContext, ParentElement, Window, px};
use ui::WindowExtension;
use ui::input::{Input, InputState};

pub fn open(window: &mut Window, cx: &mut App) {
    let name_input = cx.new(|cx| InputState::new(window, cx).placeholder("Community name"));

    window.open_modal(cx, move |this, _window, _cx| {
        let name_input = name_input.clone();

        this.width(px(380.))
            .confirm()
            .title("New community")
            .child(Input::new(&name_input))
            .on_ok(move |_event, _window, cx| {
                let name = name_input.read(cx).value().trim().to_owned();

                if name.is_empty() {
                    return false;
                }

                let metadata = CommunityMetadata {
                    name,
                    ..CommunityMetadata::default()
                };

                CommunityRegistry::global(cx)
                    .update(cx, |registry, cx| registry.create(metadata, cx));

                true
            })
    });
}
