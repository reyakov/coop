use gpui::{div, App, AppContext, Context, Entity, IntoElement, Render, Window};

pub fn init(window: &mut Window, cx: &mut App) -> Entity<Preferences> {
    cx.new(|cx| Preferences::new(window, cx))
}

pub struct Preferences {
    //
}

impl Preferences {
    pub fn new(_window: &mut Window, _cx: &mut App) -> Self {
        Self {}
    }
}

impl Render for Preferences {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        div()
    }
}
