use gpui::{Context, EntityInputHandler, SharedString, Window};
use ropey::RopeSlice;

use crate::input::mode::InputMode;
use crate::input::{Indent, IndentInline, InputState, Outdent, OutdentInline};

#[derive(Debug, Copy, Clone)]
pub struct TabSize {
    /// Default is 2
    pub tab_size: usize,
    /// Set true to use `\t` as tab indent, default is false
    pub hard_tabs: bool,
}

impl Default for TabSize {
    fn default() -> Self {
        Self {
            tab_size: 2,
            hard_tabs: false,
        }
    }
}

impl TabSize {
    pub(super) fn to_string(self) -> SharedString {
        if self.hard_tabs {
            "\t".into()
        } else {
            " ".repeat(self.tab_size).into()
        }
    }

    /// Count the indent size of the line in spaces.
    pub fn indent_count(&self, line: &RopeSlice) -> usize {
        let mut count = 0;
        for ch in line.chars() {
            match ch {
                '\t' => count += self.tab_size,
                ' ' => count += 1,
                _ => break,
            }
        }

        count
    }
}

impl InputState {
    /// Set the tab size for the input.
    ///
    /// Only for [`InputMode::PlainText`] mode with multi_line.
    pub fn tab_size(mut self, tab: TabSize) -> Self {
        debug_assert!(self.mode.is_multi_line());
        if let InputMode::PlainText { tab: t, .. } = &mut self.mode {
            *t = tab;
        }
        self
    }

    pub(super) fn indent_inline(
        &mut self,
        _: &IndentInline,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.indent(false, window, cx);
    }

    pub(super) fn indent_block(&mut self, _: &Indent, window: &mut Window, cx: &mut Context<Self>) {
        self.indent(true, window, cx);
    }

    pub(super) fn outdent_inline(
        &mut self,
        _: &OutdentInline,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.outdent(false, window, cx);
    }

    pub(super) fn outdent_block(
        &mut self,
        _: &Outdent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.outdent(true, window, cx);
    }

    pub(super) fn indent(&mut self, block: bool, window: &mut Window, cx: &mut Context<Self>) {
        if !self.mode.is_indentable() {
            cx.propagate();
            return;
        };

        let tab_indent = self.mode.tab_size().to_string();
        let selected_range = self.selected_range;
        let mut added_len = 0;
        let is_selected = !self.selected_range.is_empty();

        if is_selected || block {
            let start_offset = self.start_of_line_of_selection(window, cx);
            let mut offset = start_offset;

            let selected_text = self
                .text_for_range(
                    self.range_to_utf16(&(offset..selected_range.end)),
                    &mut None,
                    window,
                    cx,
                )
                .unwrap_or("".into());

            for line in selected_text.split('\n') {
                self.replace_text_in_range_silent(
                    Some(self.range_to_utf16(&(offset..offset))),
                    &tab_indent,
                    window,
                    cx,
                );
                added_len += tab_indent.len();
                // +1 for "\n", the `\r` is included in the `line`.
                offset += line.len() + tab_indent.len() + 1;
            }

            if is_selected {
                self.selected_range = (start_offset..selected_range.end + added_len).into();
            } else {
                self.selected_range =
                    (selected_range.start + added_len..selected_range.end + added_len).into();
            }
        } else {
            // Selected none
            let offset = self.selected_range.start;
            self.replace_text_in_range_silent(
                Some(self.range_to_utf16(&(offset..offset))),
                &tab_indent,
                window,
                cx,
            );
            added_len = tab_indent.len();

            self.selected_range =
                (selected_range.start + added_len..selected_range.end + added_len).into();
        }
    }

    pub(super) fn outdent(&mut self, block: bool, window: &mut Window, cx: &mut Context<Self>) {
        if !self.mode.is_indentable() {
            cx.propagate();
            return;
        };

        let tab_indent = self.mode.tab_size().to_string();
        let selected_range = self.selected_range;
        let mut removed_len = 0;
        let is_selected = !self.selected_range.is_empty();

        if is_selected || block {
            let start_offset = self.start_of_line_of_selection(window, cx);
            let mut offset = start_offset;

            let selected_text = self
                .text_for_range(
                    self.range_to_utf16(&(offset..selected_range.end)),
                    &mut None,
                    window,
                    cx,
                )
                .unwrap_or("".into());

            for line in selected_text.split('\n') {
                if line.starts_with(tab_indent.as_ref()) {
                    self.replace_text_in_range_silent(
                        Some(self.range_to_utf16(&(offset..offset + tab_indent.len()))),
                        "",
                        window,
                        cx,
                    );
                    removed_len += tab_indent.len();

                    // +1 for "\n"
                    offset += line.len().saturating_sub(tab_indent.len()) + 1;
                } else {
                    offset += line.len() + 1;
                }
            }

            if is_selected {
                self.selected_range =
                    (start_offset..selected_range.end.saturating_sub(removed_len)).into();
            } else {
                self.selected_range = (selected_range.start.saturating_sub(removed_len)
                    ..selected_range.end.saturating_sub(removed_len))
                    .into();
            }
        } else {
            // Selected none
            let start_offset = self.selected_range.start;
            let offset = self.start_of_line_of_selection(window, cx);
            let offset = self.offset_from_utf16(self.offset_to_utf16(offset));
            // FIXME: To improve performance
            if self
                .text
                .slice(offset..self.text.len())
                .to_string()
                .starts_with(tab_indent.as_ref())
            {
                self.replace_text_in_range_silent(
                    Some(self.range_to_utf16(&(offset..offset + tab_indent.len()))),
                    "",
                    window,
                    cx,
                );
                removed_len = tab_indent.len();
                let new_offset = start_offset.saturating_sub(removed_len);
                self.selected_range = (new_offset..new_offset).into();
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use ropey::RopeSlice;

    use super::TabSize;

    #[test]
    fn test_tab_size() {
        let tab = TabSize {
            tab_size: 2,
            hard_tabs: false,
        };
        assert_eq!(tab.to_string(), "  ");
        let tab = TabSize {
            tab_size: 4,
            hard_tabs: false,
        };
        assert_eq!(tab.to_string(), "    ");

        let tab = TabSize {
            tab_size: 2,
            hard_tabs: true,
        };
        assert_eq!(tab.to_string(), "\t");
        let tab = TabSize {
            tab_size: 4,
            hard_tabs: true,
        };
        assert_eq!(tab.to_string(), "\t");
    }

    #[test]
    fn test_tab_size_indent_count() {
        let tab = TabSize {
            tab_size: 4,
            hard_tabs: false,
        };
        assert_eq!(tab.indent_count(&RopeSlice::from("abc")), 0);
        assert_eq!(tab.indent_count(&RopeSlice::from("  abc")), 2);
        assert_eq!(tab.indent_count(&RopeSlice::from("    abc")), 4);
        assert_eq!(tab.indent_count(&RopeSlice::from("\tabc")), 4);
        assert_eq!(tab.indent_count(&RopeSlice::from("  \tabc")), 6);
        assert_eq!(tab.indent_count(&RopeSlice::from(" \t abc  ")), 6);
        assert_eq!(tab.indent_count(&RopeSlice::from("abc")), 0);
    }
}
