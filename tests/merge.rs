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
    // Same command and args as the existing entry, one more env var.
    let mut p = profile();
    let c7 = p.mcp_servers.get_mut("context7").unwrap();
    c7.args = vec!["-y".into(), "@upstash/context7-mcp".into()];
    c7.env.insert("C7_LOG".into(), "info".into());
    apply(&sb, &p, &["codex"], &sb.env(Scope::User));
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
        cfg.contains("C7_LOG = \"info\""),
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
            args: vec!["/opt/weather/index.js".into()],
            ..Default::default()
        },
    );
    apply(&sb, &p, &["gemini-cli"], &sb.env(Scope::User));
    let s = sb.json("home/.gemini/settings.json");
    assert_eq!(
        s["mcpServers"]["weather"]["timeout"], 30000,
        "gemini-only key kept"
    );
    assert_eq!(
        s["mcpServers"]["weather"]["args"][0],
        "/opt/weather/index.js"
    );
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

#[test]
fn no_overwrite_protects_nested_settings_too() {
    let sb = Sandbox::empty();
    let settings = sb.home().join(".claude/settings.json");
    fs::create_dir_all(settings.parent().unwrap()).unwrap();
    fs::write(
        &settings,
        r#"{"permissions": {"defaultMode": "default", "allow": ["A"]}}"#,
    )
    .unwrap();
    let mut p = Profile::new("x");
    p.settings.insert(
        "claude-code".into(),
        serde_json::json!({"permissions": {"defaultMode": "bypassPermissions", "allow": ["B"]}})
            .as_object()
            .cloned()
            .unwrap(),
    );
    let o = ApplyOptions {
        overwrite: false,
        ..opts(&p)
    };
    let pl = engine::plan(&p, &targets(&["claude-code"]), &sb.env(Scope::User), &o).unwrap();
    engine::commit(&pl, "t", &sb.state()).unwrap();
    let v = sb.json("home/.claude/settings.json");
    assert_eq!(v["permissions"]["defaultMode"], "default");
    assert_eq!(v["permissions"]["allow"], serde_json::json!(["A", "B"]));
}

#[cfg(unix)]
#[test]
fn symlinked_config_files_stay_symlinks() {
    let sb = Sandbox::empty();
    let real = sb.path("dotfiles/CLAUDE.md");
    fs::create_dir_all(real.parent().unwrap()).unwrap();
    fs::write(&real, "# Mine\n").unwrap();
    fs::create_dir_all(sb.home().join(".claude")).unwrap();
    let link = sb.home().join(".claude/CLAUDE.md");
    std::os::unix::fs::symlink(&real, &link).unwrap();
    apply(&sb, &profile(), &["claude-code"], &sb.env(Scope::User));
    assert!(
        fs::symlink_metadata(&link)
            .unwrap()
            .file_type()
            .is_symlink()
    );
    assert!(
        fs::read_to_string(&real)
            .unwrap()
            .contains("Team instructions.")
    );
    Store::new(sb.state()).undo(None, false).unwrap();
    assert!(
        fs::symlink_metadata(&link)
            .unwrap()
            .file_type()
            .is_symlink()
    );
    assert_eq!(fs::read_to_string(&real).unwrap(), "# Mine\n");
}

fn one_server(name: &str, s: McpServer) -> Profile {
    let mut p = Profile::new("x");
    p.mcp_servers.insert(name.into(), s);
    p
}

#[test]
fn a_server_with_a_new_command_or_url_does_not_inherit_trust_or_credentials() {
    let sb = Sandbox::empty();
    let env = sb.env(Scope::User);
    fs::create_dir_all(sb.home().join(".gemini")).unwrap();
    fs::write(
        sb.home().join(".gemini/settings.json"),
        r#"{"mcpServers":{"docs":{"command":"trusted-docs","trust":true,"timeout":5000}}}"#,
    )
    .unwrap();
    fs::write(
        sb.home().join(".claude.json"),
        r#"{"mcpServers":{"api":{"type":"http","url":"https://good.example/mcp","headersHelper":"/usr/local/bin/get-token.sh"}}}"#,
    )
    .unwrap();
    fs::create_dir_all(sb.home().join(".codex")).unwrap();
    fs::write(
        sb.home().join(".codex/config.toml"),
        "[mcp_servers.api]\nurl = \"https://good.example/mcp\"\nbearer_token_env_var = \"GOOD_TOKEN\"\noauth_resource = \"https://good.example\"\n",
    )
    .unwrap();

    let evil = one_server(
        "docs",
        McpServer {
            command: Some("evil-binary".into()),
            ..Default::default()
        },
    );
    let plan = apply(&sb, &evil, &["gemini-cli"], &env);
    assert_eq!(
        sb.json("home/.gemini/settings.json")["mcpServers"]["docs"],
        serde_json::json!({"command": "evil-binary"})
    );
    assert!(
        plan.notes
            .iter()
            .any(|n| n.message.contains("'docs'") && n.message.contains("trust, timeout")),
        "{:?}",
        plan.notes
    );

    let evil = one_server(
        "api",
        McpServer {
            url: Some("https://evil.example/mcp".into()),
            ..Default::default()
        },
    );
    apply(&sb, &evil, &["claude-code", "codex"], &env);
    assert_eq!(
        sb.json("home/.claude.json")["mcpServers"]["api"],
        serde_json::json!({"type": "http", "url": "https://evil.example/mcp"})
    );
    let codex = sb.read("home/.codex/config.toml");
    assert!(
        !codex.contains("GOOD_TOKEN") && !codex.contains("oauth_resource"),
        "{codex}"
    );

    // Same command, different package: a different server.
    let evil_pkg = one_server(
        "docs",
        McpServer {
            command: Some("npx".into()),
            args: vec!["-y".into(), "evil-pkg".into()],
            ..Default::default()
        },
    );
    fs::write(
        sb.home().join(".gemini/settings.json"),
        r#"{"mcpServers":{"docs":{"command":"npx","args":["-y","good-pkg"],"trust":true}}}"#,
    )
    .unwrap();
    apply(&sb, &evil_pkg, &["gemini-cli"], &env);
    assert_eq!(
        sb.json("home/.gemini/settings.json")["mcpServers"]["docs"],
        serde_json::json!({"command": "npx", "args": ["-y", "evil-pkg"]})
    );

    // Same command and args, up to ${VAR} references: the user's own keys stay.
    let same = one_server(
        "docs",
        McpServer {
            command: Some("npx".into()),
            args: vec![
                "-y".into(),
                "good-pkg".into(),
                "--key=${NEW_KEY}".into(),
                "${TOKEN}".into(),
            ],
            env: [("A".to_string(), "1".to_string())].into_iter().collect(),
            ..Default::default()
        },
    );
    fs::write(
        sb.home().join(".gemini/settings.json"),
        r#"{"mcpServers":{"docs":{"command":"npx","args":["-y","good-pkg","--key=${OLD_KEY}","FAKE-resolved-value"],"trust":true}}}"#,
    )
    .unwrap();
    apply(&sb, &same, &["gemini-cli"], &env);
    let docs = &sb.json("home/.gemini/settings.json")["mcpServers"]["docs"];
    assert_eq!(docs["trust"], true, "{docs}");
    assert_eq!(docs["env"]["A"], "1", "{docs}");
}
