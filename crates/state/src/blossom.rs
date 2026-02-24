use std::path::PathBuf;

use anyhow::{anyhow, Error};
use gpui::AsyncApp;
use gpui_tokio::Tokio;
use mime_guess::from_path;
use nostr_blossom::prelude::*;
use nostr_sdk::prelude::*;

pub async fn upload(server: Url, path: PathBuf, cx: &AsyncApp) -> Result<Url, Error> {
    let content_type = from_path(&path).first_or_octet_stream().to_string();
    let data = smol::fs::read(path).await?;
    let keys = Keys::generate();

    // Construct the blossom client
    let client = BlossomClient::new(server);

    Tokio::spawn(cx, async move {
        let blob = client
            .upload_blob(data, Some(content_type), None, Some(&keys))
            .await?;

        Ok(blob.url)
    })
    .await
    .map_err(|e| anyhow!("Upload error: {e}"))?
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_mime_type_detection() {
        // Test common file extensions
        assert_eq!(
            from_path("image.jpg").first_or_octet_stream().to_string(),
            "image/jpeg"
        );
        assert_eq!(
            from_path("document.pdf")
                .first_or_octet_stream()
                .to_string(),
            "application/pdf"
        );
        assert_eq!(
            from_path("page.html").first_or_octet_stream().to_string(),
            "text/html"
        );
        assert_eq!(
            from_path("data.json").first_or_octet_stream().to_string(),
            "application/json"
        );
        assert_eq!(
            from_path("script.js").first_or_octet_stream().to_string(),
            "text/javascript"
        );
        assert_eq!(
            from_path("style.css").first_or_octet_stream().to_string(),
            "text/css"
        );

        // Test unknown extension falls back to octet-stream
        assert_eq!(
            from_path("unknown.xyz").first_or_octet_stream().to_string(),
            "chemical/x-xyz"
        );

        // Test no extension falls back to octet-stream
        assert_eq!(
            from_path("file_without_extension")
                .first_or_octet_stream()
                .to_string(),
            "application/octet-stream"
        );

        // Test truly unknown extension
        assert_eq!(
            from_path("unknown.unknown123")
                .first_or_octet_stream()
                .to_string(),
            "application/octet-stream"
        );
    }
}
