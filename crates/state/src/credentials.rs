use anyhow::Result;
#[cfg(target_arch = "wasm32")]
use common::web_store;
use gpui::{App, AppContext as _, Task};

use crate::USER_KEYRING;

/// The storage key for the log of handled NIP-4e key-sync request ids
const PROCESSED_REQUESTS_KEY: &str = "coop/nip4e/processed-requests";

/// Read the stored credential as `(username, secret)`
#[allow(clippy::type_complexity)]
pub(crate) fn read(cx: &App) -> Task<Result<Option<(String, Vec<u8>)>>> {
    #[cfg(not(target_arch = "wasm32"))]
    return cx.read_credentials(USER_KEYRING);

    #[cfg(target_arch = "wasm32")]
    {
        let _ = cx;
        Task::ready(
            web_store::get(USER_KEYRING)
                .map(|stored| stored.map(|content| (String::new(), content.into_bytes()))),
        )
    }
}

/// Write the stored credential
pub(crate) fn write(cx: &App, username: &str, secret: &[u8]) -> Task<Result<()>> {
    #[cfg(not(target_arch = "wasm32"))]
    return cx.write_credentials(USER_KEYRING, username, secret);

    #[cfg(target_arch = "wasm32")]
    {
        let _ = (cx, username);
        let content = match std::str::from_utf8(secret) {
            Ok(content) => content,
            Err(e) => return Task::ready(Err(anyhow::anyhow!("credential is not UTF-8: {e}"))),
        };

        Task::ready(web_store::set(USER_KEYRING, content))
    }
}

/// Delete the stored credential
pub(crate) fn delete(cx: &App) -> Task<Result<()>> {
    #[cfg(not(target_arch = "wasm32"))]
    return cx.delete_credentials(USER_KEYRING);

    #[cfg(target_arch = "wasm32")]
    {
        let _ = cx;
        Task::ready(web_store::remove(USER_KEYRING))
    }
}

/// Read named bytes from the platform keyring,
/// falling back to browser storage on web where no keyring exists
fn read_named(cx: &App, key: &str) -> Task<Result<Option<Vec<u8>>>> {
    #[cfg(not(target_arch = "wasm32"))]
    {
        let key = key.to_owned();
        let read = cx.read_credentials(&key);
        cx.background_spawn(async move { Ok(read.await?.map(|(_, secret)| secret)) })
    }

    #[cfg(target_arch = "wasm32")]
    {
        let _ = cx;
        let stored = web_store::get(key).map(|content| content.map(|s| s.into_bytes()));
        Task::ready(stored)
    }
}

/// Write named bytes to the platform keyring,
/// falling back to browser storage on web where no keyring exists
fn write_named(cx: &App, key: &str, secret: &[u8]) -> Task<Result<()>> {
    #[cfg(not(target_arch = "wasm32"))]
    {
        let key = key.to_owned();
        let secret = secret.to_vec();
        let write = cx.write_credentials(&key, "", &secret);
        cx.background_spawn(write)
    }

    #[cfg(target_arch = "wasm32")]
    {
        let _ = cx;
        let content = match std::str::from_utf8(secret) {
            Ok(content) => content,
            Err(e) => return Task::ready(Err(anyhow::anyhow!("secret is not UTF-8: {e}"))),
        };

        Task::ready(web_store::set(key, content))
    }
}

/// Read the log of handled key-sync request ids, as concatenated hex ids
pub fn read_processed_requests(cx: &App) -> Task<Result<Option<Vec<u8>>>> {
    read_named(cx, PROCESSED_REQUESTS_KEY)
}

/// Persist the log of handled key-sync request ids, as concatenated hex ids
pub fn write_processed_requests(cx: &App, ids: &[u8]) -> Task<Result<()>> {
    write_named(cx, PROCESSED_REQUESTS_KEY, ids)
}
