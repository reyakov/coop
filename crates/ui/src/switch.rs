use std::rc::Rc;
use std::time::Duration;

use gpui::prelude::FluentBuilder as _;
use gpui::{
    App, ElementId, IntoElement, ParentElement as _, RenderOnce, SharedString, Styled as _, Window,
    div, px, white,
};
use gpui_base::{Spring, Switch as BaseSwitch, SwitchThumb, SwitchTrack, spring};
use theme::{ActiveTheme, Side};

use crate::{Disableable, Sizable, Size};

type OnClick = Option<Rc<dyn Fn(&bool, &mut Window, &mut App)>>;

#[derive(IntoElement)]
pub struct Switch {
    id: ElementId,
    checked: bool,
    disabled: bool,
    label: Option<SharedString>,
    description: Option<SharedString>,
    label_side: Side,
    on_click: OnClick,
    size: Size,
}

impl Switch {
    pub fn new(id: impl Into<ElementId>) -> Self {
        let id: ElementId = id.into();

        Self {
            id: id.clone(),
            checked: false,
            disabled: false,
            label: None,
            description: None,
            on_click: None,
            label_side: Side::Left,
            size: Size::Medium,
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

    pub fn label_side(mut self, label_side: Side) -> Self {
        self.label_side = label_side;
        self
    }
}

impl Sizable for Switch {
    fn with_size(mut self, size: impl Into<Size>) -> Self {
        self.size = size.into();
        self
    }
}

impl Disableable for Switch {
    fn disabled(mut self, disabled: bool) -> Self {
        self.disabled = disabled;
        self
    }
}

impl RenderOnce for Switch {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let checked = self.checked;
        let on_click = self.on_click.clone();

        let (bg, toggle_bg) = match checked {
            true => (cx.theme().element_background, white()),
            false => (cx.theme().elevated_surface_background, white()),
        };

        let (bg, toggle_bg) = match self.disabled {
            true => (bg.opacity(0.3), toggle_bg.opacity(0.8)),
            false => (bg, toggle_bg),
        };

        let (bg_width, bg_height) = match self.size {
            Size::XSmall | Size::Small => (px(28.), px(16.)),
            _ => (px(36.), px(20.)),
        };

        let bar_width = match self.size {
            Size::XSmall | Size::Small => px(12.),
            _ => px(16.),
        };

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
                .disabled(self.disabled)
                .when_some(accessibility_label, |this, label| {
                    this.accessibility_label(label)
                })
                .when_some(on_click, |this, on_click| {
                    this.on_change(move |next, _event, window, cx| on_click(&next, window, cx))
                })
                .when(self.label_side.is_left(), |this| this.flex_row_reverse())
                .child(
                    div()
                        .w_full()
                        .flex()
                        .justify_between()
                        .items_center()
                        .gap_4()
                        .when_some(label, |this, label| {
                            // Label
                            this.child(div().text_sm().text_color(cx.theme().text).child(label))
                        })
                        .child(
                            // Switch Bar
                            SwitchTrack::new((self.id.clone(), "track"))
                                .checked(checked)
                                .disabled(self.disabled)
                                .flex_shrink_0()
                                .w(bg_width)
                                .h(bg_height)
                                .rounded(bg_height / 2.)
                                .flex()
                                .items_center()
                                .border(inset)
                                .border_color(cx.theme().border_transparent)
                                .bg(bg)
                                .when(!self.disabled, |this| this.cursor_pointer())
                                .child(
                                    // Switch Toggle
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
                            .text_xs()
                            .text_color(cx.theme().text_muted)
                            .child(description),
                    )
                }),
        )
    }
}
