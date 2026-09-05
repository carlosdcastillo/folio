//! Unified-diff parsing and application.
//!
//! `propose_edit` accepts either whole `content` or a `patch`. A patch is
//! applied to the *base* the agent read, never blindly to whatever is on disk
//! now — that is what makes conflicts detectable at review time instead of
//! silently mis-applying.

use crate::error::{Error, Result};
use crate::util::{dominant_newline, join_lines, split_lines};

/// How far from its declared position a hunk may be found. Generous enough to
/// absorb an unrelated edit earlier in the file, tight enough that a hunk
/// never matches a coincidentally similar passage on the other side of a
/// long document.
const SEARCH_WINDOW: usize = 400;

#[derive(Debug, Clone)]
pub struct PatchHunk {
    /// 0-based line where the hunk claims to start in the old file.
    pub old_start: usize,
    /// Context + deleted lines: what must be present to apply.
    pub old_lines: Vec<String>,
    /// Context + inserted lines: what replaces them.
    pub new_lines: Vec<String>,
    /// The hunk ended with `\ No newline at end of file` after a line that is
    /// part of the new side. Every other line in a unified diff carries an
    /// implied newline, so this flag is the only way to produce a file that
    /// does not end with one.
    pub no_newline_new: bool,
}

/// Parse a unified diff. File headers are tolerated and ignored — Folio
/// patches always target one known path, which the caller already supplied.
pub fn parse(patch: &str) -> Result<Vec<PatchHunk>> {
    let mut hunks: Vec<PatchHunk> = Vec::new();
    let mut current: Option<PatchHunk> = None;
    // Which side the previous body line belonged to, so `\ No newline` can be
    // attributed to it.
    let mut previous_side: Option<char> = None;

    for raw in patch.replace("\r\n", "\n").lines() {
        if raw.starts_with("@@") {
            if let Some(h) = current.take() {
                hunks.push(h);
            }
            let old_start = parse_hunk_header(raw)?;
            current = Some(PatchHunk {
                old_start,
                old_lines: Vec::new(),
                new_lines: Vec::new(),
                no_newline_new: false,
            });
            previous_side = None;
            continue;
        }
        let Some(h) = current.as_mut() else {
            // Everything before the first `@@` is header noise.
            continue;
        };
        if raw.starts_with(r"\ No newline") {
            if matches!(previous_side, Some('+') | Some(' ')) {
                h.no_newline_new = true;
            }
            continue;
        }
        previous_side = raw.chars().next().or(Some(' '));
        match raw.chars().next() {
            Some(' ') => {
                h.old_lines.push(raw[1..].to_string());
                h.new_lines.push(raw[1..].to_string());
            }
            Some('-') => h.old_lines.push(raw[1..].to_string()),
            Some('+') => h.new_lines.push(raw[1..].to_string()),
            // A bare empty line inside a hunk is a context line whose trailing
            // space was stripped in transit. Agents produce these constantly.
            None => {
                h.old_lines.push(String::new());
                h.new_lines.push(String::new());
            }
            Some(_) => {
                // A new file header ends the current hunk.
                if raw.starts_with("--- ") || raw.starts_with("+++ ") || raw.starts_with("diff ") {
                    hunks.push(current.take().unwrap());
                }
            }
        }
    }
    if let Some(h) = current.take() {
        hunks.push(h);
    }

    if hunks.is_empty() {
        return Err(Error::invalid("patch contains no hunks"));
    }
    Ok(hunks)
}

fn parse_hunk_header(line: &str) -> Result<usize> {
    // `@@ -12,7 +12,9 @@ optional section heading`
    let body = line.trim_start_matches('@').trim();
    let minus = body
        .split_whitespace()
        .find(|t| t.starts_with('-'))
        .ok_or_else(|| Error::invalid(format!("malformed hunk header: {line}")))?;
    let numeric = &minus[1..];
    let start: usize = numeric
        .split(',')
        .next()
        .unwrap_or("1")
        .parse()
        .map_err(|_| Error::invalid(format!("malformed hunk header: {line}")))?;
    Ok(start.saturating_sub(1))
}

/// Apply a parsed patch to `base`.
pub fn apply(base: &str, patch: &str) -> Result<String> {
    let hunks = parse(patch)?;
    apply_hunks(base, &hunks)
}

pub fn apply_hunks(base: &str, hunks: &[PatchHunk]) -> Result<String> {
    let (lines, trailing) = split_lines(base);
    let newline = dominant_newline(base);

    let mut out: Vec<String> = Vec::new();
    let mut cursor = 0usize;

    for (i, hunk) in hunks.iter().enumerate() {
        let pos = locate(&lines, &hunk.old_lines, hunk.old_start, cursor).ok_or_else(|| {
            Error::Conflict(format!(
                "hunk {} of the patch does not apply: the context around line {} is not in the base",
                i + 1,
                hunk.old_start + 1
            ))
        })?;
        out.extend_from_slice(&lines[cursor..pos]);
        out.extend(hunk.new_lines.iter().cloned());
        cursor = pos + hunk.old_lines.len();
    }

    // Whoever supplied the last line decides whether the file ends with a
    // newline: the patch if it reached the end, the base if its tail survived.
    let ends_with_patch_content = cursor >= lines.len();
    out.extend_from_slice(&lines[cursor.min(lines.len())..]);
    let final_newline = if ends_with_patch_content {
        !hunks.last().is_some_and(|h| h.no_newline_new)
    } else {
        trailing
    };

    Ok(join_lines(&out, final_newline && !out.is_empty(), newline))
}

/// Find where `want` sits in `lines`, preferring the declared position and
/// searching outward from it. Never matches before `min_pos`, so hunks stay
/// in order and cannot overlap.
fn locate(lines: &[String], want: &[String], hint: usize, min_pos: usize) -> Option<usize> {
    if want.is_empty() {
        return Some(hint.max(min_pos).min(lines.len()));
    }
    if want.len() > lines.len() {
        return None;
    }
    let last_valid = lines.len() - want.len();
    let matches_at = |p: usize| lines[p..p + want.len()] == *want;

    let start = hint.max(min_pos).min(last_valid);
    if start >= min_pos && matches_at(start) {
        return Some(start);
    }
    for delta in 1..=SEARCH_WINDOW {
        if let Some(p) = start.checked_sub(delta) {
            if p >= min_pos && matches_at(p) {
                return Some(p);
            }
        }
        let p = start + delta;
        if p <= last_valid && matches_at(p) {
            return Some(p);
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn applies_a_simple_patch() {
        let base = "alpha\nbeta\ngamma\n";
        let patch = "--- a/x.md\n+++ b/x.md\n@@ -1,3 +1,3 @@\n alpha\n-beta\n+BETA\n gamma\n";
        assert_eq!(apply(base, patch).unwrap(), "alpha\nBETA\ngamma\n");
    }

    #[test]
    fn applies_with_an_offset_when_lines_shifted() {
        let base = "new header\n\nalpha\nbeta\ngamma\n";
        let patch = "@@ -1,3 +1,3 @@\n alpha\n-beta\n+BETA\n gamma\n";
        assert_eq!(apply(base, patch).unwrap(), "new header\n\nalpha\nBETA\ngamma\n");
    }

    #[test]
    fn a_patch_whose_context_is_gone_is_a_conflict() {
        let base = "totally\ndifferent\n";
        let patch = "@@ -1,3 +1,3 @@\n alpha\n-beta\n+BETA\n gamma\n";
        let err = apply(base, patch).unwrap_err();
        assert_eq!(err.code(), "conflict");
    }

    #[test]
    fn multiple_hunks_apply_in_order() {
        let base = "a\nb\nc\nd\ne\nf\ng\n";
        let patch = "@@ -1,2 +1,2 @@\n a\n-b\n+B\n@@ -6,2 +6,2 @@\n f\n-g\n+G\n";
        assert_eq!(apply(base, patch).unwrap(), "a\nB\nc\nd\ne\nf\nG\n");
    }

    #[test]
    fn creates_content_in_an_empty_base() {
        let patch = "@@ -0,0 +1,2 @@\n+first\n+second\n";
        assert_eq!(apply("", patch).unwrap(), "first\nsecond\n");
    }

    #[test]
    fn round_trips_with_the_unified_writer() {
        let old = "one\ntwo\nthree\nfour\nfive\n";
        let new = "one\nTWO\nthree\nfour\nFIVE\n";
        let patch = super::super::unified(old, new, "a/x.md", "b/x.md", 3);
        assert_eq!(apply(old, &patch).unwrap(), new);
    }

    #[test]
    fn a_patch_with_no_hunks_is_rejected() {
        assert_eq!(parse("just some prose").unwrap_err().code(), "invalid");
    }
}
