use std::sync::LazyLock;

use gpui::SharedUri;
use regex::Regex;

/// Matches image URLs with common image extensions
static IMAGE_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r#"(?i)\bhttps?://[^\s<>"']+\.(?:jpg|jpeg|png|gif|bmp|webp|svg|ico)(?:\?[^\s<>"']*)?\b"#,
    )
    .unwrap()
});

/// Extracts media URLs from a string and returns the URLs and the cleaned text
struct MediaExtractor;

impl MediaExtractor {
    /// Extracts all media URLs from a string
    fn extract_media_urls(text: &str) -> Vec<SharedUri> {
        IMAGE_RE
            .find_iter(text)
            .map(|capture| capture.as_str().to_string().into())
            .collect()
    }

    /// Removes all media URLs from a string and returns the cleaned text
    fn remove_media_urls(text: &str) -> String {
        let mut cleaned = String::with_capacity(text.len());
        let mut last_end = 0;
        // Whitespace bordered a removed URL, so one space stands in for it
        let mut spaced = false;

        for url in IMAGE_RE.find_iter(text) {
            let raw_gap = &text[last_end..url.start()];
            let surrounded = raw_gap.ends_with(char::is_whitespace)
                || text[url.end()..].starts_with(char::is_whitespace);

            let gap = if last_end == 0 {
                raw_gap.trim_end()
            } else {
                raw_gap.trim()
            };

            if !gap.is_empty() {
                if spaced && !cleaned.is_empty() {
                    cleaned.push(' ');
                }
                cleaned.push_str(gap);
            }

            spaced |= surrounded;
            last_end = url.end();
        }

        let tail = text[last_end..].trim_start();
        if spaced && !cleaned.is_empty() && !tail.is_empty() {
            cleaned.push(' ');
        }
        cleaned.push_str(tail);

        cleaned.trim().to_string()
    }

    /// Extracts media URLs and removes them from the string, returning both
    fn extract_and_remove(text: &str) -> (Vec<SharedUri>, String) {
        (
            Self::extract_media_urls(text),
            Self::remove_media_urls(text),
        )
    }
}

/// Convenience function for one-time extraction and removal
pub fn extract_and_remove_media_urls(text: &str) -> (Vec<SharedUri>, String) {
    MediaExtractor::extract_and_remove(text)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn multi_line_text_without_urls_keeps_line_breaks() {
        let text = "first line\n\nsecond line";
        let (urls, cleaned) = extract_and_remove_media_urls(text);

        assert!(urls.is_empty());
        assert_eq!(cleaned, text);
    }

    #[test]
    fn removes_url_and_tidies_around_it() {
        let (urls, cleaned) =
            extract_and_remove_media_urls("check https://example.com/pic.png here");

        assert_eq!(urls, vec![SharedUri::from("https://example.com/pic.png")]);
        assert_eq!(cleaned, "check here");
    }

    #[test]
    fn removing_url_keeps_other_line_breaks() {
        let (urls, cleaned) = extract_and_remove_media_urls(
            "first\nsecond https://example.com/pic.png third\nfourth",
        );

        assert_eq!(urls.len(), 1);
        assert_eq!(cleaned, "first\nsecond third\nfourth");
    }

    #[test]
    fn url_on_its_own_line_joins_neighbors_with_one_space() {
        let (urls, cleaned) =
            extract_and_remove_media_urls("first\n\nhttps://example.com/pic.png\n\nlast");

        assert_eq!(urls.len(), 1);
        assert_eq!(cleaned, "first last");
    }

    #[test]
    fn multiple_urls_collapse_to_single_spaces() {
        let (urls, cleaned) = extract_and_remove_media_urls(
            "a https://example.com/a.png b https://example.com/b.png c",
        );

        assert_eq!(urls.len(), 2);
        assert_eq!(cleaned, "a b c");
    }

    #[test]
    fn message_with_only_a_url_becomes_empty() {
        let (urls, cleaned) = extract_and_remove_media_urls("https://example.com/pic.png");

        assert_eq!(urls.len(), 1);
        assert_eq!(cleaned, "");
    }

    #[test]
    fn digits_glued_to_url_scheme_prevent_match() {
        let text = "v1.2https://example.com/pic.png";
        let (urls, cleaned) = extract_and_remove_media_urls(text);

        assert!(urls.is_empty());
        assert_eq!(cleaned, text);
    }

    #[test]
    fn digits_around_removed_url_survive() {
        let (urls, cleaned) = extract_and_remove_media_urls("123 https://example.com/pic.png 456");

        assert_eq!(urls.len(), 1);
        assert_eq!(cleaned, "123 456");
    }

    #[test]
    fn emoji_around_removed_url_survives() {
        let (urls, cleaned) = extract_and_remove_media_urls("👍 https://example.com/pic.png 👍");

        assert_eq!(urls.len(), 1);
        assert_eq!(cleaned, "👍 👍");
    }
}
