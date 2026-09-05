//! Skill validation: frontmatter, progressive-disclosure structure, reference
//! integrity, the command inventory cross-check, and the self-referential
//! linter — a skill may declare its own forbidden patterns and Folio enforces
//! them across the skill's own files.

use super::frontmatter;
use super::Finding;
use crate::util::to_fs_path;
use serde_json::Value;

/// The subdirectories that make a skill tree a skill tree.
pub const DISCLOSURE_DIRS: &[&str] = &["commands", "templates", "references", "scripts", "assets"];

/// Frontmatter descriptions are a budget, not a place for the manual.
const MAX_DESCRIPTION: usize = 1024;
/// The overview exists to route the reader deeper, not to hold everything.
const MAX_OVERVIEW_WORDS: usize = 350;

pub fn validate(path: &str, content: &str) -> Vec<Finding> {
    let mut out = Vec::new();
    let fm = frontmatter::parse(content);
    let dir = path.rfind('/').map(|i| &path[..i]).unwrap_or("");

    validate_frontmatter(path, dir, &fm, &mut out);
    validate_structure(content, &fm, &mut out);
    validate_references(path, dir, content, &mut out);
    self_lint(path, dir, content, &fm, &mut out);

    out
}

fn validate_frontmatter(path: &str, dir: &str, fm: &frontmatter::Frontmatter, out: &mut Vec<Finding>) {
    if !fm.present {
        out.push(
            Finding::error("skill.frontmatter.missing", "A skill needs YAML frontmatter with `name` and `description`.")
                .at(1),
        );
        return;
    }

    match fm.get_str("name") {
        None | Some("") => out.push(
            Finding::error("skill.frontmatter.name.missing", "Frontmatter is missing `name`.").at(1),
        ),
        Some(name) => {
            let dir_name = dir.rsplit('/').next().unwrap_or("");
            // A skill whose name disagrees with its directory is the one that
            // gets invoked by the wrong id and quietly never runs.
            if !dir_name.is_empty() && !dir_name.eq_ignore_ascii_case(name) && path.to_lowercase().ends_with("/skill.md") {
                out.push(
                    Finding::error(
                        "skill.frontmatter.name.mismatch",
                        format!("`name: {name}` does not match the directory name `{dir_name}`."),
                    )
                    .at(1)
                    .with_hint(format!("Rename the directory to `{name}`, or set `name: {dir_name}`.")),
                );
            }
            if name.contains(char::is_whitespace) {
                out.push(
                    Finding::warning(
                        "skill.frontmatter.name.spaces",
                        format!("`name: {name}` contains whitespace; skill names are invoked as identifiers."),
                    )
                    .at(1),
                );
            }
        }
    }

    match fm.get_str("description") {
        None | Some("") => out.push(
            Finding::error("skill.frontmatter.description.missing", "Frontmatter is missing `description`.").at(1),
        ),
        Some(d) if d.chars().count() > MAX_DESCRIPTION => out.push(
            Finding::error(
                "skill.frontmatter.description.long",
                format!(
                    "`description` is {} characters; the limit is {MAX_DESCRIPTION}.",
                    d.chars().count()
                ),
            )
            .at(1),
        ),
        Some(_) => {}
    }
}

fn validate_structure(content: &str, fm: &frontmatter::Frontmatter, out: &mut Vec<Finding>) {
    let body = &content[fm.body_offset.min(content.len())..];
    let mut sections: Vec<(usize, String)> = Vec::new();
    for (i, line) in body.lines().enumerate() {
        let t = line.trim_start();
        if t.starts_with("##") {
            sections.push((fm.body_line + i, t.trim_start_matches('#').trim().to_lowercase()));
        }
    }

    // Overview brevity: everything before the first `##`.
    let overview_end = body
        .lines()
        .position(|l| l.trim_start().starts_with("##"))
        .unwrap_or_else(|| body.lines().count());
    let overview_words: usize = body
        .lines()
        .take(overview_end)
        .map(|l| l.split_whitespace().count())
        .sum();
    if overview_words > MAX_OVERVIEW_WORDS {
        out.push(
            Finding::warning(
                "skill.overview.long",
                format!(
                    "The overview is {overview_words} words before the first section. Progressive disclosure wants a short overview that routes the reader deeper."
                ),
            )
            .at(fm.body_line),
        );
    }

    if sections.is_empty() {
        out.push(
            Finding::warning(
                "skill.structure.no_sections",
                "The skill has no `##` sections; there is nothing to disclose progressively.",
            )
            .at(fm.body_line),
        );
        return;
    }

    let has = |needles: &[&str]| sections.iter().any(|(_, t)| needles.iter().any(|n| t.contains(n)));
    if !has(&["workflow", "steps", "process", "how to", "usage"]) {
        out.push(Finding::info(
            "skill.structure.no_workflow",
            "No workflow section found (looked for `workflow`, `steps`, `process`, `usage`).",
        ));
    }
    if !has(&["quality", "check", "criteria", "review"]) {
        out.push(Finding::info(
            "skill.structure.no_quality_checks",
            "No quality-checks section found (looked for `quality`, `checks`, `criteria`).",
        ));
    }
}

// ---------------------------------------------------------------------------
// Reference integrity
// ---------------------------------------------------------------------------

/// A relative link found in the document, with the line it sits on.
#[derive(Debug, Clone)]
pub struct Reference {
    pub target: String,
    pub line: usize,
}

/// Markdown links plus bare backticked paths — skills reference their files
/// both ways, and a broken reference is broken either way.
pub fn collect_references(content: &str) -> Vec<Reference> {
    let mut out: Vec<Reference> = Vec::new();
    for (i, line) in content.lines().enumerate() {
        let line_no = i + 1;
        // `[text](target)`
        let bytes = line.as_bytes();
        let mut j = 0usize;
        while j < bytes.len() {
            if bytes[j] == b']' && j + 1 < bytes.len() && bytes[j + 1] == b'(' {
                if let Some(close) = line[j + 2..].find(')') {
                    let target = line[j + 2..j + 2 + close].trim();
                    let target = target.split_whitespace().next().unwrap_or(target);
                    push_reference(&mut out, target, line_no);
                    j = j + 2 + close + 1;
                    continue;
                }
            }
            j += 1;
        }
        // Backticked relative paths, e.g. `references/palette.md`.
        for piece in line.split('`').skip(1).step_by(2) {
            let candidate = piece.trim();
            if candidate.contains('/') && looks_like_a_tree_path(candidate) {
                push_reference(&mut out, candidate, line_no);
            }
        }
    }
    out
}

fn looks_like_a_tree_path(candidate: &str) -> bool {
    let head = candidate.trim_start_matches("./");
    DISCLOSURE_DIRS
        .iter()
        .any(|d| head.starts_with(&format!("{d}/")))
        && head.contains('.')
        && !head.contains(' ')
}

fn push_reference(out: &mut Vec<Reference>, target: &str, line: usize) {
    let t = target.trim();
    if t.is_empty() || t.starts_with('#') {
        return;
    }
    let lower = t.to_lowercase();
    for scheme in ["http://", "https://", "mailto:", "data:", "ftp://", "file://", "tel:"] {
        if lower.starts_with(scheme) {
            return;
        }
    }
    if t.starts_with('/') || t.contains(':') {
        return;
    }
    // Strip an anchor: `references/palette.md#swatches`.
    let clean = t.split('#').next().unwrap_or(t).to_string();
    if clean.is_empty() {
        return;
    }
    if !out.iter().any(|r| r.target == clean && r.line == line) {
        out.push(Reference { target: clean, line });
    }
}

fn validate_references(path: &str, dir: &str, content: &str, out: &mut Vec<Finding>) {
    let base = to_fs_path(dir);
    let mut referenced: Vec<String> = Vec::new();

    for reference in collect_references(content) {
        let relative = reference.target.trim_start_matches("./");
        let target = base.join(to_fs_path(relative));
        if target.exists() {
            referenced.push(normalise_rel(relative));
        } else {
            out.push(
                Finding::error(
                    "skill.reference.missing",
                    format!("`{}` does not resolve.", reference.target),
                )
                .at(reference.line)
                .with_hint(format!("Expected {}", target.display())),
            );
        }
    }

    // Orphans: files that live in the disclosure directories but nothing
    // points at them. A skill's files should all be reachable from SKILL.md.
    let self_name = path.rsplit('/').next().unwrap_or("").to_lowercase();
    for sub in DISCLOSURE_DIRS {
        let sub_dir = base.join(sub);
        if !sub_dir.is_dir() {
            continue;
        }
        let mut entries: Vec<String> = walkdir::WalkDir::new(&sub_dir)
            .into_iter()
            .filter_map(|e| e.ok())
            .filter(|e| e.file_type().is_file())
            .filter_map(|e| {
                e.path()
                    .strip_prefix(&base)
                    .ok()
                    .map(|p| normalise_rel(&p.to_string_lossy()))
            })
            .collect();
        entries.sort();
        for entry in entries {
            if entry.to_lowercase() == self_name {
                continue;
            }
            if !referenced.iter().any(|r| r.eq_ignore_ascii_case(&entry)) {
                out.push(
                    Finding::warning(
                        "skill.reference.orphan",
                        format!("`{entry}` is not referenced from this skill."),
                    )
                    .in_file(entry.clone())
                    .with_hint("Link it, or delete it."),
                );
            }
        }
    }

    validate_command_inventory(&base, content, out);
}

fn normalise_rel(p: &str) -> String {
    p.replace('\\', "/").trim_start_matches("./").to_string()
}

/// Cross-check the command inventory table against the contents of `commands/`.
/// A table that has drifted from the directory is the single most common way a
/// skill lies to its reader.
fn validate_command_inventory(base: &std::path::Path, content: &str, out: &mut Vec<Finding>) {
    let commands_dir = base.join("commands");
    if !commands_dir.is_dir() {
        return;
    }

    let mut on_disk: Vec<String> = std::fs::read_dir(&commands_dir)
        .map(|entries| {
            entries
                .filter_map(|e| e.ok())
                .filter(|e| e.path().is_file())
                .filter_map(|e| e.path().file_stem().map(|s| s.to_string_lossy().to_string()))
                .collect()
        })
        .unwrap_or_default();
    on_disk.sort();

    let Some((table_line, listed)) = find_command_table(content) else {
        if !on_disk.is_empty() {
            out.push(Finding::info(
                "skill.commands.no_table",
                format!(
                    "`commands/` holds {} file(s) but the skill has no command inventory table.",
                    on_disk.len()
                ),
            ));
        }
        return;
    };

    for (name, line) in &listed {
        if !on_disk.iter().any(|d| d.eq_ignore_ascii_case(name)) {
            out.push(
                Finding::error(
                    "skill.commands.table_ghost",
                    format!("The inventory table lists `{name}`, but `commands/` has no such file."),
                )
                .at(*line),
            );
        }
    }
    for name in &on_disk {
        if !listed.iter().any(|(n, _)| n.eq_ignore_ascii_case(name)) {
            out.push(
                Finding::warning(
                    "skill.commands.table_missing",
                    format!("`commands/{name}` exists but is not in the inventory table."),
                )
                .at(table_line),
            );
        }
    }
}

/// Find a markdown table whose header mentions "command" and return its
/// first-column entries.
fn find_command_table(content: &str) -> Option<(usize, Vec<(String, usize)>)> {
    let lines: Vec<&str> = content.lines().collect();
    for (i, line) in lines.iter().enumerate() {
        let t = line.trim();
        if !t.starts_with('|') || !t.to_lowercase().contains("command") {
            continue;
        }
        let separator = lines.get(i + 1).map(|l| l.trim()).unwrap_or("");
        if !separator.starts_with('|') || !separator.contains("---") {
            continue;
        }
        let mut entries = Vec::new();
        for (j, row) in lines.iter().enumerate().skip(i + 2) {
            let r = row.trim();
            if !r.starts_with('|') {
                break;
            }
            let first = r
                .trim_matches('|')
                .split('|')
                .next()
                .unwrap_or("")
                .trim()
                .trim_matches('`')
                .trim()
                .trim_start_matches('/')
                .to_string();
            let first = first.split_whitespace().next().unwrap_or("").to_string();
            let first = first.trim_end_matches(".md").to_string();
            if !first.is_empty() {
                entries.push((first, j + 1));
            }
        }
        return Some((i + 1, entries));
    }
    None
}

// ---------------------------------------------------------------------------
// The self-referential linter
// ---------------------------------------------------------------------------

/// A skill may declare its own forbidden patterns:
///
/// ```yaml
/// lint:
///   forbidden: ["#8b5cf6", "font-family: Inter"]
/// ```
///
/// Folio enforces them across the skill's own files. A skill's rules apply to
/// itself.
fn self_lint(
    path: &str,
    dir: &str,
    content: &str,
    fm: &frontmatter::Frontmatter,
    out: &mut Vec<Finding>,
) {
    let Some(lint) = fm.get("lint") else { return };
    let forbidden: Vec<String> = match lint {
        Value::Object(map) => map
            .get("forbidden")
            .and_then(|v| v.as_array())
            .map(|a| a.iter().filter_map(|v| v.as_str().map(str::to_string)).collect())
            .unwrap_or_default(),
        Value::Array(a) => a.iter().filter_map(|v| v.as_str().map(str::to_string)).collect(),
        _ => Vec::new(),
    };
    if forbidden.is_empty() {
        return;
    }

    let case_sensitive = lint
        .get("case_sensitive")
        .and_then(Value::as_bool)
        .unwrap_or(false);

    let base = to_fs_path(dir);
    let files: Vec<std::path::PathBuf> = walkdir::WalkDir::new(&base)
        .max_depth(6)
        .into_iter()
        .filter_map(|e| e.ok())
        .filter(|e| e.file_type().is_file())
        .map(|e| e.into_path())
        .collect();

    // The rules live in this file's own frontmatter, and a rule that forbids a
    // string necessarily contains that string. Declaring a pattern is not
    // violating it, so the declaration block is exempt — from its own rules
    // only; another file's frontmatter is scanned like anything else.
    let declaring_file = crate::util::to_fs_path(path);
    let declaration_ends_at = if fm.present { fm.body_line.saturating_sub(1) } else { 0 };

    // A skill being written for the first time has no SKILL.md on disk yet, so
    // the walk would not reach it. Lint the buffer regardless.
    let mut targets: Vec<std::path::PathBuf> = files;
    if !targets.iter().any(|f| *f == declaring_file) {
        targets.push(declaring_file.clone());
    }

    for file in targets {
        let is_declaring_file = file == declaring_file;

        // The declaring file is linted from the text being validated, not from
        // disk: the editor should mark a forbidden pattern as you type it, not
        // only after you save.
        let text: String = if is_declaring_file {
            content.to_string()
        } else {
            let Ok(bytes) = std::fs::read(&file) else { continue };
            if bytes.len() > 2 * 1024 * 1024 || bytes.contains(&0) {
                continue;
            }
            String::from_utf8_lossy(&bytes).into_owned()
        };

        let relative = file
            .strip_prefix(&base)
            .map(|p| normalise_rel(&p.to_string_lossy()))
            .unwrap_or_else(|_| file.to_string_lossy().to_string());
        for (i, line) in text.lines().enumerate() {
            if is_declaring_file && i + 1 <= declaration_ends_at {
                continue;
            }
            for pattern in &forbidden {
                let hit = if case_sensitive {
                    line.contains(pattern.as_str())
                } else {
                    line.to_lowercase().contains(&pattern.to_lowercase())
                };
                if hit {
                    out.push(
                        Finding::error(
                            "skill.lint.forbidden",
                            format!("`{pattern}` is forbidden by this skill's own lint rules."),
                        )
                        .at(i + 1)
                        .in_file(relative.clone()),
                    );
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn skill_tree() -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        let skill = dir.path().join("visual-explainer");
        fs::create_dir_all(skill.join("commands")).unwrap();
        fs::create_dir_all(skill.join("references")).unwrap();
        fs::write(skill.join("commands").join("render.md"), "# render\n").unwrap();
        fs::write(skill.join("references").join("palette.md"), "# palette\n").unwrap();
        dir
    }

    fn skill_path(dir: &tempfile::TempDir) -> String {
        crate::util::canonical_key(&dir.path().join("visual-explainer").join("SKILL.md"))
    }

    #[test]
    fn a_clean_skill_has_no_errors() {
        let dir = skill_tree();
        let content = "---\nname: visual-explainer\ndescription: Draws things clearly.\n---\n\
            Short overview.\n\n## Workflow\n\nSee [palette](references/palette.md).\n\n\
            ## Commands\n\n| Command | Purpose |\n|---|---|\n| `render` | Render it |\n\n\
            ## Quality checks\n\nLook at it.\n";
        let findings = validate(&skill_path(&dir), content);
        let errors: Vec<_> = findings.iter().filter(|f| f.severity == super::super::Severity::Error).collect();
        assert!(errors.is_empty(), "unexpected errors: {errors:#?}");
    }

    #[test]
    fn a_broken_reference_is_an_error() {
        let dir = skill_tree();
        let content = "---\nname: visual-explainer\ndescription: Draws.\n---\n\
            ## Workflow\n\nSee [gone](references/gone.md).\n";
        let findings = validate(&skill_path(&dir), content);
        assert!(findings.iter().any(|f| f.rule == "skill.reference.missing" && f.line == Some(7)));
    }

    #[test]
    fn an_unreferenced_file_is_reported_as_an_orphan() {
        let dir = skill_tree();
        let content = "---\nname: visual-explainer\ndescription: Draws.\n---\n## Workflow\n\nNothing linked.\n";
        let findings = validate(&skill_path(&dir), content);
        assert!(findings
            .iter()
            .any(|f| f.rule == "skill.reference.orphan" && f.file.as_deref() == Some("references/palette.md")));
    }

    #[test]
    fn the_command_table_is_cross_checked_both_ways() {
        let dir = skill_tree();
        let content = "---\nname: visual-explainer\ndescription: Draws.\n---\n\
            ## Commands\n\n| Command | Purpose |\n|---|---|\n| `explain` | Explain |\n\n\
            [palette](references/palette.md)\n";
        let findings = validate(&skill_path(&dir), content);
        assert!(findings.iter().any(|f| f.rule == "skill.commands.table_ghost"), "listed-but-absent");
        assert!(findings.iter().any(|f| f.rule == "skill.commands.table_missing"), "present-but-unlisted");
    }

    #[test]
    fn name_must_match_the_directory() {
        let dir = skill_tree();
        let content = "---\nname: something-else\ndescription: Draws.\n---\n## Workflow\n";
        let findings = validate(&skill_path(&dir), content);
        assert!(findings.iter().any(|f| f.rule == "skill.frontmatter.name.mismatch"));
    }

    #[test]
    fn a_skill_lints_itself() {
        let dir = skill_tree();
        let skill = dir.path().join("visual-explainer");
        fs::write(
            skill.join("references").join("palette.md"),
            "Use #8b5cf6 for accents.\n",
        )
        .unwrap();
        let content = "---\nname: visual-explainer\ndescription: Draws.\nlint:\n  forbidden: [\"#8b5cf6\"]\n---\n\
            ## Workflow\n\n[palette](references/palette.md)\n\n[render](commands/render.md)\n";
        let findings = validate(&skill_path(&dir), content);
        let hit = findings
            .iter()
            .find(|f| f.rule == "skill.lint.forbidden")
            .expect("the skill's own rule must apply to its own files");
        assert_eq!(hit.file.as_deref(), Some("references/palette.md"));
    }

    #[test]
    fn declaring_a_forbidden_pattern_is_not_violating_it() {
        let dir = skill_tree();
        let content = "---\nname: visual-explainer\ndescription: Draws.\nlint:\n  forbidden: [\"font-family: Inter\"]\n---\n\
            ## Workflow\n\n[palette](references/palette.md)\n\n[render](commands/render.md)\n";
        let findings = validate(&skill_path(&dir), content);
        assert!(
            !findings.iter().any(|f| f.rule == "skill.lint.forbidden"),
            "the rule's own declaration must not trip it: {findings:#?}"
        );

        // But the same string in the body still does.
        let violating = content.replace("## Workflow\n", "## Workflow\n\nUse font-family: Inter here.\n");
        let findings = validate(&skill_path(&dir), &violating);
        let hit = findings
            .iter()
            .find(|f| f.rule == "skill.lint.forbidden")
            .expect("a real violation is still caught");
        assert_eq!(hit.line, Some(9));
    }

    #[test]
    fn an_over_long_description_is_an_error() {
        let dir = skill_tree();
        let long = "x".repeat(1100);
        let content = format!("---\nname: visual-explainer\ndescription: {long}\n---\n## Workflow\n");
        let findings = validate(&skill_path(&dir), &content);
        assert!(findings.iter().any(|f| f.rule == "skill.frontmatter.description.long"));
    }
}
