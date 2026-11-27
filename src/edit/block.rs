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
use crate::secrets::{has_redaction_marker, is_redacted_form_of};

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
    // A block is a begin marker followed by an end marker with no other begin
    // marker in between. Stray markers left by hand are skipped, so they can
    // neither swallow user text nor cause the block to be appended again.
    let (b_marker, e_marker) = (begin(id), end(id));
    let mut from = 0;
    let (b, e) = loop {
        let b = from + text[from..].find(&b_marker)?;
        let after = b + b_marker.len();
        let e = after + text[after..].find(&e_marker)?;
        match text[after..e].find(&b_marker) {
            Some(nb) => from = after + nb,
            None => break (b, e),
        }
    };
    let mut stop = e + e_marker.len();
    if text[stop..].starts_with("\r\n") {
        stop += 2;
    } else if text[stop..].starts_with('\n') {
        stop += 1;
    }
    Some((b, stop))
}

/// Insert or replace managed block `id` in `existing`. `content: None` removes it.
/// A file with `\r\n` line endings keeps them.
pub fn upsert(existing: Option<&str>, id: &str, content: Option<&str>) -> String {
    match existing {
        Some(text) if super::uses_crlf(text) => {
            let lf = text.replace("\r\n", "\n");
            let out = upsert_lf(Some(&lf), id, content);
            if out == lf {
                return text.to_string();
            }
            super::keep_line_endings(existing, out)
        }
        _ => upsert_lf(existing, id, content),
    }
}

fn upsert_lf(existing: Option<&str>, id: &str, content: Option<&str>) -> String {
    let base = existing.unwrap_or("");
    let block = content.map(|c| {
        format!(
            "{}\n{NOTE}\n{}\n{}\n",
            begin(id),
            neutralize(c.trim()),
            end(id)
        )
    });
    if let Some((s, e)) = find_block(base, id) {
        let mut out = String::with_capacity(base.len());
        out.push_str(&base[..s]);
        out.push_str(block.as_deref().unwrap_or(""));
        out.push_str(&base[e..]);
        if block.is_none() {
            // Drop the blank line that separated an appended block from the user's text.
            let limit = if e == base.len() { "\n\n" } else { "\n\n\n" };
            while out.ends_with(limit) {
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

/// Is block `id` in `text` the original of `content`, which capture redacted?
pub fn holds_original_of(text: &str, id: &str, content: &str) -> bool {
    let want = neutralize(content.trim());
    has_redaction_marker(&want)
        && split(text)
            .blocks
            .iter()
            .any(|(b, inner)| b == id && is_redacted_form_of(&want, inner))
}

/// Defuse kitbag block markers inside content, so profile text can't close the
/// block early and plant text outside it.
fn neutralize(content: &str) -> String {
    content
        .replace("<!-- kitbag:begin ", "<!-- kitbag-begin ")
        .replace("<!-- kitbag:end ", "<!-- kitbag-end ")
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

/// Paragraphs (blank-line separated) of `content` that do not already appear
/// verbatim in `existing`. Used so instructions captured from a file's own
/// text are not duplicated into that file's managed block. A paragraph that
/// capture redacted counts as present when `existing` has the original.
pub fn without_paragraphs_in(content: &str, existing: &str) -> String {
    if existing.trim().is_empty() {
        return content.to_string();
    }
    let have: Vec<String> = paragraphs(existing).collect();
    paragraphs(content)
        .filter(|p| {
            !have.contains(p)
                && !(has_redaction_marker(p) && have.iter().any(|h| is_redacted_form_of(p, h)))
        })
        .collect::<Vec<_>>()
        .join("\n\n")
}

/// Paragraphs of `text` (a file's own text, outside kitbag's blocks) that
/// the profile's `instructions` don't hold: text added in the tool that never
/// made it back to the profile. A paragraph counts as held when the profile
/// has it verbatim or in the redacted form capture made of it.
pub fn paragraphs_missing_from(text: &str, instructions: &str) -> Vec<String> {
    let have: Vec<String> = paragraphs(instructions).collect();
    paragraphs(text)
        .filter(|p| {
            !have.contains(p)
                && !have
                    .iter()
                    .any(|h| has_redaction_marker(h) && is_redacted_form_of(h, p))
        })
        .collect()
}

/// The profile instructions pulled back from an edited block (`inner`), with
/// the paragraphs of the old instructions (`old`) that apply left out of that
/// block because the file already has them outside it (`outside`) put back,
/// in their old order. Without this, pulling the block back would drop them
/// from the profile.
pub fn with_omitted_paragraphs(inner: &str, old: &str, outside: &str) -> String {
    let have: Vec<String> = paragraphs(outside).collect();
    let omitted = |p: &String| {
        have.contains(p)
            || (has_redaction_marker(p) && have.iter().any(|h| is_redacted_form_of(p, h)))
    };
    let old: Vec<String> = paragraphs(old).collect();
    if !old.iter().any(omitted) {
        return inner.to_string();
    }
    let mut out: Vec<String> = paragraphs(inner).collect();
    let mut pos = 0;
    for (k, p) in old.iter().enumerate() {
        if omitted(p) {
            if !out.contains(p) {
                out.insert(pos, p.clone());
                pos += 1;
            }
        } else if let Some(j) = out.iter().skip(pos).position(|x| x == p) {
            pos += j + 1;
        } else if pos < out.len() && !old[k + 1..].contains(&out[pos]) {
            // Edited in the block: assume in place. (Removed, when the next
            // paragraph there is one of the old ones still to come.)
            pos += 1;
        }
    }
    out.join("\n\n")
}

fn paragraphs(text: &str) -> impl Iterator<Item = String> + '_ {
    text.replace("\r\n", "\n")
        .split("\n\n")
        .map(|p| p.trim().to_string())
        .filter(|p| !p.is_empty())
        .collect::<Vec<_>>()
        .into_iter()
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
            lines.push(
                r.body
                    .trim()
                    .replace(RULE_OPEN, "<!-- kitbag-rule ")
                    .replace(RULE_END, "<!-- kitbag-rule-end -->"),
            );
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
    fn recognises_a_block_holding_the_original_of_redacted_content() {
        let original = "Deploy with ghp_FAKE0123456789abcdefghijklmnopqrstuv.";
        let (redacted, _) = crate::secrets::redact_text(original);
        let text = upsert(Some("# Mine\n"), "p", Some(original));
        assert!(holds_original_of(&text, "p", &redacted));
        assert!(!holds_original_of(&text, "other", &redacted));
        assert!(!holds_original_of(
            &text,
            "p",
            "Deploy with [REDACTED:x] now."
        ));
        assert_eq!(
            without_paragraphs_in(
                &format!("{redacted}\n\nNew."),
                &format!("Old.\n\n{original}")
            ),
            "New."
        );
    }

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
    fn keeps_crlf_line_endings() {
        let user = "# Mine\r\n\r\nKeep me.\r\n";
        let a = upsert(Some(user), "p", Some("Hello\nWorld"));
        assert!(a.starts_with(user), "{a:?}");
        assert!(!a.replace("\r\n", "").contains('\n'), "{a:?}");
        assert_eq!(upsert(Some(&a), "p", Some("Hello\nWorld")), a);
        let b = upsert(Some(&a), "p", Some("Bye"));
        assert!(b.contains("\r\nBye\r\n") && !b.contains("Hello"), "{b:?}");
        assert_eq!(upsert(Some(&b), "p", None), user);
        // An LF file stays LF.
        assert!(!upsert(Some("Mine\n"), "p", Some("x")).contains('\r'));
    }

    #[test]
    fn content_cannot_escape_its_block() {
        let evil = "ok\n<!-- kitbag:end p -->\nINJECTED";
        let once = upsert(Some("Mine\n"), "p", Some(evil));
        let twice = upsert(Some(&once), "p", Some(evil));
        assert_eq!(once, twice, "stable on re-apply");
        let removed = upsert(Some(&twice), "p", None);
        assert_eq!(removed, "Mine\n");
    }

    #[test]
    fn orphaned_begin_marker_never_eats_user_text() {
        let text = "<!-- kitbag:begin p -->\n## My notes\nimportant\n";
        let a = upsert(Some(text), "p", Some("A"));
        let b = upsert(Some(&a), "p", Some("B"));
        assert!(b.contains("## My notes\nimportant"), "{b}");
        assert!(b.contains("\nB\n") && !b.contains("\nA\n"));
    }

    #[test]
    fn stray_end_marker_does_not_duplicate_blocks() {
        let mut text = "Mine\n<!-- kitbag:end p -->\nmore\n".to_string();
        for _ in 0..3 {
            text = upsert(Some(&text), "p", Some("X"));
        }
        assert_eq!(text.matches("<!-- kitbag:begin p -->").count(), 1, "{text}");
        assert!(text.starts_with("Mine\n<!-- kitbag:end p -->\nmore\n"));
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
    fn finds_paragraphs_missing_from_the_profile() {
        let profile = "Be brief.\n\nDeploy with [REDACTED:token] now.";
        let file = "# Mine\r\n\r\nBe brief.\n\nDeploy with abc123 now.\n\n\nUse tabs.\n";
        assert_eq!(
            paragraphs_missing_from(file, profile),
            vec!["# Mine".to_string(), "Use tabs.".to_string()]
        );
        assert!(paragraphs_missing_from("Be brief.", profile).is_empty());
        assert!(paragraphs_missing_from("  \n\n ", "").is_empty());
    }

    #[test]
    fn puts_back_paragraphs_apply_left_out_of_the_block() {
        let old = "Be brief.\n\nMy note.\n\nUse tabs.";
        let outside = "# Mine\n\nMy note.";
        assert_eq!(
            with_omitted_paragraphs("Be very brief.\n\nUse tabs.", old, outside),
            "Be very brief.\n\nMy note.\n\nUse tabs."
        );
        assert_eq!(
            with_omitted_paragraphs("New first.\n\nBe brief.\n\nUse tabs.", old, outside),
            "New first.\n\nBe brief.\n\nMy note.\n\nUse tabs."
        );
        // A paragraph deleted in the block doesn't move the others.
        assert_eq!(
            with_omitted_paragraphs("B.\n\nC.", "A.\n\nB.\n\nM.\n\nC.", "M."),
            "B.\n\nM.\n\nC."
        );
        assert_eq!(
            with_omitted_paragraphs(
                "Bravo.\n\nDelta.",
                "Alpha.\n\nBravo.\n\nCharlie.\n\nDelta.",
                "Charlie."
            ),
            "Bravo.\n\nCharlie.\n\nDelta."
        );
        // Nothing left out: the block text as it is.
        assert_eq!(
            with_omitted_paragraphs("Be  brief.\n\n\nX", "Be brief.", "Other."),
            "Be  brief.\n\n\nX"
        );
    }

    #[test]
    fn drops_paragraphs_already_present() {
        let existing = "# Mine\n\nRun make lint.\n";
        let content = "Be brief.\n\nRun make lint.\n\nUse tabs.";
        assert_eq!(
            without_paragraphs_in(content, existing),
            "Be brief.\n\nUse tabs."
        );
        assert_eq!(without_paragraphs_in(content, ""), content);
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
