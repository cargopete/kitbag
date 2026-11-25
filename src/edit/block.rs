//! Managed blocks inside files the user also edits (CLAUDE.md, AGENTS.md, GEMINI.md).
//!
//! kitbag owns only the text between its markers. Everything outside them is
//! left byte-for-byte untouched, and re-applying replaces the block in place:
//!
//! ```text
//! <!-- kitbag:begin my-setup -->
//! <!-- Managed by kitbag. Edit the profile and re-apply instead of editing here. -->
//! ...
//! <!-- kitbag:end my-setup -->
//! ```
//!
//! Tools with a single instructions file get rules folded in as sections, each
//! wrapped in a `kitbag:rule` marker so `capture` can split them out losslessly.

use serde_json::{Map, Value, json};

use crate::model::{Rule, RuleMode, is_valid_name};

const NOTE: &str =
    "<!-- Managed by kitbag. Edit the profile and re-apply instead of editing here. -->";
const BEGIN_PREFIX: &str = "<!-- kitbag:begin ";
const MARKER_SUFFIX: &str = " -->";

fn begin(id: &str) -> String {
    format!("{BEGIN_PREFIX}{id}{MARKER_SUFFIX}")
}
fn end(id: &str) -> String {
    format!("<!-- kitbag:end {id}{MARKER_SUFFIX}")
}

/// Byte range of block `id` in `text`, including a trailing newline.
fn find_block(text: &str, id: &str) -> Option<(usize, usize)> {
    let b = text.find(&begin(id))?;
    let e_marker = end(id);
    let e = b + text[b..].find(&e_marker)?;
    let mut stop = e + e_marker.len();
    if text[stop..].starts_with("\r\n") {
        stop += 2;
    } else if text[stop..].starts_with('\n') {
        stop += 1;
    }
    Some((b, stop))
}

/// Insert or replace managed block `id` in `existing`. `content: None` removes it.
pub fn upsert(existing: Option<&str>, id: &str, content: Option<&str>) -> String {
    let base = existing.unwrap_or("");
    let block = content.map(|c| format!("{}\n{NOTE}\n{}\n{}\n", begin(id), c.trim(), end(id)));
    if let Some((s, e)) = find_block(base, id) {
        let mut out = String::with_capacity(base.len());
        out.push_str(&base[..s]);
        out.push_str(block.as_deref().unwrap_or(""));
        out.push_str(&base[e..]);
        if block.is_none() {
            while out.ends_with("\n\n\n") {
                out.pop();
            }
            if out.trim().is_empty() {
                return String::new();
            }
        }
        return out;
    }
    let Some(block) = block else {
        return base.to_string();
    };
    if base.trim().is_empty() {
        return block;
    }
    let sep = if base.ends_with("\n\n") {
        ""
    } else if base.ends_with('\n') {
        "\n"
    } else {
        "\n\n"
    };
    format!("{base}{sep}{block}")
}

/// Content outside any kitbag block, plus each block's inner content by id.
#[derive(Debug, Default, PartialEq)]
pub struct Split {
    pub outside: String,
    pub blocks: Vec<(String, String)>,
}

pub fn split(text: &str) -> Split {
    let mut out = Split::default();
    let mut rest = text;
    let mut outside = String::new();
    while let Some(b) = rest.find(BEGIN_PREFIX) {
        let after = &rest[b + BEGIN_PREFIX.len()..];
        let Some(id_end) = after.find(MARKER_SUFFIX) else {
            break;
        };
        let id = after[..id_end].to_string();
        let Some((s, e)) = find_block(&rest[b..], &id) else {
            break;
        };
        let (s, e) = (b + s, b + e);
        outside.push_str(&rest[..s]);
        let inner_start = s + begin(&id).len();
        let inner_end = rest[..e].rfind(&end(&id)).unwrap_or(e);
        let inner = rest[inner_start..inner_end].replace(NOTE, "");
        out.blocks.push((id, inner.trim().to_string()));
        rest = &rest[e..];
    }
    outside.push_str(rest);
    out.outside = collapse_blank_lines(&outside).trim().to_string();
    out
}

fn collapse_blank_lines(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut newlines = 0;
    for ch in s.chars() {
        if ch == '\n' {
            newlines += 1;
            if newlines > 2 {
                continue;
            }
        } else if ch != '\r' {
            newlines = 0;
        }
        out.push(ch);
    }
    out
}

const RULE_OPEN: &str = "<!-- kitbag:rule ";
const RULE_END: &str = "<!-- kitbag:rule-end -->";

/// Render rules as one markdown document for tools with a single instructions file.
pub fn fold_rules(rules: &[Rule]) -> Option<String> {
    if rules.is_empty() {
        return None;
    }
    let parts: Vec<String> = rules
        .iter()
        .map(|r| {
            let mut meta = Map::new();
            meta.insert("name".into(), json!(r.name));
            if let Some(d) = &r.description {
                meta.insert("description".into(), json!(d));
            }
            if !r.globs.is_empty() {
                meta.insert("globs".into(), json!(r.globs));
            }
            if let Some(a) = r.always_apply {
                meta.insert("alwaysApply".into(), json!(a));
            }
            let meta = serde_json::to_string(&Value::Object(meta))
                .expect("serializable")
                .replace("--", "-\\u002d");
            let mut lines = vec![
                format!("{RULE_OPEN}{meta} -->"),
                format!("## {}", r.description.as_deref().unwrap_or(&r.name)),
            ];
            match r.mode() {
                RuleMode::Globs => lines.push(format!(
                    "_Applies when working with files matching: {}_",
                    r.globs
                        .iter()
                        .map(|g| format!("`{g}`"))
                        .collect::<Vec<_>>()
                        .join(", ")
                )),
                RuleMode::OnDemand => lines.push("_Use when relevant to the task._".to_string()),
                RuleMode::Always => {}
            }
            lines.push(String::new());
            lines.push(r.body.trim().to_string());
            lines.push(RULE_END.to_string());
            lines.join("\n")
        })
        .collect();
    Some(parts.join("\n\n"))
}

/// Recover folded rules from `text`. Returns the remaining text and the rules.
pub fn unfold_rules(text: &str) -> (String, Vec<Rule>) {
    let mut rules = Vec::new();
    let mut rest = text;
    let mut remaining = String::new();
    while let Some(start) = rest.find(RULE_OPEN) {
        let after = &rest[start + RULE_OPEN.len()..];
        let (Some(meta_end), Some(close)) = (after.find(MARKER_SUFFIX), after.find(RULE_END))
        else {
            break;
        };
        if meta_end > close {
            break;
        }
        remaining.push_str(&rest[..start]);
        let meta: Option<Map<String, Value>> = serde_json::from_str(&after[..meta_end]).ok();
        let inner = &after[meta_end + MARKER_SUFFIX.len()..close];
        if let Some(meta) = meta
            && let Some(name) = meta.get("name").and_then(Value::as_str)
            && is_valid_name(name)
        {
            let mut lines: Vec<&str> = inner.trim_start_matches(['\r', '\n']).lines().collect();
            if lines.first().is_some_and(|l| l.starts_with("## ")) {
                lines.remove(0);
            }
            if lines
                .first()
                .is_some_and(|l| l.starts_with("_Applies when") || l.starts_with("_Use when"))
            {
                lines.remove(0);
            }
            rules.push(Rule {
                name: name.to_string(),
                description: meta
                    .get("description")
                    .and_then(Value::as_str)
                    .map(str::to_string),
                globs: meta
                    .get("globs")
                    .and_then(Value::as_array)
                    .map(|a| {
                        a.iter()
                            .filter_map(|g| g.as_str().map(str::to_string))
                            .collect()
                    })
                    .unwrap_or_default(),
                always_apply: meta.get("alwaysApply").and_then(Value::as_bool),
                body: lines.join("\n").trim().to_string(),
            });
        } else {
            remaining.push_str(inner);
        }
        rest = &after[close + RULE_END.len()..];
    }
    remaining.push_str(rest);
    (collapse_blank_lines(&remaining).trim().to_string(), rules)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn upsert_appends_replaces_and_removes() {
        let user = "# Mine\n\nKeep me.\n";
        let a = upsert(Some(user), "p", Some("Hello"));
        assert!(a.starts_with(user));
        assert!(a.contains("<!-- kitbag:begin p -->\n"));
        let b = upsert(Some(&a), "p", Some("Bye"));
        assert!(b.contains("Bye") && !b.contains("Hello"));
        assert_eq!(b.matches("kitbag:begin").count(), 1);
        assert_eq!(upsert(Some(&b), "p", Some("Bye")), b);
        let c = upsert(Some(&b), "p", None);
        assert_eq!(c.trim_end(), user.trim_end());
        assert_eq!(
            upsert(None, "p", Some("x")),
            format!("{}\n{NOTE}\nx\n{}\n", begin("p"), end("p"))
        );
    }

    #[test]
    fn split_separates_user_text() {
        let t = upsert(Some("Mine\n"), "a", Some("A body"));
        let t = upsert(Some(&t), "a:rules", Some("R body"));
        let s = split(&t);
        assert_eq!(s.outside, "Mine");
        assert_eq!(
            s.blocks,
            vec![
                ("a".to_string(), "A body".to_string()),
                ("a:rules".to_string(), "R body".to_string())
            ]
        );
    }

    #[test]
    fn fold_round_trip() {
        let rules = vec![
            Rule {
                name: "ts".into(),
                description: Some("TypeScript".into()),
                globs: vec!["src/**/*.ts".into()],
                always_apply: None,
                body: "Strict mode.\n\nNo any.".into(),
            },
            Rule {
                name: "style".into(),
                body: "Be brief.".into(),
                ..Default::default()
            },
        ];
        let folded = fold_rules(&rules).unwrap();
        assert!(folded.contains("_Applies when working with files matching: `src/**/*.ts`_"));
        let (rest, back) = unfold_rules(&format!("Intro\n\n{folded}\n"));
        assert_eq!(rest, "Intro");
        assert_eq!(back, rules);
    }
}
