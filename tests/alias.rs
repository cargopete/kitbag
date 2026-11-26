//! `claude` is an alias for the `claude-code` target (#55): accepted wherever
//! a target id is read, never written or printed, and never `claude-desktop`.

mod common;

use std::fs;

use common::*;
use predicates::prelude::*;
use predicates::str::contains;

const MANIFEST: &str = "kitbag = 1\nname = \"me\"\n";

/// A local profile `p` with instructions and the given MCP server section.
fn write_profile(sb: &Sandbox, servers: &str) {
    fs::create_dir_all(sb.path("p")).unwrap();
    fs::write(sb.path("p/kitbag.toml"), format!("{MANIFEST}{servers}")).unwrap();
    fs::write(sb.path("p/instructions.md"), "Be brief.\n").unwrap();
}

fn stdout(sb: &Sandbox, args: &[&str]) -> String {
    let out = sb.cli().args(args).output().unwrap();
    let text = String::from_utf8_lossy(&out.stdout).into_owned();
    assert!(
        out.status.success(),
        "{args:?} failed:\n{text}{}",
        String::from_utf8_lossy(&out.stderr)
    );
    text
}

/// Every target id recorded in the state directory's JSON files.
fn state_text(sb: &Sandbox) -> String {
    let mut all = String::new();
    for e in fs::read_dir(sb.state()).unwrap() {
        let p = e.unwrap().path();
        if p.extension().is_some_and(|x| x == "json") {
            all.push_str(&fs::read_to_string(p).unwrap());
        }
    }
    all
}

#[test]
fn apply_to_claude_means_claude_code() {
    let sb = Sandbox::empty();
    write_profile(&sb, "\n[mcp_servers.docs]\ncommand = \"docs-mcp\"\n");
    let out = stdout(&sb, &["apply", "p", "--to", "claude", "--yes"]);
    assert!(out.contains("claude-code"), "{out}");
    assert!(sb.read("home/.claude/CLAUDE.md").contains("Be brief."));
    assert!(sb.json("home/.claude.json")["mcpServers"]["docs"].is_object());
    // Never Claude Desktop.
    assert!(!sb.path(desktop_config()).exists());
    let state = state_text(&sb);
    assert!(state.contains("\"claude-code\""), "{state}");
    assert!(!state.contains("\"claude\""), "{state}");
}

#[test]
fn alias_and_id_together_are_one_target() {
    let sb = Sandbox::empty();
    write_profile(&sb, "");
    let out = stdout(&sb, &["diff", "p", "--to", "claude,claude-code"]);
    assert!(out.contains("claude-code"), "{out}");
    let json = stdout(&sb, &["status", "p", "--to", "claude", "--json"]);
    assert!(json.contains("\"claude-code\""), "{json}");
    assert!(!json.contains("\"claude\""), "{json}");
}

#[test]
fn the_alias_is_case_sensitive() {
    let sb = Sandbox::empty();
    write_profile(&sb, "");
    sb.cli()
        .args(["diff", "p", "--to", "Claude"])
        .assert()
        .code(2)
        .stderr(contains("unknown target 'Claude'"));
}

#[test]
fn doctor_to_claude_checks_claude_code_only() {
    let sb = Sandbox::empty();
    sb.cli()
        .args(["doctor", "--to", "claude"])
        .assert()
        .success()
        .stdout(contains("Claude Code").and(contains("Claude Desktop").not()));
}

#[test]
fn capture_from_claude_writes_claude_code() {
    let sb = Sandbox::fixtures();
    // An existing profile whose targeting uses the alias: capture --force keeps
    // it, under the canonical name.
    fs::create_dir_all(sb.path("out")).unwrap();
    fs::write(
        sb.path("out/kitbag.toml"),
        "kitbag = 1\nname = \"out\"\n\n[mcp_servers.linear]\nurl = \"https://mcp.linear.app/mcp\"\ntargets = [\"claude\"]\n",
    )
    .unwrap();
    stdout(
        &sb,
        &["capture", "--from", "claude", "-o", "out", "--force"],
    );
    let manifest = sb.read("out/kitbag.toml");
    assert!(
        manifest.contains("targets = [\"claude-code\"]"),
        "{manifest}"
    );
    assert!(!manifest.contains("\"claude\""), "{manifest}");
    assert!(manifest.contains("[mcp_servers.github]"), "{manifest}");
}

#[test]
fn sync_from_claude_pulls_back_claude_code_edits() {
    let sb = Sandbox::empty();
    write_profile(&sb, "");
    stdout(&sb, &["apply", "p", "--to", "claude-code,codex", "--yes"]);
    let path = sb.path("home/.claude/CLAUDE.md");
    let text = fs::read_to_string(&path).unwrap();
    fs::write(&path, text.replace("Be brief.", "Be very brief.")).unwrap();
    stdout(
        &sb,
        &[
            "sync",
            "p",
            "--to",
            "claude-code,codex",
            "--from",
            "claude",
            "--yes",
        ],
    );
    assert!(sb.read("p/instructions.md").contains("Be very brief."));
    assert!(sb.read("home/.codex/AGENTS.md").contains("Be very brief."));
    // And the other way round: --to claude, --from claude-code.
    let text = fs::read_to_string(&path).unwrap();
    fs::write(&path, text.replace("Be very brief.", "Be terse.")).unwrap();
    stdout(
        &sb,
        &[
            "sync",
            "p",
            "--to",
            "claude",
            "--from",
            "claude-code",
            "--yes",
        ],
    );
    assert!(sb.read("p/instructions.md").contains("Be terse."));
}

#[test]
fn profile_targeting_accepts_the_alias() {
    let sb = Sandbox::empty();
    write_profile(
        &sb,
        "\n[mcp_servers.only_claude]\ncommand = \"a-mcp\"\ntargets = [\"claude\"]\n\
         \n[mcp_servers.not_claude]\ncommand = \"b-mcp\"\ndisabled_on = [\"claude\"]\n\
         \n[mcp_servers.legacy]\ncommand = \"c-mcp\"\nonly = [\"claude\"]\n\
         \n[mcp_servers.legacy_except]\ncommand = \"d-mcp\"\nexcept = [\"claude\"]\n",
    );
    let out = sb
        .cli()
        .args(["apply", "p", "--to", "claude-code,cursor", "--yes"])
        .output()
        .unwrap();
    let all = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(out.status.success(), "{all}");
    assert!(!all.contains("unknown target"), "{all}");
    let claude = &sb.json("home/.claude.json")["mcpServers"];
    let cursor = &sb.json("home/.cursor/mcp.json")["mcpServers"];
    for name in ["only_claude", "legacy"] {
        assert!(claude.get(name).is_some(), "{name} on claude-code: {all}");
        assert!(cursor.get(name).is_none(), "{name} on cursor: {all}");
    }
    for name in ["not_claude", "legacy_except"] {
        assert!(claude.get(name).is_none(), "{name} on claude-code: {all}");
        assert!(cursor.get(name).is_some(), "{name} on cursor: {all}");
    }
    let state = state_text(&sb);
    assert!(!state.contains("\"claude\""), "{state}");
}

#[test]
fn disabling_claude_leaves_claude_desktop_alone() {
    let sb = Sandbox::empty();
    write_profile(
        &sb,
        "\n[mcp_servers.docs]\ncommand = \"docs-mcp\"\ndisabled_on = [\"claude\"]\n",
    );
    stdout(
        &sb,
        &["apply", "p", "--to", "claude-code,claude-desktop", "--yes"],
    );
    let desktop: serde_json::Value = serde_json::from_str(&sb.read(desktop_config())).unwrap();
    assert!(desktop["mcpServers"]["docs"].is_object(), "{desktop}");
    let claude = sb.path("home/.claude.json");
    assert!(
        !claude.exists()
            || sb.json("home/.claude.json")["mcpServers"]
                .get("docs")
                .is_none()
    );
}

#[test]
fn help_mentions_the_alias() {
    let sb = Sandbox::empty();
    for cmd in ["apply", "status", "sync"] {
        let out = sb.cli().args([cmd, "--help"]).assert().success();
        // clap wraps help to the terminal width, so compare with whitespace collapsed.
        let text = String::from_utf8_lossy(&out.get_output().stdout)
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ");
        assert!(text.contains("alias: claude"), "{cmd} --help: {text}");
    }
}

#[test]
fn settings_under_the_alias_apply_to_claude_code() {
    for to in ["claude", "claude-code"] {
        let sb = Sandbox::empty();
        write_profile(&sb, "\n[settings.claude]\nmodel = \"opus\"\n");
        stdout(&sb, &["apply", "p", "--to", to, "--yes"]);
        let settings = sb.json("home/.claude/settings.json");
        assert_eq!(settings["model"], "opus", "--to {to}: {settings}");
        assert!(!state_text(&sb).contains("\"claude\""));
    }
}

#[test]
fn settings_under_both_names_is_an_error() {
    let sb = Sandbox::empty();
    write_profile(
        &sb,
        "\n[settings.claude]\nmodel = \"opus\"\n\n[settings.claude-code]\nmodel = \"sonnet\"\n",
    );
    sb.cli()
        .args(["apply", "p", "--to", "claude-code", "--yes"])
        .assert()
        .failure()
        .stderr(contains(
            "both [settings.claude] and [settings.claude-code] are set",
        ));
    assert!(!sb.path("home/.claude/settings.json").exists());
}

#[test]
fn doctor_reads_settings_under_the_alias_as_claude_code() {
    let sb = Sandbox::empty();
    write_profile(
        &sb,
        "\n[settings.claude]\nmodel = \"opus\"\n\n[settings.vim]\nmodel = \"x\"\n",
    );
    let out = sb.cli().args(["doctor", "p"]).output().unwrap();
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(!text.contains("settings.claude:"), "{text}");
    assert!(
        !text.contains("settings.claude-code: unknown target"),
        "{text}"
    );
    assert!(
        !text.contains("settings.claude-code: will not apply"),
        "{text}"
    );
    // A key that is no alias is still an unknown target.
    assert!(text.contains("settings.vim: unknown target"), "{text}");
}
