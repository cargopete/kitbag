//! Per-adapter fixture tests: capture real-world-shaped configs, and apply into
//! empty and existing setups.

mod common;

use common::*;
use kitbag::adapters::Scope;
use kitbag::model::{McpServer, Profile, Rule, Transport};
use kitbag::txn::Store;
use serde_json::json;

// ---------------------------------------------------------------- capture

#[test]
fn claude_code_user_capture() {
    let sb = Sandbox::fixtures();
    let r = capture(&["claude-code"], &sb.env(Scope::User));
    let p = &r.profile;

    let gh = &p.mcp_servers["github"];
    assert_eq!(gh.command.as_deref(), Some("npx"));
    assert_eq!(
        gh.env["GITHUB_PERSONAL_ACCESS_TOKEN"],
        "${GITHUB_PERSONAL_ACCESS_TOKEN}"
    );
    let linear = &p.mcp_servers["linear"];
    assert_eq!(linear.transport(), Transport::Http);
    assert_eq!(linear.headers["Authorization"], "Bearer ${LINEAR_API_KEY}");
    assert_eq!(
        p.mcp_servers["postgres"].args[2],
        "postgresql://admin:${POSTGRES_PASSWORD}@db.internal:5432/app"
    );
    assert_eq!(p.mcp_servers["postgres"].env["LOG_LEVEL"], "debug");

    assert!(
        p.instructions
            .as_deref()
            .unwrap()
            .contains("British English")
    );
    assert_eq!(p.rules.len(), 1);
    assert_eq!(p.rules[0].globs, vec!["src/**/*.ts"]);
    let review = &p.commands[0];
    assert_eq!(review.description.as_deref(), Some("Review staged changes"));
    assert_eq!(review.argument_hint.as_deref(), Some("[focus area]"));
    assert_eq!(review.extra["allowed-tools"], json!("Bash(git diff:*)"));
    assert_eq!(p.skills[0].name, "pdf-tools");
    assert_eq!(p.skills[0].files.len(), 2);

    // Allow-listed settings only; env (with an API key) is never captured.
    assert_eq!(
        serde_json::Value::Object(p.settings["claude-code"].clone()),
        json!({"model": "opus", "permissions": {"allow": ["Bash(npm test:*)"], "deny": ["Read(./.env)"]}})
    );
    let locations: Vec<&str> = r.redactions.iter().map(|x| x.location.as_str()).collect();
    assert_eq!(
        locations,
        vec![
            "mcp_servers.github.env.GITHUB_PERSONAL_ACCESS_TOKEN",
            "mcp_servers.postgres.args[2]"
        ]
    );
}

#[test]
fn claude_code_project_capture_includes_local_scope_servers() {
    let sb = Sandbox::fixtures();
    // ~/.claude.json keeps "local" servers per project path.
    let cj = sb.home().join(".claude.json");
    let mut doc: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&cj).unwrap()).unwrap();
    doc["projects"][sb.project().to_string_lossy().as_ref()] =
        json!({"mcpServers": {"local-db": {"type": "stdio", "command": "db-mcp", "args": []}}});
    std::fs::write(&cj, serde_json::to_string_pretty(&doc).unwrap()).unwrap();

    let p = capture(&["claude-code"], &sb.env(Scope::Project)).profile;
    assert!(p.mcp_servers.contains_key("playwright"));
    assert!(p.mcp_servers.contains_key("local-db"));
    assert_eq!(
        p.instructions.as_deref(),
        Some("# App\n\nThis is a Next.js app. Use pnpm.")
    );
    assert_eq!(p.commands[0].name, "deploy");
}

#[test]
fn cursor_capture() {
    let sb = Sandbox::fixtures();
    let p = capture(&["cursor"], &sb.env(Scope::User)).profile;
    // ${env:VAR} is normalised to ${VAR}; the literal Sentry token is redacted.
    assert_eq!(
        p.mcp_servers["brave-search"].env["BRAVE_API_KEY"],
        "${BRAVE_API_KEY}"
    );
    assert_eq!(
        p.mcp_servers["sentry"].headers["X-Api-Key"],
        "${SENTRY_X_API_KEY}"
    );
    assert_eq!(p.commands[0].name, "fix-tests");
    assert!(p.rules.is_empty(), "Cursor user rules are UI-only");

    let p = capture(&["cursor"], &sb.env(Scope::Project)).profile;
    let py = p.rules.iter().find(|r| r.name == "python").unwrap();
    assert_eq!(py.globs, vec!["*.py", "scripts/**/*.py"]);
    assert_eq!(py.description.as_deref(), Some("Python style"));
    assert_eq!(py.always_apply, None);
    assert!(p.instructions.as_deref().unwrap().contains("pnpm test"));
}

#[test]
fn codex_capture() {
    let sb = Sandbox::fixtures();
    let r = capture(&["codex"], &sb.env(Scope::User));
    let p = &r.profile;
    assert_eq!(
        p.mcp_servers["context7"].env["CONTEXT7_API_KEY"],
        "${CONTEXT7_API_KEY}"
    );
    assert_eq!(
        p.mcp_servers["figma"].headers["Authorization"],
        "Bearer ${FIGMA_TOKEN}"
    );
    assert!(p.mcp_servers["legacy"].disabled);
    assert_eq!(p.mcp_servers["legacy"].env["MODE"], "safe");
    assert_eq!(
        serde_json::Value::Object(p.settings["codex"].clone()),
        json!({"model": "gpt-5-codex", "model_reasoning_effort": "high", "approval_policy": "on-request"})
    );
    assert_eq!(p.commands[0].name, "explain");
    assert!(p.instructions.as_deref().unwrap().contains("make lint"));
    assert!(
        r.notes
            .iter()
            .any(|n| n.message.contains("startup_timeout_sec"))
    );
}

#[test]
fn gemini_capture() {
    let sb = Sandbox::fixtures();
    let p = capture(&["gemini-cli"], &sb.env(Scope::User)).profile;
    assert_eq!(
        p.mcp_servers["weather"].env["WEATHER_API_KEY"],
        "${WEATHER_API_KEY}"
    );
    assert_eq!(
        p.mcp_servers["docs"].url.as_deref(),
        Some("https://docs.example.com/mcp")
    );
    assert_eq!(p.mcp_servers["docs"].transport(), Transport::Http);
    assert_eq!(
        p.commands[0].body,
        "Write a changelog entry for: $ARGUMENTS"
    );
    assert_eq!(p.instructions.as_deref(), Some("Prefer small functions."));
}

#[test]
fn claude_desktop_capture() {
    let sb = Sandbox::fixtures();
    let p = capture(&["claude-desktop"], &sb.env(Scope::User)).profile;
    assert_eq!(p.mcp_servers.len(), 1);
    assert_eq!(
        p.mcp_servers["memory"].args,
        vec!["-y", "@modelcontextprotocol/server-memory"]
    );
}

#[test]
fn capture_merges_targets_first_wins() {
    let sb = Sandbox::fixtures();
    let r = capture(
        &[
            "claude-code",
            "cursor",
            "codex",
            "gemini-cli",
            "claude-desktop",
        ],
        &sb.env(Scope::User),
    );
    let names: Vec<&str> = r.profile.mcp_servers.keys().map(String::as_str).collect();
    assert_eq!(
        names,
        vec![
            "brave-search",
            "context7",
            "docs",
            "figma",
            "github",
            "legacy",
            "linear",
            "memory",
            "postgres",
            "sentry",
            "weather"
        ]
    );
    let instr = r.profile.instructions.unwrap();
    assert!(
        instr.contains("British English")
            && instr.contains("make lint")
            && instr.contains("small functions")
    );
}

// ---------------------------------------------------------------- apply

fn sample() -> Profile {
    let mut p = Profile::new("team");
    p.mcp_servers.insert(
        "github".into(),
        McpServer {
            command: Some("npx".into()),
            args: vec!["-y".into(), "@modelcontextprotocol/server-github".into()],
            env: [("GITHUB_TOKEN".to_string(), "${GITHUB_TOKEN}".to_string())]
                .into_iter()
                .collect(),
            ..Default::default()
        },
    );
    p.mcp_servers.insert(
        "linear".into(),
        McpServer {
            url: Some("https://mcp.linear.app/mcp".into()),
            headers: [(
                "Authorization".to_string(),
                "Bearer ${LINEAR_API_KEY}".to_string(),
            )]
            .into_iter()
            .collect(),
            ..Default::default()
        },
    );
    p.instructions = Some("Use British English.".into());
    p.rules.push(Rule {
        name: "ts".into(),
        description: Some("TypeScript".into()),
        globs: vec!["src/**/*.ts".into()],
        always_apply: None,
        body: "Strict mode.".into(),
    });
    p.commands.push(kitbag::model::Command {
        name: "review".into(),
        description: Some("Review".into()),
        body: "Review $ARGUMENTS".into(),
        ..Default::default()
    });
    p
}

#[test]
fn claude_code_apply_user() {
    let sb = Sandbox::empty();
    let p = plan(&sample(), &["claude-code"], &sb.env(Scope::User));
    let cj: serde_json::Value =
        serde_json::from_str(&planned(&p, &sb.home().join(".claude.json")).unwrap()).unwrap();
    assert_eq!(
        cj["mcpServers"]["github"],
        json!({"type": "stdio", "command": "npx", "args": ["-y", "@modelcontextprotocol/server-github"], "env": {"GITHUB_TOKEN": "${GITHUB_TOKEN}"}})
    );
    assert_eq!(cj["mcpServers"]["linear"]["type"], "http");
    let md = planned(&p, &sb.home().join(".claude/CLAUDE.md")).unwrap();
    assert!(md.contains("<!-- kitbag:begin team -->") && md.contains("Use British English."));
    let rule = planned(&p, &sb.home().join(".claude/rules/ts.md")).unwrap();
    assert_eq!(
        rule,
        "---\ndescription: TypeScript\npaths:\n  - \"src/**/*.ts\"\n---\n\nStrict mode.\n"
    );
    assert!(planned(&p, &sb.home().join(".claude/commands/review.md")).is_some());
}

#[test]
fn cursor_apply_project() {
    let sb = Sandbox::empty();
    let p = plan(&sample(), &["cursor"], &sb.env(Scope::Project));
    let mcp: serde_json::Value =
        serde_json::from_str(&planned(&p, &sb.project().join(".cursor/mcp.json")).unwrap())
            .unwrap();
    assert_eq!(
        mcp["mcpServers"]["github"]["env"]["GITHUB_TOKEN"],
        "${env:GITHUB_TOKEN}"
    );
    assert_eq!(
        mcp["mcpServers"]["linear"],
        json!({"url": "https://mcp.linear.app/mcp", "headers": {"Authorization": "Bearer ${env:LINEAR_API_KEY}"}})
    );
    let mdc = planned(&p, &sb.project().join(".cursor/rules/ts.mdc")).unwrap();
    assert_eq!(
        mdc,
        "---\ndescription: TypeScript\nglobs: src/**/*.ts\nalwaysApply: false\n---\n\nStrict mode.\n"
    );
    assert!(
        planned(&p, &sb.project().join("AGENTS.md"))
            .unwrap()
            .contains("Use British English.")
    );
    assert_eq!(
        planned(&p, &sb.project().join(".cursor/commands/review.md")).unwrap(),
        "Review $ARGUMENTS\n"
    );
}

#[test]
fn cursor_user_scope_explains_ui_only_rules() {
    let sb = Sandbox::empty();
    let p = plan(&sample(), &["cursor"], &sb.env(Scope::User));
    assert!(
        p.changes
            .iter()
            .all(|c| !c.path.to_string_lossy().contains("rules"))
    );
    assert!(
        p.notes
            .iter()
            .any(|n| n.message.contains("Settings > Rules"))
    );
}

#[test]
fn codex_apply_user() {
    let sb = Sandbox::empty();
    let p = plan(&sample(), &["codex"], &sb.env(Scope::User));
    let cfg = planned(&p, &sb.home().join(".codex/config.toml")).unwrap();
    assert!(cfg.contains("[mcp_servers.github]"), "{cfg}");
    assert!(cfg.contains("env_vars = [\"GITHUB_TOKEN\"]"), "{cfg}");
    assert!(
        cfg.contains("bearer_token_env_var = \"LINEAR_API_KEY\""),
        "{cfg}"
    );
    let agents = planned(&p, &sb.home().join(".codex/AGENTS.md")).unwrap();
    assert!(agents.contains("Use British English."));
    assert!(agents.contains("<!-- kitbag:begin team:rules -->"));
    assert!(agents.contains("_Applies when working with files matching: `src/**/*.ts`_"));
    assert!(planned(&p, &sb.home().join(".codex/prompts/review.md")).is_some());
    // No secret could leak: nothing needed resolving.
    assert!(p.sensitive.is_empty() && p.missing_env.is_empty());
}

#[test]
fn codex_apply_project_skips_prompts_and_user_only_keys() {
    let sb = Sandbox::empty();
    let mut prof = sample();
    prof.settings.insert(
        "codex".into(),
        json!({"model": "o3", "profile": "x"})
            .as_object()
            .cloned()
            .unwrap(),
    );
    let p = plan(&prof, &["codex"], &sb.env(Scope::Project));
    assert!(
        planned(&p, &sb.project().join(".codex/config.toml"))
            .unwrap()
            .contains("model = \"o3\"")
    );
    assert!(
        p.notes
            .iter()
            .any(|n| n.message.contains("no project-scoped custom prompts"))
    );
    assert!(p.notes.iter().any(|n| n.message.contains("trusted")));
    // 'profile' is not on the allow-list at all.
    assert!(
        p.notes
            .iter()
            .any(|n| n.message.contains("allow-list") && n.message.contains("profile"))
    );
}

#[test]
fn gemini_apply() {
    let sb = Sandbox::empty();
    let p = plan(&sample(), &["gemini-cli"], &sb.env(Scope::User));
    let s: serde_json::Value =
        serde_json::from_str(&planned(&p, &sb.home().join(".gemini/settings.json")).unwrap())
            .unwrap();
    assert_eq!(
        s["mcpServers"]["linear"]["httpUrl"],
        "https://mcp.linear.app/mcp"
    );
    assert_eq!(
        s["mcpServers"]["github"]["env"]["GITHUB_TOKEN"],
        "${GITHUB_TOKEN}"
    );
    let cmd = planned(&p, &sb.home().join(".gemini/commands/review.toml")).unwrap();
    assert!(cmd.contains("{{args}}"));
    assert!(
        planned(&p, &sb.home().join(".gemini/GEMINI.md"))
            .unwrap()
            .contains("kitbag:rule")
    );
}

#[test]
fn claude_desktop_apply_resolves_and_skips_remote() {
    let sb = Sandbox::empty();
    let env = sb.env_with(Scope::User, &[("GITHUB_TOKEN", "ghp_realvalue123")]);
    let p = plan(&sample(), &["claude-desktop"], &env);
    let cfg: serde_json::Value = serde_json::from_str(
        &planned(
            &p,
            &sb.home().join(".config/Claude/claude_desktop_config.json"),
        )
        .unwrap(),
    )
    .unwrap();
    assert_eq!(
        cfg["mcpServers"]["github"]["env"]["GITHUB_TOKEN"],
        "ghp_realvalue123"
    );
    assert!(cfg["mcpServers"].get("linear").is_none());
    assert!(p.sensitive.contains("ghp_realvalue123"));
    assert!(p.notes.iter().any(|n| n.message.contains("Connectors")));
}

#[test]
fn targets_and_disabled_on_filter_servers() {
    let sb = Sandbox::empty();
    let mut prof = sample();
    prof.mcp_servers.get_mut("github").unwrap().targets = Some(vec!["codex".into()]);
    prof.mcp_servers.get_mut("linear").unwrap().disabled_on = vec!["claude-code".into()];
    let p = plan(&prof, &["claude-code", "codex"], &sb.env(Scope::User));
    assert!(
        planned(&p, &sb.home().join(".claude.json")).is_none(),
        "claude-code gets neither server"
    );
    let cfg = planned(&p, &sb.home().join(".codex/config.toml")).unwrap();
    assert!(cfg.contains("[mcp_servers.github]") && cfg.contains("[mcp_servers.linear]"));
}

#[test]
fn env_overrides_relocate_configs() {
    let sb = Sandbox::empty();
    let mut env = sb.env(Scope::User);
    let codex_home = sb.path("elsewhere/codex");
    env.locations.insert(
        "CODEX_HOME".into(),
        codex_home.to_string_lossy().into_owned(),
    );
    let p = plan(&sample(), &["codex"], &env);
    assert!(planned(&p, &codex_home.join("config.toml")).is_some());
}

// ---------------------------------------------------------------- antigravity

#[test]
fn antigravity_capture() {
    let sb = Sandbox::fixtures();
    let r = capture(&["antigravity"], &sb.env(Scope::User));
    let p = &r.profile;
    assert_eq!(
        p.mcp_servers["firebase"].args,
        vec!["-y", "firebase-tools@latest", "mcp"]
    );
    let stitch = &p.mcp_servers["stitch"];
    assert_eq!(
        stitch.url.as_deref(),
        Some("https://stitch.example.com/mcp")
    );
    assert_eq!(stitch.transport(), Transport::Http);
    // A literal key in headers is redacted (Antigravity has no env syntax to keep).
    assert_eq!(stitch.headers["X-Goog-Api-Key"], "${STITCH_X_GOOG_API_KEY}");
    assert!(p.mcp_servers["time"].disabled);
    // Every file Antigravity reads: ~/.gemini/GEMINI.md and ~/.gemini/config/AGENTS.md.
    assert_eq!(
        p.instructions.as_deref(),
        Some("Prefer small functions.\n\nExplain the plan before editing files.")
    );

    let p = capture(&["antigravity"], &sb.env(Scope::Project)).profile;
    let devtools = &p.mcp_servers["chrome-devtools"];
    assert_eq!(devtools.cwd.as_deref(), Some("tools"));
    assert!(p.instructions.as_deref().unwrap().contains("pnpm test"));
}

#[test]
fn antigravity_capture_leaves_out_rules_folded_by_other_tools() {
    let sb = Sandbox::empty();
    apply(&sb, &sample(), &["gemini-cli"], &sb.env(Scope::User));
    let r = capture(&["antigravity"], &sb.env(Scope::User));
    assert_eq!(
        r.profile.instructions.as_deref(),
        Some("Use British English.")
    );
    assert!(r.profile.rules.is_empty());
    assert!(
        r.notes
            .iter()
            .any(|n| n.message.contains("were not captured (ts)"))
    );
}

#[test]
fn antigravity_apply_user() {
    let sb = Sandbox::empty();
    let env = sb.env_with(Scope::User, &[("GITHUB_TOKEN", "ghp_FAKEvalue123")]);
    let p = plan(&sample(), &["antigravity"], &env);
    let cfg: serde_json::Value = serde_json::from_str(
        &planned(&p, &sb.home().join(".gemini/config/mcp_config.json")).unwrap(),
    )
    .unwrap();
    // No interpolation in mcp_config.json: references are resolved, like Claude Desktop.
    assert_eq!(
        cfg["mcpServers"]["github"],
        json!({"command": "npx", "args": ["-y", "@modelcontextprotocol/server-github"], "env": {"GITHUB_TOKEN": "ghp_FAKEvalue123"}})
    );
    assert!(p.sensitive.contains("ghp_FAKEvalue123"));
    // Remote servers use serverUrl; the unset LINEAR_API_KEY stays a placeholder.
    assert_eq!(
        cfg["mcpServers"]["linear"],
        json!({"serverUrl": "https://mcp.linear.app/mcp", "headers": {"Authorization": "Bearer ${LINEAR_API_KEY}"}})
    );
    assert!(p.missing_env.contains("LINEAR_API_KEY"));
    let md = planned(&p, &sb.home().join(".gemini/GEMINI.md")).unwrap();
    assert!(md.contains("<!-- kitbag:begin team -->") && md.contains("Use British English."));
    assert!(!md.contains("kitbag:rule"), "rules are not supported yet");
    assert!(p.notes.iter().any(|n| n.target == "antigravity"
        && n.message.contains("not supported")
        && n.message.contains("1 rules")
        && n.message.contains("1 commands")));
    assert_eq!(p.changes.len(), 2, "{:?}", p.changes);
}

#[test]
fn antigravity_apply_project_keeps_placeholders_with_secrets_never() {
    let sb = Sandbox::empty();
    let env = sb.env_with(Scope::Project, &[("GITHUB_TOKEN", "ghp_FAKEvalue123")]);
    let prof = sample();
    let mut o = opts(&prof);
    o.secrets = kitbag::adapters::SecretsMode::Never;
    let p = kitbag::engine::plan(&prof, &targets(&["antigravity"]), &env, &o).unwrap();
    let cfg = planned(&p, &sb.project().join(".agents/mcp_config.json")).unwrap();
    assert!(cfg.contains("${GITHUB_TOKEN}") && !cfg.contains("ghp_FAKE"));
    assert!(p.sensitive.is_empty());
    assert!(
        planned(&p, &sb.project().join("AGENTS.md"))
            .unwrap()
            .contains("Use British English.")
    );
}

#[test]
fn antigravity_apply_merges_into_existing_servers() {
    let sb = Sandbox::empty();
    let cfg = sb.home().join(".gemini/config/mcp_config.json");
    std::fs::create_dir_all(cfg.parent().unwrap()).unwrap();
    std::fs::write(
        &cfg,
        r#"{
  "mcpServers": {
    "github": {
      "command": "npx",
      "args": ["-y", "@modelcontextprotocol/server-github"],
      "disabledTools": ["delete_repo"]
    },
    "linear": { "url": "https://mcp.linear.app/mcp", "timeoutSeconds": 30 },
    "mine": { "command": "my-mcp" }
  }
}
"#,
    )
    .unwrap();
    let mut prof = sample();
    prof.mcp_servers.get_mut("github").unwrap().disabled = true;
    prof.mcp_servers.insert(
        "events".into(),
        McpServer {
            url: Some("https://events.example.com/sse".into()),
            transport: Some(Transport::Sse),
            ..Default::default()
        },
    );
    let p = plan(&prof, &["antigravity"], &sb.env(Scope::User));
    let doc: serde_json::Value = serde_json::from_str(&planned(&p, &cfg).unwrap()).unwrap();
    let s = &doc["mcpServers"];
    assert_eq!(s["github"]["disabled"], true);
    assert_eq!(s["github"]["disabledTools"], json!(["delete_repo"]));
    // Same URL: the old `url` key gives way to serverUrl, timeoutSeconds stays.
    assert_eq!(
        s["linear"],
        json!({"timeoutSeconds": 30, "serverUrl": "https://mcp.linear.app/mcp", "headers": {"Authorization": "Bearer ${LINEAR_API_KEY}"}})
    );
    assert_eq!(s["mine"], json!({"command": "my-mcp"}));
    // SSE isn't something Antigravity connects to.
    assert!(s.get("events").is_none());
    assert!(
        p.notes
            .iter()
            .any(|n| n.message.contains("events skipped") && n.message.contains("SSE"))
    );
}

#[cfg(unix)]
#[test]
fn antigravity_config_holding_a_resolved_secret_is_private() {
    use std::os::unix::fs::PermissionsExt;
    let sb = Sandbox::empty();
    let env = sb.env_with(Scope::User, &[("GITHUB_TOKEN", "ghp_FAKEvalue123")]);
    apply(&sb, &sample(), &["antigravity"], &env);
    let cfg = sb.home().join(".gemini/config/mcp_config.json");
    let mode = std::fs::metadata(&cfg).unwrap().permissions().mode() & 0o777;
    assert_eq!(mode, 0o600);
}

/// Gemini CLI and Antigravity both read `~/.gemini/GEMINI.md`: applying to
/// both must leave one managed block, in either order, and undo cleanly.
#[test]
fn gemini_and_antigravity_share_one_block_in_gemini_md() {
    let count = |text: &str| text.matches("<!-- kitbag:begin team -->").count();
    let prof = sample();

    // Together, in one apply.
    let sb = Sandbox::empty();
    let env = sb.env(Scope::User);
    let gemini_md = sb.home().join(".gemini/GEMINI.md");
    std::fs::create_dir_all(gemini_md.parent().unwrap()).unwrap();
    std::fs::write(&gemini_md, "My own notes.\n").unwrap();
    let p = apply(&sb, &prof, &["gemini-cli", "antigravity"], &env);
    assert_eq!(
        p.changes.iter().filter(|c| c.path == gemini_md).count(),
        1,
        "GEMINI.md is planned once"
    );
    let text = std::fs::read_to_string(&gemini_md).unwrap();
    assert_eq!(count(&text), 1, "{text}");
    assert_eq!(text.matches("Use British English.").count(), 1, "{text}");
    assert!(text.starts_with("My own notes.") && text.contains("kitbag:begin team:rules"));
    for ids in [
        &["antigravity"][..],
        &["gemini-cli"],
        &["antigravity", "gemini-cli"],
    ] {
        let again = plan(&prof, ids, &env);
        assert!(again.changes.is_empty(), "{ids:?}: {:?}", again.changes);
    }
    Store::new(sb.state()).undo(None, false).unwrap();
    assert_eq!(
        std::fs::read_to_string(&gemini_md).unwrap(),
        "My own notes.\n"
    );
    assert!(!sb.home().join(".gemini/config").exists());

    // One after the other, each order.
    for order in [["antigravity", "gemini-cli"], ["gemini-cli", "antigravity"]] {
        let sb = Sandbox::empty();
        let env = sb.env(Scope::User);
        apply(&sb, &prof, &[order[0]], &env);
        apply(&sb, &prof, &[order[1]], &env);
        let text = sb.read("home/.gemini/GEMINI.md");
        assert_eq!(count(&text), 1, "{order:?}: {text}");
        assert_eq!(text.matches("Use British English.").count(), 1, "{order:?}");
        let again = plan(&prof, &order, &env);
        assert!(again.changes.is_empty(), "{order:?}: {:?}", again.changes);
    }
}

/// In a project, AGENTS.md is shared with Codex and Cursor.
#[test]
fn antigravity_codex_and_cursor_share_agents_md() {
    let sb = Sandbox::empty();
    let env = sb.env(Scope::Project);
    let prof = sample();
    apply(&sb, &prof, &["codex", "cursor", "antigravity"], &env);
    let text = sb.read("project/AGENTS.md");
    assert_eq!(
        text.matches("<!-- kitbag:begin team -->").count(),
        1,
        "{text}"
    );
    assert!(plan(&prof, &["antigravity"], &env).changes.is_empty());
}
