//! Layout constants, ported from bezel
//! (`crates/theme/src/theme/layout.rs`, MIT License, Copyright (c) 2026 bezel
//! contributors). Numbers drive layout, colors are paint: these live as plain
//! numbers and never depend on which color is painted.

use std::sync::atomic::{AtomicU32, Ordering};

/// The branded base radius behind every radius accessor, as raw `f32` bits.
static BASE: AtomicU32 = AtomicU32::new(BASE_RADIUS.to_bits());

/// Point the radius accessors at a base. Called when a palette is installed.
pub(crate) fn set_base_radius(radius: f32) {
    BASE.store(radius.to_bits(), Ordering::Relaxed);
}

/// The gap between siblings. Measured on macOS 26: `NSStackView().spacing`,
/// visual format's `-`, and `constraint(equalToSystemSpacingAfter:multiplier: 1)`
/// all report 8.
pub const SPACE: f32 = 8.0;

/// The margin from content to its container's edge. Same measurement,
/// visual format's `|-`.
pub const CONTENT_MARGIN: f32 = 20.0;

/// From a column's edge to the glyph of the control at the end of a row,
/// measured to the glyph rather than the button. Mirrored at the trailing end.
pub const EDGE: f32 = 12.0;

/// Button, text field and select-trigger height. Measured:
/// `NSButton`, `NSTextField` and `NSPopUpButton` all report 24 at
/// `.regular` — the body line box with 4 above and below.
pub const BUTTON_HEIGHT: f32 = 24.0;

/// The same controls at `.small`.
pub const CONTROL_HEIGHT_SMALL: f32 = 20.0;

/// Every radius below is a ratio of this, so a brand radius moves the whole
/// set together and keeps the concentric relationships intact.
pub const BASE_RADIUS: f32 = 8.0;

/// Message bubble corner radius (2x).
pub fn bubble_radius() -> f32 {
    radius(2.0)
}

/// Floating-surface corner radius — popovers, menus, dialogs (1.5x).
pub fn surface_radius() -> f32 {
    radius(1.5)
}

/// Panel / card corner radius (1.25x).
pub fn panel_radius() -> f32 {
    radius(1.25)
}

/// Button, text field and select-trigger radius (1x).
pub fn button_radius() -> f32 {
    radius(1.0)
}

/// Small control radius (chips, tags, steppers) — a size down from
/// [`button_radius`], for things that sit inside a control rather than being one
/// (0.75x).
pub fn control_radius() -> f32 {
    radius(0.75)
}

/// A corner as a multiple of the branded base radius.
///
/// Read from a process-wide mirror rather than the theme global for the reason
/// [`crate::paint::current_appearance`] is: the element builders that round a
/// corner are free functions with no `cx` in scope, and a radius is one number
/// for the whole app.
fn radius(ratio: f32) -> f32 {
    f32::from_bits(BASE.load(Ordering::Relaxed)) * ratio
}

/// The concentric child of a surface: a row inset by `inset` inside a
/// container of radius `outer` keeps its corners parallel to the container's,
/// rather than looking pasted onto it.
///
/// This is SwiftUI's `ContainerRelativeShape` rule done as arithmetic. gpui
/// has no container shape to inherit at paint time, so the relationship is
/// stated where the child is *defined* instead of resolved at runtime.
pub const fn inset_radius(outer: f32, inset: f32) -> f32 {
    if outer > inset { outer - inset } else { 0.0 }
}
