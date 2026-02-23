use gpui::prelude::FluentBuilder;
#[cfg(target_os = "linux")]
use gpui::MouseButton;
#[cfg(not(target_os = "windows"))]
use gpui::Pixels;
use gpui::{
    px, AnyElement, Context, Decorations, Hsla, InteractiveElement as _, IntoElement,
    ParentElement, Render, StatefulInteractiveElement as _, Styled, Window, WindowControlArea,
};
use smallvec::{smallvec, SmallVec};
use theme::{ActiveTheme, PlatformKind, CLIENT_SIDE_DECORATION_ROUNDING};
use ui::h_flex;

#[cfg(target_os = "linux")]
use crate::platforms::linux::LinuxWindowControls;
use crate::platforms::mac::TRAFFIC_LIGHT_PADDING;
use crate::platforms::windows::WindowsWindowControls;

mod platforms;

/// Titlebar
pub struct TitleBar {
    /// Children elements of the title bar.
    children: SmallVec<[AnyElement; 2]>,

    /// Whether the title bar is currently being moved.
    should_move: bool,
}

impl TitleBar {
    pub fn new() -> Self {
        Self {
            children: smallvec![],
            should_move: false,
        }
    }

    #[cfg(not(target_os = "windows"))]
    pub fn height(&self, window: &mut Window) -> Pixels {
        (1.75 * window.rem_size()).max(px(34.))
    }

    #[cfg(target_os = "windows")]
    pub fn height(&self, _window: &mut Window) -> Pixels {
        px(32.)
    }

    pub fn titlebar_color(&self, window: &mut Window, cx: &mut Context<Self>) -> Hsla {
        if cfg!(any(target_os = "linux", target_os = "freebsd")) {
            if window.is_window_active() && !self.should_move {
                cx.theme().title_bar
            } else {
                cx.theme().title_bar_inactive
            }
        } else {
            cx.theme().title_bar
        }
    }

    pub fn set_children<T>(&mut self, children: T)
    where
        T: IntoIterator<Item = AnyElement>,
    {
        self.children = children.into_iter().collect();
    }
}

impl Default for TitleBar {
    fn default() -> Self {
        Self::new()
    }
}

impl Render for TitleBar {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let height = self.height(window);
        let color = self.titlebar_color(window, cx);
        let children = std::mem::take(&mut self.children);

        #[cfg(target_os = "linux")]
        let supported_controls = window.window_controls();
        let decorations = window.window_decorations();

        h_flex()
            .window_control_area(WindowControlArea::Drag)
            .h(height)
            .w_full()
            .map(|this| {
                if window.is_fullscreen() {
                    this.px_2()
                } else if cx.theme().platform.is_mac() {
                    this.pr_2().pl(px(TRAFFIC_LIGHT_PADDING))
                } else {
                    this.px_2()
                }
            })
            .map(|this| match decorations {
                Decorations::Server => this,
                Decorations::Client { tiling } => this
                    .when(!(tiling.top || tiling.right), |div| {
                        div.rounded_tr(CLIENT_SIDE_DECORATION_ROUNDING)
                    })
                    .when(!(tiling.top || tiling.left), |div| {
                        div.rounded_tl(CLIENT_SIDE_DECORATION_ROUNDING)
                    }),
            })
            .bg(color)
            .border_b_1()
            .border_color(cx.theme().border_variant)
            .content_stretch()
            .child(
                h_flex()
                    .id("title-bar")
                    .justify_between()
                    .w_full()
                    .when(cx.theme().platform.is_mac(), |this| {
                        this.on_click(|event, window, _| {
                            if event.click_count() == 2 {
                                window.titlebar_double_click();
                            }
                        })
                    })
                    .when(cx.theme().platform.is_linux(), |this| {
                        this.on_click(|event, window, _| {
                            if event.click_count() == 2 {
                                window.zoom_window();
                            }
                        })
                    })
                    .when(!cx.theme().platform.is_mac(), |this| this.pr_2())
                    .children(children),
            )
            .when(!window.is_fullscreen(), |this| match cx.theme().platform {
                PlatformKind::Linux => {
                    #[cfg(target_os = "linux")]
                    if matches!(decorations, Decorations::Client { .. }) {
                        this.child(LinuxWindowControls::new(None))
                            .when(supported_controls.window_menu, |this| {
                                this.on_mouse_down(MouseButton::Right, move |ev, window, _| {
                                    window.show_window_menu(ev.position)
                                })
                            })
                            .on_mouse_move(cx.listener(move |this, _ev, window, _| {
                                if this.should_move {
                                    this.should_move = false;
                                    window.start_window_move();
                                }
                            }))
                            .on_mouse_down_out(cx.listener(move |this, _ev, _window, _cx| {
                                this.should_move = false;
                            }))
                            .on_mouse_up(
                                MouseButton::Left,
                                cx.listener(move |this, _ev, _window, _cx| {
                                    this.should_move = false;
                                }),
                            )
                            .on_mouse_down(
                                MouseButton::Left,
                                cx.listener(move |this, _ev, _window, _cx| {
                                    this.should_move = true;
                                }),
                            )
                    } else {
                        this
                    }
                    #[cfg(not(target_os = "linux"))]
                    this
                }
                PlatformKind::Windows => this.child(WindowsWindowControls::new(height)),
                PlatformKind::Mac => this,
            })
    }
}
