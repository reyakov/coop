use gpui::{
    App, Div, InteractiveElement as _, MouseButton, Stateful, StatefulInteractiveElement as _,
    Window, WindowControlArea,
};

struct DragState {
    should_move: bool,
}

/// Window-drag and double-click handlers for the app's integrated title bar.
pub fn title_bar_drag_handlers(
    this: Stateful<Div>,
    window: &mut Window,
    cx: &mut App,
) -> Stateful<Div> {
    let state = window.use_state(cx, |_, _| DragState { should_move: false });

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
