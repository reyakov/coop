use std::sync::atomic::{AtomicU32, Ordering};

use gpui::{FontWeight, Styled, px};

/// The base the ladder starts at; bezel's measured body is 13, coop keeps 15.
pub const DEFAULT_BASE_TEXT_SIZE: f32 = 15.0;

/// The body size every painted role is scaled against, as raw `f32` bits.
static BASE: AtomicU32 = AtomicU32::new(DEFAULT_BASE_TEXT_SIZE.to_bits());

/// Point the ladder at a body size in points; the caller refreshes windows.
pub fn set_base_text_size(points: f32) {
    BASE.store(points.max(1.0).to_bits(), Ordering::Relaxed);
}

/// The body size in points every painted role is scaled against.
pub fn base_text_size() -> f32 {
    f32::from_bits(BASE.load(Ordering::Relaxed))
}

/// A role in the type ladder — SwiftUI's `Font.TextStyle`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum TextStyle {
    LargeTitle,
    Title,
    Title2,
    Title3,
    Headline,
    Subheadline,
    Body,
    Callout,
    Footnote,
    Caption,
    Caption2,
}

impl TextStyle {
    /// The role's measured size in points.
    pub const fn size(self) -> f32 {
        match self {
            Self::LargeTitle => 26.0,
            Self::Title => 22.0,
            Self::Title2 => 17.0,
            Self::Title3 => 15.0,
            Self::Headline | Self::Body => 13.0,
            Self::Callout => 12.0,
            Self::Subheadline => 11.0,
            Self::Footnote | Self::Caption | Self::Caption2 => 10.0,
        }
    }

    pub fn painted(self) -> f32 {
        self.size() * base_text_size() / Self::Body.size()
    }

    pub const fn line_height(self) -> f32 {
        match self {
            Self::LargeTitle => 32.0,
            Self::Title => 26.0,
            Self::Title2 => 22.0,
            Self::Title3 => 20.0,
            Self::Headline | Self::Body => 16.0,
            Self::Callout => 15.0,
            Self::Subheadline => 14.0,
            Self::Footnote | Self::Caption | Self::Caption2 => 13.0,
        }
    }

    pub fn painted_line_height(self) -> f32 {
        self.line_height() * base_text_size() / Self::Body.size()
    }

    pub const fn weight(self) -> FontWeight {
        match self {
            Self::Headline => FontWeight::BOLD,
            Self::Caption2 => FontWeight::MEDIUM,
            _ => FontWeight::NORMAL,
        }
    }
}

/// One role as it is set: a rung on the ladder, its leading, weight and scale.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Metrics {
    pub role: TextStyle,
    pub leading: f32,
    pub weight: FontWeight,
    pub scale: f32,
}

impl Metrics {
    pub const fn new(role: TextStyle, leading: f32, weight: FontWeight) -> Self {
        Self {
            role,
            leading,
            weight,
            scale: 1.0,
        }
    }

    pub const fn scaled(self, scale: f32) -> Self {
        Self { scale, ..self }
    }

    pub fn size(self) -> f32 {
        self.role.painted() * self.scale
    }

    pub fn line_height(self) -> f32 {
        self.size() * self.leading
    }
}

impl From<TextStyle> for Metrics {
    /// The ladder's own setting for a role: its measured leading and weight.
    fn from(role: TextStyle) -> Self {
        Self::new(role, role.line_height() / role.size(), role.weight())
    }
}

/// The ladder, on anything styled.
pub trait Typeset: Styled + Sized {
    /// Size, line height and weight together, from [`TextStyle`].
    fn text_style(self, style: TextStyle) -> Self {
        self.text_size(px(style.painted()))
            .line_height(px(style.painted_line_height()))
            .font_weight(style.weight())
    }
}

impl<E: Styled> Typeset for E {}
