use std::ops::Range;

use gpui::{Context, Window};
use ropey::Rope;
use sum_tree::Bias;

use crate::input::{InputState, RopeExt};

impl InputState {
    /// Select the word at the given offset on double-click.
    ///
    /// The offset is the UTF-8 offset.
    pub(super) fn select_word(&mut self, offset: usize, _: &mut Window, cx: &mut Context<Self>) {
        let Some(range) = TextSelector::word_range(&self.text, offset) else {
            return;
        };

        self.selected_range = (range.start..range.end).into();
        self.selected_word_range = Some(self.selected_range);
        cx.notify()
    }

    /// Select the line at the given offset on triple-click.
    ///
    /// The offset is the UTF-8 offset.
    pub(super) fn select_line(&mut self, offset: usize, _: &mut Window, cx: &mut Context<Self>) {
        let range = TextSelector::line_range(&self.text, offset);
        self.selected_range = (range.start..range.end).into();
        self.selected_word_range = None;
        cx.notify()
    }
}

struct TextSelector;
impl TextSelector {
    /// Select a line in the given text at the specified offset.
    ///
    /// The offset is the UTF-8 offset.
    ///
    /// Returns the start and end offsets of the selected line.
    pub fn line_range(text: &Rope, offset: usize) -> Range<usize> {
        let offset = text.clip_offset(offset, Bias::Left);
        let row = text.offset_to_point(offset).row;
        let start = text.line_start_offset(row);
        let end = text.line_end_offset(row);

        start..end
    }

    /// Select a word in the given text at the specified offset.
    ///
    /// The offset is the UTF-8 offset.
    ///
    /// Returns the start and end offsets of the selected word.
    pub fn word_range(text: &Rope, offset: usize) -> Option<Range<usize>> {
        let offset = text.clip_offset(offset, Bias::Left);
        let char = text.char_at(offset)?;
        let end = offset + char.len_utf8();
        let prev_chars = text.chars_at(offset).reversed().take(128);
        let next_chars = text.chars_at(end).take(128);

        Some(word_range_from_chars(offset, char, prev_chars, next_chars))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CharType {
    /// a-z, A-Z, 0-9, _
    Word,
    /// '\t', ' ', '\u{00A0}' etc.
    Whitespace,
    /// \n, \r
    Newline,
    /// . , ; : ( ) [ ] { } ... or CJK characters: `汉`, `🎉` etc.
    Other,
}

impl From<char> for CharType {
    fn from(c: char) -> Self {
        match c {
            c if is_word_char(c) => CharType::Word,
            c if c == '\n' || c == '\r' => CharType::Newline,
            c if c.is_whitespace() => CharType::Whitespace,
            _ => CharType::Other,
        }
    }
}

impl CharType {
    fn is_connectable(self, c: char) -> bool {
        matches!(
            (self, CharType::from(c)),
            (CharType::Word, CharType::Word) | (CharType::Whitespace, CharType::Whitespace)
        )
    }
}

fn is_word_char(c: char) -> bool {
    matches!(c, '_')
        // ASCII alphanumeric characters, for English, numbers: `Hello123`, etc.
        || c.is_ascii_alphanumeric()
        // Latin script in Unicode for French, German, Spanish, etc.
        || matches!(c, '\u{00C0}'..='\u{00FF}')
        || matches!(c, '\u{0100}'..='\u{017F}')
        || matches!(c, '\u{0180}'..='\u{024F}')
        // Cyrillic for Russian, Ukrainian, etc.
        || matches!(c, '\u{0400}'..='\u{04FF}')
        // Vietnamese
        || matches!(c, '\u{1E00}'..='\u{1EFF}')
        || matches!(c, '\u{0300}'..='\u{036F}')
}

pub(crate) fn word_range_from_chars(
    offset: usize,
    c: char,
    prev_chars: impl Iterator<Item = char>,
    next_chars: impl Iterator<Item = char>,
) -> Range<usize> {
    let char_type = CharType::from(c);
    let mut start = offset;
    let mut end = offset + c.len_utf8();

    for prev in prev_chars.take(128) {
        if char_type.is_connectable(prev) {
            start -= prev.len_utf8();
        } else {
            break;
        }
    }

    for next in next_chars.take(128) {
        if char_type.is_connectable(next) {
            end += next.len_utf8();
        } else {
            break;
        }
    }

    start..end
}
