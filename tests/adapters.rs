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
        ..Default::default()
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
    // Rules get files of their own, not a folded block.
    assert!(!md.contains("kitbag:rule"), "{md}");
    assert_eq!(
        planned(&p, &sb.home().join(".gemini/config/rules/ts.md")).unwrap(),
        "---\ntrigger: glob\ndescription: TypeScript\nglobs: \"src/**/*.ts\"\n---\n\nStrict mode.\n"
    );
    assert!(p.notes.iter().any(|n| n.target == "antigravity"
        && n.message.contains("not supported")
        && n.message.contains("1 commands")
        && !n.message.contains("rules")));
    assert_eq!(p.changes.len(), 3, "{:?}", p.changes);
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
    let reads_gemini_rules = |p: &kitbag::engine::Plan| {
        p.notes
            .iter()
            .any(|n| n.message.contains("reads Gemini CLI's rules block"))
    };
    assert!(reads_gemini_rules(&p), "{:?}", p.notes);
    assert!(!reads_gemini_rules(&plan(&prof, &["antigravity"], &env)));
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
        let earlier = sb.read("home/.gemini/GEMINI.md");
        apply(&sb, &prof, &[order[1]], &env);
        let text = sb.read("home/.gemini/GEMINI.md");
        assert_eq!(count(&text), 1, "{order:?}: {text}");
        assert_eq!(text.matches("Use British English.").count(), 1, "{order:?}");
        let again = plan(&prof, &order, &env);
        assert!(again.changes.is_empty(), "{order:?}: {:?}", again.changes);
        // Undoing the later apply leaves the earlier target's block as it was.
        Store::new(sb.state()).undo(None, false).unwrap();
        assert_eq!(sb.read("home/.gemini/GEMINI.md"), earlier, "{order:?}");
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

// --- antigravity rules and skills

fn write(path: &std::path::Path, text: &str) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, text).unwrap();
}

fn rule(name: &str, description: Option<&str>, globs: &[&str], always: Option<bool>) -> Rule {
    Rule {
        name: name.into(),
        description: description.map(String::from),
        globs: globs.iter().map(|g| g.to_string()).collect(),
        always_apply: always,
        body: format!("Body of {name}."),
        targeting: Default::default(),
    }
}

#[test]
fn antigravity_rule_triggers() {
    let sb = Sandbox::empty();
    let mut prof = Profile::new("team");
    prof.rules = vec![
        rule("always", None, &[], None),
        rule("scoped", Some("Go"), &["*.go", "cmd/**"], None),
        rule("decide", Some("DB migrations"), &[], Some(false)),
        rule("quiet", None, &[], Some(false)),
    ];
    let p = plan(&prof, &["antigravity"], &sb.env(Scope::Project));
    let dir = sb.project().join(".agents/rules");
    assert_eq!(
        planned(&p, &dir.join("always.md")).unwrap(),
        "---\ntrigger: always_on\n---\n\nBody of always.\n"
    );
    assert_eq!(
        planned(&p, &dir.join("scoped.md")).unwrap(),
        "---\ntrigger: glob\ndescription: Go\nglobs: \"*.go, cmd/**\"\n---\n\nBody of scoped.\n"
    );
    assert_eq!(
        planned(&p, &dir.join("decide.md")).unwrap(),
        "---\ntrigger: model_decision\ndescription: DB migrations\n---\n\nBody of decide.\n"
    );
    assert_eq!(
        planned(&p, &dir.join("quiet.md")).unwrap(),
        "---\ntrigger: manual\n---\n\nBody of quiet.\n"
    );
    assert!(
        p.notes
            .iter()
            .any(|n| n.message.contains("rule 'quiet'") && n.message.contains("trigger: manual"))
    );
    // Nothing else is written for rules: no folded block.
    assert!(!p.changes.iter().any(|c| c.path.ends_with("AGENTS.md")));
}

#[test]
fn antigravity_capture_reads_rule_files_and_skills() {
    for scope in [Scope::User, Scope::Project] {
        let sb = Sandbox::empty();
        let base = match scope {
            Scope::User => sb.home().join(".gemini/config"),
            Scope::Project => sb.project().join(".agents"),
        };
        write(
            &base.join("rules/go.md"),
            "---\ntrigger: glob\nglobs: \"*.go, cmd/**\"\ndescription: Go\n---\n\nUse gofmt.\n",
        );
        write(
            &base.join("rules/style.md"),
            "---\ntrigger: always_on\n---\n\nBe terse.\n",
        );
        write(
            &base.join("rules/db.md"),
            "---\ntrigger: model_decision\ndescription: Migrations\n---\n\nReversible only.\n",
        );
        write(
            &base.join("rules/notes.md"),
            "Plain text, no frontmatter.\n",
        );
        write(
            &base.join("rules/odd.md"),
            "---\ntrigger: sometimes\n---\n\nx\n",
        );
        write(
            &base.join("rules/sub/deep.md"),
            "---\ntrigger: always_on\n---\n\nx\n",
        );
        write(&base.join("rules/readme.txt"), "not a rule\n");
        write(
            &base.join("skills/pdf/SKILL.md"),
            "---\nname: pdf\ndescription: PDFs\n---\n\nRun the script.\n",
        );
        write(&base.join("skills/pdf/scripts/run.sh"), "echo hi\n");
        let r = capture(&["antigravity"], &sb.env(scope));
        let names: Vec<&str> = r.profile.rules.iter().map(|r| r.name.as_str()).collect();
        assert_eq!(names, vec!["db", "go", "style"], "{scope}: {:?}", r.notes);
        let go = r.profile.rules.iter().find(|r| r.name == "go").unwrap();
        assert_eq!(go.globs, vec!["*.go", "cmd/**"]);
        assert_eq!(go.description.as_deref(), Some("Go"));
        assert_eq!(go.body, "Use gofmt.");
        let db = r.profile.rules.iter().find(|r| r.name == "db").unwrap();
        assert_eq!(db.always_apply, Some(false));
        let style = r.profile.rules.iter().find(|r| r.name == "style").unwrap();
        assert!(style.globs.is_empty() && style.always_apply.is_none());
        // Files without a valid trigger are dropped by Antigravity, and say so here.
        for f in ["notes.md", "odd.md"] {
            assert!(
                r.notes
                    .iter()
                    .any(|n| n.message.contains(f) && n.message.contains("discards")),
                "{f}: {:?}",
                r.notes
            );
        }
        assert_eq!(r.profile.skills.len(), 1);
        let pdf = &r.profile.skills[0];
        assert_eq!(pdf.name, "pdf");
        assert!(pdf.files.iter().any(|f| f.path == "scripts/run.sh"));
    }
}

#[test]
fn antigravity_capture_keeps_the_file_of_a_rule_also_folded_into_gemini_md() {
    let sb = Sandbox::empty();
    let env = sb.env(Scope::User);
    let mut prof = sample();
    prof.rules.push(rule("only-folded", None, &[], None));
    apply(&sb, &prof, &["gemini-cli"], &env);
    // The folded `ts` rule also has a file of its own, with a different trigger.
    write(
        &sb.home().join(".gemini/config/rules/ts.md"),
        "---\ntrigger: always_on\n---\n\nStrict mode, always.\n",
    );
    let r = capture(&["antigravity"], &env);
    assert_eq!(r.profile.rules.len(), 1, "{:?}", r.profile.rules);
    let ts = &r.profile.rules[0];
    assert_eq!(ts.name, "ts");
    assert!(ts.globs.is_empty() && ts.body == "Strict mode, always.");
    assert_eq!(
        r.profile.instructions.as_deref(),
        Some("Use British English.")
    );
    assert!(
        r.notes
            .iter()
            .any(|n| n.message.contains("were not captured (only-folded)")),
        "{:?}",
        r.notes
    );
}

#[test]
fn antigravity_apply_rules_and_skills_is_additive_idempotent_and_undoable() {
    for scope in [Scope::User, Scope::Project] {
        let sb = Sandbox::empty();
        let env = sb.env(scope);
        let base = match scope {
            Scope::User => sb.home().join(".gemini/config"),
            Scope::Project => sb.project().join(".agents"),
        };
        // The user's own rule and skill, and a file in a skill kitbag also writes.
        write(
            &base.join("rules/mine.md"),
            "---\ntrigger: always_on\n---\n\nMine.\n",
        );
        write(
            &base.join("skills/mine/SKILL.md"),
            "---\nname: mine\ndescription: mine\n---\nMine.\n",
        );
        let mut prof = sample();
        prof.skills.push(skill(
            "db-migrate",
            "---\nname: db-migrate\ndescription: Migrations\n---\nWrite one.\n",
        ));
        prof.skills.push(skill("bare", "No frontmatter.\n"));
        let p = apply(&sb, &prof, &["antigravity"], &env);
        assert!(base.join("rules/ts.md").is_file());
        assert!(base.join("skills/db-migrate/SKILL.md").is_file());
        let bare = std::fs::read_to_string(base.join("skills/bare/SKILL.md")).unwrap();
        assert!(
            bare.starts_with("---\nname: bare\ndescription: bare skill\n---"),
            "{bare}"
        );
        assert!(
            !p.notes
                .iter()
                .any(|n| n.message.contains("not supported") && n.message.contains("skills")),
            "{:?}",
            p.notes
        );
        // Second apply: up to date.
        let again = plan(&prof, &["antigravity"], &env);
        assert!(again.changes.is_empty(), "{scope}: {:?}", again.changes);
        // A profile with no rules or skills leaves the existing ones alone.
        let empty = Profile::new("team");
        let none = plan(&empty, &["antigravity"], &env);
        assert!(
            none.changes
                .iter()
                .all(|c| !c.path.starts_with(base.join("rules"))
                    && !c.path.starts_with(base.join("skills"))),
            "{:?}",
            none.changes
        );
        // Undo removes what kitbag wrote and nothing else.
        Store::new(sb.state()).undo(None, false).unwrap();
        assert!(!base.join("rules/ts.md").exists());
        assert!(!base.join("skills/db-migrate").exists());
        assert!(!base.join("skills/bare").exists());
        assert_eq!(
            sb.read(&format!(
                "{}/{}",
                if scope == Scope::User {
                    "home/.gemini/config"
                } else {
                    "project/.agents"
                },
                "rules/mine.md"
            )),
            "---\ntrigger: always_on\n---\n\nMine.\n"
        );
        assert!(base.join("skills/mine/SKILL.md").is_file());
    }
}

#[test]
fn antigravity_rules_and_skills_survive_a_round_trip_through_a_profile() {
    let sb = Sandbox::empty();
    let env = sb.env(Scope::User);
    let mut prof = Profile::new("team");
    prof.rules = vec![
        rule("always", None, &[], None),
        rule("scoped", Some("Go"), &["*.go", "cmd/**"], None),
        rule("decide", Some("DB migrations"), &[], Some(false)),
    ];
    prof.skills.push(skill(
        "db-migrate",
        "---\nname: db-migrate\ndescription: Migrations\n---\nWrite one.\n",
    ));
    apply(&sb, &prof, &["antigravity"], &env);
    let back = capture(&["antigravity"], &env).profile;
    let mut want = prof.rules.clone();
    want.sort_by(|a, b| a.name.cmp(&b.name));
    assert_eq!(back.rules, want);
    assert_eq!(back.skills.len(), 1);
    assert_eq!(back.skills[0].description.as_deref(), Some("Migrations"));
}

/// At user scope Antigravity shares `~/.gemini/` with Gemini CLI: rules go to
/// `config/rules/` and to the folded block, skills to two directories, and
/// neither tool's files are touched by the other.
#[test]
fn antigravity_rules_and_skills_coexist_with_gemini_cli() {
    let mut prof = sample();
    prof.skills.push(skill(
        "db-migrate",
        "---\nname: db-migrate\ndescription: Migrations\n---\nWrite one.\n",
    ));
    for order in [["gemini-cli", "antigravity"], ["antigravity", "gemini-cli"]] {
        let sb = Sandbox::empty();
        let env = sb.env(Scope::User);
        let gemini_md = sb.home().join(".gemini/GEMINI.md");
        write(&gemini_md, "My own notes.\n");
        let p = apply(&sb, &prof, &order, &env);
        let text = std::fs::read_to_string(&gemini_md).unwrap();
        assert!(text.starts_with("My own notes."), "{text}");
        assert_eq!(text.matches("kitbag:begin team -->").count(), 1, "{text}");
        assert_eq!(text.matches("kitbag:begin team:rules").count(), 1, "{text}");
        assert!(sb.home().join(".gemini/config/rules/ts.md").is_file());
        assert!(
            sb.home()
                .join(".gemini/config/skills/db-migrate/SKILL.md")
                .is_file()
        );
        assert!(
            sb.home()
                .join(".gemini/skills/db-migrate/SKILL.md")
                .is_file()
        );
        // The folded block is not duplicated into a rules file, and the
        // rules file doesn't leak into GEMINI.md.
        assert!(!text.contains("trigger:"), "{text}");
        let twice = |p: &kitbag::engine::Plan| {
            p.notes.iter().any(|n| {
                n.target == "antigravity" && n.message.contains("reads Gemini CLI's rules block")
            })
        };
        assert!(twice(&p), "{order:?}: {:?}", p.notes);
        let again = plan(&prof, &order, &env);
        assert!(again.changes.is_empty(), "{order:?}: {:?}", again.changes);
        // Capturing both gives one rule of each name: the file's version wins.
        let r = capture(&["antigravity", "gemini-cli"], &env);
        assert_eq!(r.profile.rules.len(), 1, "{:?}", r.profile.rules);
        assert_eq!(r.profile.skills.len(), 1);
        Store::new(sb.state()).undo(None, false).unwrap();
        assert_eq!(sb.read("home/.gemini/GEMINI.md"), "My own notes.\n");
        assert!(!sb.home().join(".gemini/config").exists());
        assert!(!sb.home().join(".gemini/skills").exists());
    }
    // Antigravity alone says nothing about Gemini CLI.
    let sb = Sandbox::empty();
    let p = plan(&prof, &["antigravity"], &sb.env(Scope::User));
    assert!(!p.notes.iter().any(|n| n.message.contains("Gemini CLI")));
}

/// `.agents/skills/` is Codex's project skills directory too: the same file,
/// written once. `AGENTS.md` is shared with Codex and Cursor.
#[test]
fn antigravity_skills_share_agents_skills_dir_with_codex_in_a_project() {
    let sb = Sandbox::empty();
    let env = sb.env(Scope::Project);
    let mut prof = sample();
    prof.skills.push(skill("bare", "No frontmatter.\n"));
    let p = apply(&sb, &prof, &["codex", "cursor", "antigravity"], &env);
    let md = sb.project().join(".agents/skills/bare/SKILL.md");
    assert_eq!(p.changes.iter().filter(|c| c.path == md).count(), 1);
    assert!(md.is_file());
    for ids in [&["antigravity"][..], &["codex"], &["codex", "antigravity"]] {
        let again = plan(&prof, ids, &env);
        assert!(again.changes.is_empty(), "{ids:?}: {:?}", again.changes);
    }
    // Codex folds rules into AGENTS.md; Antigravity has files of its own.
    assert!(sb.project().join(".agents/rules/ts.md").is_file());
    let text = sb.read("project/AGENTS.md");
    assert_eq!(text.matches("<!-- kitbag:begin team -->").count(), 1);
    // Undo with Codex's skill in place: the shared file goes with the apply that wrote it.
    Store::new(sb.state()).undo(None, false).unwrap();
    assert!(!sb.project().join(".agents").exists());
}

#[test]
fn antigravity_warns_about_large_rules_and_comma_globs() {
    let sb = Sandbox::empty();
    let mut prof = Profile::new("team");
    let mut big = rule("big", None, &[], None);
    big.body = "x ".repeat(13_000);
    prof.rules = vec![big, rule("comma", None, &["{a,b}/*.rs"], None)];
    let p = plan(&prof, &["antigravity"], &sb.env(Scope::Project));
    let said = |needle: &str| p.notes.iter().any(|n| n.message.contains(needle));
    assert!(said("rule 'big' is"), "{:?}", p.notes);
    assert!(said("rule 'comma': Antigravity splits globs at commas"));
}

// ---------------------------------------------------------------- opencode

#[test]
fn opencode_capture() {
    let sb = Sandbox::fixtures();
    let r = capture(&["opencode"], &sb.env(Scope::User));
    let p = &r.profile;
    // `{env:VAR}` comes back as ${VAR}; opencode-only keys (oauth, codemode) stay behind.
    let c7 = &p.mcp_servers["context7"];
    assert_eq!(c7.url.as_deref(), Some("https://mcp.context7.com/mcp"));
    assert_eq!(c7.transport(), Transport::Http);
    assert_eq!(c7.headers["CONTEXT7_API_KEY"], "${CONTEXT7_API_KEY}");
    // The command array splits into command and args; the literal token is redacted.
    let sentry = &p.mcp_servers["sentry"];
    assert_eq!(sentry.command.as_deref(), Some("npx"));
    assert_eq!(sentry.args, vec!["-y", "@sentry/mcp-server"]);
    assert_eq!(sentry.env["SENTRY_ACCESS_TOKEN"], "${SENTRY_ACCESS_TOKEN}");
    assert!(p.mcp_servers["time"].disabled);
    assert!(!p.mcp_servers.contains_key("timeout"));
    assert_eq!(p.instructions.as_deref(), Some("Keep commits small."));

    // The project uses opencode 1's layout, in .opencode/opencode.json.
    let p = capture(&["opencode"], &sb.env(Scope::Project)).profile;
    let pw = &p.mcp_servers["playwright"];
    assert_eq!(pw.command.as_deref(), Some("npx"));
    assert_eq!(pw.args, vec!["@playwright/mcp@latest"]);
    assert_eq!(pw.cwd.as_deref(), Some("web"));
    assert_eq!(
        pw.env["PLAYWRIGHT_BROWSERS_PATH"],
        "${HOME}/.cache/ms-playwright"
    );
    assert!(p.instructions.as_deref().unwrap().contains("pnpm test"));
}

#[test]
fn opencode_capture_reads_both_layouts_and_warns_about_bad_entries() {
    let sb = Sandbox::empty();
    let dir = sb.home().join(".config/opencode");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("opencode.json"),
        r#"{
  "mcp": {
    "servers": { "a": { "type": "remote", "url": "https://a.example.com/mcp" } },
    "old": { "type": "local", "command": ["old-mcp"], "enabled": false },
    "odd": { "type": "websocket", "url": "wss://x" },
    "keyfile": { "type": "local", "command": ["k"], "environment": { "KEY": "{file:~/.secrets/k}" } }
  },
  "instructions": ["CONTRIBUTING.md"]
}
"#,
    )
    .unwrap();
    let r = capture(&["opencode"], &sb.env(Scope::User));
    let s = &r.profile.mcp_servers;
    assert!(s.contains_key("a"));
    assert!(s["old"].disabled);
    assert!(!s.contains_key("odd"));
    let msgs: Vec<&str> = r.notes.iter().map(|n| n.message.as_str()).collect();
    assert!(
        msgs.iter()
            .any(|m| m.contains("'odd'") && m.contains("type 'websocket'")),
        "{msgs:?}"
    );
    assert!(msgs.iter().any(|m| m.contains("{file:...}")), "{msgs:?}");
    assert!(
        msgs.iter().any(|m| m.contains("under 'instructions'")),
        "{msgs:?}"
    );
}

#[test]
fn opencode_apply_user() {
    let sb = Sandbox::empty();
    let env = sb.env_with(Scope::User, &[("GITHUB_TOKEN", "ghp_FAKEvalue123")]);
    let p = plan(&sample(), &["opencode"], &env);
    let cfg_path = sb.home().join(".config/opencode/opencode.json");
    let cfg: serde_json::Value = serde_json::from_str(&planned(&p, &cfg_path).unwrap()).unwrap();
    // opencode expands {env:VAR}: the secret is never written.
    assert_eq!(
        cfg["mcp"]["servers"]["github"],
        json!({"type": "local", "command": ["npx", "-y", "@modelcontextprotocol/server-github"], "environment": {"GITHUB_TOKEN": "{env:GITHUB_TOKEN}"}})
    );
    assert_eq!(
        cfg["mcp"]["servers"]["linear"],
        json!({"type": "remote", "url": "https://mcp.linear.app/mcp", "headers": {"Authorization": "Bearer {env:LINEAR_API_KEY}"}})
    );
    assert!(p.sensitive.is_empty());
    let md = planned(&p, &sb.home().join(".config/opencode/AGENTS.md")).unwrap();
    assert!(md.contains("<!-- kitbag:begin team -->") && md.contains("Use British English."));
    // opencode has no rule files: rules are folded into AGENTS.md.
    assert!(
        md.contains("<!-- kitbag:begin team:rules -->") && md.contains("Strict mode."),
        "{md}"
    );
    let cmd = planned(&p, &sb.home().join(".config/opencode/commands/review.md")).unwrap();
    assert_eq!(cmd, "---\ndescription: Review\n---\n\nReview $ARGUMENTS\n");
    assert_eq!(p.changes.len(), 3, "{:?}", p.changes);
}

#[test]
fn opencode_follows_xdg_config_home() {
    let sb = Sandbox::empty();
    let mut env = sb.env(Scope::User);
    let xdg = sb.path("xdg");
    env.locations
        .insert("XDG_CONFIG_HOME".into(), xdg.display().to_string());
    let p = plan(&sample(), &["opencode"], &env);
    assert!(planned(&p, &xdg.join("opencode/opencode.json")).is_some());
    assert!(planned(&p, &xdg.join("opencode/AGENTS.md")).is_some());
}

#[test]
fn opencode_apply_merges_into_existing_jsonc() {
    let sb = Sandbox::empty();
    let dir = sb.home().join(".config/opencode");
    std::fs::create_dir_all(&dir).unwrap();
    let cfg = dir.join("opencode.jsonc");
    std::fs::write(
        &cfg,
        r#"{
  // my model
  "model": "anthropic/claude-sonnet-4-5",
  "mcp": {
    "servers": {
      "github": {
        "type": "local",
        "command": ["npx", "-y", "@modelcontextprotocol/server-github"],
        "timeout": { "execution": 30000 },
      },
      "linear": { "type": "remote", "url": "https://mcp.linear.app/mcp", "disabled": true, "oauth": false },
      "mine": { "type": "local", "command": ["my-mcp"] },
    },
  },
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
    let p = plan(&prof, &["opencode"], &sb.env(Scope::User));
    assert!(
        planned(&p, &dir.join("opencode.json")).is_none(),
        "the existing .jsonc file is used"
    );
    let text = planned(&p, &cfg).unwrap();
    assert!(text.contains("// my model"), "comments are kept: {text}");
    let doc = kitbag::edit::json::parse_value(Some(&text), &cfg).unwrap();
    let s = &doc["mcp"]["servers"];
    assert_eq!(s["github"]["disabled"], true);
    assert_eq!(s["github"]["timeout"], json!({"execution": 30000}));
    // Turned back on: `disabled` goes, oauth stays.
    assert_eq!(
        s["linear"],
        json!({"type": "remote", "url": "https://mcp.linear.app/mcp", "oauth": false, "headers": {"Authorization": "Bearer {env:LINEAR_API_KEY}"}})
    );
    assert_eq!(s["mine"], json!({"type": "local", "command": ["my-mcp"]}));
    assert_eq!(doc["model"], "anthropic/claude-sonnet-4-5");
    assert!(s.get("events").is_none());
    assert!(
        p.notes
            .iter()
            .any(|n| n.message.contains("events skipped") && n.message.contains("SSE"))
    );
}

/// A file in opencode 1's layout (servers directly under `mcp`) keeps it, so
/// opencode 1 can still read it.
#[test]
fn opencode_apply_keeps_the_legacy_layout() {
    let sb = Sandbox::empty();
    let cfg = sb.project().join("opencode.json");
    std::fs::write(
        &cfg,
        r#"{
  "mcp": {
    "mine": { "type": "local", "command": ["my-mcp"], "enabled": true }
  }
}
"#,
    )
    .unwrap();
    let mut prof = sample();
    prof.mcp_servers.get_mut("linear").unwrap().disabled = true;
    let env = sb.env(Scope::Project);
    apply(&sb, &prof, &["opencode"], &env);
    let doc = sb.json("project/opencode.json");
    assert!(doc["mcp"].get("servers").is_none(), "{doc}");
    assert_eq!(doc["mcp"]["github"]["type"], "local");
    assert_eq!(doc["mcp"]["linear"]["enabled"], false);
    assert!(doc["mcp"]["linear"].get("disabled").is_none());
    assert_eq!(doc["mcp"]["mine"]["enabled"], true);
    assert!(plan(&prof, &["opencode"], &env).changes.is_empty());
    // Back on: `enabled: false` goes.
    prof.mcp_servers.get_mut("linear").unwrap().disabled = false;
    apply(&sb, &prof, &["opencode"], &env);
    let doc = sb.json("project/opencode.json");
    assert!(doc["mcp"]["linear"].get("enabled").is_none(), "{doc}");
}

/// In opencode 1's layout a server named after one of opencode's own `mcp`
/// keys is skipped, so it can't turn the file into a `mcp.servers` one.
#[test]
fn opencode_legacy_layout_skips_reserved_server_names() {
    let sb = Sandbox::empty();
    let cfg = sb.project().join("opencode.json");
    std::fs::write(
        &cfg,
        r#"{
  "mcp": {
    "mine": { "type": "local", "command": ["my-mcp"] }
  }
}
"#,
    )
    .unwrap();
    let mut prof = sample();
    for name in ["servers", "timeout"] {
        prof.mcp_servers.insert(
            name.into(),
            McpServer {
                command: Some("reserved-mcp".into()),
                ..Default::default()
            },
        );
    }
    let env = sb.env(Scope::Project);
    let p = plan(&prof, &["opencode"], &env);
    assert!(
        p.notes.iter().any(|n| n.target == "opencode"
            && n.message.contains("servers, timeout skipped")
            && n.message.contains("opencode.json")),
        "{:?}",
        p.notes
    );
    apply(&sb, &prof, &["opencode"], &env);
    let doc = sb.json("project/opencode.json");
    assert!(doc["mcp"].get("servers").is_none(), "{doc}");
    assert!(doc["mcp"].get("timeout").is_none(), "{doc}");
    assert_eq!(doc["mcp"]["github"]["type"], "local");
    assert_eq!(doc["mcp"]["mine"]["command"], json!(["my-mcp"]));

    let r = capture(&["opencode"], &env);
    let s = &r.profile.mcp_servers;
    assert!(s.contains_key("github") && s.contains_key("mine"), "{s:?}");
    assert!(!s.contains_key("type") && !s.contains_key("command"));
    assert!(
        !r.notes
            .iter()
            .any(|n| n.message.contains("'type'") || n.message.contains("'command'")),
        "{:?}",
        r.notes
    );
}

/// In a project, AGENTS.md is shared with Codex, Cursor and Antigravity:
/// one managed block, and undo restores the file.
#[test]
fn opencode_shares_agents_md_with_codex_cursor_and_antigravity() {
    let sb = Sandbox::empty();
    let env = sb.env(Scope::Project);
    let prof = sample();
    let agents = sb.project().join("AGENTS.md");
    std::fs::write(&agents, "My own notes.\n").unwrap();
    let p = apply(
        &sb,
        &prof,
        &["codex", "cursor", "antigravity", "opencode"],
        &env,
    );
    assert_eq!(
        p.changes.iter().filter(|c| c.path == agents).count(),
        1,
        "AGENTS.md is planned once"
    );
    let text = sb.read("project/AGENTS.md");
    assert_eq!(
        text.matches("<!-- kitbag:begin team -->").count(),
        1,
        "{text}"
    );
    assert_eq!(text.matches("Use British English.").count(), 1, "{text}");
    assert!(plan(&prof, &["opencode"], &env).changes.is_empty());
    assert!(plan(&prof, &["codex", "opencode"], &env).changes.is_empty());
    // opencode's rules are AGENTS.md, so the rules Codex folded in are captured.
    let r = capture(&["opencode"], &env);
    assert_eq!(r.profile.rules, prof.rules);
    Store::new(sb.state()).undo(None, false).unwrap();
    assert_eq!(std::fs::read_to_string(&agents).unwrap(), "My own notes.\n");
    assert!(!sb.project().join("opencode.json").exists());
}

#[test]
fn opencode_notes_when_agents_md_hides_claude_md() {
    let sb = Sandbox::empty();
    std::fs::write(sb.project().join("CLAUDE.md"), "Claude notes.\n").unwrap();
    let p = plan(&sample(), &["opencode"], &sb.env(Scope::Project));
    assert!(
        p.notes
            .iter()
            .any(|n| n.target == "opencode" && n.message.contains("CLAUDE.md only while")),
        "{:?}",
        p.notes
    );
}

// ---------------------------------------------------------------- copilot

#[test]
fn copilot_capture_user() {
    let sb = Sandbox::fixtures();
    let r = capture(&["copilot"], &sb.env(Scope::User));
    let p = &r.profile;
    // `type: local` is Copilot CLI's older name for stdio.
    let pw = &p.mcp_servers["playwright"];
    assert_eq!(pw.command.as_deref(), Some("npx"));
    assert_eq!(pw.transport(), Transport::Stdio);
    let ctx = &p.mcp_servers["context7"];
    assert_eq!(ctx.url.as_deref(), Some("https://mcp.context7.com/mcp"));
    assert!(
        !ctx.headers["CONTEXT7_API_KEY"].contains("ctx7_FAKE"),
        "literal key is redacted: {:?}",
        ctx.headers
    );
    assert!(
        r.notes
            .iter()
            .any(|n| n.message.contains("'context7': tools not captured"))
    );
    // Copilot expands ${VAR:-default} itself, so it is kept.
    assert_eq!(
        p.mcp_servers["notes"].args,
        vec!["--root", "${NOTES_DIR:-/tmp/notes}"]
    );
    // settings.json disabledMcpServers.
    assert!(p.mcp_servers["old-time"].disabled);
    assert!(!pw.disabled);
    assert_eq!(
        p.instructions.as_deref(),
        Some("Answer in short paragraphs.")
    );
    assert_eq!(p.rules.len(), 1);
    assert_eq!(p.rules[0].name, "go");
    assert_eq!(p.rules[0].globs, vec!["**/*.go"]);
}

#[test]
fn copilot_capture_project() {
    let sb = Sandbox::fixtures();
    let r = capture(&["copilot"], &sb.env(Scope::Project));
    let p = &r.profile;
    // .mcp.json (shared with Claude Code) wins over the older .vscode/mcp.json.
    assert_eq!(
        p.mcp_servers["playwright"].args,
        vec!["@playwright/mcp@latest"]
    );
    // From .vscode/mcp.json: ${env:VAR} becomes ${VAR}.
    assert_eq!(
        p.mcp_servers["github"].headers["Authorization"],
        "Bearer ${GITHUB_TOKEN}"
    );
    // ${input:...} can't be carried anywhere.
    assert!(!p.mcp_servers.contains_key("perplexity"));
    assert!(
        r.notes
            .iter()
            .any(|n| n.message.contains("skipped MCP server 'perplexity'"))
    );
    assert!(p.instructions.as_deref().unwrap().contains("pnpm test"));
    let sql = &p.rules[0];
    assert_eq!(sql.name, "sql");
    assert_eq!(
        sql.description.as_deref(),
        Some("Writing database migrations")
    );
    assert_eq!(sql.globs, vec!["db/**/*.sql", "migrations/**"]);
}

#[test]
fn copilot_capture_reads_github_mcp_json_in_the_bare_format() {
    let sb = Sandbox::empty();
    let f = sb.project().join(".github/mcp.json");
    std::fs::create_dir_all(f.parent().unwrap()).unwrap();
    std::fs::write(
        &f,
        r#"{
  // bare format: each top-level key is a server
  "playwright": { "type": "local", "command": "npx", "args": ["@playwright/mcp@latest"] },
}
"#,
    )
    .unwrap();
    let p = capture(&["copilot"], &sb.env(Scope::Project)).profile;
    assert_eq!(p.mcp_servers["playwright"].command.as_deref(), Some("npx"));
}

#[test]
fn copilot_apply_user() {
    let sb = Sandbox::empty();
    let env = sb.env_with(Scope::User, &[("GITHUB_TOKEN", "ghp_FAKEvalue123")]);
    let p = plan(&sample(), &["copilot"], &env);
    let cfg: serde_json::Value =
        serde_json::from_str(&planned(&p, &sb.home().join(".copilot/mcp-config.json")).unwrap())
            .unwrap();
    // Copilot expands ${VAR} itself: nothing is resolved.
    assert_eq!(
        cfg["mcpServers"]["github"],
        json!({"type": "stdio", "command": "npx", "args": ["-y", "@modelcontextprotocol/server-github"], "env": {"GITHUB_TOKEN": "${GITHUB_TOKEN}"}, "tools": ["*"]})
    );
    assert_eq!(
        cfg["mcpServers"]["linear"],
        json!({"type": "http", "url": "https://mcp.linear.app/mcp", "headers": {"Authorization": "Bearer ${LINEAR_API_KEY}"}, "tools": ["*"]})
    );
    assert!(p.sensitive.is_empty() && p.private.is_empty());
    let md = planned(&p, &sb.home().join(".copilot/copilot-instructions.md")).unwrap();
    assert!(md.contains("<!-- kitbag:begin team -->") && md.contains("Use British English."));
    assert_eq!(
        planned(
            &p,
            &sb.home().join(".copilot/instructions/ts.instructions.md")
        )
        .unwrap(),
        "---\ndescription: TypeScript\napplyTo: \"src/**/*.ts\"\n---\n\nStrict mode.\n"
    );
    assert!(p.notes.iter().any(|n| n.target == "copilot"
        && n.message.contains("not supported")
        && n.message.contains("1 commands")));
    assert_eq!(p.changes.len(), 3, "{:?}", p.changes);
}

#[test]
fn copilot_home_relocates_the_user_files() {
    let sb = Sandbox::empty();
    let mut env = sb.env(Scope::User);
    let dir = sb.path("elsewhere/copilot");
    env.locations
        .insert("COPILOT_HOME".into(), dir.to_string_lossy().into_owned());
    let p = plan(&sample(), &["copilot"], &env);
    assert!(planned(&p, &dir.join("mcp-config.json")).is_some());
    assert!(planned(&p, &dir.join("copilot-instructions.md")).is_some());
    assert!(planned(&p, &dir.join("instructions/ts.instructions.md")).is_some());
    assert!(!sb.home().join(".copilot").exists());
}

#[test]
fn copilot_apply_user_keeps_your_tool_lists_and_keys() {
    let sb = Sandbox::empty();
    let cfg = sb.home().join(".copilot/mcp-config.json");
    std::fs::create_dir_all(cfg.parent().unwrap()).unwrap();
    std::fs::write(
        &cfg,
        r#"{
  "mcpServers": {
    // trimmed on purpose
    "github": {
      "type": "local",
      "command": "npx",
      "args": ["-y", "@modelcontextprotocol/server-github"],
      "tools": ["list_issues"],
      "timeout": 60000
    },
    "linear": { "type": "http", "url": "https://old.example.com/mcp", "tools": ["search"], "oauthClientId": "abc" },
    "mine": { "command": "my-mcp", "args": [], "tools": ["*"] }
  }
}
"#,
    )
    .unwrap();
    let mut prof = sample();
    prof.mcp_servers.get_mut("github").unwrap().cwd = Some("/srv/gh".into());
    let p = plan(&prof, &["copilot"], &sb.env(Scope::User));
    let text = planned(&p, &cfg).unwrap();
    assert!(
        text.contains("// trimmed on purpose"),
        "comments are kept: {text}"
    );
    let doc: serde_json::Value =
        serde_json::from_str(&text.replace("// trimmed on purpose", "")).unwrap();
    let s = &doc["mcpServers"];
    // Same server: your tool list and timeout stay; cwd is written at user scope.
    assert_eq!(s["github"]["tools"], json!(["list_issues"]));
    assert_eq!(s["github"]["timeout"], 60000);
    assert_eq!(s["github"]["type"], "stdio");
    assert_eq!(s["github"]["cwd"], "/srv/gh");
    // A different URL is a different server: its tool list and OAuth client go.
    assert_eq!(
        s["linear"],
        json!({"type": "http", "url": "https://mcp.linear.app/mcp", "headers": {"Authorization": "Bearer ${LINEAR_API_KEY}"}, "tools": ["*"]})
    );
    assert!(
        p.notes
            .iter()
            .any(|n| n.message.contains("'linear'") && n.message.contains("oauthClientId"))
    );
    assert_eq!(
        s["mine"],
        json!({"command": "my-mcp", "args": [], "tools": ["*"]})
    );
    let again = {
        apply(&sb, &prof, &["copilot"], &sb.env(Scope::User));
        plan(&prof, &["copilot"], &sb.env(Scope::User))
    };
    assert!(again.changes.is_empty(), "{:?}", again.changes);
}

#[test]
fn copilot_apply_project_writes_claude_codes_shape() {
    let sb = Sandbox::empty();
    let env = sb.env(Scope::Project);
    let mut prof = sample();
    prof.mcp_servers.get_mut("github").unwrap().cwd = Some("tools".into());
    let p = plan(&prof, &["copilot"], &env);
    let mcp: serde_json::Value =
        serde_json::from_str(&planned(&p, &sb.project().join(".mcp.json")).unwrap()).unwrap();
    // No tools and no cwd: Claude Code reads this file too.
    assert_eq!(
        mcp["mcpServers"]["github"],
        json!({"type": "stdio", "command": "npx", "args": ["-y", "@modelcontextprotocol/server-github"], "env": {"GITHUB_TOKEN": "${GITHUB_TOKEN}"}})
    );
    assert!(
        p.notes
            .iter()
            .any(|n| n.message.contains("'cwd' was dropped"))
    );
    assert!(
        planned(&p, &sb.project().join(".github/copilot-instructions.md"))
            .unwrap()
            .contains("Use British English.")
    );
    assert!(
        planned(
            &p,
            &sb.project().join(".github/instructions/ts.instructions.md")
        )
        .is_some()
    );
}

fn skill(name: &str, main: &str) -> kitbag::model::Skill {
    kitbag::model::Skill {
        name: name.into(),
        description: None,
        files: vec![kitbag::model::SkillFile {
            path: "SKILL.md".into(),
            content: main.as_bytes().to_vec(),
            executable: false,
        }],
        ..Default::default()
    }
}

#[test]
fn copilot_capture_reads_prompt_files_and_skills() {
    let sb = Sandbox::fixtures();
    let user = capture(&["copilot"], &sb.env(Scope::User)).profile;
    assert_eq!(user.skills.len(), 1);
    assert_eq!(user.skills[0].name, "pr-notes");
    assert_eq!(
        user.skills[0].description.as_deref(),
        Some("Write pull request notes from the staged diff")
    );
    // VS Code keeps user prompt files in its own storage: none at user scope.
    assert!(user.commands.is_empty());

    let r = capture(&["copilot"], &sb.env(Scope::Project));
    let p = &r.profile;
    let release = p.commands.iter().find(|c| c.name == "release").unwrap();
    assert_eq!(release.description.as_deref(), Some("Draft release notes"));
    assert_eq!(release.extra["agent"], json!("agent"));
    assert_eq!(release.extra["tools"], json!(["search", "fetch"]));
    assert!(
        release
            .body
            .contains("${input:version:the version to release}")
    );
    assert_eq!(p.skills.len(), 1);
    assert_eq!(p.skills[0].name, "db-migrate");
    assert!(
        !r.notes
            .iter()
            .any(|n| n.message.contains("prompt files") || n.message.contains("skills (")),
        "prompt files and skills are carried now: {:?}",
        r.notes
    );
}

#[test]
fn copilot_captures_custom_agents_and_legacy_chat_modes() {
    let sb = Sandbox::empty();
    let write = |rel: &str, text: &str| {
        let p = sb.project().join(rel);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, text).unwrap();
    };
    write(
        ".github/agents/reviewer.agent.md",
        "---\ndescription: Reviews\ntools: ['read', 'search']\nmodel: GPT-5\nhandoffs:\n  - label: Fix\n    agent: fixer\n---\nReview.\n",
    );
    // A plain `.md` is an agent too; the `.agent.md` of the same name wins.
    write(
        ".github/agents/fixer.md",
        "---\ndescription: Fixes\n---\nFix.\n",
    );
    write(".github/agents/reviewer.md", "Not this one.\n");
    // Chat modes are the old name; the agent of the same name wins.
    write(
        ".github/chatmodes/planner.chatmode.md",
        "---\ndescription: Plans\ntools: ['search']\n---\nPlan.\n",
    );
    write(".github/chatmodes/fixer.chatmode.md", "Old fixer.\n");
    let r = capture(&["copilot"], &sb.env(Scope::Project));
    let names: Vec<&str> = r.profile.agents.iter().map(|a| a.name.as_str()).collect();
    assert_eq!(names, ["fixer", "planner", "reviewer"]);
    let reviewer = r
        .profile
        .agents
        .iter()
        .find(|a| a.name == "reviewer")
        .unwrap();
    assert_eq!(reviewer.description.as_deref(), Some("Reviews"));
    assert_eq!(reviewer.body, "Review.");
    assert_eq!(reviewer.extra["tools"], json!(["read", "search"]));
    assert_eq!(reviewer.extra["model"], json!("GPT-5"));
    assert_eq!(reviewer.extra["handoffs"][0]["agent"], json!("fixer"));
    let fixer = r.profile.agents.iter().find(|a| a.name == "fixer").unwrap();
    assert_eq!(fixer.body, "Fix.");
    assert!(
        r.notes.iter().any(|n| n
            .message
            .contains("captured chat modes as agents (planner)")
            && n.message.contains("old name")),
        "{:?}",
        r.notes
    );
    assert!(!r.notes.iter().any(|n| n.message.contains("not carried")));
}

#[test]
fn copilot_agents_that_flatten_to_the_same_name_fail_the_capture() {
    let sb = Sandbox::empty();
    let dir = sb.project().join(".github/agents");
    std::fs::create_dir_all(dir.join("team")).unwrap();
    std::fs::write(dir.join("team/x.agent.md"), "One.\n").unwrap();
    std::fs::write(dir.join("team-x.agent.md"), "Two.\n").unwrap();
    let err = kitbag::engine::capture(&targets(&["copilot"]), &sb.env(Scope::Project), "test")
        .map(|_| ())
        .expect_err("a flattened name clash is an error");
    let msg = format!("{err:#}");
    assert!(msg.contains("both flatten to the name 'team-x'"), "{msg}");
    assert!(msg.contains("team-x.agent.md"), "{msg}");

    // The same clash across extensions is an error too, while `x.agent.md`
    // and `x.md` (one path, two extensions) are still one agent.
    std::fs::remove_file(dir.join("team-x.agent.md")).unwrap();
    std::fs::write(dir.join("team-x.md"), "Three.\n").unwrap();
    assert!(
        kitbag::engine::capture(&targets(&["copilot"]), &sb.env(Scope::Project), "test").is_err()
    );
    std::fs::remove_file(dir.join("team-x.md")).unwrap();
    std::fs::write(dir.join("team/x.md"), "Plain.\n").unwrap();
    let r = capture(&["copilot"], &sb.env(Scope::Project));
    assert_eq!(r.profile.agents.len(), 1);
    assert_eq!(r.profile.agents[0].body, "One.");
}

#[test]
fn copilot_skips_prompt_instructions_and_readme_files_among_agents() {
    let sb = Sandbox::empty();
    let dir = sb.project().join(".github/agents");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("writer.agent.md"), "Write.\n").unwrap();
    std::fs::write(dir.join("helper.md"), "Help.\n").unwrap();
    std::fs::write(dir.join("ship.prompt.md"), "Ship.\n").unwrap();
    std::fs::write(dir.join("style.instructions.md"), "Style.\n").unwrap();
    std::fs::write(dir.join("README.md"), "About these agents.\n").unwrap();
    let r = capture(&["copilot"], &sb.env(Scope::Project));
    let names: Vec<&str> = r.profile.agents.iter().map(|a| a.name.as_str()).collect();
    assert_eq!(names, ["helper", "writer"]);
    for (file, why) in [
        ("ship.prompt.md", "a prompt file"),
        ("style.instructions.md", "an instructions file"),
        ("README.md", "a README"),
    ] {
        assert!(
            r.notes
                .iter()
                .any(|n| n.message.contains(file) && n.message.contains(why)),
            "{file}: {:?}",
            r.notes
        );
    }
}

#[test]
fn copilot_captures_user_agents_from_copilot_home() {
    let sb = Sandbox::empty();
    let dir = sb.home().join(".copilot/agents");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("writer.agent.md"),
        "---\ndescription: Writes docs\n---\nWrite.\n",
    )
    .unwrap();
    let r = capture(&["copilot"], &sb.env(Scope::User));
    assert_eq!(r.profile.agents.len(), 1);
    assert_eq!(r.profile.agents[0].name, "writer");
}

#[test]
fn copilot_apply_project_writes_prompt_files_and_skills() {
    let sb = Sandbox::empty();
    let mut prof = sample();
    let mut extra = serde_json::Map::new();
    extra.insert("agent".into(), json!("agent"));
    extra.insert("allowed-tools".into(), json!("Bash(git:*)"));
    prof.commands.push(kitbag::model::Command {
        name: "ship".into(),
        description: Some("Ship it".into()),
        argument_hint: Some("[version]".into()),
        extra,
        body: "Ship the release.".into(),
        ..Default::default()
    });
    prof.skills
        .push(skill("db-migrate", "Write a reversible migration.\n"));
    let p = plan(&prof, &["copilot"], &sb.env(Scope::Project));
    let prompts = sb.project().join(".github/prompts");
    assert_eq!(
        planned(&p, &prompts.join("ship.prompt.md")).unwrap(),
        "---\ndescription: Ship it\nargument-hint: \"[version]\"\nagent: agent\n---\n\nShip the release.\n"
    );
    assert!(
        p.notes.iter().any(|n| n.target == "copilot"
            && n.message.contains("command 'ship': allowed-tools left out"))
    );
    assert!(planned(&p, &prompts.join("review.prompt.md")).is_some());
    assert!(
        p.notes.iter().any(
            |n| n.target == "copilot" && n.message.contains("command 'review' uses $ARGUMENTS")
        )
    );
    // name and description are required in SKILL.md, so they are filled in.
    let md = planned(&p, &sb.project().join(".github/skills/db-migrate/SKILL.md")).unwrap();
    assert!(
        md.starts_with("---\nname: db-migrate\ndescription: db-migrate skill\n---"),
        "{md}"
    );
    assert!(
        !p.notes
            .iter()
            .any(|n| n.message.contains("not supported") && n.target == "copilot"),
        "{:?}",
        p.notes
    );
}

#[test]
fn copilot_apply_user_writes_skills_under_copilot_home() {
    let sb = Sandbox::empty();
    let mut env = sb.env(Scope::User);
    let dir = sb.path("elsewhere/copilot");
    env.locations
        .insert("COPILOT_HOME".into(), dir.to_string_lossy().into_owned());
    let mut prof = sample();
    prof.skills.push(skill(
        "Notes_Tool",
        "---\nname: Notes_Tool\ndescription: Notes\n---\nTake notes.\n",
    ));
    let p = plan(&prof, &["copilot"], &env);
    assert!(planned(&p, &dir.join("skills/Notes_Tool/SKILL.md")).is_some());
    // No user prompt files: commands stay unsupported there.
    assert!(
        !p.changes
            .iter()
            .any(|c| c.path.to_string_lossy().contains("prompt"))
    );
    assert!(p.notes.iter().any(|n| {
        n.target == "copilot"
            && n.message
                .contains("skill 'Notes_Tool': VS Code only loads skills named with lowercase")
    }));
}

#[test]
fn copilot_project_capture_takes_a_bare_dollar_as_a_reference() {
    let sb = Sandbox::empty();
    std::fs::write(
        sb.project().join(".mcp.json"),
        r#"{"mcpServers": {"gh": {"command": "gh-mcp", "env": {"GITHUB_TOKEN": "$GITHUB_TOKEN"}}}}"#,
    )
    .unwrap();
    std::fs::create_dir_all(sb.project().join(".github/copilot")).unwrap();
    let p = capture(&["copilot"], &sb.env(Scope::Project)).profile;
    assert_eq!(p.mcp_servers["gh"].env["GITHUB_TOKEN"], "${GITHUB_TOKEN}");
}

#[test]
fn copilot_warns_about_references_in_url_and_cwd() {
    let sb = Sandbox::empty();
    let mut prof = Profile::new("t");
    prof.mcp_servers.insert(
        "api".into(),
        McpServer {
            url: Some("https://${API_HOST}/mcp".into()),
            ..Default::default()
        },
    );
    prof.mcp_servers.insert(
        "local".into(),
        McpServer {
            command: Some("local-mcp".into()),
            cwd: Some("${WORK_DIR}".into()),
            ..Default::default()
        },
    );
    let p = plan(&prof, &["copilot"], &sb.env(Scope::User));
    for (name, field) in [("api", "url"), ("local", "cwd")] {
        assert!(
            p.notes.iter().any(|n| n.message.contains(&format!(
                "MCP server '{name}': env reference in {field}; Copilot's docs only say"
            ))),
            "{name}: {:?}",
            p.notes
        );
    }
    let cfg: serde_json::Value =
        serde_json::from_str(&planned(&p, &sb.home().join(".copilot/mcp-config.json")).unwrap())
            .unwrap();
    assert_eq!(cfg["mcpServers"]["api"]["url"], "https://${API_HOST}/mcp");
}

#[test]
fn copilot_leaves_a_bare_format_mcp_json_alone() {
    let sb = Sandbox::empty();
    let f = sb.project().join(".mcp.json");
    let bare = "{ \"pw\": { \"command\": \"npx\", \"args\": [\"@playwright/mcp\"] } }\n";
    std::fs::write(&f, bare).unwrap();
    let p = plan(&sample(), &["copilot"], &sb.env(Scope::Project));
    assert!(planned(&p, &f).is_none());
    assert!(
        p.notes
            .iter()
            .any(|n| n.message.contains("without mcpServers"))
    );
}

/// Claude Code and Copilot both read the project `.mcp.json`: applying to
/// both, together or one after the other, writes each entry once and neither
/// changes what the other wrote. Undo restores the file.
#[test]
fn claude_code_and_copilot_share_mcp_json() {
    let prof = sample();
    let sb = Sandbox::empty();
    let env = sb.env(Scope::Project);
    let mcp = sb.project().join(".mcp.json");
    std::fs::write(
        &mcp,
        "{\n  \"mcpServers\": {\n    \"mine\": { \"command\": \"my-mcp\" }\n  }\n}\n",
    )
    .unwrap();
    let before = std::fs::read_to_string(&mcp).unwrap();
    let p = apply(&sb, &prof, &["claude-code", "copilot"], &env);
    assert_eq!(p.changes.iter().filter(|c| c.path == mcp).count(), 1);
    let doc = sb.json("project/.mcp.json");
    assert_eq!(doc["mcpServers"]["mine"], json!({"command": "my-mcp"}));
    assert_eq!(doc["mcpServers"]["github"]["type"], "stdio");
    assert!(doc["mcpServers"]["github"].get("tools").is_none());
    for ids in [
        &["copilot"][..],
        &["claude-code"],
        &["copilot", "claude-code"],
    ] {
        let again = plan(&prof, ids, &env);
        assert!(again.changes.is_empty(), "{ids:?}: {:?}", again.changes);
    }
    Store::new(sb.state()).undo(None, false).unwrap();
    assert_eq!(std::fs::read_to_string(&mcp).unwrap(), before);

    for order in [["copilot", "claude-code"], ["claude-code", "copilot"]] {
        let sb = Sandbox::empty();
        let env = sb.env(Scope::Project);
        apply(&sb, &prof, &[order[0]], &env);
        let first = sb.read("project/.mcp.json");
        let second = apply(&sb, &prof, &[order[1]], &env);
        assert!(
            !second.changes.iter().any(|c| c.path.ends_with(".mcp.json")),
            "{order:?}: the second target leaves .mcp.json as the first wrote it"
        );
        assert_eq!(sb.read("project/.mcp.json"), first);
    }
}

/// Copilot instructions and rules have their own files, so applying next to
/// Codex and Claude Code adds no second block to AGENTS.md or CLAUDE.md.
#[test]
fn copilot_instructions_have_their_own_files() {
    let sb = Sandbox::empty();
    let env = sb.env(Scope::Project);
    let prof = sample();
    apply(&sb, &prof, &["codex", "claude-code", "copilot"], &env);
    for f in [
        "project/AGENTS.md",
        "project/CLAUDE.md",
        "project/.github/copilot-instructions.md",
    ] {
        let text = sb.read(f);
        assert_eq!(
            text.matches("<!-- kitbag:begin team -->").count(),
            1,
            "{f}: {text}"
        );
    }
    assert!(plan(&prof, &["copilot"], &env).changes.is_empty());
}

// ------------------------------------------------------------ grok build

#[test]
fn grok_build_capture() {
    let sb = Sandbox::fixtures();
    let r = capture(&["grok-build"], &sb.env(Scope::User));
    let p = &r.profile;

    let gh = &p.mcp_servers["github"];
    assert_eq!(gh.command.as_deref(), Some("npx"));
    // The pasted token is redacted; Grok's own ${VAR:-default} is kept.
    assert_eq!(
        gh.env["GITHUB_PERSONAL_ACCESS_TOKEN"],
        "${GITHUB_PERSONAL_ACCESS_TOKEN}"
    );
    assert_eq!(gh.env["LOG_LEVEL"], "${LOG_LEVEL:-info}");
    let linear = &p.mcp_servers["linear"];
    assert_eq!(linear.transport(), Transport::Http);
    assert_eq!(linear.headers["Authorization"], "Bearer ${LINEAR_API_KEY}");
    assert!(p.mcp_servers["old"].disabled);

    assert!(
        p.instructions
            .as_deref()
            .unwrap()
            .contains("British English")
    );
    assert_eq!(p.rules.len(), 1);
    assert_eq!(p.rules[0].name, "commits");
    assert!(p.rules[0].globs.is_empty());
    assert_eq!(p.commands[0].name, "explain");
    assert_eq!(p.commands[0].argument_hint.as_deref(), Some("[path]"));
    assert_eq!(p.skills[0].name, "release-notes");
    assert!(p.agents.is_empty());

    let locations: Vec<&str> = r.redactions.iter().map(|x| x.location.as_str()).collect();
    assert_eq!(
        locations,
        vec!["mcp_servers.github.env.GITHUB_PERSONAL_ACCESS_TOKEN"]
    );
    let notes: Vec<&str> = r.notes.iter().map(|n| n.message.as_str()).collect();
    assert!(
        notes.iter().any(|n| n.contains("startup_timeout_sec")),
        "{notes:?}"
    );
    assert!(
        notes.iter().any(|n| n.contains("agent definitions")),
        "{notes:?}"
    );
}

#[test]
fn grok_build_project_capture_unwraps_scoped_rules() {
    let sb = Sandbox::fixtures();
    let p = capture(&["grok-build"], &sb.env(Scope::Project)).profile;
    let pg = &p.mcp_servers["postgres"];
    assert_eq!(pg.args, vec!["--dsn", "${DATABASE_URL}"]);
    assert_eq!(pg.cwd.as_deref(), Some("tools"));
    let py = p.rules.iter().find(|r| r.name == "python").unwrap();
    assert_eq!(py.globs, vec!["*.py"]);
    assert_eq!(py.description.as_deref(), Some("Python style"));
    assert_eq!(py.body, "Use type hints everywhere.");
    assert_eq!(p.skills[0].name, "db-migrate");
}

#[test]
fn grok_build_apply_user() {
    let sb = Sandbox::empty();
    let p = plan(&sample(), &["grok-build"], &sb.env(Scope::User));
    let cfg = planned(&p, &sb.home().join(".grok/config.toml")).unwrap();
    assert!(cfg.contains("[mcp_servers.github]"), "{cfg}");
    // Grok expands references itself: written as they are, nothing resolved.
    assert!(cfg.contains("GITHUB_TOKEN = \"${GITHUB_TOKEN}\""), "{cfg}");
    // A `Bearer ${VAR}` header is Grok's bearer_token_env_var.
    assert!(
        cfg.contains("bearer_token_env_var = \"LINEAR_API_KEY\""),
        "{cfg}"
    );
    assert!(!cfg.contains("Authorization"), "{cfg}");
    assert!(p.sensitive.is_empty() && p.missing_env.is_empty());

    let agents = planned(&p, &sb.home().join(".grok/AGENTS.md")).unwrap();
    assert!(agents.contains("Use British English."));
    assert!(
        !agents.contains("Strict mode."),
        "rules get their own files"
    );
    let rule = planned(&p, &sb.home().join(".grok/rules/ts.md")).unwrap();
    assert!(
        rule.contains("_Applies when working with files matching: `src/**/*.ts`_\n\nStrict mode."),
        "{rule}"
    );
    let cmd = planned(&p, &sb.home().join(".grok/commands/review.md")).unwrap();
    assert!(cmd.contains("Review $ARGUMENTS"), "{cmd}");
    assert!(
        p.notes
            .iter()
            .any(|n| n.target == "grok-build" && n.message.contains("no file-scoped rules")),
        "{:?}",
        p.notes
    );
}

#[test]
fn grok_build_apply_project() {
    let sb = Sandbox::empty();
    let mut prof = sample();
    prof.skills = capture(&["grok-build"], &Sandbox::fixtures().env(Scope::User))
        .profile
        .skills;
    let p = plan(&prof, &["grok-build"], &sb.env(Scope::Project));
    assert!(planned(&p, &sb.project().join(".grok/config.toml")).is_some());
    assert!(planned(&p, &sb.project().join("AGENTS.md")).is_some());
    assert!(planned(&p, &sb.project().join(".grok/rules/ts.md")).is_some());
    assert!(planned(&p, &sb.project().join(".grok/commands/review.md")).is_some());
    assert!(
        planned(
            &p,
            &sb.project().join(".grok/skills/release-notes/SKILL.md")
        )
        .is_some()
    );
    let notes: Vec<&str> = p.notes.iter().map(|n| n.message.as_str()).collect();
    assert!(notes.iter().any(|n| n.contains("trusted")), "{notes:?}");
    assert!(
        notes.iter().any(|n| n.contains("replaces a user server")),
        "{notes:?}"
    );
}

#[test]
fn grok_build_follows_grok_home() {
    let sb = Sandbox::empty();
    let mut env = sb.env(Scope::User);
    let custom = sb.path("grok-home");
    env.locations
        .insert("GROK_HOME".into(), custom.to_string_lossy().into_owned());
    let p = plan(&sample(), &["grok-build"], &env);
    assert!(planned(&p, &custom.join("config.toml")).is_some());
    assert!(planned(&p, &custom.join("AGENTS.md")).is_some());
    assert!(planned(&p, &sb.home().join(".grok/config.toml")).is_none());
}

#[test]
fn grok_build_apply_merges_into_existing_toml_and_keeps_its_keys() {
    let sb = Sandbox::empty();
    let cfg_path = sb.home().join(".grok/config.toml");
    std::fs::create_dir_all(cfg_path.parent().unwrap()).unwrap();
    std::fs::write(
        &cfg_path,
        "# mine\n[ui]\ntheme = \"dark\"\n\n# keep me\n[mcp_servers.github]\ncommand = \"npx\"\nargs = [\"-y\", \"@modelcontextprotocol/server-github\"]\nenv = { GITHUB_TOKEN = \"old\" }\nstartup_timeout_sec = 90\ntool_timeout_sec = 600\n",
    )
    .unwrap();
    apply(&sb, &sample(), &["grok-build"], &sb.env(Scope::User));
    let text = std::fs::read_to_string(&cfg_path).unwrap();
    assert!(text.starts_with("# mine\n[ui]\ntheme = \"dark\""), "{text}");
    assert!(text.contains("# keep me\n[mcp_servers.github]"), "{text}");
    assert!(
        text.contains("GITHUB_TOKEN = \"${GITHUB_TOKEN}\""),
        "{text}"
    );
    assert!(text.contains("startup_timeout_sec = 90"), "{text}");
    assert!(text.contains("tool_timeout_sec = 600"), "{text}");
    assert!(text.contains("[mcp_servers.linear]"), "{text}");
    // Idempotent.
    assert!(
        plan(&sample(), &["grok-build"], &sb.env(Scope::User))
            .changes
            .is_empty()
    );
    Store::new(sb.state()).undo(None, false).unwrap();
    assert!(
        std::fs::read_to_string(&cfg_path)
            .unwrap()
            .contains("GITHUB_TOKEN = \"old\"")
    );
}

#[test]
fn grok_build_keeps_env_refs_with_defaults_and_writes_disabled_servers() {
    let sb = Sandbox::empty();
    let mut prof = Profile::new("team");
    prof.mcp_servers.insert(
        "db".into(),
        McpServer {
            command: Some("db-mcp".into()),
            args: vec!["--url".into(), "${DB_URL:-postgres://localhost/dev}".into()],
            disabled: true,
            ..Default::default()
        },
    );
    prof.mcp_servers.insert(
        "events".into(),
        McpServer {
            url: Some("https://example.com/sse".into()),
            transport: Some(Transport::Sse),
            ..Default::default()
        },
    );
    let p = plan(&prof, &["grok-build"], &sb.env(Scope::User));
    let cfg = planned(&p, &sb.home().join(".grok/config.toml")).unwrap();
    assert!(
        cfg.contains("\"${DB_URL:-postgres://localhost/dev}\""),
        "{cfg}"
    );
    assert!(cfg.contains("enabled = false"), "{cfg}");
    // Grok Build speaks SSE when `type = "sse"`.
    assert!(
        cfg.contains("[mcp_servers.events]\nurl = \"https://example.com/sse\"\ntype = \"sse\""),
        "{cfg}"
    );
}

#[test]
fn grok_build_capture_reads_sse_bearer_and_disabled_list_then_reapplies_cleanly() {
    let sb = Sandbox::empty();
    let cfg_path = sb.home().join(".grok/config.toml");
    std::fs::create_dir_all(cfg_path.parent().unwrap()).unwrap();
    let original = "disabled_mcp_servers = [\"gh\"]\n\n[mcp_servers.linear]\ntype = \"sse\"\nurl = \"https://mcp.linear.app/stream\"\ntool_timeout_sec = 30\n\n[mcp_servers.events]\nurl = \"https://example.com/sse\"\n\n[mcp_servers.api]\nurl = \"https://api.example.com/mcp\"\nbearer_token_env_var = \"API_TOKEN\"\nstartup_timeout_sec = 5\n\n[mcp_servers.gh]\ncommand = \"gh-mcp\"\n";
    std::fs::write(&cfg_path, original).unwrap();
    let env = sb.env(Scope::User);
    let p = capture(&["grok-build"], &env).profile;
    assert_eq!(p.mcp_servers["linear"].transport(), Transport::Sse);
    assert_eq!(p.mcp_servers["events"].transport(), Transport::Sse);
    assert_eq!(p.mcp_servers["api"].transport(), Transport::Http);
    assert_eq!(
        p.mcp_servers["api"].headers["Authorization"],
        "Bearer ${API_TOKEN}"
    );
    assert!(p.mcp_servers["gh"].disabled);

    // Applying what was just captured adds `type = "sse"` to the server Grok
    // only knew as SSE from its URL, and leaves everything else alone.
    apply(&sb, &p, &["grok-build"], &env);
    let text = std::fs::read_to_string(&cfg_path).unwrap();
    assert!(
        text.contains("bearer_token_env_var = \"API_TOKEN\""),
        "{text}"
    );
    assert!(!text.contains("Authorization"), "{text}");
    assert!(text.contains("tool_timeout_sec = 30"), "{text}");
    assert!(text.contains("startup_timeout_sec = 5"), "{text}");
    assert!(text.contains("disabled_mcp_servers = [\"gh\"]"), "{text}");
    assert!(plan(&p, &["grok-build"], &env).changes.is_empty());

    // An enabled server Grok keeps off through the list gets a warning.
    let mut on = p.clone();
    on.mcp_servers.get_mut("gh").unwrap().disabled = false;
    let p2 = plan(&on, &["grok-build"], &env);
    assert!(
        p2.notes
            .iter()
            .any(|n| n.message.contains("disabled_mcp_servers")),
        "{:?}",
        p2.notes
    );
}

#[test]
fn grok_build_shares_agents_md_with_codex_cursor_antigravity_and_opencode() {
    let sb = Sandbox::empty();
    let env = sb.env(Scope::Project);
    let prof = sample();
    let agents = sb.project().join("AGENTS.md");
    std::fs::write(&agents, "My own notes.\n").unwrap();
    let p = apply(
        &sb,
        &prof,
        &["codex", "cursor", "antigravity", "opencode", "grok-build"],
        &env,
    );
    assert_eq!(
        p.changes.iter().filter(|c| c.path == agents).count(),
        1,
        "AGENTS.md is planned once"
    );
    let text = sb.read("project/AGENTS.md");
    assert_eq!(text.matches("Use British English.").count(), 1, "{text}");
    // Codex folded the rules into AGENTS.md, which Grok reads as well.
    assert!(
        p.notes
            .iter()
            .any(|n| n.target == "grok-build" && n.message.contains("sees the rules twice")),
        "{:?}",
        p.notes
    );
    assert!(plan(&prof, &["grok-build"], &env).changes.is_empty());
    // Grok's capture leaves out the rules Codex folded in; its own files hold them.
    let r = capture(&["grok-build"], &env);
    assert_eq!(r.profile.rules, prof.rules);
    assert!(
        r.notes
            .iter()
            .any(|n| n.message.contains("were not captured (ts)"))
    );
    Store::new(sb.state()).undo(None, false).unwrap();
    assert_eq!(std::fs::read_to_string(&agents).unwrap(), "My own notes.\n");
    assert!(!sb.project().join(".grok").exists());
}

#[test]
fn grok_build_notes_instructions_loading_twice_with_claude_code() {
    let sb = Sandbox::empty();
    let p = plan(
        &sample(),
        &["claude-code", "grok-build"],
        &sb.env(Scope::User),
    );
    assert!(
        p.notes
            .iter()
            .any(|n| n.target == "grok-build" && n.message.contains("load twice")),
        "{:?}",
        p.notes
    );
    let alone = plan(&sample(), &["grok-build"], &sb.env(Scope::User));
    assert!(!alone.notes.iter().any(|n| n.message.contains("load twice")));
}

#[test]
fn grok_build_warns_about_positional_arguments() {
    let sb = Sandbox::empty();
    let mut prof = sample();
    prof.commands[0].body = "Compare $1 with $2".into();
    let p = plan(&prof, &["grok-build"], &sb.env(Scope::User));
    assert!(
        p.notes
            .iter()
            .any(|n| n.target == "grok-build" && n.message.contains("from $0")),
        "{:?}",
        p.notes
    );
    // $ARGUMENTS alone is fine.
    let p = plan(&sample(), &["grok-build"], &sb.env(Scope::User));
    assert!(!p.notes.iter().any(|n| n.message.contains("from $0")));
}

#[test]
fn grok_is_an_alias_on_the_command_line() {
    let sb = Sandbox::empty();
    std::fs::create_dir_all(sb.path("p")).unwrap();
    std::fs::write(
        sb.path("p/kitbag.toml"),
        "kitbag = 1\nname = \"p\"\n\n[mcp_servers.test]\ncommand = \"c\"\n",
    )
    .unwrap();
    sb.cli()
        .args(["apply", "p", "--to", "grok", "--yes"])
        .assert()
        .success();
    assert!(
        sb.read("home/.grok/config.toml")
            .contains("[mcp_servers.test]")
    );
}
