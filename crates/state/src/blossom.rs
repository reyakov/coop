use std::path::PathBuf;

use anyhow::{Error, anyhow};
use gpui::AsyncApp;
#[cfg(not(target_arch = "wasm32"))]
use gpui_tokio::Tokio;
use mime_guess::from_path;
use nostr_blossom::prelude::*;
use nostr_sdk::prelude::*;

#[cfg(not(target_arch = "wasm32"))]
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

#[cfg(target_arch = "wasm32")]
pub async fn upload(_server: Url, _path: PathBuf, _cx: &AsyncApp) -> Result<Url, Error> {
    Err(anyhow!("File upload not supported on web"))
}
