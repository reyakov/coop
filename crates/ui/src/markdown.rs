use std::ops::Range;
use std::sync::{Arc, LazyLock};

use gpui::{
    AnyElement, App, ElementId, FontStyle, FontWeight, HighlightStyle, InteractiveText,
    IntoElement, SharedString, StrikethroughStyle, StyledText, UnderlineStyle, Window,
};
use regex::Regex;
use theme::ActiveTheme;

/// Matches `http://` and `https://` URLs. Only these are treated as clickable links.
static WEB_URL: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?i)^https?://").unwrap());

/// A span of the source content, by byte range, that renders as `text` instead.
///
/// Replacements are resolved before rendering and styled as an accent-colored
/// link. Mentions are one use: callers resolve the public key to a display name
/// and hand the renderer the span to substitute. They must be sorted by
/// `range.start` and must not overlap.
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

#[allow(clippy::enum_variant_names)]
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Highlight {
    Code,
    InlineCode(bool),
    Replacement,
    Style(HighlightStyle),
}

impl From<HighlightStyle> for Highlight {
    fn from(style: HighlightStyle) -> Self {
        Self::Style(style)
    }
}

/// Message content flattened into a single styled string.
///
/// Markdown structure (headings, lists, code blocks, emphasis, links) is turned
/// into plain text plus highlight ranges, ready to hand to an `InteractiveText`.
#[derive(Default)]
pub struct RenderedText {
    pub text: SharedString,
    pub highlights: Vec<(Range<usize>, Highlight)>,
    pub link_ranges: Vec<Range<usize>>,
    pub link_urls: Arc<[String]>,
}

impl RenderedText {
    /// Parse `content`, optionally as markdown, replacing `replacements` inline.
    pub fn new(content: &str, replacements: &[InlineReplacement], markdown: bool) -> Self {
        let mut text = String::new();
        let mut highlights = Vec::new();
        let mut link_ranges = Vec::new();
        let mut link_urls = Vec::new();

        render_text_mut(
            content,
            replacements,
            &mut text,
            &mut highlights,
            &mut link_ranges,
            &mut link_urls,
            markdown,
        );

        // Trim trailing whitespace and adjust highlight and link ranges.
        let trimmed_len = text.trim_end().len();

        // Retain highlights and link ranges that are within the trimmed text.
        if trimmed_len < text.len() {
            highlights.retain_mut(|(range, _)| {
                range.end = range.end.min(trimmed_len);
                range.start < range.end
            });

            let mut ix = 0;

            while ix < link_ranges.len() {
                let range = &mut link_ranges[ix];
                range.end = range.end.min(trimmed_len);
                if range.start < range.end {
                    ix += 1;
                } else {
                    link_ranges.remove(ix);
                    link_urls.remove(ix);
                }
            }

            text.truncate(trimmed_len);
        }

        RenderedText {
            text: SharedString::from(text),
            link_urls: link_urls.into(),
            link_ranges,
            highlights,
        }
    }

    pub fn element(&self, id: ElementId, window: &Window, cx: &App) -> AnyElement {
        let code_background = cx.theme().elevated_surface_background;
        let color = cx.theme().text_accent;
        let code_font = if cfg!(target_os = "macos") {
            "Menlo"
        } else if cfg!(target_os = "windows") {
            "Consolas"
        } else {
            "monospace"
        };

        InteractiveText::new(
            id,
            StyledText::new(self.text.clone())
                .with_default_highlights(
                    &window.text_style(),
                    self.highlights.iter().map(|(range, highlight)| {
                        (
                            range.clone(),
                            match highlight {
                                Highlight::Code => HighlightStyle {
                                    background_color: Some(code_background),
                                    ..Default::default()
                                },
                                Highlight::InlineCode(link) => {
                                    if *link {
                                        HighlightStyle {
                                            background_color: Some(code_background),
                                            underline: Some(UnderlineStyle {
                                                thickness: 1.0.into(),
                                                ..Default::default()
                                            }),
                                            ..Default::default()
                                        }
                                    } else {
                                        HighlightStyle {
                                            background_color: Some(code_background),
                                            ..Default::default()
                                        }
                                    }
                                }
                                Highlight::Replacement => HighlightStyle {
                                    color: Some(color),
                                    underline: Some(UnderlineStyle {
                                        thickness: 1.0.into(),
                                        ..Default::default()
                                    }),
                                    ..Default::default()
                                },
                                Highlight::Style(highlight) => *highlight,
                            },
                        )
                    }),
                )
                .with_font_family_overrides(self.highlights.iter().filter_map(
                    |(range, highlight)| match highlight {
                        Highlight::Code | Highlight::InlineCode(_) => {
                            Some((range.clone(), code_font.into()))
                        }
                        _ => None,
                    },
                )),
        )
        .on_click(self.link_ranges.clone(), {
            let link_urls = self.link_urls.clone();
            move |ix, _, cx| {
                let url = &link_urls[ix];
                if WEB_URL.is_match(url) {
                    cx.open_url(url);
                }
            }
        })
        .into_any_element()
    }
}

#[allow(clippy::too_many_arguments)]
fn render_text_mut(
    block: &str,
    replacements: &[InlineReplacement],
    text: &mut String,
    highlights: &mut Vec<(Range<usize>, Highlight)>,
    link_ranges: &mut Vec<Range<usize>>,
    link_urls: &mut Vec<String>,
    markdown: bool,
) {
    use pulldown_cmark::{Event, Options, Parser, Tag, TagEnd};

    let mut bold_depth = 0;
    let mut italic_depth = 0;
    let mut strikethrough_depth = 0;
    let mut link_url = None;
    let mut list_stack = Vec::new();
    let mut code_block = false;

    // Only enable the extensions that make sense for chat messages. Notably this leaves
    // out smart punctuation, tables, math and footnotes: they rewrite or swallow text.
    let events: Box<dyn Iterator<Item = (Event<'_>, Range<usize>)> + '_> = if markdown {
        Box::new(Parser::new_ext(block, Options::ENABLE_STRIKETHROUGH).into_offset_iter())
    } else {
        Box::new(std::iter::once((Event::Text(block.into()), 0..block.len())))
    };

    for (event, source_range) in events {
        let prev_len = text.len();

        match event {
            Event::Text(t) => {
                if code_block {
                    text.push_str(t.as_ref());
                    highlights.push((prev_len..text.len(), Highlight::Code));
                    continue;
                }

                let t_str = t.as_ref();
                let mut last_processed = 0;

                for replacement in replacements {
                    if replacement.range.start >= source_range.end {
                        break;
                    }

                    if replacement.range.start < source_range.start
                        || replacement.range.end > source_range.end
                    {
                        continue;
                    }

                    let Some(token) = block.get(replacement.range.clone()) else {
                        continue;
                    };

                    let Some(offset) = t_str[last_processed..].find(token) else {
                        continue;
                    };

                    let replacement_start_in_text = last_processed + offset;
                    let replacement_end_in_text = replacement_start_in_text + token.len();

                    // Add text before this replacement
                    if replacement_start_in_text > last_processed {
                        let before_replacement = &t_str[last_processed..replacement_start_in_text];
                        process_text_segment(
                            before_replacement,
                            bold_depth,
                            italic_depth,
                            strikethrough_depth,
                            link_url.clone(),
                            text,
                            highlights,
                            link_ranges,
                            link_urls,
                        );
                    }

                    // Process the replacement
                    let replacement_start = text.len();
                    text.push_str(&replacement.text);
                    let replacement_end = text.len();

                    highlights.push((replacement_start..replacement_end, Highlight::Replacement));

                    last_processed = replacement_end_in_text;
                }

                // Add any remaining text after the last replacement
                if last_processed < t_str.len() {
                    let remaining_text = &t_str[last_processed..];
                    process_text_segment(
                        remaining_text,
                        bold_depth,
                        italic_depth,
                        strikethrough_depth,
                        link_url.clone(),
                        text,
                        highlights,
                        link_ranges,
                        link_urls,
                    );
                }
            }
            Event::Code(t) => {
                text.push_str(t.as_ref());
                let is_link = link_url.is_some();

                if let Some(link_url) = link_url.clone() {
                    link_ranges.push(prev_len..text.len());
                    link_urls.push(link_url);
                }

                highlights.push((prev_len..text.len(), Highlight::InlineCode(is_link)))
            }
            Event::Start(tag) => match tag {
                Tag::Paragraph => new_paragraph(text, &mut list_stack),
                Tag::Heading { .. } => {
                    new_paragraph(text, &mut list_stack);
                    bold_depth += 1;
                }
                Tag::CodeBlock(_kind) => {
                    new_paragraph(text, &mut list_stack);
                    code_block = true;
                }
                Tag::Emphasis => italic_depth += 1,
                Tag::Strong => bold_depth += 1,
                Tag::Strikethrough => strikethrough_depth += 1,
                Tag::Link { dest_url, .. } => {
                    link_url = WEB_URL.is_match(&dest_url).then(|| dest_url.to_string());
                }
                Tag::List(number) => {
                    list_stack.push((number, false));
                }
                Tag::Item => {
                    let len = list_stack.len();
                    if let Some((list_number, has_content)) = list_stack.last_mut() {
                        *has_content = false;
                        if !text.is_empty() && !text.ends_with('\n') {
                            text.push('\n');
                        }
                        for _ in 0..len - 1 {
                            text.push_str("  ");
                        }
                        if let Some(number) = list_number {
                            text.push_str(&format!("{}. ", number));
                            *number += 1;
                            *has_content = false;
                        } else {
                            text.push_str("- ");
                        }
                    }
                }
                _ => {}
            },
            Event::End(tag) => match tag {
                TagEnd::CodeBlock => code_block = false,
                TagEnd::Heading(_) => bold_depth -= 1,
                TagEnd::Emphasis => italic_depth -= 1,
                TagEnd::Strong => bold_depth -= 1,
                TagEnd::Strikethrough => strikethrough_depth -= 1,
                TagEnd::Link => link_url = None,
                TagEnd::List(_) => drop(list_stack.pop()),
                _ => {}
            },
            Event::Html(t) | Event::InlineHtml(t) => text.push_str(t.as_ref()),
            Event::Rule => {
                new_paragraph(text, &mut list_stack);
                text.push_str("────────\n");
            }
            Event::HardBreak => text.push('\n'),
            Event::SoftBreak => text.push('\n'),
            _ => {}
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn process_text_segment(
    segment: &str,
    bold_depth: i32,
    italic_depth: i32,
    strikethrough_depth: i32,
    link_url: Option<String>,
    text: &mut String,
    highlights: &mut Vec<(Range<usize>, Highlight)>,
    link_ranges: &mut Vec<Range<usize>>,
    link_urls: &mut Vec<String>,
) {
    // Build the style for this segment
    let mut style = HighlightStyle::default();
    if bold_depth > 0 {
        style.font_weight = Some(FontWeight::BOLD);
    }
    if italic_depth > 0 {
        style.font_style = Some(FontStyle::Italic);
    }
    if strikethrough_depth > 0 {
        style.strikethrough = Some(StrikethroughStyle {
            thickness: 1.0.into(),
            ..Default::default()
        });
    }

    // Ranges always refer to the rendered text, including replaced spans.
    let segment_start = text.len();
    text.push_str(segment);
    let text_end = text.len();

    if let Some(link_url) = link_url {
        // Handle as a markdown link
        link_ranges.push(segment_start..text_end);
        link_urls.push(link_url);
        style.underline = Some(UnderlineStyle {
            thickness: 1.0.into(),
            ..Default::default()
        });

        // Add highlight for the entire linked segment
        if style != HighlightStyle::default() {
            highlights.push((segment_start..text_end, Highlight::Style(style)));
        }
    } else {
        // Handle link detection within the segment
        let mut finder = linkify::LinkFinder::new();
        finder.kinds(&[linkify::LinkKind::Url]);
        let mut last_link_pos = 0;

        for link in finder
            .links(segment)
            .filter(|link| WEB_URL.is_match(link.as_str()))
        {
            let start = link.start();
            let end = link.end();

            // Add non-link text before this link
            if start > last_link_pos {
                let non_link_start = segment_start + last_link_pos;
                let non_link_end = segment_start + start;

                if style != HighlightStyle::default() {
                    highlights.push((non_link_start..non_link_end, Highlight::Style(style)));
                }
            }

            // Add the link
            let range = (segment_start + start)..(segment_start + end);
            link_ranges.push(range.clone());
            link_urls.push(link.as_str().to_string());

            // Apply link styling (underline + existing style)
            let mut link_style = style;
            link_style.underline = Some(UnderlineStyle {
                thickness: 1.0.into(),
                ..Default::default()
            });

            highlights.push((range, Highlight::Style(link_style)));

            last_link_pos = end;
        }

        // Add any remaining text after the last link
        if last_link_pos < segment.len() {
            let remaining_start = segment_start + last_link_pos;
            let remaining_end = segment_start + segment.len();

            if style != HighlightStyle::default() {
                highlights.push((remaining_start..remaining_end, Highlight::Style(style)));
            }
        }
    }
}

fn new_paragraph(text: &mut String, list_stack: &mut [(Option<u64>, bool)]) {
    let mut is_subsequent_paragraph_of_list = false;

    if let Some((_, has_content)) = list_stack.last_mut() {
        if *has_content {
            is_subsequent_paragraph_of_list = true;
        } else {
            *has_content = true;
            return;
        }
    }

    if !text.is_empty() {
        if !text.ends_with('\n') {
            text.push('\n');
        }
        text.push('\n');
    }

    for _ in 0..list_stack.len().saturating_sub(1) {
        text.push_str("  ");
    }

    if is_subsequent_paragraph_of_list {
        text.push_str("  ");
    }
}
