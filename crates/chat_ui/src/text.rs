use chat::{Mention, nevent_refs};
use gpui::{App, Entity};
use nostr_sdk::prelude::Url;
use person::Directory;
use ui::markdown::InlineReplacement;
pub use ui::markdown::RenderedText;

pub fn rendered_text(
    content: &str,
    mentions: &[Mention],
    emojis: &[(String, Url)],
    persons: &Entity<Directory>,
    cx: &App,
) -> RenderedText {
    let mut replacements = mentions
        .iter()
        .map(|mention| {
            InlineReplacement::new(
                mention.range.clone(),
                format!(
                    "@{}",
                    persons.read(cx).person(&mention.public_key, cx).name()
                ),
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
