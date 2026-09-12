//! Proposals: an agent edit arrives as a changeset against a recorded base,
//! waiting for human judgement.
//!
//! ```text
//! agent calls propose_edit
//!         |
//!         v
//!   +-----------+   file changed on disk since base?   +----------+
//!   |  pending  | ------------- yes ----------------->  | conflict |
//!   +-----+-----+                                       +----+-----+
//!         | review: prose diff, hunk-by-hunk                 | rebase onto
//!         v                                                  v current
//!   +-----------+     reject (with optional note)      +----------+
//!   | accepted  | <-- accept: apply to disk + snapshot  | rejected |
//!   +-----------+     note becomes agent feedback       +----------+
//! ```
//!
//! Human edits in the Folio editor never become proposals; they save directly
//! with a snapshot. The gate is for agents, not for you.

use crate::comment;
use crate::config::Config;
use crate::corpus::{self, Policy, Resolved};
use crate::diff::{self, Diff};
use crate::error::{Error, Result};
use crate::store::Store;
use crate::util::{display_path, ms_to_rfc3339, now_ms, sha256_hex};
use crate::version::{self, Snapshot, Source};
use rusqlite::OptionalExtension;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Status {
    Pending,
    Accepted,
    Rejected,
    /// Legacy status for proposals replaced by newer drafts in older versions.
    Superseded,
    /// Pending, but the file moved under it. Derived, never stored — a
    /// conflict that resolves itself when the file moves back should stop
    /// being a conflict.
    Conflict,
}

impl Status {
    pub fn as_str(self) -> &'static str {
        match self {
            Status::Pending => "pending",
            Status::Accepted => "accepted",
            Status::Rejected => "rejected",
            Status::Superseded => "superseded",
            Status::Conflict => "conflict",
        }
    }
    pub fn parse(s: &str) -> Option<Status> {
        match s.trim().to_lowercase().as_str() {
            "pending" => Some(Status::Pending),
            "accepted" => Some(Status::Accepted),
            "rejected" => Some(Status::Rejected),
            "superseded" => Some(Status::Superseded),
            "conflict" => Some(Status::Conflict),
            _ => None,
        }
    }
    pub fn is_open(self) -> bool {
        matches!(self, Status::Pending | Status::Conflict)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Stats {
    pub added: usize,
    pub removed: usize,
    pub hunks: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Proposal {
    pub id: String,
    pub root_id: String,
    pub path: String,
    pub display: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub base_snapshot_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub base_blob_hash: Option<String>,
    pub proposed_blob_hash: String,
    pub author: String,
    pub client: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
    /// The comment id this proposal answers, if any.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub addressing: Option<String>,
    /// Derived: `conflict` when pending and the file moved under it.
    pub status: Status,
    /// What the store actually holds.
    pub stored_status: Status,
    pub created_at: i64,
    pub created_at_iso: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub decided_at: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub decided_at_iso: Option<String>,
    /// The feedback channel to agents: `list_proposals` returns this so an
    /// agent can read why its change was turned down.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub decision_note: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result_snapshot_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stats: Option<Stats>,
    /// True when the document no longer exists on disk.
    pub target_missing: bool,
}

fn row_to_proposal(row: &rusqlite::Row<'_>) -> rusqlite::Result<Proposal> {
    let path: String = row.get("path")?;
    let created_at: i64 = row.get("created_at")?;
    let decided_at: Option<i64> = row.get("decided_at")?;
    let stored = Status::parse(&row.get::<_, String>("status")?).unwrap_or(Status::Pending);
    Ok(Proposal {
        id: row.get("id")?,
        root_id: row.get("root_id")?,
        display: display_path(&path),
        path,
        base_snapshot_id: row.get("base_snapshot_id")?,
        base_blob_hash: row.get("base_blob_hash")?,
        proposed_blob_hash: row.get("proposed_blob_hash")?,
        author: row.get("author")?,
        client: row.get("client")?,
        message: row.get("message")?,
        addressing: row.get("addressing")?,
        status: stored,
        stored_status: stored,
        created_at,
        created_at_iso: ms_to_rfc3339(created_at),
        decided_at,
        decided_at_iso: decided_at.map(ms_to_rfc3339),
        decision_note: row.get("decision_note")?,
        result_snapshot_id: row.get("result_snapshot_id")?,
        stats: None,
        target_missing: false,
    })
}

/// What the caller is proposing.
pub enum Change<'a> {
    /// Replace the whole file.
    Content(&'a str),
    /// Apply a unified diff to the base the agent read.
    Patch(&'a str),
}

pub struct CreateRequest<'a> {
    pub resolved: &'a Resolved,
    pub change: Change<'a>,
    /// The exact version the agent read. Internal task operations may omit
    /// this because they perform their own stale check immediately before
    /// calling the policy layer.
    pub base_version: Option<&'a str>,
    pub author: &'a str,
    pub client: &'a str,
    pub message: Option<&'a str>,
    pub addressing: Option<&'a str>,
}

/// The result of routing a write through the policy layer.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "outcome", rename_all = "snake_case")]
pub enum WriteOutcome {
    /// Policy is `propose`: the change is queued for review.
    Proposed { proposal: Box<Proposal> },
    /// Policy is `direct`: the change is on disk, and snapshotted.
    Applied { snapshot: Box<Snapshot> },
}

// ---------------------------------------------------------------------------
// Creating
// ---------------------------------------------------------------------------

/// Resolve the content a change produces, against the base the agent read.
pub fn materialise(
    store: &Store,
    req: &CreateRequest<'_>,
) -> Result<(String, Option<Snapshot>, String)> {
    let latest = version::latest(store, &req.resolved.path)?;
    let base = match req.base_version {
        Some(claimed) => {
            let snapshot = version::get(store, claimed)?;
            if !crate::util::path_eq(&snapshot.path, &req.resolved.path) {
                return Err(Error::invalid(format!(
                    "version {claimed} belongs to {}, not {}",
                    snapshot.display,
                    req.resolved.display()
                )));
            }
            if latest.as_ref().map(|s| s.id.as_str()) != Some(claimed) {
                return Err(Error::stale(
                    format!(
                        "{} has moved on since version {claimed}; re-read it before proposing",
                        req.resolved.display()
                    ),
                    serde_json::json!({
                        "path": req.resolved.path,
                        "version": latest.as_ref().map(|s| s.id.clone()),
                    }),
                ));
            }
            Some(snapshot)
        }
        None => latest,
    };
    let base_text = match &base {
        Some(snap) => version::content(store, snap)?,
        // No history yet: fall back to disk so a patch against an unregistered
        // but present file still has something to apply to.
        None => std::fs::read(&req.resolved.fs_path)
            .map(|b| String::from_utf8_lossy(&b).into_owned())
            .unwrap_or_default(),
    };

    let proposed = match req.change {
        Change::Content(text) => text.to_string(),
        Change::Patch(patch) => diff::patch::apply(&base_text, patch)?,
    };
    Ok((proposed, base, base_text))
}

/// Route a write through the effective policy for its path: a proposal, or a
/// direct write. Enforced here in the core, not in the bridge, so no client
/// can bypass it.
pub fn write(store: &Store, cfg: &Config, req: CreateRequest<'_>) -> Result<WriteOutcome> {
    let (proposed, base, _) = materialise(store, &req)?;
    let artifact_type = corpus::detect_type(&req.resolved.path, &proposed);
    let policy = corpus::policy_for(store, req.resolved, artifact_type)?;

    match policy {
        Policy::Direct => {
            let snapshot = version::write_and_record(
                store,
                cfg,
                req.resolved,
                &proposed,
                Source::Direct,
                Some(req.author),
                Some(req.client),
                req.message,
            )?;
            Ok(WriteOutcome::Applied { snapshot: Box::new(snapshot.snapshot) })
        }
        Policy::Propose | Policy::Auto => {
            let proposal = create(store, cfg, &req, &proposed, base.as_ref())?;
            Ok(WriteOutcome::Proposed { proposal: Box::new(proposal) })
        }
    }
}

/// Queue a proposal. Durable: created headless, reviewed later. Nothing is
/// lost if the app is closed for a week.
pub fn create(
    store: &Store,
    cfg: &Config,
    req: &CreateRequest<'_>,
    proposed: &str,
    base: Option<&Snapshot>,
) -> Result<Proposal> {
    let proposed_hash = sha256_hex(proposed.as_bytes());
    if let Some(base) = base {
        if base.blob_hash == proposed_hash {
            return Err(Error::invalid(
                "the proposed content is identical to the current version",
            ));
        }
    }
    if proposed.len() as u64 > cfg.max_blob_bytes {
        return Err(Error::TooLarge(format!(
            "proposed content is {} bytes; the per-file limit is {}",
            proposed.len(),
            cfg.max_blob_bytes
        )));
    }

    // Runaway-agent guard.
    {
        let conn = store.conn();
        let pending: i64 = conn.query_row(
            "SELECT COUNT(*) FROM proposals WHERE status = 'pending'",
            [],
            |r| r.get(0),
        )?;
        if pending >= cfg.max_pending_proposals {
            return Err(Error::QueueFull(format!(
                "{pending} proposals are already pending (limit {}); review some before proposing more",
                cfg.max_pending_proposals
            )));
        }
    }

    if let Some(comment_id) = req.addressing {
        // Fail early rather than storing a dangling link.
        comment::get(store, comment_id)?;
    }

    store.blobs().put(proposed.as_bytes())?;

    let id = {
        let conn = store.conn();
        let id = Store::alloc_id(&conn, "proposals", "prop")?;

        conn.execute(
            "INSERT INTO proposals(id, root_id, path, path_key, base_snapshot_id, base_blob_hash,
                                   proposed_blob_hash, author, client, message, addressing,
                                   status, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, 'pending', ?12)",
            rusqlite::params![
                &id,
                &req.resolved.root.id,
                &req.resolved.path,
                corpus::fold(&req.resolved.path),
                base.map(|b| b.id.as_str()),
                base.map(|b| b.blob_hash.as_str()),
                &proposed_hash,
                req.author,
                req.client,
                req.message,
                req.addressing,
                now_ms(),
            ],
        )?;

        if let Some(comment_id) = req.addressing {
            comment::link_proposal(&conn, comment_id, &id)?;
        }
        id
    };

    get(store, &id)
}

// ---------------------------------------------------------------------------
// Reading
// ---------------------------------------------------------------------------

pub fn get(store: &Store, id: &str) -> Result<Proposal> {
    let mut proposal = {
        let conn = store.conn();
        conn.query_row("SELECT * FROM proposals WHERE id = ?1", [id], row_to_proposal)
            .optional()?
            .ok_or_else(|| Error::not_found(format!("proposal {id}")))?
    };
    decorate(store, &mut proposal)?;
    Ok(proposal)
}

/// Fill in the derived conflict state and the diff summary.
fn decorate(store: &Store, proposal: &mut Proposal) -> Result<()> {
    let current = read_current(proposal);
    proposal.target_missing = current.is_none();

    if proposal.stored_status == Status::Pending {
        let current_hash = current.as_ref().map(|c| sha256_hex(c.as_bytes()));
        let base = proposal.base_blob_hash.clone();
        // A conflict is exactly "the file changed after the proposal's base".
        let moved = match (&base, &current_hash) {
            (Some(b), Some(c)) => b != c,
            (None, Some(_)) => false,
            (Some(_), None) => true,
            (None, None) => false,
        };
        if moved {
            proposal.status = Status::Conflict;
        }
    }

    if let Ok(proposed) = store.blobs().get_text(&proposal.proposed_blob_hash) {
        let against = current.unwrap_or_default();
        let d = diff::diff_text(&against, &proposed);
        proposal.stats = Some(Stats { added: d.added, removed: d.removed, hunks: d.hunks.len() });
    }
    Ok(())
}

fn read_current(proposal: &Proposal) -> Option<String> {
    std::fs::read(crate::util::to_fs_path(&proposal.path))
        .ok()
        .map(|b| String::from_utf8_lossy(&b).into_owned())
}

#[derive(Debug, Clone, Default)]
pub struct ListFilter {
    pub status: Option<Status>,
    pub path: Option<String>,
    pub client: Option<String>,
    pub limit: usize,
}

pub fn list(store: &Store, filter: &ListFilter) -> Result<Vec<Proposal>> {
    let mut proposals: Vec<Proposal> = {
        let conn = store.conn();
        let mut stmt = conn.prepare("SELECT * FROM proposals ORDER BY created_at DESC")?;
        let rows = stmt
            .query_map([], row_to_proposal)?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        rows
    };

    if let Some(path) = &filter.path {
        let key = corpus::fold(path);
        proposals.retain(|p| corpus::fold(&p.path) == key);
    }
    if let Some(client) = &filter.client {
        proposals.retain(|p| p.client.eq_ignore_ascii_case(client));
    }

    for proposal in proposals.iter_mut() {
        decorate(store, proposal)?;
    }

    if let Some(status) = filter.status {
        proposals.retain(|p| p.status == status);
    }

    if filter.limit > 0 {
        proposals.truncate(filter.limit);
    }
    Ok(proposals)
}

pub fn pending_counts(store: &Store) -> Result<std::collections::HashMap<String, usize>> {
    let conn = store.conn();
    let mut stmt = conn.prepare(
        "SELECT path_key, COUNT(*) FROM proposals WHERE status = 'pending' GROUP BY path_key",
    )?;
    let mut out = std::collections::HashMap::new();
    let rows = stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?)))?;
    for row in rows {
        let (key, count) = row?;
        out.insert(key, count as usize);
    }
    Ok(out)
}

pub fn proposed_content(store: &Store, proposal: &Proposal) -> Result<String> {
    store.blobs().get_text(&proposal.proposed_blob_hash)
}

/// The reviewable diff: current disk content on the left, proposal on the
/// right. Always recomputed against what is actually there now, so accepting a
/// hunk applies to reality rather than to a remembered base.
pub fn review_diff(store: &Store, proposal: &Proposal) -> Result<(String, String, Diff)> {
    let current = read_current(proposal).unwrap_or_default();
    let proposed = proposed_content(store, proposal)?;
    let d = diff::diff_text(&current, &proposed);
    Ok((current, proposed, d))
}

// ---------------------------------------------------------------------------
// Deciding
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Decision {
    pub proposal: Proposal,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub snapshot: Option<Snapshot>,
    /// Set when accepting resolved the comment this proposal addressed.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub resolved_comment: Option<String>,
    /// Hunks actually applied, when the accept was partial.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub applied_hunks: Option<Vec<usize>>,
}

/// Accept a proposal: apply it to disk and snapshot it with the agent as author.
///
/// `hunks` selects a subset; the partial patch is recomputed against current
/// disk state, so hunk-level accept applies exactly the accepted hunks even if
/// the file moved since the proposal was made.
pub fn accept(
    store: &Store,
    cfg: &Config,
    resolved: &Resolved,
    proposal_id: &str,
    hunks: Option<&[usize]>,
) -> Result<Decision> {
    let proposal = get(store, proposal_id)?;
    if !proposal.stored_status.is_open() {
        return Err(Error::invalid(format!(
            "proposal {proposal_id} is already {}",
            proposal.stored_status.as_str()
        )));
    }

    let (current, proposed, _) = review_diff(store, &proposal)?;
    let moved = proposal
        .base_blob_hash
        .as_ref()
        .is_some_and(|base_hash| sha256_hex(current.as_bytes()) != *base_hash);
    if moved && hunks.is_some() {
        return Err(Error::Conflict(
            "cannot partially apply a proposal after the file changed; rebase it and review the updated hunks"
                .into(),
        ));
    }
    let proposed = match &proposal.base_blob_hash {
        Some(base_hash) if moved => {
            let base = store.blobs().get_text(base_hash)?;
            diff::merge3(&base, &current, &proposed).map_err(|e| {
                Error::Conflict(format!(
                    "cannot apply this proposal without dropping newer edits: {e}. Rebase it and review the result."
                ))
            })?
        }
        _ => proposed,
    };
    let d = diff::diff_text(&current, &proposed);
    let (final_text, applied) = match hunks {
        None => (proposed, None),
        Some(selected) => {
            let unknown: Vec<usize> = selected
                .iter()
                .copied()
                .filter(|i| !d.hunks.iter().any(|h| h.index == *i))
                .collect();
            if !unknown.is_empty() {
                return Err(Error::invalid(format!(
                    "hunk(s) {unknown:?} are not in this proposal's current diff; re-read it and retry"
                )));
            }
            (
                diff::apply_hunks(&current, &proposed, &d, selected),
                Some(selected.to_vec()),
            )
        }
    };

    if final_text == current {
        return Err(Error::invalid(
            "accepting these hunks would change nothing",
        ));
    }

    let message = proposal
        .message
        .clone()
        .unwrap_or_else(|| format!("proposal {}", proposal.id));
    let outcome = version::write_and_record(
        store,
        cfg,
        resolved,
        &final_text,
        Source::Proposal,
        Some(&proposal.author),
        Some(&proposal.client),
        Some(&message),
    )?;

    let mut resolved_comment = None;
    {
        let conn = store.conn();
        conn.execute(
            "UPDATE proposals SET status = 'accepted', decided_at = ?2, result_snapshot_id = ?3 WHERE id = ?1",
            rusqlite::params![&proposal.id, now_ms(), &outcome.snapshot.id],
        )?;
    }

    // Accepting an addressing proposal closes the loop: the thread resolves.
    if let Some(comment_id) = &proposal.addressing {
        comment::resolve_thread(store, comment_id, "you")?;
        resolved_comment = Some(comment_id.clone());
    }

    Ok(Decision {
        proposal: get(store, &proposal.id)?,
        snapshot: Some(outcome.snapshot),
        resolved_comment,
        applied_hunks: applied,
    })
}

/// Reject a proposal. The note is the entire feedback channel to agents, and
/// it is enough: `list_proposals` returns rejected proposals with their notes.
pub fn reject(store: &Store, proposal_id: &str, note: Option<&str>) -> Result<Decision> {
    let proposal = get(store, proposal_id)?;
    if !proposal.stored_status.is_open() {
        return Err(Error::invalid(format!(
            "proposal {proposal_id} is already {}",
            proposal.stored_status.as_str()
        )));
    }
    {
        let conn = store.conn();
        conn.execute(
            "UPDATE proposals SET status = 'rejected', decided_at = ?2, decision_note = ?3 WHERE id = ?1",
            rusqlite::params![&proposal.id, now_ms(), note.map(str::trim).filter(|n| !n.is_empty())],
        )?;
    }

    // Rejecting leaves the comment open and files the note into the thread, so
    // the agent gets the feedback where it is already looking.
    if let (Some(comment_id), Some(note)) = (&proposal.addressing, note) {
        if !note.trim().is_empty() {
            let cfg = Config::default();
            comment::reply(
                store,
                &cfg,
                comment_id,
                "you",
                &format!("Rejected {}: {}", proposal.id, note.trim()),
                None,
            )?;
        }
    }

    Ok(Decision {
        proposal: get(store, &proposal.id)?,
        snapshot: None,
        resolved_comment: None,
        applied_hunks: None,
    })
}

/// Rebase a conflicting proposal onto the current content.
///
/// This is a real three-way merge, not a base-pointer bump: the agent's change
/// (base -> proposed) is replayed on top of what is on disk now. If it will not
/// replay, the reviewer is told, and rejecting remains the other option.
pub fn rebase(store: &Store, proposal_id: &str) -> Result<Proposal> {
    let proposal = get(store, proposal_id)?;
    if !proposal.stored_status.is_open() {
        return Err(Error::invalid("only a pending proposal can be rebased"));
    }
    let base_text = match &proposal.base_blob_hash {
        Some(hash) => store.blobs().get_text(hash)?,
        None => String::new(),
    };
    let proposed = proposed_content(store, &proposal)?;
    let current = read_current(&proposal)
        .ok_or_else(|| Error::Conflict(format!("{} no longer exists on disk", proposal.display)))?;

    if base_text == current {
        return Ok(proposal);
    }

    let merged = diff::merge3(&base_text, &current, &proposed).map_err(|e| {
        Error::Conflict(format!(
            "cannot replay this proposal onto the current content: {e}. Reject it and ask the agent to re-read."
        ))
    })?;

    let merged_hash = sha256_hex(merged.as_bytes());
    store.blobs().put(merged.as_bytes())?;

    let new_base = version::latest(store, &proposal.path)?;
    {
        let conn = store.conn();
        conn.execute(
            "UPDATE proposals SET proposed_blob_hash = ?2, base_snapshot_id = ?3, base_blob_hash = ?4 WHERE id = ?1",
            rusqlite::params![
                &proposal.id,
                &merged_hash,
                new_base.as_ref().map(|s| s.id.as_str()),
                new_base.as_ref().map(|s| s.blob_hash.as_str()),
            ],
        )?;
    }
    get(store, &proposal.id)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::corpus::{Root, RootKind};
    use crate::util::canonical_key;

    struct Fixture {
        _dir: tempfile::TempDir,
        store: Store,
        cfg: Config,
        resolved: Resolved,
    }

    fn fixture(initial: &str) -> Fixture {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(&dir.path().join("store")).unwrap();
        let work = dir.path().join("work");
        std::fs::create_dir_all(&work).unwrap();
        let root_path = canonical_key(&work);
        {
            let conn = store.conn();
            conn.execute(
                "INSERT INTO roots(id, path, path_key, kind, policy, label, added_at)
                 VALUES ('root_t', ?1, ?2, 'dir', 'propose', 'work', 0)",
                (&root_path, corpus::fold(&root_path)),
            )
            .unwrap();
        }
        let root = Root {
            id: "root_t".into(),
            display: root_path.clone(),
            path: root_path,
            kind: RootKind::Dir,
            policy: Policy::Propose,
            label: "work".into(),
            added_at: 0,
        };
        let file = work.join("SPEC.md");
        std::fs::write(&file, initial).unwrap();
        let key = canonical_key(&file);
        let resolved = Resolved { root, fs_path: crate::util::to_fs_path(&key), path: key };
        let cfg = Config::default();
        version::record_from_disk(&store, &cfg, &resolved, Source::External, None, None, None).unwrap();
        Fixture { _dir: dir, store, cfg, resolved }
    }

    fn propose(f: &Fixture, content: &str) -> Proposal {
        match write(
            &f.store,
            &f.cfg,
            CreateRequest {
                resolved: &f.resolved,
                change: Change::Content(content),
                base_version: None,
                author: "claude-sonnet-4.6",
                client: "claude-code",
                message: Some("Tighten the threshold"),
                addressing: None,
            },
        )
        .unwrap()
        {
            WriteOutcome::Proposed { proposal } => *proposal,
            other => panic!("expected a proposal, got {other:?}"),
        }
    }

    #[test]
    fn a_propose_policy_queues_rather_than_writing() {
        let f = fixture("one\n\ntwo\n");
        let p = propose(&f, "one\n\nTWO\n");
        assert_eq!(p.status, Status::Pending);
        assert_eq!(std::fs::read_to_string(&f.resolved.fs_path).unwrap(), "one\n\ntwo\n");
        assert_eq!(p.stats.as_ref().unwrap().added, 1);
    }

    #[test]
    fn accepting_applies_to_disk_and_snapshots_with_the_agent_as_author() {
        let f = fixture("one\n\ntwo\n");
        let p = propose(&f, "one\n\nTWO\n");
        let decision = accept(&f.store, &f.cfg, &f.resolved, &p.id, None).unwrap();
        assert_eq!(std::fs::read_to_string(&f.resolved.fs_path).unwrap(), "one\n\nTWO\n");
        let snap = decision.snapshot.unwrap();
        assert_eq!(snap.author, "claude-sonnet-4.6");
        assert_eq!(snap.source, Source::Proposal);
        assert_eq!(decision.proposal.status, Status::Accepted);
    }

    #[test]
    fn hunk_level_accept_applies_exactly_the_accepted_hunks() {
        let f = fixture("one\n\ntwo\n\nthree\n");
        let p = propose(&f, "ONE\n\ntwo\n\nTHREE\n");
        let (_, _, d) = review_diff(&f.store, &p).unwrap();
        assert_eq!(d.hunks.len(), 2);
        let decision = accept(&f.store, &f.cfg, &f.resolved, &p.id, Some(&[1])).unwrap();
        assert_eq!(std::fs::read_to_string(&f.resolved.fs_path).unwrap(), "one\n\ntwo\n\nTHREE\n");
        assert_eq!(decision.applied_hunks, Some(vec![1]));
    }

    #[test]
    fn rejecting_records_a_note_the_agent_can_read_back() {
        let f = fixture("one\n");
        let p = propose(&f, "ONE\n");
        reject(&f.store, &p.id, Some("Keep 4+; 3-row tables are fine inline.")).unwrap();
        let rejected = list(
            &f.store,
            &ListFilter { status: Some(Status::Rejected), ..Default::default() },
        )
        .unwrap();
        assert_eq!(rejected.len(), 1);
        assert!(rejected[0].decision_note.as_ref().unwrap().contains("3-row tables"));
        assert_eq!(std::fs::read_to_string(&f.resolved.fs_path).unwrap(), "one\n");
    }

    #[test]
    fn a_file_that_moved_under_a_proposal_reads_as_a_conflict_and_rebases() {
        let f = fixture("alpha\n\nbeta\n\ngamma\n");
        let p = propose(&f, "alpha\n\nBETA\n\ngamma\n");

        // Someone else edits a different paragraph on disk.
        std::fs::write(&f.resolved.fs_path, "ALPHA\n\nbeta\n\ngamma\n").unwrap();
        version::record_from_disk(&f.store, &f.cfg, &f.resolved, Source::External, None, None, None).unwrap();

        assert_eq!(get(&f.store, &p.id).unwrap().status, Status::Conflict);

        let rebased = rebase(&f.store, &p.id).unwrap();
        assert_eq!(rebased.status, Status::Pending, "rebase clears the conflict");
        let merged = proposed_content(&f.store, &rebased).unwrap();
        assert_eq!(merged, "ALPHA\n\nBETA\n\ngamma\n", "both changes survive the merge");
    }

    #[test]
    fn a_task_list_takes_the_direct_path_and_writes_immediately() {
        let f = fixture("# TODO\n\n- [ ] one @carlos\n- [ ] two @carlos\n");
        // The root is `propose`; switch it to `auto` so type decides.
        corpus::set_root_policy(&f.store, "root_t", Policy::Auto).unwrap();
        let mut resolved = f.resolved.clone();
        resolved.root.policy = Policy::Auto;

        let outcome = write(
            &f.store,
            &f.cfg,
            CreateRequest {
                resolved: &resolved,
                change: Change::Content("# TODO\n\n- [ ] one @carlos\n- [x] two @carlos\n"),
                base_version: None,
                author: "claude-sonnet-4.6",
                client: "claude-code",
                message: Some("tick two"),
                addressing: None,
            },
        )
        .unwrap();
        assert!(matches!(outcome, WriteOutcome::Applied { .. }));
        assert!(std::fs::read_to_string(&f.resolved.fs_path).unwrap().contains("- [x] two"));
    }

    #[test]
    fn a_patch_change_applies_against_the_recorded_base() {
        let f = fixture("one\ntwo\nthree\n");
        let outcome = write(
            &f.store,
            &f.cfg,
            CreateRequest {
                resolved: &f.resolved,
                change: Change::Patch("@@ -1,3 +1,3 @@\n one\n-two\n+TWO\n three\n"),
                base_version: None,
                author: "codex",
                client: "codex-cli",
                message: Some("uppercase"),
                addressing: None,
            },
        )
        .unwrap();
        let WriteOutcome::Proposed { proposal } = outcome else { panic!("expected a proposal") };
        assert_eq!(proposed_content(&f.store, &proposal).unwrap(), "one\nTWO\nthree\n");
    }

    #[test]
    fn same_client_proposals_queue_and_both_edits_survive_acceptance() {
        let f = fixture("one\n\ntwo\n\nthree\n");
        let first = propose(&f, "ONE\n\ntwo\n\nthree\n");
        let second = propose(&f, "one\n\ntwo\n\nTHREE\n");

        assert_eq!(get(&f.store, &first.id).unwrap().status, Status::Pending);
        assert_eq!(get(&f.store, &second.id).unwrap().status, Status::Pending);
        assert_eq!(pending_counts(&f.store).unwrap()[&corpus::fold(&f.resolved.path)], 2);

        accept(&f.store, &f.cfg, &f.resolved, &second.id, None).unwrap();
        accept(&f.store, &f.cfg, &f.resolved, &first.id, None).unwrap();
        assert_eq!(
            std::fs::read_to_string(&f.resolved.fs_path).unwrap(),
            "ONE\n\ntwo\n\nTHREE\n"
        );
        assert_eq!(get(&f.store, &first.id).unwrap().status, Status::Accepted);
        assert_eq!(get(&f.store, &second.id).unwrap().status, Status::Accepted);
    }

    #[test]
    fn overlapping_queued_edits_conflict_instead_of_dropping_the_first() {
        let f = fixture("one\n");
        let first = propose(&f, "ONE\n");
        let second = propose(&f, "One!\n");

        accept(&f.store, &f.cfg, &f.resolved, &first.id, None).unwrap();
        let err = accept(&f.store, &f.cfg, &f.resolved, &second.id, None).unwrap_err();

        assert_eq!(err.code(), "conflict");
        assert_eq!(std::fs::read_to_string(&f.resolved.fs_path).unwrap(), "ONE\n");
        assert_eq!(get(&f.store, &second.id).unwrap().status, Status::Conflict);
    }

    #[test]
    fn the_pending_queue_is_capped() {
        let f = fixture("one\n");
        let mut cfg = f.cfg.clone();
        cfg.max_pending_proposals = 1;
        propose(&f, "ONE\n");
        let err = write(
            &f.store,
            &cfg,
            CreateRequest {
                resolved: &f.resolved,
                change: Change::Content("one!\n"),
                base_version: None,
                author: "a",
                client: "other-client",
                message: None,
                addressing: None,
            },
        )
        .unwrap_err();
        assert_eq!(err.code(), "queue_full");
    }

    #[test]
    fn a_no_op_proposal_is_refused() {
        let f = fixture("one\n");
        let err = write(
            &f.store,
            &f.cfg,
            CreateRequest {
                resolved: &f.resolved,
                change: Change::Content("one\n"),
                base_version: None,
                author: "a",
                client: "c",
                message: None,
                addressing: None,
            },
        )
        .unwrap_err();
        assert_eq!(err.code(), "invalid");
    }
}
