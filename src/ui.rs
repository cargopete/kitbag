//! Terminal output helpers: styles, notes, paths and diffs.

use std::collections::BTreeSet;
use std::path::Path;

use anstyle::{AnsiColor, Style};
use similar::{ChangeTag, TextDiff};

use crate::adapters::{Level, Note};
use crate::plan_fs::{ChangeKind, FileChange};
use crate::secrets::mask;

pub const BOLD: Style = Style::new().bold();
pub const DIM: Style = Style::new().dimmed();
pub const GREEN: Style = AnsiColor::Green.on_default();
pub const RED: Style = AnsiColor::Red.on_default();
pub const YELLOW: Style = AnsiColor::Yellow.on_default();
pub const CYAN: Style = AnsiColor::Cyan.on_default();

/// Wrap `text` in `style` (stripped automatically when output is not a terminal).
pub fn paint(style: Style, text: impl std::fmt::Display) -> String {
    format!("{style}{text}{style:#}")
}

/// `~/...` for paths under `home`.
pub fn tildify(path: &Path, home: &Path) -> String {
    match path.strip_prefix(home) {
        Ok(rest) if !home.as_os_str().is_empty() => {
            let rest = rest.display().to_string();
            if rest.is_empty() {
                "~".to_string()
            } else {
                format!("~{}{rest}", std::path::MAIN_SEPARATOR)
            }
        }
        _ => path.display().to_string(),
    }
}

pub fn format_note(n: &Note) -> String {
    let (label, style) = match n.level {
        Level::Warn => ("warn", YELLOW),
        Level::Info => ("note", CYAN),
    };
    format!(
        "  {} {} {}",
        paint(style, label),
        paint(DIM, format!("[{}]", n.target)),
        n.message
    )
}

/// Sorted: warnings first, then by target.
pub fn sorted_notes(notes: &[Note]) -> Vec<&Note> {
    let mut v: Vec<&Note> = notes.iter().collect();
    v.sort_by(|a, b| b.level.cmp(&a.level).then_with(|| a.target.cmp(&b.target)));
    v
}

pub fn change_label(c: &FileChange) -> String {
    match c.kind() {
        ChangeKind::Create => paint(GREEN, "create"),
        ChangeKind::Modify => paint(YELLOW, "modify"),
        ChangeKind::Delete => paint(RED, "delete"),
    }
}

fn text_of(b: Option<&Vec<u8>>) -> Option<String> {
    b.map(|b| String::from_utf8(b.clone()).ok())
        .unwrap_or(Some(String::new()))
}

/// `(+added, -removed)` line counts.
pub fn line_stats(c: &FileChange) -> Option<(usize, usize)> {
    let before = text_of(c.before.as_ref())?;
    let after = text_of(c.after.as_ref())?;
    let diff = TextDiff::from_lines(&before, &after);
    let mut add = 0;
    let mut del = 0;
    for ch in diff.iter_all_changes() {
        match ch.tag() {
            ChangeTag::Insert => add += 1,
            ChangeTag::Delete => del += 1,
            ChangeTag::Equal => {}
        }
    }
    Some((add, del))
}

/// A unified diff of one change, with secrets masked. Long diffs are truncated.
pub fn render_diff(
    c: &FileChange,
    display_path: &str,
    sensitive: &BTreeSet<String>,
    max_lines: usize,
) -> String {
    let (Some(before), Some(after)) = (text_of(c.before.as_ref()), text_of(c.after.as_ref()))
    else {
        let size = c.after.as_ref().map_or(0, Vec::len);
        return format!(
            "{}\n  (binary file, {size} bytes)\n",
            paint(BOLD, display_path)
        );
    };
    let before = mask(&before, sensitive);
    let after = mask(&after, sensitive);
    let diff = TextDiff::from_lines(&before, &after);
    let mut out = format!(
        "{}\n{}\n",
        paint(
            BOLD,
            format!(
                "--- {}",
                if c.before.is_some() {
                    display_path
                } else {
                    "/dev/null"
                }
            )
        ),
        paint(BOLD, format!("+++ {display_path}"))
    );
    let mut lines = 0;
    for group in diff.grouped_ops(3) {
        for op in group {
            for ch in diff.iter_changes(&op) {
                if lines >= max_lines {
                    out.push_str(&paint(
                        DIM,
                        "  ... (diff truncated; use 'kitbag diff' to see everything)\n",
                    ));
                    return out;
                }
                let text = ch.value().trim_end_matches('\n');
                let line = match ch.tag() {
                    ChangeTag::Insert => paint(GREEN, format!("+{text}")),
                    ChangeTag::Delete => paint(RED, format!("-{text}")),
                    ChangeTag::Equal => paint(DIM, format!(" {text}")),
                };
                out.push_str(&line);
                out.push('\n');
                lines += 1;
            }
        }
        out.push_str(&paint(DIM, "  ...\n"));
    }
    out
}
