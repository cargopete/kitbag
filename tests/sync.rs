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

/// A `~/...` display path with the platform's separator.
fn shown(path: &str) -> String {
    path.replace('/', std::path::MAIN_SEPARATOR_STR)
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
            contains(format!(
                "instructions block in {} was not synced",
                shown("~/.claude/CLAUDE.md")
            ))
            .and(contains("--from claude-code"))
            .and(contains("--overwrite")),
        );
    assert!(sb.read("home/.claude/CLAUDE.md").contains("Be very brief."));
    assert!(sb.read("home/.codex/AGENTS.md").contains("Be terse."));
    // With only the edited item left, it doesn't claim to be up to date.
    sb.cli()
        .args(["sync", "p", "--to", "claude-code", "--yes"])
        .assert()
        .code(1)
        .stdout(
            contains("Nothing to write besides the items edited in the tool")
                .and(contains("Skipped: claude-code instructions"))
                .and(contains("Up to date").not()),
        );
}

/// Add an MCP server `fresh` to the profile of [`applied`].
fn add_server(sb: &Sandbox) {
    fs::write(
        sb.path("p/kitbag.toml"),
        format!("{MANIFEST}\n[mcp_servers.fresh]\ncommand = \"fresh-mcp\"\n"),
    )
    .unwrap();
}

fn has_server(sb: &Sandbox) -> bool {
    sb.home().join(".claude.json").exists()
        && sb.json("home/.claude.json")["mcpServers"]
            .get("fresh")
            .is_some()
}

#[test]
fn sync_holds_back_only_the_edited_instructions_block() {
    let sb = applied();
    edit(&sb, "home/.claude/CLAUDE.md", "Be brief.", "Be very brief.");
    let edited = sb.read("home/.claude/CLAUDE.md");
    fs::write(sb.path("p/instructions.md"), "Be terse.\n").unwrap();
    fs::write(sb.path("p/rules/docs.md"), "Document public items.\n").unwrap();
    add_server(&sb);
    let to = ["--to", "claude-code,codex"];
    // The plan lists the held-back item with both ways out.
    sb.cli()
        .args(["sync", "p", "--dry-run"])
        .args(to)
        .assert()
        .code(1)
        .stdout(
            contains("Skipped: claude-code instructions (edited in the tool")
                .and(contains("--from claude-code"))
                .and(contains("--overwrite"))
                .and(contains("fresh"))
                .and(contains("Up to date").not()),
        );
    sb.cli()
        .args(["sync", "p", "--yes"])
        .args(to)
        .assert()
        .code(1)
        .stdout(
            contains("Wrote")
                .and(contains("Skipped: claude-code instructions"))
                .and(contains("Skipped: claude-code,").not()),
        );
    // The edited block is kept; the rest of Claude Code is synced.
    assert_eq!(sb.read("home/.claude/CLAUDE.md"), edited);
    assert!(has_server(&sb));
    assert!(sb.home().join(".claude/rules/docs.md").exists());
    assert!(sb.read("home/.codex/AGENTS.md").contains("Be terse."));
    let rows = status(&sb, &["p", "--to", "claude-code,codex"]);
    assert_eq!(
        state_of(&rows, "claude-code", "instructions"),
        "both changed"
    );
    assert_eq!(state_of(&rows, "claude-code", "rule docs"), "in sync");
    assert_eq!(state_of(&rows, "codex", "instructions"), "in sync");
    // One undo reverts all of it and leaves the edit alone.
    sb.cli().arg("undo").assert().success();
    assert!(!has_server(&sb));
    assert!(!sb.home().join(".claude/rules/docs.md").exists());
    assert!(sb.read("home/.codex/AGENTS.md").contains("Be brief."));
    assert_eq!(sb.read("home/.claude/CLAUDE.md"), edited);
}

#[test]
fn sync_holds_back_only_the_edited_rule_file() {
    let sb = applied();
    let original = sb.read("home/.claude/rules/rust.md");
    edit(
        &sb,
        "home/.claude/rules/rust.md",
        "Use `?`.",
        "Use `?` and `anyhow`.",
    );
    let edited = sb.read("home/.claude/rules/rust.md");
    fs::write(sb.path("p/instructions.md"), "Be terse.\n").unwrap();
    add_server(&sb);
    sb.cli()
        .args(["sync", "p", "--to", "claude-code,codex", "--yes"])
        .assert()
        .code(1)
        .stdout(
            contains("Skipped: claude-code rule rust")
                .and(contains(format!(
                    "rule rust in {} was not synced",
                    shown("~/.claude/rules/rust.md"),
                )))
                .and(contains("--from claude-code")),
        );
    assert_eq!(sb.read("home/.claude/rules/rust.md"), edited);
    assert!(sb.read("home/.claude/CLAUDE.md").contains("Be terse."));
    assert!(has_server(&sb));
    let rows = status(&sb, &["p", "--to", "claude-code"]);
    assert_eq!(state_of(&rows, "claude-code", "rule rust"), "tool edited");
    assert_eq!(state_of(&rows, "claude-code", "instructions"), "in sync");
    sb.cli().arg("undo").assert().success();
    assert!(!has_server(&sb));
    assert!(sb.read("home/.claude/CLAUDE.md").contains("Be brief."));
    assert_eq!(sb.read("home/.claude/rules/rust.md"), edited);
    // --overwrite still replaces the edit.
    sb.cli()
        .args(["sync", "p", "--to", "claude-code", "--overwrite", "--yes"])
        .assert()
        .success();
    assert_eq!(sb.read("home/.claude/rules/rust.md"), original);
}

#[test]
fn sync_holds_back_one_block_of_a_shared_file() {
    // Codex keeps instructions and folded rules as two blocks in AGENTS.md.
    let sb = applied();
    edit(&sb, "home/.codex/AGENTS.md", "Be brief.", "Be very brief.");
    fs::write(
        sb.path("p/rules/rust.md"),
        RULE.replace("Use `?`.", "Use `?` and `thiserror`."),
    )
    .unwrap();
    sb.cli()
        .args(["sync", "p", "--to", "codex", "--yes"])
        .assert()
        .code(1)
        .stdout(contains("Skipped: codex instructions").and(contains("Wrote")));
    let agents = sb.read("home/.codex/AGENTS.md");
    assert!(agents.contains("Be very brief."), "{agents}");
    assert!(agents.contains("`thiserror`"), "{agents}");
    let rows = status(&sb, &["p", "--to", "codex"]);
    assert_eq!(state_of(&rows, "codex", "instructions"), "tool edited");
    assert_eq!(state_of(&rows, "codex", "rules"), "in sync");
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

#[test]
fn undo_after_a_no_op_apply_restores_profile_changed() {
    let sb = applied();
    let to = ["p", "--to", "claude-code,codex"];
    fs::write(sb.path("p/instructions.md"), "Be terse.\n").unwrap();
    sb.cli()
        .args(["apply", "p", "--to", "claude-code,codex", "--yes"])
        .assert()
        .success()
        .stdout(contains("Wrote"));
    // Nothing to write the second time.
    sb.cli()
        .args(["apply", "p", "--to", "claude-code,codex", "--yes"])
        .assert()
        .success()
        .stdout(contains("Up to date"));
    sb.cli().arg("undo").assert().success();
    assert!(sb.read("home/.claude/CLAUDE.md").contains("Be brief."));
    let rows = status(&sb, &to);
    assert_eq!(
        state_of(&rows, "claude-code", "instructions"),
        "profile changed"
    );
    assert_eq!(state_of(&rows, "codex", "instructions"), "profile changed");
    assert_eq!(state_of(&rows, "claude-code", "rule rust"), "in sync");
}

#[test]
fn undo_after_a_repeated_sync_with_a_held_item_restores_profile_changed() {
    let sb = applied();
    edit(
        &sb,
        "home/.claude/rules/rust.md",
        "Use `?`.",
        "Use `?` and `anyhow`.",
    );
    fs::write(sb.path("p/instructions.md"), "Be terse.\n").unwrap();
    let sync = || {
        sb.cli()
            .args(["sync", "p", "--to", "claude-code", "--yes"])
            .assert()
            .code(1)
    };
    sync().stdout(contains("Wrote").and(contains("Skipped: claude-code rule rust")));
    // The second sync only re-records the items it leaves in place.
    sync().stdout(
        contains("Nothing to write besides the items edited in the tool")
            .and(contains("Wrote").not()),
    );
    sb.cli().arg("undo").assert().success();
    assert!(sb.read("home/.claude/CLAUDE.md").contains("Be brief."));
    let rows = status(&sb, &["p", "--to", "claude-code"]);
    assert_eq!(
        state_of(&rows, "claude-code", "instructions"),
        "profile changed"
    );
    assert_eq!(state_of(&rows, "claude-code", "rule rust"), "tool edited");
}

/// A rule file and text added in Claude Code only, after `applied()`.
fn with_one_offs() -> Sandbox {
    let sb = applied();
    fs::write(
        sb.path("home/.claude/rules/one-off.md"),
        "Never deploy on Fridays.\n",
    )
    .unwrap();
    let claude = sb.read("home/.claude/CLAUDE.md");
    fs::write(
        sb.path("home/.claude/CLAUDE.md"),
        format!("# Mine\n\nRun make lint.\n\n{claude}"),
    )
    .unwrap();
    sb
}

#[test]
fn status_lists_rules_and_text_only_in_the_tool() {
    let sb = with_one_offs();
    let to = ["p", "--to", "claude-code,codex"];
    let rows = status(&sb, &to);
    assert_eq!(state_of(&rows, "claude-code", "rule one-off"), "tool only");
    assert_eq!(
        state_of(&rows, "claude-code", "unmanaged text"),
        "tool only"
    );
    assert_eq!(state_of(&rows, "claude-code", "instructions"), "in sync");
    assert!(
        !rows.iter().any(|r| r.0 == "codex" && r.2 == "tool only"),
        "{rows:?}"
    );
    // Not drift by default; --strict counts it.
    sb.cli()
        .args(["status", "--exit-code"])
        .args(to)
        .assert()
        .success()
        .stdout(contains("tool only").and(contains("--adopt")));
    sb.cli()
        .args(["status", "--exit-code", "--strict"])
        .args(to)
        .assert()
        .code(1);
    sb.cli()
        .args(["status", "--strict"])
        .args(to)
        .assert()
        .failure();
    sb.cli().arg("diff").args(to).assert().success().stdout(
        contains("only in the tool, not in the profile: rule one-off")
            .and(contains(shown("~/.claude/rules/one-off.md")))
            .and(contains("unmanaged text")),
    );
    sb.cli()
        .arg("doctor")
        .args(to)
        .assert()
        .success()
        .stdout(contains("tool only claude-code rule one-off").and(contains("No problems found.")));
}

#[test]
fn sync_adopt_pulls_tool_only_rules_and_text_into_the_profile() {
    let sb = with_one_offs();
    let to = ["--to", "claude-code,codex"];
    let claude = sb.read("home/.claude/CLAUDE.md");
    // Without --adopt they are named, not pulled.
    sb.cli()
        .args(["sync", "p", "--from", "claude-code", "--yes"])
        .args(to)
        .assert()
        .success()
        .stdout(contains("Add --adopt"));
    assert!(!sb.path("p/rules/one-off.md").exists());
    assert_eq!(sb.read("p/instructions.md"), "Be brief.\n");
    sb.cli()
        .args(["sync", "p", "--adopt", "--yes"])
        .args(to)
        .assert()
        .failure();
    sb.cli()
        .args(["sync", "p", "--from", "claude-code", "--adopt", "--yes"])
        .args(to)
        .assert()
        .success();
    assert!(
        sb.read("p/rules/one-off.md")
            .contains("Never deploy on Fridays.")
    );
    let instructions = sb.read("p/instructions.md");
    assert!(
        instructions.starts_with("Be brief.") && instructions.contains("Run make lint."),
        "{instructions}"
    );
    // Claude Code keeps its text where it was, not repeated in the block.
    assert_eq!(sb.read("home/.claude/CLAUDE.md"), claude);
    let agents = sb.read("home/.codex/AGENTS.md");
    assert!(
        agents.contains("Run make lint.") && agents.contains("Never deploy on Fridays."),
        "{agents}"
    );
    let rows = status(&sb, &["p", "--to", "claude-code,codex"]);
    assert!(rows.iter().all(|r| r.2 == "in sync"), "{rows:?}");
    // Undo takes the profile back too.
    sb.cli().arg("undo").assert().success();
    assert!(!sb.path("p/rules/one-off.md").exists());
    assert_eq!(sb.read("p/instructions.md"), "Be brief.\n");
}

#[test]
fn adopt_skips_secrets_and_rules_of_other_profiles() {
    let sb = applied();
    // Another profile's rule is managed, just not by this one.
    fs::create_dir_all(sb.path("q/rules")).unwrap();
    fs::write(sb.path("q/kitbag.toml"), "kitbag = 1\nname = \"other\"\n").unwrap();
    fs::write(sb.path("q/rules/theirs.md"), "Theirs.\n").unwrap();
    sb.cli()
        .args(["apply", "q", "--to", "claude-code", "--yes"])
        .assert()
        .success();
    let token = format!("ghp_{}", "Qw7".repeat(12));
    fs::write(
        sb.path("home/.claude/rules/keys.md"),
        format!("Use {token} for staging.\n"),
    )
    .unwrap();
    let claude = sb.read("home/.claude/CLAUDE.md");
    fs::write(
        sb.path("home/.claude/CLAUDE.md"),
        format!("Staging token: {token}\n\n{claude}"),
    )
    .unwrap();
    let rows = status(&sb, &["p", "--to", "claude-code"]);
    assert!(!rows.iter().any(|r| r.1 == "rule theirs"), "{rows:?}");
    assert_eq!(state_of(&rows, "claude-code", "rule keys"), "tool only");
    sb.cli()
        .args([
            "sync",
            "p",
            "--to",
            "claude-code",
            "--from",
            "claude-code",
            "--adopt",
            "--yes",
        ])
        .assert()
        .success()
        .stdout(
            contains("looks like it holds a secret")
                .and(contains("1 paragraph(s) of"))
                .and(contains(&token).not()),
        );
    assert_eq!(sb.read("p/instructions.md"), "Be brief.\n");
    assert!(!sb.path("p/rules/keys.md").exists());
    assert!(!sb.path("p/rules/theirs.md").exists());
    assert!(sb.read("home/.claude/rules/keys.md").contains(&token));
}

#[test]
fn project_scope_adopts_a_cursor_rule() {
    let sb = Sandbox::empty();
    fs::create_dir_all(sb.path("p/rules")).unwrap();
    fs::write(sb.path("p/kitbag.toml"), MANIFEST).unwrap();
    fs::write(sb.path("p/instructions.md"), "Be brief.\n").unwrap();
    let proj = ["--project", "project", "--to", "cursor"];
    sb.cli()
        .args(["apply", "p", "--yes"])
        .args(proj)
        .assert()
        .success();
    fs::create_dir_all(sb.path("project/.cursor/rules")).unwrap();
    fs::write(
        sb.path("project/.cursor/rules/hotfix.mdc"),
        "---\ndescription: Hotfix\nglobs: src/**\n---\n\nKeep the flag.\n",
    )
    .unwrap();
    let mut args = vec!["p"];
    args.extend(proj);
    assert_eq!(
        state_of(&status(&sb, &args), "cursor", "rule hotfix"),
        "tool only"
    );
    sb.cli()
        .args(["sync", "p", "--from", "cursor", "--adopt", "--yes"])
        .args(proj)
        .assert()
        .success();
    let rule = sb.read("p/rules/hotfix.md");
    assert!(
        rule.contains("Keep the flag.") && rule.contains("src/**"),
        "{rule}"
    );
    assert_eq!(
        state_of(&status(&sb, &args), "cursor", "rule hotfix"),
        "in sync"
    );
}

#[test]
fn pulling_back_a_block_keeps_adopted_text() {
    let sb = applied();
    let codex = sb.read("home/.codex/AGENTS.md");
    fs::write(
        sb.path("home/.codex/AGENTS.md"),
        format!("My codex note.\n\n{codex}"),
    )
    .unwrap();
    let to = ["--to", "codex,gemini-cli"];
    sb.cli()
        .args(["sync", "p", "--from", "codex", "--adopt", "--yes"])
        .args(to)
        .assert()
        .success();
    assert!(sb.read("home/.gemini/GEMINI.md").contains("My codex note."));
    // The note isn't repeated in Codex's block, so pulling an edit of that
    // block back must not drop it from the profile.
    edit(&sb, "home/.codex/AGENTS.md", "Be brief.", "Be very brief.");
    sb.cli()
        .args(["sync", "p", "--from", "codex", "--yes"])
        .args(to)
        .assert()
        .success();
    assert_eq!(
        sb.read("p/instructions.md"),
        "Be very brief.\n\nMy codex note.\n"
    );
    let gemini = sb.read("home/.gemini/GEMINI.md");
    assert!(
        gemini.contains("Be very brief.") && gemini.contains("My codex note."),
        "{gemini}"
    );
    let rows = status(&sb, &["p", "--to", "codex,gemini-cli"]);
    assert!(rows.iter().all(|r| r.2 == "in sync"), "{rows:?}");
}

#[test]
fn adopt_says_what_rewriting_a_rule_file_drops() {
    let sb = applied();
    fs::write(
        sb.path("home/.claude/rules/hotfix.md"),
        "---\npaths: src/**\nfoo: bar\n---\n\nKeep the flag.\n",
    )
    .unwrap();
    sb.cli()
        .args([
            "sync",
            "p",
            "--to",
            "claude-code",
            "--from",
            "claude-code",
            "--adopt",
            "--yes",
        ])
        .assert()
        .success()
        .stdout(contains(
            "rewritten in kitbag's format, which drops: 'foo' from the frontmatter (",
        ));
    assert!(sb.read("p/rules/hotfix.md").contains("Keep the flag."));
    sb.cli().arg("undo").assert().success();
    assert!(sb.read("home/.claude/rules/hotfix.md").contains("foo: bar"));
}

#[test]
fn adopt_takes_a_renamed_top_level_rule_but_not_a_nested_one() {
    let sb = Sandbox::empty();
    fs::create_dir_all(sb.path("p")).unwrap();
    fs::write(sb.path("p/kitbag.toml"), MANIFEST).unwrap();
    fs::write(sb.path("p/instructions.md"), "Be brief.\n").unwrap();
    let proj = ["--project", "project", "--to", "cursor"];
    sb.cli()
        .args(["apply", "p", "--yes"])
        .args(proj)
        .assert()
        .success();
    fs::create_dir_all(sb.path("project/.cursor/rules/team")).unwrap();
    fs::write(
        sb.path("project/.cursor/rules/My Rule.mdc"),
        "---\nalwaysApply: true\n---\n\nMine.\n",
    )
    .unwrap();
    fs::write(
        sb.path("project/.cursor/rules/team/x.mdc"),
        "---\nalwaysApply: true\n---\n\nTeam.\n",
    )
    .unwrap();
    sb.cli()
        .args(["sync", "p", "--from", "cursor", "--adopt", "--yes"])
        .args(proj)
        .assert()
        .success()
        .stdout(contains("team-x").and(contains("in a sub-directory")));
    assert!(sb.read("p/rules/My-Rule.md").contains("Mine."));
    assert!(!sb.path("p/rules/team-x.md").exists());
}

#[test]
fn strict_counts_removed_and_folded_rules_are_not_tool_only() {
    let sb = applied();
    // Codex folds rules into AGENTS.md: they are never listed as tool only.
    let rows = status(&sb, &["p", "--to", "codex"]);
    assert!(!rows.iter().any(|r| r.2 == "tool only"), "{rows:?}");
    fs::create_dir_all(sb.path("home/.claude/commands")).unwrap();
    fs::write(sb.path("home/.claude/commands/mine.md"), "Do it.\n").unwrap();
    let to = ["p", "--to", "claude-code"];
    assert_eq!(
        state_of(&status(&sb, &to), "claude-code", "command mine"),
        "removed"
    );
    sb.cli()
        .args(["status", "--exit-code"])
        .args(to)
        .assert()
        .success();
    sb.cli()
        .args(["status", "--exit-code", "--strict"])
        .args(to)
        .assert()
        .code(1);
}

#[test]
fn text_between_two_profiles_blocks_is_tool_only() {
    let sb = applied();
    fs::create_dir_all(sb.path("q")).unwrap();
    fs::write(sb.path("q/kitbag.toml"), "kitbag = 1\nname = \"other\"\n").unwrap();
    fs::write(sb.path("q/instructions.md"), "Theirs.\n").unwrap();
    sb.cli()
        .args(["apply", "q", "--to", "claude-code", "--yes"])
        .assert()
        .success();
    let text = sb.read("home/.claude/CLAUDE.md");
    let at = text.find("<!-- kitbag:begin other").unwrap();
    fs::write(
        sb.path("home/.claude/CLAUDE.md"),
        format!("{}Between.\n\n{}", &text[..at], &text[at..]),
    )
    .unwrap();
    let rows = status(&sb, &["p", "--to", "claude-code"]);
    assert_eq!(
        state_of(&rows, "claude-code", "unmanaged text"),
        "tool only"
    );
    // The other profile's block is not text of this tool.
    sb.cli()
        .args([
            "sync",
            "p",
            "--to",
            "claude-code",
            "--from",
            "claude-code",
            "--adopt",
            "--yes",
        ])
        .assert()
        .success();
    assert_eq!(sb.read("p/instructions.md"), "Be brief.\n\nBetween.\n");
}
