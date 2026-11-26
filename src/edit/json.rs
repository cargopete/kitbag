//! Format-preserving JSON (and JSONC) editing.
//!
//! Tool configs such as `~/.claude.json` hold far more than kitbag manages, and
//! some (VS Code-style) contain comments. Edits go through a concrete syntax
//! tree so untouched keys, ordering, comments and indentation stay as they were.

use std::path::Path;

use anyhow::{Result, anyhow, bail};
use jsonc_parser::ParseOptions;
use jsonc_parser::cst::{CstInputValue, CstNode, CstObject, CstRootNode};
use serde_json::{Map, Value};

pub struct JsonDoc {
    root: CstRootNode,
    path: std::path::PathBuf,
    /// The file used `\r\n` line endings; edits keep them.
    crlf: bool,
}

/// The first property name that appears twice in `obj`, if any.
fn duplicate_key(obj: &CstObject) -> Option<String> {
    let mut seen = std::collections::HashSet::new();
    obj.properties()
        .iter()
        .filter_map(|p| p.name()?.decoded_value().ok())
        .find(|name| !seen.insert(name.clone()))
}

/// The first duplicate key in `node` or anything nested in it, with the keys
/// leading to the object that holds it.
fn duplicate_key_deep(node: &CstNode, at: &mut Vec<String>) -> Option<String> {
    if let Some(obj) = node.as_object() {
        if let Some(dup) = duplicate_key(&obj) {
            return Some(dup);
        }
        for prop in obj.properties() {
            let (Some(name), Some(value)) = (prop.name(), prop.value()) else {
                continue;
            };
            at.push(name.decoded_value().unwrap_or_default());
            if let Some(dup) = duplicate_key_deep(&value, at) {
                return Some(dup);
            }
            at.pop();
        }
    } else if let Some(arr) = node.as_array() {
        for (i, el) in arr.elements().iter().enumerate() {
            at.push(i.to_string());
            if let Some(dup) = duplicate_key_deep(el, at) {
                return Some(dup);
            }
            at.pop();
        }
    }
    None
}

fn to_input(v: &Value) -> CstInputValue {
    match v {
        Value::Null => CstInputValue::Null,
        Value::Bool(b) => CstInputValue::Bool(*b),
        Value::Number(n) => CstInputValue::Number(n.to_string()),
        Value::String(s) => CstInputValue::String(s.clone()),
        Value::Array(a) => CstInputValue::Array(a.iter().map(to_input).collect()),
        Value::Object(m) => {
            CstInputValue::Object(m.iter().map(|(k, v)| (k.clone(), to_input(v))).collect())
        }
    }
}

impl JsonDoc {
    /// Parse `text` (missing or blank files start as `{}`).
    pub fn parse(text: Option<&str>, path: &Path) -> Result<Self> {
        let text = match text {
            Some(t) if !t.trim().is_empty() => t,
            _ => "{}\n",
        };
        let root = CstRootNode::parse(text, &ParseOptions::default()).map_err(|e| {
            anyhow!(
                "{}: invalid JSON at {}",
                path.display(),
                super::safe_parse_error(text, Some(e.range().start), &e.kind().to_string())
            )
        })?;
        match root.value() {
            Some(v) if v.as_object().is_some() => {}
            _ => bail!(
                "{}: expected a JSON object at the top level",
                path.display()
            ),
        }
        Ok(JsonDoc {
            root,
            path: path.to_path_buf(),
            crlf: super::uses_crlf(text),
        })
    }

    /// Refuse to edit at `path` when a key appears twice in the same object,
    /// in any object along `path` or anywhere inside the value at `path`.
    /// Edits would change the first copy while tools read the last one, so
    /// kitbag does not guess which one was meant.
    pub fn check_unique_keys(&self, path: &[&str]) -> Result<()> {
        let fail = |at: &[String], key: &str| {
            let place = if at.is_empty() {
                "at the top level".to_string()
            } else {
                format!("in '{}'", at.join("."))
            };
            anyhow!(
                "{}: key '{key}' appears more than once {place}. Tools read the last one and \
                 kitbag would edit the first, so it changed nothing; remove the duplicate and \
                 run kitbag again",
                self.path.display()
            )
        };
        let Some(mut node) = self.root.value() else {
            return Ok(());
        };
        let mut at: Vec<String> = Vec::new();
        for key in path {
            let Some(obj) = node.as_object() else {
                return Ok(());
            };
            if let Some(dup) = duplicate_key(&obj) {
                return Err(fail(&at, &dup));
            }
            let Some(next) = obj.get(key).and_then(|p| p.value()) else {
                return Ok(());
            };
            at.push((*key).to_string());
            node = next;
        }
        match duplicate_key_deep(&node, &mut at) {
            Some(dup) => Err(fail(&at, &dup)),
            None => Ok(()),
        }
    }

    /// The whole document as a plain value.
    pub fn value(&self) -> Value {
        self.root.to_serde_value().unwrap_or(Value::Null)
    }

    /// The value at `path`, if present.
    pub fn get(&self, path: &[&str]) -> Option<Value> {
        let mut cur = self.value();
        for k in path {
            cur = cur.get(*k)?.clone();
        }
        Some(cur)
    }

    /// The object at `path` as a map (empty if missing or not an object).
    pub fn object(&self, path: &[&str]) -> Map<String, Value> {
        match self.get(path) {
            Some(Value::Object(m)) => m,
            _ => Map::new(),
        }
    }

    fn container(&self, path: &[&str]) -> CstObject {
        let mut obj = self.root.object_value_or_set();
        for k in path {
            obj = obj.object_value_or_set(k);
        }
        obj
    }

    /// Set `path` (non-empty) to `value`, creating intermediate objects as needed.
    pub fn set(&mut self, path: &[&str], value: &Value) {
        let (last, parents) = path.split_last().expect("non-empty path");
        let obj = self.container(parents);
        match obj.get(last) {
            Some(prop) => prop.set_value(to_input(value)),
            None => {
                obj.append(last, to_input(value));
            }
        }
    }

    /// Remove `path` if present.
    pub fn remove(&mut self, path: &[&str]) {
        let (last, parents) = path.split_last().expect("non-empty path");
        if self.get(parents).is_none_or(|v| !v.is_object()) {
            return;
        }
        if let Some(prop) = self.container(parents).get(last) {
            prop.remove();
        }
    }

    pub fn render(&self) -> String {
        let mut s = self.root.to_string();
        if !s.ends_with('\n') {
            s.push('\n');
        }
        if self.crlf {
            s = super::keep_line_endings(Some("\r\n"), s);
        }
        s
    }
}

/// Parse a JSON/JSONC file into a value (missing -> empty object).
pub fn parse_value(text: Option<&str>, path: &Path) -> Result<Value> {
    Ok(JsonDoc::parse(text, path)?.value())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    const P: &str = "test.json";

    #[test]
    fn preserves_unrelated_content_and_comments() {
        let text = "{\n  // my comment\n  \"theme\": \"dark\",\n  \"mcpServers\": {\n    \"a\": { \"command\": \"x\" }\n  }\n}\n";
        let mut d = JsonDoc::parse(Some(text), Path::new(P)).unwrap();
        d.set(
            &["mcpServers", "b"],
            &json!({"command": "y", "args": ["1"]}),
        );
        let out = d.render();
        assert!(out.contains("// my comment"));
        assert!(out.contains("\"theme\": \"dark\""));
        assert!(out.contains("\"a\": { \"command\": \"x\" }"));
        let v: Value = JsonDoc::parse(Some(&out), Path::new(P)).unwrap().value();
        assert_eq!(v["mcpServers"]["b"], json!({"command": "y", "args": ["1"]}));
    }

    #[test]
    fn creates_from_nothing() {
        let mut d = JsonDoc::parse(None, Path::new(P)).unwrap();
        d.set(&["mcpServers", "s"], &json!({"command": "c"}));
        let v: Value = serde_json::from_str(&d.render()).unwrap();
        assert_eq!(v, json!({"mcpServers": {"s": {"command": "c"}}}));
    }

    #[test]
    fn replaces_and_removes() {
        let mut d = JsonDoc::parse(Some("{\"a\": 1, \"b\": {\"c\": 2}}"), Path::new(P)).unwrap();
        d.set(&["a"], &json!(5));
        d.remove(&["b", "c"]);
        d.remove(&["nope", "x"]);
        assert_eq!(d.value(), json!({"a": 5, "b": {}}));
    }

    #[test]
    fn duplicate_keys_are_refused_where_kitbag_edits() {
        let text = "{\"mcpServers\": {\"a\": {}}, \"x\": 1, \"mcpServers\": {\"b\": {}}}";
        let d = JsonDoc::parse(Some(text), Path::new(P)).unwrap();
        let err = d
            .check_unique_keys(&["mcpServers"])
            .unwrap_err()
            .to_string();
        assert!(
            err.contains("test.json") && err.contains("'mcpServers'"),
            "{err}"
        );
        assert!(err.contains("at the top level"), "{err}");

        let text = "{\"mcpServers\": {\"a\": {\"command\": \"x\", \"command\": \"y\"}}}";
        let d = JsonDoc::parse(Some(text), Path::new(P)).unwrap();
        let err = d
            .check_unique_keys(&["mcpServers"])
            .unwrap_err()
            .to_string();
        assert!(
            err.contains("'command'") && err.contains("in 'mcpServers.a'"),
            "{err}"
        );

        // Duplicates elsewhere in the file are not kitbag's business.
        let text = "{\"other\": {\"k\": 1, \"k\": 2}, \"mcpServers\": {\"a\": {}}}";
        let d = JsonDoc::parse(Some(text), Path::new(P)).unwrap();
        d.check_unique_keys(&["mcpServers"]).unwrap();
        d.check_unique_keys(&["missing", "deeper"]).unwrap();
        assert!(d.check_unique_keys(&["other"]).is_err());
    }

    #[test]
    fn keeps_crlf_line_endings() {
        let text = "{\r\n  \"theme\": \"dark\",\r\n  \"mcpServers\": {}\r\n}\r\n";
        let mut d = JsonDoc::parse(Some(text), Path::new(P)).unwrap();
        d.set(
            &["mcpServers", "b"],
            &json!({"command": "y", "args": ["1"]}),
        );
        let out = d.render();
        assert!(!out.replace("\r\n", "").contains('\n'), "{out:?}");
        assert!(
            out.starts_with("{\r\n  \"theme\": \"dark\",\r\n"),
            "{out:?}"
        );
        let lf = JsonDoc::parse(Some("{\"a\": 1}"), Path::new(P)).unwrap();
        assert!(!lf.render().contains('\r'));
    }

    #[test]
    fn rejects_non_objects_and_garbage() {
        assert!(JsonDoc::parse(Some("[1]"), Path::new(P)).is_err());
        assert!(JsonDoc::parse(Some("{ nope"), Path::new(P)).is_err());
    }
}
