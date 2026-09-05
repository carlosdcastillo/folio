//! The single entry point every shell goes through.
//!
//! The Tauri commands, the local IPC socket, and the `folio mcp` bridge all
//! call [`Folio::dispatch`] with the same operation names and the same JSON
//! shapes. That is what keeps the MCP surface honest: it cannot drift from the
//! UI, because there is only one implementation underneath both.

use crate::artifact::{self, prompt, task};
use crate::comment::{self, Comment};
use crate::config::Config;
use crate::corpus::{self, ArtifactType, Policy, Resolved, Root};
use crate::diff;
use crate::error::{Error, Result};
use crate::event::{Bus, ClientInfo, CommentActivityKind, Event};
use crate::proposal::{self, Change, CreateRequest, WriteOutcome};
use crate::search;
use crate::store::{self, Store};
use crate::util::{display_path, ms_to_rfc3339, now_ms};
use crate::version::{self, Source};
use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};
use std::path::Path;
use std::sync::{Arc, Mutex};

/// Who is asking. Agents and the human user share one API and one enforcement
/// point; they differ only in what they are allowed to decide.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Caller {
    /// Recorded as the snapshot author: `you`, or the agent's model name.
    pub author: String,
    /// The MCP client, e.g. `claude-code`.
    pub client: String,
    /// Human callers may decide: accept, reject, resolve, restore, configure.
    pub is_human: bool,
}

impl Caller {
    pub fn human() -> Caller {
        Caller { author: "you".into(), client: "folio-app".into(), is_human: true }
    }
    pub fn agent(author: impl Into<String>, client: impl Into<String>) -> Caller {
        Caller { author: author.into(), client: client.into(), is_human: false }
    }
    fn require_human(&self, what: &str) -> Result<()> {
        if self.is_human {
            Ok(())
        } else {
            Err(Error::PolicyDenied {
                path: what.to_string(),
                policy: "human-only".into(),
            })
        }
    }
}

/// A document as the sidebar and `list_docs` see it.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DocSummary {
    pub path: String,
    pub display: String,
    /// Path relative to its root, which is what the sidebar tree renders.
    pub relative: String,
    pub root_id: String,
    #[serde(rename = "type")]
    pub artifact_type: ArtifactType,
    pub versions: i64,
    pub pending_proposals: usize,
    pub open_comments: usize,
    pub size: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub latest_version: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub updated_at: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub updated_at_iso: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_author: Option<String>,
    /// False when the file has history but is no longer on disk.
    pub exists: bool,
    pub policy: Policy,
}

pub struct Folio {
    pub store: Store,
    pub config: Config,
    pub bus: Bus,
    watcher: Mutex<Option<crate::watch::Watcher>>,
}

impl Folio {
    pub fn open(store_dir: &Path) -> Result<Arc<Folio>> {
        let store = Store::open(store_dir)?;
        Ok(Arc::new(Folio {
            store,
            config: Config::default(),
            bus: Bus::new(),
            watcher: Mutex::new(None),
        }))
    }

    pub fn open_default() -> Result<Arc<Folio>> {
        Folio::open(&store::default_store_dir())
    }

    pub fn resolve(&self, path: &str) -> Result<Resolved> {
        corpus::resolve(&self.store, path)
    }

    pub fn roots(&self) -> Result<Vec<Root>> {
        corpus::list_roots(&self.store)
    }

    // -----------------------------------------------------------------------
    // Indexing and watching
    // -----------------------------------------------------------------------

    /// Snapshot everything in a root that is not already recorded. Cheap on a
    /// second run: unchanged files hash to the version already stored and
    /// write nothing.
    pub fn index_root(&self, root: &Root) -> Result<usize> {
        let mut indexed = 0usize;
        for file in corpus::walk_root(root) {
            let key = crate::util::canonical_key(&file);
            let resolved = Resolved {
                root: root.clone(),
                fs_path: crate::util::to_fs_path(&key),
                path: key,
            };
            match version::record_from_disk(
                &self.store,
                &self.config,
                &resolved,
                Source::External,
                None,
                None,
                None,
            ) {
                Ok(outcome) => {
                    if outcome.created {
                        indexed += 1;
                    }
                }
                // A file too large to snapshot, or one that vanished mid-walk,
                // must not abort indexing the rest of the corpus.
                Err(Error::TooLarge(_)) | Err(Error::Io(_)) => continue,
                Err(e) => return Err(e),
            }
        }
        Ok(indexed)
    }

    pub fn index_all(&self) -> Result<usize> {
        let mut total = 0usize;
        for root in self.roots()? {
            total += self.index_root(&root)?;
        }
        Ok(total)
    }

    /// Start watching every root. Idempotent: restarts cleanly.
    pub fn start_watching(self: &Arc<Self>) -> Result<()> {
        let watcher = crate::watch::Watcher::start(Arc::clone(self))?;
        let count = watcher.watching();
        *self.watcher.lock().unwrap_or_else(|e| e.into_inner()) = Some(watcher);
        self.bus.emit(Event::WatcherStatus { watching: count, healthy: true, message: None });
        Ok(())
    }

    pub fn stop_watching(&self) {
        *self.watcher.lock().unwrap_or_else(|e| e.into_inner()) = None;
        self.bus.emit(Event::WatcherStatus { watching: 0, healthy: true, message: None });
    }

    pub fn is_watching(&self) -> usize {
        self.watcher
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .as_ref()
            .map(|w| w.watching())
            .unwrap_or(0)
    }

    // -----------------------------------------------------------------------
    // Documents
    // -----------------------------------------------------------------------

    pub fn doc_summaries(
        &self,
        root_filter: Option<&str>,
        type_filter: Option<ArtifactType>,
    ) -> Result<Vec<DocSummary>> {
        let roots = self.roots()?;
        let pending = proposal::pending_counts(&self.store)?;
        let comments = comment::open_counts(&self.store)?;

        let rows: Vec<(String, String, String, i64, i64, String, i64, String)> = {
            let conn = self.store.conn();
            // One row per document: the newest snapshot, plus its version count.
            let mut stmt = conn.prepare(
                "SELECT s.path, s.path_key, s.root_id, s.created_at, s.size, s.artifact_type,
                        (SELECT COUNT(*) FROM snapshots c WHERE c.path_key = s.path_key) AS versions,
                        s.id
                 FROM snapshots s
                 JOIN (SELECT path_key, MAX(created_at) AS m, MAX(rowid) AS r
                       FROM snapshots GROUP BY path_key) latest
                   ON s.path_key = latest.path_key AND s.rowid = latest.r
                 ORDER BY s.path",
            )?;
            let rows = stmt
                .query_map([], |r| {
                    Ok((
                        r.get::<_, String>(0)?,
                        r.get::<_, String>(1)?,
                        r.get::<_, String>(2)?,
                        r.get::<_, i64>(3)?,
                        r.get::<_, i64>(4)?,
                        r.get::<_, String>(5)?,
                        r.get::<_, i64>(6)?,
                        r.get::<_, String>(7)?,
                    ))
                })?
                .collect::<std::result::Result<Vec<_>, _>>()?;
            rows
        };

        let mut out = Vec::with_capacity(rows.len());
        for (path, path_key, root_id, created_at, size, artifact_type, versions, latest_id) in rows {
            if let Some(want) = root_filter {
                if want != root_id {
                    continue;
                }
            }
            let artifact_type = ArtifactType::parse(&artifact_type);
            if let Some(want) = type_filter {
                if want != artifact_type {
                    continue;
                }
            }
            let Some(root) = roots.iter().find(|r| r.id == root_id) else {
                // The root was removed; its history is kept but it is no
                // longer part of the corpus.
                continue;
            };
            let last_author: Option<String> = {
                let conn = self.store.conn();
                conn.query_row(
                    "SELECT author FROM snapshots WHERE id = ?1",
                    [&latest_id],
                    |r| r.get(0),
                )
                .ok()
            };
            let policy = corpus::policy_for(
                &self.store,
                &Resolved {
                    root: root.clone(),
                    fs_path: crate::util::to_fs_path(&path),
                    path: path.clone(),
                },
                artifact_type,
            )?;
            out.push(DocSummary {
                relative: corpus::relative_to_root(root, &path),
                display: display_path(&path),
                exists: crate::util::to_fs_path(&path).exists(),
                pending_proposals: pending.get(&path_key).copied().unwrap_or(0),
                open_comments: comments.get(&path_key).copied().unwrap_or(0),
                root_id,
                artifact_type,
                versions,
                size,
                latest_version: Some(latest_id),
                updated_at: Some(created_at),
                updated_at_iso: Some(ms_to_rfc3339(created_at)),
                last_author,
                policy,
                path,
            });
        }
        Ok(out)
    }

    /// Current text of a document: from disk when it is there, from the newest
    /// snapshot when it is not.
    pub fn read_current(&self, resolved: &Resolved) -> Result<String> {
        if resolved.fs_path.exists() {
            return corpus::read_text(&resolved.fs_path, self.config.max_editor_bytes);
        }
        match version::latest(&self.store, &resolved.path)? {
            Some(snap) => version::content(&self.store, &snap),
            None => Err(Error::not_found(resolved.display())),
        }
    }

    /// Save from the editor. Human edits never become proposals — the gate is
    /// for agents, not for you.
    pub fn save_doc(&self, resolved: &Resolved, content: &str, message: Option<&str>) -> Result<Value> {
        let outcome = version::write_and_record(
            &self.store,
            &self.config,
            resolved,
            content,
            Source::Save,
            Some(version::AUTHOR_YOU),
            None,
            message,
        )?;
        if outcome.created {
            self.bus.emit(Event::SnapshotCreated { snapshot: Box::new(outcome.snapshot.clone()) });
        }
        Ok(json!({ "snapshot": outcome.snapshot, "created": outcome.created }))
    }

    // -----------------------------------------------------------------------
    // Today
    // -----------------------------------------------------------------------

    pub fn last_review_at(&self) -> i64 {
        self.store
            .get_meta("last_review_at")
            .ok()
            .flatten()
            .and_then(|v| v.parse::<i64>().ok())
            // First run: "overnight" means the last day.
            .unwrap_or_else(|| now_ms() - 24 * 60 * 60 * 1000)
    }

    /// The morning view: open tasks across the whole corpus, open comments,
    /// and every change since the last review session grouped by author.
    pub fn today(&self) -> Result<Value> {
        let since = self.last_review_at();
        let docs = self.doc_summaries(None, Some(ArtifactType::TaskList))?;

        let mut tasks: Vec<task::Task> = Vec::new();
        for doc in &docs {
            if !doc.exists {
                continue;
            }
            let Ok(content) = corpus::read_text(&crate::util::to_fs_path(&doc.path), self.config.max_editor_bytes)
            else {
                continue;
            };
            for mut t in task::parse(&content) {
                t.doc = Some(doc.path.clone());
                t.display = Some(doc.display.clone());
                t.version = doc.latest_version.clone();
                tasks.push(t);
            }
        }

        let open: Vec<&task::Task> = tasks.iter().filter(|t| !t.done).collect();
        let by_owner = group_tasks(&open, |t| t.owner.clone().unwrap_or_else(|| "unassigned".into()));
        let mut by_tag: Map<String, Value> = Map::new();
        for t in &open {
            if t.tags.is_empty() {
                by_tag.entry("untagged".to_string()).or_insert_with(|| json!([]));
            }
            for tag in &t.tags {
                by_tag
                    .entry(tag.clone())
                    .or_insert_with(|| json!([]))
                    .as_array_mut()
                    .unwrap()
                    .push(serde_json::to_value(t)?);
            }
        }

        let open_comments = comment::list(
            &self.store,
            &comment::ListFilter { status: Some(comment::Status::Open), limit: 200, ..Default::default() },
        )?;
        let outdated_comments = comment::list(
            &self.store,
            &comment::ListFilter { status: Some(comment::Status::Outdated), limit: 200, ..Default::default() },
        )?;

        let changes = version::since(&self.store, since, 500)?;
        let mut by_author: Map<String, Value> = Map::new();
        for snap in &changes {
            by_author
                .entry(snap.author.clone())
                .or_insert_with(|| json!([]))
                .as_array_mut()
                .unwrap()
                .push(serde_json::to_value(snap)?);
        }

        let pending = proposal::list(
            &self.store,
            &proposal::ListFilter { limit: 200, ..Default::default() },
        )?
        .into_iter()
        .filter(|p| p.status.is_open())
        .collect::<Vec<_>>();

        Ok(json!({
            "since": since,
            "since_iso": ms_to_rfc3339(since),
            "tasks": {
                "open": open.len(),
                "total": tasks.len(),
                "by_owner": by_owner,
                "by_tag": Value::Object(by_tag),
                "all": open,
            },
            "comments": {
                "open": open_comments.len(),
                "outdated": outdated_comments.len(),
                "threads": open_comments,
                "outdated_threads": outdated_comments,
            },
            "changes": {
                "total": changes.len(),
                "by_author": Value::Object(by_author),
                "all": changes,
            },
            "proposals": {
                "pending": pending.len(),
                "all": pending,
            }
        }))
    }

    pub fn status(&self) -> Result<Value> {
        let clients: Vec<ClientInfo> = self
            .store
            .active_clients(self.config.client_active_window_ms)?
            .into_iter()
            .map(|(name, mode, last_seen)| ClientInfo { name, mode, last_seen })
            .collect();
        let pending: i64 = {
            let conn = self.store.conn();
            conn.query_row("SELECT COUNT(*) FROM proposals WHERE status = 'pending'", [], |r| r.get(0))?
        };
        let open_comments = comment::open_counts(&self.store)?.values().sum::<usize>();
        Ok(json!({
            "version": crate::VERSION,
            "store_dir": self.store.dir().to_string_lossy(),
            "store_bytes": self.store.size_bytes(),
            "cloud_sync_warning": self.store.cloud_sync_warning(),
            "watching": self.is_watching(),
            "clients": clients,
            "pending_proposals": pending,
            "open_comments": open_comments,
            "roots": self.roots()?.len(),
        }))
    }

    // -----------------------------------------------------------------------
    // Dispatch
    // -----------------------------------------------------------------------

    /// Every operation the product has, by name. This table *is* the API.
    pub fn dispatch(&self, caller: &Caller, op: &str, params: &Value) -> Result<Value> {
        let p = Params(params);
        match op {
            // -- corpus -----------------------------------------------------
            "list_roots" => {
                let roots = self.roots()?;
                let conn = self.store.conn();
                let mut out = Vec::new();
                for root in roots {
                    let docs = corpus::count_docs(&conn, &root.id)?;
                    let mut value = serde_json::to_value(&root)?;
                    value["docs"] = json!(docs);
                    out.push(value);
                }
                Ok(json!({ "roots": out }))
            }
            // Registering a root is explicitly an MCP capability, not a
            // human-only one: an agent that knows where its skills live should
            // be able to put them under version control.
            "add_root" => {
                let policy = p.opt_str("policy").map(|s| Policy::parse(&s)).unwrap_or(Policy::Auto);
                let root = corpus::add_root(&self.store, &p.req_str("path")?, policy, p.opt_str("label").as_deref())?;
                let indexed = self.index_root(&root)?;
                self.bus.emit(Event::CorpusChanged);
                Ok(json!({ "root": root, "indexed": indexed }))
            }
            "remove_root" => {
                caller.require_human("remove_root")?;
                corpus::remove_root(&self.store, &p.req_str("id")?)?;
                self.bus.emit(Event::CorpusChanged);
                Ok(json!({ "ok": true }))
            }
            "set_root_policy" => {
                caller.require_human("set_root_policy")?;
                let root = corpus::set_root_policy(
                    &self.store,
                    &p.req_str("id")?,
                    Policy::parse(&p.req_str("policy")?),
                )?;
                self.bus.emit(Event::CorpusChanged);
                Ok(json!({ "root": root }))
            }
            "set_path_policy" => {
                caller.require_human("set_path_policy")?;
                let policy = corpus::set_path_policy(
                    &self.store,
                    &p.req_str("root_id")?,
                    &p.req_str("pattern")?,
                    Policy::parse(&p.req_str("policy")?),
                )?;
                self.bus.emit(Event::CorpusChanged);
                Ok(json!({ "policy": policy }))
            }
            "remove_path_policy" => {
                caller.require_human("remove_path_policy")?;
                corpus::remove_path_policy(&self.store, &p.req_str("id")?)?;
                self.bus.emit(Event::CorpusChanged);
                Ok(json!({ "ok": true }))
            }
            "list_path_policies" => Ok(json!({
                "policies": corpus::list_path_policies(&self.store, &p.req_str("root_id")?)?
            })),
            "index_root" => {
                let root = corpus::get_root(&self.store, &p.req_str("id")?)?;
                Ok(json!({ "indexed": self.index_root(&root)? }))
            }
            "index_all" => Ok(json!({ "indexed": self.index_all()? })),

            "list_docs" => {
                let root = p.opt_str("root");
                let root_id = match &root {
                    Some(value) => Some(self.root_id_for(value)?),
                    None => None,
                };
                let type_filter = p.opt_str("type").map(|t| ArtifactType::parse(&t));
                Ok(json!({ "docs": self.doc_summaries(root_id.as_deref(), type_filter)? }))
            }
            "read_doc" => {
                let resolved = self.resolve(&p.req_str("path")?)?;
                let content = self.read_current(&resolved)?;
                let fm = artifact::frontmatter::parse(&content);
                let artifact_type = corpus::detect_type(&resolved.path, &content);
                let latest = version::latest(&self.store, &resolved.path)?;
                let policy = corpus::policy_for(&self.store, &resolved, artifact_type)?;
                let versions = {
                    let conn = self.store.conn();
                    version::version_count(&conn, &resolved.path)?
                };
                Ok(json!({
                    "path": resolved.path,
                    "display": resolved.display(),
                    "content": content,
                    "frontmatter": Value::Object(fm.data.clone()),
                    "frontmatter_present": fm.present,
                    "frontmatter_error": fm.parse_error,
                    "body_line": fm.body_line,
                    "type": artifact_type,
                    "version": latest.as_ref().map(|s| s.id.clone()),
                    "versions": versions,
                    "policy": policy,
                    "exists": resolved.fs_path.exists(),
                }))
            }
            "create_doc" => {
                caller.require_human("create_doc")?;
                let resolved = self.resolve(&p.req_str("path")?)?;
                if resolved.fs_path.exists() {
                    return Err(Error::invalid(format!("{} already exists", resolved.display())));
                }
                let content = p.opt_text("content").unwrap_or_default();
                let outcome = version::write_and_record(
                    &self.store,
                    &self.config,
                    &resolved,
                    &content,
                    Source::Save,
                    Some(version::AUTHOR_YOU),
                    None,
                    Some("created"),
                )?;
                self.bus.emit(Event::SnapshotCreated { snapshot: Box::new(outcome.snapshot.clone()) });
                Ok(json!({ "snapshot": outcome.snapshot, "path": resolved.path }))
            }
            "save_doc" => {
                caller.require_human("save_doc")?;
                let resolved = self.resolve(&p.req_str("path")?)?;
                self.save_doc(&resolved, &p.req_text("content")?, p.opt_str("message").as_deref())
            }
            "search_docs" => {
                let query = search::Query {
                    pattern: p.req_str("query")?,
                    regex: p.opt_bool("regex").unwrap_or(false),
                    glob: p.opt_str("glob"),
                    case_sensitive: p.opt_bool("case_sensitive").unwrap_or(false),
                    max_results: p.opt_usize("max_results").unwrap_or(200),
                    ..Default::default()
                };
                let matches = search::search(&self.roots()?, &query)?;
                Ok(json!({ "matches": matches, "count": matches.len() }))
            }

            // -- versioning -------------------------------------------------
            "list_versions" => {
                let resolved = self.resolve(&p.req_str("path")?)?;
                Ok(json!({
                    "versions": version::list_versions(&self.store, &resolved.path, p.opt_usize("limit").unwrap_or(200))?
                }))
            }
            "read_version" => {
                let resolved = self.resolve(&p.req_str("path")?)?;
                let reference = p.opt_str("version_id").or_else(|| p.opt_str("version")).unwrap_or_default();
                let snapshot = version::resolve_ref(&self.store, &resolved.path, &reference)?;
                let content = version::content(&self.store, &snapshot)?;
                Ok(json!({ "version": snapshot, "content": content }))
            }
            "diff_versions" => {
                let resolved = self.resolve(&p.req_str("path")?)?;
                let from = version::resolve_ref(&self.store, &resolved.path, &p.opt_str("from").unwrap_or_else(|| "~1".into()))?;
                let to = version::resolve_ref(&self.store, &resolved.path, &p.opt_str("to").unwrap_or_else(|| "latest".into()))?;
                let old = version::content(&self.store, &from)?;
                let new = version::content(&self.store, &to)?;
                let d = diff::diff_text(&old, &new);
                Ok(json!({
                    "from": from, "to": to,
                    "old": old, "new": new,
                    "diff": d,
                    "unified": diff::unified(&old, &new, &format!("a/{}", from.display), &format!("b/{}", to.display), 3),
                }))
            }
            "checkpoint" => {
                let resolved = self.resolve(&p.req_str("path")?)?;
                let content = self.read_current(&resolved)?;
                let message = p.req_str("message")?;
                let outcome = version::record(
                    &self.store,
                    &self.config,
                    version::RecordRequest {
                        resolved: &resolved,
                        content: content.as_bytes(),
                        source: Source::Checkpoint,
                        author: Some(&caller.author),
                        client: if caller.is_human { None } else { Some(&caller.client) },
                        message: Some(&message),
                        mtime: None,
                    },
                )?;
                self.bus.emit(Event::SnapshotCreated { snapshot: Box::new(outcome.snapshot.clone()) });
                Ok(json!({ "snapshot": outcome.snapshot }))
            }
            "restore_version" => {
                caller.require_human("restore_version")?;
                let resolved = self.resolve(&p.req_str("path")?)?;
                let outcome = version::restore(&self.store, &self.config, &resolved, &p.req_str("version_id")?)?;
                self.bus.emit(Event::SnapshotCreated { snapshot: Box::new(outcome.snapshot.clone()) });
                Ok(json!({ "snapshot": outcome.snapshot }))
            }
            "export_history" => {
                let resolved = self.resolve(&p.req_str("path")?)?;
                let series = version::export_history(&self.store, &resolved.path)?;
                Ok(json!({ "path": resolved.path, "display": resolved.display(), "patch_series": series }))
            }

            // -- writing ----------------------------------------------------
            "propose_edit" => self.propose_edit(caller, &p),
            "task_list" => self.task_list(&p),
            "task_add" => self.task_add(caller, &p),
            "task_set_status" => self.task_set_status(caller, &p),

            // -- artifact intelligence --------------------------------------
            "validate_doc" => {
                let resolved = self.resolve(&p.req_str("path")?)?;
                let content = self.read_current(&resolved)?;
                Ok(serde_json::to_value(artifact::validate(&resolved.path, &content))?)
            }
            "validate_all" => {
                let type_filter = p.opt_str("type").map(|t| ArtifactType::parse(&t)).or(Some(ArtifactType::Skill));
                let mut reports = Vec::new();
                for doc in self.doc_summaries(None, type_filter)? {
                    if !doc.exists {
                        continue;
                    }
                    let Ok(content) = corpus::read_text(&crate::util::to_fs_path(&doc.path), self.config.max_editor_bytes) else {
                        continue;
                    };
                    reports.push(artifact::validate(&doc.path, &content));
                }
                let errors: usize = reports.iter().map(|r| r.errors).sum();
                let warnings: usize = reports.iter().map(|r| r.warnings).sum();
                Ok(json!({ "reports": reports, "errors": errors, "warnings": warnings }))
            }
            "render_prompt" => {
                let resolved = self.resolve(&p.req_str("path")?)?;
                let content = self.read_current(&resolved)?;
                let variables = match params.get("variables") {
                    Some(Value::Object(map)) => map.clone(),
                    Some(Value::Null) | None => Map::new(),
                    Some(other) => {
                        return Err(Error::invalid(format!(
                            "`variables` must be an object, got {other}"
                        )))
                    }
                };
                let rendered = prompt::render(&content, &variables)?;
                Ok(json!({
                    "path": resolved.path,
                    "display": resolved.display(),
                    "rendered": rendered,
                    "declared": prompt::declared_variables(&content),
                    "used": prompt::used_slots(&content),
                }))
            }

            // -- proposals ---------------------------------------------------
            "list_proposals" => {
                let mut filter = proposal::ListFilter {
                    status: p.opt_str("status").and_then(|s| proposal::Status::parse(&s)),
                    path: p.opt_str("path"),
                    limit: p.opt_usize("limit").unwrap_or(0),
                    client: None,
                };
                if p.opt_bool("mine_only").unwrap_or(false) {
                    filter.client = Some(caller.client.clone());
                }
                if let Some(client) = p.opt_str("client") {
                    filter.client = Some(client);
                }
                let proposals = proposal::list(&self.store, &filter)?;
                Ok(json!({ "proposals": proposals, "count": proposals.len() }))
            }
            "get_proposal" => Ok(json!({ "proposal": proposal::get(&self.store, &p.req_str("id")?)? })),
            "proposal_diff" => {
                let proposal = proposal::get(&self.store, &p.req_str("id")?)?;
                let (current, proposed, d) = proposal::review_diff(&self.store, &proposal)?;
                let addressed = match &proposal.addressing {
                    Some(id) => comment::get(&self.store, id).ok(),
                    None => None,
                };
                Ok(json!({
                    "proposal": proposal,
                    "current": current,
                    "proposed": proposed,
                    "diff": d,
                    "addresses": addressed,
                }))
            }
            "accept_proposal" => {
                caller.require_human("accept_proposal")?;
                let id = p.req_str("id")?;
                let target = proposal::get(&self.store, &id)?;
                let resolved = self.resolve(&target.path)?;
                let hunks = p.opt_usize_list("hunks");
                let decision = proposal::accept(
                    &self.store,
                    &self.config,
                    &resolved,
                    &id,
                    hunks.as_deref(),
                )?;
                if let Some(snapshot) = &decision.snapshot {
                    self.bus.emit(Event::SnapshotCreated { snapshot: Box::new(snapshot.clone()) });
                }
                self.bus.emit(Event::ProposalDecided { proposal: Box::new(decision.proposal.clone()) });
                if let Some(comment_id) = &decision.resolved_comment {
                    if let Ok(c) = comment::get(&self.store, comment_id) {
                        self.bus.emit(Event::CommentActivity {
                            kind: CommentActivityKind::Resolved,
                            comment: Box::new(c),
                        });
                    }
                }
                Ok(serde_json::to_value(decision)?)
            }
            "reject_proposal" => {
                caller.require_human("reject_proposal")?;
                let decision = proposal::reject(&self.store, &p.req_str("id")?, p.opt_str("note").as_deref())?;
                self.bus.emit(Event::ProposalDecided { proposal: Box::new(decision.proposal.clone()) });
                Ok(serde_json::to_value(decision)?)
            }
            "rebase_proposal" => {
                caller.require_human("rebase_proposal")?;
                let proposal = proposal::rebase(&self.store, &p.req_str("id")?)?;
                self.bus.emit(Event::ProposalDecided { proposal: Box::new(proposal.clone()) });
                Ok(json!({ "proposal": proposal }))
            }

            // -- comments ----------------------------------------------------
            "list_comments" => {
                let path = match p.opt_str("path") {
                    Some(path) => Some(self.resolve(&path)?.path),
                    None => None,
                };
                let mut filter = comment::ListFilter {
                    path,
                    status: p.opt_str("status").and_then(|s| comment::Status::parse(&s)),
                    participant: None,
                    limit: p.opt_usize("limit").unwrap_or(0),
                };
                if p.opt_bool("mine_only").unwrap_or(false) {
                    filter.participant = Some(caller.author.clone());
                }
                let threads = comment::list(&self.store, &filter)?;
                Ok(json!({ "comments": threads, "count": threads.len() }))
            }
            "get_comment" => Ok(json!({ "comment": comment::get(&self.store, &p.req_str("comment_id")?)? })),
            "create_comment" => {
                // Only the human user creates comments; agents reply and address.
                caller.require_human("create_comment")?;
                let resolved = self.resolve(&p.req_str("path")?)?;
                let content = self.read_current(&resolved)?;
                let created = comment::create(
                    &self.store,
                    &self.config,
                    &resolved,
                    &content,
                    p.req_usize("selection_start")?,
                    p.req_usize("selection_end")?,
                    &p.req_str("body")?,
                )?;
                self.emit_comment(CommentActivityKind::Created, &created);
                Ok(json!({ "comment": created }))
            }
            "reply_comment" => {
                let comment_id = p.req_str("comment_id")?;
                let reply = comment::reply(
                    &self.store,
                    &self.config,
                    &comment_id,
                    &caller.author,
                    &p.req_str("body")?,
                    if caller.is_human { None } else { Some(&caller.client) },
                )?;
                if let Ok(thread) = comment::get(&self.store, &comment_id) {
                    self.emit_comment(CommentActivityKind::Replied, &thread);
                }
                Ok(json!({ "reply": reply }))
            }
            "resolve_comment" => {
                caller.require_human("resolve_comment")?;
                let thread = comment::resolve_thread(&self.store, &p.req_str("comment_id")?, &caller.author)?;
                self.emit_comment(CommentActivityKind::Resolved, &thread);
                Ok(json!({ "comment": thread }))
            }
            "reopen_comment" => {
                caller.require_human("reopen_comment")?;
                let thread = comment::reopen_thread(&self.store, &p.req_str("comment_id")?)?;
                self.emit_comment(CommentActivityKind::Reopened, &thread);
                Ok(json!({ "comment": thread }))
            }
            "delete_comment" => {
                caller.require_human("delete_comment")?;
                let id = p.req_str("comment_id")?;
                let thread = comment::get(&self.store, &id)?;
                comment::delete(&self.store, &id)?;
                self.emit_comment(CommentActivityKind::Deleted, &thread);
                Ok(json!({ "ok": true }))
            }

            // -- views and housekeeping --------------------------------------
            "today" => self.today(),
            "status" => self.status(),
            "mark_reviewed" => {
                caller.require_human("mark_reviewed")?;
                let now = now_ms();
                self.store.set_meta("last_review_at", &now.to_string())?;
                Ok(json!({ "last_review_at": now, "last_review_at_iso": ms_to_rfc3339(now) }))
            }
            "start_watching" => {
                caller.require_human("start_watching")?;
                Err(Error::UnknownOp(
                    "start_watching is driven by the app shell, not by dispatch".into(),
                ))
            }
            "touch_client" => {
                let name = p.req_str("name")?;
                self.store.touch_client(&name, &p.opt_str("mode").unwrap_or_else(|| "headless".into()))?;
                let clients: Vec<ClientInfo> = self
                    .store
                    .active_clients(self.config.client_active_window_ms)?
                    .into_iter()
                    .map(|(name, mode, last_seen)| ClientInfo { name, mode, last_seen })
                    .collect();
                self.bus.emit(Event::ClientsChanged { clients });
                Ok(json!({ "ok": true }))
            }
            // Small scraps of UI state the *shell* also needs to read — the
            // theme, so the window can be painted in it before the webview has
            // said anything. Namespaced so this can never become a general
            // key-value store for agents.
            "ui_state_set" => {
                caller.require_human("ui_state_set")?;
                let key = p.req_str("key")?;
                self.store
                    .set_meta(&format!("ui.{key}"), &p.opt_str("value").unwrap_or_default())?;
                Ok(json!({ "ok": true }))
            }
            "ui_state_get" => {
                let key = p.req_str("key")?;
                Ok(json!({ "value": self.store.get_meta(&format!("ui.{key}"))? }))
            }
            "config" => Ok(serde_json::to_value(&self.config)?),
            "ping" => Ok(json!({ "ok": true, "version": crate::VERSION })),

            other => Err(Error::UnknownOp(other.to_string())),
        }
    }

    fn emit_comment(&self, kind: CommentActivityKind, thread: &Comment) {
        self.bus.emit(Event::CommentActivity { kind, comment: Box::new(thread.clone()) });
    }

    /// A root reference may be an id, a label, or a path.
    fn root_id_for(&self, value: &str) -> Result<String> {
        let roots = self.roots()?;
        if let Some(root) = roots.iter().find(|r| r.id == value) {
            return Ok(root.id.clone());
        }
        if let Some(root) = roots.iter().find(|r| r.label.eq_ignore_ascii_case(value)) {
            return Ok(root.id.clone());
        }
        let key = crate::util::canonical_key(&crate::util::expand_tilde(value));
        roots
            .iter()
            .find(|r| crate::util::path_eq(&r.path, &key))
            .map(|r| r.id.clone())
            .ok_or_else(|| Error::not_found(format!("root `{value}`")))
    }

    // -----------------------------------------------------------------------
    // Writing operations, split out for readability
    // -----------------------------------------------------------------------

    fn propose_edit(&self, caller: &Caller, p: &Params<'_>) -> Result<Value> {
        let resolved = self.resolve(&p.req_str("path")?)?;
        let content = p.opt_text("content");
        let patch = p.opt_text("patch");
        let change = match (&content, &patch) {
            (Some(c), None) => Change::Content(c),
            (None, Some(patch)) => Change::Patch(patch),
            (Some(_), Some(_)) => {
                return Err(Error::invalid("pass either `content` or `patch`, not both"))
            }
            (None, None) => return Err(Error::invalid("pass either `content` or `patch`")),
        };

        let outcome = proposal::write(
            &self.store,
            &self.config,
            CreateRequest {
                resolved: &resolved,
                change,
                author: &caller.author,
                client: &caller.client,
                message: p.opt_str("message").as_deref(),
                addressing: p.opt_str("addressing").as_deref(),
            },
        )?;

        match &outcome {
            WriteOutcome::Proposed { proposal } => {
                self.bus.emit(Event::ProposalArrived { proposal: proposal.clone() });
            }
            WriteOutcome::Applied { snapshot } => {
                self.bus.emit(Event::SnapshotCreated { snapshot: snapshot.clone() });
            }
        }
        Ok(serde_json::to_value(outcome)?)
    }

    /// Read tasks from one document or from the whole corpus.
    fn task_list(&self, p: &Params<'_>) -> Result<Value> {
        let filter = task::Filter {
            owner: p.opt_str("owner"),
            tag: p.opt_str("tag"),
            open_only: p.opt_bool("open_only").unwrap_or(false),
            query: p.opt_str("query"),
        };

        let docs: Vec<DocSummary> = match p.opt_str("doc") {
            Some(path) => {
                let resolved = self.resolve(&path)?;
                self.doc_summaries(None, None)?
                    .into_iter()
                    .filter(|d| crate::util::path_eq(&d.path, &resolved.path))
                    .collect()
            }
            None => self.doc_summaries(None, Some(ArtifactType::TaskList))?,
        };

        let mut tasks = Vec::new();
        for doc in &docs {
            if !doc.exists {
                continue;
            }
            let Ok(content) = corpus::read_text(&crate::util::to_fs_path(&doc.path), self.config.max_editor_bytes)
            else {
                continue;
            };
            for mut t in task::parse(&content) {
                if !filter.matches(&t) {
                    continue;
                }
                t.doc = Some(doc.path.clone());
                t.display = Some(doc.display.clone());
                t.version = doc.latest_version.clone();
                tasks.push(t);
            }
        }

        Ok(json!({
            "tasks": tasks,
            "count": tasks.len(),
            "note": "Task ids are line-anchored and valid only for the `version` they were read from. Pass that `version` back to task_set_status.",
        }))
    }

    fn task_add(&self, caller: &Caller, p: &Params<'_>) -> Result<Value> {
        let resolved = self.resolve(&p.req_str("doc")?)?;
        let content = self.read_current(&resolved)?;
        let tags: Vec<String> = p.opt_str_list("tag").or_else(|| p.opt_str_list("tags")).unwrap_or_default();
        let (updated, line) = task::add(&content, &p.req_str("text")?, p.opt_str("owner").as_deref(), &tags)?;

        let message = p
            .opt_str("message")
            .unwrap_or_else(|| format!("add task: {}", p.req_str("text").unwrap_or_default()));
        let outcome = self.route_task_write(caller, &resolved, &updated, &message)?;
        Ok(merge(outcome, json!({ "task_id": task::id_for_line(line), "line": line })))
    }

    fn task_set_status(&self, caller: &Caller, p: &Params<'_>) -> Result<Value> {
        let resolved = self.resolve(&p.req_str("doc")?)?;
        let content = self.read_current(&resolved)?;
        let latest = version::latest(&self.store, &resolved.path)?;
        let task_id = p.req_str("task_id")?;

        // Stale-snapshot protection. Agents mutate task lists the way they
        // should: read, then write. A stale write fails with the current list
        // attached so the re-read costs no extra round trip.
        let stale = |reason: String| -> Error {
            let tasks = task::parse(&content)
                .into_iter()
                .map(|mut t| {
                    t.doc = Some(resolved.path.clone());
                    t.display = Some(resolved.display());
                    t.version = latest.as_ref().map(|s| s.id.clone());
                    t
                })
                .collect::<Vec<_>>();
            Error::stale(
                reason,
                json!({
                    "doc": resolved.path,
                    "version": latest.as_ref().map(|s| s.id.clone()),
                    "tasks": tasks,
                }),
            )
        };

        if let Some(claimed) = p.opt_str("version") {
            let current = latest.as_ref().map(|s| s.id.as_str()).unwrap_or("");
            if claimed != current {
                return Err(stale(format!(
                    "{} has moved on since version {claimed}; re-read the list before writing",
                    resolved.display()
                )));
            }
        }

        let status = p.req_str("status")?;
        let done = match status.trim().to_lowercase().as_str() {
            "done" | "x" | "closed" | "complete" | "completed" | "true" => true,
            "open" | "todo" | " " | "false" | "incomplete" => false,
            other => return Err(Error::invalid(format!("unknown status `{other}`; use `done` or `open`"))),
        };

        if let Some(expected) = p.opt_str("text") {
            let line = task::line_of_id(&task_id)?;
            let actual = task::parse(&content).into_iter().find(|t| t.line == line);
            match actual {
                Some(t) if t.text.trim() != expected.trim() => {
                    return Err(stale(format!(
                        "{task_id} is now `{}`, not `{expected}`; re-read the list before writing",
                        t.text
                    )))
                }
                None => return Err(stale(format!("{task_id} is no longer a task"))),
                Some(_) => {}
            }
        }

        let updated = match task::set_status(&content, &task_id, done) {
            Ok(updated) => updated,
            Err(Error::NotFound(reason)) => return Err(stale(reason)),
            Err(e) => return Err(e),
        };
        if updated == content {
            return Ok(json!({ "outcome": "unchanged", "task_id": task_id }));
        }

        let message = format!("{task_id} -> {}", if done { "done" } else { "open" });
        Ok(self.route_task_write(caller, &resolved, &updated, &message)?)
    }

    /// Task writes obey the same policy layer as every other write; the
    /// default just happens to be `direct` for task lists.
    fn route_task_write(
        &self,
        caller: &Caller,
        resolved: &Resolved,
        updated: &str,
        message: &str,
    ) -> Result<Value> {
        if caller.is_human {
            return self.save_doc(resolved, updated, Some(message));
        }
        let outcome = proposal::write(
            &self.store,
            &self.config,
            CreateRequest {
                resolved,
                change: Change::Content(updated),
                author: &caller.author,
                client: &caller.client,
                message: Some(message),
                addressing: None,
            },
        )?;
        match &outcome {
            WriteOutcome::Proposed { proposal } => {
                self.bus.emit(Event::ProposalArrived { proposal: proposal.clone() })
            }
            WriteOutcome::Applied { snapshot } => {
                self.bus.emit(Event::SnapshotCreated { snapshot: snapshot.clone() })
            }
        }
        Ok(serde_json::to_value(outcome)?)
    }
}

fn merge(mut base: Value, extra: Value) -> Value {
    if let (Some(base_map), Value::Object(extra_map)) = (base.as_object_mut(), extra) {
        for (k, v) in extra_map {
            base_map.insert(k, v);
        }
    }
    base
}

fn group_tasks(tasks: &[&task::Task], key: impl Fn(&task::Task) -> String) -> Value {
    let mut map: Map<String, Value> = Map::new();
    for t in tasks {
        map.entry(key(t))
            .or_insert_with(|| json!([]))
            .as_array_mut()
            .unwrap()
            .push(serde_json::to_value(t).unwrap_or(Value::Null));
    }
    Value::Object(map)
}

// ---------------------------------------------------------------------------
// Parameter access
// ---------------------------------------------------------------------------

/// Thin accessor over the JSON params, so every operation reports a missing or
/// mistyped argument the same way.
pub struct Params<'a>(pub &'a Value);

impl Params<'_> {
    pub fn req_str(&self, key: &str) -> Result<String> {
        self.opt_str(key)
            .ok_or_else(|| Error::invalid(format!("`{key}` is required")))
    }

    /// Like `opt_str`, but an empty string is a value rather than an absence.
    /// Emptying a document is a legitimate edit, and `opt_str` would silently
    /// turn it into "you passed no content".
    pub fn opt_text(&self, key: &str) -> Option<String> {
        match self.0.get(key) {
            Some(Value::String(s)) => Some(s.clone()),
            _ => self.opt_str(key),
        }
    }

    pub fn req_text(&self, key: &str) -> Result<String> {
        self.opt_text(key)
            .ok_or_else(|| Error::invalid(format!("`{key}` is required")))
    }

    pub fn opt_str(&self, key: &str) -> Option<String> {
        match self.0.get(key) {
            Some(Value::String(s)) if !s.is_empty() => Some(s.clone()),
            Some(Value::Number(n)) => Some(n.to_string()),
            Some(Value::Bool(b)) => Some(b.to_string()),
            _ => None,
        }
    }

    pub fn opt_bool(&self, key: &str) -> Option<bool> {
        match self.0.get(key) {
            Some(Value::Bool(b)) => Some(*b),
            Some(Value::String(s)) => match s.to_lowercase().as_str() {
                "true" | "yes" | "1" => Some(true),
                "false" | "no" | "0" => Some(false),
                _ => None,
            },
            _ => None,
        }
    }

    pub fn opt_usize(&self, key: &str) -> Option<usize> {
        match self.0.get(key) {
            Some(Value::Number(n)) => n.as_u64().map(|v| v as usize),
            Some(Value::String(s)) => s.parse().ok(),
            _ => None,
        }
    }

    pub fn req_usize(&self, key: &str) -> Result<usize> {
        self.opt_usize(key)
            .ok_or_else(|| Error::invalid(format!("`{key}` is required and must be a number")))
    }

    pub fn opt_usize_list(&self, key: &str) -> Option<Vec<usize>> {
        match self.0.get(key)? {
            Value::Array(items) => Some(
                items
                    .iter()
                    .filter_map(|v| match v {
                        Value::Number(n) => n.as_u64().map(|v| v as usize),
                        Value::String(s) => s.parse().ok(),
                        _ => None,
                    })
                    .collect(),
            ),
            Value::Number(n) => n.as_u64().map(|v| vec![v as usize]),
            _ => None,
        }
    }

    pub fn opt_str_list(&self, key: &str) -> Option<Vec<String>> {
        match self.0.get(key)? {
            Value::Array(items) => Some(
                items
                    .iter()
                    .filter_map(|v| v.as_str().map(str::to_string))
                    .filter(|s| !s.is_empty())
                    .collect(),
            ),
            Value::String(s) if !s.is_empty() => Some(
                s.split(',')
                    .map(|p| p.trim().to_string())
                    .filter(|p| !p.is_empty())
                    .collect(),
            ),
            _ => None,
        }
    }
}
