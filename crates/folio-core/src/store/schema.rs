//! The SQLite schema, applied idempotently on open.
//!
//! Everything here is an *index over* content that lives elsewhere: markdown
//! on disk, blobs in the blob store. Losing this database costs history, never
//! documents — which is the whole point of keeping files plain.

pub const SCHEMA_VERSION: i64 = 2;

pub const SCHEMA: &str = r#"
CREATE TABLE IF NOT EXISTS meta (
    key   TEXT PRIMARY KEY,
    value TEXT NOT NULL
);

-- A registered directory or single file. The corpus is the union of these.
CREATE TABLE IF NOT EXISTS roots (
    id       TEXT PRIMARY KEY,
    path     TEXT NOT NULL,
    path_key TEXT NOT NULL UNIQUE,
    kind     TEXT NOT NULL,               -- dir | file
    policy   TEXT NOT NULL,               -- propose | direct
    label    TEXT,
    added_at INTEGER NOT NULL
);

-- Per-path overrides of a root's write policy, matched as globs against the
-- path relative to the root. Most specific (longest pattern) wins.
CREATE TABLE IF NOT EXISTS path_policies (
    id         TEXT PRIMARY KEY,
    root_id    TEXT NOT NULL REFERENCES roots(id) ON DELETE CASCADE,
    pattern    TEXT NOT NULL,
    policy     TEXT NOT NULL,
    created_at INTEGER NOT NULL
);

-- Every version of every tracked file, from all six triggers.
CREATE TABLE IF NOT EXISTS snapshots (
    id              TEXT PRIMARY KEY,
    root_id         TEXT NOT NULL,
    path            TEXT NOT NULL,
    path_key        TEXT NOT NULL,
    blob_hash       TEXT NOT NULL,
    size            INTEGER NOT NULL,
    mtime           INTEGER,
    source          TEXT NOT NULL,        -- save|external|proposal|restore|checkpoint|direct
    author          TEXT NOT NULL,        -- you | <agent> | external
    client          TEXT,
    message         TEXT,
    artifact_type   TEXT NOT NULL,        -- skill|prompt|task_list|doc|asset
    created_at      INTEGER NOT NULL,
    coalesce_window INTEGER NOT NULL
);

-- The double-watcher guard, by design rather than by hope: the app and a
-- headless bridge watching the same root cannot write the same content twice
-- inside one coalescing window.
--
-- Scoped to `external` on purpose. Passive observation is the only source that
-- can fire twice for one write; every other source is somebody deciding
-- something, and a decision that happens to restore two-second-old content is
-- still a version. Deliberate no-ops are caught in code instead, by comparing
-- against the newest snapshot's hash.
CREATE UNIQUE INDEX IF NOT EXISTS snapshots_dedupe
    ON snapshots(path_key, blob_hash, coalesce_window)
    WHERE source = 'external';

CREATE INDEX IF NOT EXISTS snapshots_by_path ON snapshots(path_key, created_at DESC, id DESC);
CREATE INDEX IF NOT EXISTS snapshots_by_time ON snapshots(created_at DESC);
CREATE INDEX IF NOT EXISTS snapshots_by_root ON snapshots(root_id);

-- An agent's edit, waiting for human judgement.
CREATE TABLE IF NOT EXISTS proposals (
    id                 TEXT PRIMARY KEY,
    root_id            TEXT NOT NULL,
    path               TEXT NOT NULL,
    path_key           TEXT NOT NULL,
    base_snapshot_id   TEXT,
    base_blob_hash     TEXT,
    proposed_blob_hash TEXT NOT NULL,
    author             TEXT NOT NULL,
    client             TEXT NOT NULL,
    message            TEXT,
    addressing         TEXT,              -- comment id this proposal answers
    status             TEXT NOT NULL,     -- pending|accepted|rejected|superseded|conflict
    created_at         INTEGER NOT NULL,
    decided_at         INTEGER,
    decision_note      TEXT,
    result_snapshot_id TEXT
);

CREATE INDEX IF NOT EXISTS proposals_by_status ON proposals(status, created_at DESC);
CREATE INDEX IF NOT EXISTS proposals_by_path ON proposals(path_key, status);

-- Anchored review threads. Never written into the markdown itself.
CREATE TABLE IF NOT EXISTS comments (
    id                    TEXT PRIMARY KEY,
    path                  TEXT NOT NULL,
    path_key              TEXT NOT NULL,
    anchor_hash           TEXT NOT NULL,
    anchor_text           TEXT NOT NULL,
    context_before        TEXT NOT NULL,
    context_after         TEXT NOT NULL,
    author                TEXT NOT NULL,
    body                  TEXT NOT NULL,
    created_at            INTEGER NOT NULL,
    resolved_at           INTEGER,
    resolved_by           TEXT,
    addressed_by_proposal TEXT
);

CREATE INDEX IF NOT EXISTS comments_by_path ON comments(path_key, created_at DESC);

CREATE TABLE IF NOT EXISTS replies (
    id         TEXT PRIMARY KEY,
    comment_id TEXT NOT NULL REFERENCES comments(id) ON DELETE CASCADE,
    author     TEXT NOT NULL,
    body       TEXT NOT NULL,
    created_at INTEGER NOT NULL
);

CREATE INDEX IF NOT EXISTS replies_by_comment ON replies(comment_id, created_at);

-- Connected MCP clients, for the status bar and for attributing writes.
CREATE TABLE IF NOT EXISTS clients (
    name       TEXT PRIMARY KEY,
    mode       TEXT NOT NULL,             -- bridged | headless
    first_seen INTEGER NOT NULL,
    last_seen  INTEGER NOT NULL
);

-- Sliding-window counters behind the per-client reply rate limit. A looping
-- agent must not be able to flood a comment thread.
CREATE TABLE IF NOT EXISTS rate_events (
    id     INTEGER PRIMARY KEY AUTOINCREMENT,
    client TEXT NOT NULL,
    kind   TEXT NOT NULL,
    at     INTEGER NOT NULL
);

CREATE INDEX IF NOT EXISTS rate_events_window ON rate_events(client, kind, at);

-- Optional, local-only product instrumentation. Event names are deliberately
-- context-free: no document paths, content, search terms, or other properties
-- are collected. The user can inspect and clear this table from Preferences.
CREATE TABLE IF NOT EXISTS usage_events (
    id          INTEGER PRIMARY KEY AUTOINCREMENT,
    session     TEXT NOT NULL,
    event       TEXT NOT NULL,
    app_version TEXT NOT NULL,
    at          INTEGER NOT NULL
);

CREATE INDEX IF NOT EXISTS usage_events_by_time ON usage_events(at, id);
"#;
