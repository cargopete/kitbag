//! Regression tests for the correctness fixes of #38 and two rough edges of 0.1.2.

mod common;

use std::fs;

use common::*;
use predicates::prelude::*;
use predicates::str::contains;

const MANIFEST: &str = "kitbag = 1\nname = \"me\"\n";

fn profile(sb: &Sandbox) {
    fs::create_dir_all(sb.path("p")).unwrap();
    fs::write(sb.path("p/kitbag.toml"), MANIFEST).unwrap();
    fs::write(sb.path("p/instructions.md"), "Be brief.\n").unwrap();
}

#[test]
fn bare_allow_git_mcp_before_the_source_keeps_the_source() {
    if !has_git() {
        return;
    }
    let sb = Sandbox::empty();
    fs::write(sb.home().join(".claude.json"), "{}").unwrap();
    let url = sb.git_profile(
        "g",
        &[(
            "kitbag.toml",
            "kitbag = 1\nname = \"g\"\n\n[mcp_servers.docs]\ncommand = \"npx\"\nargs = [\"-y\", \"docs-mcp\"]\n",
        )],
    );
    sb.cli()
        .args([
            "apply",
            "--allow-git-mcp",
            &url,
            "--to",
            "claude-code",
            "--yes",
        ])
        .assert()
        .success();
    assert!(sb.read("home/.claude.json").contains("docs-mcp"));
    // Named servers before a source and after `=` still work.
    sb.cli()
        .args([
            "apply",
            "--allow-git-mcp",
            "docs",
            &url,
            "--to",
            "claude-code",
            "--yes",
        ])
        .assert()
        .success();
    sb.cli()
        .args([
            "apply",
            "--allow-git-mcp=docs",
            &url,
            "--to",
            "claude-code",
            "--yes",
        ])
        .assert()
        .success();
}

#[test]
fn a_skipped_target_is_not_called_up_to_date() {
    let sb = Sandbox::empty();
    profile(&sb);
    sb.cli()
        .args(["apply", "p", "--to", "claude-code,codex", "--yes"])
        .assert()
        .success();
    let path = sb.path("home/.claude/CLAUDE.md");
    let text = fs::read_to_string(&path).unwrap();
    fs::write(&path, text.replace("Be brief.", "Be very brief.")).unwrap();
    sb.cli()
        .args(["sync", "p", "--to", "claude-code,codex", "--yes"])
        .assert()
        .code(1)
        .stdout(
            contains("Skipped: claude-code").and(contains("every target already matches").not()),
        );
}

#[test]
fn undo_with_an_empty_id_is_an_error() {
    let sb = Sandbox::empty();
    profile(&sb);
    sb.cli()
        .args(["apply", "p", "--to", "claude-code", "--yes"])
        .assert()
        .success();
    for id in ["", "  "] {
        sb.cli()
            .args(["undo", id])
            .assert()
            .failure()
            .stderr(contains("empty transaction id"));
    }
    assert!(sb.read("home/.claude/CLAUDE.md").contains("Be brief."));
}

#[test]
fn doctor_to_restricts_and_rejects_unknown_targets() {
    let sb = Sandbox::empty();
    sb.cli()
        .args(["doctor", "--to", "nope"])
        .assert()
        .failure()
        .stderr(contains("unknown target 'nope'"));
    sb.cli()
        .args(["doctor", "--to", "codex"])
        .assert()
        .success()
        .stdout(contains("Codex").and(contains("Claude Code").not()));
}

#[cfg(unix)]
#[test]
fn skill_scripts_keep_their_executable_bit() {
    use std::os::unix::fs::PermissionsExt;
    let sb = Sandbox::empty();
    profile(&sb);
    fs::create_dir_all(sb.path("p/skills/s")).unwrap();
    fs::write(
        sb.path("p/skills/s/SKILL.md"),
        "---\nname: s\ndescription: d\n---\nBody\n",
    )
    .unwrap();
    let script = sb.path("p/skills/s/run.sh");
    fs::write(&script, "#!/bin/sh\necho hi\n").unwrap();
    fs::set_permissions(&script, fs::Permissions::from_mode(0o755)).unwrap();
    fs::write(sb.path("p/skills/s/notes.txt"), "plain").unwrap();
    sb.cli()
        .args(["apply", "p", "--to", "claude-code", "--yes"])
        .assert()
        .success();
    let mode = |rel: &str| fs::metadata(sb.path(rel)).unwrap().permissions().mode() & 0o777;
    assert_eq!(mode("home/.claude/skills/s/run.sh") & 0o111, 0o111);
    assert_eq!(mode("home/.claude/skills/s/notes.txt") & 0o111, 0);
}

#[test]
fn commands_that_flatten_to_the_same_name_fail_the_capture() {
    let sb = Sandbox::empty();
    fs::create_dir_all(sb.path("home/.claude/commands/team")).unwrap();
    fs::write(sb.path("home/.claude/commands/team-build.md"), "One\n").unwrap();
    fs::write(sb.path("home/.claude/commands/team/build.md"), "Two\n").unwrap();
    sb.cli()
        .args(["capture", "--from", "claude-code", "-o", "out"])
        .assert()
        .failure()
        .stderr(
            contains("team-build.md")
                .and(contains("build.md"))
                .and(contains("both flatten to the name 'team-build'")),
        );
}

#[test]
fn capture_force_replaces_a_profile_with_mdc_rules() {
    let sb = Sandbox::empty();
    profile(&sb);
    fs::create_dir_all(sb.path("p/rules")).unwrap();
    fs::write(
        sb.path("p/rules/style.mdc"),
        "---\ndescription: s\n---\n\nUse tabs.\n",
    )
    .unwrap();
    fs::create_dir_all(sb.path("home/.claude")).unwrap();
    fs::write(sb.path("home/.claude/CLAUDE.md"), "Captured.\n").unwrap();
    sb.cli()
        .args(["capture", "--from", "claude-code", "-o", "p", "--force"])
        .assert()
        .success();
    assert!(!sb.path("p/rules/style.mdc").exists());
    assert!(sb.read("p/instructions.md").contains("Captured."));
}

#[test]
fn no_overwrite_keeps_blocks_and_skill_support_files() {
    let sb = Sandbox::empty();
    profile(&sb);
    fs::create_dir_all(sb.path("p/skills/s")).unwrap();
    fs::write(
        sb.path("p/skills/s/SKILL.md"),
        "---\nname: s\ndescription: d\n---\nBody\n",
    )
    .unwrap();
    fs::write(sb.path("p/skills/s/helper.txt"), "v1").unwrap();
    sb.cli()
        .args(["apply", "p", "--to", "claude-code", "--yes"])
        .assert()
        .success();
    fs::write(sb.path("p/instructions.md"), "Be terse.\n").unwrap();
    fs::write(sb.path("p/skills/s/helper.txt"), "v2").unwrap();
    sb.cli()
        .args([
            "apply",
            "p",
            "--to",
            "claude-code",
            "--yes",
            "--no-overwrite",
        ])
        .assert()
        .success()
        .stdout(contains("kept existing kitbag block").and(contains("kept existing skill file")));
    assert!(sb.read("home/.claude/CLAUDE.md").contains("Be brief."));
    assert_eq!(sb.read("home/.claude/skills/s/helper.txt"), "v1");
    // Without the flag they are replaced.
    sb.cli()
        .args(["apply", "p", "--to", "claude-code", "--yes"])
        .assert()
        .success();
    assert!(sb.read("home/.claude/CLAUDE.md").contains("Be terse."));
    assert_eq!(sb.read("home/.claude/skills/s/helper.txt"), "v2");
}
