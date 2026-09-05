//! Prose-aware diffing, in three levels.
//!
//! 1. **Line diff** — `similar` over the two blobs.
//! 2. **Paragraph grouping** — line hunks are merged and split at block
//!    boundaries, so a diff reads as "this paragraph was rewritten" instead of
//!    interleaved line noise. Headings, list items and fenced blocks are their
//!    own units.
//! 3. **Word-level intra-line highlights** — changed segments inside a
//!    rewritten line are emitted as token ranges; the frontend renders spans.
//!
//! Move detection is deliberately out of scope for v1: a paragraph that moved
//! is reported as a delete plus an add.

pub mod patch;

use crate::util::{join_lines, split_lines};
use serde::{Deserialize, Serialize};
use similar::{ChangeTag, TextDiff};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum RowKind {
    Context,
    Insert,
    Delete,
}

/// A word-level range inside one row. Only `changed` ranges are emphasised;
/// the unchanged ones are emitted too so the frontend can render without
/// recomputing offsets.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Span {
    pub start: usize,
    pub end: usize,
    pub changed: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Row {
    pub kind: RowKind,
    pub old_line: Option<usize>,
    pub new_line: Option<usize>,
    pub text: String,
    /// Empty unless this row is half of a detected rewrite pair.
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    pub spans: Vec<Span>,
}

/// One reviewable unit. Accept/reject in the review view operates on these.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Hunk {
    pub index: usize,
    pub old_start: usize,
    pub old_lines: usize,
    pub new_start: usize,
    pub new_lines: usize,
    /// Nearest preceding heading, the way git shows the enclosing function.
    pub header: String,
    pub added: usize,
    pub removed: usize,
    pub rows: Vec<Row>,
}

impl Hunk {
    /// `@@ -a,b +c,d @@` as a reviewer reads it (1-based).
    pub fn range_header(&self) -> String {
        format!(
            "@@ -{},{} +{},{} @@",
            self.old_start + 1,
            self.old_lines,
            self.new_start + 1,
            self.new_lines
        )
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Diff {
    pub hunks: Vec<Hunk>,
    pub added: usize,
    pub removed: usize,
    /// True when either side is not text; assets are tracked but never diffed.
    pub binary: bool,
    pub old_line_count: usize,
    pub new_line_count: usize,
}

impl Diff {
    pub fn is_empty(&self) -> bool {
        self.hunks.is_empty()
    }
}

#[derive(Debug, Clone, Copy)]
pub struct Options {
    /// Extra lines kept around a change after block expansion.
    pub context: usize,
    /// Above this, word-level highlighting is skipped — it is a nicety, and
    /// the 100 KB budget is not.
    pub word_level_max_bytes: usize,
}

impl Default for Options {
    fn default() -> Self {
        Options { context: 1, word_level_max_bytes: 4096 }
    }
}

// ---------------------------------------------------------------------------
// Block structure (level 2)
// ---------------------------------------------------------------------------

/// Does `line` open a new block? Headings, list items, fences, tables, and
/// blockquotes each start their own unit; everything else continues the
/// paragraph it is in.
fn starts_block(line: &str) -> bool {
    let t = line.trim_start();
    if t.is_empty() {
        return true;
    }
    if t.starts_with('#') {
        return true;
    }
    if t.starts_with("```") || t.starts_with("~~~") {
        return true;
    }
    if t.starts_with('>') || t.starts_with('|') {
        return true;
    }
    if t.starts_with("---") || t.starts_with("***") || t.starts_with("___") {
        return true;
    }
    // Bullet or ordered list item.
    if let Some(rest) = t.strip_prefix("- ").or_else(|| t.strip_prefix("* ")).or_else(|| t.strip_prefix("+ ")) {
        let _ = rest;
        return true;
    }
    let digits: String = t.chars().take_while(char::is_ascii_digit).collect();
    if !digits.is_empty() {
        let after = &t[digits.len()..];
        if after.starts_with(". ") || after.starts_with(") ") {
            return true;
        }
    }
    false
}

fn is_blank(line: &str) -> bool {
    line.trim().is_empty()
}

/// Walk back to the first line of the block containing `i`.
fn block_start(lines: &[String], i: usize) -> usize {
    if i >= lines.len() || is_blank(&lines[i]) {
        return i;
    }
    let mut j = i;
    while j > 0 {
        if starts_block(&lines[j]) {
            break;
        }
        if is_blank(&lines[j - 1]) {
            break;
        }
        j -= 1;
    }
    j
}

/// Walk forward past the last line of the block containing `i` (exclusive).
fn block_end(lines: &[String], i: usize) -> usize {
    if i >= lines.len() {
        return lines.len();
    }
    if is_blank(&lines[i]) {
        return i + 1;
    }
    let mut j = i + 1;
    while j < lines.len() {
        if is_blank(&lines[j]) || starts_block(&lines[j]) {
            break;
        }
        j += 1;
    }
    j
}

fn nearest_heading(lines: &[String], from: usize) -> String {
    for i in (0..from.min(lines.len())).rev() {
        let t = lines[i].trim_start();
        if t.starts_with('#') {
            return t.trim().to_string();
        }
    }
    String::new()
}

// ---------------------------------------------------------------------------
// The diff itself
// ---------------------------------------------------------------------------

/// A single changed region, in both files' line coordinates. This is the raw
/// level-1 output, before any paragraph expansion — the exact lines that
/// differ and nothing else. Merging needs this precision; display does not.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Range {
    pub old_start: usize,
    pub old_end: usize,
    pub new_start: usize,
    pub new_end: usize,
}

impl Range {
    pub fn touches(&self, other: &Range) -> bool {
        self.old_start < other.old_end && other.old_start < self.old_end
    }
}

/// Level 1: the changed regions, unexpanded.
pub fn change_ranges(old_lines: &[String], new_lines: &[String]) -> Vec<Range> {
    let old_joined = old_lines.join("\n");
    let new_joined = new_lines.join("\n");
    let text_diff = TextDiff::from_lines(&old_joined, &new_joined);

    let mut ranges: Vec<Range> = Vec::new();
    let mut old_i = 0usize;
    let mut new_i = 0usize;
    let mut pending: Option<Range> = None;
    for change in text_diff.iter_all_changes() {
        match change.tag() {
            ChangeTag::Equal => {
                if let Some(r) = pending.take() {
                    ranges.push(r);
                }
                old_i += 1;
                new_i += 1;
            }
            ChangeTag::Delete => {
                let r = pending.get_or_insert(Range {
                    old_start: old_i,
                    old_end: old_i,
                    new_start: new_i,
                    new_end: new_i,
                });
                r.old_end = old_i + 1;
                old_i += 1;
            }
            ChangeTag::Insert => {
                let r = pending.get_or_insert(Range {
                    old_start: old_i,
                    old_end: old_i,
                    new_start: new_i,
                    new_end: new_i,
                });
                r.new_end = new_i + 1;
                new_i += 1;
            }
        }
    }
    if let Some(r) = pending.take() {
        ranges.push(r);
    }
    ranges
}

pub fn diff_text(old: &str, new: &str) -> Diff {
    diff_text_with(old, new, Options::default())
}

pub fn diff_text_with(old: &str, new: &str, opts: Options) -> Diff {
    let (old_lines, _) = split_lines(old);
    let (new_lines, _) = split_lines(new);
    diff_lines(&old_lines, &new_lines, opts)
}

pub fn diff_lines(old_lines: &[String], new_lines: &[String], opts: Options) -> Diff {
    let ranges = change_ranges(old_lines, new_lines);

    if ranges.is_empty() {
        return Diff {
            hunks: Vec::new(),
            added: 0,
            removed: 0,
            binary: false,
            old_line_count: old_lines.len(),
            new_line_count: new_lines.len(),
        };
    }

    // Level 2: expand each range to enclosing blocks, then merge overlaps so a
    // rewritten paragraph is one hunk rather than three interleaved ones.
    let mut expanded: Vec<Range> = ranges
        .iter()
        .map(|r| {
            let old_start = block_start(old_lines, r.old_start.min(old_lines.len().saturating_sub(1)));
            let old_end = if r.old_end > r.old_start {
                block_end(old_lines, r.old_end - 1)
            } else {
                r.old_end
            };
            let new_start = block_start(new_lines, r.new_start.min(new_lines.len().saturating_sub(1)));
            let new_end = if r.new_end > r.new_start {
                block_end(new_lines, r.new_end - 1)
            } else {
                r.new_end
            };
            // Expansion must stay anchored: the same number of unchanged lines
            // is pulled in on both sides, or the region between two hunks
            // would stop being identical and hunk-level accept would corrupt
            // the file. Block expansion sets the appetite; the equal-length
            // constraint and the buffer ends set the limit.
            let back = (r.old_start - old_start)
                .min(r.new_start - new_start)
                .max(opts.context)
                .min(r.old_start)
                .min(r.new_start);
            let fwd = old_end
                .saturating_sub(r.old_end)
                .min(new_end.saturating_sub(r.new_end))
                .max(opts.context)
                .min(old_lines.len().saturating_sub(r.old_end))
                .min(new_lines.len().saturating_sub(r.new_end));
            Range {
                old_start: r.old_start - back,
                old_end: r.old_end + fwd,
                new_start: r.new_start - back,
                new_end: r.new_end + fwd,
            }
        })
        .collect();

    expanded.sort_by_key(|r| r.old_start);
    let mut merged: Vec<Range> = Vec::new();
    for r in expanded {
        match merged.last_mut() {
            Some(prev) if r.old_start <= prev.old_end && r.new_start <= prev.new_end => {
                prev.old_end = prev.old_end.max(r.old_end);
                prev.new_end = prev.new_end.max(r.new_end);
            }
            _ => merged.push(r),
        }
    }

    // Level 3: rows, with word-level spans on rewrite pairs.
    let mut hunks = Vec::new();
    let mut added_total = 0usize;
    let mut removed_total = 0usize;
    for (index, r) in merged.iter().enumerate() {
        let rows = build_rows(old_lines, new_lines, *r, opts);
        let added = rows.iter().filter(|row| row.kind == RowKind::Insert).count();
        let removed = rows.iter().filter(|row| row.kind == RowKind::Delete).count();
        added_total += added;
        removed_total += removed;
        hunks.push(Hunk {
            index,
            old_start: r.old_start,
            old_lines: r.old_end - r.old_start,
            new_start: r.new_start,
            new_lines: r.new_end - r.new_start,
            header: nearest_heading(new_lines, r.new_start),
            added,
            removed,
            rows,
        });
    }

    Diff {
        hunks,
        added: added_total,
        removed: removed_total,
        binary: false,
        old_line_count: old_lines.len(),
        new_line_count: new_lines.len(),
    }
}

fn build_rows(old_lines: &[String], new_lines: &[String], r: Range, opts: Options) -> Vec<Row> {
    let old_slice = &old_lines[r.old_start..r.old_end];
    let new_slice = &new_lines[r.new_start..r.new_end];
    let old_joined = old_slice.join("\n");
    let new_joined = new_slice.join("\n");
    let diff = TextDiff::from_lines(&old_joined, &new_joined);

    let mut rows: Vec<Row> = Vec::new();
    let mut old_no = r.old_start;
    let mut new_no = r.new_start;
    for change in diff.iter_all_changes() {
        let text = change.value().trim_end_matches('\n').to_string();
        match change.tag() {
            ChangeTag::Equal => {
                rows.push(Row {
                    kind: RowKind::Context,
                    old_line: Some(old_no),
                    new_line: Some(new_no),
                    text,
                    spans: Vec::new(),
                });
                old_no += 1;
                new_no += 1;
            }
            ChangeTag::Delete => {
                rows.push(Row {
                    kind: RowKind::Delete,
                    old_line: Some(old_no),
                    new_line: None,
                    text,
                    spans: Vec::new(),
                });
                old_no += 1;
            }
            ChangeTag::Insert => {
                rows.push(Row {
                    kind: RowKind::Insert,
                    old_line: None,
                    new_line: Some(new_no),
                    text,
                    spans: Vec::new(),
                });
                new_no += 1;
            }
        }
    }

    annotate_word_level(&mut rows, opts);
    rows
}

/// Pair each run of deletes with the run of inserts that follows it and mark
/// the words that actually changed. Only same-length runs are paired: guessing
/// across a 3-for-1 rewrite produces noise, not insight.
fn annotate_word_level(rows: &mut [Row], opts: Options) {
    let mut i = 0usize;
    while i < rows.len() {
        if rows[i].kind != RowKind::Delete {
            i += 1;
            continue;
        }
        let del_start = i;
        while i < rows.len() && rows[i].kind == RowKind::Delete {
            i += 1;
        }
        let del_end = i;
        let ins_start = i;
        while i < rows.len() && rows[i].kind == RowKind::Insert {
            i += 1;
        }
        let ins_end = i;

        let del_len = del_end - del_start;
        let ins_len = ins_end - ins_start;
        if del_len == 0 || ins_len == 0 || del_len != ins_len {
            continue;
        }
        for k in 0..del_len {
            let old_text = rows[del_start + k].text.clone();
            let new_text = rows[ins_start + k].text.clone();
            if old_text.len() + new_text.len() > opts.word_level_max_bytes {
                continue;
            }
            let (old_spans, new_spans) = word_spans(&old_text, &new_text);
            rows[del_start + k].spans = old_spans;
            rows[ins_start + k].spans = new_spans;
        }
    }
}

/// Word-level ranges for a rewrite pair. Returns `(old, new)` spans as byte
/// offsets into each row's text.
pub fn word_spans(old: &str, new: &str) -> (Vec<Span>, Vec<Span>) {
    let diff = TextDiff::from_words(old, new);
    let mut old_spans: Vec<Span> = Vec::new();
    let mut new_spans: Vec<Span> = Vec::new();
    let mut old_at = 0usize;
    let mut new_at = 0usize;

    for change in diff.iter_all_changes() {
        let value = change.value();
        let len = value.len();
        match change.tag() {
            ChangeTag::Equal => {
                push_span(&mut old_spans, old_at, old_at + len, false);
                push_span(&mut new_spans, new_at, new_at + len, false);
                old_at += len;
                new_at += len;
            }
            ChangeTag::Delete => {
                push_span(&mut old_spans, old_at, old_at + len, true);
                old_at += len;
            }
            ChangeTag::Insert => {
                push_span(&mut new_spans, new_at, new_at + len, true);
                new_at += len;
            }
        }
    }

    // A row where everything changed gains nothing from highlighting.
    let all_changed = |spans: &Vec<Span>| spans.iter().all(|s| s.changed);
    if all_changed(&old_spans) && all_changed(&new_spans) {
        return (Vec::new(), Vec::new());
    }
    (old_spans, new_spans)
}

fn push_span(spans: &mut Vec<Span>, start: usize, end: usize, changed: bool) {
    if start == end {
        return;
    }
    if let Some(last) = spans.last_mut() {
        if last.changed == changed && last.end == start {
            last.end = end;
            return;
        }
    }
    spans.push(Span { start, end, changed });
}

// ---------------------------------------------------------------------------
// Hunk-level apply (the review view's accept/reject)
// ---------------------------------------------------------------------------

/// Apply only the accepted hunks of `diff` to `old`, taking the rejected
/// regions from `old` unchanged. The regions between hunks are identical on
/// both sides by construction, so this is exact, not a re-merge.
pub fn apply_hunks(old: &str, new: &str, diff: &Diff, accepted: &[usize]) -> String {
    let (old_lines, old_trailing) = split_lines(old);
    let (new_lines, new_trailing) = split_lines(new);
    let newline = crate::util::dominant_newline(old);

    let mut out: Vec<String> = Vec::new();
    let mut cursor = 0usize;
    let mut took_all = true;
    for hunk in &diff.hunks {
        out.extend_from_slice(&old_lines[cursor.min(old_lines.len())..hunk.old_start.min(old_lines.len())]);
        if accepted.contains(&hunk.index) {
            let end = (hunk.new_start + hunk.new_lines).min(new_lines.len());
            out.extend_from_slice(&new_lines[hunk.new_start.min(new_lines.len())..end]);
        } else {
            took_all = false;
            let end = (hunk.old_start + hunk.old_lines).min(old_lines.len());
            out.extend_from_slice(&old_lines[hunk.old_start.min(old_lines.len())..end]);
        }
        cursor = hunk.old_start + hunk.old_lines;
    }
    out.extend_from_slice(&old_lines[cursor.min(old_lines.len())..]);

    // Trailing-newline convention follows whichever side we ended up trusting.
    let trailing = if took_all { new_trailing } else { old_trailing };
    join_lines(&out, trailing, newline)
}

// ---------------------------------------------------------------------------
// Three-way merge (rebasing a conflicting proposal)
// ---------------------------------------------------------------------------

/// Replay `theirs` on top of `ours`, both measured against a common `base`.
///
/// Used to rebase a proposal whose file moved under it: `ours` is what is on
/// disk now, `theirs` is what the agent proposed. Regions only one side
/// touched are taken from that side; a region both sides changed differently
/// is a genuine conflict and is reported rather than guessed at.
///
/// This works on raw change ranges, not on display hunks: paragraph expansion
/// exists to make a diff readable, and using it here would invent conflicts
/// between edits that never overlapped.
pub fn merge3(base: &str, ours: &str, theirs: &str) -> crate::error::Result<String> {
    let (base_lines, base_trailing) = split_lines(base);
    let (our_lines, our_trailing) = split_lines(ours);
    let (their_lines, their_trailing) = split_lines(theirs);
    let newline = crate::util::dominant_newline(ours);

    let our_changes = change_ranges(&base_lines, &our_lines);
    let their_changes = change_ranges(&base_lines, &their_lines);

    // Interleave both sides' changes in base coordinates.
    let mut steps: Vec<(Range, bool)> = Vec::new();
    steps.extend(our_changes.iter().map(|r| (*r, true)));
    steps.extend(their_changes.iter().map(|r| (*r, false)));
    steps.sort_by_key(|(r, ours)| (r.old_start, r.old_end, !*ours));

    let mut out: Vec<String> = Vec::new();
    let mut cursor = 0usize;
    let mut i = 0usize;
    while i < steps.len() {
        let (range, is_ours) = steps[i];

        // Does the other side touch the same base lines?
        let overlapping: Vec<&(Range, bool)> = steps[i + 1..]
            .iter()
            .take_while(|(other, _)| other.old_start < range.old_end.max(range.old_start + 1))
            .filter(|(other, other_is_ours)| *other_is_ours != is_ours && other.touches(&range))
            .collect();

        let replacement = |r: &Range, ours_side: bool| -> Vec<String> {
            let lines = if ours_side { &our_lines } else { &their_lines };
            lines[r.new_start.min(lines.len())..r.new_end.min(lines.len())].to_vec()
        };

        if let Some((other, other_is_ours)) = overlapping.first().copied() {
            let mine = replacement(&range, is_ours);
            let theirs_side = replacement(other, *other_is_ours);
            if mine != theirs_side {
                return Err(crate::error::Error::Conflict(format!(
                    "both sides changed lines {}-{} differently",
                    range.old_start.min(other.old_start) + 1,
                    range.old_end.max(other.old_end)
                )));
            }
            // The same edit made twice is not a conflict; take it once.
            out.extend_from_slice(&base_lines[cursor.min(base_lines.len())..range.old_start.min(base_lines.len())]);
            out.extend(mine);
            cursor = range.old_end.max(other.old_end);
            i += 1;
            while i < steps.len() && steps[i].0.touches(&range) {
                i += 1;
            }
            continue;
        }

        if range.old_start >= cursor {
            out.extend_from_slice(&base_lines[cursor.min(base_lines.len())..range.old_start.min(base_lines.len())]);
            out.extend(replacement(&range, is_ours));
            cursor = range.old_end;
        }
        i += 1;
    }
    out.extend_from_slice(&base_lines[cursor.min(base_lines.len())..]);

    // If either side dropped the trailing newline deliberately, respect it.
    let trailing = our_trailing && their_trailing || (base_trailing && our_trailing);
    Ok(join_lines(&out, trailing, newline))
}

// ---------------------------------------------------------------------------
// Unified diff text
// ---------------------------------------------------------------------------

/// Classic unified diff, for `export_history` and for showing a patch as text.
pub fn unified(old: &str, new: &str, old_label: &str, new_label: &str, context: usize) -> String {
    let text_diff = TextDiff::from_lines(old, new);
    let mut out = String::new();
    out.push_str(&format!("--- {old_label}\n"));
    out.push_str(&format!("+++ {new_label}\n"));
    for group in text_diff.grouped_ops(context.max(1)) {
        let (mut old_start, mut old_len, mut new_start, mut new_len) = (usize::MAX, 0usize, usize::MAX, 0usize);
        for op in &group {
            let o = op.old_range();
            let n = op.new_range();
            if o.start < old_start {
                old_start = o.start;
            }
            if n.start < new_start {
                new_start = n.start;
            }
            old_len += o.len();
            new_len += n.len();
        }
        if old_start == usize::MAX {
            old_start = 0;
        }
        if new_start == usize::MAX {
            new_start = 0;
        }
        out.push_str(&format!(
            "@@ -{},{} +{},{} @@\n",
            old_start + 1,
            old_len,
            new_start + 1,
            new_len
        ));
        for op in &group {
            for change in text_diff.iter_changes(op) {
                let sign = match change.tag() {
                    ChangeTag::Equal => ' ',
                    ChangeTag::Delete => '-',
                    ChangeTag::Insert => '+',
                };
                let value = change.value();
                out.push(sign);
                out.push_str(value.trim_end_matches('\n'));
                out.push('\n');
                if !value.ends_with('\n') {
                    out.push_str("\\ No newline at end of file\n");
                }
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identical_text_has_no_hunks() {
        let d = diff_text("a\nb\n", "a\nb\n");
        assert!(d.is_empty());
        assert_eq!((d.added, d.removed), (0, 0));
    }

    #[test]
    fn a_rewritten_paragraph_is_one_hunk() {
        let old = "# Title\n\nAlpha one.\nAlpha two.\nAlpha three.\n\n# Other\n\nUntouched.\n";
        let new = "# Title\n\nBeta one.\nBeta two.\nBeta three.\n\n# Other\n\nUntouched.\n";
        let d = diff_text(old, new);
        assert_eq!(d.hunks.len(), 1, "paragraph grouping should merge the three lines");
        assert_eq!(d.hunks[0].header, "# Title");
    }

    #[test]
    fn word_level_spans_mark_only_the_changed_words() {
        let (old_spans, new_spans) = word_spans(
            "if the table has 4+ rows or 3+ columns",
            "if the table has 4+ rows and 4+ columns",
        );
        assert!(!old_spans.is_empty() && !new_spans.is_empty());
        let changed_old: String = old_spans
            .iter()
            .filter(|s| s.changed)
            .map(|s| &"if the table has 4+ rows or 3+ columns"[s.start..s.end])
            .collect();
        assert!(changed_old.contains("or") || changed_old.contains('3'));
    }

    #[test]
    fn hunk_apply_takes_exactly_the_accepted_hunks() {
        let old = "one\n\ntwo\n\nthree\n";
        let new = "ONE\n\ntwo\n\nTHREE\n";
        let d = diff_text(old, new);
        assert_eq!(d.hunks.len(), 2);

        let none = apply_hunks(old, new, &d, &[]);
        assert_eq!(none, old);

        let all = apply_hunks(old, new, &d, &[0, 1]);
        assert_eq!(all, new);

        let first_only = apply_hunks(old, new, &d, &[0]);
        assert_eq!(first_only, "ONE\n\ntwo\n\nthree\n");

        let second_only = apply_hunks(old, new, &d, &[1]);
        assert_eq!(second_only, "one\n\ntwo\n\nTHREE\n");
    }

    #[test]
    fn pure_insertion_at_the_end_applies() {
        let old = "a\n";
        let new = "a\n\nb\n";
        let d = diff_text(old, new);
        assert_eq!(apply_hunks(old, new, &d, &[0]), new);
    }

    #[test]
    fn crlf_input_keeps_its_line_endings() {
        let old = "a\r\nb\r\n";
        let new = "a\r\nB\r\n";
        let d = diff_text(old, new);
        let applied = apply_hunks(old, new, &d, &[0]);
        assert!(applied.contains("\r\n"));
        assert_eq!(applied.replace("\r\n", "\n"), "a\nB\n");
    }

    #[test]
    fn merge3_keeps_both_sides_when_they_touch_different_paragraphs() {
        let base = "alpha\n\nbeta\n\ngamma\n";
        let ours = "ALPHA\n\nbeta\n\ngamma\n";
        let theirs = "alpha\n\nBETA\n\ngamma\n";
        assert_eq!(merge3(base, ours, theirs).unwrap(), "ALPHA\n\nBETA\n\ngamma\n");
    }

    #[test]
    fn merge3_reports_a_real_conflict_rather_than_guessing() {
        let base = "alpha\n";
        let err = merge3(base, "OURS\n", "THEIRS\n").unwrap_err();
        assert_eq!(err.code(), "conflict");
    }

    #[test]
    fn merge3_accepts_the_same_edit_made_twice() {
        let base = "alpha\n";
        assert_eq!(merge3(base, "SAME\n", "SAME\n").unwrap(), "SAME\n");
    }

    #[test]
    fn unified_output_has_headers_and_a_hunk() {
        let u = unified("a\nb\n", "a\nc\n", "a/x.md", "b/x.md", 3);
        assert!(u.starts_with("--- a/x.md\n+++ b/x.md\n"));
        assert!(u.contains("@@ -"));
        assert!(u.contains("-b"));
        assert!(u.contains("+c"));
    }

    #[test]
    fn hundred_kilobyte_documents_diff_inside_the_budget() {
        let mut old = String::new();
        for i in 0..2000 {
            old.push_str(&format!("Paragraph {i} with a reasonable amount of prose in it.\n\n"));
        }
        let new = old.replace("Paragraph 1000", "Paragraph one thousand");
        assert!(old.len() > 100_000);
        let start = std::time::Instant::now();
        let d = diff_text(&old, &new);
        let elapsed = start.elapsed();
        assert!(!d.is_empty());
        assert!(elapsed.as_millis() < 100, "word-level diff took {elapsed:?}, budget is 100ms");
    }
}
