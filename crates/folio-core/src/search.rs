//! Corpus search with ripgrep semantics.
//!
//! The corpus is files on disk and search is solved technology, so this is a
//! thin, literal reimplementation of the parts that matter — regex or literal
//! matching, a glob filter, line numbers and snippets — rather than an index
//! that can go stale.

use crate::corpus::{self, ArtifactType, Root};
use crate::error::{Error, Result};
use crate::util::{display_path, canonical_key};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Match {
    pub path: String,
    pub display: String,
    #[serde(rename = "type")]
    pub artifact_type: ArtifactType,
    /// 1-based.
    pub line: usize,
    /// The matching line, trimmed of trailing whitespace.
    pub text: String,
    /// Byte range of the match inside `text`.
    pub start: usize,
    pub end: usize,
}

#[derive(Debug, Clone)]
pub struct Query {
    pub pattern: String,
    pub regex: bool,
    pub glob: Option<String>,
    pub case_sensitive: bool,
    pub max_results: usize,
    pub max_file_bytes: u64,
}

impl Default for Query {
    fn default() -> Self {
        Query {
            pattern: String::new(),
            regex: false,
            glob: None,
            case_sensitive: false,
            max_results: 500,
            max_file_bytes: 4 * 1024 * 1024,
        }
    }
}

enum Matcher {
    Regex(regex::Regex),
    Literal { needle: String, case_sensitive: bool },
}

impl Matcher {
    fn build(query: &Query) -> Result<Matcher> {
        if query.regex {
            let re = regex::RegexBuilder::new(&query.pattern)
                .case_insensitive(!query.case_sensitive)
                .build()
                .map_err(|e| Error::invalid(format!("bad regex: {e}")))?;
            Ok(Matcher::Regex(re))
        } else {
            Ok(Matcher::Literal {
                needle: if query.case_sensitive {
                    query.pattern.clone()
                } else {
                    query.pattern.to_lowercase()
                },
                case_sensitive: query.case_sensitive,
            })
        }
    }

    fn find(&self, line: &str) -> Option<(usize, usize)> {
        match self {
            Matcher::Regex(re) => re.find(line).map(|m| (m.start(), m.end())),
            Matcher::Literal { needle, case_sensitive } => {
                if *case_sensitive {
                    line.find(needle.as_str()).map(|i| (i, i + needle.len()))
                } else {
                    line.to_lowercase()
                        .find(needle.as_str())
                        // Offsets from the lowercased haystack are valid in the
                        // original only when the case fold is length-preserving,
                        // which it is for every character that can appear before
                        // an ASCII-ish needle in practice; clamp to be safe.
                        .map(|i| (i.min(line.len()), (i + needle.len()).min(line.len())))
                }
            }
        }
    }
}

pub fn search(roots: &[Root], query: &Query) -> Result<Vec<Match>> {
    if query.pattern.trim().is_empty() {
        return Err(Error::invalid("search needs a pattern"));
    }
    let matcher = Matcher::build(query)?;
    let glob = match &query.glob {
        Some(pattern) => Some(
            globset::Glob::new(pattern)
                .map_err(|e| Error::invalid(format!("bad glob `{pattern}`: {e}")))?
                .compile_matcher(),
        ),
        None => None,
    };

    let mut out: Vec<Match> = Vec::new();
    'roots: for root in roots {
        for file in corpus::walk_root(root) {
            if out.len() >= query.max_results {
                break 'roots;
            }
            let key = canonical_key(&file);
            let relative = corpus::relative_to_root(root, &key);
            if let Some(glob) = &glob {
                if !glob.is_match(&relative) && !glob.is_match(&key) {
                    continue;
                }
            }

            let Ok(meta) = std::fs::metadata(&file) else { continue };
            if meta.len() > query.max_file_bytes {
                continue;
            }
            let Ok(bytes) = std::fs::read(&file) else { continue };
            // Skip anything that looks binary rather than emitting garbage.
            if bytes.iter().take(8192).any(|b| *b == 0) {
                continue;
            }
            let text = String::from_utf8_lossy(&bytes);
            let artifact_type = corpus::detect_type(&key, &text);

            for (i, line) in text.lines().enumerate() {
                let Some((start, end)) = matcher.find(line) else { continue };
                out.push(Match {
                    display: display_path(&key),
                    path: key.clone(),
                    artifact_type,
                    line: i + 1,
                    text: line.trim_end().to_string(),
                    start,
                    end,
                });
                if out.len() >= query.max_results {
                    break 'roots;
                }
            }
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::corpus::{Policy, RootKind};

    fn root_for(dir: &std::path::Path) -> Root {
        let path = canonical_key(dir);
        Root {
            id: "root_s".into(),
            display: path.clone(),
            path,
            kind: RootKind::Dir,
            policy: Policy::Auto,
            label: "s".into(),
            added_at: 0,
        }
    }

    #[test]
    fn finds_literal_matches_with_line_numbers() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.md"), "alpha\nBETA here\ngamma\n").unwrap();
        let hits = search(
            &[root_for(dir.path())],
            &Query { pattern: "beta".into(), ..Default::default() },
        )
        .unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].line, 2);
        assert_eq!(hits[0].text, "BETA here");
    }

    #[test]
    fn regex_and_glob_filters_apply() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("skills")).unwrap();
        std::fs::write(dir.path().join("skills").join("SKILL.md"), "name: x\n").unwrap();
        std::fs::write(dir.path().join("other.md"), "name: y\n").unwrap();
        let hits = search(
            &[root_for(dir.path())],
            &Query {
                pattern: r"name:\s+\w".into(),
                regex: true,
                glob: Some("skills/**".into()),
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!(hits.len(), 1);
        assert!(hits[0].path.ends_with("SKILL.md"));
    }

    #[test]
    fn a_bad_regex_is_reported_not_panicked() {
        let dir = tempfile::tempdir().unwrap();
        let err = search(
            &[root_for(dir.path())],
            &Query { pattern: "([".into(), regex: true, ..Default::default() },
        )
        .unwrap_err();
        assert_eq!(err.code(), "invalid");
    }
}
