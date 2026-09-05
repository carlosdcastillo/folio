//! Anchored comments: highlight a region, leave a note, and the note becomes a
//! first-class work item for every connected agent.
//!
//! **Files stay pure.** A comment never lives inside the markdown. It is
//! anchored *textually* — the selected text plus a couple of lines of context
//! on each side, hashed — and re-resolved against the current content every
//! time it is rendered:
//!
//! * the anchored text still exists verbatim → the comment pins to it wherever
//!   it moved, so a rewritten paragraph above it does not orphan it;
//! * the text changed → the comment is marked **outdated** and shown in the
//!   drawer, never silently dropped;
//! * the file was deleted → the comment is **orphaned** but retained.

use crate::config::Config;
use crate::corpus::{self, Resolved};
use crate::error::{Error, Result};
use crate::store::Store;
use crate::util::{display_path, ms_to_rfc3339, now_ms, sha256_hex};
use rusqlite::{Connection, OptionalExtension};
use serde::{Deserialize, Serialize};

pub const AUTHOR_YOU: &str = "you";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Status {
    Open,
    Resolved,
    /// The anchored text no longer exists in the document.
    Outdated,
    /// The document itself is gone.
    Orphaned,
}

impl Status {
    pub fn as_str(self) -> &'static str {
        match self {
            Status::Open => "open",
            Status::Resolved => "resolved",
            Status::Outdated => "outdated",
            Status::Orphaned => "orphaned",
        }
    }
    pub fn parse(s: &str) -> Option<Status> {
        match s.trim().to_lowercase().as_str() {
            "open" => Some(Status::Open),
            "resolved" => Some(Status::Resolved),
            "outdated" => Some(Status::Outdated),
            "orphaned" => Some(Status::Orphaned),
            "all" | "" => None,
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Reply {
    pub id: String,
    pub comment_id: String,
    pub author: String,
    pub body: String,
    pub created_at: i64,
    pub created_at_iso: String,
}

/// Where a comment currently sits in the live document.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AnchorPosition {
    /// Byte offset of the anchored text in the current content.
    pub offset: usize,
    pub end: usize,
    /// 1-based line the anchor starts on.
    pub line: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Comment {
    pub id: String,
    pub path: String,
    pub display: String,
    pub anchor_hash: String,
    pub anchor_text: String,
    pub context_before: String,
    pub context_after: String,
    pub author: String,
    pub body: String,
    pub created_at: i64,
    pub created_at_iso: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub resolved_at: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub resolved_by: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub addressed_by_proposal: Option<String>,
    pub replies: Vec<Reply>,
    /// Derived on read, never stored: anchors are re-resolved every time.
    pub status: Status,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub anchor: Option<AnchorPosition>,
    /// A short quotable excerpt for list views.
    pub excerpt: String,
}

// ---------------------------------------------------------------------------
// Anchoring
// ---------------------------------------------------------------------------

/// The stored fingerprint of an anchor. Not used for matching (verbatim text
/// is), but it makes an unchanged anchor cheap to recognise.
pub fn anchor_hash(before: &str, text: &str, after: &str) -> String {
    sha256_hex(format!("{before}\u{0}{text}\u{0}{after}").as_bytes())
}

/// Build an anchor from a selection: the selected text plus `context_lines`
/// lines on each side.
pub fn anchor_from_selection(
    content: &str,
    start: usize,
    end: usize,
    context_lines: usize,
) -> Result<(String, String, String)> {
    let start = floor_boundary(content, start.min(content.len()));
    let end = ceil_boundary(content, end.min(content.len()).max(start));
    if start == end {
        return Err(Error::invalid("a comment needs a non-empty selection"));
    }

    let before_start = back_over_lines(content, start, context_lines);
    let after_end = forward_over_lines(content, end, context_lines);

    Ok((
        content[start..end].to_string(),
        content[before_start..start].to_string(),
        content[end..after_end].to_string(),
    ))
}

fn floor_boundary(s: &str, mut i: usize) -> usize {
    while i > 0 && !s.is_char_boundary(i) {
        i -= 1;
    }
    i
}

fn ceil_boundary(s: &str, mut i: usize) -> usize {
    while i < s.len() && !s.is_char_boundary(i) {
        i += 1;
    }
    i
}

fn back_over_lines(s: &str, from: usize, lines: usize) -> usize {
    let mut at = from;
    let mut seen = 0usize;
    // Step back to the start of the current line first, then over `lines` more.
    while at > 0 {
        let prev = s[..at].rfind('\n');
        match prev {
            Some(idx) => {
                if seen == lines {
                    return idx + 1;
                }
                seen += 1;
                at = idx;
            }
            None => return 0,
        }
    }
    0
}

fn forward_over_lines(s: &str, from: usize, lines: usize) -> usize {
    let mut at = from;
    let mut seen = 0usize;
    while at < s.len() {
        match s[at..].find('\n') {
            Some(rel) => {
                let idx = at + rel;
                if seen == lines {
                    return idx;
                }
                seen += 1;
                at = idx + 1;
            }
            None => return s.len(),
        }
    }
    s.len()
}

/// Find the anchor in the current content.
///
/// Verbatim matching, deliberately: a fuzzy matcher would keep more threads
/// alive across agent rewrites at the cost of occasionally pinning to the
/// wrong spot. When the text appears more than once, the stored context breaks
/// the tie — which is what lets a comment survive its paragraph being moved.
pub fn locate(content: &str, anchor_text: &str, before: &str, after: &str) -> Option<AnchorPosition> {
    if anchor_text.is_empty() {
        return None;
    }
    let mut best: Option<(usize, usize)> = None;
    let mut search_from = 0usize;
    while let Some(rel) = content[search_from..].find(anchor_text) {
        let at = search_from + rel;
        let score = common_suffix_len(&content[..at], before) + common_prefix_len(&content[at + anchor_text.len()..], after);
        if best.is_none_or(|(_, best_score)| score > best_score) {
            best = Some((at, score));
        }
        search_from = at + 1;
        if search_from >= content.len() {
            break;
        }
    }

    let (offset, _) = best?;
    let line = content[..offset].matches('\n').count() + 1;
    Some(AnchorPosition { offset, end: offset + anchor_text.len(), line })
}

fn common_suffix_len(a: &str, b: &str) -> usize {
    let (a, b) = (a.as_bytes(), b.as_bytes());
    let mut n = 0usize;
    while n < a.len() && n < b.len() && a[a.len() - 1 - n] == b[b.len() - 1 - n] {
        n += 1;
    }
    n
}

fn common_prefix_len(a: &str, b: &str) -> usize {
    let (a, b) = (a.as_bytes(), b.as_bytes());
    let mut n = 0usize;
    while n < a.len() && n < b.len() && a[n] == b[n] {
        n += 1;
    }
    n
}

// ---------------------------------------------------------------------------
// Store access
// ---------------------------------------------------------------------------

fn row_to_comment(row: &rusqlite::Row<'_>) -> rusqlite::Result<Comment> {
    let path: String = row.get("path")?;
    let created_at: i64 = row.get("created_at")?;
    let anchor_text: String = row.get("anchor_text")?;
    Ok(Comment {
        id: row.get("id")?,
        display: display_path(&path),
        path,
        anchor_hash: row.get("anchor_hash")?,
        excerpt: excerpt_of(&anchor_text),
        anchor_text,
        context_before: row.get("context_before")?,
        context_after: row.get("context_after")?,
        author: row.get("author")?,
        body: row.get("body")?,
        created_at,
        created_at_iso: ms_to_rfc3339(created_at),
        resolved_at: row.get("resolved_at")?,
        resolved_by: row.get("resolved_by")?,
        addressed_by_proposal: row.get("addressed_by_proposal")?,
        replies: Vec::new(),
        status: Status::Open,
        anchor: None,
    })
}

fn excerpt_of(text: &str) -> String {
    let flat = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if flat.chars().count() <= 120 {
        flat
    } else {
        let truncated: String = flat.chars().take(117).collect();
        format!("{truncated}...")
    }
}

/// Create a comment. Only the human user creates and resolves comments from
/// the UI; agents reply and address.
pub fn create(
    store: &Store,
    cfg: &Config,
    resolved: &Resolved,
    content: &str,
    selection_start: usize,
    selection_end: usize,
    body: &str,
) -> Result<Comment> {
    if body.trim().is_empty() {
        return Err(Error::invalid("a comment needs a body"));
    }
    let (anchor_text, before, after) =
        anchor_from_selection(content, selection_start, selection_end, cfg.anchor_context_lines)?;
    create_with_anchor(store, resolved, &anchor_text, &before, &after, body, AUTHOR_YOU)
}

pub fn create_with_anchor(
    store: &Store,
    resolved: &Resolved,
    anchor_text: &str,
    before: &str,
    after: &str,
    body: &str,
    author: &str,
) -> Result<Comment> {
    let conn = store.conn();
    let id = Store::alloc_id(&conn, "comments", "cm")?;
    conn.execute(
        "INSERT INTO comments(id, path, path_key, anchor_hash, anchor_text, context_before,
                              context_after, author, body, created_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
        rusqlite::params![
            &id,
            &resolved.path,
            corpus::fold(&resolved.path),
            anchor_hash(before, anchor_text, after),
            anchor_text,
            before,
            after,
            author,
            body.trim(),
            now_ms(),
        ],
    )?;
    drop(conn);
    get(store, &id)
}

pub fn get(store: &Store, id: &str) -> Result<Comment> {
    let mut comment = {
        let conn = store.conn();
        conn.query_row("SELECT * FROM comments WHERE id = ?1", [id], row_to_comment)
            .optional()?
            .ok_or_else(|| Error::not_found(format!("comment {id}")))?
    };
    hydrate(store, std::slice::from_mut(&mut comment))?;
    Ok(comment)
}

/// Fill in replies and re-resolve anchors against what is on disk right now.
fn hydrate(store: &Store, comments: &mut [Comment]) -> Result<()> {
    if comments.is_empty() {
        return Ok(());
    }
    {
        let conn = store.conn();
        let mut stmt = conn.prepare(
            "SELECT id, comment_id, author, body, created_at FROM replies WHERE comment_id = ?1 ORDER BY created_at, rowid",
        )?;
        for comment in comments.iter_mut() {
            comment.replies = stmt
                .query_map([&comment.id], |r| {
                    let created_at: i64 = r.get(4)?;
                    Ok(Reply {
                        id: r.get(0)?,
                        comment_id: r.get(1)?,
                        author: r.get(2)?,
                        body: r.get(3)?,
                        created_at,
                        created_at_iso: ms_to_rfc3339(created_at),
                    })
                })?
                .collect::<std::result::Result<Vec<_>, _>>()?;
        }
    }

    // One read per distinct file, however many threads it carries.
    let mut cache: std::collections::HashMap<String, Option<String>> = std::collections::HashMap::new();
    for comment in comments.iter_mut() {
        let key = corpus::fold(&comment.path);
        let content = cache.entry(key).or_insert_with(|| {
            std::fs::read(crate::util::to_fs_path(&comment.path))
                .ok()
                .map(|b| String::from_utf8_lossy(&b).into_owned())
        });

        match content {
            None => {
                comment.status = Status::Orphaned;
                comment.anchor = None;
            }
            Some(text) => {
                comment.anchor = locate(text, &comment.anchor_text, &comment.context_before, &comment.context_after);
                comment.status = if comment.resolved_at.is_some() {
                    Status::Resolved
                } else if comment.anchor.is_none() {
                    Status::Outdated
                } else {
                    Status::Open
                };
            }
        }
    }
    Ok(())
}

#[derive(Debug, Clone, Default)]
pub struct ListFilter {
    pub path: Option<String>,
    pub status: Option<Status>,
    /// `mine_only` for an agent means "threads I have taken part in".
    pub participant: Option<String>,
    pub limit: usize,
}

pub fn list(store: &Store, filter: &ListFilter) -> Result<Vec<Comment>> {
    let mut comments: Vec<Comment> = {
        let conn = store.conn();
        match &filter.path {
            Some(path) => {
                let mut stmt = conn.prepare(
                    "SELECT * FROM comments WHERE path_key = ?1 ORDER BY created_at DESC",
                )?;
                let rows = stmt
                    .query_map([corpus::fold(path)], row_to_comment)?
                    .collect::<std::result::Result<Vec<_>, _>>()?;
                rows
            }
            None => {
                let mut stmt = conn.prepare("SELECT * FROM comments ORDER BY created_at DESC")?;
                let rows = stmt
                    .query_map([], row_to_comment)?
                    .collect::<std::result::Result<Vec<_>, _>>()?;
                rows
            }
        }
    };

    hydrate(store, &mut comments)?;

    if let Some(status) = filter.status {
        comments.retain(|c| c.status == status);
    }
    if let Some(participant) = &filter.participant {
        comments.retain(|c| {
            c.author.eq_ignore_ascii_case(participant)
                || c.replies.iter().any(|r| r.author.eq_ignore_ascii_case(participant))
        });
    }

    // Outdated threads first — the drawer pins them, because a thread whose
    // anchor moved out from under it is the one at risk of being forgotten.
    comments.sort_by(|a, b| {
        let rank = |c: &Comment| match c.status {
            Status::Outdated => 0,
            Status::Open => 1,
            Status::Orphaned => 2,
            Status::Resolved => 3,
        };
        rank(a).cmp(&rank(b)).then(b.created_at.cmp(&a.created_at))
    });

    if filter.limit > 0 {
        comments.truncate(filter.limit);
    }
    Ok(comments)
}

/// Add a reply. Rate-limited per client so a looping agent cannot flood a thread.
pub fn reply(
    store: &Store,
    cfg: &Config,
    comment_id: &str,
    author: &str,
    body: &str,
    client: Option<&str>,
) -> Result<Reply> {
    if body.trim().is_empty() {
        return Err(Error::invalid("a reply needs a body"));
    }
    if let Some(client) = client {
        store.check_rate_limit(client, "reply", cfg.reply_rate_limit, cfg.reply_rate_window_ms)?;
    }

    let conn = store.conn();
    let exists: Option<i64> = conn
        .query_row("SELECT 1 FROM comments WHERE id = ?1", [comment_id], |r| r.get(0))
        .optional()?;
    if exists.is_none() {
        return Err(Error::not_found(format!("comment {comment_id}")));
    }

    let id = Store::alloc_id(&conn, "replies", "rp")?;
    let now = now_ms();
    conn.execute(
        "INSERT INTO replies(id, comment_id, author, body, created_at) VALUES (?1, ?2, ?3, ?4, ?5)",
        rusqlite::params![&id, comment_id, author, body.trim(), now],
    )?;
    Ok(Reply {
        id,
        comment_id: comment_id.to_string(),
        author: author.to_string(),
        body: body.trim().to_string(),
        created_at: now,
        created_at_iso: ms_to_rfc3339(now),
    })
}

pub fn resolve_thread(store: &Store, comment_id: &str, by: &str) -> Result<Comment> {
    {
        let conn = store.conn();
        let n = conn.execute(
            "UPDATE comments SET resolved_at = ?2, resolved_by = ?3 WHERE id = ?1 AND resolved_at IS NULL",
            rusqlite::params![comment_id, now_ms(), by],
        )?;
        if n == 0 {
            let exists: Option<i64> = conn
                .query_row("SELECT 1 FROM comments WHERE id = ?1", [comment_id], |r| r.get(0))
                .optional()?;
            if exists.is_none() {
                return Err(Error::not_found(format!("comment {comment_id}")));
            }
        }
    }
    get(store, comment_id)
}

pub fn reopen_thread(store: &Store, comment_id: &str) -> Result<Comment> {
    {
        let conn = store.conn();
        conn.execute(
            "UPDATE comments SET resolved_at = NULL, resolved_by = NULL WHERE id = ?1",
            [comment_id],
        )?;
    }
    get(store, comment_id)
}

pub fn delete(store: &Store, comment_id: &str) -> Result<()> {
    let conn = store.conn();
    let n = conn.execute("DELETE FROM comments WHERE id = ?1", [comment_id])?;
    if n == 0 {
        return Err(Error::not_found(format!("comment {comment_id}")));
    }
    Ok(())
}

/// Link a proposal to the thread it addresses.
pub fn link_proposal(conn: &Connection, comment_id: &str, proposal_id: &str) -> Result<()> {
    let n = conn.execute(
        "UPDATE comments SET addressed_by_proposal = ?2 WHERE id = ?1",
        (comment_id, proposal_id),
    )?;
    if n == 0 {
        return Err(Error::not_found(format!("comment {comment_id}")));
    }
    Ok(())
}

/// Count open threads per document, for sidebar badges and the Today view.
pub fn open_counts(store: &Store) -> Result<std::collections::HashMap<String, usize>> {
    let comments = list(store, &ListFilter { status: Some(Status::Open), ..Default::default() })?;
    let mut out = std::collections::HashMap::new();
    for c in comments {
        *out.entry(corpus::fold(&c.path)).or_insert(0) += 1;
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    const DOC: &str = "# Skill\n\nrender it as HTML automatically and tell them the file path.\n\
        The threshold: if the table has 4+ rows or 3+ columns, it belongs in the browser.\n\
        You can still include a brief text summary in the chat,\n";

    fn anchor() -> (String, String, String) {
        let anchor_text = "The threshold: if the table has 4+ rows or 3+ columns, it belongs in the browser.";
        let start = DOC.find(anchor_text).unwrap();
        anchor_from_selection(DOC, start, start + anchor_text.len(), 2).unwrap()
    }

    #[test]
    fn an_anchor_captures_context_on_both_sides() {
        let (text, before, after) = anchor();
        assert!(text.starts_with("The threshold"));
        assert!(before.contains("render it as HTML"));
        assert!(after.contains("brief text summary"));
    }

    #[test]
    fn a_comment_survives_its_paragraph_moving_verbatim() {
        let (text, before, after) = anchor();
        // The same sentence, moved to the top of a rewritten document.
        let moved = format!(
            "{text}\n\n# Skill\n\nEntirely new opening prose that did not exist before.\n"
        );
        let position = locate(&moved, &text, &before, &after).expect("moved anchors must still pin");
        assert_eq!(position.offset, 0);
        assert_eq!(position.line, 1);
    }

    #[test]
    fn edited_anchor_text_goes_outdated_not_missing() {
        let (text, before, after) = anchor();
        let edited = DOC.replace("4+ rows or 3+ columns", "4 rows and 4 columns");
        assert!(locate(&edited, &text, &before, &after).is_none());
    }

    #[test]
    fn context_breaks_ties_between_duplicate_anchors() {
        let text = "Do the thing.";
        let doc = format!("## First\n\nlead in one\n{text}\ntail one\n\n## Second\n\nlead in two\n{text}\ntail two\n");
        let second_start = doc.rfind(text).unwrap();
        let (a, b, c) = anchor_from_selection(&doc, second_start, second_start + text.len(), 2).unwrap();
        let found = locate(&doc, &a, &b, &c).unwrap();
        assert_eq!(found.offset, second_start, "the stored context must select the right occurrence");
    }

    #[test]
    fn an_empty_selection_is_refused() {
        assert_eq!(anchor_from_selection(DOC, 5, 5, 2).unwrap_err().code(), "invalid");
    }
}
