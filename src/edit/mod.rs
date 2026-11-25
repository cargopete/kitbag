//! Editors that change only what kitbag manages and leave the rest of a file alone.

pub mod block;
pub mod json;
pub mod toml;

/// A parse error that is safe to print: where it is (line and column, from a
/// byte offset into `text`) and the parser's message, but never the source
/// text itself, which can hold secrets. Quoted strings in the message (serde
/// quotes the offending value) are elided and anything else that looks like
/// a secret is redacted.
pub fn safe_parse_error(text: &str, offset: Option<usize>, message: &str) -> String {
    use std::sync::LazyLock;
    static QUOTED: LazyLock<regex::Regex> =
        LazyLock::new(|| regex::Regex::new(r#""(?:[^"\\]|\\.)*"|'[^'\n]*'"#).expect("valid regex"));
    let message = message.lines().next().unwrap_or("").trim();
    let message = QUOTED.replace_all(message, "\"…\"");
    let (message, _) = crate::secrets::redact_text(&message);
    match offset {
        Some(at) => {
            let (line, col) = line_col(text, at);
            format!("line {line}, column {col}: {message}")
        }
        None => message,
    }
}

/// 1-based line and column (in characters) of byte offset `at` in `text`.
pub fn line_col(text: &str, at: usize) -> (usize, usize) {
    let mut at = at.min(text.len());
    while !text.is_char_boundary(at) {
        at -= 1;
    }
    let before = &text[..at];
    let line = before.matches('\n').count() + 1;
    let col = before.rsplit('\n').next().unwrap_or("").chars().count() + 1;
    (line, col)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_errors_carry_no_source_text() {
        assert_eq!(line_col("ab\ncd", 4), (2, 2));
        assert_eq!(line_col("é", 1), (1, 1));
        let msg = safe_parse_error(
            "a\nb",
            Some(2),
            "invalid type: string \"hunter2hunter2\", expected i64\n  |\n2 | b",
        );
        assert_eq!(
            msg,
            "line 2, column 1: invalid type: string \"…\", expected i64"
        );
    }
}
