//! The private store: a SQLite index beside a content-addressed blob store.
//!
//! Layout (under the platform's local data directory by default):
//!
//! ```text
//! store.db     SQLite (WAL): roots, snapshots, proposals, comments
//! blobs/       content-addressed: blobs/ab/cd/<sha256>
//! token        random secret for the local IPC socket (0600)
//! ipc.json     {port, token} — how the MCP bridge finds a running app
//! ```

pub mod blobs;
pub mod schema;

use crate::error::{Error, Result};
use crate::util::{now_ms, random_suffix};
use blobs::BlobStore;
use rusqlite::{Connection, OptionalExtension};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard};

pub struct Store {
    dir: PathBuf,
    conn: Mutex<Connection>,
    blobs: BlobStore,
}

/// An explicit `--store` / `FOLIO_STORE`, if one was given.
///
/// The distinction matters to the MCP bridge: told nothing, it should follow
/// whichever app is running; told a store explicitly, it must use that one.
pub fn store_dir_override() -> Option<PathBuf> {
    std::env::var("FOLIO_STORE")
        .ok()
        .filter(|explicit| !explicit.trim().is_empty())
        .map(|explicit| crate::util::expand_tilde(&explicit))
}

/// Where Folio keeps its data when nobody says otherwise.
pub fn standard_store_dir() -> PathBuf {
    dirs::data_local_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("Folio")
}

/// Where the store lives unless `--store` overrides it.
pub fn default_store_dir() -> PathBuf {
    store_dir_override().unwrap_or_else(standard_store_dir)
}

impl Store {
    pub fn open(dir: &Path) -> Result<Store> {
        std::fs::create_dir_all(dir)?;
        let conn = Connection::open(dir.join("store.db"))?;

        // WAL is the reason the app and a headless bridge can both hold the
        // store open. The busy timeout absorbs the brief writer overlap that
        // WAL still serialises.
        conn.pragma_update(None, "journal_mode", "WAL")?;
        conn.pragma_update(None, "synchronous", "NORMAL")?;
        conn.pragma_update(None, "foreign_keys", "ON")?;
        conn.busy_timeout(std::time::Duration::from_secs(10))?;

        conn.execute_batch(schema::SCHEMA)?;

        let store = Store {
            dir: dir.to_path_buf(),
            conn: Mutex::new(conn),
            blobs: BlobStore::new(dir.join("blobs"))?,
        };
        store.set_meta("schema_version", &schema::SCHEMA_VERSION.to_string())?;
        store.warn_if_cloud_synced();
        Ok(store)
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    pub fn blobs(&self) -> &BlobStore {
        &self.blobs
    }

    pub fn conn(&self) -> MutexGuard<'_, Connection> {
        // A poisoned lock means a panic inside a store call. The database
        // itself is transactional, so recovering the guard is safe and is
        // preferable to bringing the whole app down.
        match self.conn.lock() {
            Ok(guard) => guard,
            Err(poisoned) => poisoned.into_inner(),
        }
    }

    /// Run `f` inside a transaction, committing on `Ok` and rolling back on
    /// `Err`. Every multi-row mutation in the core goes through this.
    pub fn tx<T>(&self, f: impl FnOnce(&rusqlite::Transaction<'_>) -> Result<T>) -> Result<T> {
        let mut conn = self.conn();
        let tx = conn.transaction()?;
        let out = f(&tx)?;
        tx.commit()?;
        Ok(out)
    }

    pub fn get_meta(&self, key: &str) -> Result<Option<String>> {
        let conn = self.conn();
        Ok(conn
            .query_row("SELECT value FROM meta WHERE key = ?1", [key], |r| r.get(0))
            .optional()?)
    }

    pub fn set_meta(&self, key: &str, value: &str) -> Result<()> {
        let conn = self.conn();
        conn.execute(
            "INSERT INTO meta(key, value) VALUES (?1, ?2)
             ON CONFLICT(key) DO UPDATE SET value = excluded.value",
            (key, value),
        )?;
        Ok(())
    }

    /// Allocate a short, quotable id (`prop_8f3a`) that is unique in `table`.
    /// Widens the suffix rather than looping forever if the space is crowded.
    pub fn alloc_id(conn: &Connection, table: &str, prefix: &str) -> Result<String> {
        let sql = format!("SELECT 1 FROM {table} WHERE id = ?1");
        for attempt in 0..40 {
            let width = if attempt < 24 { 4 } else { 12 };
            let candidate = format!("{prefix}_{}", random_suffix(width));
            let taken: Option<i64> = conn.query_row(&sql, [&candidate], |r| r.get(0)).optional()?;
            if taken.is_none() {
                return Ok(candidate);
            }
        }
        Err(Error::other(format!("could not allocate a unique {prefix} id")))
    }

    /// The token that authenticates the local IPC socket. Created on first
    /// use; the file is restricted to the current user.
    pub fn ipc_token(&self) -> Result<String> {
        let path = self.dir.join("token");
        if let Ok(existing) = std::fs::read_to_string(&path) {
            let trimmed = existing.trim().to_string();
            if trimmed.len() >= 32 {
                return Ok(trimmed);
            }
        }
        let token = random_suffix(48);
        std::fs::write(&path, &token)?;
        restrict_to_owner(&path);
        Ok(token)
    }

    /// SQLite on a cloud-synced folder corrupts. Detect the well-known sync
    /// roots and record a warning the UI shows loudly rather than failing
    /// mysteriously three weeks later.
    fn warn_if_cloud_synced(&self) {
        let lowered = self.dir.to_string_lossy().to_lowercase();
        let suspects = ["dropbox", "onedrive", "google drive", "googledrive", "icloud", "sync.com", "box sync"];
        if let Some(hit) = suspects.iter().find(|s| lowered.contains(**s)) {
            let _ = self.set_meta("cloud_sync_warning", hit);
        } else {
            let _ = self.set_meta("cloud_sync_warning", "");
        }
    }

    pub fn cloud_sync_warning(&self) -> Option<String> {
        match self.get_meta("cloud_sync_warning") {
            Ok(Some(s)) if !s.is_empty() => Some(s),
            _ => None,
        }
    }

    /// Bytes the store occupies: database plus blobs. Shown in the status bar.
    pub fn size_bytes(&self) -> u64 {
        let db = std::fs::metadata(self.dir.join("store.db")).map(|m| m.len()).unwrap_or(0);
        db + self.blobs.total_size()
    }

    /// Record a connected MCP client so the status bar can list who is driving.
    pub fn touch_client(&self, name: &str, mode: &str) -> Result<()> {
        let conn = self.conn();
        let now = now_ms();
        conn.execute(
            "INSERT INTO clients(name, mode, first_seen, last_seen) VALUES (?1, ?2, ?3, ?3)
             ON CONFLICT(name) DO UPDATE SET last_seen = excluded.last_seen, mode = excluded.mode",
            (name, mode, now),
        )?;
        Ok(())
    }

    /// Clients seen within `window_ms`. Anything older is assumed gone; MCP
    /// stdio servers exit without saying goodbye.
    pub fn active_clients(&self, window_ms: i64) -> Result<Vec<(String, String, i64)>> {
        let conn = self.conn();
        let cutoff = now_ms() - window_ms;
        let mut stmt = conn.prepare(
            "SELECT name, mode, last_seen FROM clients WHERE last_seen >= ?1 ORDER BY name",
        )?;
        let rows = stmt
            .query_map([cutoff], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    /// Sliding-window rate limit. Returns an error rather than silently
    /// dropping, so a looping agent learns it is looping.
    pub fn check_rate_limit(&self, client: &str, kind: &str, limit: i64, window_ms: i64) -> Result<()> {
        let conn = self.conn();
        let now = now_ms();
        let cutoff = now - window_ms;
        conn.execute("DELETE FROM rate_events WHERE at < ?1", [cutoff - window_ms])?;
        let count: i64 = conn.query_row(
            "SELECT COUNT(*) FROM rate_events WHERE client = ?1 AND kind = ?2 AND at >= ?3",
            (client, kind, cutoff),
            |r| r.get(0),
        )?;
        if count >= limit {
            return Err(Error::RateLimited(format!(
                "{client} has made {count} {kind} calls in the last {} minutes (limit {limit})",
                window_ms / 60_000
            )));
        }
        conn.execute(
            "INSERT INTO rate_events(client, kind, at) VALUES (?1, ?2, ?3)",
            (client, kind, now),
        )?;
        Ok(())
    }
}

#[cfg(windows)]
fn restrict_to_owner(path: &Path) {
    // On Windows the file inherits the user profile ACL, which is already
    // user-only; make it at least non-obvious by hiding it.
    use std::os::windows::ffi::OsStrExt;
    let wide: Vec<u16> = path.as_os_str().encode_wide().chain(std::iter::once(0)).collect();
    unsafe {
        // FILE_ATTRIBUTE_HIDDEN = 0x2
        extern "system" {
            fn SetFileAttributesW(name: *const u16, attrs: u32) -> i32;
        }
        SetFileAttributesW(wide.as_ptr(), 0x2);
    }
}

#[cfg(not(windows))]
fn restrict_to_owner(path: &Path) {
    use std::os::unix::fs::PermissionsExt;
    let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn opens_and_allocates_unique_ids() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path()).unwrap();
        let conn = store.conn();
        let a = Store::alloc_id(&conn, "proposals", "prop").unwrap();
        assert!(a.starts_with("prop_"));
        assert_eq!(a.len(), "prop_".len() + 4);
    }

    #[test]
    fn rate_limit_trips_then_reports() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path()).unwrap();
        for _ in 0..3 {
            store.check_rate_limit("looping-agent", "reply", 3, 60_000).unwrap();
        }
        let err = store.check_rate_limit("looping-agent", "reply", 3, 60_000).unwrap_err();
        assert_eq!(err.code(), "rate_limited");
    }

    #[test]
    fn meta_round_trips() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path()).unwrap();
        store.set_meta("last_review_at", "42").unwrap();
        store.set_meta("last_review_at", "43").unwrap();
        assert_eq!(store.get_meta("last_review_at").unwrap().as_deref(), Some("43"));
    }
}
