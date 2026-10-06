use gpui::SharedUri;
use regex::Regex;

/// Extracts media URLs from a string and returns both
/// the extracted URLs and the string with media URLs removed
struct MediaExtractor {
    image_regex: Regex,
}

impl MediaExtractor {
    /// Creates a new MediaExtractor with compiled regex patterns
    fn new() -> Self {
        MediaExtractor {
            // Match common image extensions
            image_regex: Regex::new(
                r#"(?i)\bhttps?://[^\s<>"']+\.(?:jpg|jpeg|png|gif|bmp|webp|svg|ico)(?:\?[^\s<>"']*)?\b"#,
            ).unwrap(),
        }
    }

    /// Extracts all media URLs from a string
    fn extract_media_urls(&self, text: &str) -> Vec<SharedUri> {
        let mut urls = Vec::new();

        // Extract image URLs
        for capture in self.image_regex.find_iter(text) {
            urls.push(capture.as_str().to_string().into());
        }

        urls
    }

    /// Removes all media URLs from a string and returns the cleaned text
    fn remove_media_urls(&self, text: &str) -> String {
        let mut result = text.to_string();

        // Remove image URLs
        result = self.image_regex.replace_all(&result, "").to_string();

        // Clean up extra whitespace that might result from removal
        self.cleanup_text(&result)
    }

    /// Extracts media URLs and removes them from the string, returning both
    fn extract_and_remove(&self, text: &str) -> (Vec<SharedUri>, String) {
        let urls = self.extract_media_urls(text);
        let cleaned_text = self.remove_media_urls(text);
        (urls, cleaned_text)
    }

    /// Helper function to clean up text after URL removal
    fn cleanup_text(&self, text: &str) -> String {
        let text = text.trim();

        // Remove multiple consecutive spaces
        let re = Regex::new(r"\s+").unwrap();
        re.replace_all(text, " ").trim().to_string()
    }
}

/// Convenience function for one-time extraction and removal
pub fn extract_and_remove_media_urls(text: &str) -> (Vec<SharedUri>, String) {
    let extractor = MediaExtractor::new();
    extractor.extract_and_remove(text)
}
