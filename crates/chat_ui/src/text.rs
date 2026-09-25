use chat::Mention;
use gpui::{App, Entity};
use person::PersonRegistry;
use ui::markdown::InlineReplacement;
pub use ui::markdown::RenderedText;

/// Render message `content` to text, replacing mentions with their display names.
///
/// When `markdown` is set the content is parsed as markdown.
pub fn rendered_text(
    content: &str,
    mentions: &[Mention],
    persons: &Entity<PersonRegistry>,
    markdown: bool,
    cx: &App,
) -> RenderedText {
    let replacements = mentions
        .iter()
        .map(|mention| {
            InlineReplacement::new(
                mention.range.clone(),
                format!("@{}", persons.read(cx).get(&mention.public_key, cx).name()),
            )
        })
        .collect::<Vec<_>>();

    RenderedText::new(content, &replacements, markdown)
}
