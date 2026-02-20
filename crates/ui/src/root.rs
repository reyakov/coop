use std::rc::Rc;

use gpui::prelude::FluentBuilder;
use gpui::{
    canvas, div, point, px, size, AnyView, App, AppContext, Bounds, Context, CursorStyle,
    Decorations, Edges, Entity, FocusHandle, HitboxBehavior, Hsla, InteractiveElement, IntoElement,
    MouseButton, ParentElement as _, Pixels, Point, Render, ResizeEdge, SharedString, Size, Styled,
    Tiling, WeakFocusHandle, Window,
};
use theme::{
    ActiveTheme, CLIENT_SIDE_DECORATION_BORDER, CLIENT_SIDE_DECORATION_ROUNDING,
    CLIENT_SIDE_DECORATION_SHADOW,
};

use crate::input::InputState;
use crate::modal::Modal;
use crate::notification::{Notification, NotificationList};

#[derive(Clone)]
#[allow(clippy::type_complexity)]
pub struct ActiveModal {
    focus_handle: FocusHandle,
    /// The previous focused handle before opening the modal.
    previous_focused_handle: Option<WeakFocusHandle>,
    builder: Rc<dyn Fn(Modal, &mut Window, &mut App) -> Modal + 'static>,
}

impl ActiveModal {
    fn new(
        focus_handle: FocusHandle,
        previous_focused_handle: Option<WeakFocusHandle>,
        builder: impl Fn(Modal, &mut Window, &mut App) -> Modal + 'static,
    ) -> Self {
        Self {
            focus_handle,
            previous_focused_handle,
            builder: Rc::new(builder),
        }
    }
}

/// Root is a view for the App window for as the top level view (Must be the first view in the window).
///
/// It is used to manage the Modal, and Notification.
pub struct Root {
    /// All active models
    pub(crate) active_modals: Vec<ActiveModal>,

    /// Notification layer
    pub(crate) notification: Entity<NotificationList>,

    /// Current focused input
    pub(crate) focused_input: Option<Entity<InputState>>,

    /// App view
    view: AnyView,
}

impl Root {
    pub fn new(view: AnyView, window: &mut Window, cx: &mut Context<Self>) -> Self {
        Self {
            focused_input: None,
            active_modals: Vec::new(),
            notification: cx.new(|cx| NotificationList::new(window, cx)),
            view,
        }
    }

    pub fn update<F>(window: &mut Window, cx: &mut App, f: F)
    where
        F: FnOnce(&mut Self, &mut Window, &mut Context<Self>) + 'static,
    {
        if let Some(Some(root)) = window.root::<Root>() {
            root.update(cx, |root, cx| f(root, window, cx));
        }
    }

    pub fn read<'a>(window: &'a mut Window, cx: &'a mut App) -> &'a Self {
        window
            .root::<Root>()
            .expect("The window root view should be of type `ui::Root`.")
            .unwrap()
            .read(cx)
    }

    pub fn view(&self) -> &AnyView {
        &self.view
    }

    /// Render the notification layer.
    pub fn render_notification_layer(
        window: &mut Window,
        cx: &mut App,
    ) -> Option<impl IntoElement> {
        let root = window.root::<Root>()??;

        Some(
            div()
                .absolute()
                .top_0()
                .right_0()
                .child(root.read(cx).notification.clone()),
        )
    }

    /// Render the modal layer.
    pub fn render_modal_layer(window: &mut Window, cx: &mut App) -> Option<impl IntoElement> {
        let root = window.root::<Root>()??;
        let active_modals = root.read(cx).active_modals.clone();

        if active_modals.is_empty() {
            return None;
        }

        let mut show_overlay_ix = None;

        let mut modals = active_modals
            .iter()
            .enumerate()
            .map(|(i, active_modal)| {
                let mut modal = Modal::new(window, cx);

                modal = (active_modal.builder)(modal, window, cx);

                // Give the modal the focus handle, because `modal` is a temporary value, is not possible to
                // keep the focus handle in the modal.
                //
                // So we keep the focus handle in the `active_modal`, this is owned by the `Root`.
                modal.focus_handle = active_modal.focus_handle.clone();

                modal.layer_ix = i;
                // Find the modal which one needs to show overlay.
                if modal.has_overlay() {
                    show_overlay_ix = Some(i);
                }

                modal
            })
            .collect::<Vec<_>>();

        if let Some(ix) = show_overlay_ix {
            if let Some(modal) = modals.get_mut(ix) {
                modal.overlay_visible = true;
            }
        }

        Some(div().children(modals))
    }

    /// Open a modal.
    pub fn open_modal<F>(&mut self, builder: F, window: &mut Window, cx: &mut Context<'_, Self>)
    where
        F: Fn(Modal, &mut Window, &mut App) -> Modal + 'static,
    {
        let previous_focused_handle = window.focused(cx).map(|h| h.downgrade());
        let focus_handle = cx.focus_handle();
        focus_handle.focus(window, cx);

        self.active_modals.push(ActiveModal::new(
            focus_handle,
            previous_focused_handle,
            builder,
        ));

        cx.notify();
    }

    /// Close the topmost modal.
    pub fn close_modal(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.focused_input = None;

        if let Some(handle) = self
            .active_modals
            .pop()
            .and_then(|d| d.previous_focused_handle)
            .and_then(|h| h.upgrade())
        {
            window.focus(&handle, cx);
        }

        cx.notify();
    }

    /// Close all modals.
    pub fn close_all_modals(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.focused_input = None;
        self.active_modals.clear();

        let previous_focused_handle = self
            .active_modals
            .first()
            .and_then(|d| d.previous_focused_handle.clone());

        if let Some(handle) = previous_focused_handle.and_then(|h| h.upgrade()) {
            window.focus(&handle, cx);
        }

        cx.notify();
    }

    /// Check if there are any active modals.
    pub fn has_active_modals(&self) -> bool {
        !self.active_modals.is_empty()
    }

    /// Push a notification to the notification layer.
    pub fn push_notification<T>(&mut self, note: T, window: &mut Window, cx: &mut Context<'_, Root>)
    where
        T: Into<Notification>,
    {
        self.notification
            .update(cx, |view, cx| view.push(note, window, cx));
        cx.notify();
    }

    /// Clear a notification by its ID.
    pub fn clear_notification<T>(&mut self, id: T, window: &mut Window, cx: &mut Context<Self>)
    where
        T: Into<SharedString>,
    {
        self.notification
            .update(cx, |view, cx| view.close(id.into(), window, cx));
        cx.notify();
    }

    /// Clear all notifications from the notification layer.
    pub fn clear_notifications(&mut self, window: &mut Window, cx: &mut Context<'_, Root>) {
        self.notification
            .update(cx, |view, cx| view.clear(window, cx));
        cx.notify();
    }
}

impl Render for Root {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let rem_size = cx.theme().font_size;
        let font_family = cx.theme().font_family.clone();
        let decorations = window.window_decorations();

        // Set the base font size
        window.set_rem_size(rem_size);

        // Set the client inset (linux only)
        match decorations {
            Decorations::Client { .. } => window.set_client_inset(CLIENT_SIDE_DECORATION_SHADOW),
            Decorations::Server => window.set_client_inset(px(0.0)),
        }

        div()
            .id("window")
            .size_full()
            .bg(gpui::transparent_black())
            .map(|div| match decorations {
                Decorations::Server => div,
                Decorations::Client { tiling } => div
                    .bg(gpui::transparent_black())
                    .child(
                        canvas(
                            |_bounds, window, _cx| {
                                window.insert_hitbox(
                                    Bounds::new(
                                        point(px(0.0), px(0.0)),
                                        window.window_bounds().get_bounds().size,
                                    ),
                                    HitboxBehavior::Normal,
                                )
                            },
                            move |_bounds, hitbox, window, _cx| {
                                let mouse = window.mouse_position();
                                let size = window.window_bounds().get_bounds().size;

                                let Some(edge) =
                                    resize_edge(mouse, CLIENT_SIDE_DECORATION_SHADOW, size, tiling)
                                else {
                                    return;
                                };

                                window.set_cursor_style(
                                    match edge {
                                        ResizeEdge::Top | ResizeEdge::Bottom => {
                                            CursorStyle::ResizeUpDown
                                        }
                                        ResizeEdge::Left | ResizeEdge::Right => {
                                            CursorStyle::ResizeLeftRight
                                        }
                                        ResizeEdge::TopLeft | ResizeEdge::BottomRight => {
                                            CursorStyle::ResizeUpLeftDownRight
                                        }
                                        ResizeEdge::TopRight | ResizeEdge::BottomLeft => {
                                            CursorStyle::ResizeUpRightDownLeft
                                        }
                                    },
                                    &hitbox,
                                );
                            },
                        )
                        .size_full()
                        .absolute(),
                    )
                    .when(!(tiling.top || tiling.right), |div| {
                        div.rounded_tr(CLIENT_SIDE_DECORATION_ROUNDING)
                    })
                    .when(!(tiling.top || tiling.left), |div| {
                        div.rounded_tl(CLIENT_SIDE_DECORATION_ROUNDING)
                    })
                    .when(!(tiling.bottom || tiling.right), |div| {
                        div.rounded_br(CLIENT_SIDE_DECORATION_ROUNDING)
                    })
                    .when(!(tiling.bottom || tiling.left), |div| {
                        div.rounded_bl(CLIENT_SIDE_DECORATION_ROUNDING)
                    })
                    .when(!tiling.top, |div| div.pt(CLIENT_SIDE_DECORATION_SHADOW))
                    .when(!tiling.bottom, |div| div.pb(CLIENT_SIDE_DECORATION_SHADOW))
                    .when(!tiling.left, |div| div.pl(CLIENT_SIDE_DECORATION_SHADOW))
                    .when(!tiling.right, |div| div.pr(CLIENT_SIDE_DECORATION_SHADOW))
                    .on_mouse_down(MouseButton::Left, move |e, window, _cx| {
                        let size = window.window_bounds().get_bounds().size;
                        let pos = e.position;

                        if let Some(edge) =
                            resize_edge(pos, CLIENT_SIDE_DECORATION_SHADOW, size, tiling)
                        {
                            window.start_window_resize(edge)
                        };
                    }),
            })
            .child(
                div()
                    .map(|div| match decorations {
                        Decorations::Server => div,
                        Decorations::Client { tiling } => div
                            .border_color(cx.theme().border)
                            .when(!(tiling.top || tiling.right), |div| {
                                div.rounded_tr(CLIENT_SIDE_DECORATION_ROUNDING)
                            })
                            .when(!(tiling.top || tiling.left), |div| {
                                div.rounded_tl(CLIENT_SIDE_DECORATION_ROUNDING)
                            })
                            .when(!(tiling.bottom || tiling.right), |div| {
                                div.rounded_br(CLIENT_SIDE_DECORATION_ROUNDING)
                            })
                            .when(!(tiling.bottom || tiling.left), |div| {
                                div.rounded_bl(CLIENT_SIDE_DECORATION_ROUNDING)
                            })
                            .when(!tiling.top, |div| {
                                div.border_t(CLIENT_SIDE_DECORATION_BORDER)
                            })
                            .when(!tiling.bottom, |div| {
                                div.border_b(CLIENT_SIDE_DECORATION_BORDER)
                            })
                            .when(!tiling.left, |div| {
                                div.border_l(CLIENT_SIDE_DECORATION_BORDER)
                            })
                            .when(!tiling.right, |div| {
                                div.border_r(CLIENT_SIDE_DECORATION_BORDER)
                            })
                            .when(!tiling.is_tiled(), |div| {
                                div.shadow(vec![gpui::BoxShadow {
                                    color: Hsla {
                                        h: 0.,
                                        s: 0.,
                                        l: 0.,
                                        a: 0.4,
                                    },
                                    blur_radius: CLIENT_SIDE_DECORATION_SHADOW / 2.,
                                    spread_radius: px(0.),
                                    offset: point(px(0.0), px(0.0)),
                                }])
                            }),
                    })
                    .on_mouse_move(|_e, _, cx| {
                        cx.stop_propagation();
                    })
                    .size_full()
                    .font_family(font_family)
                    .bg(cx.theme().background)
                    .text_color(cx.theme().text)
                    .child(self.view.clone()),
            )
    }
}

/// Get the window paddings.
pub fn window_paddings(window: &Window, _cx: &App) -> Edges<Pixels> {
    match window.window_decorations() {
        Decorations::Server => Edges::all(px(0.0)),
        Decorations::Client { tiling } => {
            let mut paddings = Edges::all(CLIENT_SIDE_DECORATION_SHADOW);
            if tiling.top {
                paddings.top = px(0.0);
            }
            if tiling.bottom {
                paddings.bottom = px(0.0);
            }
            if tiling.left {
                paddings.left = px(0.0);
            }
            if tiling.right {
                paddings.right = px(0.0);
            }
            paddings
        }
    }
}

/// Get the window resize edge.
fn resize_edge(
    pos: Point<Pixels>,
    shadow_size: Pixels,
    window_size: Size<Pixels>,
    tiling: Tiling,
) -> Option<ResizeEdge> {
    let bounds = Bounds::new(Point::default(), window_size).inset(shadow_size * 1.5);
    if bounds.contains(&pos) {
        return None;
    }

    let corner_size = size(shadow_size * 1.5, shadow_size * 1.5);
    let top_left_bounds = Bounds::new(Point::new(px(0.), px(0.)), corner_size);
    if !tiling.top && top_left_bounds.contains(&pos) {
        return Some(ResizeEdge::TopLeft);
    }

    let top_right_bounds = Bounds::new(
        Point::new(window_size.width - corner_size.width, px(0.)),
        corner_size,
    );
    if !tiling.top && top_right_bounds.contains(&pos) {
        return Some(ResizeEdge::TopRight);
    }

    let bottom_left_bounds = Bounds::new(
        Point::new(px(0.), window_size.height - corner_size.height),
        corner_size,
    );
    if !tiling.bottom && bottom_left_bounds.contains(&pos) {
        return Some(ResizeEdge::BottomLeft);
    }

    let bottom_right_bounds = Bounds::new(
        Point::new(
            window_size.width - corner_size.width,
            window_size.height - corner_size.height,
        ),
        corner_size,
    );
    if !tiling.bottom && bottom_right_bounds.contains(&pos) {
        return Some(ResizeEdge::BottomRight);
    }

    if !tiling.top && pos.y < shadow_size {
        Some(ResizeEdge::Top)
    } else if !tiling.bottom && pos.y > window_size.height - shadow_size {
        Some(ResizeEdge::Bottom)
    } else if !tiling.left && pos.x < shadow_size {
        Some(ResizeEdge::Left)
    } else if !tiling.right && pos.x > window_size.width - shadow_size {
        Some(ResizeEdge::Right)
    } else {
        None
    }
}
