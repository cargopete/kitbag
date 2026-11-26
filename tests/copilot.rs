//! GitHub Copilot through the CLI: apply and undo, and the project
//! `.mcp.json` it shares with Claude Code, where a server the profile keeps
//! off one of the two must not be removed while the other still gets it.

mod common;

use std::fs;

use common::*;

const MCP: &str = "project/.mcp.json";

fn write_profile(sb: &Sandbox, heavy_extra: &str) {
    fs::create_dir_all(sb.path("p")).unwrap();
    fs::write(
        sb.path("p/kitbag.toml"),
        format!(
            "kitbag = 1\nname = \"shared\"\n\n[mcp_servers.heavy]\ncommand = \"heavy-mcp\"\nargs = [\"--big\"]\n{heavy_extra}\n[mcp_servers.light]\ncommand = \"light-mcp\"\n"
        ),
    )
    .unwrap();
}

fn apply_project(sb: &Sandbox, to: &str) -> String {
    let out = sb
        .cli()
        .args(["apply", "p", "--project", "project", "--to", to, "--yes"])
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    assert!(
        out.status.success(),
        "{stdout}{}",
        String::from_utf8_lossy(&out.stderr)
    );
    stdout
}

fn has(sb: &Sandbox, name: &str) -> bool {
    sb.json(MCP)["mcpServers"].get(name).is_some()
}

#[test]
fn apply_to_copilot_then_undo() {
    let sb = Sandbox::empty();
    sb.cli().args(["init", "prof"]).assert().success();
    fs::write(
        sb.path("prof/kitbag.toml"),
        "kitbag = 1\nname = \"prof\"\n\n[mcp_servers.docs]\nurl = \"https://docs.example.com/mcp\"\nheaders = { Authorization = \"Bearer ${DOCS_TOKEN}\" }\n",
    )
    .unwrap();
    sb.cli()
        .args(["apply", "prof", "--to", "copilot", "--yes"])
        .env("DOCS_TOKEN", "fake-docs-token")
        .assert()
        .success();
    let cfg = sb.json("home/.copilot/mcp-config.json");
    assert_eq!(
        cfg["mcpServers"]["docs"],
        serde_json::json!({"type": "http", "url": "https://docs.example.com/mcp", "headers": {"Authorization": "Bearer ${DOCS_TOKEN}"}, "tools": ["*"]})
    );
    assert!(
        !sb.read("home/.copilot/mcp-config.json")
            .contains("fake-docs-token"),
        "references stay references"
    );
    assert!(
        sb.read("home/.copilot/copilot-instructions.md")
            .contains("Write instructions every agent should follow")
    );
    sb.cli().arg("undo").assert().success();
    assert!(
        !sb.home().join(".copilot").exists(),
        "undo removes what apply created"
    );
}

#[test]
fn a_server_kept_off_copilot_stays_in_mcp_json_for_claude_code() {
    let sb = Sandbox::empty();
    write_profile(&sb, "");
    apply_project(&sb, "claude-code,copilot");
    assert!(has(&sb, "heavy"));

    // Copilot wrote it too, but Claude Code still gets it from the same file.
    write_profile(&sb, "disabled_on = [\"copilot\"]\n");
    let out = apply_project(&sb, "claude-code,copilot");
    assert!(has(&sb, "heavy"), "{out}");
    assert!(
        out.contains("Claude Code gets it from") && !out.contains("removed from"),
        "{out}"
    );
    // Also when only Copilot is applied: kitbag wrote it there for Claude Code.
    let out = apply_project(&sb, "copilot");
    assert!(has(&sb, "heavy"), "{out}");
    assert!(has(&sb, "light"));
}

#[test]
fn a_server_only_for_copilot_stays_in_mcp_json() {
    let sb = Sandbox::empty();
    write_profile(&sb, "");
    apply_project(&sb, "claude-code,copilot");

    write_profile(&sb, "targets = [\"copilot\"]\n");
    let out = apply_project(&sb, "claude-code,copilot");
    assert!(has(&sb, "heavy"), "{out}");
    assert!(
        out.contains("Claude Code reads .mcp.json too") && !out.contains("removed from"),
        "{out}"
    );
    let again = sb
        .cli()
        .args([
            "diff",
            "p",
            "--project",
            "project",
            "--to",
            "claude-code,copilot",
            "--exit-code",
        ])
        .output()
        .unwrap();
    assert!(again.status.success(), "no changes left to make");
}

#[test]
fn each_target_alone_still_removes_what_it_added() {
    // Claude Code only: Copilot never used the file, so the removal goes ahead.
    let sb = Sandbox::empty();
    write_profile(&sb, "");
    apply_project(&sb, "claude-code");
    write_profile(&sb, "disabled_on = [\"claude-code\"]\n");
    let out = apply_project(&sb, "claude-code");
    assert!(!has(&sb, "heavy"), "{out}");
    assert!(out.contains("removed from"), "{out}");

    // Copilot only, the same.
    let sb = Sandbox::empty();
    write_profile(&sb, "");
    apply_project(&sb, "copilot");
    write_profile(&sb, "disabled_on = [\"copilot\"]\n");
    let out = apply_project(&sb, "copilot");
    assert!(!has(&sb, "heavy"), "{out}");
    assert!(has(&sb, "light"));

    // Disabled for both: removed even with both applied.
    let sb = Sandbox::empty();
    write_profile(&sb, "");
    apply_project(&sb, "claude-code,copilot");
    write_profile(&sb, "disabled_on = [\"claude-code\", \"copilot\"]\n");
    let out = apply_project(&sb, "claude-code,copilot");
    assert!(!has(&sb, "heavy"), "{out}");
}
