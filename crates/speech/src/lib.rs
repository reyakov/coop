#[cfg(not(target_arch = "wasm32"))]
pub mod model;
#[cfg(not(target_arch = "wasm32"))]
mod parakeet;
pub mod speaker;
#[cfg(target_arch = "wasm32")]
mod web;

use std::time::Duration;

use gpui::{
    Animation, AnimationExt as _, AnyElement, App, IntoElement as _, ParentElement as _,
    Styled as _, div,
};
use gpui_kit::component::ActiveTheme as _;
use gpui_kit::component::speech::SpeechRecognizer;

/// A ticking `M:SS` counter for the dictation in progress.
pub fn elapsed(started: instant::Instant, cx: &App) -> AnyElement {
    div()
        .text_xs()
        .text_color(cx.theme().muted_foreground)
        .with_animation(
            "dictation-elapsed",
            Animation::new(Duration::from_secs(1))
                .repeat()
                .with_max_fps(1.),
            move |counter, _| counter.child(format_elapsed(started)),
        )
        .into_any_element()
}

fn format_elapsed(started: instant::Instant) -> String {
    let seconds = started.elapsed().as_secs();
    format!("{}:{:02}", seconds / 60, seconds % 60)
}

/// The recognizer backing the composer's microphone button.
#[cfg(not(target_arch = "wasm32"))]
pub fn recognizer() -> impl SpeechRecognizer {
    parakeet::ParakeetRecognizer
}

/// The recognizer backing the composer's microphone button.
#[cfg(target_arch = "wasm32")]
pub fn recognizer() -> impl SpeechRecognizer {
    web::WebSpeechRecognizer
}

/// The audio source backing the composer's microphone button on the web.
#[cfg(target_arch = "wasm32")]
pub fn input() -> impl gpui_kit::component::speech::AudioInput {
    web::SelfCapturedInput
}
