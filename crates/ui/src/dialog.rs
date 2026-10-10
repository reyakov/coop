use std::rc::Rc;

use gpui::prelude::FluentBuilder;
use gpui::{
    AnimationExt as _, AnyElement, App, ClickEvent, Div, FocusHandle, IntoElement, ParentElement,
    Pixels, RenderOnce, SharedString, StyleRefinement, Styled, Window, div, px, transparent_black,
};
use gpui_base::{DialogBackdrop, DialogPopup, DialogTitle};
use theme::{ActiveTheme, SCRIM_ALPHA_DARK, scrim, surface_radius, surface_shadows};

use crate::button::{Button, ButtonCustomVariant, ButtonVariant, ButtonVariants as _};
use crate::motion::DIALOG_IN;
use crate::scroll::ScrollableElement;
use crate::{IconName, Root, StyledExt, WindowExtension, h_flex, v_flex};

type OnOk = Option<Rc<dyn Fn(&ClickEvent, &mut Window, &mut App) -> bool + 'static>>;
type OnCancel = Rc<dyn Fn(&ClickEvent, &mut Window, &mut App) -> bool + 'static>;
type RenderButtonFn = Box<dyn FnOnce(&mut Window, &mut App) -> AnyElement>;
type FooterFn =
    Box<dyn Fn(RenderButtonFn, RenderButtonFn, &mut Window, &mut App) -> Vec<AnyElement>>;

/// Dialog button props.
pub struct DialogButtonProps {
    ok_text: Option<SharedString>,
    ok_variant: ButtonVariant,
    cancel_text: Option<SharedString>,
    cancel_variant: ButtonVariant,
}

impl Default for DialogButtonProps {
    fn default() -> Self {
        Self {
            ok_text: None,
            ok_variant: ButtonVariant::Primary,
            cancel_text: None,
            cancel_variant: ButtonVariant::Ghost { alt: false },
        }
    }
}

impl DialogButtonProps {
    /// Sets the text of the OK button. Default is `OK`.
    pub fn ok_text(mut self, ok_text: impl Into<SharedString>) -> Self {
        self.ok_text = Some(ok_text.into());
        self
    }

    /// Sets the variant of the OK button. Default is `ButtonVariant::Primary`.
    pub fn ok_variant(mut self, ok_variant: ButtonVariant) -> Self {
        self.ok_variant = ok_variant;
        self
    }

    /// Sets the text of the Cancel button. Default is `Cancel`.
    pub fn cancel_text(mut self, cancel_text: impl Into<SharedString>) -> Self {
        self.cancel_text = Some(cancel_text.into());
        self
    }
}

#[derive(IntoElement)]
pub struct Dialog {
    style: StyleRefinement,
    title: Option<AnyElement>,
    footer: Option<FooterFn>,
    content: Div,
    width: Pixels,
    max_width: Option<Pixels>,

    on_ok: OnOk,
    on_cancel: OnCancel,

    overlay: bool,
    overlay_closable: bool,
    keyboard: bool,
    show_close: bool,
    button_props: DialogButtonProps,

    /// The focus handle created when the dialog is opened, owned by the `Root`.
    pub(crate) focus_handle: FocusHandle,
    pub(crate) layer_ix: usize,
    pub(crate) overlay_visible: bool,
}

impl Dialog {
    pub fn new(_window: &mut Window, cx: &mut App) -> Self {
        Self {
            style: StyleRefinement::default(),
            focus_handle: cx.focus_handle(),
            title: None,
            footer: None,
            content: v_flex(),
            width: px(380.),
            max_width: None,
            overlay: true,
            keyboard: true,
            layer_ix: 0,
            overlay_visible: false,
            on_ok: None,
            on_cancel: Rc::new(|_, _, _| true),
            button_props: DialogButtonProps::default(),
            show_close: true,
            overlay_closable: true,
        }
    }

    /// Sets the title of the dialog.
    pub fn title(mut self, title: impl IntoElement) -> Self {
        self.title = Some(title.into_any_element());
        self
    }

    /// Sets a custom footer, replacing the default footer buttons.
    fn footer<E, F>(mut self, footer: F) -> Self
    where
        E: IntoElement,
        F: Fn(RenderButtonFn, RenderButtonFn, &mut Window, &mut App) -> Vec<E> + 'static,
    {
        self.footer = Some(Box::new(move |ok, cancel, window, cx| {
            footer(ok, cancel, window, cx)
                .into_iter()
                .map(|e| e.into_any_element())
                .collect()
        }));
        self
    }

    /// Set to use confirm dialog, with OK and Cancel buttons.
    pub fn confirm(self) -> Self {
        self.footer(|ok, cancel, window, cx| vec![cancel(window, cx), ok(window, cx)])
            .overlay_closable(false)
            .show_close(false)
    }

    /// Set the button props of the dialog.
    pub fn button_props(mut self, button_props: DialogButtonProps) -> Self {
        self.button_props = button_props;
        self
    }

    /// Sets the confirm callback, returning `false` keeps the dialog open.
    pub fn on_ok(
        mut self,
        on_ok: impl Fn(&ClickEvent, &mut Window, &mut App) -> bool + 'static,
    ) -> Self {
        self.on_ok = Some(Rc::new(on_ok));
        self
    }

    /// Sets the cancel callback, returning `false` keeps the dialog open.
    pub fn on_cancel(
        mut self,
        on_cancel: impl Fn(&ClickEvent, &mut Window, &mut App) -> bool + 'static,
    ) -> Self {
        self.on_cancel = Rc::new(on_cancel);
        self
    }

    /// Sets the false to hide close icon, default: true
    pub fn show_close(mut self, show_close: bool) -> Self {
        self.show_close = show_close;
        self
    }

    /// Sets the width of the dialog, defaults to 380px.
    pub fn width(mut self, width: Pixels) -> Self {
        self.width = width;
        self
    }

    /// Sets whether clicking the overlay closes the dialog, defaults to `true`.
    pub fn overlay_closable(mut self, overlay_closable: bool) -> Self {
        self.overlay_closable = overlay_closable;
        self
    }

    /// Set whether to support keyboard esc to close the dialog, defaults to `true`.
    pub fn keyboard(mut self, keyboard: bool) -> Self {
        self.keyboard = keyboard;
        self
    }

    pub(crate) fn has_overlay(&self) -> bool {
        self.overlay
    }
}

impl ParentElement for Dialog {
    fn extend(&mut self, elements: impl IntoIterator<Item = AnyElement>) {
        self.content.extend(elements);
    }
}

impl Styled for Dialog {
    fn style(&mut self) -> &mut gpui::StyleRefinement {
        &mut self.style
    }
}

impl RenderOnce for Dialog {
    fn render(self, window: &mut Window, cx: &mut App) -> impl gpui::IntoElement {
        let layer_ix = self.layer_ix;
        let is_topmost = layer_ix + 1 == Root::read(window, cx).active_dialogs.len();
        let has_footer = self.footer.is_some();

        let on_ok = self.on_ok.clone();
        let on_cancel = self.on_cancel.clone();

        let render_ok: RenderButtonFn = Box::new({
            let on_ok = on_ok.clone();
            let ok_variant = self.button_props.ok_variant;
            let ok_text = self.button_props.ok_text.unwrap_or_else(|| "OK".into());

            move |_, _| {
                Button::new("ok")
                    .label(ok_text)
                    .with_variant(ok_variant)
                    .font_semibold()
                    .on_click({
                        let on_ok = on_ok.clone();

                        move |_, window, cx| {
                            if let Some(on_ok) = &on_ok
                                && !on_ok(&ClickEvent::default(), window, cx)
                            {
                                return;
                            }

                            window.close_dialog(cx);
                        }
                    })
                    .into_any_element()
            }
        });

        let render_cancel: RenderButtonFn = Box::new({
            let on_cancel = on_cancel.clone();
            let cancel_variant = self.button_props.cancel_variant;
            let cancel_text = self
                .button_props
                .cancel_text
                .unwrap_or_else(|| "Cancel".into());

            move |_, _| {
                Button::new("cancel")
                    .label(cancel_text)
                    .with_variant(cancel_variant)
                    .on_click({
                        let on_cancel = on_cancel.clone();
                        move |_, window, cx| {
                            if !on_cancel(&ClickEvent::default(), window, cx) {
                                return;
                            }

                            window.close_dialog(cx);
                        }
                    })
                    .into_any_element()
            }
        });

        let mut padding_right = px(16.);
        let mut padding_left = px(16.);

        if let Some(pl) = self.style.padding.left {
            padding_left = pl.to_pixels(self.width.into(), window.rem_size());
        }

        if let Some(pr) = self.style.padding.right {
            padding_right = pr.to_pixels(self.width.into(), window.rem_size());
        }

        let backdrop = DialogBackdrop::new()
            .absolute()
            .inset_0()
            .when(self.overlay_visible, |this| {
                this.bg(scrim(SCRIM_ALPHA_DARK))
            })
            .with_animation("fade-in", DIALOG_IN.animation(), move |this, delta| {
                this.opacity(delta)
            });

        let card = DialogPopup::new()
            .flex()
            .flex_col()
            .bg(cx.theme().surface_dialog)
            .border_1()
            .border_color(cx.theme().border)
            .rounded(px(surface_radius()))
            .when(cx.theme().shadow, |this| this.shadow(surface_shadows()))
            .min_h_24()
            .refine_style(&self.style)
            .relative()
            .w(self.width)
            .when_some(self.max_width, |this, w| this.max_w(w))
            .child(
                div()
                    .px_4()
                    .h_8()
                    .w_full()
                    .flex()
                    .items_center()
                    .justify_center()
                    .relative()
                    .when_some(self.title, |this, title| {
                        this.h_10().child(
                            DialogTitle::new()
                                .font_semibold()
                                .text_center()
                                .child(title),
                        )
                    })
                    .when(self.show_close, |this| {
                        let on_cancel = on_cancel.clone();

                        this.child(
                            div()
                                .absolute()
                                .top_0()
                                .bottom_0()
                                .right_2()
                                .flex()
                                .items_center()
                                .child(
                                    Button::new("close")
                                        .icon(IconName::CloseCircleFill)
                                        .custom(
                                            ButtonCustomVariant::new(window, cx)
                                                .foreground(cx.theme().text_faint)
                                                .color(transparent_black())
                                                .hover(cx.theme().element_hover)
                                                .active(cx.theme().element_active),
                                        )
                                        .on_click(move |_, window, cx| {
                                            on_cancel(&ClickEvent::default(), window, cx);
                                            window.close_dialog(cx);
                                        }),
                                ),
                        )
                    }),
            )
            .child(
                div()
                    .pt_px()
                    .w_full()
                    .h_auto()
                    .flex_1()
                    .overflow_hidden()
                    .child(
                        v_flex()
                            .pr(padding_right)
                            .pl(padding_left)
                            .size_full()
                            .overflow_y_scrollbar()
                            .child(self.content),
                    ),
            )
            .when_none(&self.footer, |this| this.child(div().pt(padding_left)))
            .when_some(self.footer, |this, footer| {
                this.child(
                    h_flex()
                        .gap_1()
                        .pt(padding_left)
                        .pr(padding_right)
                        .pb(padding_left)
                        .pl(padding_right)
                        .justify_end()
                        .children(footer(render_ok, render_cancel, window, cx)),
                )
            })
            .with_animation("slide-down", DIALOG_IN.animation(), move |this, delta| {
                let y_offset = px(2.) * (delta - 1.);
                let mut shadow = surface_shadows();
                for shadow in &mut shadow {
                    shadow.color.a *= delta;
                }
                this.top(y_offset).shadow(shadow)
            });

        gpui_base::Dialog::new(cx)
            .layer(layer_ix, is_topmost)
            .backdrop(backdrop)
            .popup(card)
            .focus_handle(self.focus_handle.clone())
            .close_on_escape(self.keyboard)
            .close_on_backdrop_press(self.overlay_closable)
            .on_ok(move |event, window, cx| match &on_ok {
                Some(on_ok) => on_ok(event, window, cx),
                None => has_footer,
            })
            .on_cancel(move |event, window, cx| on_cancel(event, window, cx))
            .on_close(move |_, window, cx| window.close_dialog(cx))
    }
}
