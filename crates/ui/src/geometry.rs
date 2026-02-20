use std::fmt::{self, Debug, Display, Formatter};

use gpui::{AbsoluteLength, Axis, Corner, Length, Pixels};
use serde::{Deserialize, Serialize};

/// A enum for defining the placement of the element.
///
/// See also: [`Side`] if you need to define the left, right side.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Placement {
    #[serde(rename = "top")]
    Top,
    #[serde(rename = "bottom")]
    Bottom,
    #[serde(rename = "left")]
    Left,
    #[serde(rename = "right")]
    Right,
}

impl Display for Placement {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        match self {
            Placement::Top => write!(f, "Top"),
            Placement::Bottom => write!(f, "Bottom"),
            Placement::Left => write!(f, "Left"),
            Placement::Right => write!(f, "Right"),
        }
    }
}

impl Placement {
    #[inline]
    pub fn is_horizontal(&self) -> bool {
        matches!(self, Placement::Left | Placement::Right)
    }

    #[inline]
    pub fn is_vertical(&self) -> bool {
        matches!(self, Placement::Top | Placement::Bottom)
    }

    #[inline]
    pub fn axis(&self) -> Axis {
        match self {
            Placement::Top | Placement::Bottom => Axis::Vertical,
            Placement::Left | Placement::Right => Axis::Horizontal,
        }
    }
}

/// The anchor position of an element.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum Anchor {
    #[default]
    #[serde(rename = "top-left")]
    TopLeft,
    #[serde(rename = "top-center")]
    TopCenter,
    #[serde(rename = "top-right")]
    TopRight,
    #[serde(rename = "bottom-left")]
    BottomLeft,
    #[serde(rename = "bottom-center")]
    BottomCenter,
    #[serde(rename = "bottom-right")]
    BottomRight,
}

impl Display for Anchor {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        match self {
            Anchor::TopLeft => write!(f, "TopLeft"),
            Anchor::TopCenter => write!(f, "TopCenter"),
            Anchor::TopRight => write!(f, "TopRight"),
            Anchor::BottomLeft => write!(f, "BottomLeft"),
            Anchor::BottomCenter => write!(f, "BottomCenter"),
            Anchor::BottomRight => write!(f, "BottomRight"),
        }
    }
}

impl Anchor {
    /// Returns true if the anchor is at the top.
    #[inline]
    pub fn is_top(&self) -> bool {
        matches!(self, Self::TopLeft | Self::TopCenter | Self::TopRight)
    }

    /// Returns true if the anchor is at the bottom.
    #[inline]
    pub fn is_bottom(&self) -> bool {
        matches!(
            self,
            Self::BottomLeft | Self::BottomCenter | Self::BottomRight
        )
    }

    /// Returns true if the anchor is at the left.
    #[inline]
    pub fn is_left(&self) -> bool {
        matches!(self, Self::TopLeft | Self::BottomLeft)
    }

    /// Returns true if the anchor is at the right.
    #[inline]
    pub fn is_right(&self) -> bool {
        matches!(self, Self::TopRight | Self::BottomRight)
    }

    /// Returns true if the anchor is at the center.
    #[inline]
    pub fn is_center(&self) -> bool {
        matches!(self, Self::TopCenter | Self::BottomCenter)
    }

    /// Swaps the vertical position of the anchor.
    pub fn swap_vertical(&self) -> Self {
        match self {
            Anchor::TopLeft => Anchor::BottomLeft,
            Anchor::TopCenter => Anchor::BottomCenter,
            Anchor::TopRight => Anchor::BottomRight,
            Anchor::BottomLeft => Anchor::TopLeft,
            Anchor::BottomCenter => Anchor::TopCenter,
            Anchor::BottomRight => Anchor::TopRight,
        }
    }

    /// Swaps the horizontal position of the anchor.
    pub fn swap_horizontal(&self) -> Self {
        match self {
            Anchor::TopLeft => Anchor::TopRight,
            Anchor::TopCenter => Anchor::TopCenter,
            Anchor::TopRight => Anchor::TopLeft,
            Anchor::BottomLeft => Anchor::BottomRight,
            Anchor::BottomCenter => Anchor::BottomCenter,
            Anchor::BottomRight => Anchor::BottomLeft,
        }
    }

    pub(crate) fn other_side_corner_along(&self, axis: Axis) -> Anchor {
        match axis {
            Axis::Vertical => match self {
                Self::TopLeft => Self::BottomLeft,
                Self::TopCenter => Self::BottomCenter,
                Self::TopRight => Self::BottomRight,
                Self::BottomLeft => Self::TopLeft,
                Self::BottomCenter => Self::TopCenter,
                Self::BottomRight => Self::TopRight,
            },
            Axis::Horizontal => match self {
                Self::TopLeft => Self::TopRight,
                Self::TopCenter => Self::TopCenter,
                Self::TopRight => Self::TopLeft,
                Self::BottomLeft => Self::BottomRight,
                Self::BottomCenter => Self::BottomCenter,
                Self::BottomRight => Self::BottomLeft,
            },
        }
    }
}

impl From<Corner> for Anchor {
    fn from(corner: Corner) -> Self {
        match corner {
            Corner::TopLeft => Anchor::TopLeft,
            Corner::TopRight => Anchor::TopRight,
            Corner::BottomLeft => Anchor::BottomLeft,
            Corner::BottomRight => Anchor::BottomRight,
        }
    }
}

impl From<Anchor> for Corner {
    fn from(anchor: Anchor) -> Self {
        match anchor {
            Anchor::TopLeft => Corner::TopLeft,
            Anchor::TopRight => Corner::TopRight,
            Anchor::BottomLeft => Corner::BottomLeft,
            Anchor::BottomRight => Corner::BottomRight,
            Anchor::TopCenter => Corner::TopLeft,
            Anchor::BottomCenter => Corner::BottomLeft,
        }
    }
}

/// A enum for defining the side of the element.
///
/// See also: [`Placement`] if you need to define the 4 edges.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Side {
    #[serde(rename = "left")]
    Left,
    #[serde(rename = "right")]
    Right,
}

impl Side {
    /// Returns true if the side is left.
    #[inline]
    pub fn is_left(&self) -> bool {
        matches!(self, Self::Left)
    }

    /// Returns true if the side is right.
    #[inline]
    pub fn is_right(&self) -> bool {
        matches!(self, Self::Right)
    }
}

/// A trait to extend the [`Axis`] enum with utility methods.
pub trait AxisExt {
    #[allow(clippy::wrong_self_convention)]
    fn is_horizontal(self) -> bool;
    #[allow(clippy::wrong_self_convention)]
    fn is_vertical(self) -> bool;
}

impl AxisExt for Axis {
    #[inline]
    fn is_horizontal(self) -> bool {
        self == Axis::Horizontal
    }

    #[inline]
    fn is_vertical(self) -> bool {
        self == Axis::Vertical
    }
}

/// A trait for converting [`Pixels`] to `f32` and `f64`.
pub trait PixelsExt {
    fn as_f32(&self) -> f32;
    #[allow(clippy::wrong_self_convention)]
    fn as_f64(self) -> f64;
}
impl PixelsExt for Pixels {
    fn as_f32(&self) -> f32 {
        f32::from(self)
    }

    fn as_f64(self) -> f64 {
        f64::from(self)
    }
}

/// A trait to extend the [`Length`] enum with utility methods.
pub trait LengthExt {
    /// Converts the [`Length`] to [`Pixels`] based on a given `base_size` and `rem_size`.
    ///
    /// If the [`Length`] is [`Length::Auto`], it returns `None`.
    fn to_pixels(&self, base_size: AbsoluteLength, rem_size: Pixels) -> Option<Pixels>;
}

impl LengthExt for Length {
    fn to_pixels(&self, base_size: AbsoluteLength, rem_size: Pixels) -> Option<Pixels> {
        match self {
            Length::Auto => None,
            Length::Definite(len) => Some(len.to_pixels(base_size, rem_size)),
        }
    }
}

/// A struct for defining the edges of an element.
///
/// A extend version of [`gpui::Edges`] to serialize/deserialize.
#[derive(Debug, Clone, Default, Serialize, Deserialize, Eq, PartialEq)]
#[repr(C)]
pub struct Edges<T: Clone + Debug + Default + PartialEq> {
    /// The size of the top edge.
    pub top: T,
    /// The size of the right edge.
    pub right: T,
    /// The size of the bottom edge.
    pub bottom: T,
    /// The size of the left edge.
    pub left: T,
}

impl<T> Edges<T>
where
    T: Clone + Debug + Default + PartialEq,
{
    /// Creates a new `Edges` instance with all edges set to the same value.
    pub fn all(value: T) -> Self {
        Self {
            top: value.clone(),
            right: value.clone(),
            bottom: value.clone(),
            left: value,
        }
    }
}
