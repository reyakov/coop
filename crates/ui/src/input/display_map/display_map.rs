use std::ops::Range;

use gpui::{App, Font, Pixels};
use ropey::Rope;

use super::text_wrapper::{LineItem, WrapDisplayPoint};
use super::wrap_map::WrapMap;
use crate::input::Point as TreeSitterPoint;

/// DisplayMap is the main interface for Input coordinate mapping.
pub struct DisplayMap {
    wrap_map: WrapMap,
}

impl DisplayMap {
    pub fn new(font: Font, font_size: Pixels, wrap_width: Option<Pixels>) -> Self {
        Self {
            wrap_map: WrapMap::new(font, font_size, wrap_width),
        }
    }

    /// Get total number of display rows (same as wrap rows without folding)
    #[inline]
    pub fn display_row_count(&self) -> usize {
        self.wrap_map.wrap_row_count()
    }

    /// Get the buffer line for a given display row
    pub fn display_row_to_buffer_line(&self, display_row: usize) -> usize {
        self.wrap_map.wrap_row_to_buffer_line(display_row)
    }

    /// Get the display row range for a buffer line: [start, end)
    pub fn buffer_line_to_display_row_range(&self, line: usize) -> Option<Range<usize>> {
        let range = self.wrap_map.buffer_line_to_wrap_row_range(line);
        if range.is_empty() { None } else { Some(range) }
    }

    /// Check if a buffer line is completely hidden (never true without folding)
    #[inline]
    pub fn is_buffer_line_hidden(&self, _line: usize) -> bool {
        false
    }

    /// All wrap rows are visible since there's no folding.
    #[inline]
    pub fn folded_ranges(&self) -> &[()] {
        &[]
    }

    /// Adjust folds for edit (no-op without folding)
    pub fn adjust_folds_for_edit(
        &mut self,
        _old_text: &Rope,
        _range: &Range<usize>,
        _new_text: &str,
    ) {
        // No-op: no folding
    }

    /// Update text (incremental or full)
    pub fn on_text_changed(
        &mut self,
        changed_text: &Rope,
        range: &Range<usize>,
        new_text: &Rope,
        cx: &mut App,
    ) {
        self.wrap_map
            .on_text_changed(changed_text, range, new_text, cx);
    }

    /// Update layout parameters (wrap width or font)
    pub fn on_layout_changed(&mut self, wrap_width: Option<Pixels>, cx: &mut App) {
        self.wrap_map.on_layout_changed(wrap_width, cx);
    }

    /// Set font parameters
    pub fn set_font(&mut self, font: Font, font_size: Pixels, cx: &mut App) {
        self.wrap_map.set_font(font, font_size, cx);
    }

    /// Ensure text is prepared (initializes wrapper if needed)
    pub fn ensure_text_prepared(&mut self, text: &Rope, cx: &mut App) {
        self.wrap_map.ensure_text_prepared(text, cx);
    }

    /// Initialize with text
    pub fn set_text(&mut self, text: &Rope, cx: &mut App) {
        self.wrap_map.set_text(text, cx);
    }

    /// Convert byte offset to wrap display point (with soft wrap info).
    #[inline]
    pub(crate) fn offset_to_wrap_display_point(&self, offset: usize) -> WrapDisplayPoint {
        self.wrap_map.wrapper().offset_to_display_point(offset)
    }

    /// Convert wrap display point to byte offset.
    #[inline]
    pub(crate) fn wrap_display_point_to_offset(&self, point: WrapDisplayPoint) -> usize {
        self.wrap_map.wrapper().display_point_to_offset(point)
    }

    /// Convert wrap display point to TreeSitterPoint (buffer line/col).
    #[inline]
    pub(crate) fn wrap_display_point_to_point(&self, point: WrapDisplayPoint) -> TreeSitterPoint {
        self.wrap_map.wrapper().display_point_to_point(point)
    }

    /// Since there's no folding, wrap row == display row.
    #[inline]
    pub fn wrap_row_to_display_row(&self, wrap_row: usize) -> Option<usize> {
        if wrap_row < self.wrap_row_count() {
            Some(wrap_row)
        } else {
            None
        }
    }

    /// Since there's no folding, nearest visible row is the row itself.
    #[inline]
    pub fn nearest_visible_display_row(&self, wrap_row: usize) -> usize {
        wrap_row.min(self.wrap_row_count().saturating_sub(1))
    }

    /// Since there's no folding, display row == wrap row.
    #[inline]
    pub fn display_row_to_wrap_row(&self, display_row: usize) -> Option<usize> {
        if display_row < self.wrap_row_count() {
            Some(display_row)
        } else {
            None
        }
    }

    /// Get the longest row index (by byte length).
    #[inline]
    pub(crate) fn longest_row(&self) -> usize {
        self.wrap_map.wrapper().longest_row.row
    }

    /// Get access to line items (for rendering)
    #[inline]
    pub(crate) fn lines(&self) -> &[LineItem] {
        self.wrap_map.lines()
    }

    /// Get the rope text
    #[inline]
    pub fn text(&self) -> &Rope {
        self.wrap_map.text()
    }

    /// Calculate how many wrap rows of a buffer line are visible
    #[inline]
    pub fn visible_wrap_row_count_for_buffer_line(&self, line: usize) -> usize {
        self.wrap_map.visible_wrap_row_count_for_buffer_line(line)
    }

    /// Get the wrap row count
    #[inline]
    pub fn wrap_row_count(&self) -> usize {
        self.wrap_map.wrap_row_count()
    }

    /// Get the buffer line count (logical lines)
    #[inline]
    pub fn buffer_line_count(&self) -> usize {
        self.wrap_map.buffer_line_count()
    }
}
