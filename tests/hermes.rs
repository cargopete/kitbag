//! Hermes Agent: MCP servers in `~/.hermes/config.yaml` (`mcp_servers`) and
//! instructions in `SOUL.md` (user) or `.hermes.md` / `AGENTS.md` (project),
//! and skills in `~/.hermes/skills` and a project's `.hermes/skills`.

mod common;

use std::collections::HashMap;
use std::fs;

use common::*;
use kitbag::adapters::Scope;
use kitbag::model::{Command, McpServer, Profile, Rule, Skill, SkillFile, Transport};
use kitbag::profile_io;
use kitbag::txn::Store;
use serde_json::{Value, json};

const CONFIG: &str = "home/.hermes/config.yaml";
const SOUL: &str = "home/.hermes/SOUL.md";

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
            cwd: Some("tools".into()),
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
        body: "Strict mode.".into(),
        ..Default::default()
    });
    p
}

/// The YAML file as JSON, to compare what it means.
fn yaml(text: &str) -> Value {
    serde_norway::from_str(text).unwrap()
}

fn config_at(p: &kitbag::engine::Plan, sb: &Sandbox) -> String {
    planned(p, &sb.home().join(".hermes/config.yaml")).unwrap()
}

#[test]
fn capture_user() {
    let sb = Sandbox::fixtures();
    let r = capture(&["hermes"], &sb.env(Scope::User));
    let p = &r.profile;
    let fs_server = &p.mcp_servers["filesystem"];
    assert_eq!(fs_server.command.as_deref(), Some("npx"));
    assert_eq!(fs_server.args.len(), 3);
    let gh = &p.mcp_servers["github"];
    assert_eq!(
        gh.args,
        vec![
            "-y".to_string(),
            "@modelcontextprotocol/server-github".into()
        ]
    );
    assert_eq!(
        gh.env["GITHUB_PERSONAL_ACCESS_TOKEN"],
        "${GITHUB_PERSONAL_ACCESS_TOKEN}"
    );
    // ${env:VAR} comes back as ${VAR}; a literal key is redacted.
    let api = &p.mcp_servers["remote_api"];
    assert_eq!(api.url.as_deref(), Some("https://mcp.example.com/mcp"));
    assert_eq!(api.transport(), Transport::Http);
    assert_eq!(api.headers["Authorization"], "Bearer ${REMOTE_API_TOKEN}");
    assert_eq!(api.headers["X-Api-Key"], "${REMOTE_API_X_API_KEY}");
    assert!(!r.redactions.is_empty());
    assert_eq!(p.mcp_servers["events"].transport(), Transport::Sse);
    assert!(p.mcp_servers["time"].disabled);
    assert!(!p.mcp_servers["github"].disabled);
    // SOUL.md holds only the persona here, which is not portable instructions.
    assert_eq!(p.instructions, None);
    let said = |what: &str| r.notes.iter().any(|n| n.message.contains(what));
    assert!(
        said("Hermes-specific keys not captured: timeout"),
        "{:?}",
        r.notes
    );
    assert!(said("keys not captured: tools"));
    assert!(said("'linear' signs in with OAuth"));
    assert!(said("SOUL.md is Hermes's persona file"));
}

#[test]
fn capture_project_reads_agents_md_and_prefers_hermes_md() {
    let sb = Sandbox::fixtures();
    let r = capture(&["hermes"], &sb.env(Scope::Project));
    assert!(r.profile.mcp_servers.is_empty(), "no project MCP file");
    assert!(
        r.profile
            .instructions
            .as_deref()
            .unwrap()
            .contains("pnpm test")
    );

    // Hermes loads .hermes.md first, so that is what capture reads.
    fs::write(sb.project().join(".hermes.md"), "Hermes-only rules.\n").unwrap();
    let r = capture(&["hermes"], &sb.env(Scope::Project));
    assert_eq!(
        r.profile.instructions.as_deref(),
        Some("Hermes-only rules.")
    );
}

/// A fresh Hermes install seeds a SOUL.md persona. A default capture (every
/// detected tool) must not carry that text into the instructions other tools
/// would receive, while CLAUDE.md still does.
#[test]
fn default_capture_keeps_the_soul_persona_out_of_shared_instructions() {
    let sb = Sandbox::fixtures();
    let env = sb.env(Scope::User);
    let claude = sb.read("home/.claude/CLAUDE.md");
    let r = capture(&["claude-code", "hermes"], &env);
    let instructions = r.profile.instructions.clone().unwrap();
    assert!(!instructions.contains("plain-spoken"), "{instructions}");
    assert!(
        !instructions.contains("Keep diffs small."),
        "{instructions}"
    );
    assert!(instructions.contains(claude.trim()), "{instructions}");

    // Applied to Codex and Hermes, the persona doesn't reach AGENTS.md or
    // anywhere else, and the persona file keeps its own text.
    let sb2 = Sandbox::empty();
    let env2 = sb2.env(Scope::User);
    fs::create_dir_all(sb2.home().join(".hermes")).unwrap();
    fs::write(sb2.home().join(".hermes/SOUL.md"), "You are the persona.\n").unwrap();
    let p = apply(&sb2, &r.profile, &["codex", "hermes"], &env2);
    let codex = planned(&p, &sb2.home().join(".codex/AGENTS.md")).unwrap();
    assert!(!codex.contains("persona"), "{codex}");
    let soul = sb2.read("home/.hermes/SOUL.md");
    assert!(soul.starts_with("You are the persona."));
    assert!(soul.contains(claude.trim()));
}

/// Only a block kitbag wrote into SOUL.md is captured; the persona around it
/// is left out, with a note.
#[test]
fn capture_reads_only_the_managed_block_in_soul() {
    let sb = Sandbox::empty();
    let env = sb.env(Scope::User);
    fs::create_dir_all(sb.home().join(".hermes")).unwrap();
    fs::write(sb.home().join(".hermes/SOUL.md"), "You are the persona.\n").unwrap();
    let prof = Profile {
        instructions: Some("Use British English.".into()),
        ..Profile::default()
    };
    apply(&sb, &prof, &["hermes"], &env);
    let r = capture(&["hermes"], &env);
    assert_eq!(
        r.profile.instructions.as_deref(),
        Some("Use British English.")
    );
    assert!(r.notes.iter().any(|n| n.message.contains("persona file")));
}

#[test]
fn capture_leaves_out_rules_folded_by_other_tools() {
    let sb = Sandbox::empty();
    apply(&sb, &sample(), &["codex"], &sb.env(Scope::Project));
    let r = capture(&["hermes"], &sb.env(Scope::Project));
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

/// `.env`, `auth.json` and `mcp-tokens/` hold credentials: never read, never
/// captured, never touched by apply.
#[test]
fn credentials_are_never_read_captured_or_written() {
    let sb = Sandbox::fixtures();
    let home = sb.home().join(".hermes");
    fs::write(
        home.join(".env"),
        "OPENROUTER_API_KEY=sk-or-FAKEhermesenvvalue123456\n",
    )
    .unwrap();
    fs::write(
        home.join("auth.json"),
        r#"{"access_token": "ghp_FAKEhermesauthvalue0123456789abcdef"}"#,
    )
    .unwrap();
    fs::create_dir_all(home.join("mcp-tokens")).unwrap();
    fs::write(
        home.join("mcp-tokens/linear.json"),
        "ghp_FAKEhermestokenvalue0123456789abcdef",
    )
    .unwrap();
    fs::create_dir_all(home.join("memories")).unwrap();
    fs::create_dir_all(home.join("sessions")).unwrap();

    let r = capture(&["hermes"], &sb.env(Scope::User));
    let warned = r
        .notes
        .iter()
        .find(|n| n.message.contains("found credentials"));
    let warned = warned.expect("warns that credentials were found");
    assert!(
        warned.message.contains(".env, auth.json, mcp-tokens"),
        "{}",
        warned.message
    );
    assert!(
        r.notes
            .iter()
            .any(|n| n.message.contains("Hermes memories, sessions not captured"))
    );
    let dir = sb.path("profile");
    profile_io::save(&r.profile, &dir, false).unwrap();
    let all = all_text_under(&dir);
    assert!(
        !all.contains("FAKEhermes"),
        "a credential leaked into the profile"
    );
    assert!(
        r.sources
            .iter()
            .all(|s| !s.to_string_lossy().contains("auth.json") && !s.ends_with(".env"))
    );

    let p = plan(&sample(), &["hermes"], &sb.env(Scope::User));
    for c in &p.changes {
        let path = c.path.to_string_lossy();
        assert!(
            !path.ends_with(".env") && !path.contains("auth.json") && !path.contains("mcp-tokens"),
            "{path}"
        );
    }
}

#[test]
fn hermes_home_moves_the_config() {
    let sb = Sandbox::empty();
    let other = sb.path("elsewhere/coder");
    fs::create_dir_all(&other).unwrap();
    fs::write(
        other.join("config.yaml"),
        "mcp_servers:\n  time:\n    command: uvx\n",
    )
    .unwrap();
    fs::write(other.join("SOUL.md"), "I am the coder.\n").unwrap();
    let mut env = sb.env(Scope::User);
    env.locations = HashMap::from([("HERMES_HOME".to_string(), other.display().to_string())]);
    let r = capture(&["hermes"], &env);
    assert!(r.profile.mcp_servers.contains_key("time"));
    assert_eq!(r.profile.instructions, None);

    let p = plan(&sample(), &["hermes"], &env);
    assert!(
        planned(&p, &other.join("config.yaml"))
            .unwrap()
            .contains("github")
    );
    assert!(planned(&p, &other.join("SOUL.md")).is_some());
    assert!(p.changes.iter().all(|c| !c.path.starts_with(sb.home())));
    // The sandbox's own home is untouched, and --home ignores the variable.
    assert!(kitbag::adapters::LOCATION_VARS.contains(&"HERMES_HOME"));
}

#[test]
fn detect_looks_for_hermes_not_shared_files() {
    let sb = Sandbox::empty();
    let a = kitbag::adapters::get("hermes").unwrap();
    for scope in [Scope::User, Scope::Project] {
        assert!(!a.detect(&sb.env(scope)));
    }
    fs::write(sb.project().join("AGENTS.md"), "x\n").unwrap();
    assert!(!a.detect(&sb.env(Scope::Project)), "AGENTS.md is shared");
    fs::create_dir_all(sb.home().join(".hermes")).unwrap();
    assert!(a.detect(&sb.env(Scope::User)));
    fs::write(sb.project().join(".hermes.md"), "x\n").unwrap();
    assert!(a.detect(&sb.env(Scope::Project)));
}

#[test]
fn apply_user_writes_env_references_not_secrets() {
    let sb = Sandbox::empty();
    let env = sb.env_with(Scope::User, &[("GITHUB_TOKEN", "ghp_FAKEvalue123")]);
    let p = plan(&sample(), &["hermes"], &env);
    let text = config_at(&p, &sb);
    let cfg = yaml(&text);
    // Hermes expands ${VAR} itself, so the secret never touches disk.
    assert_eq!(
        cfg["mcp_servers"]["github"],
        json!({"command": "npx", "args": ["-y", "@modelcontextprotocol/server-github"], "env": {"GITHUB_TOKEN": "${GITHUB_TOKEN}"}, "cwd": "tools"})
    );
    assert_eq!(
        cfg["mcp_servers"]["linear"],
        json!({"url": "https://mcp.linear.app/mcp", "headers": {"Authorization": "Bearer ${LINEAR_API_KEY}"}})
    );
    assert!(!text.contains("ghp_FAKE"));
    assert!(p.sensitive.is_empty());
    assert!(
        p.notes
            .iter()
            .any(|n| n.target == "hermes" && n.message.contains("~/.hermes/.env"))
    );
    let soul = planned(&p, &sb.home().join(".hermes/SOUL.md")).unwrap();
    assert!(soul.contains("<!-- kitbag:begin team -->") && soul.contains("Use British English."));
    assert!(!soul.contains("Strict mode."), "rules are not supported");
    assert!(p.notes.iter().any(|n| n.target == "hermes"
        && n.message.contains("not supported")
        && n.message.contains("1 rules")));
    assert_eq!(p.changes.len(), 2, "{:?}", p.changes);
}

#[test]
fn a_default_value_is_resolved_and_never_makes_a_secret_file() {
    let sb = Sandbox::empty();
    let mut prof = Profile::new("team");
    prof.mcp_servers.insert(
        "api".into(),
        McpServer {
            url: Some("https://${HOST:-api.example.com}/mcp".into()),
            ..Default::default()
        },
    );
    let p = plan(&prof, &["hermes"], &sb.env(Scope::User));
    let cfg = yaml(&config_at(&p, &sb));
    assert_eq!(
        cfg["mcp_servers"]["api"]["url"],
        "https://api.example.com/mcp"
    );
}

#[test]
fn apply_transports_cwd_and_disabled() {
    let sb = Sandbox::empty();
    let mut prof = sample();
    prof.mcp_servers.insert(
        "events".into(),
        McpServer {
            url: Some("https://events.example.com/sse".into()),
            transport: Some(Transport::Sse),
            cwd: Some("ignored".into()),
            ..Default::default()
        },
    );
    prof.mcp_servers.insert(
        "time".into(),
        McpServer {
            command: Some("uvx".into()),
            args: vec!["mcp-server-time".into()],
            disabled: true,
            ..Default::default()
        },
    );
    let p = plan(&prof, &["hermes"], &sb.env(Scope::User));
    let cfg = yaml(&config_at(&p, &sb));
    assert_eq!(
        cfg["mcp_servers"]["events"],
        json!({"url": "https://events.example.com/sse", "transport": "sse"})
    );
    assert_eq!(
        cfg["mcp_servers"]["time"],
        json!({"command": "uvx", "args": ["mcp-server-time"], "enabled": false})
    );
    assert!(
        p.notes
            .iter()
            .any(|n| n.message.contains("'cwd' only applies to local servers"))
    );
}

/// A config with comments, other settings and servers of the user's own: only
/// the entries that change are rewritten, the rest is byte for byte as it was.
#[test]
fn apply_merges_into_the_fixture_config_and_keeps_the_rest() {
    let sb = Sandbox::fixtures();
    let original = sb.read(CONFIG);
    let mut prof = Profile::new("team");
    prof.mcp_servers.insert(
        "github".into(),
        McpServer {
            command: Some("npx".into()),
            args: vec!["-y".into(), "@modelcontextprotocol/server-github".into()],
            env: [(
                "GITHUB_PERSONAL_ACCESS_TOKEN".to_string(),
                "${GITHUB_PERSONAL_ACCESS_TOKEN}".to_string(),
            )]
            .into_iter()
            .collect(),
            ..Default::default()
        },
    );
    prof.mcp_servers.insert(
        "filesystem".into(),
        McpServer {
            command: Some("npx".into()),
            args: vec![
                "-y".into(),
                "@modelcontextprotocol/server-filesystem".into(),
                "/work".into(),
            ],
            ..Default::default()
        },
    );
    prof.mcp_servers.insert(
        "added".into(),
        McpServer {
            command: Some("added-mcp".into()),
            ..Default::default()
        },
    );
    let p = plan(&prof, &["hermes"], &sb.env(Scope::User));
    let out = config_at(&p, &sb);
    // github is the same server: not rewritten at all (its `tools` filter and
    // the layout of its args stay), and so is everything outside mcp_servers.
    for kept in [
        "# Hermes config (fixture)\nmodel:\n  default: \"anthropic/claude-sonnet-5\"   # keep this comment\n",
        "terminal:\n  backend: local\n",
        "  github:\n    command: npx\n    args:\n      - -y\n      - \"@modelcontextprotocol/server-github\"\n    env:\n      GITHUB_PERSONAL_ACCESS_TOKEN: \"${GITHUB_PERSONAL_ACCESS_TOKEN}\"\n    tools:\n      include: [create_issue, list_issues]\n",
        "  remote_api:\n    url: \"https://mcp.example.com/mcp\"\n",
        "  events:\n    url: \"https://events.example.com/sse\"\n    transport: sse\n",
        "memory:\n  memory_enabled: true\n",
        "  # filesystem access for the projects folder\n",
    ] {
        assert!(out.contains(kept), "lost {kept:?} in:\n{out}");
    }
    let cfg = yaml(&out);
    let old = yaml(&original);
    for k in ["model", "terminal", "memory"] {
        assert_eq!(cfg[k], old[k], "{k}");
    }
    for name in ["github", "remote_api", "linear", "events", "time"] {
        assert_eq!(cfg["mcp_servers"][name], old["mcp_servers"][name], "{name}");
    }
    // The filesystem server runs something else now, so its own keys go.
    assert_eq!(
        cfg["mcp_servers"]["filesystem"],
        json!({"command": "npx", "args": ["-y", "@modelcontextprotocol/server-filesystem", "/work"]})
    );
    assert_eq!(cfg["mcp_servers"]["added"], json!({"command": "added-mcp"}));
    assert!(p.notes.iter().any(|n| {
        n.message
            .contains("settings of the old entry were not kept: timeout")
    }));
    assert_eq!(
        p.changes
            .iter()
            .filter(|c| c.path.ends_with("config.yaml"))
            .count(),
        1
    );
}

#[test]
fn keys_hermes_owns_survive_when_the_server_is_the_same() {
    let sb = Sandbox::empty();
    let cfg = sb.home().join(".hermes/config.yaml");
    fs::create_dir_all(cfg.parent().unwrap()).unwrap();
    fs::write(
        &cfg,
        "mcp_servers:\n  linear:\n    url: https://mcp.linear.app/mcp\n    auth: oauth\n    timeout: 60\n    enabled: false\n",
    )
    .unwrap();
    let mut prof = Profile::new("team");
    prof.mcp_servers.insert(
        "linear".into(),
        McpServer {
            url: Some("https://mcp.linear.app/mcp".into()),
            headers: [("X-Team".to_string(), "core".to_string())]
                .into_iter()
                .collect(),
            ..Default::default()
        },
    );
    let p = plan(&prof, &["hermes"], &sb.env(Scope::User));
    let out = yaml(&planned(&p, &cfg).unwrap());
    // `enabled` is managed (the profile enables it); auth and timeout are not.
    assert_eq!(
        out["mcp_servers"]["linear"],
        json!({"url": "https://mcp.linear.app/mcp", "auth": "oauth", "timeout": 60, "headers": {"X-Team": "core"}})
    );
}

#[test]
fn a_layout_kitbag_cannot_edit_is_reported_not_written() {
    let sb = Sandbox::empty();
    let cfg = sb.home().join(".hermes/config.yaml");
    fs::create_dir_all(cfg.parent().unwrap()).unwrap();
    let text = "mcp_servers: {old: {command: x}}\nmodel: y\n";
    fs::write(&cfg, text).unwrap();
    let p = plan(&sample(), &["hermes"], &sb.env(Scope::User));
    assert!(planned(&p, &cfg).is_none(), "config.yaml is left alone");
    assert!(
        p.notes
            .iter()
            .any(|n| n.message.contains("can't edit in place"))
    );
    // Instructions are still written.
    assert!(planned(&p, &sb.home().join(".hermes/SOUL.md")).is_some());

    // Invalid YAML is an error that doesn't quote the file.
    fs::write(&cfg, "a: [sk-ant-FAKEhermesbroken\n").unwrap();
    let err = kitbag::engine::plan(
        &sample(),
        &targets(&["hermes"]),
        &sb.env(Scope::User),
        &opts(&sample()),
    )
    .unwrap_err();
    let msg = format!("{err:#}");
    assert!(
        msg.contains("invalid YAML") && !msg.contains("FAKEhermesbroken"),
        "{msg}"
    );
}

#[test]
fn windows_line_endings_are_kept() {
    let sb = Sandbox::empty();
    let cfg = sb.home().join(".hermes/config.yaml");
    fs::create_dir_all(cfg.parent().unwrap()).unwrap();
    fs::write(
        &cfg,
        "model: x\r\nmcp_servers:\r\n  a:\r\n    command: a\r\n",
    )
    .unwrap();
    let p = plan(&sample(), &["hermes"], &sb.env(Scope::User));
    let out = planned(&p, &cfg).unwrap();
    assert!(out.starts_with("model: x\r\nmcp_servers:\r\n  a:\r\n    command: a\r\n"));
    assert_eq!(
        out.matches('\n').count(),
        out.matches("\r\n").count(),
        "{out:?}"
    );
    assert!(out.contains("github"));
}

#[test]
fn apply_is_idempotent_and_undo_restores_everything() {
    let sb = Sandbox::fixtures();
    let env = sb.env(Scope::User);
    let before_cfg = sb.read(CONFIG);
    let before_soul = sb.read(SOUL);
    let prof = sample();
    apply(&sb, &prof, &["hermes"], &env);
    assert!(sb.read(CONFIG).contains("github"));
    let soul = sb.read(SOUL);
    assert!(soul.starts_with("You are a careful, plain-spoken assistant."));
    assert!(soul.contains("Use British English."));
    assert!(plan(&prof, &["hermes"], &env).changes.is_empty());
    // Capturing it back gives the same profile content.
    let back = capture(&["hermes"], &env).profile;
    assert_eq!(
        back.mcp_servers["linear"].url,
        prof.mcp_servers["linear"].url
    );
    assert_eq!(back.mcp_servers["github"].cwd.as_deref(), Some("tools"));
    let back_instructions = back.instructions.unwrap();
    assert!(back_instructions.contains("Use British English."));
    assert!(!back_instructions.contains("plain-spoken"));
    Store::new(sb.state()).undo(None, false).unwrap();
    assert_eq!(sb.read(CONFIG), before_cfg);
    assert_eq!(sb.read(SOUL), before_soul);
}

#[test]
fn apply_to_an_empty_home_creates_files_and_undo_removes_them() {
    let sb = Sandbox::empty();
    apply(&sb, &sample(), &["hermes"], &sb.env(Scope::User));
    assert!(sb.read(CONFIG).starts_with("mcp_servers:\n"));
    Store::new(sb.state()).undo(None, false).unwrap();
    assert!(!sb.home().join(".hermes").exists());
}

#[test]
fn apply_project_has_no_mcp_file_and_says_so() {
    let sb = Sandbox::empty();
    let env = sb.env(Scope::Project);
    let p = plan(&sample(), &["hermes"], &env);
    assert_eq!(p.changes.len(), 1, "{:?}", p.changes);
    assert!(
        planned(&p, &sb.project().join("AGENTS.md"))
            .unwrap()
            .contains("Use British English.")
    );
    assert!(p.notes.iter().any(|n| n.target == "hermes"
        && n.message.contains("no project-level MCP config")
        && n.message.contains("2 MCP server(s) not written")));
}

/// Hermes loads `.hermes.md` before AGENTS.md, so a project that has one gets
/// the block there; HERMES.md too; and AGENTS.override.md is warned about.
#[test]
fn project_instructions_go_where_hermes_reads_them() {
    let prof = sample();
    for name in [".hermes.md", "HERMES.md"] {
        let sb = Sandbox::empty();
        let env = sb.env(Scope::Project);
        fs::write(sb.project().join(name), "Mine.\n").unwrap();
        fs::write(sb.project().join("AGENTS.md"), "Shared.\n").unwrap();
        let p = apply(&sb, &prof, &["hermes"], &env);
        let text = sb.read(&format!("project/{name}"));
        assert!(
            text.starts_with("Mine.") && text.contains("Use British English."),
            "{text}"
        );
        assert_eq!(
            sb.read("project/AGENTS.md"),
            "Shared.\n",
            "AGENTS.md untouched"
        );
        assert!(
            p.notes
                .iter()
                .any(|n| n.message.contains(&format!("has a {name}")))
        );
        assert!(plan(&prof, &["hermes"], &env).changes.is_empty());
        Store::new(sb.state()).undo(None, false).unwrap();
        assert_eq!(sb.read(&format!("project/{name}")), "Mine.\n");
    }

    let sb = Sandbox::empty();
    fs::write(sb.project().join("AGENTS.override.md"), "Private.\n").unwrap();
    let p = plan(&prof, &["hermes"], &sb.env(Scope::Project));
    assert!(
        p.notes
            .iter()
            .any(|n| n.level == kitbag::adapters::Level::Warn
                && n.message.contains("AGENTS.override.md"))
    );
}

/// In a project, AGENTS.md is shared with Codex, Cursor, Antigravity, opencode,
/// Windsurf and Grok Build: one managed block, in any order, in one plan or several.
#[test]
fn hermes_shares_agents_md_with_the_other_targets() {
    let count = |text: &str| text.matches("<!-- kitbag:begin team -->").count();
    let prof = sample();
    let others = [
        "codex",
        "cursor",
        "antigravity",
        "opencode",
        "grok-build",
        "windsurf",
    ];

    let sb = Sandbox::empty();
    let env = sb.env(Scope::Project);
    fs::write(sb.project().join("AGENTS.md"), "My own notes.\n").unwrap();
    let mut together = others.to_vec();
    together.push("hermes");
    let p = apply(&sb, &prof, &together, &env);
    let agents = sb.project().join("AGENTS.md");
    assert_eq!(p.changes.iter().filter(|c| c.path == agents).count(), 1);
    let text = sb.read("project/AGENTS.md");
    assert_eq!(count(&text), 1, "{text}");
    assert_eq!(text.matches("Use British English.").count(), 1, "{text}");
    assert!(text.starts_with("My own notes."));
    assert!(plan(&prof, &["hermes"], &env).changes.is_empty());
    assert!(plan(&prof, &together, &env).changes.is_empty());

    for order in [["hermes", "codex"], ["codex", "hermes"]] {
        let sb = Sandbox::empty();
        let env = sb.env(Scope::Project);
        apply(&sb, &prof, &[order[0]], &env);
        let earlier = sb.read("project/AGENTS.md");
        let second = apply(&sb, &prof, &[order[1]], &env);
        let text = sb.read("project/AGENTS.md");
        assert_eq!(count(&text), 1, "{order:?}: {text}");
        assert_eq!(text.matches("Use British English.").count(), 1, "{order:?}");
        assert!(plan(&prof, &order, &env).changes.is_empty(), "{order:?}");
        if !second.changes.is_empty() {
            Store::new(sb.state()).undo(None, false).unwrap();
            assert_eq!(sb.read("project/AGENTS.md"), earlier, "{order:?}");
        }
    }
}

#[test]
fn hermes_and_codex_agree_when_rules_are_folded() {
    let sb = Sandbox::empty();
    let env = sb.env(Scope::Project);
    let prof = sample();
    apply(&sb, &prof, &["codex"], &env);
    let before = sb.read("project/AGENTS.md");
    assert!(before.contains("Strict mode."));
    assert!(plan(&prof, &["hermes"], &env).changes.is_empty());
    assert!(plan(&prof, &["codex", "hermes"], &env).changes.is_empty());
    assert_eq!(sb.read("project/AGENTS.md"), before);
}

#[test]
fn hermes_to_claude_code_and_back() {
    let src = Sandbox::fixtures();
    let captured = capture(&["hermes"], &src.env(Scope::User)).profile;
    let dir = src.path("profile");
    profile_io::save(&captured, &dir, false).unwrap();
    let loaded = profile_io::load(&dir).unwrap();
    assert_eq!(loaded, captured, "profile survives save + load unchanged");

    let dst = Sandbox::empty();
    apply(&dst, &loaded, &["claude-code"], &dst.env(Scope::User));
    let back = capture(&["claude-code"], &dst.env(Scope::User)).profile;
    for (name, s) in &loaded.mcp_servers {
        if !s.disabled {
            // `auth: oauth` is a marker Claude Code has no key for.
            let mut s = s.clone();
            s.oauth = None;
            assert_eq!(&back.mcp_servers[name], &s, "server {name}");
        }
    }
    assert_eq!(back.instructions, loaded.instructions);

    // And into Hermes in a clean home: the same profile comes back, with
    // the disabled and SSE servers too.
    let again = Sandbox::empty();
    apply(&again, &loaded, &["hermes"], &again.env(Scope::User));
    let round = capture(&["hermes"], &again.env(Scope::User)).profile;
    assert_eq!(round.instructions, loaded.instructions);
    assert_eq!(round.mcp_servers, loaded.mcp_servers);
}

#[test]
fn claude_code_to_hermes_user() {
    let src = Sandbox::fixtures();
    let profile = capture(&["claude-code"], &src.env(Scope::User)).profile;
    let dst = Sandbox::empty();
    apply(&dst, &profile, &["hermes"], &dst.env(Scope::User));
    let back = capture(&["hermes"], &dst.env(Scope::User)).profile;
    for (name, s) in &profile.mcp_servers {
        let mut want = s.clone();
        want.targets = None;
        assert_eq!(&back.mcp_servers[name], &want, "server {name}");
    }
    assert_eq!(back.instructions, profile.instructions);
}

#[test]
fn apply_to_hermes_then_undo_through_the_cli() {
    let sb = Sandbox::empty();
    sb.cli().args(["init", "prof"]).assert().success();
    fs::write(
        sb.path("prof/kitbag.toml"),
        "kitbag = 1\nname = \"prof\"\n\n[mcp_servers.docs]\nurl = \"https://docs.example.com/mcp\"\nheaders = { Authorization = \"Bearer ${DOCS_TOKEN}\" }\n",
    )
    .unwrap();
    sb.cli()
        .args(["apply", "prof", "--to", "hermes", "--yes"])
        .env("DOCS_TOKEN", "fake-token-value")
        .assert()
        .success();
    let text = sb.read(CONFIG);
    assert_eq!(
        yaml(&text)["mcp_servers"]["docs"],
        json!({"url": "https://docs.example.com/mcp", "headers": {"Authorization": "Bearer ${DOCS_TOKEN}"}})
    );
    assert!(!text.contains("fake-token-value"));
    assert!(
        sb.read(SOUL)
            .contains("Write instructions every agent should follow")
    );
    sb.cli().arg("undo").assert().success();
    assert!(
        !sb.home().join(".hermes").exists(),
        "undo removes what apply created"
    );
}

#[test]
fn a_server_the_profile_disables_for_hermes_is_turned_off_only_if_kitbag_added_it() {
    let sb = Sandbox::empty();
    let write = |extra: &str| {
        fs::create_dir_all(sb.path("prof")).unwrap();
        fs::write(
            sb.path("prof/kitbag.toml"),
            format!(
                "kitbag = 1\nname = \"prof\"\n\n[mcp_servers.docs]\nurl = \"https://docs.example.com/mcp\"\n{extra}\n[mcp_servers.mine]\ncommand = \"mine\"\n"
            ),
        )
        .unwrap();
    };
    let cfg = sb.home().join(".hermes/config.yaml");
    fs::create_dir_all(cfg.parent().unwrap()).unwrap();
    fs::write(&cfg, "# top\nmcp_servers:\n  mine:\n    command: mine\n").unwrap();
    write("");
    sb.cli()
        .args(["apply", "prof", "--to", "hermes", "--yes"])
        .assert()
        .success();
    assert!(sb.read(CONFIG).contains("docs:"));
    write("disabled_on = [\"hermes\"]\n");
    sb.cli()
        .args(["apply", "prof", "--to", "hermes", "--yes"])
        .assert()
        .success();
    // Hermes has `enabled: false`, so the entry stays, turned off.
    let text = sb.read(CONFIG);
    assert!(text.contains("docs:"), "{text}");
    let docs = text.split("docs:").nth(1).unwrap();
    assert!(docs.contains("enabled: false"), "{text}");
    assert!(
        text.starts_with("# top\nmcp_servers:\n  mine:\n    command: mine\n"),
        "{text}"
    );
    // Enabling it again clears the flag.
    write("");
    sb.cli()
        .args(["apply", "prof", "--to", "hermes", "--yes"])
        .assert()
        .success();
    assert!(!sb.read(CONFIG).contains("enabled"), "{}", sb.read(CONFIG));
    write("disabled_on = [\"hermes\"]\n");
    sb.cli()
        .args(["apply", "prof", "--to", "hermes", "--yes"])
        .assert()
        .success();
    // `mine` was the user's own: disabling it leaves it, with a note.
    write("disabled_on = [\"hermes\"]\n");
    let fresh = fs::read_to_string(sb.path("prof/kitbag.toml"))
        .unwrap()
        .replace(
            "command = \"mine\"\n",
            "command = \"mine\"\ndisabled_on = [\"hermes\"]\n",
        );
    fs::write(sb.path("prof/kitbag.toml"), fresh).unwrap();
    sb.cli()
        .args(["apply", "prof", "--to", "hermes", "--yes"])
        .assert()
        .success()
        .stdout(predicates::str::contains("no record of adding it"));
    assert!(sb.read(CONFIG).contains("mine:"));
}

// ---------------------------------------------------------------- skills

fn put(sb: &Sandbox, rel: &str, text: &str) {
    let path = sb.path(rel);
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, text).unwrap();
}

fn skill(name: &str, description: &str) -> Skill {
    Skill {
        name: name.into(),
        files: vec![
            SkillFile {
                path: "SKILL.md".into(),
                content: format!(
                    "---\nname: {name}\ndescription: {description}\n---\n\nBody of {name}.\n"
                )
                .into_bytes(),
                executable: false,
            },
            SkillFile {
                path: "scripts/run.sh".into(),
                content: b"echo run\n".to_vec(),
                executable: true,
            },
        ],
        ..Default::default()
    }
}

fn with_skill(name: &str) -> Profile {
    let mut p = Profile::new("team");
    p.skills.push(skill(name, &format!("Does {name} things.")));
    p
}

fn names(p: &Profile) -> Vec<String> {
    let mut v: Vec<String> = p.skills.iter().map(|s| s.name.clone()).collect();
    v.sort();
    v
}

fn said(p: &kitbag::engine::Plan, what: &str) -> bool {
    p.notes
        .iter()
        .any(|n| n.target == "hermes" && n.message.contains(what))
}

fn warned(p: &kitbag::engine::Plan, what: &str) -> bool {
    p.notes.iter().any(|n| {
        n.target == "hermes" && n.level == kitbag::adapters::Level::Warn && n.message.contains(what)
    })
}

fn md(name: &str) -> String {
    format!("---\nname: {name}\ndescription: {name} skill.\n---\n\nText of {name}.\n")
}

#[test]
fn capture_reads_user_skills_flat_and_in_category_folders_and_leaves_hermes_own() {
    let sb = Sandbox::empty();
    let h = "home/.hermes";
    put(
        &sb,
        &format!("{h}/config.yaml"),
        "skills:\n  external_dirs:\n    - ~/.agents/skills\n",
    );
    put(&sb, &format!("{h}/skills/mine/SKILL.md"), &md("mine"));
    put(&sb, &format!("{h}/skills/mine/scripts/run.sh"), "echo hi\n");
    put(
        &sb,
        &format!("{h}/skills/devops/deploy-k8s/SKILL.md"),
        &md("deploy-k8s"),
    );
    put(
        &sb,
        &format!("{h}/skills/devops/deploy-k8s/references/api.md"),
        "ref\n",
    );
    // Seeded by Hermes (v2 and v1 manifest lines) and installed from the hub.
    put(
        &sb,
        &format!("{h}/skills/mlops/axolotl/SKILL.md"),
        &md("axolotl"),
    );
    put(
        &sb,
        &format!("{h}/skills/old-bundled/SKILL.md"),
        &md("old-bundled"),
    );
    put(&sb, &format!("{h}/skills/hubby/SKILL.md"), &md("hubby"));
    put(
        &sb,
        &format!("{h}/skills/.bundled_manifest"),
        "axolotl:0123abcd\nold-bundled\n",
    );
    put(
        &sb,
        &format!("{h}/skills/.hub/lock.json"),
        r#"{"version": 1, "installed": {"hubby": {"source": "x", "install_path": "hubby"}}}"#,
    );
    put(
        &sb,
        &format!("{h}/skills/.hub/quarantine/bad/SKILL.md"),
        &md("bad"),
    );
    put(
        &sb,
        &format!("{h}/skills/.archive/old/SKILL.md"),
        &md("old"),
    );
    // Only Hermes's own folder is read: not the shared ones, not another tool's.
    put(&sb, "home/.agents/skills/shared/SKILL.md", &md("shared"));
    put(
        &sb,
        "home/.claude/skills/claude-only/SKILL.md",
        &md("claude-only"),
    );
    put(&sb, "project/.hermes/skills/proj/SKILL.md", &md("proj"));
    fs::create_dir_all(sb.path(&format!("{h}/memories"))).unwrap();

    let r = capture(&["hermes"], &sb.env(Scope::User));
    assert_eq!(names(&r.profile), ["deploy-k8s", "mine"]);
    let mine = r.profile.skills.iter().find(|s| s.name == "mine").unwrap();
    assert!(mine.files.iter().any(|f| f.path == "scripts/run.sh"));
    let k8s = r
        .profile
        .skills
        .iter()
        .find(|s| s.name == "deploy-k8s")
        .unwrap();
    assert!(k8s.files.iter().any(|f| f.path == "references/api.md"));
    let said = |what: &str| r.notes.iter().any(|n| n.message.contains(what));
    assert!(
        said("3 skill(s) that Hermes bundled or installed"),
        "{:?}",
        r.notes
    );
    assert!(said("axolotl, hubby, old-bundled"));
    assert!(said("skills.external_dirs"));
    assert!(said("Hermes memories not captured"));
    assert!(!said("Hermes skills not captured"));

    let project = capture(&["hermes"], &sb.env(Scope::Project));
    assert_eq!(names(&project.profile), ["proj"]);
}

#[test]
fn capture_warns_about_a_skill_name_in_two_folders() {
    let sb = Sandbox::empty();
    put(&sb, "home/.hermes/skills/a/dup/SKILL.md", &md("dup"));
    put(&sb, "home/.hermes/skills/b/dup/SKILL.md", &md("dup"));
    let r = capture(&["hermes"], &sb.env(Scope::User));
    assert_eq!(names(&r.profile), ["dup"]);
    assert!(
        r.notes
            .iter()
            .any(|n| n.message.contains("'dup' is in more than one folder"))
    );
}

#[test]
fn apply_user_writes_skills_flat_to_hermes_skills_and_undo_removes_only_those() {
    let sb = Sandbox::empty();
    let env = sb.env(Scope::User);
    put(&sb, "home/.hermes/skills/theirs/SKILL.md", &md("theirs"));
    let mut prof = with_skill("deploy-notes");
    prof.rules.push(Rule {
        name: "ts".into(),
        body: "Strict.".into(),
        ..Default::default()
    });
    prof.commands.push(Command {
        name: "release".into(),
        body: "Ship.".into(),
        ..Default::default()
    });
    let p = apply(&sb, &prof, &["hermes"], &env);

    let main = sb.read("home/.hermes/skills/deploy-notes/SKILL.md");
    assert!(
        main.contains("name: deploy-notes") && main.contains("Body of deploy-notes."),
        "{main}"
    );
    assert_eq!(
        sb.read("home/.hermes/skills/deploy-notes/scripts/run.sh"),
        "echo run\n"
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = fs::metadata(sb.path("home/.hermes/skills/deploy-notes/scripts/run.sh"))
            .unwrap()
            .permissions()
            .mode();
        assert_ne!(mode & 0o111, 0, "the executable bit is kept");
    }
    // Nowhere else; rules and commands, which Hermes's docs don't describe, are skipped.
    assert!(!sb.path("home/.agents").exists());
    assert!(!sb.path("home/.claude").exists());
    assert!(!sb.path("home/.hermes/rules").exists());
    assert!(!sb.path("home/.hermes/commands").exists());
    assert!(said(
        &p,
        "not supported by Hermes Agent at user scope, skipped: 1 rules, 1 commands"
    ));

    // A second apply is up to date, and capture reads it back with `theirs`.
    assert!(plan(&prof, &["hermes"], &env).changes.is_empty());
    assert_eq!(
        names(&capture(&["hermes"], &env).profile),
        ["deploy-notes", "theirs"]
    );

    Store::new(sb.state()).undo(None, false).unwrap();
    assert!(!sb.path("home/.hermes/skills/deploy-notes").exists());
    assert!(
        sb.path("home/.hermes/skills/theirs/SKILL.md").is_file(),
        "their skill stays"
    );
}

#[test]
fn apply_updates_a_skill_where_it_is_in_a_category_folder() {
    let sb = Sandbox::empty();
    let env = sb.env(Scope::User);
    put(
        &sb,
        "home/.hermes/skills/devops/deploy-notes/SKILL.md",
        "---\nname: deploy-notes\ndescription: Old.\n---\n\nOld body.\n",
    );
    let prof = with_skill("deploy-notes");
    apply(&sb, &prof, &["hermes"], &env);
    assert!(
        sb.read("home/.hermes/skills/devops/deploy-notes/SKILL.md")
            .contains("Body of deploy-notes.")
    );
    assert!(
        !sb.path("home/.hermes/skills/deploy-notes").exists(),
        "no second, flat copy for Hermes to call ambiguous"
    );
    assert!(plan(&prof, &["hermes"], &env).changes.is_empty());
}

#[test]
fn apply_leaves_skills_hermes_bundled_or_installed_alone() {
    let sb = Sandbox::empty();
    let env = sb.env(Scope::User);
    put(
        &sb,
        "home/.hermes/skills/mlops/axolotl/SKILL.md",
        &md("axolotl"),
    );
    put(
        &sb,
        "home/.hermes/skills/.bundled_manifest",
        "axolotl:abc123\n",
    );
    put(
        &sb,
        "home/.hermes/skills/.hub/lock.json",
        r#"{"version": 1, "installed": {"hubby": {}}}"#,
    );
    let mut prof = Profile::new("team");
    prof.skills.push(skill("axolotl", "Mine."));
    prof.skills.push(skill("hubby", "Mine."));
    prof.skills.push(skill("mine", "Mine."));
    let p = apply(&sb, &prof, &["hermes"], &env);
    assert!(warned(&p, "skill 'axolotl' skipped"));
    assert!(warned(&p, "skill 'hubby' skipped"));
    assert_eq!(
        sb.read("home/.hermes/skills/mlops/axolotl/SKILL.md"),
        md("axolotl")
    );
    assert!(!sb.path("home/.hermes/skills/axolotl").exists());
    assert!(!sb.path("home/.hermes/skills/hubby").exists());
    assert!(sb.path("home/.hermes/skills/mine/SKILL.md").is_file());
}

#[test]
fn apply_project_writes_hermes_skills_not_the_shared_folder_and_notes_trust() {
    let sb = Sandbox::empty();
    let env = sb.env(Scope::Project);
    let prof = with_skill("deploy-notes");
    let p = apply(&sb, &prof, &["hermes"], &env);
    assert!(
        sb.path("project/.hermes/skills/deploy-notes/SKILL.md")
            .is_file()
    );
    assert!(!sb.path("project/.agents").exists());
    assert!(!sb.path("project/.claude").exists());
    assert!(said(&p, "hermes skills trust"));
    assert!(plan(&prof, &["hermes"], &env).changes.is_empty());
    assert_eq!(names(&capture(&["hermes"], &env).profile), ["deploy-notes"]);
    assert!(
        kitbag::adapters::get("hermes").unwrap().detect(&env),
        "a project's .hermes/skills marks it as using Hermes"
    );

    Store::new(sb.state()).undo(None, false).unwrap();
    assert!(!sb.path("project/.hermes").exists());
}

#[test]
fn project_skill_also_in_the_shared_folder_is_noted() {
    let sb = Sandbox::empty();
    put(
        &sb,
        "project/.agents/skills/deploy-notes/SKILL.md",
        &md("deploy-notes"),
    );
    let p = plan(
        &with_skill("deploy-notes"),
        &["hermes"],
        &sb.env(Scope::Project),
    );
    assert!(said(&p, "'deploy-notes' also exists in"));
    // The other tool's copy is not touched.
    assert!(
        p.changes
            .iter()
            .all(|c| !c.path.starts_with(sb.path("project/.agents")))
    );
}

#[test]
fn hermes_home_moves_the_skills_folder() {
    let sb = Sandbox::empty();
    let other = sb.path("elsewhere/coder");
    put(&sb, "elsewhere/coder/skills/cat/mine/SKILL.md", &md("mine"));
    let mut env = sb.env(Scope::User);
    env.locations = HashMap::from([("HERMES_HOME".to_string(), other.display().to_string())]);
    assert_eq!(names(&capture(&["hermes"], &env).profile), ["mine"]);
    let p = plan(&with_skill("deploy-notes"), &["hermes"], &env);
    assert!(planned(&p, &other.join("skills/deploy-notes/SKILL.md")).is_some());
    assert!(p.changes.iter().all(|c| !c.path.starts_with(sb.home())));
}

#[test]
fn apply_keeps_a_differing_skill_with_no_overwrite_and_adds_missing_frontmatter() {
    let sb = Sandbox::empty();
    let env = sb.env(Scope::User);
    put(
        &sb,
        "home/.hermes/skills/deploy-notes/SKILL.md",
        "---\nname: deploy-notes\ndescription: Mine.\n---\n\nMy body.\n",
    );
    let prof = with_skill("deploy-notes");
    let mut o = opts(&prof);
    o.overwrite = false;
    let p = kitbag::engine::plan(&prof, &targets(&["hermes"]), &env, &o).unwrap();
    assert!(warned(&p, "kept existing skill 'deploy-notes'"));
    assert!(planned(&p, &sb.path("home/.hermes/skills/deploy-notes/SKILL.md")).is_none());

    // Hermes needs a name and description; a bare SKILL.md gets them.
    let mut bare = Profile::new("team");
    bare.skills.push(Skill {
        name: "bare".into(),
        description: Some("A bare one.".into()),
        files: vec![SkillFile {
            path: "SKILL.md".into(),
            content: b"Just text.\n".to_vec(),
            executable: false,
        }],
        ..Default::default()
    });
    apply(&sb, &bare, &["hermes"], &env);
    let text = sb.read("home/.hermes/skills/bare/SKILL.md");
    assert!(text.starts_with("---\nname: bare\ndescription:"), "{text}");
}

#[test]
fn skills_honour_targets_and_disabled_on() {
    let sb = Sandbox::empty();
    let env = sb.env(Scope::User);
    let mut prof = Profile::new("team");
    let mut only_cursor = skill("only-cursor", "Cursor only.");
    only_cursor.targeting.targets = Some(vec!["cursor".into()]);
    let mut not_hermes = skill("not-hermes", "Not for Hermes.");
    not_hermes.targeting.disabled_on = vec!["hermes".into()];
    prof.skills
        .extend([only_cursor, not_hermes, skill("everywhere", "All.")]);
    let p = plan(&prof, &["hermes"], &env);
    let has =
        |n: &str| planned(&p, &sb.path(&format!("home/.hermes/skills/{n}/SKILL.md"))).is_some();
    assert!(has("everywhere") && !has("only-cursor") && !has("not-hermes"));
}

#[test]
fn skills_survive_hermes_to_claude_code_and_back() {
    let src = Sandbox::empty();
    let prof = with_skill("deploy-notes");
    apply(&src, &prof, &["hermes"], &src.env(Scope::User));
    let captured = capture(&["hermes"], &src.env(Scope::User)).profile;
    let dir = src.path("profile");
    profile_io::save(&captured, &dir, false).unwrap();
    let loaded = profile_io::load(&dir).unwrap();
    assert_eq!(names(&loaded), ["deploy-notes"]);

    let dst = Sandbox::empty();
    apply(&dst, &loaded, &["claude-code"], &dst.env(Scope::User));
    assert!(
        dst.path("home/.claude/skills/deploy-notes/SKILL.md")
            .is_file()
    );
    let again = Sandbox::empty();
    apply(&again, &loaded, &["hermes"], &again.env(Scope::User));
    assert_eq!(
        capture(&["hermes"], &again.env(Scope::User)).profile.skills,
        captured.skills
    );
}

#[test]
fn apply_skills_to_hermes_through_the_cli_then_remove_one_when_disabled() {
    let sb = Sandbox::empty();
    sb.cli().args(["init", "prof"]).assert().success();
    put(&sb, "prof/skills/notes/SKILL.md", &md("notes"));
    sb.cli()
        .args(["apply", "prof", "--to", "hermes", "--yes"])
        .assert()
        .success();
    assert!(
        sb.read("home/.hermes/skills/notes/SKILL.md")
            .contains("Text of notes.")
    );

    // Disabling it for Hermes removes the folder kitbag wrote.
    put(
        &sb,
        "prof/skills/notes/SKILL.md",
        "---\nname: notes\ndescription: Takes notes.\ndisabled_on: [hermes]\n---\n\nNotes body.\n",
    );
    sb.cli()
        .args(["apply", "prof", "--to", "hermes", "--yes"])
        .assert()
        .success();
    assert!(!sb.path("home/.hermes/skills/notes").exists());
    sb.cli().arg("undo").assert().success();
    assert!(sb.path("home/.hermes/skills/notes/SKILL.md").is_file());
    sb.cli().arg("undo").assert().success();
    assert!(!sb.path("home/.hermes/skills/notes").exists());
}

#[test]
fn skills_never_reach_hermes_credential_files() {
    let sb = Sandbox::empty();
    put(
        &sb,
        "home/.hermes/.env",
        "KEY=sk-or-FAKEhermesenvvalue123456\n",
    );
    put(&sb, "home/.hermes/skills/mine/SKILL.md", &md("mine"));
    let r = capture(&["hermes"], &sb.env(Scope::User));
    let dir = sb.path("profile");
    profile_io::save(&r.profile, &dir, false).unwrap();
    assert!(!all_text_under(&dir).contains("FAKEhermes"));
    let p = plan(&with_skill("x"), &["hermes"], &sb.env(Scope::User));
    assert!(p.changes.iter().all(|c| !c.path.ends_with(".env")));
}
