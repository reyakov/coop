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
