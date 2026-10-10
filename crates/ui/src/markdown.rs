use std::ops::Range;

use gpui::{
    AnyElement, App, ElementId, HighlightStyle, IntoElement, SharedString, StyleRefinement,
    Styled as _, Window,
};
use gpui_base::{TextView, TextViewStyle};
use theme::ActiveTheme as _;

/// A span of the source content, by byte range, that renders as `text` instead.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InlineReplacement {
    pub range: Range<usize>,
    pub text: SharedString,
}

impl InlineReplacement {
    pub fn new(range: Range<usize>, text: impl Into<SharedString>) -> Self {
        Self {
            range,
            text: text.into(),
        }
    }
}

/// Message content prepared for rendering as a GPUI Kit text view.
#[derive(Default)]
pub struct RenderedText {
    text: SharedString,
}

impl RenderedText {
    pub fn new(content: &str, replacements: &[InlineReplacement]) -> Self {
        let mut text = String::with_capacity(content.len());
        let mut last = 0;

        for replacement in replacements {
            let range = &replacement.range;
            if range.start < last
                || range.end > content.len()
                || range.start >= range.end
                || !content.is_char_boundary(range.start)
                || !content.is_char_boundary(range.end)
            {
                continue;
            }

            text.push_str(&content[last..range.start]);
            text.push_str(&format!(
                "[{}](<{}>)",
                escape_link_text(&replacement.text),
                &content[range.start..range.end]
            ));
            last = range.end;
        }
        text.push_str(&content[last..]);

        Self {
            text: SharedString::from(text),
        }
    }

    /// Render the message as a GPUI Kit text view.
    pub fn element(&self, id: ElementId, _window: &Window, cx: &App) -> AnyElement {
        let theme = cx.theme();
        let code_block = StyleRefinement::default().font_family(theme.font_mono.clone());
        let muted_heading = theme.text_muted;

        TextView::markdown(id, self.text.clone())
            .style(
                TextViewStyle::default()
                    .with_foreground(theme.text)
                    .with_muted_foreground(theme.text_muted)
                    // Deeply nested headings read as secondary text.
                    .with_heading(move |level| {
                        if level >= 4 {
                            StyleRefinement::default().text_color(muted_heading)
                        } else {
                            StyleRefinement::default()
                        }
                    })
                    .with_link(theme.accent)
                    .with_code_background(theme.code_wash)
                    .with_inline_code(HighlightStyle {
                        color: Some(theme.code_text),
                        ..Default::default()
                    })
                    .with_border(theme.border)
                    .with_code_block(code_block)
                    .with_dark(theme.is_dark()),
            )
            .on_link_click(|url, _, _, cx| {
                if is_web_url(url) {
                    cx.open_url(url);
                }
            })
            .into_any_element()
    }
}

/// Escapes the characters that would terminate a markdown link label.
fn escape_link_text(text: &str) -> String {
    let mut escaped = String::with_capacity(text.len());
    for character in text.chars() {
        if matches!(character, '[' | ']' | '\\') {
            escaped.push('\\');
        }
        escaped.push(character);
    }
    escaped
}

/// Matches `http://` and `https://` URLs. Only these are treated as clickable links.
fn is_web_url(url: &str) -> bool {
    let url = url.to_ascii_lowercase();
    url.starts_with("http://") || url.starts_with("https://")
}
