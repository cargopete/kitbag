//! Format-preserving JSON (and JSONC) editing.
//!
//! Tool configs such as `~/.claude.json` hold far more than kitbag manages, and
//! some (VS Code-style) contain comments. Edits go through a concrete syntax
//! tree so untouched keys, ordering, comments and indentation stay as they were.

use std::path::Path;

use anyhow::{Result, anyhow, bail};
use jsonc_parser::ParseOptions;
use jsonc_parser::cst::{CstInputValue, CstObject, CstRootNode};
use serde_json::{Map, Value};

pub struct JsonDoc {
    root: CstRootNode,
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
        Ok(JsonDoc { root })
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
    fn rejects_non_objects_and_garbage() {
        assert!(JsonDoc::parse(Some("[1]"), Path::new(P)).is_err());
        assert!(JsonDoc::parse(Some("{ nope"), Path::new(P)).is_err());
    }
}
