//! End-to-end tests of the `kitbag` binary in a sandboxed home directory.

mod common;

use std::fs;
use std::process::Command;

use common::*;
use predicates::prelude::*;
use predicates::str::contains;

#[test]
fn version_and_help() {
    let sb = Sandbox::empty();
    sb.cli()
        .arg("--version")
        .assert()
        .success()
        .stdout(contains(env!("CARGO_PKG_VERSION")));
    sb.cli().arg("--help").assert().success().stdout(
        contains("capture")
            .and(contains("apply"))
            .and(contains("undo"))
            .and(contains("list-targets")),
    );
}

#[test]
fn init_then_apply_then_undo() {
    let sb = Sandbox::empty();
    sb.cli()
        .args(["init", "prof"])
        .assert()
        .success()
        .stdout(contains("Created profile 'prof'"));
    assert!(sb.path("prof/kitbag.toml").exists());
    sb.cli()
        .args(["init", "prof"])
        .assert()
        .code(2)
        .stderr(contains("--force"));

    // Dry run writes nothing.
    sb.cli()
        .args(["apply", "prof", "--to", "claude-code,codex", "--dry-run"])
        .assert()
        .success()
        .stdout(
            contains(format!("+++ {}", shown("~/.claude/CLAUDE.md")))
                .and(contains("nothing written")),
        );
    assert!(!sb.home().join(".claude").exists());

    // Without --yes in a non-interactive session, apply refuses.
    sb.cli()
        .args(["apply", "prof", "--to", "claude-code,codex"])
        .assert()
        .code(2)
        .stderr(contains("pass --yes"));
    assert!(!sb.home().join(".claude").exists());

    sb.cli()
        .args(["apply", "prof", "--to", "claude-code,codex", "--yes"])
        .assert()
        .success()
        .stdout(contains("Wrote").and(contains("kitbag undo")));
    assert!(
        sb.read("home/.claude/CLAUDE.md")
            .contains("Write instructions every agent should follow")
    );
    assert!(sb.home().join(".claude/rules/example.md").exists());
    assert!(
        sb.read("home/.codex/AGENTS.md")
            .contains("Prefer `?` over `unwrap()`")
    );

    // Idempotent: nothing left to do, and diff --exit-code agrees.
    sb.cli()
        .args(["diff", "prof", "--to", "claude-code,codex", "--exit-code"])
        .assert()
        .success()
        .stdout(contains("Up to date"));
    fs::write(sb.path("prof/instructions.md"), "Changed.\n").unwrap();
    sb.cli()
        .args(["diff", "prof", "--to", "claude-code,codex", "--exit-code"])
        .assert()
        .code(1);

    sb.cli()
        .arg("history")
        .assert()
        .success()
        .stdout(contains("applied"));
    let hist: serde_json::Value = serde_json::from_slice(
        &sb.cli()
            .args(["history", "--json"])
            .output()
            .unwrap()
            .stdout,
    )
    .unwrap();
    assert_eq!(hist[0]["status"], "applied");

    sb.cli()
        .arg("undo")
        .assert()
        .success()
        .stdout(contains("Undid"));
    assert!(
        !sb.home().join(".claude").exists(),
        "undo removes created files and directories"
    );
    assert!(!sb.home().join(".codex").exists());
    sb.cli()
        .arg("undo")
        .assert()
        .code(2)
        .stderr(contains("nothing to undo"));
}

#[test]
fn capture_writes_a_redacted_profile() {
    let sb = Sandbox::fixtures();
    sb.cli()
        .args([
            "capture",
            "--from",
            "claude-code,cursor,codex",
            "-o",
            "captured",
        ])
        .assert()
        .success()
        .stdout(
            contains("Secrets replaced with env references")
                .and(contains("GITHUB_PERSONAL_ACCESS_TOKEN")),
        );
    let text = all_text_under(&sb.path("captured"));
    assert!(
        !text.contains("ghp_FAKE")
            && !text.contains("hunter2secret")
            && !text.contains("sntrys_FAKE")
    );
    assert!(
        sb.read("captured/kitbag.toml")
            .contains("[mcp_servers.github]")
    );
    // Refuses to overwrite without --force.
    sb.cli()
        .args(["capture", "--from", "codex", "-o", "captured"])
        .assert()
        .code(2)
        .stderr(contains("--force"));
    sb.cli()
        .args(["capture", "--from", "codex", "-o", "captured", "--force"])
        .assert()
        .success();
    assert!(
        !sb.path("captured/rules/typescript.md").exists(),
        "--force replaces the previous capture"
    );
    // Dry run writes nothing.
    sb.cli()
        .args(["capture", "--from", "codex", "-o", "other", "--dry-run"])
        .assert()
        .success();
    assert!(!sb.path("other").exists());
}

#[test]
fn detected_targets_are_the_default() {
    let sb = Sandbox::empty();
    sb.cli().args(["init", "prof"]).assert().success();
    sb.cli()
        .args(["apply", "prof", "--dry-run"])
        .assert()
        .code(2)
        .stderr(contains("no supported tools detected"));
    fs::create_dir_all(sb.home().join(".codex")).unwrap();
    sb.cli()
        .args(["apply", "prof", "--dry-run"])
        .assert()
        .success()
        .stderr(contains("Detected: codex"))
        .stdout(contains(shown("~/.codex/AGENTS.md")));
}

#[test]
fn project_scope() {
    let sb = Sandbox::fixtures();
    sb.cli().args(["init", "prof"]).assert().success();
    let project = sb.project();
    sb.cli()
        .args([
            "apply",
            "prof",
            "--to",
            "cursor,claude-code",
            "--yes",
            "--project",
        ])
        .arg(&project)
        .assert()
        .success();
    assert!(project.join(".cursor/rules/example.mdc").exists());
    assert!(project.join(".claude/rules/example.md").exists());
    let agents = fs::read_to_string(project.join("AGENTS.md")).unwrap();
    assert!(
        agents.starts_with("# Agents\n\nRun `pnpm test` before committing.\n"),
        "user content kept: {agents}"
    );
    sb.cli()
        .args(["apply", "prof", "--scope", "user", "--project", "."])
        .assert()
        .code(2);
}

#[test]
fn unknown_targets_and_bad_profiles_fail_clearly() {
    let sb = Sandbox::empty();
    sb.cli().args(["init", "prof"]).assert().success();
    sb.cli()
        .args(["diff", "prof", "--to", "vim"])
        .assert()
        .code(2)
        .stderr(contains("unknown target 'vim'").and(contains("claude-code")));
    fs::write(
        sb.path("prof/kitbag.toml"),
        "kitbag = 1\nname = \"x\"\n[mcp_servers.bad]\nargs = [\"x\"]\n",
    )
    .unwrap();
    sb.cli()
        .args(["diff", "prof", "--to", "codex"])
        .assert()
        .code(2)
        .stderr(contains("stdio servers need 'command'"));
    sb.cli()
        .args(["diff", "nowhere", "--to", "codex"])
        .assert()
        .code(2)
        .stderr(contains("neither a local directory nor a git URL"));
}

#[test]
fn list_targets_json() {
    let sb = Sandbox::fixtures();
    let out = sb.cli().args(["list-targets", "--json"]).output().unwrap();
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    let ids: Vec<&str> = v
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["id"].as_str().unwrap())
        .collect();
    assert_eq!(
        ids,
        vec![
            "claude-code",
            "cursor",
            "codex",
            "gemini-cli",
            "claude-desktop"
        ]
    );
    assert_eq!(v[2]["support"]["rules"], "folded");
    assert_eq!(v[0]["detected"], true);
    sb.cli()
        .arg("list-targets")
        .assert()
        .success()
        .stdout(contains("claude-desktop"));
}

#[test]
fn doctor_reports_problems() {
    let sb = Sandbox::fixtures();
    sb.cli().args(["init", "prof"]).assert().success();
    sb.cli()
        .args(["doctor", "prof"])
        .assert()
        .success()
        .stdout(contains("No problems found"));
    fs::write(sb.home().join(".cursor/mcp.json"), "{ broken").unwrap();
    sb.cli()
        .args(["doctor", "prof"])
        .assert()
        .code(1)
        .stdout(contains("invalid JSON"));
}

#[test]
fn undo_refuses_when_files_changed_since() {
    let sb = Sandbox::empty();
    sb.cli().args(["init", "prof"]).assert().success();
    sb.cli()
        .args(["apply", "prof", "--to", "claude-code", "--yes"])
        .assert()
        .success();
    fs::write(sb.home().join(".claude/CLAUDE.md"), "my edits\n").unwrap();
    sb.cli()
        .arg("undo")
        .assert()
        .code(2)
        .stderr(contains("modified after"));
    sb.cli().args(["undo", "--force"]).assert().success();
    assert!(!sb.home().join(".claude/CLAUDE.md").exists());
}

/// A `~/...` display path with the platform's separator.
fn shown(path: &str) -> String {
    path.replace('/', std::path::MAIN_SEPARATOR_STR)
}

#[test]
fn applies_from_a_git_repository() {
    if Command::new("git").arg("--version").output().is_err() {
        eprintln!("git not installed; skipping");
        return;
    }
    let sb = Sandbox::empty();
    let repo = sb.path("repo");
    fs::create_dir_all(&repo).unwrap();
    sb.cli()
        .args(["init", "repo/agents/work"])
        .assert()
        .success();
    git(&repo, &["init", "-q"]);
    git(&repo, &["add", "."]);
    git(&repo, &["commit", "-q", "-m", "profile"]);
    let url = format!("{}//agents/work", file_url(&repo));
    sb.cli()
        .args(["apply", &url, "--to", "codex", "--yes"])
        .env(
            "GIT_CONFIG_GLOBAL",
            if cfg!(windows) { "NUL" } else { "/dev/null" },
        )
        .assert()
        .success()
        .stdout(contains("from file://").and(contains("Wrote")));
    assert!(sb.home().join(".codex/AGENTS.md").exists());
    sb.cli()
        .args(["diff", &file_url(&repo), "--path", "nope", "--to", "codex"])
        .assert()
        .code(2)
        .stderr(contains("no kitbag.toml"));
}

#[test]
fn sandbox_ignores_real_location_overrides() {
    // With --home / KITBAG_HOME, CODEX_HOME etc. from the real environment must not be used.
    let sb = Sandbox::empty();
    sb.cli().args(["init", "prof"]).assert().success();
    let elsewhere = sb.path("real-codex-home");
    sb.cli()
        .env("CODEX_HOME", &elsewhere)
        .args(["apply", "prof", "--to", "codex", "--yes"])
        .assert()
        .success();
    assert!(!elsewhere.exists());
    assert!(sb.home().join(".codex/AGENTS.md").exists());
}

#[cfg(unix)]
#[test]
fn project_symlinks_cannot_redirect_writes() {
    use std::os::unix::fs::symlink;
    let sb = Sandbox::empty();
    sb.cli().args(["init", "prof"]).assert().success();
    let victim = sb.path("victim");
    fs::create_dir_all(&victim).unwrap();
    fs::write(victim.join("authorized_keys"), "ssh-ed25519 AAAA\n").unwrap();
    let project = sb.project();
    symlink(victim.join("authorized_keys"), project.join("CLAUDE.md")).unwrap();
    sb.cli()
        .args(["apply", "prof", "--to", "claude-code", "--yes", "--project"])
        .arg(&project)
        .assert()
        .code(2)
        .stderr(contains("outside"));
    fs::remove_file(project.join("CLAUDE.md")).unwrap();
    symlink(victim.join("newdir/evil.md"), project.join("AGENTS.md")).unwrap();
    sb.cli()
        .args(["apply", "prof", "--to", "codex", "--yes", "--project"])
        .arg(&project)
        .assert()
        .code(2)
        .stderr(contains("dangling"));
    assert_eq!(
        fs::read_to_string(victim.join("authorized_keys")).unwrap(),
        "ssh-ed25519 AAAA\n"
    );
    assert!(!victim.join("newdir").exists());
}

/// A home whose skill, rule, command and CLAUDE.md hold secret-looking lines.
fn home_with_secrets(sb: &Sandbox) -> Vec<(std::path::PathBuf, String)> {
    let xai = format!("xai-{}", "Zk3jQ9vLm2Xp8RtY4wNb7HcF".repeat(3));
    let files = [
        (
            ".claude/skills/deploy/SKILL.md",
            format!(
                "---\nname: deploy\ndescription: Deploys\n---\n\nUse key {xai} when calling grok.\n\nPoint it at `postgres://admin:Xk93jdLq0@db.internal/app`. Then run it.\n"
            ),
        ),
        (
            ".claude/skills/deploy/cli-reference.md",
            "e.g. `postgres://user:pass@host/db`. Or `redis://u:Q8Zr2LkP0vXa@cache:6379`.\n"
                .to_string(),
        ),
        (
            ".claude/rules/db.md",
            "---\npaths:\n  - \"db/**\"\n---\n\nThe staging DB is postgres://app:Q8Zr2LkP0vXa@staging/app\n"
                .to_string(),
        ),
        (
            ".claude/commands/ship.md",
            format!("---\ndescription: Ship\n---\n\nexport XAI_API_KEY={xai}\n"),
        ),
        (
            ".claude/CLAUDE.md",
            format!("# Me\n\nBe brief.\n\nMy grok key is {xai}.\n"),
        ),
    ];
    files
        .into_iter()
        .map(|(rel, text)| {
            let p = sb.home().join(rel);
            fs::create_dir_all(p.parent().unwrap()).unwrap();
            fs::write(&p, &text).unwrap();
            (p, text)
        })
        .collect()
}

#[test]
fn applying_a_redacted_capture_back_keeps_the_originals() {
    let sb = Sandbox::empty();
    let files = home_with_secrets(&sb);
    sb.cli()
        .args(["capture", "--from", "claude-code", "-o", "prof"])
        .assert()
        .success();
    let profile = all_text_under(&sb.path("prof"));
    assert!(profile.contains("[REDACTED"), "{profile}");
    assert!(!profile.contains("Xk93jdLq0") && !profile.contains("Zk3jQ9"));

    sb.cli()
        .args(["diff", "prof", "--to", "claude-code", "--exit-code"])
        .assert()
        .success()
        .stdout(contains("Up to date").and(contains("redacted copy")));
    sb.cli()
        .args(["apply", "prof", "--to", "claude-code", "--yes"])
        .assert()
        .success();
    for (path, text) in &files {
        assert_eq!(
            &fs::read_to_string(path).unwrap(),
            text,
            "{}",
            path.display()
        );
    }

    // A file that differs in other ways too is replaced, with a loud warning.
    let (reference, _) = &files[1];
    fs::write(reference, "something else\n").unwrap();
    sb.cli()
        .args(["diff", "prof", "--to", "claude-code", "--exit-code"])
        .assert()
        .code(1)
        .stdout(contains("contains [REDACTED] markers"));
}

#[test]
fn risky_settings_from_remote_profiles_need_an_explicit_flag() {
    if Command::new("git").arg("--version").output().is_err() {
        eprintln!("git not installed; skipping");
        return;
    }
    let sb = Sandbox::empty();
    let repo = sb.path("repo");
    fs::create_dir_all(&repo).unwrap();
    fs::write(
        repo.join("kitbag.toml"),
        r#"kitbag = 1
name = "team"

[settings.claude-code]
model = "opus"
permissions = { defaultMode = "bypassPermissions", additionalDirectories = ["/"], allow = ["Bash(*)", "Bash(rm -rf *)", "Bash(cargo test:*)"] }

[settings.codex]
model = "o3"
approval_policy = "never"
sandbox_mode = "danger-full-access"
"#,
    )
    .unwrap();
    git(&repo, &["init", "-q"]);
    git(&repo, &["add", "."]);
    git(&repo, &["commit", "-q", "-m", "profile"]);
    let url = file_url(&repo);
    let null = if cfg!(windows) { "NUL" } else { "/dev/null" };

    sb.cli()
        .args(["apply", &url, "--to", "claude-code,codex", "--yes"])
        .env("GIT_CONFIG_GLOBAL", null)
        .assert()
        .success()
        .stdout(
            contains("comes from a remote repository: permissions.defaultMode = bypassPermissions")
                .and(contains("--allow-risky-settings"))
                .and(contains("approval_policy = never"))
                .and(contains(
                    r#"permissions.allow "Bash(*)", "Bash(rm -rf *)", "Bash(cargo test:*)""#,
                )),
        );
    assert_eq!(
        sb.json("home/.claude/settings.json"),
        serde_json::json!({"model": "opus"})
    );
    let codex = sb.read("home/.codex/config.toml");
    assert!(
        codex.contains("model = \"o3\"") && !codex.contains("never") && !codex.contains("danger")
    );

    // With the flag, they are applied (and still flagged).
    sb.cli().arg("undo").assert().success();
    sb.cli()
        .args([
            "apply",
            &url,
            "--to",
            "claude-code,codex",
            "--yes",
            "--allow-risky-settings",
        ])
        .env("GIT_CONFIG_GLOBAL", null)
        .assert()
        .success()
        .stdout(contains("Lets agents act without asking"));
    assert_eq!(
        sb.json("home/.claude/settings.json")["permissions"]["defaultMode"],
        "bypassPermissions"
    );
    assert!(
        sb.read("home/.codex/config.toml")
            .contains("danger-full-access")
    );

    // A local profile directory behaves as before: applied, with a warning.
    sb.cli().arg("undo").assert().success();
    sb.cli()
        .args(["apply", "repo", "--to", "claude-code", "--yes"])
        .assert()
        .success()
        .stdout(
            contains("Lets agents act without asking")
                .and(contains("permissions.additionalDirectories = /"))
                .and(contains("remote repository").not()),
        );
    assert_eq!(
        sb.json("home/.claude/settings.json")["permissions"]["additionalDirectories"],
        serde_json::json!(["/"])
    );
}
