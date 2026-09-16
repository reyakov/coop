use std::path::{Path, PathBuf};

use chat::FileAttachment;
use gpui::SharedString;
use nostr_sdk::prelude::*;

/// A file attachment that has been uploaded, but not sent yet.
///
/// The local `path` is kept around so the composer can preview
/// the file without downloading and decrypting it again.
pub(crate) struct PendingFile {
    pub file: FileAttachment,
    pub path: PathBuf,
}

/// State of the encrypted file attachment of a message
pub(crate) enum DecryptedFile {
    Loading,
    Ready(PathBuf),
    Failed(SharedString),
}

/// Result of an upload, either plain or encrypted
pub(crate) enum Uploaded {
    Url(Url),
    File(FileAttachment, PathBuf),
}

/// A `file://` url for a decrypted file, so it can be opened by the OS
pub(crate) fn file_url(path: &Path) -> String {
    format!("file://{}", path.display())
}
