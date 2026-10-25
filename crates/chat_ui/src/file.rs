use anyhow::Error;
use gpui::SharedString;
use nostr_sdk::prelude::*;
use state::{FileAttachment, LocalFile, PickedFile};

/// A file attachment that has been uploaded, but not sent yet.
///
/// The `local` handle is kept around so the composer can preview
/// the file without downloading and decrypting it again.
pub(crate) struct PendingFile {
    pub file: FileAttachment,
    pub local: LocalFile,
}

/// State of the encrypted file attachment of a message
pub(crate) enum DecryptedFile {
    Loading,
    Ready(LocalFile),
    Failed(SharedString),
}

/// Result of an upload, either plain or encrypted
pub(crate) enum Uploaded {
    Url(Url),
    File(Box<UploadedFile>),
}

/// An encrypted upload and the handle to preview it locally
pub(crate) struct UploadedFile {
    pub file: FileAttachment,
    pub local: LocalFile,
}

impl Uploaded {
    /// Build the upload result for a picked file, encrypted when the
    /// attachment is the whole message
    pub(crate) async fn from_pick(
        encrypted: bool,
        server: Url,
        picked: PickedFile,
        cx: &gpui::AsyncApp,
    ) -> Result<Self, Error> {
        if encrypted {
            let UploadedFileParts { file, local } = upload_encrypted(server, picked, cx).await?;

            Ok(Self::File(Box::new(UploadedFile { file, local })))
        } else {
            state::upload_file(server, picked, cx).await.map(Self::Url)
        }
    }
}

/// The pieces of an encrypted upload, split so the local handle survives the
/// picked file being consumed by the upload
struct UploadedFileParts {
    file: FileAttachment,
    local: LocalFile,
}

async fn upload_encrypted(
    server: Url,
    picked: PickedFile,
    cx: &gpui::AsyncApp,
) -> Result<UploadedFileParts, Error> {
    let local = picked.local.clone();

    state::upload_encrypted_file(server, picked, cx)
        .await
        .map(|file| UploadedFileParts { file, local })
}
