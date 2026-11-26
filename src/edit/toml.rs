//! Comment-preserving TOML editing on top of `toml_edit`.

use std::path::Path;

use anyhow::{Result, anyhow};
use serde_json::{Map, Number, Value};
use toml_edit::{Array, DocumentMut, InlineTable, Item, Table};

pub fn parse(text: Option<&str>, path: &Path) -> Result<DocumentMut> {
    let text = text.unwrap_or("");
    text.parse::<DocumentMut>().map_err(|e| {
        anyhow!(
            "{}: invalid TOML at {}",
            path.display(),
            super::safe_parse_error(text, e.span().map(|s| s.start), e.message())
        )
    })
}

/// Convert a TOML item into a JSON value (for comparison and capture).
pub fn item_to_json(item: &Item) -> Value {
    match item {
        Item::None => Value::Null,
        Item::Value(v) => value_to_json(v),
        Item::Table(t) => Value::Object(table_to_json(t)),
        Item::ArrayOfTables(a) => {
            Value::Array(a.iter().map(|t| Value::Object(table_to_json(t))).collect())
        }
    }
}

pub fn table_to_json(t: &Table) -> Map<String, Value> {
    t.iter()
        .map(|(k, v)| (k.to_string(), item_to_json(v)))
        .collect()
}

fn value_to_json(v: &toml_edit::Value) -> Value {
    use toml_edit::Value as V;
    match v {
        V::String(s) => Value::String(s.value().clone()),
        V::Integer(i) => Value::Number((*i.value()).into()),
        V::Float(f) => Number::from_f64(*f.value()).map_or(Value::Null, Value::Number),
        V::Boolean(b) => Value::Bool(*b.value()),
        V::Datetime(d) => Value::String(d.value().to_string()),
        V::Array(a) => Value::Array(a.iter().map(value_to_json).collect()),
        V::InlineTable(t) => Value::Object(
            t.iter()
                .map(|(k, v)| (k.to_string(), value_to_json(v)))
                .collect(),
        ),
    }
}

/// Convert a JSON value into an inline TOML value. `null` has no TOML form.
pub fn json_to_value(v: &Value) -> Option<toml_edit::Value> {
    Some(match v {
        Value::Null => return None,
        Value::Bool(b) => (*b).into(),
        Value::Number(n) => match (n.as_i64(), n.as_f64()) {
            (Some(i), _) => i.into(),
            (None, Some(f)) => f.into(),
            _ => return None,
        },
        Value::String(s) => s.as_str().into(),
        Value::Array(a) => {
            let mut arr = Array::new();
            for x in a {
                if let Some(v) = json_to_value(x) {
                    arr.push(v);
                }
            }
            toml_edit::Value::Array(arr)
        }
        Value::Object(m) => {
            let mut t = InlineTable::new();
            for (k, x) in m {
                if let Some(v) = json_to_value(x) {
                    t.insert(k, v);
                }
            }
            toml_edit::Value::InlineTable(t)
        }
    })
}

/// A standard `[table]` whose nested objects are written inline (`env = { A = "1" }`).
pub fn json_to_table(m: &Map<String, Value>) -> Table {
    let mut t = Table::new();
    for (k, v) in m {
        if let Some(val) = json_to_value(v) {
            t.insert(k, Item::Value(val));
        }
    }
    t
}

/// Set `key` to `val`, keeping the comments and whitespace around an existing value.
pub fn set_keep_decor(table: &mut Table, key: &str, mut val: toml_edit::Value) {
    if let Some(cur) = table.get_mut(key).and_then(Item::as_value_mut) {
        *val.decor_mut() = cur.decor().clone();
        *cur = val;
        return;
    }
    table.insert(key, Item::Value(val));
}

/// Get or create the table at `path` (creating implicit parent tables).
pub fn table_mut<'a>(doc: &'a mut DocumentMut, path: &[&str]) -> Result<&'a mut Table> {
    let mut cur = doc.as_table_mut();
    for k in path {
        if !cur.contains_key(k) {
            let mut t = Table::new();
            t.set_implicit(true);
            cur.insert(k, Item::Table(t));
        }
        let item = cur.get_mut(k).expect("just inserted");
        if let Some(it) = item.as_inline_table_mut() {
            // Promote `mcp_servers = { ... }` to a real table so we can add sub-tables.
            let t = std::mem::take(it).into_table();
            *item = Item::Table(t);
        }
        cur = item
            .as_table_mut()
            .ok_or_else(|| anyhow!("'{k}' is not a table"))?;
    }
    Ok(cur)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn edits_keep_comments() {
        let text =
            "# top\nmodel = \"x\" # trailing\n\n# servers\n[mcp_servers.a]\ncommand = \"a\"\n";
        let mut doc = parse(Some(text), Path::new("c.toml")).unwrap();
        let servers = table_mut(&mut doc, &["mcp_servers"]).unwrap();
        let def = json!({"command": "b", "args": ["-y"], "env": {"K": "v"}});
        servers.insert("b", Item::Table(json_to_table(def.as_object().unwrap())));
        let out = doc.to_string();
        assert!(out.starts_with("# top\nmodel = \"x\" # trailing\n\n# servers\n[mcp_servers.a]\n"));
        assert!(
            out.contains(
                "[mcp_servers.b]\ncommand = \"b\"\nargs = [\"-y\"]\nenv = { K = \"v\" }\n"
            )
        );
        let back = parse(Some(&out), Path::new("c.toml")).unwrap();
        assert_eq!(item_to_json(&back["mcp_servers"]["b"]), def);
    }

    #[test]
    fn duplicate_keys_and_tables_are_parse_errors() {
        for text in [
            "[mcp_servers.a]\ncommand = \"x\"\n[mcp_servers.a]\ncommand = \"y\"\n",
            "[mcp_servers.a]\ncommand = \"x\"\ncommand = \"y\"\n",
        ] {
            assert!(parse(Some(text), Path::new("c.toml")).is_err(), "{text}");
        }
    }

    #[test]
    fn set_keeps_comments() {
        let mut doc = parse(Some("# head\nmodel = \"a\" # why\n"), Path::new("c.toml")).unwrap();
        set_keep_decor(doc.as_table_mut(), "model", "b".into());
        assert_eq!(doc.to_string(), "# head\nmodel = \"b\" # why\n");
    }

    #[test]
    fn promotes_inline_parent() {
        let mut doc = parse(
            Some("mcp_servers = { a = { command = \"a\" } }\n"),
            Path::new("c.toml"),
        )
        .unwrap();
        let t = table_mut(&mut doc, &["mcp_servers"]).unwrap();
        t.insert(
            "b",
            Item::Table(json_to_table(json!({"command": "b"}).as_object().unwrap())),
        );
        let back = parse(Some(&doc.to_string()), Path::new("c.toml")).unwrap();
        assert_eq!(
            item_to_json(&back["mcp_servers"]["a"]["command"]),
            json!("a")
        );
        assert_eq!(
            item_to_json(&back["mcp_servers"]["b"]["command"]),
            json!("b")
        );
    }
}
