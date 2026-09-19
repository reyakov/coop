use gpui::prelude::FluentBuilder;
use gpui::{
    AbsoluteLength, AnyElement, App, Bounds, Div, Hsla, ImageSource, InteractiveElement,
    Interactivity, IntoElement, ObjectFit, ParentElement, PathBuilder, Pixels, Point, RenderOnce,
    SharedString, StyleRefinement, Styled, StyledImage, Window, canvas, div, img, point, px,
};
use theme::ActiveTheme;

use crate::{Selectable, Sizable, Size, StyledExt};

/// Number of rows and columns in the generated pixel grid.
const PIXEL_GRID: usize = 8;
/// Probability that a cell in the left half of the grid is filled.
const FILL_PROBABILITY: f32 = 0.42;
/// Probability that a filled cell uses the accent shade instead of the main color.
const ACCENT_PROBABILITY: f32 = 0.25;
/// Minimum number of filled left-half cells, so a pattern never reads as empty.
const MIN_FILLED: usize = 5;
/// Fallback seed for an avatar that has neither a picture nor a seed of its own.
const FALLBACK_SEED: &str = "coop";
/// Number of segments used to approximate the avatar circle.
const CIRCLE_SEGMENTS: usize = 32;

/// Returns the size of the avatar based on the given [`Size`].
pub(super) fn avatar_size(size: Size) -> AbsoluteLength {
    match size {
        Size::Large => px(64.).into(),
        Size::Medium => px(32.).into(),
        Size::Small => px(24.).into(),
        Size::XSmall => px(20.).into(),
        Size::Size(size) => size.into(),
    }
}

/// A deterministic, offline pixel-art avatar derived from a seed.
///
/// Use it for entities that have no profile picture: the same seed always
/// renders the same pattern, so identities stay recognizable without a
/// network round trip. The pattern is painted as geometry and cropped to a
/// circle, at the same sizes as [`Avatar`].
///
/// # Examples
///
/// ```
/// use ui::avatar::PixelAvatar;
///
/// PixelAvatar::new("alice");
/// ```
#[derive(IntoElement)]
pub struct PixelAvatar {
    seed: u64,
    size: Size,
    style: StyleRefinement,
}

impl PixelAvatar {
    /// Creates a pixel avatar from `seed`.
    pub fn new(seed: impl AsRef<str>) -> Self {
        Self {
            seed: fnv1a(seed.as_ref().as_bytes()),
            size: Size::Medium,
            style: StyleRefinement::default(),
        }
    }
}

impl Sizable for PixelAvatar {
    fn with_size(mut self, size: impl Into<Size>) -> Self {
        self.size = size.into();
        self
    }
}

impl Styled for PixelAvatar {
    fn style(&mut self) -> &mut StyleRefinement {
        &mut self.style
    }
}

impl RenderOnce for PixelAvatar {
    fn render(self, window: &mut Window, _cx: &mut App) -> impl IntoElement {
        let side = avatar_size(self.size).to_pixels(window.rem_size());
        let seed = self.seed;

        canvas(
            move |_bounds, _window, _cx| seed,
            move |bounds, seed, window, cx| {
                let theme = cx.theme();
                let main = Hsla {
                    h: (theme.icon_accent.h + seed as f32 / u64::MAX as f32) % 1.,
                    s: 0.6,
                    l: if theme.is_dark() { 0.6 } else { 0.45 },
                    a: 1.,
                };
                let shade = if theme.is_dark() {
                    Hsla {
                        l: (main.l * 1.6).min(0.95),
                        ..main
                    }
                } else {
                    Hsla {
                        l: (main.l * 0.45).max(0.18),
                        ..main
                    }
                };

                let circle = circle_polygon(bounds.center(), bounds.size.width.as_f32() / 2.);
                paint_polygons(window, std::iter::once(&circle), main.opacity(0.16));

                let pattern = pixel_pattern(seed);
                let mut cells = Vec::new();

                for (value, color) in [(1u8, main), (2u8, shade)] {
                    cells.clear();

                    for row in 0..PIXEL_GRID {
                        for col in 0..PIXEL_GRID {
                            if pattern[row * PIXEL_GRID + col] != value {
                                continue;
                            }

                            let cell = clip_polygon(&cell_polygon(&bounds, row, col), &circle);
                            if cell.len() >= 3 {
                                cells.push(cell);
                            }
                        }
                    }

                    paint_polygons(window, cells.iter(), color);
                }
            },
        )
        .refine_style(&self.style)
        .size(side)
        .flex_shrink_0()
    }
}

/// Builds the mirrored fill pattern for `seed`.
fn pixel_pattern(seed: u64) -> [u8; PIXEL_GRID * PIXEL_GRID] {
    let mut rng = PixelRng::new(seed);
    let mut pattern = [0u8; PIXEL_GRID * PIXEL_GRID];
    let mut filled = 0usize;

    for row in 0..PIXEL_GRID {
        for col in 0..PIXEL_GRID / 2 {
            if rng.chance(FILL_PROBABILITY) {
                let accent = rng.chance(ACCENT_PROBABILITY);
                set_cell(&mut pattern, row, col, if accent { 2 } else { 1 });
                filled += 1;
            }
        }
    }

    if filled < MIN_FILLED {
        let half = PIXEL_GRID * PIXEL_GRID / 2;
        let start = (rng.next() % half as u64) as usize;

        for offset in 0..half {
            if filled >= MIN_FILLED {
                break;
            }

            let ix = (start + offset) % half;
            let row = ix / (PIXEL_GRID / 2);
            let col = ix % (PIXEL_GRID / 2);

            if pattern[row * PIXEL_GRID + col] == 0 {
                set_cell(&mut pattern, row, col, 1);
                filled += 1;
            }
        }
    }

    pattern
}

/// Paints `polygons` as a single anti-aliased filled path in `color`.
fn paint_polygons<'a>(
    window: &mut Window,
    polygons: impl IntoIterator<Item = &'a Vec<Point<Pixels>>>,
    color: Hsla,
) {
    let mut builder = PathBuilder::fill();
    let mut painted = false;

    for polygon in polygons {
        if polygon.len() >= 3 {
            builder.add_polygon(polygon, true);
            painted = true;
        }
    }

    if painted && let Ok(path) = builder.build() {
        window.paint_path(path, color);
    }
}

/// Approximates the circle of `radius` around `center` as a convex polygon,
/// wound so that its interior is on the left of every directed edge.
fn circle_polygon(center: Point<Pixels>, radius: f32) -> Vec<Point<Pixels>> {
    let center_x = center.x.as_f32();
    let center_y = center.y.as_f32();

    (0..CIRCLE_SEGMENTS)
        .map(|index| {
            let angle = std::f32::consts::TAU * index as f32 / CIRCLE_SEGMENTS as f32;
            point(
                px(center_x + radius * angle.cos()),
                px(center_y + radius * angle.sin()),
            )
        })
        .collect()
}

/// The four corners of cell `(row, col)` of the grid laid out in `bounds`.
fn cell_polygon(bounds: &Bounds<Pixels>, row: usize, col: usize) -> [Point<Pixels>; 4] {
    let cell = bounds.size.width.as_f32() / PIXEL_GRID as f32;
    let left = bounds.origin.x.as_f32() + col as f32 * cell;
    let top = bounds.origin.y.as_f32() + row as f32 * cell;

    [
        point(px(left), px(top)),
        point(px(left + cell), px(top)),
        point(px(left + cell), px(top + cell)),
        point(px(left), px(top + cell)),
    ]
}

/// Clips `subject` to the convex `clip` polygon, keeping the part inside it.
fn clip_polygon(subject: &[Point<Pixels>], clip: &[Point<Pixels>]) -> Vec<Point<Pixels>> {
    let mut current = subject.to_vec();
    let mut next = Vec::with_capacity(subject.len() + 4);

    for (&start, &end) in clip.iter().zip(clip.iter().cycle().skip(1)) {
        if current.is_empty() {
            break;
        }

        next.clear();
        let mut previous = match current.last() {
            Some(&vertex) => vertex,
            None => break,
        };

        for &vertex in current.iter() {
            let previous_inside = is_inside(start, end, previous);
            let vertex_inside = is_inside(start, end, vertex);

            if vertex_inside {
                if !previous_inside
                    && let Some(crossing) = line_intersection(start, end, previous, vertex)
                {
                    next.push(crossing);
                }
                next.push(vertex);
            } else if previous_inside
                && let Some(crossing) = line_intersection(start, end, previous, vertex)
            {
                next.push(crossing);
            }

            previous = vertex;
        }

        std::mem::swap(&mut current, &mut next);
    }

    current
}

/// Whether `vertex` lies on the interior side of the directed edge `start -> end`.
fn is_inside(start: Point<Pixels>, end: Point<Pixels>, vertex: Point<Pixels>) -> bool {
    let start_x = start.x.as_f32();
    let start_y = start.y.as_f32();
    let edge_x = end.x.as_f32() - start_x;
    let edge_y = end.y.as_f32() - start_y;
    let to_vertex_x = vertex.x.as_f32() - start_x;
    let to_vertex_y = vertex.y.as_f32() - start_y;

    edge_x * to_vertex_y - edge_y * to_vertex_x >= 0.
}

/// The intersection of segment `from -> to` with the infinite line `start -> end`.
fn line_intersection(
    start: Point<Pixels>,
    end: Point<Pixels>,
    from: Point<Pixels>,
    to: Point<Pixels>,
) -> Option<Point<Pixels>> {
    let start_x = start.x.as_f32();
    let start_y = start.y.as_f32();
    let edge_x = end.x.as_f32() - start_x;
    let edge_y = end.y.as_f32() - start_y;
    let from_x = from.x.as_f32();
    let from_y = from.y.as_f32();
    let segment_x = to.x.as_f32() - from_x;
    let segment_y = to.y.as_f32() - from_y;
    let denominator = edge_x * segment_y - edge_y * segment_x;

    if denominator.abs() < f32::EPSILON {
        return None;
    }

    let offset_x = from_x - start_x;
    let offset_y = from_y - start_y;
    let t = (edge_y * offset_x - edge_x * offset_y) / denominator;

    Some(point(
        px(from_x + segment_x * t),
        px(from_y + segment_y * t),
    ))
}

/// Fills `cell (row, col)` and its horizontal mirror.
fn set_cell(pattern: &mut [u8; PIXEL_GRID * PIXEL_GRID], row: usize, col: usize, value: u8) {
    pattern[row * PIXEL_GRID + col] = value;
    pattern[row * PIXEL_GRID + (PIXEL_GRID - 1 - col)] = value;
}

/// FNV-1a 64-bit hash, stable across platforms and runs.
fn fnv1a(bytes: &[u8]) -> u64 {
    let mut hash = 0xcbf2_9ce4_8422_2325u64;
    for &byte in bytes {
        hash ^= byte as u64;
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    hash
}

/// Tiny xorshift64* PRNG for deriving the pattern from the seed.
struct PixelRng(u64);

impl PixelRng {
    fn new(seed: u64) -> Self {
        Self(seed.max(1))
    }

    fn next(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.0 = x;
        x.wrapping_mul(0x2545_f491_4f6c_dd1d)
    }

    fn chance(&mut self, probability: f32) -> bool {
        self.next() as f32 / (u64::MAX as f32) < probability
    }
}

/// Renders the generated pixel avatar shown in place of a missing picture.
fn generated_avatar(seed: Option<&str>, size: Pixels) -> AnyElement {
    PixelAvatar::new(seed.unwrap_or(FALLBACK_SEED))
        .with_size(size)
        .into_any_element()
}

/// An element that renders a user avatar with customizable appearance options.
///
/// Entities without a picture still get a stable identity: the avatar falls
/// back to a [`PixelAvatar`] seeded through [`Avatar::seed`], both when there
/// is no picture and when the picture fails to load.
///
/// # Examples
///
/// ```
/// use ui::avatar::Avatar;
///
/// Avatar::new(None).seed("alice");
/// ```
#[derive(IntoElement)]
pub struct Avatar {
    base: Div,
    picture: Option<ImageSource>,
    grayscale: bool,
    seed: Option<SharedString>,
    style: StyleRefinement,
    size: Size,
    border_color: Option<Hsla>,
    selected: bool,
}

impl Avatar {
    /// Creates an avatar for an entity whose profile picture may be missing.
    ///
    /// Use [`Avatar::seed`] to choose the generated
    /// pixel avatar rendered when `picture` is `None`.
    pub fn new(picture: Option<SharedString>) -> Self {
        Self::from_picture(picture.map(ImageSource::from))
    }

    /// Creates an avatar from an already-resolved source.
    pub fn from_source(picture: impl Into<ImageSource>) -> Self {
        Self::from_picture(Some(picture.into()))
    }

    fn from_picture(picture: Option<ImageSource>) -> Self {
        Avatar {
            base: div(),
            picture,
            grayscale: false,
            seed: None,
            style: StyleRefinement::default(),
            size: Size::Medium,
            border_color: None,
            selected: false,
        }
    }

    /// Sets the seed for the generated pixel avatar.
    ///
    /// The seed should be a stable identifier of the entity the avatar
    /// represents, such as a public key.
    pub fn seed(mut self, seed: impl Into<SharedString>) -> Self {
        self.seed = Some(seed.into());
        self
    }

    /// Applies a grayscale filter to the avatar image.
    ///
    /// # Examples
    ///
    /// ```
    /// use ui::avatar::Avatar;
    ///
    /// Avatar::new(None).grayscale(true);
    /// ```
    pub fn grayscale(mut self, grayscale: bool) -> Self {
        self.grayscale = grayscale;
        self
    }

    /// Sets the border color of the avatar.
    ///
    /// This might be used to match the border to the background color of
    /// the parent element to create the illusion of cropping another
    /// shape underneath (for example in face piles.)
    pub fn border_color(mut self, color: impl Into<Hsla>) -> Self {
        self.border_color = Some(color.into());
        self
    }
}

impl Sizable for Avatar {
    fn with_size(mut self, size: impl Into<Size>) -> Self {
        self.size = size.into();
        self
    }
}

impl Styled for Avatar {
    fn style(&mut self) -> &mut StyleRefinement {
        &mut self.style
    }
}

impl Selectable for Avatar {
    fn is_selected(&self) -> bool {
        self.selected
    }

    fn selected(mut self, selected: bool) -> Self {
        self.selected = selected;
        self
    }
}

impl InteractiveElement for Avatar {
    fn interactivity(&mut self) -> &mut Interactivity {
        self.base.interactivity()
    }
}

impl RenderOnce for Avatar {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let border_width = if self.border_color.is_some() {
            px(2.)
        } else {
            px(0.)
        };
        let image_size = avatar_size(self.size).to_pixels(window.rem_size());
        let container_size = image_size + border_width * 2.;

        let content = match self.picture {
            Some(picture) => {
                let seed = self.seed;
                let grayscale = self.grayscale;
                img(picture)
                    .size(image_size)
                    .rounded_full()
                    .object_fit(ObjectFit::Cover)
                    .grayscale(grayscale)
                    .bg(cx.theme().ghost_element_background)
                    .with_fallback(move || generated_avatar(seed.as_deref(), image_size))
                    .into_any_element()
            }
            None => generated_avatar(self.seed.as_deref(), image_size),
        };

        div()
            .flex_shrink_0()
            .size(container_size)
            .rounded_full()
            .overflow_hidden()
            .when_some(self.border_color, |this, color| {
                this.border(border_width).border_color(color)
            })
            .child(content)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pixel_patterns_are_symmetric_and_stable() {
        for seed in 0..50 {
            let pattern = pixel_pattern(seed);
            let filled = pattern.iter().filter(|&&cell| cell != 0).count();

            assert!(
                filled >= MIN_FILLED * 2,
                "pattern too sparse for seed {seed}"
            );

            for row in 0..PIXEL_GRID {
                for col in 0..PIXEL_GRID {
                    assert_eq!(
                        pattern[row * PIXEL_GRID + col],
                        pattern[row * PIXEL_GRID + (PIXEL_GRID - 1 - col)],
                        "asymmetric pattern for seed {seed} at ({row}, {col})"
                    );
                }
            }
        }

        for seed in [0, 1, 42, u64::MAX] {
            assert_eq!(pixel_pattern(seed), pixel_pattern(seed));
        }

        assert_ne!(pixel_pattern(42), pixel_pattern(43));
    }

    fn area(polygon: &[Point<Pixels>]) -> f32 {
        let mut sum: f32 = 0.;
        for (&a, &b) in polygon.iter().zip(polygon.iter().cycle().skip(1)) {
            sum += a.x.as_f32() * b.y.as_f32() - b.x.as_f32() * a.y.as_f32();
        }
        (sum / 2.).abs()
    }

    #[test]
    fn clipping_keeps_only_the_part_inside_the_circle() {
        let circle = circle_polygon(point(px(10.), px(10.)), 10.);
        let square = |left: f32, top: f32| {
            [
                point(px(left), px(top)),
                point(px(left + 4.), px(top)),
                point(px(left + 4.), px(top + 4.)),
                point(px(left), px(top + 4.)),
            ]
        };

        let inside = clip_polygon(&square(8., 8.), &circle);
        assert!((area(&inside) - 16.).abs() < 0.05, "area {}", area(&inside));

        assert!(clip_polygon(&square(20., 20.), &circle).is_empty());

        let straddling = clip_polygon(&square(0., 0.), &circle);
        for vertex in &straddling {
            let delta_x = vertex.x.as_f32() - 10.;
            let delta_y = vertex.y.as_f32() - 10.;
            assert!(
                delta_x.hypot(delta_y) <= 10. + 0.1,
                "clipped vertex outside the circle"
            );
        }

        let area = area(&straddling);
        assert!(area > 0. && area < 16., "area {area}");
    }
}
