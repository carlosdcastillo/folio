//! Prompt templates: `{{slot}}` bodies with declared variables.
//!
//! Rendering is in scope; execution is not. Folio never calls a model — that
//! boundary is what keeps it a markdown workshop instead of a chat client.

use super::frontmatter;
use super::Finding;
use crate::corpus::slot_names;
use crate::error::{Error, Result};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VariableSpec {
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub default: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    pub required: bool,
}

/// Variables as declared in frontmatter, in declaration order.
pub fn declared_variables(content: &str) -> Vec<VariableSpec> {
    let fm = frontmatter::parse(content);
    let Some(raw) = fm.get("variables") else {
        return Vec::new();
    };
    let mut out = Vec::new();
    match raw {
        Value::Array(items) => {
            for item in items {
                match item {
                    Value::String(name) => out.push(VariableSpec {
                        name: name.clone(),
                        default: None,
                        description: None,
                        required: true,
                    }),
                    Value::Object(map) => {
                        if let Some(name) = map.get("name").and_then(Value::as_str) {
                            out.push(spec_from_map(name, map));
                        }
                    }
                    _ => {}
                }
            }
        }
        // `variables: {topic: {default: x}}` — a mapping form agents also emit.
        Value::Object(map) => {
            for (name, value) in map {
                match value {
                    Value::Object(inner) => out.push(spec_from_map(name, inner)),
                    Value::String(default) => out.push(VariableSpec {
                        name: name.clone(),
                        default: Some(default.clone()),
                        description: None,
                        required: false,
                    }),
                    _ => out.push(VariableSpec {
                        name: name.clone(),
                        default: None,
                        description: None,
                        required: true,
                    }),
                }
            }
        }
        Value::String(name) => out.push(VariableSpec {
            name: name.clone(),
            default: None,
            description: None,
            required: true,
        }),
        _ => {}
    }
    out
}

fn spec_from_map(name: &str, map: &Map<String, Value>) -> VariableSpec {
    let default = map.get("default").map(value_to_string);
    VariableSpec {
        name: name.to_string(),
        required: map
            .get("required")
            .and_then(Value::as_bool)
            .unwrap_or(default.is_none()),
        default,
        description: map.get("description").and_then(Value::as_str).map(str::to_string),
    }
}

fn value_to_string(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        Value::Null => String::new(),
        other => other.to_string(),
    }
}

/// Slots used in the body, in first-use order.
pub fn used_slots(content: &str) -> Vec<String> {
    let fm = frontmatter::parse(content);
    slot_names(&content[fm.body_offset.min(content.len())..])
}

pub fn validate(content: &str) -> Vec<Finding> {
    let mut out = Vec::new();
    let declared = declared_variables(content);
    let used = used_slots(content);
    let fm = frontmatter::parse(content);
    let body = &content[fm.body_offset.min(content.len())..];

    // Every slot in the body must be declared, or a render silently leaves a
    // literal `{{topic}}` in the text sent to a model.
    for slot in &used {
        if !declared.iter().any(|d| &d.name == slot) {
            let line = slot_line(body, fm.body_line, slot);
            out.push(
                Finding::error(
                    "prompt.slot.undeclared",
                    format!("`{{{{{slot}}}}}` is used in the body but not declared in `variables`."),
                )
                .at(line)
                .with_hint(format!("Add `{slot}` to the frontmatter `variables` list.")),
            );
        }
    }

    // Every declared variable must be used, or the caller is asked for
    // something that goes nowhere.
    for spec in &declared {
        if !used.iter().any(|u| u == &spec.name) {
            out.push(
                Finding::warning(
                    "prompt.variable.unused",
                    format!("`{}` is declared but never used in the body.", spec.name),
                )
                .at(1),
            );
        }
    }

    if declared.is_empty() && used.is_empty() {
        out.push(Finding::info(
            "prompt.no_variables",
            "No variables and no slots; this reads as a plain document.",
        ));
    }

    out
}

fn slot_line(body: &str, body_line: usize, slot: &str) -> usize {
    let needle = format!("{{{{{slot}");
    for (i, line) in body.lines().enumerate() {
        if line.contains(&needle) {
            return body_line + i;
        }
    }
    body_line
}

/// Fill the slots and return the text. Missing values are an error unless the
/// variable declares a default — a prompt rendered with a hole in it is worse
/// than one that refuses to render.
pub fn render(content: &str, variables: &Map<String, Value>) -> Result<String> {
    let fm = frontmatter::parse(content);
    let body = &content[fm.body_offset.min(content.len())..];
    let declared = declared_variables(content);
    let used = used_slots(content);

    let mut missing: Vec<String> = Vec::new();
    let mut values: Map<String, Value> = Map::new();
    for slot in &used {
        if let Some(v) = variables.get(slot) {
            values.insert(slot.clone(), v.clone());
            continue;
        }
        if let Some(default) = declared
            .iter()
            .find(|d| &d.name == slot)
            .and_then(|d| d.default.clone())
        {
            values.insert(slot.clone(), Value::String(default));
            continue;
        }
        missing.push(slot.clone());
    }

    if !missing.is_empty() {
        return Err(Error::invalid(format!(
            "missing value{} for {}",
            if missing.len() == 1 { "" } else { "s" },
            missing.join(", ")
        )));
    }

    // The blank line after a frontmatter fence is separation, not content; a
    // rendered prompt should not start with it.
    let body = body.trim_start_matches(['\n', '\r']);

    // An extra variable the body never uses is not fatal: the agent still
    // gets its prompt, and `validate_doc` is where that mistake is reported.
    Ok(substitute(body, &values))
}

/// Replace `{{name}}` outside fenced code blocks, so a prompt that documents
/// its own syntax renders correctly.
fn substitute(body: &str, values: &Map<String, Value>) -> String {
    let mut out = String::with_capacity(body.len());
    let mut fence: Option<String> = None;
    let mut first = true;
    for line in body.lines() {
        if !first {
            out.push('\n');
        }
        first = false;

        let trimmed = line.trim_start();
        let is_fence = trimmed.starts_with("```") || trimmed.starts_with("~~~");
        if is_fence {
            match &fence {
                None => {
                    fence = Some(trimmed.chars().take_while(|c| *c == '`' || *c == '~').collect())
                }
                Some(marker) if trimmed.starts_with(marker.as_str()) => fence = None,
                Some(_) => {}
            }
            out.push_str(line);
            continue;
        }

        if fence.is_some() {
            out.push_str(line);
        } else {
            out.push_str(&substitute_line(line, values));
        }
    }
    if body.ends_with('\n') {
        out.push('\n');
    }
    out
}

fn substitute_line(line: &str, values: &Map<String, Value>) -> String {
    let mut out = String::with_capacity(line.len());
    let bytes = line.as_bytes();
    let mut i = 0usize;
    while i < bytes.len() {
        if bytes[i] == b'{' && i + 1 < bytes.len() && bytes[i + 1] == b'{' {
            if let Some(close) = line[i + 2..].find("}}") {
                let name = line[i + 2..i + 2 + close].trim();
                if let Some(value) = values.get(name) {
                    out.push_str(&value_to_string(value));
                    i += 2 + close + 2;
                    continue;
                }
            }
        }
        let ch = line[i..].chars().next().unwrap();
        out.push(ch);
        i += ch.len_utf8();
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn vars(pairs: &[(&str, &str)]) -> Map<String, Value> {
        pairs
            .iter()
            .map(|(k, v)| (k.to_string(), Value::String(v.to_string())))
            .collect()
    }

    #[test]
    fn an_undeclared_slot_is_flagged() {
        let doc = "---\nname: brief\nvariables: [topic]\n---\nWrite about {{topic}} in a {{tone}} voice.\n";
        let findings = validate(doc);
        let f = findings.iter().find(|f| f.rule == "prompt.slot.undeclared").unwrap();
        assert!(f.message.contains("tone"));
    }

    #[test]
    fn an_unused_variable_is_a_warning() {
        let doc = "---\nname: brief\nvariables: [topic, tone]\n---\nWrite about {{topic}}.\n";
        let findings = validate(doc);
        assert!(findings
            .iter()
            .any(|f| f.rule == "prompt.variable.unused" && f.message.contains("tone")));
    }

    #[test]
    fn render_fills_slots() {
        let doc = "---\nname: brief\nvariables: [topic, tone]\n---\nWrite about {{topic}} in a {{tone}} voice.\n";
        let out = render(doc, &vars(&[("topic", "otters"), ("tone", "dry")])).unwrap();
        assert_eq!(out, "Write about otters in a dry voice.\n");
    }

    #[test]
    fn render_refuses_rather_than_leaving_a_hole() {
        let doc = "---\nname: brief\nvariables: [topic]\n---\n{{topic}}\n";
        let err = render(doc, &Map::new()).unwrap_err();
        assert_eq!(err.code(), "invalid");
        assert!(err.to_string().contains("topic"));
    }

    #[test]
    fn declared_defaults_are_used_when_no_value_is_given() {
        let doc = "---\nname: brief\nvariables:\n  - name: tone\n    default: dry\n---\nA {{tone}} voice.\n";
        assert_eq!(render(doc, &Map::new()).unwrap(), "A dry voice.\n");
    }

    #[test]
    fn slots_inside_code_fences_are_left_alone() {
        let doc = "---\nname: t\nvariables: [topic]\n---\nAbout {{topic}}.\n\n```\nliteral {{topic}}\n```\n";
        let out = render(doc, &vars(&[("topic", "otters")])).unwrap();
        assert!(out.contains("About otters."));
        assert!(out.contains("literal {{topic}}"), "fenced text must render verbatim");
    }
}
