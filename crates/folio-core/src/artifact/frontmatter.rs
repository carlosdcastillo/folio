//! YAML frontmatter: the `---` fenced block a skill or prompt opens with.
//!
//! Parsing is deliberately forgiving. A malformed block is reported as a
//! finding by the validators, never as a failure to open the document — a
//! broken skill is exactly the file you most need to be able to read.

use serde_json::{Map, Value};

#[derive(Debug, Clone, Default)]
pub struct Frontmatter {
    /// Parsed mapping, empty when there is no block or it failed to parse.
    pub data: Map<String, Value>,
    /// The raw YAML text between the fences, for round-tripping.
    pub raw: String,
    /// Byte offset in the original document where the body begins.
    pub body_offset: usize,
    /// Line number (1-based) where the body begins.
    pub body_line: usize,
    pub present: bool,
    pub parse_error: Option<String>,
}

impl Frontmatter {
    pub fn get_str(&self, key: &str) -> Option<&str> {
        self.data.get(key).and_then(|v| v.as_str())
    }

    pub fn get(&self, key: &str) -> Option<&Value> {
        self.data.get(key)
    }

    pub fn has(&self, key: &str) -> bool {
        self.data.contains_key(key)
    }

    /// Declared variables of a prompt, in declaration order.
    pub fn string_list(&self, key: &str) -> Option<Vec<String>> {
        match self.data.get(key)? {
            Value::Array(items) => Some(
                items
                    .iter()
                    .filter_map(|v| match v {
                        Value::String(s) => Some(s.clone()),
                        // `variables: [{name: topic, default: x}]` is a shape
                        // agents produce often enough to be worth accepting.
                        Value::Object(o) => o.get("name").and_then(|n| n.as_str()).map(str::to_string),
                        other => other.as_str().map(str::to_string),
                    })
                    .collect(),
            ),
            Value::String(s) => Some(vec![s.clone()]),
            _ => None,
        }
    }
}

/// Split a document into its frontmatter and body.
pub fn parse(text: &str) -> Frontmatter {
    let normalized_prefix_len = if text.starts_with('\u{feff}') { '\u{feff}'.len_utf8() } else { 0 };
    let rest = &text[normalized_prefix_len..];

    let mut fm = Frontmatter { body_offset: 0, body_line: 1, ..Default::default() };

    let first_line_end = rest.find('\n').map(|i| i + 1).unwrap_or(rest.len());
    let first_line = rest[..first_line_end].trim_end_matches(['\r', '\n']);
    if first_line.trim_end() != "---" {
        return fm;
    }

    // Find the closing fence: the next line that is exactly `---` or `...`.
    let mut offset = first_line_end;
    let mut line_no = 2usize;
    let mut yaml_start = first_line_end;
    let mut yaml_end = None;
    while offset < rest.len() {
        let line_end = rest[offset..].find('\n').map(|i| offset + i + 1).unwrap_or(rest.len());
        let line = rest[offset..line_end].trim_end_matches(['\r', '\n']);
        let trimmed = line.trim_end();
        if trimmed == "---" || trimmed == "..." {
            yaml_end = Some(offset);
            offset = line_end;
            line_no += 1;
            break;
        }
        offset = line_end;
        line_no += 1;
    }

    let Some(yaml_end) = yaml_end else {
        // An opening fence with no closing fence is not frontmatter; treat the
        // whole document as body rather than swallowing it.
        return fm;
    };
    if yaml_start > yaml_end {
        yaml_start = yaml_end;
    }

    fm.present = true;
    fm.raw = rest[yaml_start..yaml_end].to_string();
    fm.body_offset = normalized_prefix_len + offset;
    fm.body_line = line_no;

    match serde_yaml_ng::from_str::<serde_yaml_ng::Value>(&fm.raw) {
        Ok(value) => match yaml_to_json(value) {
            Value::Object(map) => fm.data = map,
            Value::Null => {}
            other => {
                fm.parse_error = Some(format!(
                    "frontmatter must be a mapping, found {}",
                    json_kind(&other)
                ));
            }
        },
        Err(e) => fm.parse_error = Some(e.to_string()),
    }

    fm
}

/// The document with its frontmatter block removed.
pub fn body_of(text: &str) -> &str {
    let fm = parse(text);
    &text[fm.body_offset..]
}

fn json_kind(v: &Value) -> &'static str {
    match v {
        Value::Null => "null",
        Value::Bool(_) => "a boolean",
        Value::Number(_) => "a number",
        Value::String(_) => "a string",
        Value::Array(_) => "a list",
        Value::Object(_) => "a mapping",
    }
}

fn yaml_to_json(value: serde_yaml_ng::Value) -> Value {
    use serde_yaml_ng::Value as Y;
    match value {
        Y::Null => Value::Null,
        Y::Bool(b) => Value::Bool(b),
        Y::Number(n) => {
            if let Some(i) = n.as_i64() {
                Value::from(i)
            } else if let Some(f) = n.as_f64() {
                serde_json::Number::from_f64(f).map(Value::Number).unwrap_or(Value::Null)
            } else {
                Value::Null
            }
        }
        Y::String(s) => Value::String(s),
        Y::Sequence(items) => Value::Array(items.into_iter().map(yaml_to_json).collect()),
        Y::Mapping(map) => {
            let mut out = Map::new();
            for (k, v) in map {
                let key = match k {
                    Y::String(s) => s,
                    other => serde_yaml_ng::to_string(&other)
                        .unwrap_or_default()
                        .trim()
                        .to_string(),
                };
                out.insert(key, yaml_to_json(v));
            }
            Value::Object(out)
        }
        Y::Tagged(tagged) => yaml_to_json(tagged.value),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_a_skill_header() {
        let doc = "---\nname: visual-explainer\ndescription: Draws things\n---\n# Body\n";
        let fm = parse(doc);
        assert!(fm.present);
        assert_eq!(fm.get_str("name"), Some("visual-explainer"));
        assert_eq!(&doc[fm.body_offset..], "# Body\n");
        assert_eq!(fm.body_line, 5);
    }

    #[test]
    fn an_unclosed_fence_is_not_frontmatter() {
        let doc = "---\nname: x\n# Body\n";
        let fm = parse(doc);
        assert!(!fm.present);
        assert_eq!(fm.body_offset, 0);
    }

    #[test]
    fn a_horizontal_rule_is_not_frontmatter() {
        let doc = "# Title\n\n---\n\ntext\n";
        assert!(!parse(doc).present);
    }

    #[test]
    fn variables_accept_both_shapes() {
        let plain = parse("---\nvariables: [topic, tone]\n---\n");
        assert_eq!(plain.string_list("variables").unwrap(), vec!["topic", "tone"]);
        let rich = parse("---\nvariables:\n  - name: topic\n  - name: tone\n---\n");
        assert_eq!(rich.string_list("variables").unwrap(), vec!["topic", "tone"]);
    }

    #[test]
    fn malformed_yaml_is_reported_not_fatal() {
        let fm = parse("---\nname: [unclosed\n---\nbody\n");
        assert!(fm.present);
        assert!(fm.parse_error.is_some());
    }
}
