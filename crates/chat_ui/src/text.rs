use chat::Mention;
use gpui::{App, Entity};
use nostr_sdk::prelude::Url;
use person::PersonRegistry;
use ui::markdown::InlineReplacement;
pub use ui::markdown::RenderedText;

pub fn rendered_text(
    content: &str,
    mentions: &[Mention],
    emojis: &[(String, Url)],
    persons: &Entity<PersonRegistry>,
    cx: &App,
) -> RenderedText {
    let mut replacements = mentions
        .iter()
        .map(|mention| {
            InlineReplacement::new(
                mention.range.clone(),
                format!("@{}", persons.read(cx).get(&mention.public_key, cx).name()),
            )
        })
        .collect::<Vec<_>>();

    replacements.extend(emoji_replacements(content, emojis));
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

#[cfg(test)]
mod tests {
    use super::*;

    fn emoji(shortcode: &str, url: &str) -> (String, Url) {
        (String::from(shortcode), Url::parse(url).expect("valid url"))
    }

    #[test]
    fn declared_shortcodes_become_image_replacements() {
        let emojis = vec![emoji("soapbox", "https://example.com/soapbox.png")];

        let replacements = emoji_replacements("hi :soapbox: and :other:", &emojis);

        assert_eq!(replacements.len(), 1);
        assert_eq!(replacements[0].range, 3..12);
    }

    #[test]
    fn matches_glued_to_word_characters_are_skipped() {
        let emojis = vec![emoji("30", "https://example.com/30.png")];

        assert!(emoji_replacements("at 12:30:45 exactly", &emojis).is_empty());
        assert!(emoji_replacements("x:30: and :30:y", &emojis).is_empty());
        assert_eq!(emoji_replacements("(:30:)", &emojis).len(), 1);
    }
}
