use std::path::PathBuf;

use aes_gcm::aead::consts::{U12, U16, U32};
use aes_gcm::aead::{Aead, AeadCore, KeyInit, OsRng};
use aes_gcm::aes::Aes256;
use aes_gcm::{Aes256Gcm, AesGcm, Nonce};
use anyhow::{Error, anyhow, bail};
use data_encoding::HEXLOWER;
use futures::AsyncReadExt;
use gpui::http_client::AsyncBody;
use gpui::{AsyncApp, SharedString};
#[cfg(not(target_arch = "wasm32"))]
use mime_guess::from_path;
use nostr::nips::nip94::Sha256Hash;
use nostr_sdk::prelude::*;
use sha2::{Digest, Sha256};

pub const ALGORITHM: &str = "aes-gcm";

pub const MAX_FILE_SIZE: usize = 25 * 1024 * 1024;

const TAG_SHA256: &str = "x";
const TAG_ORIGINAL_SHA256: &str = "ox";
const TAG_FILE_TYPE: &str = "file-type";
const TAG_ALGORITHM: &str = "encryption-algorithm";
const TAG_KEY: &str = "decryption-key";
const TAG_NONCE: &str = "decryption-nonce";
const TAG_SIZE: &str = "size";
const TAG_DIM: &str = "dim";
const TAG_ALT: &str = "alt";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EncryptedFile {
    pub data: Vec<u8>,
    pub key: String,
    pub nonce: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileAttachment {
    pub url: Url,
    pub mime: String,
    pub key: String,
    pub nonce: String,
    pub sha256: Option<String>,
    pub original_sha256: Option<String>,
    pub size: Option<u64>,
    pub dim: Option<(u32, u32)>,
    pub name: Option<String>,
}

impl FileAttachment {
    pub fn tags(&self) -> Vec<Tag> {
        let mut tags = vec![
            Tag::custom(TAG_FILE_TYPE, [self.mime.clone()]),
            Tag::custom(TAG_ALGORITHM, [ALGORITHM]),
            Tag::custom(TAG_KEY, [self.key.clone()]),
            Tag::custom(TAG_NONCE, [self.nonce.clone()]),
        ];

        if let Some(sha256) = &self.sha256 {
            tags.push(Tag::custom(TAG_SHA256, [sha256.clone()]));
        }

        if let Some(original_sha256) = &self.original_sha256 {
            tags.push(Tag::custom(TAG_ORIGINAL_SHA256, [original_sha256.clone()]));
        }

        if let Some(size) = self.size {
            tags.push(Tag::custom(TAG_SIZE, [size.to_string()]));
        }

        if let Some((width, height)) = self.dim {
            tags.push(Tag::custom(TAG_DIM, [format!("{width}x{height}")]));
        }

        if let Some(name) = &self.name {
            tags.push(Tag::custom(TAG_ALT, [name.clone()]));
        }

        tags
    }

    pub fn from_tags(content: &str, tags: &Tags) -> Option<Self> {
        if tag_value(tags, TAG_ALGORITHM)? != ALGORITHM {
            return None;
        }

        Some(Self {
            url: Url::parse(content).ok()?,
            mime: tag_value(tags, TAG_FILE_TYPE)?.to_string(),
            key: tag_value(tags, TAG_KEY)?.to_string(),
            nonce: tag_value(tags, TAG_NONCE)?.to_string(),
            sha256: tag_value(tags, TAG_SHA256).map(str::to_string),
            original_sha256: tag_value(tags, TAG_ORIGINAL_SHA256).map(str::to_string),
            size: tag_value(tags, TAG_SIZE).and_then(|size| size.parse().ok()),
            dim: tag_value(tags, TAG_DIM).and_then(parse_dim),
            name: tag_value(tags, TAG_ALT).map(str::to_string),
        })
    }

    pub fn is_image(&self) -> bool {
        self.mime.starts_with("image/")
    }

    pub fn display_name(&self) -> SharedString {
        if let Some(name) = &self.name {
            return name.clone().into();
        }

        match self.size {
            Some(size) => format!("{} ({size} bytes)", self.mime).into(),
            None => self.mime.clone().into(),
        }
    }
}

pub fn encrypt(data: &[u8]) -> Result<EncryptedFile, Error> {
    let key = Aes256Gcm::generate_key(OsRng);
    let nonce = AesGcm::<Aes256, U16>::generate_nonce(OsRng);
    let cipher = AesGcm::<Aes256, U16>::new(&key);

    let data = cipher
        .encrypt(&nonce, data)
        .map_err(|_| anyhow!("Failed to encrypt file"))?;

    Ok(EncryptedFile {
        data,
        key: HEXLOWER.encode(key.as_slice()),
        nonce: HEXLOWER.encode(nonce.as_slice()),
    })
}

pub fn decrypt(data: &[u8], key: &str, nonce: &str) -> Result<Vec<u8>, Error> {
    let key = decode(key, "decryption key")?;
    let nonce = decode(nonce, "decryption nonce")?;

    if key.len() != 32 {
        bail!(
            "Invalid decryption key length: expected 32 bytes, got {}",
            key.len()
        );
    }

    match nonce.len() {
        12 => Aes256Gcm::new_from_slice(&key)
            .map_err(|_| anyhow!("Invalid decryption key"))?
            .decrypt(Nonce::<U12>::from_slice(&nonce), data)
            .map_err(|_| anyhow!("Failed to decrypt file")),
        16 => AesGcm::<Aes256, U16>::new_from_slice(&key)
            .map_err(|_| anyhow!("Invalid decryption key"))?
            .decrypt(Nonce::<U16>::from_slice(&nonce), data)
            .map_err(|_| anyhow!("Failed to decrypt file")),
        32 => AesGcm::<Aes256, U32>::new_from_slice(&key)
            .map_err(|_| anyhow!("Invalid decryption key"))?
            .decrypt(Nonce::<U32>::from_slice(&nonce), data)
            .map_err(|_| anyhow!("Failed to decrypt file")),
        len => bail!("Unsupported decryption nonce length: {len} bytes"),
    }
}

pub fn sha256_hex(data: &[u8]) -> String {
    let hash: [u8; 32] = Sha256::digest(data).into();

    Sha256Hash::from_byte_array(hash).to_hex()
}

#[cfg(not(target_arch = "wasm32"))]
pub async fn upload_encrypted(
    server: Url,
    path: PathBuf,
    cx: &AsyncApp,
) -> Result<FileAttachment, Error> {
    let mime = from_path(&path).first_or_octet_stream().to_string();
    let name = path
        .file_name()
        .map(|name| name.to_string_lossy().into_owned());
    let data = smol::fs::read(&path).await?;

    let encrypted = encrypt(&data)?;
    let sha256 = sha256_hex(&encrypted.data);
    let original_sha256 = sha256_hex(&data);
    let size = encrypted.data.len() as u64;
    let base_url = server.to_string();

    let url = crate::blossom::upload_blob(
        &server,
        encrypted.data,
        "application/octet-stream",
        &sha256,
        cx,
    )
    .await
    .map_err(|e| {
        let message = e.to_string();

        if !message.contains("415") {
            return anyhow!(message);
        }

        anyhow!(
            "{base_url} rejected the encrypted file. Encrypted attachments are uploaded as
             opaque data, which this file server does not accept. Choose a different file
             server in the settings."
        )
    })?;

    Ok(FileAttachment {
        url,
        mime,
        key: encrypted.key,
        nonce: encrypted.nonce,
        sha256: Some(sha256),
        original_sha256: Some(original_sha256),
        size: Some(size),
        dim: None,
        name,
    })
}

#[cfg(target_arch = "wasm32")]
pub async fn upload_encrypted(
    _server: Url,
    _path: PathBuf,
    _cx: &AsyncApp,
) -> Result<FileAttachment, Error> {
    Err(anyhow!("File upload not supported on web"))
}

pub async fn download_and_decrypt(
    url: &Url,
    key: &str,
    nonce: &str,
    expected_sha256: Option<&str>,
    cx: &AsyncApp,
) -> Result<Vec<u8>, Error> {
    let client = cx.update(|app| app.http_client());
    let response = client.get(url.as_str(), AsyncBody::default(), true).await?;

    if !response.status().is_success() {
        bail!("Failed to download file: HTTP {}", response.status());
    }

    let mut data = Vec::new();
    response
        .into_body()
        .take(MAX_FILE_SIZE as u64 + 1)
        .read_to_end(&mut data)
        .await?;

    if data.len() > MAX_FILE_SIZE {
        bail!("File is too large (max {MAX_FILE_SIZE} bytes)");
    }

    if let Some(expected) = expected_sha256
        && !sha256_hex(&data).eq_ignore_ascii_case(expected)
    {
        bail!("File hash mismatch");
    }

    decrypt(&data, key, nonce)
}

/// Download and decrypt a file attachment into a temporary file.
///
/// The same attachment always maps to the same path, so callers can render the
/// result directly (e.g. with `img`) without downloading it more than once.
#[cfg(not(target_arch = "wasm32"))]
pub async fn download_and_decrypt_to_file(
    file: &FileAttachment,
    cx: &AsyncApp,
) -> Result<PathBuf, Error> {
    let name = file
        .sha256
        .clone()
        .unwrap_or_else(|| sha256_hex(file.url.as_str().as_bytes()));

    let extension = mime_guess::get_mime_extensions_str(&file.mime)
        .and_then(|extensions| extensions.first())
        .copied()
        .unwrap_or("bin");

    let path = std::env::temp_dir()
        .join("coop-files")
        .join(format!("{name}.{extension}"));

    if smol::fs::metadata(&path).await.is_ok() {
        return Ok(path);
    }

    let data = download_and_decrypt(
        &file.url,
        &file.key,
        &file.nonce,
        file.sha256.as_deref(),
        cx,
    )
    .await?;

    let Some(parent) = path.parent() else {
        bail!("Invalid file path");
    };
    smol::fs::create_dir_all(parent).await?;

    // Write under a temporary name first, so an interrupted download is never reused
    let partial = path.with_extension("download");
    smol::fs::write(&partial, data).await?;
    smol::fs::rename(&partial, &path).await?;

    Ok(path)
}

#[cfg(target_arch = "wasm32")]
pub async fn download_and_decrypt_to_file(
    _file: &FileAttachment,
    _cx: &AsyncApp,
) -> Result<PathBuf, Error> {
    Err(anyhow!("File download not supported on web"))
}

/// The cache file a decrypted blob for `plaintext_sha256` is written to.
#[cfg(not(target_arch = "wasm32"))]
fn blob_cache_path(plaintext_sha256: &str) -> PathBuf {
    std::env::temp_dir()
        .join("coop-blobs")
        .join(plaintext_sha256)
}

/// Download an encrypted blob whose pointer carries the *plaintext* hash
/// and write the decrypted bytes to a content-addressed cache file,
/// so later renders skip the network.
///
/// The cache file carries no extension: `img` sniffs the format from the bytes.
#[cfg(not(target_arch = "wasm32"))]
pub async fn download_and_decrypt_to_cache(
    url: &Url,
    key: &str,
    nonce: &str,
    plaintext_sha256: &str,
    cx: &AsyncApp,
) -> Result<PathBuf, Error> {
    let path = blob_cache_path(plaintext_sha256);

    if smol::fs::metadata(&path).await.is_ok() {
        return Ok(path);
    }

    let data = download_and_decrypt(url, key, nonce, None, cx).await?;

    if !sha256_hex(&data).eq_ignore_ascii_case(plaintext_sha256) {
        bail!("Blob hash mismatch");
    }

    let Some(parent) = path.parent() else {
        bail!("Invalid blob cache path");
    };
    smol::fs::create_dir_all(parent).await?;

    // Write under a temporary name first, so an interrupted download is never reused
    let partial = path.with_extension("download");
    smol::fs::write(&partial, data).await?;
    smol::fs::rename(&partial, &path).await?;

    Ok(path)
}

#[cfg(target_arch = "wasm32")]
pub async fn download_and_decrypt_to_cache(
    _url: &Url,
    _key: &str,
    _nonce: &str,
    _plaintext_sha256: &str,
    _cx: &AsyncApp,
) -> Result<PathBuf, Error> {
    Err(anyhow!("Blob download not supported on web"))
}

fn tag_value<'a>(tags: &'a Tags, name: &str) -> Option<&'a str> {
    tags.iter()
        .find(|tag| tag.kind() == name)
        .and_then(|tag| tag.content())
}

fn parse_dim(value: &str) -> Option<(u32, u32)> {
    let (width, height) = value.split_once('x')?;

    Some((width.parse().ok()?, height.parse().ok()?))
}

fn decode(value: &str, label: &str) -> Result<Vec<u8>, Error> {
    HEXLOWER
        .decode(value.to_ascii_lowercase().as_bytes())
        .map_err(|_| anyhow!("Invalid {label} encoding"))
}
