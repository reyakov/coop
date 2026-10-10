//! Light/dark switching, adapted from bezel
//! (`crates/theme/src/appearance.rs`, MIT License, Copyright (c) 2026 bezel
//! contributors): what the user asked for, what the OS reports, and the
//! plumbing that turns a change in either into a repaint.

use gpui::{App, Global, Subscription, Window};
use serde::{Deserialize, Serialize};

use crate::{Theme, ThemeMode};

/// The user's appearance preference. Serde-serializable so callers can persist
/// it wherever their settings live; this crate never touches disk.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum AppearanceMode {
    /// Follow the OS — matches every other native app on the machine,
    /// including when the OS is set to switch at sunset.
    System,
    Light,
    /// Paint dark regardless of the OS.
    #[default]
    Dark,
}

impl AppearanceMode {
    pub const ALL: [Self; 3] = [Self::System, Self::Light, Self::Dark];

    /// Menu/label text.
    pub fn label(self) -> &'static str {
        match self {
            Self::System => "System",
            Self::Light => "Light",
            Self::Dark => "Dark",
        }
    }
}

/// Global state behind the current theme: what the user chose, and what the OS
/// last said. Kept separate from [`Theme`] itself so that flipping the OS
/// appearance while the user has pinned Light still records the new system
/// value (and takes effect the moment they switch back to `System`).
pub struct AppearanceState {
    pub mode: AppearanceMode,
    pub system: ThemeMode,
}

impl Global for AppearanceState {}

/// Combine the user's choice with the OS state.
pub fn resolve(mode: AppearanceMode, system: ThemeMode) -> ThemeMode {
    match mode {
        AppearanceMode::System => system,
        AppearanceMode::Light => ThemeMode::Light,
        AppearanceMode::Dark => ThemeMode::Dark,
    }
}

/// Install the appearance globals and the matching theme. Call once at boot,
/// before any window opens, so the first frame is already the right palette
/// (installing later produces a visible dark-to-light flash).
pub fn init(mode: AppearanceMode, cx: &mut App) {
    let system = ThemeMode::from_window(cx.window_appearance());
    cx.set_global(AppearanceState { mode, system });
    Theme::change(resolve(mode, system), None, cx);
}

/// The mode currently in effect (defaults to `Dark` before [`init`]).
pub fn mode(cx: &App) -> AppearanceMode {
    cx.try_global::<AppearanceState>()
        .map(|state| state.mode)
        .unwrap_or_default()
}

/// Change the user's preference and repaint if that changed the palette.
/// Persisting the choice is the caller's job.
pub fn set_mode(mode: AppearanceMode, cx: &mut App) {
    if !cx.has_global::<AppearanceState>() {
        return;
    }
    let state = cx.global_mut::<AppearanceState>();
    if state.mode == mode {
        return;
    }
    state.mode = mode;
    // Coming back to `System`, re-read the OS: a pinned mode holds an
    // appearance override, so `system` may be stale from before the pin.
    if mode == AppearanceMode::System {
        let system = ThemeMode::from_window(cx.window_appearance());
        cx.global_mut::<AppearanceState>().system = system;
    }
    apply(cx);
}

/// Subscribe a window to OS appearance changes. The returned [`Subscription`]
/// must outlive the window — callers typically `.detach()` it.
///
/// The notification is *per window*, but the appearance it reports is a system
/// setting, so any one window is enough to learn about the change; re-applying
/// is idempotent when several fire.
pub fn observe_window(window: &mut Window, cx: &mut App) -> Subscription {
    // Reconcile against the *window's* appearance before subscribing: `init`
    // runs before any window exists and can only ask the app, which on some
    // platforms is not reliably populated that early in launch.
    record_system(window.appearance().into(), cx);
    window.observe_window_appearance(move |window, cx| {
        record_system(window.appearance().into(), cx);
    })
}

/// Record the OS appearance and re-apply if it moved. `window`, when given,
/// is the more authoritative source for what the OS currently reports.
pub fn record_system_appearance(window: Option<&Window>, cx: &mut App) {
    let appearance = match window {
        Some(window) => window.appearance(),
        None => cx.window_appearance(),
    };
    record_system(ThemeMode::from_window(appearance), cx);
}

/// Record the OS appearance and re-apply if it moved. Only recorded while the
/// user's mode is `System` — a pinned mode ignores the OS anyway, and
/// recording an override read back would overwrite what the OS said with what
/// we asked for.
fn record_system(system: ThemeMode, cx: &mut App) {
    if !cx.has_global::<AppearanceState>() {
        return;
    }
    let state = cx.global_mut::<AppearanceState>();
    if state.mode != AppearanceMode::System || state.system == system {
        return;
    }
    state.system = system;
    apply(cx);
}

/// Re-resolve the palette and, if it moved, swap the theme and force a full
/// repaint. A no-op when the resolved appearance is unchanged — the OS fires
/// the appearance notification for other changes too, and repainting every
/// window for those would be a visible hitch for nothing.
pub fn apply(cx: &mut App) {
    let Some(state) = cx.try_global::<AppearanceState>() else {
        return;
    };
    let wanted = resolve(state.mode, state.system);
    let changed = !cx
        .try_global::<Theme>()
        .is_some_and(|theme| theme.mode == wanted);
    if changed {
        Theme::change(wanted, None, cx);
        cx.refresh_windows();
    }
}
