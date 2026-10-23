use std::ops::Range;

use common::nevent_refs;
use gpui::{AnyElement, App, ElementId, Entity, IntoElement as _, SharedString, SharedUri, Window};
use gpui_kit::component::text::TextView;
use nostr_sdk::prelude::{PublicKey, Url};
use person::Directory;

/// A span of the source content, by byte range, that renders as `text` instead.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InlineReplacement {
    pub range: Range<usize>,
    pub text: SharedString,
    /// When set, the span renders as this image (NIP-30 custom emoji)
    pub image: Option<SharedUri>,
}

impl InlineReplacement {
    pub fn new(range: Range<usize>, text: impl Into<SharedString>) -> Self {
        Self {
            range,
            text: text.into(),
            image: None,
        }
    }

    pub fn image(
        range: Range<usize>,
        alt: impl Into<SharedString>,
        url: impl Into<SharedUri>,
    ) -> Self {
        Self {
            range,
            text: alt.into(),
            image: Some(url.into()),
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
            match &replacement.image {
                Some(url) => text.push_str(&format!(
                    "![{}](<{url}>)",
                    escape_link_text(&replacement.text)
                )),
                None => text.push_str(&format!(
                    "[{}](<{}>)",
                    escape_link_text(&replacement.text),
                    &content[range.start..range.end]
                )),
            }
            last = range.end;
        }
        text.push_str(&content[last..]);

        Self {
            text: SharedString::from(text),
        }
    }

    /// Render the message as a GPUI Kit text view.
    ///
    /// The framework Root installs a theme-derived base style for text views,
    /// so only the Nostr link handling is customized here.
    pub fn element(&self, id: ElementId, _window: &Window, _cx: &App) -> AnyElement {
        TextView::markdown(id, self.text.clone())
            .on_link_click(|url, _, _, cx| {
                if is_web_url(url) {
                    cx.open_url(url);
                } else if let Some(bech32) = nevent_bech32(url) {
                    cx.open_url(&format!("https://njump.me/{bech32}"));
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

/// Extracts the bech32 from a NIP-19 event reference (`nevent1...` or `nostr:nevent1...`).
fn nevent_bech32(url: &str) -> Option<&str> {
    let bech32 = url.strip_prefix("nostr:").unwrap_or(url);
    bech32.starts_with("nevent1").then_some(bech32)
}

/// Composes a message's rendered text: mention names, NIP-30 emojis, and nevent links.
///
/// `mentions` are the `(byte range, mentioned public key)` pairs found in `content`.
pub fn rendered_text(
    content: &str,
    mentions: impl IntoIterator<Item = (Range<usize>, PublicKey)>,
    emojis: &[(String, Url)],
    persons: &Entity<Directory>,
    cx: &App,
) -> RenderedText {
    let mut replacements = mentions
        .into_iter()
        .map(|(range, public_key)| {
            InlineReplacement::new(
                range,
                format!("@{}", persons.read(cx).person(&public_key, cx).name()),
            )
        })
        .collect::<Vec<_>>();

    replacements.extend(emoji_replacements(content, emojis));
    replacements.extend(
        nevent_refs(content)
            .into_iter()
            .map(|reference| InlineReplacement::new(reference.range, reference.label)),
    );
    replacements.sort_by_key(|replacement| replacement.range.start);

    RenderedText::new(content, &replacements)
}

fn emoji_replacements(content: &str, emojis: &[(String, Url)]) -> Vec<InlineReplacement> {
    let mut replacements = Vec::new();

    for (shortcode, url) in emojis {
        let pattern = format!(":{shortcode}:");
        for (start, _) in content.match_indices(&pattern) {
            let range = start..start + pattern.len();

            // Skip matches glued to word characters, like the `30` in `12:30:45`
            if content[..start].ends_with(char::is_alphanumeric)
                || content[range.end..].starts_with(char::is_alphanumeric)
            {
                continue;
            }

            replacements.push(InlineReplacement::image(
                range,
                shortcode.clone(),
                url.to_string(),
            ));
        }
    }

    replacements
}
