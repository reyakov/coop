use gpui::prelude::FluentBuilder as _;
use gpui::{
    AnyElement, App, DefiniteLength, Edges, Entity, Hsla, InteractiveElement as _, IntoElement,
    MouseButton, ParentElement as _, Pixels, Rems, RenderOnce, StyleRefinement, Styled, TextAlign,
    Window, div, px, relative,
};
use gpui_base::InputBase;
use gpui_base::input::{InputBaseState, InputEditorStyle, InputMode, InputModeKind, TextareaMode};
use theme::ActiveTheme;

use crate::button::{Button, ButtonVariants as _};
use crate::indicator::Indicator;
use crate::input::clear_button;
use crate::{IconName, Selectable, Sizable, Size, StyleSized, StyledExt, h_flex, v_flex};

/// The background of an input frame, which reads muted while the input is disabled.
fn input_background(disabled: bool, cx: &App) -> Hsla {
    if disabled {
        cx.theme().surface_background
    } else {
        cx.theme().elevated_surface_background
    }
}

/// The colors base paints input text with, read from the coop theme.
///
/// Base fills in any color left transparent from its own palette, and that
/// palette is only a projection of this one, so every color coop paints with is
/// named here rather than left to resolve.
fn input_editor_style(cx: &App) -> InputEditorStyle {
    let theme = cx.theme();
    InputEditorStyle {
        foreground: theme.text,
        muted_foreground: theme.text_muted,
        background: theme.elevated_surface_background,
        border: theme.border,
        selection: theme.selection,
        caret: theme.cursor,
        ..InputEditorStyle::default()
    }
}

/// The input's own padding, resolved to pixels.
///
/// Base applies the multi-line padding itself so that the text, the gutter, and
/// the scrollbar share one inset, and the single-line frame carries its own.
/// Both come from the same size table, resolved through the window's rem size.
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
///
/// The editing kind lives on the state, so `Input::new` accepts either and
/// infers which one is rendered.
#[derive(IntoElement)]
pub struct Input<M: InputModeKind = InputMode> {
    state: Entity<InputBaseState<M>>,
    style: StyleRefinement,
    size: Size,
    prefix: Option<AnyElement>,
    suffix: Option<AnyElement>,
    height: Option<DefiniteLength>,
    appearance: bool,
    cleanable: bool,
    mask_toggle: bool,
    disabled: bool,
    tab_index: isize,
    selected: bool,
}

/// A styled multi-line text input.
pub type Textarea = Input<TextareaMode>;

impl<M: InputModeKind> Sizable for Input<M> {
    fn with_size(mut self, size: impl Into<Size>) -> Self {
        self.size = size.into();
        self
    }
}

impl<M: InputModeKind> Selectable for Input<M> {
    fn selected(mut self, selected: bool) -> Self {
        self.selected = selected;
        self
    }

    fn is_selected(&self) -> bool {
        self.selected
    }
}

impl<M: InputModeKind> Input<M> {
    /// Create a new [`Input`] element bind to the given state.
    pub fn new(state: &Entity<InputBaseState<M>>) -> Self {
        Self {
            state: state.clone(),
            size: Size::default(),
            style: StyleRefinement::default(),
            prefix: None,
            suffix: None,
            height: None,
            appearance: true,
            cleanable: false,
            mask_toggle: false,
            disabled: false,
            tab_index: 0,
            selected: false,
        }
    }

    pub fn prefix(mut self, prefix: impl IntoElement) -> Self {
        self.prefix = Some(prefix.into_any_element());
        self
    }

    pub fn suffix(mut self, suffix: impl IntoElement) -> Self {
        self.suffix = Some(suffix.into_any_element());
        self
    }

    /// Set full height of the input (Multi-line only).
    pub fn h_full(mut self) -> Self {
        self.height = Some(relative(1.));
        self
    }

    /// Set height of the input (Multi-line only).
    pub fn h(mut self, height: impl Into<DefiniteLength>) -> Self {
        self.height = Some(height.into());
        self
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

    /// Set to enable toggle button for password mask state.
    pub fn mask_toggle(mut self) -> Self {
        self.mask_toggle = true;
        self
    }

    /// Set to disable the input field.
    pub fn disabled(mut self, disabled: bool) -> Self {
        self.disabled = disabled;
        self
    }

    /// Set the tab index for the input, default is 0.
    pub fn tab_index(mut self, index: isize) -> Self {
        self.tab_index = index;
        self
    }

    fn render_toggle_mask_button(state: &Entity<InputBaseState<M>>) -> impl IntoElement {
        Button::new("toggle-mask")
            .icon(IconName::Eye)
            .xsmall()
            .ghost()
            .tab_stop(false)
            .on_click({
                let state = state.clone();
                move |_, window, cx| state.update(cx, |state, cx| state.toggle_masked(window, cx))
            })
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
        let has_suffix = self.suffix.is_some() || loading || self.mask_toggle || show_clear_button;

        let prefix = self.prefix;
        let suffix = self.suffix;
        let state_entity = self.state.clone();

        InputBase::new(("input", self.state.entity_id()))
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
            })
            .items_center()
            .when(multi_line, |this| {
                this.h_auto()
                    .when_some(self.height, |this, height| this.h(height))
            })
            .when(self.appearance, |this| {
                this.bg(background)
                    .when(self.disabled, |this| this.opacity(0.5))
                    .rounded(cx.theme().radius)
            })
            .tab_index(self.tab_index)
            .gap(gap_x)
            .refine_style(&self.style)
            .children(prefix)
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
                        .when(self.mask_toggle, |this| {
                            this.child(Self::render_toggle_mask_button(&state_entity))
                        })
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
                        })
                        .children(suffix),
                )
            })
    }
}
