//! Safe apply: merge rather than clobber, backups and undo against real fixture configs.

mod common;

use std::fs;

use common::*;
use kitbag::adapters::{ApplyOptions, Scope};
use kitbag::engine;
use kitbag::model::{McpServer, Profile};
use kitbag::txn::Store;

fn profile() -> Profile {
    let mut p = Profile::new("team");
    p.mcp_servers.insert(
        "context7".into(),
        McpServer {
            command: Some("npx".into()),
            args: vec!["-y".into(), "@upstash/context7-mcp@latest".into()],
            env: [(
                "CONTEXT7_API_KEY".to_string(),
                "${CONTEXT7_API_KEY}".to_string(),
            )]
            .into_iter()
            .collect(),
            ..Default::default()
        },
    );
    p.mcp_servers.insert(
        "new-server".into(),
        McpServer {
            command: Some("new-mcp".into()),
            ..Default::default()
        },
    );
    p.instructions = Some("Team instructions.".into());
    p
}

#[test]
fn json_merge_keeps_unrelated_content() {
    let sb = Sandbox::fixtures();
    let before = sb.json("home/.claude.json");
    apply(&sb, &profile(), &["claude-code"], &sb.env(Scope::User));
    let after = sb.json("home/.claude.json");
    for k in ["numStartups", "theme", "hasCompletedOnboarding", "projects"] {
        assert_eq!(after[k], before[k], "{k} preserved");
    }
    for s in ["github", "linear", "postgres"] {
        assert_eq!(
            after["mcpServers"][s], before["mcpServers"][s],
            "existing server {s} untouched"
        );
    }
    assert_eq!(after["mcpServers"]["new-server"]["command"], "new-mcp");
    // Formatting of untouched parts is preserved byte for byte.
    let text = sb.read("home/.claude.json");
    assert!(
        text.starts_with("{\n  \"numStartups\": 42,\n  \"theme\": \"dark\","),
        "{text}"
    );
}

#[test]
fn toml_merge_keeps_comments_and_tool_specific_keys() {
    let sb = Sandbox::fixtures();
    apply(&sb, &profile(), &["codex"], &sb.env(Scope::User));
    let cfg = sb.read("home/.codex/config.toml");
    assert!(
        cfg.starts_with("# My Codex config\nmodel = \"gpt-5-codex\""),
        "{cfg}"
    );
    assert!(
        cfg.contains("# Docs server\n[mcp_servers.context7]"),
        "comment above the table kept: {cfg}"
    );
    assert!(
        cfg.contains("startup_timeout_sec = 20"),
        "keys kitbag doesn't manage are preserved: {cfg}"
    );
    assert!(
        cfg.contains("\"@upstash/context7-mcp@latest\""),
        "managed keys updated: {cfg}"
    );
    assert!(cfg.contains("[features]\nweb_search_request = true"));
    assert!(cfg.contains("[mcp_servers.legacy]"));
    assert!(cfg.contains("[mcp_servers.new-server]"));
}

#[test]
fn json_merge_keeps_tool_specific_server_keys() {
    let sb = Sandbox::fixtures();
    let mut p = Profile::new("x");
    p.mcp_servers.insert(
        "weather".into(),
        McpServer {
            command: Some("node".into()),
            args: vec!["/opt/weather/v2.js".into()],
            ..Default::default()
        },
    );
    apply(&sb, &p, &["gemini-cli"], &sb.env(Scope::User));
    let s = sb.json("home/.gemini/settings.json");
    assert_eq!(
        s["mcpServers"]["weather"]["timeout"], 30000,
        "gemini-only key kept"
    );
    assert_eq!(s["mcpServers"]["weather"]["args"][0], "/opt/weather/v2.js");
    assert!(
        s["mcpServers"]["weather"].get("env").is_none(),
        "managed key removed when the profile drops it"
    );
    assert_eq!(s["theme"], "GitHub");
}

#[test]
fn no_overwrite_keeps_existing_entries() {
    let sb = Sandbox::fixtures();
    let before = sb.read("home/.codex/config.toml");
    let prof = profile();
    let o = ApplyOptions {
        overwrite: false,
        ..opts(&prof)
    };
    let p = engine::plan(&prof, &targets(&["codex"]), &sb.env(Scope::User), &o).unwrap();
    engine::commit(&p, "t", &sb.state()).unwrap();
    let after = sb.read("home/.codex/config.toml");
    assert!(
        after.contains("\"@upstash/context7-mcp\"]"),
        "existing definition kept"
    );
    assert!(
        after.contains("[mcp_servers.new-server]"),
        "new servers still added"
    );
    assert!(
        p.notes
            .iter()
            .any(|n| n.message.contains("kept existing MCP server 'context7'"))
    );
    assert_ne!(before, after);
}

#[test]
fn managed_blocks_leave_user_text_alone_and_update_in_place() {
    let sb = Sandbox::fixtures();
    let env = sb.env(Scope::User);
    apply(&sb, &profile(), &["codex"], &env);
    let md = sb.read("home/.codex/AGENTS.md");
    assert!(
        md.starts_with("# Codex global instructions\n\nAlways run `make lint` before finishing.\n")
    );
    assert!(md.contains("Team instructions."));

    let mut p2 = profile();
    p2.instructions = Some("Updated team instructions.".into());
    apply(&sb, &p2, &["codex"], &env);
    let md2 = sb.read("home/.codex/AGENTS.md");
    assert_eq!(md2.matches("kitbag:begin team -->").count(), 1);
    assert!(md2.contains("Updated team instructions.") && !md2.contains("\nTeam instructions."));
    assert!(md2.starts_with("# Codex global instructions\n"));
}

#[test]
fn undo_restores_every_file_exactly() {
    let sb = Sandbox::fixtures();
    let env = sb.env(Scope::User);
    let snapshot = |rel: &str| fs::read(sb.path(rel)).ok();
    let files = [
        "home/.claude.json",
        "home/.claude/CLAUDE.md",
        "home/.codex/config.toml",
        "home/.codex/AGENTS.md",
        "home/.cursor/mcp.json",
        "home/.gemini/settings.json",
        "home/.gemini/GEMINI.md",
        "home/.config/Claude/claude_desktop_config.json",
    ];
    let before: Vec<_> = files.iter().map(|f| snapshot(f)).collect();
    let all = [
        "claude-code",
        "cursor",
        "codex",
        "gemini-cli",
        "claude-desktop",
    ];
    let p = apply(&sb, &profile(), &all, &env);
    assert!(p.changes.len() >= 6);
    let changed: Vec<_> = files.iter().map(|f| snapshot(f)).collect();
    assert_ne!(before, changed);

    Store::new(sb.state()).undo(None, false).unwrap();
    let after: Vec<_> = files.iter().map(|f| snapshot(f)).collect();
    assert_eq!(before, after, "byte-identical after undo");
    assert!(!sb.home().join(".codex/prompts/new-server.md").exists());
}

#[test]
fn backups_are_kept_in_the_state_dir() {
    let sb = Sandbox::fixtures();
    apply(&sb, &profile(), &["claude-code"], &sb.env(Scope::User));
    let txs = Store::new(sb.state()).list().unwrap();
    assert_eq!(txs.len(), 1);
    let tx = &txs[0];
    let entry = tx
        .entries
        .iter()
        .find(|e| e.path.ends_with(".claude.json"))
        .unwrap();
    let backup = sb
        .state()
        .join("backups")
        .join(&tx.id)
        .join(entry.backup.as_ref().unwrap());
    assert_eq!(
        fs::read(backup).unwrap(),
        fs::read(common::fixtures().join("home/.claude.json")).unwrap()
    );
}

#[test]
fn invalid_existing_config_fails_before_writing_anything() {
    let sb = Sandbox::fixtures();
    fs::write(sb.home().join(".cursor/mcp.json"), "{ not json").unwrap();
    let e = engine::plan(
        &profile(),
        &targets(&["claude-code", "cursor"]),
        &sb.env(Scope::User),
        &opts(&profile()),
    )
    .unwrap_err();
    assert!(format!("{e:#}").contains("mcp.json: invalid JSON"), "{e:#}");
    assert_eq!(
        sb.json("home/.claude.json"),
        serde_json::from_str::<serde_json::Value>(
            &fs::read_to_string(common::fixtures().join("home/.claude.json")).unwrap()
        )
        .unwrap()
    );
}
