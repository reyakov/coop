use std::collections::HashMap;
use std::sync::OnceLock;

use gpui::prelude::FluentBuilder;
use gpui::{
    svg, Action, App, InteractiveElement, IntoElement, MouseButton, ParentElement, RenderOnce,
    SharedString, StatefulInteractiveElement, Styled, Window,
};
use linicon::{lookup_icon, IconType};
use theme::ActiveTheme;
use ui::{h_flex, Icon, IconName, Sizable};

#[derive(IntoElement)]
pub struct LinuxWindowControls {
    close_window_action: Option<Box<dyn Action>>,
}

impl LinuxWindowControls {
    pub fn new(close_window_action: Option<Box<dyn Action>>) -> Self {
        Self {
            close_window_action,
        }
    }
}

impl RenderOnce for LinuxWindowControls {
    fn render(self, window: &mut Window, _cx: &mut App) -> impl IntoElement {
        let supported_controls = window.window_controls();

        h_flex()
            .id("linux-window-controls")
            .gap_2()
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .when(supported_controls.minimize, |this| {
                this.child(WindowControl::new(
                    LinuxControl::Minimize,
                    IconName::WindowMinimize,
                ))
            })
            .when(supported_controls.maximize, |this| {
                this.child({
                    if window.is_maximized() {
                        WindowControl::new(LinuxControl::Restore, IconName::WindowRestore)
                    } else {
                        WindowControl::new(LinuxControl::Maximize, IconName::WindowMaximize)
                    }
                })
            })
            .child(
                WindowControl::new(LinuxControl::Close, IconName::WindowClose)
                    .when_some(self.close_window_action, |this, close_action| {
                        this.close_action(close_action)
                    }),
            )
    }
}

#[derive(IntoElement)]
pub struct WindowControl {
    kind: LinuxControl,
    fallback: IconName,
    close_action: Option<Box<dyn Action>>,
}

impl WindowControl {
    pub fn new(kind: LinuxControl, fallback: IconName) -> Self {
        Self {
            kind,
            fallback,
            close_action: None,
        }
    }

    pub fn close_action(mut self, action: Box<dyn Action>) -> Self {
        self.close_action = Some(action);
        self
    }

    pub fn is_gnome(&self) -> bool {
        matches!(detect_desktop_environment(), DesktopEnvironment::Gnome)
    }
}

impl RenderOnce for WindowControl {
    fn render(self, _window: &mut Window, cx: &mut App) -> impl IntoElement {
        let is_gnome = self.is_gnome();

        h_flex()
            .id(self.kind.as_icon_name())
            .group("")
            .justify_center()
            .items_center()
            .rounded_full()
            .size_6()
            .when(is_gnome, |this| {
                this.bg(cx.theme().ghost_element_background_alt)
                    .hover(|this| this.bg(cx.theme().ghost_element_hover))
                    .active(|this| this.bg(cx.theme().ghost_element_active))
            })
            .map(|this| {
                if let Some(Some(path)) = linux_controls().get(&self.kind).cloned() {
                    this.child(
                        svg()
                            .external_path(SharedString::from(path))
                            .size_4()
                            .text_color(cx.theme().text),
                    )
                } else {
                    this.child(Icon::new(self.fallback).small().text_color(cx.theme().text))
                }
            })
            .on_mouse_move(|_, _window, cx| cx.stop_propagation())
            .on_click(move |_, window, cx| {
                cx.stop_propagation();
                match self.kind {
                    LinuxControl::Minimize => window.minimize_window(),
                    LinuxControl::Restore => window.zoom_window(),
                    LinuxControl::Maximize => window.zoom_window(),
                    LinuxControl::Close => cx.quit(),
                }
            })
    }
}

static DE: OnceLock<DesktopEnvironment> = OnceLock::new();
static LINUX_CONTROLS: OnceLock<HashMap<LinuxControl, Option<String>>> = OnceLock::new();

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum DesktopEnvironment {
    Gnome,
    Kde,
    Unknown,
}

/// Detect the current desktop environment
pub fn detect_desktop_environment() -> &'static DesktopEnvironment {
    DE.get_or_init(|| {
        // Try to use environment variables first
        if let Ok(output) = std::env::var("XDG_CURRENT_DESKTOP") {
            let desktop = output.to_lowercase();
            if desktop.contains("gnome") {
                return DesktopEnvironment::Gnome;
            } else if desktop.contains("kde") {
                return DesktopEnvironment::Kde;
            }
        }

        // Fallback detection methods
        if let Ok(output) = std::env::var("DESKTOP_SESSION") {
            let session = output.to_lowercase();
            if session.contains("gnome") {
                return DesktopEnvironment::Gnome;
            } else if session.contains("kde") || session.contains("plasma") {
                return DesktopEnvironment::Kde;
            }
        }

        DesktopEnvironment::Unknown
    })
}

#[derive(Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Clone, Copy)]
pub enum LinuxControl {
    Minimize,
    Restore,
    Maximize,
    Close,
}

impl LinuxControl {
    pub fn as_icon_name(&self) -> &'static str {
        match self {
            LinuxControl::Close => "window-close",
            LinuxControl::Minimize => "window-minimize",
            LinuxControl::Maximize => "window-maximize",
            LinuxControl::Restore => "window-restore",
        }
    }
}

fn linux_controls() -> &'static HashMap<LinuxControl, Option<String>> {
    LINUX_CONTROLS.get_or_init(|| {
        let mut icons = HashMap::new();
        icons.insert(LinuxControl::Close, None);
        icons.insert(LinuxControl::Minimize, None);
        icons.insert(LinuxControl::Maximize, None);
        icons.insert(LinuxControl::Restore, None);

        let icon_names = [
            (LinuxControl::Close, vec!["window-close", "dialog-close"]),
            (
                LinuxControl::Minimize,
                vec!["window-minimize", "window-lower"],
            ),
            (
                LinuxControl::Maximize,
                vec!["window-maximize", "window-expand"],
            ),
            (
                LinuxControl::Restore,
                vec!["window-restore", "window-return"],
            ),
        ];

        for (control, icon_names) in icon_names {
            for icon_name in icon_names {
                // Try GNOME-style naming first
                let mut control_icon = lookup_icon(format!("{icon_name}-symbolic"))
                    .find(|icon| matches!(icon, Ok(icon) if icon.icon_type == IconType::SVG));

                // If not found, try KDE-style naming
                if control_icon.is_none() {
                    control_icon = lookup_icon(icon_name)
                        .find(|icon| matches!(icon, Ok(icon) if icon.icon_type == IconType::SVG));
                }

                if let Some(Ok(icon)) = control_icon {
                    icons
                        .entry(control)
                        .and_modify(|v| *v = Some(icon.path.to_string_lossy().to_string()));
                }
            }
        }

        icons
    })
}
