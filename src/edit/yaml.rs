//! Line-based YAML editing for one top-level mapping of servers.
//!
//! There is no comment-preserving YAML library, so this edits the text of a
//! config file directly: it finds the top-level section (`mcp_servers:`),
//! finds each entry in it by indentation, and replaces, adds or removes whole
//! entries. Everything else in the file (other keys, comments, blank lines,
//! entries kitbag didn't touch, line endings) is left byte for byte as it was.
//! An entry that changes is rewritten as a whole, so comments inside it go.
//!
//! The result is checked before it is returned: the new text is parsed again
//! and has to equal the old document with just the intended changes made.

use std::path::Path;
use std::sync::LazyLock;

use anyhow::{Result, anyhow, bail};
use regex::Regex;
use serde_json::{Map, Value};

/// A parsed YAML config file that can be edited one section entry at a time.
#[derive(Debug)]
pub struct YamlDoc {
    /// The file split on `\n`; a line keeps its `\r`. A file that ends with a
    /// newline ends with an empty element, so joining gives the text back.
    lines: Vec<String>,
    bom: bool,
    crlf: bool,
    /// What the file means now.
    parsed: Map<String, Value>,
    /// What it should mean after the edits so far.
    expected: Map<String, Value>,
    path: String,
    changed: bool,
}

/// Where a section and its entries are in the lines.
struct Layout {
    /// Line of `section:`.
    key_line: usize,
    /// Is the value on the key's own line (`section: {}`, `section: null`)?
    inline_empty: bool,
    /// Line after the last line of content in the section.
    end: usize,
    indent: usize,
    /// How much deeper the lines below an entry's name are indented.
    step: usize,
    /// (name, first line, line after its last content line)
    entries: Vec<(String, usize, usize)>,
}

static ENTRY_KEY: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r#"^(?:"((?:[^"\\]|\\.)*)"|'((?:[^']|'')*)'|([^\s#'"\[\]{},&*!|>%@`-][^#]*?|-[^\s#][^#]*?))\s*:(?:\s|$)"#)
        .expect("valid regex")
});

/// Parse `text` (a missing file is an empty mapping) into JSON values.
pub fn parse_map(text: Option<&str>, path: &Path) -> Result<Map<String, Value>> {
    let text = text.unwrap_or("");
    let body = text.strip_prefix('\u{feff}').unwrap_or(text);
    if body.trim().is_empty() {
        return Ok(Map::new());
    }
    match serde_norway::from_str::<Value>(body) {
        Ok(Value::Object(m)) => Ok(m),
        Ok(Value::Null) => Ok(Map::new()),
        Ok(_) => bail!(
            "{}: expected a YAML mapping at the top level",
            path.display()
        ),
        Err(e) => bail!(
            "{}: invalid YAML at {}",
            path.display(),
            super::safe_parse_error(body, e.location().map(|l| l.index()), &e.to_string())
        ),
    }
}

impl YamlDoc {
    pub fn parse(text: Option<&str>, path: &Path) -> Result<Self> {
        let parsed = parse_map(text, path)?;
        let text = text.unwrap_or("");
        let (bom, body) = match text.strip_prefix('\u{feff}') {
            Some(b) => (true, b),
            None => (false, text),
        };
        Ok(YamlDoc {
            lines: body.split('\n').map(str::to_string).collect(),
            bom,
            crlf: super::uses_crlf(text),
            expected: parsed.clone(),
            parsed,
            path: path.display().to_string(),
            changed: false,
        })
    }

    /// The entries of `section` as they are now (empty if there are none).
    pub fn entries(&self, section: &str) -> Map<String, Value> {
        match self.parsed.get(section) {
            Some(Value::Object(m)) => m.clone(),
            _ => Map::new(),
        }
    }

    /// The value of a top-level key, for reading other settings.
    pub fn get(&self, key: &str) -> Option<&Value> {
        self.parsed.get(key)
    }

    /// Can `section` be edited in place? If not, why not. Parse errors are
    /// reported by [`YamlDoc::parse`]; this is about layouts the line editor
    /// does not handle (a flow mapping with entries on the section's line, an
    /// anchor or alias, a section that isn't a mapping).
    pub fn check_editable(&self, section: &str) -> std::result::Result<(), String> {
        match self.parsed.get(section) {
            None | Some(Value::Null) => {}
            Some(Value::Object(_)) => {}
            Some(_) => return Err(format!("'{section}' is not a mapping")),
        }
        self.layout(section).map(|_| ())
    }

    fn layout(&self, section: &str) -> std::result::Result<Option<Layout>, String> {
        let Some(key_line) = self.lines.iter().position(|l| is_section_key(l, section)) else {
            return Ok(None);
        };
        let rest = value_after_key(&self.lines[key_line]);
        let inline_empty = match rest.as_str() {
            "" => false,
            "{}" | "null" | "~" => true,
            other => {
                return Err(format!(
                    "'{section}' is written as `{other}` on the same line as its name"
                ));
            }
        };
        // The section runs to the next top-level line that has content.
        let mut end = key_line + 1;
        let mut indent = None;
        let mut step = None;
        let mut entries: Vec<(String, usize, usize)> = Vec::new();
        let mut i = key_line + 1;
        while i < self.lines.len() {
            let line = self.lines[i].trim_end_matches('\r');
            if is_blank_or_comment(line) {
                i += 1;
                continue;
            }
            let lead = leading_spaces(line);
            if lead == 0 {
                break;
            }
            if line.starts_with('\t') {
                return Err("it is indented with tabs".into());
            }
            let ind = *indent.get_or_insert(lead);
            if lead < ind {
                return Err("its entries are not indented evenly".into());
            }
            if lead > ind {
                step.get_or_insert(lead - ind);
            }
            // A list written at the key's own indentation (`name:` and, below
            // it, `- item` lines, the way PyYAML dumps) belongs to that entry.
            let continues = !entries.is_empty() && is_list_item(line);
            if lead == ind && !continues {
                let Some(name) = entry_name(line) else {
                    return Err(format!("line {} is not a `name:` entry", i + 1));
                };
                if let Some(last) = entries.last_mut() {
                    last.2 = end;
                }
                entries.push((name, i, i + 1));
            }
            end = i + 1;
            i += 1;
        }
        if let Some(last) = entries.last_mut() {
            last.2 = end;
        }
        if inline_empty && !entries.is_empty() {
            return Err(format!("'{section}' has entries below `{rest}`"));
        }
        Ok(Some(Layout {
            key_line,
            inline_empty,
            end,
            indent: indent.unwrap_or(2),
            step: step.unwrap_or(2),
            entries,
        }))
    }

    /// Add or replace entry `name` of `section`.
    pub fn set(&mut self, section: &str, name: &str, def: &Value) -> Result<()> {
        let layout = self.layout(section).map_err(|e| self.cannot(section, &e))?;
        let (indent, step) = layout.as_ref().map_or((2, 2), |l| (l.indent, l.step));
        let text = render_entry(name, def, indent, step);
        match layout {
            None => {
                let mut block = vec![format!("{}:", key_text(section))];
                block.extend(text);
                let at = self.append_point();
                self.insert(at, block);
            }
            Some(l) => {
                let mut at = l.end;
                if l.inline_empty {
                    self.lines[l.key_line] = replace_value(&self.lines[l.key_line], "");
                    at = l.key_line + 1;
                }
                if let Some((_, start, end)) = l.entries.iter().find(|(n, _, _)| n == name) {
                    let (start, end) = (*start, *end);
                    self.lines.drain(start..end);
                    at = start;
                }
                self.insert(at, text);
            }
        }
        let entry = self
            .expected
            .entry(section.to_string())
            .or_insert_with(|| Value::Object(Map::new()));
        if !entry.is_object() {
            *entry = Value::Object(Map::new());
        }
        if let Value::Object(m) = entry {
            m.insert(name.to_string(), def.clone());
        }
        self.changed = true;
        Ok(())
    }

    /// Remove entry `name` of `section`, if there.
    pub fn remove(&mut self, section: &str, name: &str) -> Result<()> {
        let layout = self.layout(section).map_err(|e| self.cannot(section, &e))?;
        let Some(l) = layout else { return Ok(()) };
        let Some((_, start, end)) = l.entries.iter().find(|(n, _, _)| n == name) else {
            return Ok(());
        };
        let (start, end) = (*start, *end);
        self.lines.drain(start..end);
        if l.entries.len() == 1 {
            // Don't leave `section:` with nothing under it (that is null).
            let line = replace_value(&self.lines[l.key_line], " {}");
            self.lines[l.key_line] = line;
        }
        if let Some(Value::Object(m)) = self.expected.get_mut(section) {
            m.remove(name);
        }
        self.changed = true;
        Ok(())
    }

    pub fn changed(&self) -> bool {
        self.changed
    }

    /// The edited text, after checking that it says what was meant.
    pub fn render(&self) -> Result<String> {
        let mut text = self.lines.join("\n");
        if self.bom {
            text.insert(0, '\u{feff}');
        }
        if !self.changed {
            return Ok(text);
        }
        let again = parse_map(Some(&text), Path::new(&self.path))
            .map_err(|e| anyhow!("{e:#}\nkitbag did not write it"))?;
        if again != self.expected {
            bail!(
                "{}: kitbag could not edit this file without changing something else, so it did not write it; add the MCP servers by hand",
                self.path
            );
        }
        Ok(text)
    }

    fn cannot(&self, section: &str, why: &str) -> anyhow::Error {
        anyhow!(
            "{}: kitbag can't edit '{section}' in place: {why}",
            self.path
        )
    }

    /// Where a new top-level section goes: the end of the file.
    fn append_point(&self) -> usize {
        // A file that ends with a newline has an empty last element.
        match self.lines.last() {
            Some(l) if l.is_empty() => self.lines.len() - 1,
            _ => self.lines.len(),
        }
    }

    fn insert(&mut self, at: usize, new: Vec<String>) {
        let tail = if self.crlf { "\r" } else { "" };
        let new = new.into_iter().map(|l| format!("{l}{tail}"));
        self.lines.splice(at..at, new);
    }
}

fn is_blank_or_comment(line: &str) -> bool {
    let t = line.trim();
    t.is_empty() || t.starts_with('#')
}

/// Is this line a `- item` of a block list?
fn is_list_item(line: &str) -> bool {
    let t = line.trim_start_matches(' ');
    t == "-" || t.starts_with("- ")
}

fn leading_spaces(line: &str) -> usize {
    line.len() - line.trim_start_matches([' ', '\t']).len()
}

fn key_text(key: &str) -> String {
    if plain_key(key) {
        key.to_string()
    } else {
        Value::String(key.to_string()).to_string()
    }
}

/// Can `key` be written without quotes, and still read back as that string?
fn plain_key(key: &str) -> bool {
    static PLAIN: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r"^[A-Za-z_][A-Za-z0-9_.-]*$").expect("valid regex"));
    const WORDS: &[&str] = &[
        "true", "false", "null", "yes", "no", "on", "off", "y", "n", "~",
    ];
    PLAIN.is_match(key) && !WORDS.contains(&key.to_ascii_lowercase().as_str())
}

/// Is this a top-level `section:` line (the key may be quoted)?
fn is_section_key(line: &str, section: &str) -> bool {
    let line = line.trim_end_matches('\r');
    if line.starts_with([' ', '\t', '#']) {
        return false;
    }
    entry_name(line).is_some_and(|n| n == section)
}

/// The name of the entry a `name: ...` line starts.
fn entry_name(line: &str) -> Option<String> {
    let c = ENTRY_KEY.captures(line.trim_start_matches(' '))?;
    if let Some(q) = c.get(1) {
        return serde_norway::from_str::<String>(&format!("\"{}\"", q.as_str())).ok();
    }
    if let Some(q) = c.get(2) {
        return Some(q.as_str().replace("''", "'"));
    }
    c.get(3).map(|m| m.as_str().trim_end().to_string())
}

/// What follows `key:` on a line, without a trailing comment.
fn value_after_key(line: &str) -> String {
    let line = line.trim_end_matches('\r');
    let Some(colon) = find_key_colon(line) else {
        return String::new();
    };
    let rest = &line[colon + 1..];
    let rest = match rest.find(" #") {
        Some(i) => &rest[..i],
        None => rest,
    };
    let rest = rest.trim();
    if rest.starts_with('#') {
        String::new()
    } else {
        rest.to_string()
    }
}

/// The colon that ends the key at the start of `line`.
fn find_key_colon(line: &str) -> Option<usize> {
    let c = ENTRY_KEY.captures(line)?;
    let whole = c.get(0)?;
    // The match includes the colon and one space after it (if any).
    line[..whole.end()].rfind(':')
}

/// `line` with what follows its key's colon replaced by `value` (kept comment
/// dropped: the value was `{}` or `null`, and a comment on it is not worth
/// keeping). A `value` of `""` leaves just `key:`.
fn replace_value(line: &str, value: &str) -> String {
    let cr = if line.ends_with('\r') { "\r" } else { "" };
    let line = line.trim_end_matches('\r');
    match find_key_colon(line) {
        Some(c) => format!("{}{value}{cr}", &line[..=c]),
        None => format!("{line}{cr}"),
    }
}

/// `name:` and its definition, as lines indented by `indent`.
fn render_entry(name: &str, def: &Value, indent: usize, step: usize) -> Vec<String> {
    let mut out = vec![format!("{}{}:", " ".repeat(indent), key_text(name))];
    emit_value(def, indent + step, step, &mut out);
    out
}

/// Add `v` to the key line at the end of `out`: a mapping as indented
/// `key: value` lines below it, anything else on the key's own line.
fn emit_value(v: &Value, indent: usize, step: usize, out: &mut Vec<String>) {
    match v {
        Value::Object(m) if !m.is_empty() => emit_map(m, indent, step, out),
        // A list of mappings (hook entries) as a block list; lists of plain
        // values stay on the key's line.
        Value::Array(a)
            if !a.is_empty()
                && a.iter()
                    .all(|x| x.as_object().is_some_and(|m| !m.is_empty())) =>
        {
            let pad = " ".repeat(indent);
            for item in a {
                let Value::Object(m) = item else { continue };
                let mut lines = Vec::new();
                emit_map(m, indent + 2, step, &mut lines);
                if let Some(first) = lines.first_mut() {
                    *first = format!("{pad}- {}", first.trim_start_matches(' '));
                }
                out.extend(lines);
            }
        }
        other => {
            if let Some(last) = out.last_mut() {
                last.push(' ');
                last.push_str(&flow(other));
            }
        }
    }
}

fn emit_map(m: &Map<String, Value>, indent: usize, step: usize, out: &mut Vec<String>) {
    let pad = " ".repeat(indent);
    for (k, v) in m {
        out.push(format!("{pad}{}:", key_text(k)));
        emit_value(v, indent + step, step, out);
    }
}

/// A value as a YAML flow node. JSON is valid YAML: strings are double
/// quoted, so `${VAR}`, colons, leading `*` and the like are safe.
fn flow(v: &Value) -> String {
    match v {
        Value::Array(a) => {
            let items: Vec<String> = a.iter().map(flow).collect();
            format!("[{}]", items.join(", "))
        }
        Value::Object(m) => {
            let items: Vec<String> = m
                .iter()
                .map(|(k, v)| format!("{}: {}", Value::String(k.clone()), flow(v)))
                .collect();
            format!("{{{}}}", items.join(", "))
        }
        other => other.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn doc(text: &str) -> YamlDoc {
        YamlDoc::parse(Some(text), Path::new("config.yaml")).unwrap()
    }

    #[test]
    fn adds_a_section_to_the_end_and_keeps_the_rest() {
        let text = "# my config\nmodel: x  # fast\n\nterminal:\n  cwd: .\n";
        let mut d = doc(text);
        d.set(
            "mcp_servers",
            "git",
            &json!({"command": "uvx", "args": ["mcp-server-git"]}),
        )
        .unwrap();
        let out = d.render().unwrap();
        assert!(out.starts_with(text), "{out}");
        assert_eq!(
            &out[text.len()..],
            "mcp_servers:\n  git:\n    command: \"uvx\"\n    args: [\"mcp-server-git\"]\n"
        );
    }

    #[test]
    fn creates_a_file_and_handles_no_trailing_newline() {
        let mut d = doc("");
        d.set("mcp_servers", "a", &json!({"url": "https://x.example/mcp"}))
            .unwrap();
        assert_eq!(
            d.render().unwrap(),
            "mcp_servers:\n  a:\n    url: \"https://x.example/mcp\"\n"
        );
        let mut d = doc("model: x");
        d.set("mcp_servers", "a", &json!({"command": "c"})).unwrap();
        assert_eq!(
            d.render().unwrap(),
            "model: x\nmcp_servers:\n  a:\n    command: \"c\""
        );
    }

    #[test]
    fn replaces_one_entry_and_leaves_neighbours_and_comments() {
        let text = "model: x\nmcp_servers:\n  # mine\n  mine:\n    command: m  # keep\n    args: [a]\n\n  old:\n    command: old\n    timeout: 30\n    # trailing\n  last:\n    url: https://l.example\ntools: 1\n";
        let mut d = doc(text);
        d.set(
            "mcp_servers",
            "old",
            &json!({"command": "new", "timeout": 30}),
        )
        .unwrap();
        let out = d.render().unwrap();
        assert_eq!(
            out,
            "model: x\nmcp_servers:\n  # mine\n  mine:\n    command: m  # keep\n    args: [a]\n\n  old:\n    command: \"new\"\n    timeout: 30\n    # trailing\n  last:\n    url: https://l.example\ntools: 1\n"
        );
    }

    #[test]
    fn removes_entries_and_collapses_an_empty_section() {
        let text = "a: 1\nmcp_servers:\n  x:\n    command: x\n  y:\n    command: y\nb: 2\n";
        let mut d = doc(text);
        d.remove("mcp_servers", "x").unwrap();
        assert_eq!(
            d.render().unwrap(),
            "a: 1\nmcp_servers:\n  y:\n    command: y\nb: 2\n"
        );
        d.remove("mcp_servers", "y").unwrap();
        assert_eq!(d.render().unwrap(), "a: 1\nmcp_servers: {}\nb: 2\n");
        // And entries can be added again under `{}`.
        d.set("mcp_servers", "z", &json!({"command": "z"})).unwrap();
        assert_eq!(
            d.render().unwrap(),
            "a: 1\nmcp_servers:\n  z:\n    command: \"z\"\nb: 2\n"
        );
    }

    #[test]
    fn fills_an_empty_or_null_section() {
        for first in [
            "mcp_servers: {}",
            "mcp_servers:",
            "mcp_servers: null  # none yet",
        ] {
            let mut d = doc(&format!("{first}\nmodel: x\n"));
            d.set("mcp_servers", "a", &json!({"command": "c"})).unwrap();
            assert_eq!(
                d.render().unwrap(),
                "mcp_servers:\n  a:\n    command: \"c\"\nmodel: x\n",
                "{first}"
            );
        }
    }

    #[test]
    fn follows_the_files_indent_and_line_endings() {
        let text = "mcp_servers:\r\n    a:\r\n        command: a\r\nmodel: x\r\n";
        let mut d = doc(text);
        d.set(
            "mcp_servers",
            "b",
            &json!({"command": "b", "env": {"K": "${V}"}}),
        )
        .unwrap();
        assert_eq!(
            d.render().unwrap(),
            "mcp_servers:\r\n    a:\r\n        command: a\r\n    b:\r\n        command: \"b\"\r\n        env:\r\n            K: \"${V}\"\r\nmodel: x\r\n"
        );
    }

    #[test]
    fn quotes_awkward_names_and_values() {
        let mut d = doc("");
        d.set(
            "mcp_servers",
            "no",
            &json!({"command": "a: b", "args": ["# x", "*y", "true"], "env": {"yes": "1"}}),
        )
        .unwrap();
        let out = d.render().unwrap();
        assert!(out.contains("  \"no\":\n"), "{out}");
        let again = parse_map(Some(&out), Path::new("c")).unwrap();
        assert_eq!(
            again["mcp_servers"]["no"]["args"],
            json!(["# x", "*y", "true"])
        );
        assert_eq!(again["mcp_servers"]["no"]["env"]["yes"], "1");
    }

    #[test]
    fn keeps_a_byte_order_mark() {
        let mut d = doc("\u{feff}model: x\n");
        d.set("mcp_servers", "a", &json!({"command": "c"})).unwrap();
        assert!(d.render().unwrap().starts_with("\u{feff}model: x\n"));
    }

    #[test]
    fn refuses_layouts_it_cannot_edit() {
        for text in [
            "mcp_servers: {a: {command: x}}\n",
            "mcp_servers: *other\n",
            "mcp_servers:\n  - a\n",
        ] {
            let d = YamlDoc::parse(Some(text), Path::new("c"));
            // Some of these don't parse into servers; either way no edit.
            if let Ok(d) = d {
                assert!(d.check_editable("mcp_servers").is_err(), "{text}");
            }
        }
        assert!(YamlDoc::parse(Some("a: [1\n"), Path::new("c")).is_err());
        assert!(YamlDoc::parse(Some("- a\n"), Path::new("c")).is_err());
    }

    #[test]
    fn writes_a_list_of_mappings_as_a_block_list_and_keeps_its_neighbours() {
        let text = "hooks:\n  pre_tool_call:\n  - command: mine   # mine\n  outbound:\n  - name: ci\n    url: https://x.example\nother: 1\n";
        let mut d = doc(text);
        let list = json!([
            {"command": "mine"},
            {"command": "/x/check.sh", "matcher": "a|b", "timeout": 5, "fail_closed": true}
        ]);
        d.set("hooks", "pre_tool_call", &list).unwrap();
        d.set(
            "hooks",
            "on_session_start",
            &json!([{"command": "echo hi"}]),
        )
        .unwrap();
        let out = d.render().unwrap();
        assert_eq!(
            out,
            "hooks:\n  pre_tool_call:\n    - command: \"mine\"\n    - command: \"/x/check.sh\"\n      matcher: \"a|b\"\n      timeout: 5\n      fail_closed: true\n  outbound:\n  - name: ci\n    url: https://x.example\n  on_session_start:\n    - command: \"echo hi\"\nother: 1\n"
        );
        // Lists of plain values stay on one line.
        let mut d = doc("a: 1\n");
        d.set("s", "k", &json!({"args": ["x", "y"]})).unwrap();
        assert_eq!(
            d.render().unwrap(),
            "a: 1\ns:\n  k:\n    args: [\"x\", \"y\"]\n"
        );
    }

    #[test]
    fn a_list_at_the_keys_indent_is_part_of_its_entry_and_crlf_is_kept() {
        let text =
            "hooks:\r\n  a:\r\n  - command: x\r\n  - command: y\r\n  b:\r\n  - command: z\r\n";
        let mut d = doc(text);
        d.set("hooks", "a", &json!([{"command": "w"}])).unwrap();
        d.remove("hooks", "b").unwrap();
        assert_eq!(
            d.render().unwrap(),
            "hooks:\r\n  a:\r\n    - command: \"w\"\r\n"
        );
        // A list where the section's entries should be is still refused.
        let d = doc("hooks:\n- a\n");
        assert!(d.check_editable("hooks").is_err());
    }

    #[test]
    fn an_unchanged_doc_renders_as_it_was() {
        let text = "# c\r\nmcp_servers:\n  a: {command: x}\n\n\n";
        assert_eq!(doc(text).render().unwrap(), text);
    }

    #[test]
    fn parse_errors_do_not_quote_the_file() {
        let e = YamlDoc::parse(Some("key: [sk-ant-FAKEsecretvalue\n"), Path::new("c.yaml"))
            .unwrap_err();
        assert!(!format!("{e:#}").contains("FAKEsecret"), "{e:#}");
    }
}
