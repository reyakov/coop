use std::path::PathBuf;

use aes_gcm::aead::consts::{U12, U16, U32};
use aes_gcm::aead::{Aead, AeadCore, KeyInit, OsRng};
use aes_gcm::aes::Aes256;
use aes_gcm::{Aes256Gcm, AesGcm, Nonce};
use anyhow::{Error, anyhow, bail};
use data_encoding::HEXLOWER;
use futures::AsyncReadExt;
#[cfg(not(target_arch = "wasm32"))]
use gpui::PathPromptOptions;
use gpui::http_client::AsyncBody;
use gpui::{AppContext as _, AsyncApp, SharedString, SharedUri, Task};
#[cfg(not(target_arch = "wasm32"))]
use mime_guess::from_path;
use nostr::nips::nip94::Sha256Hash;
use nostr_sdk::prelude::*;
use sha2::{Digest, Sha256};

pub(crate) const ALGORITHM: &str = "aes-gcm";

pub(crate) const MAX_FILE_SIZE: usize = 25 * 1024 * 1024;

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
pub(crate) struct EncryptedFile {
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

/// A file the user picked, read into memory.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PickedFile {
    pub name: String,
    pub mime: String,
    pub data: Vec<u8>,
    pub local: LocalFile,
}

/// A handle to a file stored on the user's device
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LocalFile {
    /// A path on disk (desktop)
    Path(PathBuf),
    /// An in-memory blob URL (web)
    Url(String),
}

impl LocalFile {
    /// The source an `img` element can render
    pub fn image_source(&self) -> gpui::ImageSource {
        match self {
            LocalFile::Path(path) => path.clone().into(),
            LocalFile::Url(url) => SharedUri::from(url.clone()).into(),
        }
    }

    /// The URL the platform opener understands
    pub fn open_url(&self) -> String {
        match self {
            LocalFile::Path(path) => format!("file://{}", path.display()),
            LocalFile::Url(url) => url.clone(),
        }
    }

    /// Open the file with the platform handler
    pub fn open(&self, cx: &gpui::App) -> Result<(), Error> {
        match self {
            LocalFile::Path(_) => {
                cx.open_url(&self.open_url());
                Ok(())
            }
            #[cfg(target_arch = "wasm32")]
            LocalFile::Url(url) => crate::web_file::open_url(url),
            #[cfg(not(target_arch = "wasm32"))]
            LocalFile::Url(_) => Ok(()),
        }
    }
}

pub(crate) fn encrypt(data: &[u8]) -> Result<EncryptedFile, Error> {
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

pub(crate) fn decrypt(data: &[u8], key: &str, nonce: &str) -> Result<Vec<u8>, Error> {
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

pub(crate) fn sha256_hex(data: &[u8]) -> String {
    let hash: [u8; 32] = Sha256::digest(data).into();

    Sha256Hash::from_byte_array(hash).to_hex()
}

/// Ask the user for a single file and read it into memory
pub fn pick_file(cx: &gpui::App) -> Task<Result<Option<PickedFile>, Error>> {
    #[cfg(not(target_arch = "wasm32"))]
    {
        let dialog = cx.prompt_for_paths(PathPromptOptions {
            files: true,
            directories: false,
            multiple: false,
            prompt: None,
        });

        cx.background_spawn(async move {
            let Some(path) = dialog.await?? else {
                return Ok(None);
            };
            let path = path.into_iter().next().ok_or_else(|| anyhow!("No file"))?;

            let mime = from_path(&path).first_or_octet_stream().to_string();
            let name = path
                .file_name()
                .map(|name| name.to_string_lossy().into_owned())
                .unwrap_or_else(|| path.display().to_string());
            let data = smol::fs::read(&path).await?;

            Ok(Some(PickedFile {
                name,
                mime,
                data,
                local: LocalFile::Path(path),
            }))
        })
    }

    #[cfg(target_arch = "wasm32")]
    {
        let _ = cx;
        cx.background_spawn(async move { crate::web_file::pick_file().await })
    }
}

/// Upload a file unencrypted and return its URL
pub async fn upload_file(server: Url, file: PickedFile, cx: &AsyncApp) -> Result<Url, Error> {
    let PickedFile { mime, data, .. } = file;
    let sha256 = sha256_hex(&data);

    crate::blossom::upload_blob(&server, data, &mime, &sha256, cx).await
}

/// Upload a file encrypted as an opaque blob, returning the attachment metadata
pub async fn upload_encrypted_file(
    server: Url,
    file: PickedFile,
    cx: &AsyncApp,
) -> Result<FileAttachment, Error> {
    let encrypted = encrypt(&file.data)?;
    let sha256 = sha256_hex(&encrypted.data);
    let original_sha256 = sha256_hex(&file.data);
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
        mime: file.mime,
        key: encrypted.key,
        nonce: encrypted.nonce,
        sha256: Some(sha256),
        original_sha256: Some(original_sha256),
        size: Some(size),
        dim: None,
        name: Some(file.name),
    })
}

pub(crate) async fn download_and_decrypt(
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

/// A downloaded and decrypted attachment on the user's device
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DownloadedFile {
    pub local: LocalFile,
    pub mime: String,
    pub name: String,
    /// The decrypted bytes, kept so the web can offer them as a download
    #[cfg(target_arch = "wasm32")]
    pub(crate) data: Vec<u8>,
}

impl DownloadedFile {
    pub fn open(&self, cx: &gpui::App) -> Result<(), Error> {
        match &self.local {
            LocalFile::Path(path) => {
                cx.open_with_system(path);
                Ok(())
            }
            #[cfg(target_arch = "wasm32")]
            LocalFile::Url(url) => {
                if self.mime.starts_with("image/") {
                    crate::web_file::open_url(url)
                } else {
                    crate::web_file::download(&self.name, &self.mime, &self.data)
                }
            }
            #[cfg(not(target_arch = "wasm32"))]
            LocalFile::Url(_) => Ok(()),
        }
    }

    /// Save the file to the user's downloads (web)
    #[cfg(target_arch = "wasm32")]
    pub fn save(&self) -> Result<(), Error> {
        crate::web_file::download(&self.name, &self.mime, &self.data)
    }
}

/// Download and decrypt a file attachment to the user's device
pub async fn download_and_decrypt_file(
    file: &FileAttachment,
    cx: &AsyncApp,
) -> Result<DownloadedFile, Error> {
    let data = download_and_decrypt(
        &file.url,
        &file.key,
        &file.nonce,
        file.sha256.as_deref(),
        cx,
    )
    .await?;

    let name = file
        .name
        .clone()
        .unwrap_or_else(|| fallback_name(&file.url));

    let local = store_file(&name, &file.mime, &data).await?;

    Ok(DownloadedFile {
        local,
        mime: file.mime.clone(),
        name,
        #[cfg(target_arch = "wasm32")]
        data,
    })
}

/// Persist decrypted bytes somewhere the platform can render or open them
async fn store_file(name: &str, mime: &str, data: &[u8]) -> Result<LocalFile, Error> {
    #[cfg(not(target_arch = "wasm32"))]
    {
        let digest = sha256_hex(name.as_bytes());
        let extension = mime_guess::get_mime_extensions_str(mime)
            .and_then(|extensions| extensions.first())
            .copied()
            .unwrap_or("bin");

        let path = std::env::temp_dir()
            .join("coop-files")
            .join(format!("{digest}.{extension}"));

        if smol::fs::metadata(&path).await.is_err() {
            let Some(parent) = path.parent() else {
                bail!("Invalid file path");
            };
            smol::fs::create_dir_all(parent).await?;

            // Write under a temporary name first, so an interrupted download is never reused
            let partial = path.with_extension("download");
            smol::fs::write(&partial, data).await?;
            smol::fs::rename(&partial, &path).await?;
        }

        Ok(LocalFile::Path(path))
    }

    #[cfg(target_arch = "wasm32")]
    {
        let _ = name;
        let url = crate::web_file::blob_url(mime, data)?;
        Ok(LocalFile::Url(url))
    }
}

/// A file name derived from the attachment URL's last path segment
fn fallback_name(url: &Url) -> String {
    url.path()
        .rsplit('/')
        .find(|segment| !segment.is_empty())
        .unwrap_or("attachment")
        .to_string()
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
