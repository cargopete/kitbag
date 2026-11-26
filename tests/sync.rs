//! `kitbag status` and `kitbag sync`: drift detection for the instructions and
//! rules kitbag manages, and syncing them without losing hand edits.

mod common;

use std::fs;

use common::*;
use predicates::prelude::*;
use predicates::str::contains;

const MANIFEST: &str = "kitbag = 1\nname = \"me\"\n";
const RULE: &str = "---\ndescription: Rust\nglobs:\n  - \"**/*.rs\"\n---\n\nUse `?`.\n";

/// A local profile `p` with instructions and one rule, applied to Claude Code
/// and Codex at user scope.
fn applied() -> Sandbox {
    let sb = Sandbox::empty();
    fs::create_dir_all(sb.path("p/rules")).unwrap();
    fs::write(sb.path("p/kitbag.toml"), MANIFEST).unwrap();
    fs::write(sb.path("p/instructions.md"), "Be brief.\n").unwrap();
    fs::write(sb.path("p/rules/rust.md"), RULE).unwrap();
    sb.cli()
        .args(["apply", "p", "--to", "claude-code,codex", "--yes"])
        .assert()
        .success();
    sb
}

/// Replace `from` with `to` inside a file of the sandbox.
fn edit(sb: &Sandbox, rel: &str, from: &str, to: &str) {
    let text = sb.read(rel);
    assert!(text.contains(from), "{rel} has no {from:?}:\n{text}");
    fs::write(sb.path(rel), text.replace(from, to)).unwrap();
}

/// `kitbag status --json` rows as (target, item, state).
fn status(sb: &Sandbox, args: &[&str]) -> Vec<(String, String, String)> {
    let out = sb
        .cli()
        .arg("status")
        .args(args)
        .arg("--json")
        .output()
        .unwrap();
    let v: serde_json::Value = serde_json::from_slice(&out.stdout)
        .unwrap_or_else(|e| panic!("{e}: {}", String::from_utf8_lossy(&out.stdout)));
    v.as_array()
        .unwrap()
        .iter()
        .map(|r| {
            (
                r["target"].as_str().unwrap().to_string(),
                r["item"].as_str().unwrap().to_string(),
                r["state"].as_str().unwrap().to_string(),
            )
        })
        .collect()
}

fn state_of(rows: &[(String, String, String)], target: &str, item: &str) -> String {
    rows.iter()
        .find(|(t, i, _)| t == target && i == item)
        .map(|r| r.2.clone())
        .unwrap_or_else(|| panic!("no {target} {item} in {rows:?}"))
}

#[test]
fn status_reports_each_drift_state() {
    let sb = applied();
    let to = ["p", "--to", "claude-code,codex"];
    let rows = status(&sb, &to);
    assert!(rows.iter().all(|r| r.2 == "in sync"), "{rows:?}");
    assert_eq!(state_of(&rows, "claude-code", "instructions"), "in sync");
    assert_eq!(state_of(&rows, "claude-code", "rule rust"), "in sync");
    assert_eq!(state_of(&rows, "codex", "rules"), "in sync");
    sb.cli()
        .args(["status", "p", "--to", "claude-code,codex", "--exit-code"])
        .assert()
        .success()
        .stdout(contains("in sync"));

    // Edited in the tool.
    edit(&sb, "home/.claude/CLAUDE.md", "Be brief.", "Be very brief.");
    let rows = status(&sb, &to);
    assert_eq!(
        state_of(&rows, "claude-code", "instructions"),
        "tool edited"
    );
    assert_eq!(state_of(&rows, "codex", "instructions"), "in sync");
    sb.cli()
        .args(["status", "p", "--to", "claude-code,codex", "--exit-code"])
        .assert()
        .code(1)
        .stdout(contains("tool edited"));

    // Then the profile changes too; a new rule hasn't been applied anywhere.
    fs::write(sb.path("p/instructions.md"), "Be terse.\n").unwrap();
    fs::write(sb.path("p/rules/docs.md"), "Document public items.\n").unwrap();
    let rows = status(&sb, &to);
    assert_eq!(
        state_of(&rows, "claude-code", "instructions"),
        "both changed"
    );
    assert_eq!(state_of(&rows, "codex", "instructions"), "profile changed");
    assert_eq!(state_of(&rows, "claude-code", "rule docs"), "not applied");
    assert_eq!(state_of(&rows, "codex", "rules"), "profile changed");

    // Without a profile: drift since the last apply only.
    let rows = status(&sb, &[]);
    assert_eq!(
        state_of(&rows, "claude-code", "instructions"),
        "tool edited"
    );
    assert_eq!(state_of(&rows, "codex", "instructions"), "in sync");
    assert_eq!(state_of(&rows, "claude-code", "rule rust"), "in sync");
    sb.cli().args(["status", "--exit-code"]).assert().code(1);

    // Status never writes.
    assert!(sb.read("home/.claude/CLAUDE.md").contains("Be very brief."));
    assert!(!sb.home().join(".claude/rules/docs.md").exists());
}

#[test]
fn status_names_unsupported_locations() {
    let sb = applied();
    let rows = status(&sb, &["p", "--to", "cursor"]);
    assert_eq!(state_of(&rows, "cursor", "instructions"), "unsupported");
    sb.cli()
        .args(["status", "p", "--to", "cursor"])
        .assert()
        .success()
        .stdout(contains("app storage"));
}

#[test]
fn sync_applies_profile_changes() {
    let sb = applied();
    fs::write(sb.path("p/instructions.md"), "Be terse.\n").unwrap();
    sb.cli()
        .args(["sync", "p", "--to", "claude-code,codex", "--yes"])
        .assert()
        .success()
        .stdout(contains("Wrote"));
    assert!(sb.read("home/.codex/AGENTS.md").contains("Be terse."));
    assert!(sb.read("home/.claude/CLAUDE.md").contains("Be terse."));
    let rows = status(&sb, &["p", "--to", "claude-code,codex"]);
    assert!(rows.iter().all(|r| r.2 == "in sync"), "{rows:?}");
    // And it is undoable like any apply.
    sb.cli().arg("undo").assert().success();
    assert!(sb.read("home/.codex/AGENTS.md").contains("Be brief."));
}

#[test]
fn sync_leaves_hand_edits_alone() {
    let sb = applied();
    edit(&sb, "home/.claude/CLAUDE.md", "Be brief.", "Be very brief.");
    fs::write(sb.path("p/instructions.md"), "Be terse.\n").unwrap();
    sb.cli()
        .args(["sync", "p", "--to", "claude-code,codex", "--yes"])
        .assert()
        .code(1)
        .stdout(
            contains("claude-code was not synced")
                .and(contains("--from claude-code"))
                .and(contains("--overwrite")),
        );
    assert!(sb.read("home/.claude/CLAUDE.md").contains("Be very brief."));
    assert!(sb.read("home/.codex/AGENTS.md").contains("Be terse."));
    // With every target skipped, it doesn't claim to be up to date.
    sb.cli()
        .args(["sync", "p", "--to", "claude-code", "--yes"])
        .assert()
        .code(1)
        .stdout(contains("Nothing to apply").and(contains("Up to date").not()));
}

#[test]
fn sync_from_pulls_an_edit_back_with_redaction() {
    let sb = applied();
    let token = format!("ghp_{}", "Zx9".repeat(12));
    edit(
        &sb,
        "home/.claude/CLAUDE.md",
        "Be brief.",
        &format!("Be very brief.\n\nUse the token {token} for the API."),
    );
    sb.cli()
        .args([
            "sync",
            "p",
            "--from",
            "claude-code",
            "--to",
            "claude-code,codex",
            "--yes",
        ])
        .assert()
        .success()
        .stdout(contains("redacted").and(contains(&token).not()));
    let instructions = sb.read("p/instructions.md");
    assert!(instructions.contains("Be very brief."), "{instructions}");
    assert!(instructions.contains("[REDACTED"), "{instructions}");
    assert!(!instructions.contains(&token));
    // The manifest is left as written.
    assert_eq!(sb.read("p/kitbag.toml"), MANIFEST);
    let agents = sb.read("home/.codex/AGENTS.md");
    assert!(agents.contains("Be very brief."), "{agents}");
    assert!(!agents.contains(&token));
    let rows = status(&sb, &["p", "--to", "claude-code,codex"]);
    assert_eq!(state_of(&rows, "codex", "instructions"), "in sync");
    // Undo also restores the profile file.
    sb.cli().arg("undo").assert().success();
    assert_eq!(sb.read("p/instructions.md"), "Be brief.\n");
}

#[test]
fn sync_from_pulls_back_an_edited_rule() {
    let sb = applied();
    edit(
        &sb,
        "home/.claude/rules/rust.md",
        "Use `?`.",
        "Use `?` and `anyhow`.",
    );
    sb.cli()
        .args([
            "sync",
            "p",
            "--from",
            "claude-code",
            "--to",
            "claude-code,codex",
            "--yes",
        ])
        .assert()
        .success();
    let rule = sb.read("p/rules/rust.md");
    assert!(rule.contains("Use `?` and `anyhow`."), "{rule}");
    assert!(rule.contains("**/*.rs"), "globs survive: {rule}");
    assert!(sb.read("home/.codex/AGENTS.md").contains("`anyhow`"));
}

#[test]
fn sync_overwrite_replaces_the_edit_with_a_backup() {
    let sb = applied();
    edit(&sb, "home/.claude/CLAUDE.md", "Be brief.", "Be very brief.");
    sb.cli()
        .args(["sync", "p", "--to", "claude-code", "--overwrite", "--yes"])
        .assert()
        .success();
    let text = sb.read("home/.claude/CLAUDE.md");
    assert!(
        text.contains("Be brief.") && !text.contains("very"),
        "{text}"
    );
    sb.cli().arg("undo").assert().success();
    assert!(sb.read("home/.claude/CLAUDE.md").contains("Be very brief."));
}

#[test]
fn sync_from_a_git_source_cannot_write_back() {
    if !has_git() {
        return;
    }
    let sb = applied();
    let url = sb.git_profile(
        "remote",
        &[
            ("kitbag.toml", MANIFEST),
            ("instructions.md", "Be brief.\n"),
        ],
    );
    sb.cli()
        .args([
            "sync",
            &url,
            "--from",
            "claude-code",
            "--to",
            "claude-code",
            "--yes",
        ])
        .assert()
        .code(2)
        .stderr(contains("git source"));
    // Without --from it works like apply, with the remote restrictions.
    sb.cli()
        .args(["sync", &url, "--to", "claude-code", "--yes"])
        .assert()
        .success();
}

#[test]
fn project_scope_status_and_pull_back_from_a_cursor_rule() {
    let sb = Sandbox::empty();
    fs::create_dir_all(sb.path("p/rules")).unwrap();
    fs::write(sb.path("p/kitbag.toml"), MANIFEST).unwrap();
    fs::write(sb.path("p/instructions.md"), "Be brief.\n").unwrap();
    fs::write(sb.path("p/rules/rust.md"), RULE).unwrap();
    let proj = ["--project", "project", "--to", "cursor"];
    sb.cli()
        .args(["apply", "p", "--yes"])
        .args(proj)
        .assert()
        .success();
    edit(
        &sb,
        "project/.cursor/rules/rust.mdc",
        "Use `?`.",
        "Use `?` always.",
    );
    let mut args = vec!["p"];
    args.extend(proj);
    let rows = status(&sb, &args);
    assert_eq!(state_of(&rows, "cursor", "rule rust"), "tool edited");
    assert_eq!(state_of(&rows, "cursor", "instructions"), "in sync");
    // Project records don't show up at user scope.
    assert!(status(&sb, &[]).is_empty());
    sb.cli()
        .args(["sync", "p", "--from", "cursor", "--yes"])
        .args(proj)
        .assert()
        .success();
    let rule = sb.read("p/rules/rust.md");
    assert!(
        rule.contains("Use `?` always.") && rule.contains("**/*.rs"),
        "{rule}"
    );
    assert!(status(&sb, &args).iter().all(|r| r.2 == "in sync"));
}
