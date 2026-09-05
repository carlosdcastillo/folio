//! Roots, artifact typing, and path sandboxing.
//!
//! The corpus is the union of registered roots. Nothing outside a root is ever
//! read, written, watched, or snapshotted — that boundary is enforced here, in
//! the core, so no shell (Tauri, IPC, MCP) can talk its way around it.

use crate::artifact::frontmatter;
use crate::error::{Error, Result};
use crate::store::Store;
use crate::util::{canonical_key, display_path, expand_tilde, is_under, now_ms, to_fs_path};
use rusqlite::{Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

// ---------------------------------------------------------------------------
// Types
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum RootKind {
    Dir,
    File,
}

impl RootKind {
    pub fn as_str(self) -> &'static str {
        match self {
            RootKind::Dir => "dir",
            RootKind::File => "file",
        }
    }
    pub fn parse(s: &str) -> RootKind {
        match s {
            "file" => RootKind::File,
            _ => RootKind::Dir,
        }
    }
}

/// How MCP writes to a path are routed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Policy {
    /// Decide by artifact type: task lists direct, everything else proposed.
    /// This asymmetry is deliberate and is the retention hook — an agent
    /// maintaining TODO.md overnight must not block on review, while a skill
    /// rewrite is exactly where you want a gate.
    Auto,
    /// Every MCP write becomes a proposal.
    Propose,
    /// MCP writes apply immediately, always snapshotted.
    Direct,
}

impl Policy {
    pub fn as_str(self) -> &'static str {
        match self {
            Policy::Auto => "auto",
            Policy::Propose => "propose",
            Policy::Direct => "direct",
        }
    }
    pub fn parse(s: &str) -> Policy {
        match s {
            "propose" => Policy::Propose,
            "direct" => Policy::Direct,
            _ => Policy::Auto,
        }
    }
    /// Resolve `Auto` against an artifact type.
    pub fn resolve(self, ty: ArtifactType) -> Policy {
        match self {
            Policy::Auto => {
                if ty == ArtifactType::TaskList {
                    Policy::Direct
                } else {
                    Policy::Propose
                }
            }
            explicit => explicit,
        }
    }
}

/// Typing is by structure, never by folder name, and is re-derived on every
/// snapshot — a file is allowed to change what it is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ArtifactType {
    Skill,
    Prompt,
    TaskList,
    Doc,
    /// Tracked (snapshotted, restorable) but never diffed or edited.
    Asset,
}

impl ArtifactType {
    pub fn as_str(self) -> &'static str {
        match self {
            ArtifactType::Skill => "skill",
            ArtifactType::Prompt => "prompt",
            ArtifactType::TaskList => "task_list",
            ArtifactType::Doc => "doc",
            ArtifactType::Asset => "asset",
        }
    }
    pub fn parse(s: &str) -> ArtifactType {
        match s {
            "skill" => ArtifactType::Skill,
            "prompt" => ArtifactType::Prompt,
            "task_list" => ArtifactType::TaskList,
            "asset" => ArtifactType::Asset,
            _ => ArtifactType::Doc,
        }
    }
    pub fn is_text(self) -> bool {
        self != ArtifactType::Asset
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Root {
    pub id: String,
    /// Canonical key: absolute, forward slashes.
    pub path: String,
    /// The same path rendered with `~` for display.
    pub display: String,
    pub kind: RootKind,
    pub policy: Policy,
    pub label: String,
    pub added_at: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PathPolicy {
    pub id: String,
    pub root_id: String,
    pub pattern: String,
    pub policy: Policy,
}

/// A path that has passed sandboxing, together with the root that admits it.
#[derive(Debug, Clone)]
pub struct Resolved {
    pub root: Root,
    /// Canonical key.
    pub path: String,
    pub fs_path: PathBuf,
}

impl Resolved {
    pub fn display(&self) -> String {
        display_path(&self.path)
    }
}

// ---------------------------------------------------------------------------
// Roots
// ---------------------------------------------------------------------------

fn row_to_root(row: &rusqlite::Row<'_>) -> rusqlite::Result<Root> {
    let path: String = row.get("path")?;
    Ok(Root {
        id: row.get("id")?,
        display: display_path(&path),
        path,
        kind: RootKind::parse(&row.get::<_, String>("kind")?),
        policy: Policy::parse(&row.get::<_, String>("policy")?),
        label: row.get::<_, Option<String>>("label")?.unwrap_or_default(),
        added_at: row.get("added_at")?,
    })
}

pub fn list_roots(store: &Store) -> Result<Vec<Root>> {
    let conn = store.conn();
    let mut stmt = conn.prepare("SELECT * FROM roots ORDER BY added_at")?;
    let rows = stmt
        .query_map([], row_to_root)?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    Ok(rows)
}

pub fn get_root(store: &Store, id: &str) -> Result<Root> {
    let conn = store.conn();
    conn.query_row("SELECT * FROM roots WHERE id = ?1", [id], row_to_root)
        .optional()?
        .ok_or_else(|| Error::not_found(format!("root {id}")))
}

pub fn add_root(store: &Store, input: &str, policy: Policy, label: Option<&str>) -> Result<Root> {
    let expanded = expand_tilde(input.trim());
    if !expanded.exists() {
        return Err(Error::not_found(format!("{} does not exist", expanded.display())));
    }
    let key = canonical_key(&expanded);
    let kind = if expanded.is_dir() { RootKind::Dir } else { RootKind::File };

    // Nesting one root inside another would double-watch and double-snapshot.
    for existing in list_roots(store)? {
        if is_under(&existing.path, &key) {
            return Err(Error::invalid(format!(
                "{} is already covered by the root {}",
                display_path(&key),
                existing.display
            )));
        }
        if is_under(&key, &existing.path) {
            return Err(Error::invalid(format!(
                "{} contains the existing root {}; remove that one first",
                display_path(&key),
                existing.display
            )));
        }
    }

    let label = label
        .map(str::to_string)
        .filter(|l| !l.trim().is_empty())
        .unwrap_or_else(|| {
            expanded
                .file_name()
                .map(|n| n.to_string_lossy().to_string())
                .unwrap_or_else(|| key.clone())
        });

    let conn = store.conn();
    let id = Store::alloc_id(&conn, "roots", "root")?;
    conn.execute(
        "INSERT INTO roots(id, path, path_key, kind, policy, label, added_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
        (
            &id,
            &key,
            &fold(&key),
            kind.as_str(),
            policy.as_str(),
            &label,
            now_ms(),
        ),
    )?;
    drop(conn);
    get_root(store, &id)
}

/// Forget a root. History is kept: the snapshots stay in the store, so
/// re-adding the root re-attaches its timeline instead of starting over.
pub fn remove_root(store: &Store, id: &str) -> Result<()> {
    let conn = store.conn();
    let n = conn.execute("DELETE FROM roots WHERE id = ?1", [id])?;
    if n == 0 {
        return Err(Error::not_found(format!("root {id}")));
    }
    Ok(())
}

pub fn set_root_policy(store: &Store, id: &str, policy: Policy) -> Result<Root> {
    {
        let conn = store.conn();
        let n = conn.execute(
            "UPDATE roots SET policy = ?2 WHERE id = ?1",
            (id, policy.as_str()),
        )?;
        if n == 0 {
            return Err(Error::not_found(format!("root {id}")));
        }
    }
    get_root(store, id)
}

pub fn set_path_policy(store: &Store, root_id: &str, pattern: &str, policy: Policy) -> Result<PathPolicy> {
    // Validate the glob here so a bad pattern fails at the point of entry
    // rather than silently never matching.
    globset::Glob::new(pattern).map_err(|e| Error::invalid(format!("bad glob `{pattern}`: {e}")))?;
    let conn = store.conn();
    conn.execute(
        "DELETE FROM path_policies WHERE root_id = ?1 AND pattern = ?2",
        (root_id, pattern),
    )?;
    let id = Store::alloc_id(&conn, "path_policies", "pol")?;
    conn.execute(
        "INSERT INTO path_policies(id, root_id, pattern, policy, created_at)
         VALUES (?1, ?2, ?3, ?4, ?5)",
        (&id, root_id, pattern, policy.as_str(), now_ms()),
    )?;
    Ok(PathPolicy {
        id,
        root_id: root_id.to_string(),
        pattern: pattern.to_string(),
        policy,
    })
}

pub fn remove_path_policy(store: &Store, id: &str) -> Result<()> {
    let conn = store.conn();
    conn.execute("DELETE FROM path_policies WHERE id = ?1", [id])?;
    Ok(())
}

pub fn list_path_policies(store: &Store, root_id: &str) -> Result<Vec<PathPolicy>> {
    let conn = store.conn();
    let mut stmt = conn.prepare(
        "SELECT id, root_id, pattern, policy FROM path_policies WHERE root_id = ?1 ORDER BY LENGTH(pattern) DESC",
    )?;
    let rows = stmt
        .query_map([root_id], |r| {
            Ok(PathPolicy {
                id: r.get(0)?,
                root_id: r.get(1)?,
                pattern: r.get(2)?,
                policy: Policy::parse(&r.get::<_, String>(3)?),
            })
        })?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    Ok(rows)
}

/// The effective write policy for one path: an explicit per-path glob wins,
/// then the root's setting, and `auto` finally resolves by artifact type.
pub fn policy_for(store: &Store, resolved: &Resolved, ty: ArtifactType) -> Result<Policy> {
    let relative = relative_to_root(&resolved.root, &resolved.path);
    // Longest pattern first: the most specific override wins.
    for pp in list_path_policies(store, &resolved.root.id)? {
        if let Ok(glob) = globset::Glob::new(&pp.pattern) {
            let matcher = glob.compile_matcher();
            if matcher.is_match(&relative) || matcher.is_match(&resolved.path) {
                return Ok(pp.policy.resolve(ty));
            }
        }
    }
    Ok(resolved.root.policy.resolve(ty))
}

pub fn relative_to_root(root: &Root, path: &str) -> String {
    if path.len() > root.path.len() && is_under(&root.path, path) {
        path[root.path.len()..].trim_start_matches('/').to_string()
    } else {
        path.rsplit('/').next().unwrap_or(path).to_string()
    }
}

// ---------------------------------------------------------------------------
// Sandboxing
// ---------------------------------------------------------------------------

/// Case-folded key for indexed lookups on case-insensitive filesystems.
pub fn fold(key: &str) -> String {
    if cfg!(windows) {
        key.to_lowercase()
    } else {
        key.to_string()
    }
}

/// Resolve a caller-supplied path against the corpus.
///
/// Accepts an absolute path, a `~`-relative path, or a path relative to a
/// registered root. Rejects anything that lands outside every root, including
/// `..` traversal and symlinks that escape — `canonical_key` resolves links
/// before the containment test, so a link out of a root fails the test.
pub fn resolve(store: &Store, input: &str) -> Result<Resolved> {
    let roots = list_roots(store)?;
    if roots.is_empty() {
        return Err(Error::OutsideRoot(format!(
            "{input}: no roots are registered yet"
        )));
    }
    resolve_within(&roots, input)
}

pub fn resolve_within(roots: &[Root], input: &str) -> Result<Resolved> {
    let trimmed = input.trim();
    if trimmed.is_empty() {
        return Err(Error::invalid("path is empty"));
    }

    let mut candidates: Vec<String> = Vec::new();
    let expanded = expand_tilde(trimmed);
    if expanded.is_absolute() {
        candidates.push(canonical_key(&expanded));
    } else {
        // Relative: try it under every root, and require exactly one hit so a
        // bare `SKILL.md` can never silently target the wrong skill.
        let cleaned = trimmed.trim_start_matches("./").trim_start_matches(".\\");
        for root in roots {
            let joined = to_fs_path(&root.path).join(to_fs_path(cleaned));
            let key = canonical_key(&joined);
            if is_under(&root.path, &key) {
                candidates.push(key);
            }
        }
        candidates.sort();
        candidates.dedup();
        let existing: Vec<String> = candidates
            .iter()
            .filter(|c| to_fs_path(c).exists())
            .cloned()
            .collect();
        if existing.len() > 1 {
            return Err(Error::invalid(format!(
                "`{trimmed}` is ambiguous; it matches {} roots. Pass an absolute path.",
                existing.len()
            )));
        }
        if existing.len() == 1 {
            candidates = existing;
        } else if candidates.len() > 1 {
            // Nothing is there yet — creating a document. Picking the first
            // root would put it somewhere the caller did not choose.
            return Err(Error::invalid(format!(
                "`{trimmed}` could be created under {} different roots. Pass an absolute path.",
                candidates.len()
            )));
        }
    }

    for key in &candidates {
        for root in roots {
            let admitted = match root.kind {
                RootKind::Dir => is_under(&root.path, key),
                // A file root admits exactly itself.
                RootKind::File => crate::util::path_eq(&root.path, key),
            };
            if admitted {
                return Ok(Resolved {
                    root: root.clone(),
                    fs_path: to_fs_path(key),
                    path: key.clone(),
                });
            }
        }
    }

    Err(Error::OutsideRoot(display_path(
        candidates.first().map(String::as_str).unwrap_or(trimmed),
    )))
}

// ---------------------------------------------------------------------------
// Walking
// ---------------------------------------------------------------------------

/// Directories that are never part of a markdown corpus. `.git` is on this
/// list because Folio never reads or writes a repository — export, not entangle.
const SKIP_DIRS: &[&str] = &[
    ".git", "node_modules", "target", "__pycache__", ".venv", "venv", ".idea",
    ".vscode", "dist", "build", ".next", ".cache", ".mypy_cache", ".pytest_cache",
];

fn is_skipped_dir(name: &str) -> bool {
    SKIP_DIRS.iter().any(|d| name.eq_ignore_ascii_case(d))
}

/// Every file inside a root, in a stable order.
pub fn walk_root(root: &Root) -> Vec<PathBuf> {
    let base = to_fs_path(&root.path);
    if root.kind == RootKind::File {
        return if base.is_file() { vec![base] } else { Vec::new() };
    }
    let mut out: Vec<PathBuf> = walkdir::WalkDir::new(&base)
        .follow_links(false)
        .into_iter()
        .filter_entry(|e| {
            if e.depth() == 0 {
                return true;
            }
            let name = e.file_name().to_string_lossy();
            if e.file_type().is_dir() {
                !is_skipped_dir(&name)
            } else {
                !name.starts_with(".folio-tmp-") && !name.ends_with(".folio-tmp")
            }
        })
        .filter_map(|e| e.ok())
        .filter(|e| e.file_type().is_file())
        .map(|e| e.into_path())
        .collect();
    out.sort();
    out
}

// ---------------------------------------------------------------------------
// Artifact typing
// ---------------------------------------------------------------------------

const MARKDOWN_EXTS: &[&str] = &["md", "markdown", "mdown", "mkd", "mdx"];

pub fn is_markdown_path(path: &str) -> bool {
    let ext = path.rsplit('.').next().unwrap_or("").to_lowercase();
    path.contains('.') && MARKDOWN_EXTS.contains(&ext.as_str())
}

fn file_name_of(path: &str) -> &str {
    path.rsplit('/').next().unwrap_or(path)
}

fn parent_of(path: &str) -> Option<&str> {
    path.rfind('/').map(|i| &path[..i])
}

/// Structural typing. Order matters: a `SKILL.md` that happens to contain a
/// `{{slot}}` is still a skill.
pub fn detect_type(path: &str, content: &str) -> ArtifactType {
    if !is_markdown_path(path) {
        return ArtifactType::Asset;
    }

    let fm = frontmatter::parse(content);
    let body = &content[fm.body_offset.min(content.len())..];

    let named = fm.get_str("name").is_some_and(|s| !s.trim().is_empty())
        && fm.get_str("description").is_some_and(|s| !s.trim().is_empty());

    if named {
        if file_name_of(path).eq_ignore_ascii_case("SKILL.md") {
            return ArtifactType::Skill;
        }
        if has_skill_sibling_dirs(path) {
            return ArtifactType::Skill;
        }
    }

    if fm.string_list("variables").is_some_and(|v| !v.is_empty()) || has_slots(body) {
        return ArtifactType::Prompt;
    }

    if is_predominantly_tasks(body) {
        return ArtifactType::TaskList;
    }

    ArtifactType::Doc
}

/// A skill tree is recognised by its progressive-disclosure directories.
pub fn has_skill_sibling_dirs(path: &str) -> bool {
    let Some(parent) = parent_of(path) else {
        return false;
    };
    let base = to_fs_path(parent);
    ["commands", "templates", "references", "scripts", "assets"]
        .iter()
        .any(|d| base.join(d).is_dir())
}

/// `{{slot}}` occurrences, ignoring fenced code — a prompt library and a doc
/// about templating should not be confused.
pub fn has_slots(body: &str) -> bool {
    !slot_names(body).is_empty()
}

pub fn slot_names(body: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for line in outside_code_fences(body) {
        let bytes = line.as_bytes();
        let mut i = 0;
        while i + 3 < bytes.len() {
            if bytes[i] == b'{' && bytes[i + 1] == b'{' {
                if let Some(close) = line[i + 2..].find("}}") {
                    let name = line[i + 2..i + 2 + close].trim();
                    // `{{{ }}}` and Handlebars helpers are not variable slots.
                    let plain = !name.is_empty()
                        && name.len() <= 64
                        && name
                            .chars()
                            .all(|c| c.is_alphanumeric() || c == '_' || c == '-' || c == '.');
                    if plain && !out.iter().any(|e| e == name) {
                        out.push(name.to_string());
                    }
                    i += 2 + close + 2;
                    continue;
                } else {
                    break;
                }
            }
            i += 1;
        }
    }
    out
}

/// Lines of `body` that are not inside a fenced code block.
pub fn outside_code_fences(body: &str) -> Vec<&str> {
    let mut out = Vec::new();
    let mut fence: Option<String> = None;
    for line in body.lines() {
        let trimmed = line.trim_start();
        let is_fence = trimmed.starts_with("```") || trimmed.starts_with("~~~");
        match (&fence, is_fence) {
            (None, true) => {
                let marker: String = trimmed.chars().take_while(|c| *c == '`' || *c == '~').collect();
                fence = Some(marker);
            }
            (Some(marker), true) => {
                if trimmed.starts_with(marker.as_str()) {
                    fence = None;
                }
            }
            (None, false) => out.push(line),
            (Some(_), false) => {}
        }
    }
    out
}

/// A GFM checkbox line, e.g. `- [ ] Ship Folio 1.0 @carlos #release`.
pub fn is_task_line(line: &str) -> bool {
    let t = line.trim_start();
    let Some(rest) = t
        .strip_prefix("- ")
        .or_else(|| t.strip_prefix("* "))
        .or_else(|| t.strip_prefix("+ "))
    else {
        return false;
    };
    let rest = rest.trim_start();
    let bytes = rest.as_bytes();
    bytes.len() >= 3
        && bytes[0] == b'['
        && (bytes[1] == b' ' || bytes[1] == b'x' || bytes[1] == b'X')
        && bytes[2] == b']'
}

/// "Predominantly task-list items": the file is a list of things to do, not a
/// document that happens to contain a checklist.
fn is_predominantly_tasks(body: &str) -> bool {
    let mut tasks = 0usize;
    let mut content_lines = 0usize;
    for line in outside_code_fences(body) {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        if is_task_line(line) {
            tasks += 1;
            content_lines += 1;
        } else if trimmed.starts_with('#') || trimmed.starts_with("<!--") {
            // Headings group a task list; they are structure, not content.
            continue;
        } else {
            content_lines += 1;
        }
    }
    tasks >= 2 && content_lines > 0 && (tasks as f64 / content_lines as f64) >= 0.5
}

/// Read a document's text, refusing anything too large to edit sensibly.
pub fn read_text(path: &Path, max_bytes: u64) -> Result<String> {
    let meta = std::fs::metadata(path)?;
    if meta.len() > max_bytes {
        return Err(Error::TooLarge(format!(
            "{} is {} bytes; the editor limit is {max_bytes}",
            path.display(),
            meta.len()
        )));
    }
    let bytes = std::fs::read(path)?;
    Ok(String::from_utf8_lossy(&bytes).into_owned())
}

pub fn count_docs(conn: &Connection, root_id: &str) -> Result<i64> {
    let n: i64 = conn.query_row(
        "SELECT COUNT(DISTINCT path_key) FROM snapshots WHERE root_id = ?1",
        [root_id],
        |r| r.get(0),
    )?;
    Ok(n)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn skill_md_with_frontmatter_is_a_skill() {
        let doc = "---\nname: visual-explainer\ndescription: Draws\n---\n# Overview\n";
        assert_eq!(detect_type("C:/x/visual-explainer/SKILL.md", doc), ArtifactType::Skill);
    }

    #[test]
    fn a_slot_body_is_a_prompt() {
        assert_eq!(detect_type("C:/x/a.md", "Write about {{topic}}.\n"), ArtifactType::Prompt);
        assert_eq!(
            detect_type("C:/x/a.md", "---\nvariables: [topic]\n---\nhello\n"),
            ArtifactType::Prompt
        );
    }

    #[test]
    fn slots_inside_code_fences_do_not_make_a_prompt() {
        let doc = "# Templating\n\n```\n{{topic}}\n```\n\nProse about templates.\n";
        assert_eq!(detect_type("C:/x/a.md", doc), ArtifactType::Doc);
    }

    #[test]
    fn a_checkbox_file_is_a_task_list() {
        let doc = "# TODO\n\n- [ ] Ship Folio 1.0 @carlos #release\n- [x] Write the spec @carlos\n";
        assert_eq!(detect_type("C:/x/TODO.md", doc), ArtifactType::TaskList);
    }

    #[test]
    fn a_doc_with_one_checklist_is_still_a_doc() {
        let doc = "# Design\n\nProse paragraph one.\nProse paragraph two.\nMore prose here.\n\n- [ ] a stray item\n- [ ] another\n\nAnd a closing paragraph that keeps going.\n";
        assert_eq!(detect_type("C:/x/a.md", doc), ArtifactType::Doc);
    }

    #[test]
    fn non_markdown_is_an_asset() {
        assert_eq!(detect_type("C:/x/diagram.png", ""), ArtifactType::Asset);
    }

    #[test]
    fn auto_policy_splits_by_type() {
        assert_eq!(Policy::Auto.resolve(ArtifactType::TaskList), Policy::Direct);
        assert_eq!(Policy::Auto.resolve(ArtifactType::Skill), Policy::Propose);
        assert_eq!(Policy::Auto.resolve(ArtifactType::Doc), Policy::Propose);
        // An explicit setting is never overridden by type.
        assert_eq!(Policy::Propose.resolve(ArtifactType::TaskList), Policy::Propose);
    }

    #[test]
    fn slot_names_are_deduped_and_ordered() {
        let names = slot_names("{{topic}} and {{tone}} and {{topic}} again");
        assert_eq!(names, vec!["topic", "tone"]);
    }
}
