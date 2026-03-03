use std::ops::Range;
use std::sync::Arc;

use chat::Mention;
use common::RangeExt;
use gpui::{
    AnyElement, App, ElementId, Entity, FontStyle, FontWeight, HighlightStyle, InteractiveText,
    IntoElement, SharedString, StrikethroughStyle, StyledText, UnderlineStyle, Window,
};
use person::PersonRegistry;
use theme::ActiveTheme;

#[allow(clippy::enum_variant_names)]
#[allow(dead_code)]
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Highlight {
    Code,
    InlineCode(bool),
    Highlight(HighlightStyle),
    Mention,
}

impl From<HighlightStyle> for Highlight {
    fn from(style: HighlightStyle) -> Self {
        Self::Highlight(style)
    }
}

#[derive(Default)]
pub struct RenderedText {
    pub text: SharedString,
    pub highlights: Vec<(Range<usize>, Highlight)>,
    pub link_ranges: Vec<Range<usize>>,
    pub link_urls: Arc<[String]>,
}

impl RenderedText {
    pub fn new(
        content: &str,
        mentions: &[Mention],
        persons: &Entity<PersonRegistry>,
        cx: &App,
    ) -> Self {
        let mut text = String::new();
        let mut highlights = Vec::new();
        let mut link_ranges = Vec::new();
        let mut link_urls = Vec::new();

        render_plain_text_mut(
            content,
            mentions,
            &mut text,
            &mut highlights,
            &mut link_ranges,
            &mut link_urls,
            persons,
            cx,
        );

        text.truncate(text.trim_end().len());

        RenderedText {
            text: SharedString::from(text),
            link_urls: link_urls.into(),
            link_ranges,
            highlights,
        }
    }

    pub fn element(&self, id: ElementId, window: &Window, cx: &App) -> AnyElement {
        let code_background = cx.theme().elevated_surface_background;

        InteractiveText::new(
            id,
            StyledText::new(self.text.clone()).with_default_highlights(
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
                            Highlight::Mention => HighlightStyle {
                                underline: Some(UnderlineStyle {
                                    thickness: 1.0.into(),
                                    ..Default::default()
                                }),
                                ..Default::default()
                            },
                            Highlight::Highlight(highlight) => *highlight,
                        },
                    )
                }),
            ),
        )
        .on_click(self.link_ranges.clone(), {
            let link_urls = self.link_urls.clone();
            move |ix, _, cx| {
                let url = &link_urls[ix];
                if url.starts_with("http") {
                    cx.open_url(url);
                }
            }
        })
        .into_any_element()
    }
}

#[allow(clippy::too_many_arguments)]
fn render_plain_text_mut(
    block: &str,
    mut mentions: &[Mention],
    text: &mut String,
    highlights: &mut Vec<(Range<usize>, Highlight)>,
    link_ranges: &mut Vec<Range<usize>>,
    link_urls: &mut Vec<String>,
    persons: &Entity<PersonRegistry>,
    cx: &App,
) {
    use pulldown_cmark::{Event, Options, Parser, Tag, TagEnd};

    let mut bold_depth = 0;
    let mut italic_depth = 0;
    let mut strikethrough_depth = 0;
    let mut link_url = None;
    let mut list_stack = Vec::new();

    let mut options = Options::all();
    options.remove(pulldown_cmark::Options::ENABLE_DEFINITION_LIST);

    for (event, source_range) in Parser::new_ext(block, options).into_offset_iter() {
        let prev_len = text.len();

        match event {
            Event::Text(t) => {
                // Process text with mention replacements
                let t_str = t.as_ref();
                let mut last_processed = 0;

                while let Some(mention) = mentions.first() {
                    if !source_range.contains_inclusive(&mention.range) {
                        break;
                    }

                    // Calculate positions within the current text
                    let mention_start_in_text = mention.range.start - source_range.start;
                    let mention_end_in_text = mention.range.end - source_range.start;

                    // Add text before this mention
                    if mention_start_in_text > last_processed {
                        let before_mention = &t_str[last_processed..mention_start_in_text];
                        process_text_segment(
                            before_mention,
                            prev_len + last_processed,
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

                    // Process the mention replacement
                    let profile = persons.read(cx).get(&mention.public_key, cx);
                    let replacement_text = format!("@{}", profile.name());

                    let replacement_start = text.len();
                    text.push_str(&replacement_text);
                    let replacement_end = text.len();

                    highlights.push((replacement_start..replacement_end, Highlight::Mention));

                    last_processed = mention_end_in_text;
                    mentions = &mentions[1..];
                }

                // Add any remaining text after the last mention
                if last_processed < t_str.len() {
                    let remaining_text = &t_str[last_processed..];
                    process_text_segment(
                        remaining_text,
                        prev_len + last_processed,
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
                }
                Tag::Emphasis => italic_depth += 1,
                Tag::Strong => bold_depth += 1,
                Tag::Strikethrough => strikethrough_depth += 1,
                Tag::Link { dest_url, .. } => link_url = Some(dest_url.to_string()),
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
                TagEnd::Heading(_) => bold_depth -= 1,
                TagEnd::Emphasis => italic_depth -= 1,
                TagEnd::Strong => bold_depth -= 1,
                TagEnd::Strikethrough => strikethrough_depth -= 1,
                TagEnd::Link => link_url = None,
                TagEnd::List(_) => drop(list_stack.pop()),
                _ => {}
            },
            Event::HardBreak => text.push('\n'),
            Event::SoftBreak => text.push('\n'),
            _ => {}
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn process_text_segment(
    segment: &str,
    segment_start: usize,
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

    // Add the text
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
            highlights.push((segment_start..text_end, Highlight::Highlight(style)));
        }
    } else {
        // Handle link detection within the segment
        let mut finder = linkify::LinkFinder::new();
        finder.kinds(&[linkify::LinkKind::Url]);
        let mut last_link_pos = 0;

        for link in finder.links(segment) {
            let start = link.start();
            let end = link.end();

            // Add non-link text before this link
            if start > last_link_pos {
                let non_link_start = segment_start + last_link_pos;
                let non_link_end = segment_start + start;

                if style != HighlightStyle::default() {
                    highlights.push((non_link_start..non_link_end, Highlight::Highlight(style)));
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

            highlights.push((range, Highlight::Highlight(link_style)));

            last_link_pos = end;
        }

        // Add any remaining text after the last link
        if last_link_pos < segment.len() {
            let remaining_start = segment_start + last_link_pos;
            let remaining_end = segment_start + segment.len();

            if style != HighlightStyle::default() {
                highlights.push((remaining_start..remaining_end, Highlight::Highlight(style)));
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
