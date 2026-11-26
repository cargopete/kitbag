//! Terminal output helpers: styles, notes, paths and diffs.

use std::borrow::Cow;
use std::collections::BTreeSet;
use std::fmt::Write as _;
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

/// Text from a profile or a tool's config, made safe to print: control
/// characters and bidi overrides are shown as escapes (`\x1b`, `\r`,
/// `\u{202e}`), so they can't move the cursor, rewrite earlier lines or
/// reorder what the user reads. Newlines and tabs are kept. Everything kitbag
/// prints that it didn't write itself goes through here.
pub fn visible(text: &str) -> Cow<'_, str> {
    escape(text, true)
}

/// [`visible`] for one-line values (names, args, paths): newlines are escaped
/// too, so a value can't fake a line of its own.
pub fn visible_line(text: &str) -> Cow<'_, str> {
    escape(text, false)
}

fn escape(text: &str, keep_newlines: bool) -> Cow<'_, str> {
    let unsafe_char = |c: char| match c {
        '\t' => false,
        '\n' => !keep_newlines,
        _ => {
            c.is_control()
                || matches!(c,
                    '\u{061c}' | '\u{200e}' | '\u{200f}' | '\u{2028}' | '\u{2029}'
                    | '\u{202a}'..='\u{202e}' | '\u{2066}'..='\u{2069}')
        }
    };
    if !text.chars().any(unsafe_char) {
        return Cow::Borrowed(text);
    }
    let mut out = String::with_capacity(text.len() + 8);
    for c in text.chars() {
        if !unsafe_char(c) {
            out.push(c);
            continue;
        }
        match c {
            '\r' => out.push_str("\\r"),
            '\n' => out.push_str("\\n"),
            c if (c as u32) < 0x80 => {
                let _ = write!(out, "\\x{:02x}", c as u32);
            }
            c => {
                let _ = write!(out, "\\u{{{:x}}}", c as u32);
            }
        }
    }
    Cow::Owned(out)
}

/// `~/...` for paths under `home`, ready to print.
pub fn tildify(path: &Path, home: &Path) -> String {
    let shown = match path.strip_prefix(home) {
        Ok(rest) if !home.as_os_str().is_empty() => {
            let rest = rest.display().to_string();
            if rest.is_empty() {
                "~".to_string()
            } else {
                format!("~{}{rest}", std::path::MAIN_SEPARATOR)
            }
        }
        _ => path.display().to_string(),
    };
    visible_line(&shown).into_owned()
}

pub fn format_note(n: &Note) -> String {
    let (label, style) = match n.level {
        Level::Warn => ("warn", YELLOW),
        Level::Info => ("note", CYAN),
    };
    format!(
        "  {} {} {}",
        paint(style, label),
        paint(DIM, format!("[{}]", visible_line(&n.target))),
        visible_line(&n.message)
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

/// Whether a change keeps the content and only sets the executable bit.
pub fn mode_only(c: &FileChange) -> bool {
    c.before.is_some() && c.before == c.after
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
    if mode_only(c) {
        return format!(
            "{}\n  (content unchanged; the executable bit is set again)\n",
            paint(BOLD, visible_line(display_path))
        );
    }
    let (Some(before), Some(after)) = (text_of(c.before.as_ref()), text_of(c.after.as_ref()))
    else {
        let size = c.after.as_ref().map_or(0, Vec::len);
        return format!(
            "{}\n  (binary file, {size} bytes)\n",
            paint(BOLD, visible_line(display_path))
        );
    };
    // Private keys span lines, so they are masked before the text is split.
    let before = crate::secrets::mask_key_blocks(&mask(&before, sensitive));
    let after = crate::secrets::mask_key_blocks(&mask(&after, sensitive));
    let diff = TextDiff::from_lines(&before, &after);
    let display_path = visible_line(display_path);
    let display_path = display_path.as_ref();
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
    for (i, group) in diff.grouped_ops(3).into_iter().enumerate() {
        if i > 0 {
            out.push_str(&paint(DIM, "  ..."));
            out.push('\n');
        }
        for op in group {
            for ch in diff.iter_changes(&op) {
                if lines >= max_lines {
                    out.push_str(&paint(
                        DIM,
                        "  ... (diff truncated; use 'kitbag diff' to see everything)",
                    ));
                    out.push('\n');
                    return out;
                }
                // The line ending isn't shown; a lone `\r` that ends a line is.
                let value = ch.value();
                let value = value
                    .strip_suffix("\r\n")
                    .or_else(|| value.strip_suffix('\n'))
                    .unwrap_or(value);
                let text = crate::secrets::mask_line(value);
                let text = visible_line(&text);
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
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn control_characters_are_shown_not_sent() {
        assert!(matches!(visible("plain\ttext\nok"), Cow::Borrowed(_)));
        assert_eq!(
            visible("a\u{1b}[2K\u{1b}[1A\rb\u{7}\u{7f}\u{9b}c\u{202e}d\u{2066}\n\te"),
            "a\\x1b[2K\\x1b[1A\\rb\\x07\\x7f\\u{9b}c\\u{202e}d\\u{2066}\n\te"
        );
        assert_eq!(visible_line("one\ntwo\tthree"), "one\\ntwo\tthree");
        // Escaping is idempotent and leaves ordinary text alone.
        let once = visible_line("x\u{1b}y").into_owned();
        assert_eq!(visible_line(&once), once);
        assert_eq!(visible("C:\\Users\\me ✓ ü"), "C:\\Users\\me ✓ ü");
    }
}
