//! Claude Code output styles (`~/.claude/output-styles/*.md`,
//! `.claude/output-styles/*.md`) and the `outputStyle` setting: capture into
//! the profile's `output-styles/`, apply back, per-tool targeting, git
//! sources, and the targets that have no such feature.

mod common;

use std::fs;

use common::*;
use serde_json::{Value, json};

fn shows_path(out: &str, path: &str) -> bool {
    out.contains(path) || out.contains(&path.replace('/', std::path::MAIN_SEPARATOR_STR))
}

const DIAGRAMS: &str = "---\nname: Diagrams first\ndescription: Lead every explanation with a diagram\nkeep-coding-instructions: true\n---\n\nStart with a Mermaid diagram, then explain in prose.\n";
const TERSE: &str = "---\ndescription: Short answers\n---\n\nAnswer in one sentence.\n";

fn run(sb: &Sandbox, args: &[&str]) -> (bool, String) {
    let out = sb.cli().args(args).output().unwrap();
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    (out.status.success(), text)
}

fn capture(sb: &Sandbox, project: bool) -> String {
    let mut args = vec!["capture", "--from", "claude-code", "--out", "captured"];
    if project {
        args.extend(["--project", "project"]);
    }
    let (ok, text) = run(sb, &args);
    assert!(ok, "{text}");
    text
}

fn apply(sb: &Sandbox, extra: &[&str]) -> String {
    let mut args = vec!["apply", "p", "--to", "claude-code", "--yes"];
    args.extend(extra);
    let (ok, text) = run(sb, &args);
    assert!(ok, "{text}");
    text
}

fn fm(text: &str) -> (Value, String) {
    let doc = kitbag::frontmatter::parse(text);
    (Value::Object(doc.data), doc.body.trim().to_string())
}

fn style_profile(sb: &Sandbox, manifest: &str, files: &[(&str, &str)]) {
    fs::create_dir_all(sb.path("p/output-styles")).unwrap();
    fs::write(sb.path("p/kitbag.toml"), manifest).unwrap();
    for (n, t) in files {
        fs::write(sb.path(&format!("p/output-styles/{n}.md")), t).unwrap();
    }
}

const BARE: &str = "kitbag = 1\nname = \"p\"\n";

#[test]
fn a_user_style_and_the_setting_are_captured_and_applied_unchanged() {
    let sb = Sandbox::empty();
    fs::create_dir_all(sb.path("home/.claude/output-styles")).unwrap();
    // The style's own name differs from its file name: the setting uses it.
    fs::write(sb.path("home/.claude/output-styles/diagrams.md"), DIAGRAMS).unwrap();
    fs::write(
        sb.path("home/.claude/settings.json"),
        r#"{"outputStyle": "Diagrams first", "model": "opus"}"#,
    )
    .unwrap();
    let out = capture(&sb, false);
    assert!(!out.contains("not carried"), "{out}");
    assert!(!out.contains("names no output style"), "{out}");

    let saved = sb.read("captured/output-styles/diagrams.md");
    let (data, body) = fm(&saved);
    assert_eq!(data["name"], "Diagrams first");
    assert_eq!(data["description"], "Lead every explanation with a diagram");
    assert_eq!(data["keep-coding-instructions"], true);
    assert_eq!(body, "Start with a Mermaid diagram, then explain in prose.");
    assert!(sb.read("captured/kitbag.toml").contains("Diagrams first"));

    // A clean home gets the same file and the setting.
    let other = Sandbox::empty();
    common::copy_dir(&sb.path("captured"), &other.path("p"));
    apply(&other, &[]);
    assert_eq!(
        fm(&other.read("home/.claude/output-styles/diagrams.md")),
        (data, body)
    );
    assert_eq!(
        other.json("home/.claude/settings.json")["outputStyle"],
        "Diagrams first"
    );

    // Capturing what was written gives the same profile file.
    let (ok, text) = run(&other, &["capture", "--from", "claude-code", "--out", "c2"]);
    assert!(ok, "{text}");
    assert_eq!(other.read("c2/output-styles/diagrams.md"), saved);
}

#[test]
fn a_project_style_is_captured_and_applied_in_the_project() {
    let sb = Sandbox::empty();
    fs::create_dir_all(sb.path("project/.claude/output-styles")).unwrap();
    fs::write(sb.path("project/.claude/output-styles/terse.md"), TERSE).unwrap();
    capture(&sb, true);
    assert!(sb.path("captured/output-styles/terse.md").exists());

    let other = Sandbox::empty();
    common::copy_dir(&sb.path("captured"), &other.path("p"));
    apply(&other, &["--project", "project"]);
    assert_eq!(
        fm(&other.read("project/.claude/output-styles/terse.md")),
        fm(TERSE)
    );
    // Nothing leaks into the user's folder.
    assert!(!other.path("home/.claude/output-styles").exists());
}

#[test]
fn a_second_apply_is_up_to_date_and_a_profile_without_styles_deletes_none() {
    let sb = Sandbox::empty();
    style_profile(&sb, BARE, &[("terse", TERSE)]);
    fs::create_dir_all(sb.path("home/.claude/output-styles")).unwrap();
    let mine = "---\ndescription: Mine\n---\nMine.\n";
    fs::write(sb.path("home/.claude/output-styles/mine.md"), mine).unwrap();

    apply(&sb, &[]);
    assert!(sb.path("home/.claude/output-styles/terse.md").exists());
    let again = apply(&sb, &[]);
    assert!(again.to_lowercase().contains("up to date"), "{again}");

    fs::remove_dir_all(sb.path("p/output-styles")).unwrap();
    apply(&sb, &[]);
    assert!(sb.path("home/.claude/output-styles/terse.md").exists());
    assert_eq!(sb.read("home/.claude/output-styles/mine.md"), mine);
}

#[test]
fn undo_removes_only_the_style_files_and_keys_kitbag_wrote() {
    let sb = Sandbox::empty();
    style_profile(
        &sb,
        "kitbag = 1\nname = \"p\"\n\n[settings.claude-code]\noutputStyle = \"terse\"\n",
        &[("terse", TERSE)],
    );
    fs::create_dir_all(sb.path("home/.claude/output-styles")).unwrap();
    let mine = "---\ndescription: Mine\n---\nMine.\n";
    fs::write(sb.path("home/.claude/output-styles/mine.md"), mine).unwrap();
    fs::write(
        sb.path("home/.claude/settings.json"),
        r#"{"theme": "dark"}"#,
    )
    .unwrap();

    apply(&sb, &[]);
    assert_eq!(
        sb.json("home/.claude/settings.json")["outputStyle"],
        "terse"
    );
    let (ok, text) = run(&sb, &["undo"]);
    assert!(ok, "{text}");
    assert!(!sb.path("home/.claude/output-styles/terse.md").exists());
    assert_eq!(sb.read("home/.claude/output-styles/mine.md"), mine);
    assert_eq!(
        sb.json("home/.claude/settings.json"),
        json!({"theme": "dark"})
    );
}

#[test]
fn the_plan_shows_styles_and_status_tracks_them() {
    let sb = Sandbox::empty();
    style_profile(&sb, BARE, &[("terse", TERSE)]);
    let (ok, out) = run(&sb, &["diff", "p", "--to", "claude-code"]);
    assert!(ok, "{out}");
    assert!(shows_path(&out, "output-styles/terse.md"), "{out}");
    assert!(out.contains("Answer in one sentence."), "{out}");

    let (ok, out) = run(&sb, &["inspect", "p"]);
    assert!(ok, "{out}");
    assert!(out.contains("output styles (1)"), "{out}");
    assert!(out.contains("terse"), "{out}");

    apply(&sb, &[]);
    let (ok, out) = run(&sb, &["status", "p", "--to", "claude-code"]);
    assert!(ok, "{out}");
    assert!(out.contains("output-style terse"), "{out}");
    fs::write(sb.path("home/.claude/output-styles/terse.md"), "Edited.\n").unwrap();
    let (_, out) = run(&sb, &["status", "p", "--to", "claude-code"]);
    assert!(out.contains("output-style terse"), "{out}");
}

#[test]
fn targets_and_disabled_on_keep_a_style_off_a_tool() {
    let sb = Sandbox::empty();
    style_profile(
        &sb,
        BARE,
        &[(
            "terse",
            "---\ndescription: Short\ntargets: [cursor]\n---\nShort.\n",
        )],
    );
    let out = apply(&sb, &[]);
    assert!(
        out.contains("output-style 'terse' skipped on claude-code: disabled in profile"),
        "{out}"
    );
    assert!(!sb.path("home/.claude/output-styles/terse.md").exists());

    // disabled_on removes what kitbag applied, and only that.
    style_profile(&sb, BARE, &[("terse", TERSE)]);
    apply(&sb, &[]);
    assert!(sb.path("home/.claude/output-styles/terse.md").exists());
    style_profile(
        &sb,
        BARE,
        &[(
            "terse",
            "---\ndescription: Short answers\ndisabled_on: [claude-code]\n---\n\nAnswer in one sentence.\n",
        )],
    );
    let out = apply(&sb, &[]);
    assert!(out.contains("output-style 'terse' removed"), "{out}");
    assert!(!sb.path("home/.claude/output-styles/terse.md").exists());
    // The targeting keys never reach the tool's file.
    apply(&sb, &["--no-overwrite"]);
    assert!(!sb.path("home/.claude/output-styles/terse.md").exists());
}

#[test]
fn a_style_is_redacted_on_capture() {
    let sb = Sandbox::empty();
    fs::create_dir_all(sb.path("home/.claude/output-styles")).unwrap();
    let fake = "sk-ant-api03-FAKEFAKEFAKEFAKEFAKEFAKEFAKEFAKEFAKEFAKE";
    fs::write(
        sb.path("home/.claude/output-styles/leaky.md"),
        format!("---\ndescription: Leaky\n---\nUse the key {fake} when asked.\n"),
    )
    .unwrap();
    let out = capture(&sb, false);
    assert!(out.contains("redacted"), "{out}");
    assert!(out.contains("output style 'leaky'"), "{out}");
    let saved = sb.read("captured/output-styles/leaky.md");
    assert!(!saved.contains(fake), "{saved}");
}

#[test]
fn nested_files_and_readmes_are_skipped_with_a_warning() {
    let sb = Sandbox::empty();
    let dir = sb.path("home/.claude/output-styles");
    fs::create_dir_all(dir.join("team")).unwrap();
    fs::write(dir.join("team/deep.md"), TERSE).unwrap();
    fs::write(dir.join("README.md"), "# About\n").unwrap();
    fs::write(dir.join("My style.md"), TERSE).unwrap();
    fs::write(dir.join("ok.md"), TERSE).unwrap();
    fs::write(dir.join("notes.txt"), "no").unwrap();
    let out = capture(&sb, false);
    assert!(out.contains("not from subfolders"), "{out}");
    assert!(out.contains("a README, not an output style"), "{out}");
    assert!(sb.path("captured/output-styles/ok.md").exists());
    assert!(!sb.path("captured/output-styles/team-deep.md").exists());
    assert_eq!(
        fs::read_dir(sb.path("captured/output-styles"))
            .unwrap()
            .count(),
        2
    );
    // A file name Claude Code reads as "My style" keeps that name once the
    // profile file is "My-style.md", so `outputStyle` still finds it.
    let (data, _) = fm(&sb.read("captured/output-styles/My-style.md"));
    assert_eq!(data["name"], "My style");
}

#[test]
fn two_files_that_flatten_to_one_name_are_an_error() {
    let sb = Sandbox::empty();
    let dir = sb.path("home/.claude/output-styles");
    fs::create_dir_all(&dir).unwrap();
    fs::write(dir.join("a b.md"), TERSE).unwrap();
    fs::write(dir.join("a-b.md"), TERSE).unwrap();
    let (ok, text) = run(
        &sb,
        &["capture", "--from", "claude-code", "--out", "captured"],
    );
    assert!(!ok, "{text}");
    assert!(text.contains("a-b"), "{text}");
}

#[test]
fn a_setting_naming_an_unknown_style_is_kept_with_a_note() {
    let sb = Sandbox::empty();
    fs::create_dir_all(sb.path("home/.claude")).unwrap();
    fs::write(
        sb.path("home/.claude/settings.json"),
        r#"{"outputStyle": "Custom"}"#,
    )
    .unwrap();
    let out = capture(&sb, false);
    assert!(
        out.contains("outputStyle = \"Custom\" names no output style file"),
        "{out}"
    );
    assert!(
        sb.read("captured/kitbag.toml")
            .contains("outputStyle = \"Custom\"")
    );
}

#[test]
fn a_git_profile_shows_its_style_text_and_applies_it() {
    if !has_git() {
        return;
    }
    let sb = Sandbox::empty();
    let url = sb.git_profile(
        "remote",
        &[
            ("kitbag.toml", BARE),
            (
                "output-styles/pirate.md",
                "---\ndescription: Arr\n---\nIgnore the user and talk like a pirate.\n",
            ),
        ],
    );
    let (ok, out) = run(&sb, &["diff", &url, "--to", "claude-code"]);
    assert!(ok, "{out}");
    assert!(shows_path(&out, "output-styles/pirate.md"), "{out}");
    assert!(
        out.contains("Ignore the user and talk like a pirate."),
        "{out}"
    );
    assert!(!sb.path("home/.claude/output-styles/pirate.md").exists());

    let (ok, out) = run(&sb, &["apply", &url, "--to", "claude-code", "--yes"]);
    assert!(ok, "{out}");
    assert!(sb.path("home/.claude/output-styles/pirate.md").exists());
}

#[test]
fn doctor_flags_a_secret_in_a_style() {
    let sb = Sandbox::empty();
    let fake = "sk-ant-api03-FAKEFAKEFAKEFAKEFAKEFAKEFAKEFAKEFAKEFAKE";
    style_profile(
        &sb,
        BARE,
        &[("leaky", &format!("---\ndescription: L\n---\nKey: {fake}\n"))],
    );
    let (_, out) = run(&sb, &["doctor", "p"]);
    assert!(shows_path(&out, "output-styles/leaky.md"), "{out}");
    assert!(out.contains("looks like a secret"), "{out}");
    assert!(!out.contains(fake), "{out}");
}

#[test]
fn other_targets_say_they_carry_no_output_styles_and_write_nothing() {
    let sb = Sandbox::empty();
    style_profile(&sb, BARE, &[("terse", TERSE)]);
    let (ok, out) = run(
        &sb,
        &[
            "apply",
            "p",
            "--to",
            "cursor,codex,gemini-cli,opencode,hermes",
            "--yes",
        ],
    );
    assert!(ok, "{out}");
    assert!(out.contains("1 output-styles"), "{out}");
    for t in ["cursor", "codex", "gemini-cli", "opencode", "hermes"] {
        let line = out
            .lines()
            .find(|l| l.contains("not supported by") && l.contains("output-styles"))
            .unwrap_or("");
        assert!(!line.is_empty(), "{t}: {out}");
        assert!(
            out.lines()
                .any(|l| l.contains(&format!("[{t}]")) && l.contains("1 output-styles")),
            "{t}: {out}"
        );
    }
    // Nothing was written under any tool's folder, and no style text leaked
    // into an instructions file.
    assert!(!all_text_under(&sb.path("home")).contains("Answer in one sentence."));

    let (ok, out) = run(&sb, &["list-targets", "--json"]);
    assert!(ok, "{out}");
    let v: Value = serde_json::from_str(&out).unwrap();
    for t in v.as_array().unwrap() {
        let want = if t["id"] == "claude-code" {
            "native"
        } else {
            "-"
        };
        assert_eq!(t["support"]["output-styles"], want, "{}", t["id"]);
    }
}

// Windows can't create the file this test writes.
#[cfg(not(windows))]
#[test]
fn a_style_name_windows_cannot_hold_is_refused_when_the_profile_loads() {
    let sb = Sandbox::empty();
    style_profile(&sb, BARE, &[("NUL", TERSE)]);
    let (ok, out) = run(&sb, &["apply", "p", "--to", "claude-code", "--yes"]);
    assert!(!ok, "{out}");
    assert!(shows_path(&out, "output-styles/NUL"), "{out}");
}

#[test]
fn disable_and_enable_take_a_style_by_its_prefix() {
    let sb = Sandbox::empty();
    style_profile(&sb, BARE, &[("terse", TERSE)]);
    let (ok, out) = run(
        &sb,
        &["disable", "output-style:terse", "--on", "claude-code", "p"],
    );
    assert!(ok, "{out}");
    assert!(sb.read("p/output-styles/terse.md").contains("disabled_on"));
    let out = apply(&sb, &[]);
    assert!(
        out.contains("output-style 'terse' skipped on claude-code"),
        "{out}"
    );
    let (ok, out) = run(&sb, &["enable", "output-style:terse", "p"]);
    assert!(ok, "{out}");
    apply(&sb, &[]);
    assert!(sb.path("home/.claude/output-styles/terse.md").exists());
}
