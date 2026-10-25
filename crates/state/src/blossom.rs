use anyhow::{Error, anyhow};
use gpui::AsyncApp;
#[cfg(not(target_arch = "wasm32"))]
use gpui_tokio::Tokio;
use nostr_blossom::prelude::*;
use nostr_sdk::prelude::*;

/// Upload a blob to a blossom server and return its URL
#[cfg(not(target_arch = "wasm32"))]
pub(crate) async fn upload_blob(
    server: &Url,
    data: Vec<u8>,
    content_type: &str,
    sha256: &str,
    cx: &AsyncApp,
) -> Result<Url, Error> {
    let client = BlossomClient::new(server.clone());
    let keys = Keys::generate();
    let content_type = content_type.to_string();
    let base = server.clone();
    let hash = sha256.to_string();

    Tokio::spawn(cx, async move {
        match client
            .upload_blob(data, Some(content_type), None, Some(&keys))
            .await
        {
            Ok(blob) => Ok(blob.url),
            Err(e) if e.to_string().contains("201 Created") => Ok::<Url, Error>(base.join(&hash)?),
            Err(e) => Err(anyhow!(e.to_string())),
        }
    })
    .await
    .map_err(|e| anyhow!("Upload error: {e}"))?
}

/// Upload a blob to a blossom server and return its URL.
#[cfg(target_arch = "wasm32")]
pub(crate) async fn upload_blob(
    server: &Url,
    data: Vec<u8>,
    content_type: &str,
    sha256: &str,
    _cx: &AsyncApp,
) -> Result<Url, Error> {
    let client = BlossomClient::new(server.clone());
    let keys = Keys::generate();
    let base = server.clone();
    let hash = sha256.to_string();

    match client
        .upload_blob(data, Some(content_type.to_string()), None, Some(&keys))
        .await
    {
        Ok(blob) => Ok(blob.url),
        Err(e) if e.to_string().contains("201 Created") => Ok(base.join(&hash)?),
        Err(e) => Err(anyhow!(e.to_string())),
    }
}
