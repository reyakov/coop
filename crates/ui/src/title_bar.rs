use std::rc::Rc;

use gpui::prelude::FluentBuilder as _;
use gpui::{
    App, ClickEvent, Context, Div, Hsla, InteractiveElement as _, IntoElement, MouseButton,
    ParentElement as _, Pixels, Render, RenderOnce, Stateful, StatefulInteractiveElement as _,
    Styled, Window, WindowControlArea, div, px,
};
use theme::ActiveTheme;

use crate::{Icon, IconName, Sizable as _, h_flex};

pub const TITLE_BAR_HEIGHT: Pixels = px(34.);
pub const TRAFFIC_LIGHT_PADDING: f32 = 80.;

// The Windows control buttons have a fixed width of 35px.
//
// We don't need implementation the click event for the control buttons.
// If user clicked in the bounds, the window event will be triggered.
#[derive(IntoElement, Clone)]
#[allow(clippy::type_complexity)]
enum ControlIcon {
    Minimize,
    Restore,
    Maximize,
    Close {
        on_close_window: Option<Rc<Box<dyn Fn(&ClickEvent, &mut Window, &mut App)>>>,
    },
}

impl ControlIcon {
    fn minimize() -> Self {
        Self::Minimize
    }

    fn restore() -> Self {
        Self::Restore
    }

    fn maximize() -> Self {
        Self::Maximize
    }

    #[allow(clippy::type_complexity)]
    fn close(on_close_window: Option<Rc<Box<dyn Fn(&ClickEvent, &mut Window, &mut App)>>>) -> Self {
        Self::Close { on_close_window }
    }

    fn id(&self) -> &'static str {
        match self {
            Self::Minimize => "minimize",
            Self::Restore => "restore",
            Self::Maximize => "maximize",
            Self::Close { .. } => "close",
        }
    }

    fn icon(&self) -> IconName {
        match self {
            Self::Minimize => IconName::WindowMinimize,
            Self::Restore => IconName::WindowRestore,
            Self::Maximize => IconName::WindowMaximize,
            Self::Close { .. } => IconName::WindowClose,
        }
    }

    fn window_control_area(&self) -> WindowControlArea {
        match self {
            Self::Minimize => WindowControlArea::Min,
            Self::Restore | Self::Maximize => WindowControlArea::Max,
            Self::Close { .. } => WindowControlArea::Close,
        }
    }

    fn is_close(&self) -> bool {
        matches!(self, Self::Close { .. })
    }

    #[inline]
    fn hover_fg(&self, cx: &App) -> Hsla {
        if self.is_close() {
            cx.theme().danger_foreground
        } else {
            cx.theme().text
        }
    }

    #[inline]
    fn hover_bg(&self, cx: &App) -> Hsla {
        if self.is_close() {
            cx.theme().danger_background
        } else {
            cx.theme().ghost_element_hover
        }
    }

    #[inline]
    fn active_bg(&self, cx: &mut App) -> Hsla {
        if self.is_close() {
            cx.theme().danger_active
        } else {
            cx.theme().ghost_element_active
        }
    }
}

impl RenderOnce for ControlIcon {
    fn render(self, _: &mut Window, cx: &mut App) -> impl IntoElement {
        let is_linux = cfg!(target_os = "linux");
        let is_windows = cfg!(target_os = "windows");

        let icon = self.clone();
        let hover_fg = self.hover_fg(cx);
        let hover_bg = self.hover_bg(cx);
        let active_bg = self.active_bg(cx);

        let on_close_window = match &self {
            ControlIcon::Close { on_close_window } => on_close_window.clone(),
            _ => None,
        };

        div()
            .id(self.id())
            .flex()
            .w(TITLE_BAR_HEIGHT)
            .h_full()
            .flex_shrink_0()
            .justify_center()
            .content_center()
            .items_center()
            .text_color(cx.theme().text)
            .hover(|style| style.bg(hover_bg).text_color(hover_fg))
            .active(|style| style.bg(active_bg).text_color(hover_fg))
            .when(is_windows, |this| {
                this.window_control_area(self.window_control_area())
            })
            .when(is_linux, |this| {
                this.on_mouse_down(MouseButton::Left, move |_, window, cx| {
                    window.prevent_default();
                    cx.stop_propagation();
                })
                .on_click(move |_, window, cx| {
                    cx.stop_propagation();
                    match icon {
                        Self::Minimize => window.minimize_window(),
                        Self::Restore | Self::Maximize => window.zoom_window(),
                        Self::Close { .. } => {
                            if let Some(f) = on_close_window.clone() {
                                f(&ClickEvent::default(), window, cx);
                            } else {
                                window.remove_window();
                            }
                        }
                    }
                })
            })
            .child(Icon::new(self.icon()).small())
    }
}

#[derive(IntoElement)]
#[allow(clippy::type_complexity)]
pub(crate) struct WindowControls {
    on_close_window: Option<Rc<Box<dyn Fn(&ClickEvent, &mut Window, &mut App)>>>,
}

pub(crate) fn window_controls() -> WindowControls {
    WindowControls {
        on_close_window: None,
    }
}

pub fn title_bar_drag_handlers(
    this: Stateful<Div>,
    window: &mut Window,
    cx: &mut App,
) -> Stateful<Div> {
    let state = window.use_state(cx, |_, _| TitleBarState { should_move: false });

    let this = if cfg!(target_family = "wasm") {
        this
    } else {
        this.window_control_area(WindowControlArea::Drag)
    };

    this.on_mouse_down_out(window.listener_for(&state, |state, _, _, _| {
        state.should_move = false;
    }))
    .on_mouse_down(
        MouseButton::Left,
        window.listener_for(&state, |state, _, _, _| {
            state.should_move = true;
        }),
    )
    .on_mouse_up(
        MouseButton::Left,
        window.listener_for(&state, |state, _, _, _| {
            state.should_move = false;
        }),
    )
    .on_mouse_move(window.listener_for(&state, |state, _, window, _| {
        if state.should_move {
            state.should_move = false;
            window.start_window_move();
        }
    }))
    .on_click(|event, window, _| {
        if event.click_count() == 2 {
            if cfg!(target_os = "macos") {
                window.titlebar_double_click();
            } else {
                window.zoom_window();
            }
        }
    })
}

impl RenderOnce for WindowControls {
    fn render(self, window: &mut Window, _: &mut App) -> impl IntoElement {
        if cfg!(target_os = "macos") || cfg!(target_family = "wasm") {
            return div().id("window-controls");
        }

        h_flex()
            .id("window-controls")
            .items_center()
            .flex_shrink_0()
            .h_full()
            .child(ControlIcon::minimize())
            .child(if window.is_maximized() {
                ControlIcon::restore()
            } else {
                ControlIcon::maximize()
            })
            .child(ControlIcon::close(self.on_close_window))
    }
}

struct TitleBarState {
    should_move: bool,
}

impl Render for TitleBarState {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        div()
    }
}
