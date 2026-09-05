//! Snapshots, timelines, restore, and patch-series export.
//!
//! Files on disk stay canonical, plain, portable markdown. Everything here is
//! the private history beside them: git plumbing in spirit, entirely internal
//! in fact. Folio never writes a `.git` directory into a user's folder.

use crate::config::Config;
use crate::corpus::{self, ArtifactType, Resolved};
use crate::diff;
use crate::error::{Error, Result};
use crate::store::Store;
use crate::util::{atomic_write, display_path, ms_to_rfc3339, now_ms, sha256_hex};
use rusqlite::{Connection, OptionalExtension};
use serde::{Deserialize, Serialize};

/// Every snapshot records how it came to exist and who caused it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Source {
    /// Save from the Folio editor.
    Save,
    /// External change seen by the watcher.
    External,
    /// A proposal was accepted.
    Proposal,
    /// An old version was restored (which is itself undoable).
    Restore,
    /// Manual checkpoint, with a message.
    Checkpoint,
    /// A policy-permitted direct write over MCP.
    Direct,
}

impl Source {
    pub fn as_str(self) -> &'static str {
        match self {
            Source::Save => "save",
            Source::External => "external",
            Source::Proposal => "proposal",
            Source::Restore => "restore",
            Source::Checkpoint => "checkpoint",
            Source::Direct => "direct",
        }
    }
    pub fn parse(s: &str) -> Source {
        match s {
            "save" => Source::Save,
            "proposal" => Source::Proposal,
            "restore" => Source::Restore,
            "checkpoint" => Source::Checkpoint,
            "direct" => Source::Direct,
            _ => Source::External,
        }
    }
    /// The author recorded when the caller does not name one.
    pub fn default_author(self) -> &'static str {
        match self {
            Source::Save | Source::Restore | Source::Checkpoint => "you",
            _ => "external",
        }
    }
}

pub const AUTHOR_YOU: &str = "you";
pub const AUTHOR_EXTERNAL: &str = "external";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Snapshot {
    pub id: String,
    pub root_id: String,
    pub path: String,
    pub display: String,
    pub blob_hash: String,
    pub size: i64,
    pub mtime: Option<i64>,
    pub source: Source,
    pub author: String,
    pub client: Option<String>,
    pub message: Option<String>,
    #[serde(rename = "type")]
    pub artifact_type: ArtifactType,
    pub created_at: i64,
    /// RFC 3339, so the frontend never has to guess a timezone.
    pub created_at_iso: String,
    /// Size change against the previous version, for the timeline's delta column.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub size_delta: Option<i64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RecordOutcome {
    pub snapshot: Snapshot,
    /// False when the content was identical to the latest version, so nothing
    /// new was written. Saving twice with no change creates no new version.
    pub created: bool,
}

pub fn row_to_snapshot(row: &rusqlite::Row<'_>) -> rusqlite::Result<Snapshot> {
    let path: String = row.get("path")?;
    let created_at: i64 = row.get("created_at")?;
    Ok(Snapshot {
        id: row.get("id")?,
        root_id: row.get("root_id")?,
        display: display_path(&path),
        path,
        blob_hash: row.get("blob_hash")?,
        size: row.get("size")?,
        mtime: row.get("mtime")?,
        source: Source::parse(&row.get::<_, String>("source")?),
        author: row.get("author")?,
        client: row.get("client")?,
        message: row.get("message")?,
        artifact_type: ArtifactType::parse(&row.get::<_, String>("artifact_type")?),
        created_at,
        created_at_iso: ms_to_rfc3339(created_at),
        size_delta: None,
    })
}

// ---------------------------------------------------------------------------
// Recording
// ---------------------------------------------------------------------------

pub struct RecordRequest<'a> {
    pub resolved: &'a Resolved,
    pub content: &'a [u8],
    pub source: Source,
    pub author: Option<&'a str>,
    pub client: Option<&'a str>,
    pub message: Option<&'a str>,
    pub mtime: Option<i64>,
}

/// Snapshot `content` for a path.
///
/// Deduplication is the load-bearing behaviour here: if the newest snapshot
/// for this path already has this content hash, nothing is written and the
/// existing snapshot is returned. That single rule makes re-saving free and
/// makes a headless bridge and the GUI watching the same root harmless.
/// Checkpoints are the one exception — a milestone marker on unchanged content
/// is exactly what the user asked for.
pub fn record(store: &Store, cfg: &Config, req: RecordRequest<'_>) -> Result<RecordOutcome> {
    if req.content.len() as u64 > cfg.max_blob_bytes {
        return Err(Error::TooLarge(format!(
            "{} is {} bytes; the per-file limit is {}",
            req.resolved.display(),
            req.content.len(),
            cfg.max_blob_bytes
        )));
    }

    let hash = sha256_hex(req.content);
    let path_key = corpus::fold(&req.resolved.path);
    let forced = req.source == Source::Checkpoint;

    if !forced {
        if let Some(existing) = latest(store, &req.resolved.path)? {
            if existing.blob_hash == hash {
                return Ok(RecordOutcome { snapshot: existing, created: false });
            }
        }
    }

    // Blob before index: a hard kill can leave an unreferenced blob, never an
    // index row pointing at content that is not there.
    store.blobs().put(req.content)?;

    let artifact_type = if corpus::is_markdown_path(&req.resolved.path) {
        corpus::detect_type(&req.resolved.path, &String::from_utf8_lossy(req.content))
    } else {
        ArtifactType::Asset
    };

    let now = now_ms();
    let window = now / cfg.coalesce_ms.max(1);
    let author = req
        .author
        .map(str::to_string)
        .unwrap_or_else(|| req.source.default_author().to_string());

    let id = {
        let conn = store.conn();
        let id = Store::alloc_id(&conn, "snapshots", "snap")?;
        let inserted = conn.execute(
            "INSERT INTO snapshots(id, root_id, path, path_key, blob_hash, size, mtime,
                                   source, author, client, message, artifact_type,
                                   created_at, coalesce_window)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14)",
            rusqlite::params![
                &id,
                &req.resolved.root.id,
                &req.resolved.path,
                &path_key,
                &hash,
                req.content.len() as i64,
                req.mtime,
                req.source.as_str(),
                &author,
                req.client,
                req.message,
                artifact_type.as_str(),
                now,
                window,
            ],
        );
        match inserted {
            Ok(_) => id,
            Err(rusqlite::Error::SqliteFailure(e, _)) if e.code == rusqlite::ErrorCode::ConstraintViolation => {
                // The coalescing index caught a concurrent writer — the other
                // process recorded this exact content in this window. Its row
                // is the truth; hand it back.
                drop(conn);
                let existing = find_by_hash_in_window(store, &path_key, &hash, window)?
                    .ok_or_else(|| Error::other("snapshot dedupe raced with itself"))?;
                return Ok(RecordOutcome { snapshot: existing, created: false });
            }
            Err(e) => return Err(e.into()),
        }
    };

    let snapshot = get(store, &id)?;
    Ok(RecordOutcome { snapshot, created: true })
}

/// Read a file from disk and snapshot it. The watcher's entry point.
pub fn record_from_disk(
    store: &Store,
    cfg: &Config,
    resolved: &Resolved,
    source: Source,
    author: Option<&str>,
    client: Option<&str>,
    message: Option<&str>,
) -> Result<RecordOutcome> {
    let bytes = std::fs::read(&resolved.fs_path)?;
    let mtime = std::fs::metadata(&resolved.fs_path)
        .ok()
        .and_then(|m| m.modified().ok())
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_millis() as i64);
    record(
        store,
        cfg,
        RecordRequest { resolved, content: &bytes, source, author, client, message, mtime },
    )
}

/// Write `content` to disk and snapshot it in one step, so the file and its
/// history can never disagree.
pub fn write_and_record(
    store: &Store,
    cfg: &Config,
    resolved: &Resolved,
    content: &str,
    source: Source,
    author: Option<&str>,
    client: Option<&str>,
    message: Option<&str>,
) -> Result<RecordOutcome> {
    atomic_write(&resolved.fs_path, content.as_bytes())?;
    let mtime = std::fs::metadata(&resolved.fs_path)
        .ok()
        .and_then(|m| m.modified().ok())
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_millis() as i64);
    record(
        store,
        cfg,
        RecordRequest {
            resolved,
            content: content.as_bytes(),
            source,
            author,
            client,
            message,
            mtime,
        },
    )
}

fn find_by_hash_in_window(
    store: &Store,
    path_key: &str,
    hash: &str,
    window: i64,
) -> Result<Option<Snapshot>> {
    let conn = store.conn();
    Ok(conn
        .query_row(
            "SELECT * FROM snapshots
             WHERE path_key = ?1 AND blob_hash = ?2 AND coalesce_window = ?3
             ORDER BY created_at DESC LIMIT 1",
            (path_key, hash, window),
            row_to_snapshot,
        )
        .optional()?)
}

// ---------------------------------------------------------------------------
// Reading
// ---------------------------------------------------------------------------

pub fn get(store: &Store, id: &str) -> Result<Snapshot> {
    let conn = store.conn();
    conn.query_row("SELECT * FROM snapshots WHERE id = ?1", [id], row_to_snapshot)
        .optional()?
        .ok_or_else(|| Error::not_found(format!("version {id}")))
}

pub fn latest(store: &Store, path: &str) -> Result<Option<Snapshot>> {
    let conn = store.conn();
    latest_conn(&conn, path)
}

pub fn latest_conn(conn: &Connection, path: &str) -> Result<Option<Snapshot>> {
    Ok(conn
        .query_row(
            "SELECT * FROM snapshots WHERE path_key = ?1 ORDER BY created_at DESC, rowid DESC LIMIT 1",
            [corpus::fold(path)],
            row_to_snapshot,
        )
        .optional()?)
}

/// The timeline for one document, newest first, with size deltas filled in.
pub fn list_versions(store: &Store, path: &str, limit: usize) -> Result<Vec<Snapshot>> {
    let conn = store.conn();
    let mut stmt = conn.prepare(
        "SELECT * FROM snapshots WHERE path_key = ?1 ORDER BY created_at DESC, rowid DESC LIMIT ?2",
    )?;
    let mut rows: Vec<Snapshot> = stmt
        .query_map(rusqlite::params![corpus::fold(path), limit as i64], row_to_snapshot)?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    for i in 0..rows.len() {
        let previous = rows.get(i + 1).map(|s| s.size);
        rows[i].size_delta = previous.map(|p| rows[i].size - p);
    }
    Ok(rows)
}

pub fn content(store: &Store, snapshot: &Snapshot) -> Result<String> {
    store.blobs().get_text(&snapshot.blob_hash)
}

pub fn content_by_id(store: &Store, id: &str) -> Result<String> {
    let snap = get(store, id)?;
    content(store, &snap)
}

/// Resolve a caller-supplied version reference: a snapshot id, `latest`, or a
/// negative offset like `~1` meaning "one version back".
pub fn resolve_ref(store: &Store, path: &str, reference: &str) -> Result<Snapshot> {
    let r = reference.trim();
    if r.is_empty() || r.eq_ignore_ascii_case("latest") || r.eq_ignore_ascii_case("current") {
        return latest(store, path)?
            .ok_or_else(|| Error::not_found(format!("no versions recorded for {path}")));
    }
    if let Some(rest) = r.strip_prefix('~') {
        let back: usize = rest
            .parse()
            .map_err(|_| Error::invalid(format!("bad version reference `{reference}`")))?;
        let versions = list_versions(store, path, back + 1)?;
        return versions
            .into_iter()
            .nth(back)
            .ok_or_else(|| Error::not_found(format!("{path} has no version {reference}")));
    }
    get(store, r)
}

/// Every snapshot since a moment, newest first. Powers "changed overnight".
pub fn since(store: &Store, ms: i64, limit: usize) -> Result<Vec<Snapshot>> {
    let conn = store.conn();
    let mut stmt = conn.prepare(
        "SELECT * FROM snapshots WHERE created_at >= ?1 ORDER BY created_at DESC LIMIT ?2",
    )?;
    let rows = stmt
        .query_map(rusqlite::params![ms, limit as i64], row_to_snapshot)?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    Ok(rows)
}

pub fn version_count(conn: &Connection, path: &str) -> Result<i64> {
    let n: i64 = conn.query_row(
        "SELECT COUNT(*) FROM snapshots WHERE path_key = ?1",
        [corpus::fold(path)],
        |r| r.get(0),
    )?;
    Ok(n)
}

/// Distinct paths that have any history, optionally filtered by root.
pub fn tracked_paths(store: &Store, root_id: Option<&str>) -> Result<Vec<String>> {
    let conn = store.conn();
    let mut out = Vec::new();
    match root_id {
        Some(id) => {
            let mut stmt = conn.prepare(
                "SELECT path, MAX(created_at) FROM snapshots WHERE root_id = ?1 GROUP BY path_key ORDER BY path",
            )?;
            let rows = stmt.query_map([id], |r| r.get::<_, String>(0))?;
            for row in rows {
                out.push(row?);
            }
        }
        None => {
            let mut stmt = conn.prepare(
                "SELECT path, MAX(created_at) FROM snapshots GROUP BY path_key ORDER BY path",
            )?;
            let rows = stmt.query_map([], |r| r.get::<_, String>(0))?;
            for row in rows {
                out.push(row?);
            }
        }
    }
    Ok(out)
}

// ---------------------------------------------------------------------------
// Restore and export
// ---------------------------------------------------------------------------

/// Put an old version back on disk. The restore is itself snapshotted, so
/// restores are undoable — there is no way to lose content by exploring history.
pub fn restore(store: &Store, cfg: &Config, resolved: &Resolved, version_id: &str) -> Result<RecordOutcome> {
    let target = get(store, version_id)?;
    if !crate::util::path_eq(&target.path, &resolved.path) {
        return Err(Error::invalid(format!(
            "version {version_id} belongs to {}, not {}",
            target.display,
            resolved.display()
        )));
    }
    let bytes = store.blobs().get(&target.blob_hash)?;
    atomic_write(&resolved.fs_path, &bytes)?;
    let message = format!(
        "restore {} ({})",
        short_id(&target.id),
        target.created_at_iso
    );
    record(
        store,
        cfg,
        RecordRequest {
            resolved,
            content: &bytes,
            source: Source::Restore,
            author: Some(AUTHOR_YOU),
            client: None,
            message: Some(&message),
            mtime: Some(now_ms()),
        },
    )
}

fn short_id(id: &str) -> &str {
    id
}

/// Export a document's history as a patch series, oldest first.
///
/// Export, do not entangle: this is the whole of Folio's git interop. It
/// produces something `git am` understands without Folio ever touching a
/// repository.
pub fn export_history(store: &Store, path: &str) -> Result<String> {
    let mut versions = list_versions(store, path, 100_000)?;
    versions.reverse();
    if versions.is_empty() {
        return Err(Error::not_found(format!("no versions recorded for {path}")));
    }

    let display = display_path(path);
    let relative = display.trim_start_matches("~/").to_string();
    let total = versions.len();
    let mut out = String::new();
    let mut previous = String::new();

    for (i, snap) in versions.iter().enumerate() {
        let current = store.blobs().get_text(&snap.blob_hash).unwrap_or_default();
        let subject = snap
            .message
            .clone()
            .filter(|m| !m.trim().is_empty())
            .unwrap_or_else(|| format!("{} change to {}", snap.source.as_str(), relative));
        let subject = subject.lines().next().unwrap_or("change").to_string();

        out.push_str(&format!("From {} Mon Sep 17 00:00:00 2001\n", snap.blob_hash));
        out.push_str(&format!(
            "From: {} <{}@folio.local>\n",
            snap.author,
            snap.author.replace(' ', "-").to_lowercase()
        ));
        out.push_str(&format!("Date: {}\n", snap.created_at_iso));
        out.push_str(&format!("Subject: [PATCH {}/{}] {}\n\n", i + 1, total, subject));
        out.push_str(&format!(
            "Recorded by Folio: source={} author={} version={}\n---\n",
            snap.source.as_str(),
            snap.author,
            snap.id
        ));
        out.push_str(&format!("diff --git a/{relative} b/{relative}\n"));
        out.push_str(&diff::unified(
            &previous,
            &current,
            &format!("a/{relative}"),
            &format!("b/{relative}"),
            3,
        ));
        out.push_str("-- \nFolio\n\n");
        previous = current;
    }

    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::corpus::{Policy, RootKind, Root};
    use crate::util::canonical_key;

    fn fixture() -> (tempfile::TempDir, Store, Config, Resolved) {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(&dir.path().join("store")).unwrap();
        let work = dir.path().join("work");
        std::fs::create_dir_all(&work).unwrap();
        let root_path = canonical_key(&work);
        let root = Root {
            id: "root_test".into(),
            display: root_path.clone(),
            path: root_path.clone(),
            kind: RootKind::Dir,
            policy: Policy::Auto,
            label: "work".into(),
            added_at: 0,
        };
        {
            let conn = store.conn();
            conn.execute(
                "INSERT INTO roots(id, path, path_key, kind, policy, label, added_at)
                 VALUES (?1, ?2, ?3, 'dir', 'auto', 'work', 0)",
                (&root.id, &root.path, crate::corpus::fold(&root.path)),
            )
            .unwrap();
        }
        let file = work.join("doc.md");
        std::fs::write(&file, "one\n").unwrap();
        let key = canonical_key(&file);
        let resolved = Resolved { root, fs_path: crate::util::to_fs_path(&key), path: key };
        (dir, store, Config::default(), resolved)
    }

    #[test]
    fn identical_content_creates_no_new_version() {
        let (_d, store, cfg, resolved) = fixture();
        let a = write_and_record(&store, &cfg, &resolved, "hello\n", Source::Save, None, None, None).unwrap();
        assert!(a.created);
        let b = write_and_record(&store, &cfg, &resolved, "hello\n", Source::Save, None, None, None).unwrap();
        assert!(!b.created, "re-saving unchanged content must not create a version");
        assert_eq!(a.snapshot.id, b.snapshot.id);
        assert_eq!(list_versions(&store, &resolved.path, 50).unwrap().len(), 1);
    }

    #[test]
    fn a_checkpoint_marks_a_milestone_even_when_nothing_changed() {
        let (_d, store, cfg, resolved) = fixture();
        write_and_record(&store, &cfg, &resolved, "hello\n", Source::Save, None, None, None).unwrap();
        let cp = record(
            &store,
            &cfg,
            RecordRequest {
                resolved: &resolved,
                content: b"hello\n",
                source: Source::Checkpoint,
                author: Some(AUTHOR_YOU),
                client: None,
                message: Some("before restructuring commands"),
                mtime: None,
            },
        )
        .unwrap();
        assert!(cp.created);
        assert_eq!(list_versions(&store, &resolved.path, 50).unwrap().len(), 2);
    }

    #[test]
    fn restore_round_trips_and_is_itself_undoable() {
        let (_d, store, cfg, resolved) = fixture();
        let first = write_and_record(&store, &cfg, &resolved, "v1\n", Source::Save, None, None, None).unwrap();
        write_and_record(&store, &cfg, &resolved, "v2\n", Source::Save, None, None, None).unwrap();

        restore(&store, &cfg, &resolved, &first.snapshot.id).unwrap();
        assert_eq!(std::fs::read_to_string(&resolved.fs_path).unwrap(), "v1\n");

        let timeline = list_versions(&store, &resolved.path, 50).unwrap();
        assert_eq!(timeline.len(), 3, "the restore is itself a version");
        assert_eq!(timeline[0].source, Source::Restore);
    }

    #[test]
    fn timeline_carries_source_author_and_size_delta() {
        let (_d, store, cfg, resolved) = fixture();
        write_and_record(&store, &cfg, &resolved, "short\n", Source::Save, None, None, None).unwrap();
        write_and_record(
            &store,
            &cfg,
            &resolved,
            "a much longer body\n",
            Source::Proposal,
            Some("claude-sonnet-4.6"),
            Some("claude-code"),
            Some("Tighten table threshold"),
        )
        .unwrap();
        let timeline = list_versions(&store, &resolved.path, 10).unwrap();
        assert_eq!(timeline[0].author, "claude-sonnet-4.6");
        assert_eq!(timeline[0].source, Source::Proposal);
        assert_eq!(timeline[0].size_delta, Some("a much longer body\n".len() as i64 - "short\n".len() as i64));
        assert_eq!(timeline[1].author, "you");
    }

    #[test]
    fn export_history_produces_an_applicable_patch_series() {
        let (_d, store, cfg, resolved) = fixture();
        write_and_record(&store, &cfg, &resolved, "one\n", Source::Save, None, None, None).unwrap();
        write_and_record(&store, &cfg, &resolved, "one\ntwo\n", Source::Save, None, None, Some("add two")).unwrap();
        let series = export_history(&store, &resolved.path).unwrap();
        assert!(series.contains("Subject: [PATCH 1/2]"));
        assert!(series.contains("Subject: [PATCH 2/2] add two"));
        assert!(series.contains("diff --git"));
    }

    #[test]
    fn version_refs_accept_latest_and_offsets() {
        let (_d, store, cfg, resolved) = fixture();
        write_and_record(&store, &cfg, &resolved, "one\n", Source::Save, None, None, None).unwrap();
        let second = write_and_record(&store, &cfg, &resolved, "two\n", Source::Save, None, None, None).unwrap();
        assert_eq!(resolve_ref(&store, &resolved.path, "latest").unwrap().id, second.snapshot.id);
        assert_eq!(content(&store, &resolve_ref(&store, &resolved.path, "~1").unwrap()).unwrap(), "one\n");
    }
}
