use std::rc::Rc;

use gpui::prelude::FluentBuilder as _;
use gpui::{
    AnyElement, App, ClickEvent, ElementId, Hsla, InteractiveElement, IntoElement, MouseButton,
    ParentElement, RenderOnce, SharedString, StatefulInteractiveElement as _, StyleRefinement,
    Styled, Window, div, px, relative,
};
use gpui_base::Button as BaseButton;
use theme::{
    ActiveTheme, TextStyle, Typeset as _, bubble_radius, button_radius, plate_active, plate_hover,
    wash,
};

use crate::indicator::Indicator;
use crate::tooltip::Tooltip;
use crate::{Disableable, Icon, IconName, Selectable, Sizable, Size, h_flex};

#[derive(Clone, Copy, PartialEq, Eq)]
pub struct ButtonCustomVariant {
    color: Hsla,
    foreground: Hsla,
    hover: Hsla,
    active: Hsla,
}

impl ButtonCustomVariant {
    pub fn new(_window: &Window, cx: &App) -> Self {
        Self {
            color: cx.theme().solid,
            foreground: cx.theme().on_solid,
            hover: plate_hover(cx.theme().solid),
            active: plate_active(cx.theme().solid),
        }
    }

    pub fn color(mut self, color: Hsla) -> Self {
        self.color = color;
        self
    }

    pub fn foreground(mut self, color: Hsla) -> Self {
        self.foreground = color;
        self
    }

    pub fn hover(mut self, color: Hsla) -> Self {
        self.hover = color;
        self
    }

    pub fn active(mut self, color: Hsla) -> Self {
        self.active = color;
        self
    }
}

/// The variant of the Button.
#[derive(Clone, Copy, PartialEq, Eq, Default)]
pub enum ButtonVariant {
    #[default]
    Primary,
    Secondary,
    Danger,
    Warning,
    Ghost {
        alt: bool,
    },
    Transparent,
    Custom(ButtonCustomVariant),
}

pub trait ButtonVariants: Sized {
    fn with_variant(self, variant: ButtonVariant) -> Self;

    fn primary(self) -> Self {
        self.with_variant(ButtonVariant::Primary)
    }

    fn secondary(self) -> Self {
        self.with_variant(ButtonVariant::Secondary)
    }

    fn danger(self) -> Self {
        self.with_variant(ButtonVariant::Danger)
    }

    fn warning(self) -> Self {
        self.with_variant(ButtonVariant::Warning)
    }

    fn ghost(self) -> Self {
        self.with_variant(ButtonVariant::Ghost { alt: false })
    }

    fn ghost_alt(self) -> Self {
        self.with_variant(ButtonVariant::Ghost { alt: true })
    }

    fn transparent(self) -> Self {
        self.with_variant(ButtonVariant::Transparent)
    }

    fn custom(self, style: ButtonCustomVariant) -> Self {
        self.with_variant(ButtonVariant::Custom(style))
    }
}

/// Which side of the label the icon sits on.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum IconPosition {
    /// Left of the label.
    #[default]
    Start,
    /// Right of the label.
    End,
}

/// A Button element.
#[derive(IntoElement)]
#[allow(clippy::type_complexity)]
pub struct Button {
    base: BaseButton,
    icon: Option<Icon>,
    label: Option<SharedString>,
    tooltip: Option<SharedString>,
    children: Vec<AnyElement>,
    variant: ButtonVariant,
    size: Size,
    disabled: bool,
    loading: bool,
    rounded: bool,
    compact: bool,
    caret: bool,
    on_click: Option<Rc<dyn Fn(&ClickEvent, &mut Window, &mut App)>>,
    tab_stop: bool,
    selected: bool,
    icon_position: IconPosition,
}

impl From<Button> for AnyElement {
    fn from(button: Button) -> Self {
        button.into_any_element()
    }
}

impl Button {
    pub fn new(id: impl Into<ElementId>) -> Self {
        Self {
            base: BaseButton::new(id),
            icon: None,
            label: None,
            variant: ButtonVariant::default(),
            disabled: false,
            selected: false,
            compact: false,
            caret: false,
            rounded: false,
            size: Size::Medium,
            tooltip: None,
            on_click: None,
            loading: false,
            children: Vec::new(),
            tab_stop: true,
            icon_position: IconPosition::Start,
        }
    }

    pub fn rounded(mut self) -> Self {
        self.rounded = true;
        self
    }

    /// Set label to the Button, if no label is set, the button will be in Icon Button mode.
    pub fn label(mut self, label: impl Into<SharedString>) -> Self {
        self.label = Some(label.into());
        self
    }

    /// Set the icon of the button, if the Button has no label, the button will be in Icon Button mode.
    pub fn icon(mut self, icon: impl Into<Icon>) -> Self {
        self.icon = Some(icon.into());
        self
    }

    /// Set which side of the label the icon sits on (default [`IconPosition::Start`]).
    pub fn icon_position(mut self, position: IconPosition) -> Self {
        self.icon_position = position;
        self
    }

    pub fn tooltip(mut self, tooltip: impl Into<SharedString>) -> Self {
        self.tooltip = Some(tooltip.into());
        self
    }

    pub fn loading(mut self, loading: bool) -> Self {
        self.loading = loading;
        self
    }

    /// Set true to make the button compact (no padding).
    pub fn compact(mut self) -> Self {
        self.compact = true;
        self
    }

    pub fn caret(mut self) -> Self {
        self.caret = true;
        self
    }

    pub fn on_click(
        mut self,
        handler: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
    ) -> Self {
        self.on_click = Some(Rc::new(handler));
        self
    }

    /// Set whether the button can be focused with the tab key (default true).
    pub fn tab_stop(mut self, tab_stop: bool) -> Self {
        self.tab_stop = tab_stop;
        self
    }

    #[inline]
    fn clickable(&self) -> bool {
        !(self.disabled || self.loading) && self.on_click.is_some()
    }
}

impl Disableable for Button {
    fn disabled(mut self, disabled: bool) -> Self {
        self.disabled = disabled;
        self
    }
}

impl Selectable for Button {
    fn selected(mut self, selected: bool) -> Self {
        self.selected = selected;
        self
    }

    fn is_selected(&self) -> bool {
        self.selected
    }
}

impl Sizable for Button {
    fn with_size(mut self, size: impl Into<Size>) -> Self {
        self.size = size.into();
        self
    }
}

impl ButtonVariants for Button {
    fn with_variant(mut self, variant: ButtonVariant) -> Self {
        self.variant = variant;
        self
    }
}

impl Styled for Button {
    fn style(&mut self) -> &mut StyleRefinement {
        self.base.style()
    }
}

impl ParentElement for Button {
    fn extend(&mut self, elements: impl IntoIterator<Item = gpui::AnyElement>) {
        self.children.extend(elements)
    }
}

impl InteractiveElement for Button {
    fn interactivity(&mut self) -> &mut gpui::Interactivity {
        self.base.interactivity()
    }
}

impl RenderOnce for Button {
    fn render(self, _window: &mut Window, cx: &mut App) -> impl IntoElement {
        let style = self.variant;
        let clickable = self.clickable();
        let normal_style = style.normal(cx);
        let icon_size = match self.size {
            Size::Size(v) => Size::Size(v * 0.75),
            Size::Large => Size::Medium,
            _ => self.size,
        };
        let icon_start = self.icon_position == IconPosition::Start;

        self.base
            .tab_index(0)
            .tab_stop(self.tab_stop)
            .disabled(self.disabled)
            .when_some(self.on_click.clone(), |this, on_click| {
                this.on_click(move |event, window, cx| {
                    // Stop clicks when disabled, so a parent (e.g. dropdown menu) doesn't handle them.
                    if !clickable {
                        cx.stop_propagation();
                        return;
                    }

                    on_click(event, window, cx);
                })
            })
            .relative()
            .flex_shrink_0()
            .flex()
            .items_center()
            .justify_center()
            .cursor_default()
            .overflow_hidden()
            .map(|this| match self.rounded {
                false => this.rounded(px(button_radius())),
                true => this.rounded(px(bubble_radius())),
            })
            .when_some(
                match style {
                    ButtonVariant::Secondary => Some(cx.theme().border),
                    _ => None,
                },
                |this, border_color| this.border_1().border_color(border_color),
            )
            .when(!self.compact, |this| {
                if self.label.is_none() && self.children.is_empty() {
                    // Icon Button
                    match self.size {
                        Size::Size(px) => this.size(px),
                        Size::XSmall => this.size_5(),
                        Size::Small => this.size_6(),
                        Size::Medium => this.size_7(),
                        _ => this.size_9(),
                    }
                } else {
                    // Normal Button
                    let this = match self.size {
                        Size::Size(size) => this.px(size * 0.2),
                        Size::XSmall => {
                            if self.icon.is_some() {
                                this.h_6().pl_2().pr_2p5()
                            } else {
                                this.h_6().px_2()
                            }
                        }
                        Size::Small => {
                            if self.icon.is_some() {
                                this.h_7().pl_2().pr_2p5()
                            } else {
                                this.h_7().px_2()
                            }
                        }
                        Size::Medium => {
                            if self.icon.is_some() {
                                this.h_8().pl_3().pr_3p5()
                            } else {
                                this.h_8().px_3()
                            }
                        }
                        Size::Large => {
                            if self.icon.is_some() {
                                this.h_10().px_3().pr_3p5()
                            } else {
                                this.h_10().px_3()
                            }
                        }
                    };

                    if !icon_start && self.icon.is_some() {
                        match self.size {
                            Size::XSmall | Size::Small => this.pl_2p5().pr_2(),
                            Size::Medium | Size::Large => this.pl_3p5().pr_3(),
                            Size::Size(_) => this,
                        }
                    } else {
                        this
                    }
                }
            })
            .on_mouse_down(MouseButton::Left, move |_, window, cx| {
                // Stop clicks when disabled, so a parent (e.g. dropdown menu) doesn't handle them.
                if self.disabled {
                    cx.stop_propagation();
                    return;
                }
                // Avoid focus on mouse down.
                window.prevent_default();
            })
            .child({
                h_flex()
                    .id("label")
                    .justify_center()
                    .map(|this| match self.size {
                        Size::XSmall => this.text_style(TextStyle::Caption).gap_1(),
                        Size::Small => this.text_style(TextStyle::Callout).gap_1p5(),
                        _ => this.text_style(TextStyle::Callout).gap(px(theme::SPACE)),
                    })
                    .when(!self.loading && icon_start, |this| {
                        this.when_some(self.icon.clone(), |this, icon| {
                            this.child(icon.with_size(icon_size))
                        })
                    })
                    .when(self.loading && icon_start, |this| {
                        this.child(Indicator::new())
                    })
                    .when_some(self.label, |this, label| {
                        this.child(div().flex_none().line_height(relative(1.)).child(label))
                    })
                    .children(self.children)
                    .when(!self.loading && !icon_start, |this| {
                        this.when_some(self.icon.clone(), |this, icon| {
                            this.child(icon.with_size(icon_size))
                        })
                    })
                    .when(self.loading && !icon_start, |this| {
                        this.child(Indicator::new())
                    })
                    .when(self.caret, |this| {
                        this.justify_between().gap_0p5().child(
                            Icon::new(IconName::ChevronDown)
                                .small()
                                .text_color(cx.theme().text_muted),
                        )
                    })
            })
            .text_color(normal_style.fg)
            .when(!self.disabled && !self.selected, |this| {
                this.bg(normal_style.bg)
                    .hover(|this| {
                        let hover_style = style.hovered(cx);
                        this.bg(hover_style.bg).text_color(hover_style.fg)
                    })
                    .active(|this| {
                        let active_style = style.active(cx);
                        this.bg(active_style.bg).text_color(active_style.fg)
                    })
            })
            .when(self.selected, |this| {
                let selected_style = style.selected(cx);
                this.bg(selected_style.bg).text_color(selected_style.fg)
            })
            .when(self.disabled, |this| {
                let disabled_style = style.disabled(cx);
                this.cursor_not_allowed()
                    .bg(disabled_style.bg)
                    .text_color(disabled_style.fg)
            })
            .when(self.loading && !self.disabled, |this| {
                this.bg(normal_style.bg.opacity(0.8))
                    .text_color(normal_style.fg.opacity(0.8))
            })
            .when_some(self.tooltip.clone(), |this, tooltip| {
                this.tooltip(move |window, cx| Tooltip::new(tooltip.clone(), window, cx).into())
            })
    }
}

struct ButtonVariantStyle {
    bg: Hsla,
    fg: Hsla,
}

impl ButtonVariant {
    fn normal(&self, cx: &App) -> ButtonVariantStyle {
        let bg = self.bg_color(cx);
        let fg = self.text_color(cx);

        ButtonVariantStyle { bg, fg }
    }

    fn bg_color(&self, cx: &App) -> Hsla {
        match self {
            ButtonVariant::Primary => cx.theme().solid,
            ButtonVariant::Secondary => cx.theme().surface_raised,
            ButtonVariant::Danger => cx.theme().danger_strong,
            ButtonVariant::Warning => cx.theme().warning_muted,
            ButtonVariant::Ghost { alt } => {
                if *alt {
                    cx.theme().surface_raised
                } else {
                    gpui::transparent_black()
                }
            }
            ButtonVariant::Custom(colors) => colors.color,
            _ => gpui::transparent_black(),
        }
    }

    fn text_color(&self, cx: &App) -> Hsla {
        match self {
            ButtonVariant::Primary => cx.theme().on_solid,
            ButtonVariant::Secondary => cx.theme().text,
            // Bezel's destructive pairing: the red plate carries white ink in
            // both appearances, not the appearance-flipping on_accent.
            ButtonVariant::Danger => gpui::white(),
            // warning_muted is a translucent amber wash, so the label is the
            // amber token itself — dark amber in light, bright in dark.
            ButtonVariant::Warning => cx.theme().warning,
            ButtonVariant::Transparent => cx.theme().text_faint,
            ButtonVariant::Ghost { alt } => {
                if *alt {
                    cx.theme().text
                } else {
                    cx.theme().text_muted
                }
            }
            ButtonVariant::Custom(colors) => colors.foreground,
        }
    }

    fn hovered(&self, cx: &App) -> ButtonVariantStyle {
        let bg = match self {
            ButtonVariant::Primary => plate_hover(cx.theme().solid),
            ButtonVariant::Secondary => cx.theme().surface_raised_hover,
            ButtonVariant::Danger => plate_hover(cx.theme().danger_strong),
            ButtonVariant::Warning => plate_hover(cx.theme().warning_muted),
            ButtonVariant::Ghost { .. } => cx.theme().element_hover,
            ButtonVariant::Transparent => gpui::transparent_black(),
            ButtonVariant::Custom(colors) => colors.hover,
        };

        let fg = match self {
            ButtonVariant::Secondary => cx.theme().text,
            ButtonVariant::Ghost { .. } => cx.theme().text,
            ButtonVariant::Transparent => cx.theme().text_faint,
            _ => self.text_color(cx),
        };

        ButtonVariantStyle { bg, fg }
    }

    fn active(&self, cx: &App) -> ButtonVariantStyle {
        let bg = match self {
            ButtonVariant::Primary => plate_active(cx.theme().solid),
            ButtonVariant::Secondary => plate_active(cx.theme().surface_raised),
            ButtonVariant::Danger => plate_active(cx.theme().danger_strong),
            ButtonVariant::Warning => plate_active(cx.theme().warning_muted),
            ButtonVariant::Ghost { .. } => cx.theme().element_active,
            ButtonVariant::Transparent => gpui::transparent_black(),
            ButtonVariant::Custom(colors) => colors.active,
        };

        let fg = match self {
            ButtonVariant::Secondary => cx.theme().text,
            ButtonVariant::Transparent => cx.theme().text_faint,
            _ => self.text_color(cx),
        };

        ButtonVariantStyle { bg, fg }
    }

    fn selected(&self, cx: &App) -> ButtonVariantStyle {
        let bg = match self {
            ButtonVariant::Primary => cx.theme().element_active,
            ButtonVariant::Secondary => cx.theme().element_active,
            ButtonVariant::Danger => plate_active(cx.theme().danger_strong),
            ButtonVariant::Warning => plate_active(cx.theme().warning_muted),
            ButtonVariant::Ghost { .. } => cx.theme().element_active,
            ButtonVariant::Transparent => gpui::transparent_black(),
            ButtonVariant::Custom(colors) => colors.active,
        };

        let fg = match self {
            ButtonVariant::Secondary => cx.theme().text,
            ButtonVariant::Transparent => cx.theme().text_faint,
            _ => self.text_color(cx),
        };

        ButtonVariantStyle { bg, fg }
    }

    fn disabled(&self, cx: &App) -> ButtonVariantStyle {
        let bg = wash(0.08);
        let fg = cx.theme().text_muted;

        ButtonVariantStyle { bg, fg }
    }
}
