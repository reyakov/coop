//! The motion kit, ported from bezel (`crates/motion/src/lib.rs` and its
//! `app.rs`, MIT License, Copyright (c) 2026 bezel contributors). TranslateY
//! rides a relative `top` inset, which taffy applies after layout, so
//! siblings never move.

use std::cell::RefCell;
use std::collections::HashMap;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::Duration;

use gpui::{
    Animation, AnimationElement, AnimationExt as _, App, Context, ElementId, EntityId, Global,
    Hsla, IntoElement, Rgba, SharedString, Styled, Window, px,
};
use instant::Instant;

/// Redraw rate for anything driven by [`pulse_delta`].
const PULSE_FPS: f32 = 30.0;

/// How long a view stays on the tick list after its last pulse paint.
const PULSE_LEASE: Duration = Duration::from_millis(300);

/// Redraw rate for hover fades; [`HOVER_FADE`] paints in nine steps at it.
const HOVER_FPS: f32 = 60.0;

/// Floor on how long the clock sleeps between wake-ups.
const MIN_SLEEP: Duration = Duration::from_millis(1);

/// One view's claim on the clock.
struct Lease {
    /// The fastest rate anything on this view has claimed.
    period: Duration,
    /// When this view is next owed a redraw.
    due: Instant,
    /// A notify is out and the render it provoked has not renewed yet.
    in_flight: bool,
    /// When the claim lapses if nothing renews it.
    until: Instant,
}

#[derive(Default)]
struct PulseClock {
    epoch: Option<Instant>,
    leases: HashMap<EntityId, Lease>,
    running: bool,
}

impl Global for PulseClock {}

impl PulseClock {
    fn next_wake(&self) -> Option<Instant> {
        self.leases
            .values()
            .map(|lease| lease.due.min(lease.until))
            .min()
    }
}

/// A component's line back to the view that paints it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Painter(EntityId);

impl Painter {
    /// The view this render belongs to; take it once where the state is built.
    pub fn of<T: 'static>(cx: &Context<T>) -> Self {
        Self(cx.entity_id())
    }

    /// Redraw this view once.
    pub fn notify(self, cx: &mut App) {
        cx.notify(self.0);
    }

    /// Claim `fps` redraws a second; renew from `render` so an unmounted
    /// element drops off.
    pub fn lease(self, fps: f32, until: Duration, cx: &mut App) {
        lease(self.0, fps, until, cx);
    }

    /// Whether the clock is what asked for the render running now.
    pub fn woken(self, cx: &App) -> bool {
        cx.try_global::<PulseClock>()
            .and_then(|clock| clock.leases.get(&self.0))
            .is_some_and(|lease| lease.in_flight)
    }
}

impl From<EntityId> for Painter {
    fn from(entity_id: EntityId) -> Self {
        Self(entity_id)
    }
}

impl From<Painter> for EntityId {
    fn from(painter: Painter) -> Self {
        painter.0
    }
}

fn lease(view: EntityId, fps: f32, until: Duration, cx: &mut App) {
    if cx.pause_when_inactive() && cx.active_window().is_none() {
        return;
    }
    let now = cx.background_executor().now();
    let period = Duration::from_secs_f32(1.0 / fps.clamp(1.0, 240.0));
    let clock = cx.default_global::<PulseClock>();
    clock
        .leases
        .entry(view)
        .and_modify(|lease| {
            lease.period = lease.period.min(period);
            lease.due = lease.due.min(now + period);
            lease.in_flight = false;
            lease.until = lease.until.max(now + until);
        })
        .or_insert(Lease {
            period,
            due: now + period,
            in_flight: false,
            until: now + until,
        });
    if clock.running {
        return;
    }
    clock.running = true;
    cx.spawn(async move |cx| {
        loop {
            let sleep = cx.update(|cx| {
                let now = cx.background_executor().now();
                let clock = cx.default_global::<PulseClock>();
                clock
                    .next_wake()
                    .map(|wake| wake.saturating_duration_since(now).max(MIN_SLEEP))
            });
            let Some(sleep) = sleep else { break };
            cx.background_executor().timer(sleep).await;
            let parked = cx.update(|cx| {
                let now = cx.background_executor().now();
                tick_hover_fades();
                let clock = cx.default_global::<PulseClock>();
                clock.leases.retain(|_, lease| lease.until > now);
                if clock.leases.is_empty() {
                    clock.running = false;
                    return true;
                }
                let mut owed = Vec::new();
                for (view, lease) in clock.leases.iter_mut() {
                    if lease.due > now {
                        continue;
                    }
                    lease.due += lease.period;
                    if lease.due <= now {
                        lease.due = now + lease.period;
                    }
                    lease.in_flight = true;
                    owed.push(*view);
                }
                for view in owed {
                    cx.notify(view);
                }
                false
            });
            if parked {
                break;
            }
        }
    })
    .detach();
}

/// Current phase `[0,1)` of a repeating spec; reduced motion returns a
/// static 0 and schedules nothing.
pub fn pulse_delta(spec: &MotionSpec, painter: Painter, cx: &mut App) -> f32 {
    if cx.reduced_motion() {
        return 0.0;
    }
    painter.lease(PULSE_FPS, PULSE_LEASE, cx);
    let now = cx.background_executor().now();
    let clock = cx.default_global::<PulseClock>();
    let epoch = *clock.epoch.get_or_insert(now);
    let period = spec.total().as_secs_f32();
    ((now - epoch).as_secs_f32() / period).fract()
}

/// A CSS `cubic-bezier(x1, y1, x2, y2)` timing function, evaluated exactly.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct CubicBezier {
    pub x1: f32,
    pub y1: f32,
    pub x2: f32,
    pub y2: f32,
}

impl CubicBezier {
    pub const fn new(x1: f32, y1: f32, x2: f32, y2: f32) -> Self {
        Self { x1, y1, x2, y2 }
    }

    fn coefficients(a: f32, b: f32) -> (f32, f32, f32) {
        let c = 3.0 * a;
        let bb = 3.0 * (b - a) - c;
        let aa = 1.0 - c - bb;
        (aa, bb, c)
    }

    fn sample_x(&self, t: f32) -> f32 {
        let (a, b, c) = Self::coefficients(self.x1, self.x2);
        ((a * t + b) * t + c) * t
    }

    fn sample_y(&self, t: f32) -> f32 {
        let (a, b, c) = Self::coefficients(self.y1, self.y2);
        ((a * t + b) * t + c) * t
    }

    fn sample_x_derivative(&self, t: f32) -> f32 {
        let (a, b, c) = Self::coefficients(self.x1, self.x2);
        (3.0 * a * t + 2.0 * b) * t + c
    }

    fn solve_t_for_x(&self, x: f32) -> f32 {
        let mut t = x;
        for _ in 0..8 {
            let err = self.sample_x(t) - x;
            if err.abs() < 1e-6 {
                return t;
            }
            let d = self.sample_x_derivative(t);
            if d.abs() < 1e-6 {
                break;
            }
            t -= err / d;
        }
        let (mut lo, mut hi) = (0.0_f32, 1.0_f32);
        for _ in 0..32 {
            let mid = (lo + hi) / 2.0;
            if self.sample_x(mid) < x {
                lo = mid
            } else {
                hi = mid
            }
        }
        (lo + hi) / 2.0
    }

    /// Eased output for input progress `x ∈ [0,1]`, clamped hard — gpui's
    /// animation element asserts its delta is in `[0,1]` and aborts.
    pub fn eval(&self, x: f32) -> f32 {
        if x <= 0.0 {
            return 0.0;
        }
        if x >= 1.0 {
            return 1.0;
        }
        self.sample_y(self.solve_t_for_x(x)).clamp(0.0, 1.0)
    }

    /// This curve as a gpui easing closure.
    pub fn easing(self) -> impl Fn(f32) -> f32 + 'static {
        move |x| self.eval(x)
    }
}

/// The signature entrance curve — CSS `cubic-bezier(0.16, 1, 0.3, 1)`.
pub const EASE_OUT_EXPO: CubicBezier = CubicBezier::new(0.16, 1.0, 0.3, 1.0);
/// CSS `ease-out` — width/height transitions.
pub const EASE_OUT: CubicBezier = CubicBezier::new(0.0, 0.0, 0.58, 1.0);
/// CSS `ease` — quick fades, menu/dialog pops.
pub const EASE: CubicBezier = CubicBezier::new(0.25, 0.1, 0.25, 1.0);
/// Sidebar resort glide — CSS `cubic-bezier(0.22, 1, 0.36, 1)`.
pub const EASE_RESORT: CubicBezier = CubicBezier::new(0.22, 1.0, 0.36, 1.0);
/// CSS `ease-in-out`.
pub const EASE_IN_OUT: CubicBezier = CubicBezier::new(0.42, 0.0, 0.58, 1.0);
/// Tailwind's default transition curve — CSS `cubic-bezier(0.4, 0, 0.2, 1)`.
pub const EASE_TAILWIND: CubicBezier = CubicBezier::new(0.4, 0.0, 0.2, 1.0);

/// One catalog entry: duration + optional delay + curve; gpui has no native
/// delay, so `delay_ms` holds progress at 0 until it elapses.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct MotionSpec {
    pub duration_ms: u64,
    pub delay_ms: u64,
    pub curve: CubicBezier,
}

impl MotionSpec {
    pub const fn new(duration_ms: u64, curve: CubicBezier) -> Self {
        Self {
            duration_ms,
            delay_ms: 0,
            curve,
        }
    }

    pub const fn with_delay(mut self, delay_ms: u64) -> Self {
        self.delay_ms = delay_ms;
        self
    }

    /// Wall-clock span of the whole timeline (delay + duration).
    pub fn total(&self) -> Duration {
        Duration::from_millis(self.delay_ms + self.duration_ms)
    }

    /// Eased progress (0..1) for a raw timeline delta (0..1 across `total`).
    pub fn progress(&self, raw_delta: f32) -> f32 {
        let total = (self.delay_ms + self.duration_ms) as f32;
        if total <= 0.0 || self.duration_ms == 0 {
            return 1.0;
        }
        let t =
            (raw_delta.clamp(0.0, 1.0) * total - self.delay_ms as f32) / self.duration_ms as f32;
        self.curve.eval(t.clamp(0.0, 1.0))
    }

    /// A oneshot gpui [`Animation`] for this spec.
    pub fn animation(&self) -> Animation {
        let spec = *self;
        Animation::new(spec.total().mul_f32(speed_scale())).with_easing(move |d| spec.progress(d))
    }
}

/// Entrances: 0.5s expo-out fade + 4px rise.
pub const FADE_IN: MotionSpec = MotionSpec::new(500, EASE_OUT_EXPO);
/// Quick fade: 0.15s.
pub const FADE_QUICK: MotionSpec = MotionSpec::new(150, EASE);
/// Popover-in: 0.14s.
pub const MENU_IN: MotionSpec = MotionSpec::new(140, EASE);
/// Popover-out: 0.1s — quicker than the entrance.
pub const MENU_OUT: MotionSpec = MotionSpec::new(100, EASE);
/// Dialog-in: 0.18s.
pub const DIALOG_IN: MotionSpec = MotionSpec::new(180, EASE);
/// Boot-splash exit: 0.5s fade + 6px lift after a 0.15s hold.
pub const SPLASH_OUT: MotionSpec = MotionSpec::new(500, EASE).with_delay(150);
/// Sidebar / pane width+height transitions: 200ms ease-out.
pub const RESIZE: MotionSpec = MotionSpec::new(200, EASE_OUT);
/// Tab drag-reorder sliding transforms: 150ms.
pub const TAB_SLIDE: MotionSpec = MotionSpec::new(150, EASE_OUT);
/// Per-pane collapse: 180ms height.
pub const COLLAPSE: MotionSpec = MotionSpec::new(180, EASE_OUT);
/// A gliding move the reader caused rather than one they are dragging.
pub const LAYOUT: MotionSpec = MotionSpec::new(200, EASE_OUT);
/// Chevron rotate: 200ms.
pub const CHEVRON: MotionSpec = MotionSpec::new(200, EASE);
/// Scroll-to-row glide: 500ms ease-in-out.
pub const SCROLL_GLIDE: MotionSpec = MotionSpec::new(500, EASE_IN_OUT);
/// CSS `transition-colors` default: 150ms over [`EASE_TAILWIND`].
pub const HOVER_FADE: MotionSpec = MotionSpec::new(150, EASE_TAILWIND);
/// Toast slide/fade, coop's own: 250ms over [`EASE_TAILWIND`].
pub const NOTIFICATION_IN: MotionSpec = MotionSpec::new(250, EASE_TAILWIND);
/// Pulse loader period: 2.4s.
pub const PULSE: MotionSpec = MotionSpec::new(2400, EASE);
/// Spinner wave period: 750ms.
pub const GRADIENT_SPIN: MotionSpec = MotionSpec::new(750, EASE);

/// Standard entrance: opacity 0→1 + translateY 4→0 over [`FADE_IN`].
pub fn fade_in<E>(id: impl Into<ElementId>, element: E) -> AnimationElement<E>
where
    E: Styled + IntoElement + 'static,
{
    element.with_animation(id, FADE_IN.animation(), |el, t| {
        el.relative().opacity(t).top(px(4.0 * (1.0 - t)))
    })
}

/// Quick opacity-only fade over [`FADE_QUICK`].
pub fn fade_quick<E>(id: impl Into<ElementId>, element: E) -> AnimationElement<E>
where
    E: Styled + IntoElement + 'static,
{
    element.with_animation(id, FADE_QUICK.animation(), |el, t| el.opacity(t))
}

/// Popover entrance: fade + translateY −2→0 over [`MENU_IN`].
pub fn menu_in<E>(id: impl Into<ElementId>, element: E) -> AnimationElement<E>
where
    E: Styled + IntoElement + 'static,
{
    element.with_animation(id, MENU_IN.animation(), |el, t| {
        el.relative()
            .opacity(0.3 + 0.7 * t)
            .top(px(-2.0 * (1.0 - t)))
    })
}

/// Popover exit over [`MENU_OUT`]; the caller supplies progress because
/// `with_animation` replays from 0 on remount.
pub fn menu_out<E>(id: impl Into<ElementId>, t: f32, element: E) -> AnimationElement<E>
where
    E: Styled + IntoElement + 'static,
{
    element.with_animation(id, MENU_OUT.animation(), move |el, _| {
        el.relative().opacity(1.0 - t).top(px(-2.0 * t))
    })
}

/// Dialog entrance over [`DIALOG_IN`] (fade + 2px rise).
pub fn dialog_in<E>(id: impl Into<ElementId>, element: E) -> AnimationElement<E>
where
    E: Styled + IntoElement + 'static,
{
    element.with_animation(id, DIALOG_IN.animation(), |el, t| {
        el.relative().opacity(t).top(px(2.0 * (1.0 - t)))
    })
}

/// Boot-splash exit: hold 150ms, then fade out + lift 6px over 500ms.
pub fn splash_out<E>(id: impl Into<ElementId>, element: E) -> AnimationElement<E>
where
    E: Styled + IntoElement + 'static,
{
    element.with_animation(id, SPLASH_OUT.animation(), |el, t| {
        el.opacity(1.0 - t).top(px(-6.0 * t))
    })
}

/// Linear interpolation.
pub fn lerp(from: f32, to: f32, t: f32) -> f32 {
    from + (to - from) * t
}

/// One element's hover fade progress over [`HOVER_FADE`].
#[derive(Debug, Clone, Copy)]
pub struct FadeEntry {
    origin: f32,
    target: f32,
    started: Instant,
    seen: u64,
}

impl FadeEntry {
    fn value(&self, now: Instant, duration: Duration) -> f32 {
        let elapsed = now.saturating_duration_since(self.started);
        if duration.is_zero() || elapsed >= duration {
            return self.target;
        }
        let raw = elapsed.as_secs_f32() / duration.as_secs_f32();
        lerp(self.origin, self.target, HOVER_FADE.curve.eval(raw))
    }

    fn settled(&self, now: Instant, duration: Duration) -> bool {
        self.origin == self.target || now.saturating_duration_since(self.started) >= duration
    }
}

/// Which fade: the view that paints it plus the element key — two views
/// using the same string would trade washes.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Fade {
    pub painter: Painter,
    pub key: SharedString,
}

impl Fade {
    pub fn new(painter: Painter, key: impl Into<SharedString>) -> Self {
        Self {
            painter,
            key: key.into(),
        }
    }
}

/// Per-fade progress store.
#[derive(Default)]
pub struct HoverFades {
    pub entries: HashMap<Fade, FadeEntry>,
    frame: u64,
}

impl HoverFades {
    pub fn duration() -> Duration {
        HOVER_FADE.total().mul_f32(speed_scale())
    }

    /// Pointer entered (`hovered`) or left the element behind `fade`.
    pub fn set_at(&mut self, fade: &Fade, hovered: bool, reduced: bool, now: Instant) {
        let target = if hovered { 1.0 } else { 0.0 };
        let duration = Self::duration();
        let current = self
            .entries
            .get(fade)
            .map(|e| e.value(now, duration))
            .unwrap_or(0.0);
        if target == 0.0 && !self.entries.contains_key(fade) {
            return;
        }
        let origin = if reduced { target } else { current };
        let seen = self.frame;
        self.entries.insert(
            fade.clone(),
            FadeEntry {
                origin,
                target,
                started: now,
                seen,
            },
        );
    }

    /// Hover progress (0..1) for `fade` at `now`; stamps liveness.
    pub fn value_at(&mut self, fade: &Fade, now: Instant) -> f32 {
        let frame = self.frame;
        match self.entries.get_mut(fade) {
            Some(entry) => {
                entry.seen = frame;
                entry.value(now, Self::duration())
            }
            None => 0.0,
        }
    }

    /// Advance the frame counter and prune settled or unmounted entries.
    pub fn tick_at(&mut self, now: Instant) -> bool {
        self.frame += 1;
        let frame = self.frame;
        let duration = Self::duration();
        let mut active = false;
        self.entries.retain(|_, entry| {
            if entry.seen + 1 < frame {
                return false;
            }
            let settled = entry.settled(now, duration);
            if !settled {
                active = true;
            }
            !(settled && entry.target == 0.0)
        });
        active
    }
}

thread_local! {
    static HOVER_FADES: RefCell<HoverFades> = RefCell::new(HoverFades::default());
}

/// Hover progress (0..1) for `fade` this frame.
pub fn hover_t(fade: &Fade) -> f32 {
    HOVER_FADES.with(|fades| fades.borrow_mut().value_at(fade, Instant::now()))
}

/// Record a hover flip for `fade`; prefer [`hover_listener`].
pub fn set_hover(fade: &Fade, hovered: bool, reduced: bool) {
    HOVER_FADES.with(|fades| {
        fades
            .borrow_mut()
            .set_at(fade, hovered, reduced, Instant::now())
    });
}

/// An `.on_hover` listener driving the fade.
pub fn hover_listener(fade: Fade) -> impl Fn(&bool, &mut Window, &mut App) + 'static {
    move |hovered, _window, cx| {
        set_hover(&fade, *hovered, cx.reduced_motion());
        fade.painter.lease(HOVER_FPS, HoverFades::duration(), cx);
    }
}

/// Once-per-tick bookkeeping for the fade store, driven by the clock.
fn tick_hover_fades() {
    HOVER_FADES.with(|fades| {
        fades.borrow_mut().tick_at(Instant::now());
    });
}

/// Blend two colors by `t` the way the browser transitions them: sRGB with
/// premultiplied alpha, so a wash fading in never passes through grey.
pub fn mix(from: Hsla, to: Hsla, t: f32) -> Hsla {
    let t = t.clamp(0.0, 1.0);
    if t <= 0.0 {
        return from;
    }
    if t >= 1.0 {
        return to;
    }
    let (f, g) = (Rgba::from(from), Rgba::from(to));
    let a = lerp(f.a, g.a, t);
    if a <= f32::EPSILON {
        return Hsla::from(Rgba { a: 0.0, ..g });
    }
    Hsla::from(Rgba {
        r: lerp(f.r * f.a, g.r * g.a, t) / a,
        g: lerp(f.g * f.a, g.g * g.a, t) / a,
        b: lerp(f.b * f.a, g.b * g.a, t) / a,
        a,
    })
}

/// The standard hover blend: rest → hover color at this fade's progress.
pub fn hover_blend(fade: &Fade, rest: Hsla, hover: Hsla) -> Hsla {
    mix(rest, hover, hover_t(fade))
}

/// Process-wide motion speed, as raw `f32` bits; an atomic mirror because the
/// timelines are read from free functions with no `cx` in scope.
static SPEED: AtomicU32 = AtomicU32::new(1.0f32.to_bits());

/// How far every timeline in the catalog is stretched. `1.0` is the designed
/// speed.
pub fn speed_scale() -> f32 {
    f32::from_bits(SPEED.load(Ordering::Relaxed))
}

/// Stretch every catalog timeline by `scale`; clamped, and a non-finite
/// `scale` resets to `1.0`.
pub fn set_speed(scale: f32) {
    let scale = if scale.is_finite() {
        scale.clamp(0.01, 100.0)
    } else {
        1.0
    };
    SPEED.store(scale.to_bits(), Ordering::Relaxed);
}

/// Process-wide motion switches.
#[derive(Clone, Copy)]
struct MotionSettings {
    reduced_motion: bool,
    pause_when_inactive: bool,
}

impl Global for MotionSettings {}

impl MotionSettings {
    /// Defaults: the OS flag rules reduce motion; the clock pauses when no
    /// window is active.
    fn get(cx: &App) -> Self {
        cx.try_global::<MotionSettings>().copied().unwrap_or(Self {
            reduced_motion: false,
            pause_when_inactive: true,
        })
    }
}

/// App-level motion switches.
pub trait AppExt {
    /// The OS reduce-motion flag ORed with the manual override.
    fn reduced_motion(&self) -> bool;

    /// Set the manual reduce-motion override.
    fn set_reduced_motion(&mut self, reduced: bool);

    /// Whether the leased clock claims frames while no window is active.
    fn pause_when_inactive(&self) -> bool;

    fn set_pause_when_inactive(&mut self, pause: bool);
}

impl AppExt for App {
    fn reduced_motion(&self) -> bool {
        self.reduce_motion() || MotionSettings::get(self).reduced_motion
    }

    fn set_reduced_motion(&mut self, reduced: bool) {
        let mut settings = MotionSettings::get(self);
        settings.reduced_motion = reduced;
        self.set_global(settings);
    }

    fn pause_when_inactive(&self) -> bool {
        MotionSettings::get(self).pause_when_inactive
    }

    fn set_pause_when_inactive(&mut self, pause: bool) {
        let mut settings = MotionSettings::get(self);
        settings.pause_when_inactive = pause;
        self.set_global(settings);
    }
}
