use anyhow::Result;
#[cfg(target_arch = "wasm32")]
use common::web_store;
use gpui::{App, Task};

use crate::USER_KEYRING;

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
