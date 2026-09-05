//! Artifact intelligence: Folio knows what a skill, a prompt, and a task list
//! *are*, not just that they are markdown.

pub mod frontmatter;
pub mod prompt;
pub mod skill;
pub mod task;

use crate::corpus::ArtifactType;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Severity {
    /// The artifact is broken: a missing reference, an undeclared slot.
    Error,
    /// Works, but violates the shape the type expects.
    Warning,
    /// Worth knowing.
    Info,
}

impl Severity {
    pub fn as_str(self) -> &'static str {
        match self {
            Severity::Error => "error",
            Severity::Warning => "warning",
            Severity::Info => "info",
        }
    }
}

/// One validation result. `line` is 1-based so it can go straight into a
/// gutter marker.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Finding {
    pub severity: Severity,
    /// Stable rule id, e.g. `skill.reference.missing`.
    pub rule: String,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub line: Option<usize>,
    /// Set when the finding is about a file other than the one validated —
    /// reference integrity reaches across a skill tree.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub file: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hint: Option<String>,
}

impl Finding {
    pub fn error(rule: &str, message: impl Into<String>) -> Finding {
        Finding { severity: Severity::Error, rule: rule.into(), message: message.into(), line: None, file: None, hint: None }
    }
    pub fn warning(rule: &str, message: impl Into<String>) -> Finding {
        Finding { severity: Severity::Warning, rule: rule.into(), message: message.into(), line: None, file: None, hint: None }
    }
    pub fn info(rule: &str, message: impl Into<String>) -> Finding {
        Finding { severity: Severity::Info, rule: rule.into(), message: message.into(), line: None, file: None, hint: None }
    }
    pub fn at(mut self, line: usize) -> Finding {
        self.line = Some(line);
        self
    }
    pub fn in_file(mut self, file: impl Into<String>) -> Finding {
        self.file = Some(file.into());
        self
    }
    pub fn with_hint(mut self, hint: impl Into<String>) -> Finding {
        self.hint = Some(hint.into());
        self
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Validation {
    pub path: String,
    pub display: String,
    #[serde(rename = "type")]
    pub artifact_type: ArtifactType,
    pub findings: Vec<Finding>,
    pub errors: usize,
    pub warnings: usize,
    /// True when nothing at error severity was found.
    pub ok: bool,
}

impl Validation {
    pub fn new(path: &str, artifact_type: ArtifactType, mut findings: Vec<Finding>) -> Validation {
        // Errors first, then by position, so the findings strip reads
        // worst-first without the UI having to sort.
        findings.sort_by(|a, b| a.severity.cmp(&b.severity).then(a.line.cmp(&b.line)));
        let errors = findings.iter().filter(|f| f.severity == Severity::Error).count();
        let warnings = findings.iter().filter(|f| f.severity == Severity::Warning).count();
        Validation {
            display: crate::util::display_path(path),
            path: path.to_string(),
            artifact_type,
            errors,
            warnings,
            ok: errors == 0,
            findings,
        }
    }
}

/// Type-aware validation. Every artifact type gets the checks that make sense
/// for it and none that do not.
pub fn validate(path: &str, content: &str) -> Validation {
    let artifact_type = crate::corpus::detect_type(path, content);
    let mut findings = common_findings(content);
    match artifact_type {
        ArtifactType::Skill => findings.extend(skill::validate(path, content)),
        ArtifactType::Prompt => findings.extend(prompt::validate(content)),
        ArtifactType::TaskList => findings.extend(task::validate(content)),
        ArtifactType::Doc => {}
        ArtifactType::Asset => {
            findings.clear();
            findings.push(Finding::info(
                "asset.tracked",
                "Tracked and restorable, but not diffed or edited.",
            ));
        }
    }
    Validation::new(path, artifact_type, findings)
}

/// Checks that apply to any markdown document.
fn common_findings(content: &str) -> Vec<Finding> {
    let mut out = Vec::new();
    let fm = frontmatter::parse(content);
    if let Some(err) = &fm.parse_error {
        out.push(
            Finding::error("frontmatter.invalid", format!("Frontmatter is not valid YAML: {err}")).at(1),
        );
    }
    let mut fence: Option<(String, usize)> = None;
    for (i, line) in content.lines().enumerate() {
        let trimmed = line.trim_start();
        let is_fence = trimmed.starts_with("```") || trimmed.starts_with("~~~");
        if is_fence {
            let marker: String = trimmed.chars().take_while(|c| *c == '`' || *c == '~').collect();
            match &fence {
                None => fence = Some((marker, i + 1)),
                Some((open, _)) if trimmed.starts_with(open.as_str()) => fence = None,
                Some(_) => {}
            }
        }
    }
    if let Some((_, line)) = fence {
        out.push(
            Finding::warning("markdown.unclosed_fence", "A fenced code block is never closed.").at(line),
        );
    }
    out
}
