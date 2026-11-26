//! Regression tests for the second part of the #38 correctness fixes.

mod common;

use std::fs;

use common::*;
use predicates::str::contains;

const SERVER_PROFILE: &str = "kitbag = 1\nname = \"me\"\n\n[mcp_servers.docs]\ncommand = \"npx\"\nargs = [\"-y\", \"docs-mcp\"]\n";

fn server_profile(sb: &Sandbox) {
    fs::create_dir_all(sb.path("p")).unwrap();
    fs::write(sb.path("p/kitbag.toml"), SERVER_PROFILE).unwrap();
}

#[test]
fn duplicate_json_keys_are_refused_naming_file_and_key() {
    let sb = Sandbox::empty();
    server_profile(&sb);
    let config = "{\n  \"mcpServers\": {},\n  \"theme\": \"dark\",\n  \"mcpServers\": {\"old\": {\"command\": \"x\"}}\n}\n";
    fs::write(sb.home().join(".claude.json"), config).unwrap();
    for cmd in [
        vec!["diff", "p", "--to", "claude-code", "--exit-code"],
        vec!["apply", "p", "--to", "claude-code", "--yes"],
    ] {
        sb.cli()
            .args(&cmd)
            .assert()
            .failure()
            .stderr(contains(".claude.json"))
            .stderr(contains("key 'mcpServers' appears more than once"));
    }
    assert_eq!(sb.read("home/.claude.json"), config);

    // A duplicate inside a server definition is refused too.
    let config = "{\"mcpServers\": {\"docs\": {\"command\": \"a\", \"command\": \"b\"}}}\n";
    fs::write(sb.home().join(".claude.json"), config).unwrap();
    sb.cli()
        .args(["apply", "p", "--to", "claude-code", "--yes"])
        .assert()
        .failure()
        .stderr(contains(
            "key 'command' appears more than once in 'mcpServers.docs'",
        ));
    assert_eq!(sb.read("home/.claude.json"), config);
}

fn no_bare_lf(text: &str) -> bool {
    !text.replace("\r\n", "").contains('\n')
}

#[test]
fn crlf_files_keep_their_line_endings() {
    let sb = Sandbox::empty();
    server_profile(&sb);
    fs::write(sb.path("p/instructions.md"), "Be brief.\n\nUse tabs.\n").unwrap();
    let home = sb.home();
    fs::create_dir_all(home.join(".claude")).unwrap();
    fs::create_dir_all(home.join(".codex")).unwrap();
    fs::write(
        home.join(".claude.json"),
        "{\r\n  \"theme\": \"dark\"\r\n}\r\n",
    )
    .unwrap();
    fs::write(home.join(".claude/CLAUDE.md"), "# Mine\r\n\r\nKeep me.\r\n").unwrap();
    fs::write(
        home.join(".codex/config.toml"),
        "# mine\r\nmodel = \"o3\"\r\n",
    )
    .unwrap();
    fs::write(home.join(".codex/AGENTS.md"), "# Mine\r\n").unwrap();
    sb.cli()
        .args(["apply", "p", "--to", "claude-code,codex", "--yes"])
        .assert()
        .success();
    for rel in [
        "home/.claude.json",
        "home/.claude/CLAUDE.md",
        "home/.codex/config.toml",
        "home/.codex/AGENTS.md",
    ] {
        let text = sb.read(rel);
        assert!(no_bare_lf(&text), "{rel}: {text:?}");
        assert!(
            text.contains("docs-mcp") || text.contains("Use tabs."),
            "{rel}: {text:?}"
        );
    }
    assert!(
        sb.read("home/.claude/CLAUDE.md")
            .starts_with("# Mine\r\n\r\nKeep me.\r\n")
    );
    assert!(
        sb.read("home/.codex/config.toml")
            .starts_with("# mine\r\nmodel = \"o3\"\r\n")
    );
    // Re-applying changes nothing.
    sb.cli()
        .args(["diff", "p", "--to", "claude-code,codex", "--exit-code"])
        .assert()
        .success();
    sb.cli()
        .args(["status", "p", "--exit-code"])
        .assert()
        .success();
}

#[test]
fn reserved_and_case_clashing_names_are_refused_on_load() {
    let sb = Sandbox::empty();
    server_profile(&sb);
    fs::create_dir_all(sb.path("p/skills/lint")).unwrap();
    fs::write(sb.path("p/skills/lint/SKILL.md"), "Lint.\n").unwrap();
    // `aux.sh` can't be created on Windows; the manifest check is the same there.
    let reserved_file = if cfg!(windows) {
        None
    } else {
        Some("p/skills/lint/aux.sh")
    };
    if let Some(rel) = reserved_file {
        fs::write(sb.path(rel), "echo\n").unwrap();
        sb.cli()
            .args(["apply", "p", "--to", "claude-code", "--yes"])
            .assert()
            .failure()
            .stderr(contains(
                "skills/lint/aux.sh: reserved device name on Windows",
            ));
        fs::remove_file(sb.path(rel)).unwrap();
    }
    sb.cli()
        .args(["apply", "p", "--to", "claude-code", "--dry-run"])
        .assert()
        .success();

    // Two command files that differ only in case, where the file system allows it.
    fs::create_dir_all(sb.path("p/commands")).unwrap();
    fs::write(sb.path("p/commands/Build.md"), "Build it.\n").unwrap();
    fs::write(sb.path("p/commands/build.md"), "Build it too.\n").unwrap();
    if fs::read_dir(sb.path("p/commands")).unwrap().count() == 2 {
        sb.cli()
            .args(["apply", "p", "--to", "claude-code", "--yes"])
            .assert()
            .failure()
            .stderr(contains(
                "commands/Build and commands/build: names differ only in case",
            ));
        assert!(!sb.home().join(".claude/commands").exists());
    }
}

#[cfg(not(windows))]
#[test]
fn capture_refuses_a_reserved_command_name() {
    let sb = Sandbox::empty();
    let dir = sb.home().join(".claude/commands");
    fs::create_dir_all(&dir).unwrap();
    fs::write(dir.join("nul.md"), "Nothing.\n").unwrap();
    fs::write(dir.join("ok.md"), "Fine.\n").unwrap();
    sb.cli()
        .args(["capture", "--from", "claude-code", "-o", "prof"])
        .assert()
        .failure()
        .stderr(contains("commands/nul: reserved device name on Windows"));
    assert!(!sb.path("prof/kitbag.toml").exists());
}

#[test]
fn home_without_state_dir_keeps_state_under_that_home() {
    let sb = Sandbox::empty();
    fs::create_dir_all(sb.path("p")).unwrap();
    fs::write(sb.path("p/kitbag.toml"), "kitbag = 1\nname = \"me\"\n").unwrap();
    fs::write(sb.path("p/instructions.md"), "Be brief.\n").unwrap();
    let elsewhere = sb.path("elsewhere");
    let cli = || {
        let mut c = sb.cli();
        c.env_remove("KITBAG_STATE_DIR")
            .env("XDG_STATE_HOME", &elsewhere)
            .env("LOCALAPPDATA", &elsewhere);
        c
    };
    cli()
        .args(["apply", "p", "--to", "claude-code", "--yes"])
        .assert()
        .success();
    assert!(sb.home().join(".local/state/kitbag/backups").is_dir());
    assert!(!elsewhere.exists() && !sb.state().exists());
    cli()
        .arg("history")
        .assert()
        .success()
        .stdout(contains("claude-code"));
    cli().arg("undo").assert().success();
    assert!(!sb.home().join(".claude/CLAUDE.md").exists());
    // KITBAG_STATE_DIR still wins over --home.
    sb.cli()
        .args(["apply", "p", "--to", "claude-code", "--yes"])
        .assert()
        .success();
    assert!(sb.state().join("backups").is_dir());
}
