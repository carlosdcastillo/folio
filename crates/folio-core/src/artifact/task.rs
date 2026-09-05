//! Task lists: GFM checkboxes with two optional trailing tokens Folio parses
//! and preserves, `@owner` and `#tag`.
//!
//! ```markdown
//! - [ ] Ship Folio 1.0 @carlos #release
//! - [x] Write the spec @carlos
//! ```
//!
//! Task ids are line-anchored (`L42`) and valid only for the snapshot they
//! were read from. Writes carry the version they were read against, and a
//! stale write fails *with the current list attached* — agents mutate task
//! lists the way they should: read, then write.

use super::Finding;
use crate::corpus::is_task_line;
use crate::error::{Error, Result};
use crate::util::{dominant_newline, join_lines, split_lines};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Task {
    /// Line-anchored id, e.g. `L42`.
    pub id: String,
    /// 1-based line number in the snapshot this was read from.
    pub line: usize,
    /// Text with the `@owner` and `#tag` tokens removed.
    pub text: String,
    /// The original line, verbatim.
    pub raw: String,
    pub done: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub owner: Option<String>,
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    pub tags: Vec<String>,
    /// Indent depth in spaces, so nested subtasks keep their shape.
    pub indent: usize,
    /// Document this task lives in. Filled in by the aggregator.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub doc: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub display: Option<String>,
    /// The version these ids are anchored to.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
}

pub fn id_for_line(line: usize) -> String {
    format!("L{line}")
}

pub fn line_of_id(id: &str) -> Result<usize> {
    let digits = id.trim().trim_start_matches(['L', 'l']);
    digits
        .parse::<usize>()
        .ok()
        .filter(|n| *n > 0)
        .ok_or_else(|| Error::invalid(format!("`{id}` is not a task id; expected `L42`")))
}

/// Parse every task in a document.
pub fn parse(content: &str) -> Vec<Task> {
    let mut out = Vec::new();
    let mut fence: Option<String> = None;
    for (i, line) in content.lines().enumerate() {
        let trimmed = line.trim_start();
        let is_fence = trimmed.starts_with("```") || trimmed.starts_with("~~~");
        if is_fence {
            match &fence {
                None => fence = Some(trimmed.chars().take_while(|c| *c == '`' || *c == '~').collect()),
                Some(marker) if trimmed.starts_with(marker.as_str()) => fence = None,
                Some(_) => {}
            }
            continue;
        }
        if fence.is_some() || !is_task_line(line) {
            continue;
        }
        out.push(parse_line(line, i + 1));
    }
    out
}

fn parse_line(line: &str, line_no: usize) -> Task {
    let indent = line.len() - line.trim_start().len();
    let trimmed = line.trim_start();
    let after_bullet = &trimmed[2..].trim_start();
    let done = after_bullet.as_bytes().get(1).is_some_and(|b| *b == b'x' || *b == b'X');
    let body = after_bullet[3..].trim();

    let (text, owner, tags) = split_tokens(body);
    Task {
        id: id_for_line(line_no),
        line: line_no,
        text,
        raw: line.to_string(),
        done,
        owner,
        tags,
        indent,
        doc: None,
        display: None,
        version: None,
    }
}

/// Pull `@owner` and `#tag` out of the task text. The grammar stays at exactly
/// these two tokens: every token added is a token agents will misuse.
fn split_tokens(body: &str) -> (String, Option<String>, Vec<String>) {
    let mut owner: Option<String> = None;
    let mut tags: Vec<String> = Vec::new();
    let mut words: Vec<&str> = Vec::new();

    for word in body.split_whitespace() {
        if let Some(name) = word.strip_prefix('@') {
            let clean = name.trim_end_matches(|c: char| c.is_ascii_punctuation() && c != '_' && c != '-');
            if !clean.is_empty() && owner.is_none() {
                owner = Some(clean.to_string());
                continue;
            }
        }
        if let Some(tag) = word.strip_prefix('#') {
            let clean = tag.trim_end_matches(|c: char| c.is_ascii_punctuation() && c != '_' && c != '-');
            if !clean.is_empty() {
                tags.push(clean.to_string());
                continue;
            }
        }
        words.push(word);
    }

    (words.join(" "), owner, tags)
}

/// Render a task line from its parts, in the canonical order the parser reads.
pub fn format_line(text: &str, done: bool, owner: Option<&str>, tags: &[String], indent: usize) -> String {
    let mut line = format!(
        "{}- [{}] {}",
        " ".repeat(indent),
        if done { "x" } else { " " },
        text.trim()
    );
    if let Some(owner) = owner.map(str::trim).filter(|o| !o.is_empty()) {
        line.push_str(&format!(" @{}", owner.trim_start_matches('@')));
    }
    for tag in tags.iter().map(|t| t.trim()).filter(|t| !t.is_empty()) {
        line.push_str(&format!(" #{}", tag.trim_start_matches('#')));
    }
    line
}

// ---------------------------------------------------------------------------
// Mutation
// ---------------------------------------------------------------------------

/// Flip a checkbox. Returns the new document text.
///
/// Fails if the line is no longer a task — that is the staleness signal the
/// caller turns into an error carrying the current list.
pub fn set_status(content: &str, task_id: &str, done: bool) -> Result<String> {
    let line_no = line_of_id(task_id)?;
    let (mut lines, trailing) = split_lines(content);
    let newline = dominant_newline(content);

    let Some(line) = lines.get(line_no - 1) else {
        return Err(Error::not_found(format!(
            "{task_id} is past the end of the document ({} lines)",
            lines.len()
        )));
    };
    if !is_task_line(line) {
        return Err(Error::not_found(format!("line {line_no} is no longer a task")));
    }

    let task = parse_line(line, line_no);
    if task.done == done {
        return Ok(content.to_string());
    }
    lines[line_no - 1] = format_line(&task.text, done, task.owner.as_deref(), &task.tags, task.indent);
    Ok(join_lines(&lines, trailing, newline))
}

/// Append a task. New items join the end of the last existing task block so an
/// agent's overnight additions land where a human would put them, rather than
/// at the bottom of the file under an unrelated heading.
pub fn add(content: &str, text: &str, owner: Option<&str>, tags: &[String]) -> Result<(String, usize)> {
    if text.trim().is_empty() {
        return Err(Error::invalid("task text is empty"));
    }
    let (mut lines, trailing) = split_lines(content);
    let newline = dominant_newline(content);

    let indent = parse(content)
        .iter()
        .filter(|t| t.indent == 0)
        .map(|t| t.indent)
        .next()
        .unwrap_or(0);
    let new_line = format_line(text, false, owner, tags, indent);

    let insert_at = match last_task_line(&lines) {
        Some(last) => last + 1,
        None => {
            // No tasks yet: keep a blank line between prose and the new list.
            if !lines.is_empty() && !lines.last().is_some_and(|l| l.trim().is_empty()) {
                lines.push(String::new());
            }
            lines.len()
        }
    };
    lines.insert(insert_at, new_line);
    Ok((join_lines(&lines, trailing || !content.is_empty(), newline), insert_at + 1))
}

fn last_task_line(lines: &[String]) -> Option<usize> {
    let mut fence: Option<String> = None;
    let mut last = None;
    for (i, line) in lines.iter().enumerate() {
        let trimmed = line.trim_start();
        if trimmed.starts_with("```") || trimmed.starts_with("~~~") {
            match &fence {
                None => fence = Some(trimmed.chars().take_while(|c| *c == '`' || *c == '~').collect()),
                Some(marker) if trimmed.starts_with(marker.as_str()) => fence = None,
                Some(_) => {}
            }
            continue;
        }
        if fence.is_none() && is_task_line(line) {
            last = Some(i);
        }
    }
    last
}

// ---------------------------------------------------------------------------
// Validation
// ---------------------------------------------------------------------------

pub fn validate(content: &str) -> Vec<Finding> {
    let tasks = parse(content);
    let mut out = Vec::new();

    for task in &tasks {
        if task.text.trim().is_empty() {
            out.push(Finding::warning("task.empty", "This task has no text.").at(task.line));
        }
        for tag in &task.tags {
            if tag.chars().any(|c| !c.is_alphanumeric() && c != '-' && c != '_' && c != '/') {
                out.push(
                    Finding::info("task.tag.unusual", format!("`#{tag}` contains punctuation."))
                        .at(task.line),
                );
            }
        }
    }

    let open = tasks.iter().filter(|t| !t.done).count();
    out.push(Finding::info(
        "task.summary",
        format!("{} task(s), {open} open.", tasks.len()),
    ));
    out
}

// ---------------------------------------------------------------------------
// Filtering, for `task_list` and the Today view
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Default)]
pub struct Filter {
    pub owner: Option<String>,
    pub tag: Option<String>,
    pub open_only: bool,
    pub query: Option<String>,
}

impl Filter {
    pub fn matches(&self, task: &Task) -> bool {
        if self.open_only && task.done {
            return false;
        }
        if let Some(owner) = &self.owner {
            let want = owner.trim_start_matches('@');
            if !task.owner.as_deref().is_some_and(|o| o.eq_ignore_ascii_case(want)) {
                return false;
            }
        }
        if let Some(tag) = &self.tag {
            let want = tag.trim_start_matches('#');
            if !task.tags.iter().any(|t| t.eq_ignore_ascii_case(want)) {
                return false;
            }
        }
        if let Some(query) = &self.query {
            if !task.text.to_lowercase().contains(&query.to_lowercase()) {
                return false;
            }
        }
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const DOC: &str = "# TODO\n\n- [ ] Ship Folio 1.0 @carlos #release\n- [x] Write the spec @carlos\n  - [ ] Nested item #docs\n";

    #[test]
    fn parses_the_grammar() {
        let tasks = parse(DOC);
        assert_eq!(tasks.len(), 3);
        assert_eq!(tasks[0].id, "L3");
        assert_eq!(tasks[0].text, "Ship Folio 1.0");
        assert_eq!(tasks[0].owner.as_deref(), Some("carlos"));
        assert_eq!(tasks[0].tags, vec!["release"]);
        assert!(!tasks[0].done);
        assert!(tasks[1].done);
        assert_eq!(tasks[2].indent, 2);
    }

    #[test]
    fn tasks_inside_code_fences_are_not_tasks() {
        let doc = "```\n- [ ] not a real task\n```\n- [ ] a real one\n";
        let tasks = parse(doc);
        assert_eq!(tasks.len(), 1);
        assert_eq!(tasks[0].line, 4);
    }

    #[test]
    fn set_status_preserves_owner_tags_and_indent() {
        let out = set_status(DOC, "L5", true).unwrap();
        assert!(out.contains("  - [x] Nested item #docs"));
        // Nothing else moved.
        assert!(out.contains("- [ ] Ship Folio 1.0 @carlos #release"));
    }

    #[test]
    fn set_status_on_a_line_that_is_no_longer_a_task_fails() {
        let err = set_status(DOC, "L1", true).unwrap_err();
        assert_eq!(err.code(), "not_found");
    }

    #[test]
    fn add_appends_after_the_last_task() {
        let (out, line) = add(DOC, "Package the installer", Some("carlos"), &["release".into()]).unwrap();
        assert_eq!(line, 6);
        assert!(out.ends_with("- [ ] Package the installer @carlos #release\n"));
    }

    #[test]
    fn add_to_a_document_with_no_tasks_starts_a_list() {
        let (out, _) = add("# Notes\n", "First thing", None, &[]).unwrap();
        assert_eq!(out, "# Notes\n\n- [ ] First thing\n");
    }

    #[test]
    fn round_trips_a_formatted_line() {
        let line = format_line("Ship it", false, Some("carlos"), &["release".into()], 0);
        assert_eq!(line, "- [ ] Ship it @carlos #release");
        let task = parse_line(&line, 1);
        assert_eq!(task.text, "Ship it");
        assert_eq!(task.owner.as_deref(), Some("carlos"));
    }

    #[test]
    fn filters_by_owner_tag_and_openness() {
        let tasks = parse(DOC);
        let f = Filter { owner: Some("@carlos".into()), open_only: true, ..Default::default() };
        let hits: Vec<_> = tasks.iter().filter(|t| f.matches(t)).collect();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].text, "Ship Folio 1.0");
    }
}
