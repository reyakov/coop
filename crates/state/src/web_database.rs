use std::cell::RefCell;
use std::sync::Arc;

use nostr_memory::prelude::*;
use nostr_sqlite::prelude::NostrSqlite;
use sqlite_wasm_rs::WasmOsCallback;
use sqlite_wasm_vfs::relaxed_idb;

const DATABASE_PATH: &str = "coop.sqlite3";
const VFS_NAME: &str = "coop-relaxed-idb";

thread_local! {
    static DATABASE: RefCell<Option<NostrSqlite>> = const { RefCell::new(None) };
}

/// Install the IndexedDB storage backend and open the database.
pub async fn open() {
    match open_persistent().await {
        Some(database) => DATABASE.with(|slot| *slot.borrow_mut() = Some(database)),
        None => log::error!("Falling back to the in-memory nostr database"),
    }
}

/// Hand the opened database to the client constructor
pub(crate) fn take() -> Arc<dyn NostrDatabase> {
    let database = DATABASE.with(|slot| slot.borrow_mut().take());

    match database {
        Some(database) => Arc::new(database),
        None => {
            log::error!("Persistent web database was not opened, falling back to memory");
            Arc::new(MemoryDatabase::unbounded())
        }
    }
}

/// Open the database on IndexedDB storage, or `None` when unavailable
async fn open_persistent() -> Option<NostrSqlite> {
    let options = relaxed_idb::RelaxedIdbCfgBuilder::new()
        .vfs_name(VFS_NAME)
        .build();

    if let Err(e) = relaxed_idb::install::<WasmOsCallback>(&options, true).await {
        log::warn!("IndexedDB storage unavailable: {e:?}");
        return None;
    }

    match NostrSqlite::builder()
        .persistent_with_vfs(DATABASE_PATH, VFS_NAME)
        .build()
        .await
    {
        Ok(database) => {
            log::info!("Nostr database opened on IndexedDB storage");
            Some(database)
        }
        Err(e) => {
            log::error!("Failed to open the nostr database on IndexedDB: {e:?}");
            None
        }
    }
}
