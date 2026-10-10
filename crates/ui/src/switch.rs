use std::rc::Rc;
use std::time::Duration;

use gpui::prelude::FluentBuilder as _;
use gpui::{
    App, ElementId, IntoElement, ParentElement as _, RenderOnce, SharedString, Styled as _, Window,
    div, px, transparent_black,
};
use gpui_base::{Spring, Switch as BaseSwitch, SwitchThumb, SwitchTrack, spring};
use theme::{ActiveTheme, TextStyle, Typeset as _};

type OnClick = Option<Rc<dyn Fn(&bool, &mut Window, &mut App)>>;

#[derive(IntoElement)]
pub struct Switch {
    id: ElementId,
    checked: bool,
    label: Option<SharedString>,
    description: Option<SharedString>,
    on_click: OnClick,
}

impl Switch {
    pub fn new(id: impl Into<ElementId>) -> Self {
        let id: ElementId = id.into();

        Self {
            id: id.clone(),
            checked: false,
            label: None,
            description: None,
            on_click: None,
        }
    }

    pub fn checked(mut self, checked: bool) -> Self {
        self.checked = checked;
        self
    }

    pub fn label(mut self, label: impl Into<SharedString>) -> Self {
        self.label = Some(label.into());
        self
    }

    pub fn description(mut self, description: impl Into<SharedString>) -> Self {
        self.description = Some(description.into());
        self
    }

    pub fn on_click<F>(mut self, handler: F) -> Self
    where
        F: Fn(&bool, &mut Window, &mut App) + 'static,
    {
        self.on_click = Some(Rc::new(handler));
        self
    }
}

impl RenderOnce for Switch {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let checked = self.checked;
        let on_click = self.on_click.clone();

        let bg = if checked {
            cx.theme().accent_strong
        } else {
            cx.theme().input_bg
        };
        let toggle_bg = cx.theme().on_solid;
        // The border is structural (it insets the thumb); it only paints when off.
        let track_border = if checked {
            transparent_black()
        } else {
            cx.theme().border
        };

        let bg_width = px(36.);
        let bg_height = px(20.);
        let bar_width = px(16.);
        let inset = px(2.);

        let thumb_left = spring(
            (self.id.clone(), "thumb"),
            if checked {
                bg_width - bar_width - inset * 2.
            } else {
                px(0.)
            },
            Spring::new(Duration::from_secs_f64(0.15)),
            window,
            cx,
        );

        let accessibility_label = self.label.clone();
        let label = self.label;

        div().child(
            BaseSwitch::new(self.id.clone())
                .checked(checked)
                .when_some(accessibility_label, |this, label| {
                    this.accessibility_label(label)
                })
                .when_some(on_click, |this, on_click| {
                    this.on_change(move |next, _event, window, cx| on_click(&next, window, cx))
                })
                .flex_row_reverse()
                .child(
                    div()
                        .w_full()
                        .flex()
                        .justify_between()
                        .items_center()
                        .gap_4()
                        .when_some(label, |this, label| {
                            this.child(
                                div()
                                    .text_style(TextStyle::Callout)
                                    .text_color(cx.theme().text)
                                    .child(label),
                            )
                        })
                        .child(
                            SwitchTrack::new((self.id.clone(), "track"))
                                .checked(checked)
                                .flex_shrink_0()
                                .w(bg_width)
                                .h(bg_height)
                                .rounded(bg_height / 2.)
                                .flex()
                                .items_center()
                                .border(inset)
                                .border_color(track_border)
                                .bg(bg)
                                .cursor_pointer()
                                .child(
                                    SwitchThumb::new(checked)
                                        .rounded_full()
                                        .when(cx.theme().shadow, |this| this.shadow_sm())
                                        .bg(toggle_bg)
                                        .size(bar_width)
                                        .left(thumb_left),
                                ),
                        ),
                )
                .when_some(self.description.clone(), |this, description| {
                    this.child(
                        div()
                            .pr_3()
                            .text_style(TextStyle::Caption)
                            .text_color(cx.theme().text_muted)
                            .child(description),
                    )
                }),
        )
    }
}
