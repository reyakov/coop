use gpui::prelude::FluentBuilder as _;
use gpui::{
    App, Hsla, IntoElement, ParentElement, RenderOnce, Styled as _, Transformation, Window, div,
    percentage,
};
use instant::Duration;

use crate::motion::{EASE_IN_OUT, MotionSpec, Painter, pulse_delta};
use crate::{Icon, IconName, Sizable, Size};

#[derive(IntoElement)]
pub struct Indicator {
    size: Size,
    icon: Icon,
    speed: Duration,
    color: Option<Hsla>,
}

impl Default for Indicator {
    fn default() -> Self {
        Self::new()
    }
}

impl Indicator {
    pub fn new() -> Self {
        Self {
            size: Size::Small,
            speed: Duration::from_secs(1),
            icon: Icon::new(IconName::Loader),
            color: None,
        }
    }

    pub fn color(mut self, color: Hsla) -> Self {
        self.color = Some(color);
        self
    }
}

impl Sizable for Indicator {
    fn with_size(mut self, size: impl Into<Size>) -> Self {
        self.size = size.into();
        self
    }
}

impl RenderOnce for Indicator {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let spec = MotionSpec::new(self.speed.as_millis().max(1) as u64, EASE_IN_OUT);
        let phase = pulse_delta(&spec, Painter::from(window.current_view()), cx);
        div().child(
            self.icon
                .with_size(self.size)
                .when_some(self.color, |this, color| this.text_color(color))
                .transform(Transformation::rotate(percentage(phase))),
        )
    }
}
