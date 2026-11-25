//! Markdown frontmatter (`---` YAML header) reading and writing.
//!
//! Tools are inconsistent here: Cursor's `.mdc` reader is not a YAML parser
//! (`globs: *.py` is invalid YAML because `*` starts an alias), so parsing
//! falls back to a forgiving `key: value` reader when strict YAML fails.

use std::sync::LazyLock;

use regex::Regex;
use serde_json::{Map, Value};

/// Parsed frontmatter plus the remaining body.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Document {
    pub data: Map<String, Value>,
    pub body: String,
}

/// Split `text` into frontmatter data and body. Text without frontmatter has empty data.
pub fn parse(text: &str) -> Document {
    let text = text.strip_prefix('\u{feff}').unwrap_or(text);
    let Some(rest) = text
        .strip_prefix("---\n")
        .or_else(|| text.strip_prefix("---\r\n"))
    else {
        return Document {
            data: Map::new(),
            body: text.to_string(),
        };
    };
    // Find the closing `---` line.
    let mut offset = 0;
    let mut close = None;
    for line in rest.split_inclusive('\n') {
        if line.trim_end() == "---" {
            close = Some((offset, offset + line.len()));
            break;
        }
        offset += line.len();
    }
    // An empty header (`---\n---`) closes immediately.
    let Some((yaml_end, body_start)) = close else {
        return Document {
            data: Map::new(),
            body: text.to_string(),
        };
    };
    let yaml = &rest[..yaml_end];
    let body = &rest[body_start..];
    let data = parse_yaml_map(yaml).unwrap_or_else(|| lenient(yaml));
    Document {
        data,
        body: body.to_string(),
    }
}

fn parse_yaml_map(yaml: &str) -> Option<Map<String, Value>> {
    if yaml.trim().is_empty() {
        return Some(Map::new());
    }
    match serde_norway::from_str::<Value>(yaml).ok()? {
        Value::Object(m) => Some(m),
        Value::Null => Some(Map::new()),
        _ => None,
    }
}

static KEY_LINE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^([A-Za-z_][A-Za-z0-9_-]*)\s*:\s*(.*)$").expect("valid regex"));

/// `key: value` lines with `- item` lists; values are kept as raw strings.
fn lenient(yaml: &str) -> Map<String, Value> {
    let mut out = Map::new();
    let mut current: Option<String> = None;
    for line in yaml.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }
        if let Some(item) = trimmed.strip_prefix("- ") {
            if let Some(k) = &current {
                let entry = out
                    .entry(k.clone())
                    .or_insert_with(|| Value::Array(Vec::new()));
                if !entry.is_array() {
                    *entry = Value::Array(Vec::new());
                }
                if let Value::Array(a) = entry {
                    a.push(Value::String(unquote(item.trim()).to_string()));
                }
            }
            continue;
        }
        if let Some(c) = KEY_LINE.captures(line) {
            let key = c[1].to_string();
            let raw = c[2].trim();
            let value = match raw {
                "" => Value::Null,
                "true" => Value::Bool(true),
                "false" => Value::Bool(false),
                _ => Value::String(unquote(raw).to_string()),
            };
            out.insert(key.clone(), value);
            current = Some(key);
        }
    }
    out
}

fn unquote(s: &str) -> &str {
    let b = s.as_bytes();
    if b.len() >= 2 && (b[0] == b'"' || b[0] == b'\'') && b[b.len() - 1] == b[0] {
        &s[1..s.len() - 1]
    } else {
        s
    }
}

/// A non-empty string value.
pub fn string(v: Option<&Value>) -> Option<String> {
    match v {
        Some(Value::String(s)) if !s.trim().is_empty() => Some(s.trim().to_string()),
        _ => None,
    }
}

/// A glob list given as a YAML list or a comma-separated string.
pub fn glob_list(v: Option<&Value>) -> Vec<String> {
    let mut out: Vec<String> = match v {
        Some(Value::String(s)) => s
            .split(',')
            .map(|g| unquote(g.trim()).to_string())
            .collect(),
        Some(Value::Array(a)) => a
            .iter()
            .filter_map(|x| x.as_str().map(|s| s.trim().to_string()))
            .collect(),
        _ => Vec::new(),
    };
    out.retain(|g| !g.is_empty());
    out
}

static PLAIN_SAFE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^[A-Za-z0-9_(][A-Za-z0-9 _./()$<>=+-]*$").expect("valid regex"));

/// Render a string as a YAML scalar, quoting only when necessary.
pub fn yaml_scalar(s: &str) -> String {
    let reserved = matches!(
        s.to_ascii_lowercase().as_str(),
        "true" | "false" | "yes" | "no" | "on" | "off" | "null" | "~"
    );
    let numeric = s.parse::<f64>().is_ok();
    if PLAIN_SAFE.is_match(s) && !reserved && !numeric && !s.ends_with(' ') && !s.contains(": ") {
        s.to_string()
    } else {
        // JSON strings are valid YAML double-quoted scalars.
        serde_json::to_string(s).expect("strings serialize")
    }
}

/// Render frontmatter for `entries` (skipping `None`/null values) followed by `body`.
/// Bodies are normalised to end with exactly one newline.
pub fn render(entries: &[(&str, Value)], body: &str) -> String {
    let mut head = String::new();
    for (k, v) in entries {
        render_entry(&mut head, k, v);
    }
    let body = body.trim();
    if head.is_empty() {
        return format!("{body}\n");
    }
    format!("---\n{head}---\n\n{body}\n")
}

fn render_entry(out: &mut String, key: &str, v: &Value) {
    match v {
        Value::Null => {}
        Value::String(s) => out.push_str(&format!("{}: {}\n", key, yaml_scalar(s))),
        Value::Bool(b) => out.push_str(&format!("{key}: {b}\n")),
        Value::Number(n) => out.push_str(&format!("{key}: {n}\n")),
        Value::Array(a) if a.is_empty() => {}
        Value::Array(a) if a.iter().all(Value::is_string) => {
            out.push_str(&format!("{key}:\n"));
            for item in a {
                out.push_str(&format!(
                    "  - {}\n",
                    yaml_scalar(item.as_str().unwrap_or_default())
                ));
            }
        }
        other => {
            let mut m = Map::new();
            m.insert(key.to_string(), other.clone());
            if let Ok(y) = serde_norway::to_string(&Value::Object(m)) {
                out.push_str(&y);
                if !y.ends_with('\n') {
                    out.push('\n');
                }
            }
        }
    }
}

/// Convenience for optional string entries.
pub fn opt(s: &Option<String>) -> Value {
    s.as_ref().map_or(Value::Null, |s| Value::String(s.clone()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn parses_yaml_frontmatter() {
        let d = parse("---\ndescription: Review\npaths:\n  - \"src/**/*.ts\"\n---\n\nBody\n");
        assert_eq!(d.data["description"], json!("Review"));
        assert_eq!(glob_list(d.data.get("paths")), vec!["src/**/*.ts"]);
        assert_eq!(d.body.trim(), "Body");
    }

    #[test]
    fn falls_back_for_cursor_style_globs() {
        let d = parse(
            "---\ndescription: Python style\nglobs: *.py,scripts/**/*.py\nalwaysApply: false\n---\nUse hints.\n",
        );
        assert_eq!(
            glob_list(d.data.get("globs")),
            vec!["*.py", "scripts/**/*.py"]
        );
        assert_eq!(d.data["alwaysApply"], json!(false));
        assert_eq!(d.body.trim(), "Use hints.");
    }

    #[test]
    fn no_frontmatter() {
        let d = parse("Just text\n");
        assert!(d.data.is_empty());
        assert_eq!(d.body, "Just text\n");
    }

    #[test]
    fn render_quotes_when_needed() {
        let out = render(
            &[
                ("description", json!("Review: staged")),
                ("paths", json!(["src/**/*.ts"])),
                ("hint", json!("[focus]")),
                ("plain", json!("Deploy to staging")),
                ("none", Value::Null),
            ],
            "Body",
        );
        assert_eq!(
            out,
            "---\ndescription: \"Review: staged\"\npaths:\n  - \"src/**/*.ts\"\nhint: \"[focus]\"\nplain: Deploy to staging\n---\n\nBody\n"
        );
        let back = parse(&out);
        assert_eq!(back.data["description"], json!("Review: staged"));
        assert_eq!(back.data["hint"], json!("[focus]"));
    }

    #[test]
    fn render_without_entries_is_plain_body() {
        assert_eq!(render(&[("d", Value::Null)], "\nhello\n\n"), "hello\n");
    }
}
