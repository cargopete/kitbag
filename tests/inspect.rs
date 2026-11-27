//! `kitbag inspect`: what a source holds and what applying it would write,
//! with nothing written, started or shown in the clear.

mod common;

use common::*;

/// A made-up literal key under a secret-sounding name, and one in `args`.
const FAKE_KEY: &str = "fake-literal-key-value-xyz";
const FAKE_ARG: &str = "abcd1234efgh5678ijkl9012mnop";
const FAKE_PASSWORD: &str = "s3cret-fake-pw";

fn manifest() -> String {
    format!(
        "kitbag = 1\nname = \"demo\"\ndescription = \"Demo profile\"\n\n\
         [mcp_servers.github]\ncommand = \"npx\"\n\
         args = [\"-y\", \"server-github\", \"--api-key\", \"{FAKE_ARG}\"]\n\
         env = {{ GITHUB_TOKEN = \"${{GITHUB_TOKEN}}\", API_KEY = \"{FAKE_KEY}\" }}\n\n\
         [mcp_servers.linear]\nurl = \"https://bob:{FAKE_PASSWORD}@mcp.example.com/mcp\"\n\
         headers = {{ Authorization = \"Bearer ${{LINEAR_API_KEY}}\" }}\n"
    )
}

fn write(dir: &std::path::Path, files: &[(&str, &str)]) {
    for (name, content) in files {
        let p = dir.join(name);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, content).unwrap();
    }
}

fn local_profile(sb: &Sandbox) -> std::path::PathBuf {
    let dir = sb.path("p");
    write(
        &dir,
        &[
            ("kitbag.toml", &manifest()),
            ("instructions.md", "Be brief.\n"),
            ("rules/style.md", "Use tabs.\n"),
            ("commands/review.md", "Review the diff.\n"),
            (
                "skills/deploy/SKILL.md",
                "---\nname: deploy\ndescription: Deploys\n---\nRun run.sh.\n",
            ),
            ("skills/deploy/run.sh", "#!/bin/sh\necho hi\n"),
        ],
    );
    dir
}

/// Both streams, with Windows path separators shown as `/`.
fn stdout(out: &std::process::Output) -> String {
    format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    )
    .replace('\\', "/")
}

fn assert_masked(all: &str) {
    for secret in [FAKE_KEY, FAKE_ARG, FAKE_PASSWORD, "bob"] {
        assert!(!all.contains(secret), "{secret} shown: {all}");
    }
}

#[test]
fn inspect_lists_a_local_profile_and_masks_secrets() {
    let sb = Sandbox::empty();
    let dir = local_profile(&sb);
    let out = sb.cli().arg("inspect").arg(&dir).output().unwrap();
    assert!(out.status.success(), "{}", stdout(&out));
    let all = stdout(&out);
    for want in [
        "Profile 'demo' from",
        "instructions.md (1 line(s), ",
        "rules (1)",
        "style",
        "commands (1)",
        "review",
        "skills (1)",
        "deploy",
        "agents (0)",
        "github: npx -y server-github --api-key ••••••••",
        "API_KEY=••••••••",
        "GITHUB_TOKEN=${GITHUB_TOKEN}",
        "linear: https://mcp.example.com/mcp",
        "Authorization=Bearer ${LINEAR_API_KEY}",
        "Runs local commands",
        "Connects to (MCP servers):",
        "Env vars it references:",
        "GITHUB_TOKEN, LINEAR_API_KEY",
        "mcp_servers.github.env.API_KEY: looks like a secret",
        // No tool in the empty home: the file list needs --to.
        "pass --to <ids>",
    ] {
        assert!(all.contains(want), "missing {want:?}:\n{all}");
    }
    assert_masked(&all);
    // Read-only: no state, nothing in home.
    assert!(!sb.state().exists());
    assert_eq!(std::fs::read_dir(sb.home()).unwrap().count(), 0);
}

#[test]
fn inspect_json_masks_secrets_too() {
    let sb = Sandbox::empty();
    let dir = local_profile(&sb);
    let out = sb
        .cli()
        .arg("inspect")
        .arg(&dir)
        .args(["--json", "--to", "claude-code"])
        .output()
        .unwrap();
    assert!(out.status.success(), "{}", stdout(&out));
    assert_masked(&stdout(&out));
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(v["kind"], "profile");
    assert_eq!(v["commit"], serde_json::Value::Null);
    assert_eq!(v["rules"], serde_json::json!(["style"]));
    assert_eq!(
        v["safety"]["env"],
        serde_json::json!(["GITHUB_TOKEN", "LINEAR_API_KEY"])
    );
    let files = v["files"]["claude-code"]["files"].as_array().unwrap();
    assert!(
        files
            .iter()
            .any(|f| f["path"].as_str().unwrap().ends_with("CLAUDE.md") && f["change"] == "create"),
        "{files:?}"
    );
    assert!(!sb.home().join(".claude").exists());
}

#[test]
fn inspect_lists_the_files_apply_would_write_per_tool() {
    let sb = Sandbox::empty();
    let dir = local_profile(&sb);
    std::fs::create_dir_all(sb.home().join(".claude")).unwrap();
    std::fs::write(sb.home().join(".claude/CLAUDE.md"), "Mine.\n").unwrap();
    let out = sb
        .cli()
        .arg("inspect")
        .arg(&dir)
        .args(["--to", "claude-code,codex"])
        .output()
        .unwrap();
    assert!(out.status.success(), "{}", stdout(&out));
    let all = stdout(&out);
    let claude = all.find("\n  claude-code\n").expect(&all);
    let codex = all.find("\n  codex\n").expect(&all);
    let (claude_part, codex_part) = (&all[claude..codex], &all[codex..]);
    assert!(claude_part.contains("modify  ~/.claude/CLAUDE.md"), "{all}");
    assert!(
        claude_part.contains("create  ~/.claude/rules/style.md"),
        "{all}"
    );
    assert!(claude_part.contains("~/.claude.json"), "{all}");
    assert!(codex_part.contains("create  ~/.codex/AGENTS.md"), "{all}");
    assert!(codex_part.contains("~/.codex/config.toml"), "{all}");
    assert_masked(&all);
    // Nothing written.
    assert_eq!(sb.read("home/.claude/CLAUDE.md"), "Mine.\n");
    assert!(!sb.home().join(".codex").exists());
    assert!(!sb.home().join(".claude.json").exists());
    assert!(!sb.state().exists());
}

#[test]
fn inspect_reads_a_plugin() {
    let sb = Sandbox::empty();
    let dir = sb.path("my-plugin");
    write(
        &dir,
        &[
            (
                "rules/rust.md",
                "---\nglobs: [\"**/*.rs\"]\n---\nNo unwrap.\n",
            ),
            ("commands/fix.md", "Fix it.\n"),
        ],
    );
    let out = sb
        .cli()
        .arg("inspect")
        .arg(&dir)
        .args(["--to", "claude-code"])
        .output()
        .unwrap();
    let all = stdout(&out);
    assert!(out.status.success(), "{all}");
    for want in [
        "Plugin 'my-plugin' from",
        "rust",
        "fix",
        "Nothing to review:",
        "create  ~/.claude/rules/rust.md",
    ] {
        assert!(all.contains(want), "missing {want:?}:\n{all}");
    }
    assert!(!sb.home().join(".claude").exists());
}

#[test]
fn inspect_shows_the_commit_of_a_git_source() {
    if !has_git() {
        return;
    }
    let sb = Sandbox::empty();
    let url = sb.git_profile(
        "repo",
        &[
            ("kitbag.toml", "kitbag = 1\nname = \"g\"\n"),
            ("instructions.md", "Be brief.\n"),
        ],
    );
    let sha = git_out(&sb.path("repo"), &["rev-parse", "HEAD"]);
    // Credentials in a file:// URL are only understood on Unix (see remote.rs).
    let with_creds = if cfg!(unix) {
        url.replacen("file://", &format!("file://bob:{FAKE_PASSWORD}@"), 1)
    } else {
        url.clone()
    };
    let out = sb
        .cli()
        .args([
            "inspect",
            &format!("{with_creds}@{}", &sha[..8]),
            "--to",
            "codex",
        ])
        .output()
        .unwrap();
    let all = stdout(&out);
    assert!(out.status.success(), "{all}");
    assert!(all.contains(&format!("commit {sha}")), "{all}");
    assert!(all.contains("create  ~/.codex/AGENTS.md"), "{all}");
    assert_masked(&all);
    assert!(!sb.home().join(".codex").exists());
    assert!(!sb.state().exists());
}

/// Skill scripts the bag would install as executable are named.
#[cfg(unix)]
#[test]
fn inspect_names_executable_files() {
    use predicates::prelude::*;
    use predicates::str::contains;
    use std::os::unix::fs::PermissionsExt;
    let sb = Sandbox::empty();
    let dir = local_profile(&sb);
    let script = dir.join("skills/deploy/run.sh");
    std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
    sb.cli()
        .arg("inspect")
        .arg(&dir)
        .assert()
        .success()
        .stdout(contains("Executable files:").and(contains("skills/deploy/run.sh")));
    std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o644)).unwrap();
    sb.cli()
        .arg("inspect")
        .arg(&dir)
        .assert()
        .success()
        .stdout(contains("Executable files").not());
}
