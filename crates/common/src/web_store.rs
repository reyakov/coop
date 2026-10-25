use anyhow::{Error, anyhow};

const LOCAL_STORAGE: &str = "localStorage";

fn storage() -> Result<web_sys::Storage, Error> {
    web_sys::window()
        .ok_or_else(|| anyhow!("no browser window"))?
        .local_storage()
        .map_err(|e| anyhow!("{LOCAL_STORAGE} unavailable: {e:?}"))?
        .ok_or_else(|| anyhow!("{LOCAL_STORAGE} unavailable"))
}

/// Read a value previously written with [`set`]
pub fn get(key: &str) -> Result<Option<String>, Error> {
    storage()?
        .get_item(key)
        .map_err(|e| anyhow!("{LOCAL_STORAGE} read failed: {e:?}"))
}

/// Write a value, replacing any previous one
pub fn set(key: &str, value: &str) -> Result<(), Error> {
    storage()?
        .set_item(key, value)
        .map_err(|e| anyhow!("{LOCAL_STORAGE} write failed: {e:?}"))
}

/// Remove a value if it exists
pub fn remove(key: &str) -> Result<(), Error> {
    storage()?
        .remove_item(key)
        .map_err(|e| anyhow!("{LOCAL_STORAGE} delete failed: {e:?}"))
}
