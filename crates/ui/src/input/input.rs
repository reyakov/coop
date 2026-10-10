use gpui::prelude::FluentBuilder as _;
use gpui::{
    App, DefiniteLength, Edges, Entity, Hsla, InteractiveElement as _, IntoElement, MouseButton,
    ParentElement as _, Pixels, Rems, RenderOnce, StyleRefinement, Styled, TextAlign, Window, div,
    px,
};
use gpui_base::InputBase;
use gpui_base::input::{InputBaseState, InputEditorStyle, InputMode, InputModeKind, TextareaMode};
use theme::{ActiveTheme, button_radius};

use crate::indicator::Indicator;
use crate::input::clear_button;
use crate::{Sizable, Size, StyleSized, StyledExt, h_flex, v_flex};

#[cfg(any(target_os = "linux", target_os = "freebsd"))]
fn nearest_caret_offset(
    value: &str,
    position: gpui::Point<Pixels>,
    mut caret_bounds: impl FnMut(usize) -> Option<gpui::Bounds<Pixels>>,
) -> Option<usize> {
    value
        .char_indices()
        .map(|(offset, _)| offset)
        .chain(std::iter::once(value.len()))
        .filter_map(|offset| {
            let bounds = caret_bounds(offset)?;
            let vertical_distance = if position.y < bounds.top() {
                bounds.top() - position.y
            } else if position.y > bounds.bottom() {
                position.y - bounds.bottom()
            } else {
                px(0.)
            };
            Some((
                offset,
                vertical_distance,
                (position.x - bounds.left()).abs(),
            ))
        })
        .min_by(|left, right| left.1.cmp(&right.1).then_with(|| left.2.cmp(&right.2)))
        .map(|(offset, _, _)| offset)
}

#[cfg(any(target_os = "linux", target_os = "freebsd"))]
fn paste_from_primary<M: InputModeKind>(
    state: &Entity<InputBaseState<M>>,
    position: gpui::Point<Pixels>,
    window: &mut Window,
    cx: &mut App,
) {
    if !state.read(cx).is_editable() {
        return;
    }
    // Read before changing focus or selection: those can change PRIMARY.
    let Some(text) = cx.read_from_primary().and_then(|item| item.text()) else {
        return;
    };
    state.update(cx, |state, cx| {
        let Some(offset) = nearest_caret_offset(&state.value(), position, |offset| {
            state.range_to_bounds(&(offset..offset))
        }) else {
            return;
        };
        let text = if state.is_multi_line() {
            text
        } else {
            text.replace(['\r', '\n'], "")
        };
        state.focus(window, cx);
        state.set_selected_range(offset..offset, cx);
        state.insert(text, window, cx);
    });
    cx.stop_propagation();
}

/// The background of an input frame, which reads muted while the input is disabled.
fn input_background(disabled: bool, cx: &App) -> Hsla {
    if disabled {
        cx.theme().surface
    } else {
        cx.theme().input_bg
    }
}

/// The colors base paints input text with, read from the coop theme.
fn input_editor_style(cx: &App) -> InputEditorStyle {
    let theme = cx.theme();
    InputEditorStyle {
        foreground: theme.text,
        muted_foreground: theme.text_faint,
        background: theme.input_bg,
        border: theme.border_faint,
        selection: theme.selection,
        caret: theme.caret,
        ..InputEditorStyle::default()
    }
}

/// The input's own padding, resolved to pixels.
fn input_paddings(size: Size, style: &StyleRefinement, window: &Window) -> Edges<Pixels> {
    let mut probe = div().input_px(size).input_py(size).refine_style(style);
    let padding = probe.style().padding.clone();
    let base_size = window.text_style().font_size;
    let rem_size = window.rem_size();
    let resolve = |value: Option<DefiniteLength>| {
        value
            .map(|value| value.to_pixels(base_size, rem_size))
            .unwrap_or(px(0.))
    };

    Edges {
        left: resolve(padding.left),
        right: resolve(padding.right),
        top: resolve(padding.top),
        bottom: resolve(padding.bottom),
    }
}

/// A text input element bound to an [`InputState`] or a [`TextareaState`].
#[derive(IntoElement)]
pub struct Input<M: InputModeKind = InputMode> {
    state: Entity<InputBaseState<M>>,
    style: StyleRefinement,
    size: Size,
    appearance: bool,
    cleanable: bool,
    disabled: bool,
}

/// A styled multi-line text input.
pub type Textarea = Input<TextareaMode>;

impl<M: InputModeKind> Sizable for Input<M> {
    fn with_size(mut self, size: impl Into<Size>) -> Self {
        self.size = size.into();
        self
    }
}

impl<M: InputModeKind> Input<M> {
    /// Create a new [`Input`] element bind to the given state.
    pub fn new(state: &Entity<InputBaseState<M>>) -> Self {
        Self {
            state: state.clone(),
            size: Size::default(),
            style: StyleRefinement::default(),
            appearance: true,
            cleanable: false,
            disabled: false,
        }
    }

    /// Set the appearance of the input field, if false the input field will no border, background.
    pub fn appearance(mut self, appearance: bool) -> Self {
        self.appearance = appearance;
        self
    }

    /// Set whether to show the clear button when the input field is not empty, default is false.
    pub fn cleanable(mut self, cleanable: bool) -> Self {
        self.cleanable = cleanable;
        self
    }

    /// Set to disable the input field.
    pub fn disabled(mut self, disabled: bool) -> Self {
        self.disabled = disabled;
        self
    }
}

impl<M: InputModeKind> Styled for Input<M> {
    fn style(&mut self) -> &mut StyleRefinement {
        &mut self.style
    }
}

impl<M: InputModeKind> RenderOnce for Input<M> {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        const LINE_HEIGHT: Rems = Rems(1.25);
        let text_align = self.style.text.text_align.unwrap_or(TextAlign::Left);

        let multi_line = self.state.read(cx).is_multi_line();
        let editor_paddings = if multi_line {
            input_paddings(self.size, &self.style, window)
        } else {
            Edges::default()
        };
        self.state.update(cx, |state, cx| {
            state.set_editor_style(input_editor_style(cx));
            state.set_editor_paddings(editor_paddings);
            state.set_disabled(self.disabled, cx);
            if state.is_single_line() {
                state.set_text_align(text_align, cx);
            }
        });

        let state = self.state.read(cx);
        let presentation = state.presentation();
        let disabled = presentation.is_disabled();
        let loading = presentation.is_loading();
        let text_is_empty = state.text().len() == 0;

        let gap_x = match self.size {
            Size::Small => px(4.),
            Size::Large => px(8.),
            _ => px(6.),
        };

        let background = input_background(disabled, cx);
        let show_clear_button =
            self.cleanable && state.is_editable() && !loading && !text_is_empty && !multi_line;
        let has_suffix = loading || show_clear_button;

        let state_entity = self.state.clone();

        let input = InputBase::new(("input", self.state.entity_id()))
            .flex()
            .size_full()
            .line_height(LINE_HEIGHT)
            .when(!multi_line, |this| {
                this.input_px(self.size).input_py(self.size)
            })
            .input_h(self.size)
            .when(!disabled, |this| this.cursor_text())
            .on_mouse_down(MouseButton::Left, {
                let state_entity = state_entity.clone();
                move |_, window, cx| state_entity.update(cx, |state, cx| state.focus(window, cx))
            });

        #[cfg(any(target_os = "linux", target_os = "freebsd"))]
        let input = input.on_mouse_down(MouseButton::Middle, {
            let state_entity = state_entity.clone();
            move |event, window, cx| paste_from_primary(&state_entity, event.position, window, cx)
        });

        input
            .items_center()
            .when(multi_line, |this| this.h_auto())
            .when(self.appearance, |this| {
                this.bg(background)
                    .when(self.disabled, |this| this.opacity(0.5))
                    .rounded(px(button_radius()))
            })
            .tab_index(0)
            .gap(gap_x)
            .refine_style(&self.style)
            .when(!multi_line, |this| this.child(state_entity.clone()))
            .when(multi_line, |this| {
                this.child(
                    v_flex()
                        .size_full()
                        .child(div().relative().flex_1().child(state_entity.clone())),
                )
            })
            .when(has_suffix, |this| {
                this.pr_2().child(
                    h_flex()
                        .id("suffix")
                        .gap(gap_x)
                        .items_center()
                        .when(loading, |this| this.child(Indicator::new()))
                        .when(show_clear_button, |this| {
                            this.child(clear_button(cx).on_click({
                                let state = state_entity.clone();
                                move |_, window, cx| {
                                    state.update(cx, |state, cx| {
                                        state.clean(window, cx);
                                        state.focus(window, cx);
                                    })
                                }
                            }))
                        }),
                )
            })
    }
}
