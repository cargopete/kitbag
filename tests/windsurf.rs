//! Windsurf: MCP servers in `~/.codeium/windsurf/mcp_config.json` and
//! instructions in `global_rules.md` (user) or `AGENTS.md` (project).

mod common;

use std::fs;

use common::*;
use kitbag::adapters::{Scope, SecretsMode};
use kitbag::model::{McpServer, Profile, Rule, Transport};
use kitbag::profile_io;
use kitbag::txn::Store;
use serde_json::json;

const CONFIG: &str = "home/.codeium/windsurf/mcp_config.json";
const GLOBAL_RULES: &str = "home/.codeium/windsurf/memories/global_rules.md";

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
        body: "Strict mode.".into(),
        ..Default::default()
    });
    p.commands.push(kitbag::model::Command {
        name: "review".into(),
        body: "Review $ARGUMENTS".into(),
        ..Default::default()
    });
    p
}

#[test]
fn capture_user() {
    let sb = Sandbox::fixtures();
    let r = capture(&["windsurf"], &sb.env(Scope::User));
    let p = &r.profile;
    // ${env:VAR} comes back as ${VAR}; remote servers take serverUrl or url.
    let figma = &p.mcp_servers["figma"];
    assert_eq!(
        figma.url.as_deref(),
        Some("https://mcp.figma.example.com/mcp")
    );
    assert_eq!(figma.transport(), Transport::Http);
    assert_eq!(figma.headers["Authorization"], "Bearer ${FIGMA_TOKEN}");
    let gh = &p.mcp_servers["github"];
    assert_eq!(gh.command.as_deref(), Some("npx"));
    assert_eq!(
        gh.env["GITHUB_PERSONAL_ACCESS_TOKEN"],
        "${GITHUB_PERSONAL_ACCESS_TOKEN}"
    );
    // A literal key is redacted; Windsurf-only keys (disabledTools) stay behind.
    let sentry = &p.mcp_servers["sentry"];
    assert_eq!(
        sentry.url.as_deref(),
        Some("https://mcp.sentry.example.com/mcp")
    );
    assert_eq!(sentry.headers["X-Api-Key"], "${SENTRY_X_API_KEY}");
    assert!(p.mcp_servers["time"].disabled);
    assert_eq!(
        p.instructions.as_deref(),
        Some("Answer in plain English.\n\nKeep diffs small.")
    );
    assert!(!r.redactions.is_empty());
}

#[test]
fn capture_project_reads_agents_md_and_legacy_windsurfrules() {
    let sb = Sandbox::fixtures();
    let r = capture(&["windsurf"], &sb.env(Scope::Project));
    assert!(r.profile.mcp_servers.is_empty(), "no project MCP file");
    let i = r.profile.instructions.as_deref().unwrap();
    assert!(i.contains("pnpm test") && i.contains("Never commit generated files."));
    assert!(
        r.notes
            .iter()
            .any(|n| n.message.contains("legacy .windsurfrules"))
    );
}

#[test]
fn capture_leaves_out_rules_folded_by_other_tools() {
    let sb = Sandbox::empty();
    apply(&sb, &sample(), &["codex"], &sb.env(Scope::Project));
    let r = capture(&["windsurf"], &sb.env(Scope::Project));
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
fn detect_looks_for_windsurf_not_shared_files() {
    let sb = Sandbox::empty();
    let a = kitbag::adapters::get("windsurf").unwrap();
    for scope in [Scope::User, Scope::Project] {
        assert!(!a.detect(&sb.env(scope)));
    }
    fs::write(sb.project().join("AGENTS.md"), "x\n").unwrap();
    assert!(!a.detect(&sb.env(Scope::Project)), "AGENTS.md is shared");
    fs::create_dir_all(sb.home().join(".codeium/windsurf")).unwrap();
    assert!(a.detect(&sb.env(Scope::User)));
    fs::create_dir_all(sb.project().join(".windsurf")).unwrap();
    assert!(a.detect(&sb.env(Scope::Project)));
}

#[test]
fn apply_user_writes_env_references_not_secrets() {
    let sb = Sandbox::empty();
    let env = sb.env_with(Scope::User, &[("GITHUB_TOKEN", "ghp_FAKEvalue123")]);
    let p = plan(&sample(), &["windsurf"], &env);
    let cfg: serde_json::Value = serde_json::from_str(
        &planned(&p, &sb.home().join(".codeium/windsurf/mcp_config.json")).unwrap(),
    )
    .unwrap();
    // Windsurf expands ${env:VAR} itself, so the secret never touches disk.
    assert_eq!(
        cfg["mcpServers"]["github"],
        json!({"command": "npx", "args": ["-y", "@modelcontextprotocol/server-github"], "env": {"GITHUB_TOKEN": "${env:GITHUB_TOKEN}"}})
    );
    assert_eq!(
        cfg["mcpServers"]["linear"],
        json!({"serverUrl": "https://mcp.linear.app/mcp", "headers": {"Authorization": "Bearer ${env:LINEAR_API_KEY}"}})
    );
    assert!(p.sensitive.is_empty());
    let md = planned(
        &p,
        &sb.home().join(".codeium/windsurf/memories/global_rules.md"),
    )
    .unwrap();
    assert!(md.contains("<!-- kitbag:begin team -->") && md.contains("Use British English."));
    assert!(!md.contains("kitbag:rule"), "rules are not supported yet");
    assert!(p.notes.iter().any(|n| n.target == "windsurf"
        && n.message.contains("not supported")
        && n.message.contains("1 rules")
        && n.message.contains("1 commands")));
    assert_eq!(p.changes.len(), 2, "{:?}", p.changes);
}

#[test]
fn secrets_never_makes_no_difference_to_references() {
    let sb = Sandbox::empty();
    let env = sb.env_with(Scope::User, &[("GITHUB_TOKEN", "ghp_FAKEvalue123")]);
    let prof = sample();
    let mut o = opts(&prof);
    o.secrets = SecretsMode::Never;
    let p = kitbag::engine::plan(&prof, &targets(&["windsurf"]), &env, &o).unwrap();
    let cfg = planned(&p, &sb.home().join(".codeium/windsurf/mcp_config.json")).unwrap();
    assert!(cfg.contains("${env:GITHUB_TOKEN}") && !cfg.contains("ghp_FAKE"));
}

#[test]
fn apply_project_has_no_mcp_file_and_says_so() {
    let sb = Sandbox::empty();
    let env = sb.env(Scope::Project);
    let p = plan(&sample(), &["windsurf"], &env);
    assert_eq!(p.changes.len(), 1, "{:?}", p.changes);
    assert!(
        planned(&p, &sb.project().join("AGENTS.md"))
            .unwrap()
            .contains("Use British English.")
    );
    assert!(p.notes.iter().any(|n| n.target == "windsurf"
        && n.message.contains("no project-level MCP config")
        && n.message.contains("2 MCP server(s) not written")));
    // Nothing is written for .windsurf or .codeium.
    assert!(
        p.changes
            .iter()
            .all(|c| !c.path.to_string_lossy().contains("windsurf"))
    );
}

#[test]
fn apply_merges_into_existing_servers() {
    let sb = Sandbox::empty();
    let cfg = sb.home().join(".codeium/windsurf/mcp_config.json");
    fs::create_dir_all(cfg.parent().unwrap()).unwrap();
    fs::write(
        &cfg,
        r#"{
  "mcpServers": {
    "mine": { "command": "mine-mcp" },
    "linear": { "url": "https://mcp.linear.app/mcp", "disabled": true, "disabledTools": ["x"] }
  },
  "other": 1
}
"#,
    )
    .unwrap();
    let mut prof = sample();
    prof.mcp_servers.insert(
        "events".into(),
        McpServer {
            url: Some("https://events.example.com/sse".into()),
            transport: Some(Transport::Sse),
            ..Default::default()
        },
    );
    prof.mcp_servers.get_mut("github").unwrap().cwd = Some("tools".into());
    let p = plan(&prof, &["windsurf"], &sb.env(Scope::User));
    let out: serde_json::Value = serde_json::from_str(&planned(&p, &cfg).unwrap()).unwrap();
    assert_eq!(out["other"], 1);
    assert_eq!(out["mcpServers"]["mine"], json!({"command": "mine-mcp"}));
    // The old `url` gives way to serverUrl; keys kitbag doesn't manage stay.
    let linear = &out["mcpServers"]["linear"];
    assert_eq!(linear["serverUrl"], "https://mcp.linear.app/mcp");
    assert!(linear.get("url").is_none());
    assert_eq!(linear["disabled"], true);
    assert_eq!(linear["disabledTools"], json!(["x"]));
    assert!(out["mcpServers"].get("events").is_none());
    assert!(out["mcpServers"]["github"].get("cwd").is_none());
    let said = |what: &str| p.notes.iter().any(|n| n.message.contains(what));
    assert!(said("events skipped") && said("SSE"), "{:?}", p.notes);
    assert!(said("'cwd' is not supported by Windsurf"));
}

#[test]
fn a_disabled_server_is_not_written() {
    let sb = Sandbox::empty();
    let mut prof = sample();
    prof.mcp_servers.get_mut("linear").unwrap().disabled = true;
    let p = plan(&prof, &["windsurf"], &sb.env(Scope::User));
    let cfg: serde_json::Value = serde_json::from_str(
        &planned(&p, &sb.home().join(".codeium/windsurf/mcp_config.json")).unwrap(),
    )
    .unwrap();
    assert!(cfg["mcpServers"].get("linear").is_none());
    assert!(cfg["mcpServers"].get("github").is_some());
}

#[test]
fn instructions_keep_text_around_the_block_and_apply_is_idempotent() {
    let sb = Sandbox::empty();
    let env = sb.env(Scope::User);
    let file = sb.home().join(".codeium/windsurf/memories/global_rules.md");
    fs::create_dir_all(file.parent().unwrap()).unwrap();
    fs::write(&file, "My own notes.\n").unwrap();
    let prof = sample();
    apply(&sb, &prof, &["windsurf"], &env);
    let text = sb.read(GLOBAL_RULES);
    assert!(text.starts_with("My own notes.") && text.contains("Use British English."));
    assert!(plan(&prof, &["windsurf"], &env).changes.is_empty());
    Store::new(sb.state()).undo(None, false).unwrap();
    assert_eq!(sb.read(GLOBAL_RULES), "My own notes.\n");
    assert!(!sb.path(CONFIG).exists());
    assert!(!sb.home().join(".codeium/windsurf/mcp_config.json").exists());
}

#[test]
fn long_instructions_warn_for_global_rules_only() {
    let sb = Sandbox::empty();
    let mut prof = sample();
    prof.instructions = Some("word ".repeat(1500));
    let said = |p: &kitbag::engine::Plan| p.notes.iter().any(|n| n.message.contains("6000"));
    assert!(said(&plan(&prof, &["windsurf"], &sb.env(Scope::User))));
    assert!(!said(&plan(&prof, &["windsurf"], &sb.env(Scope::Project))));
    prof.instructions = Some("short".into());
    assert!(!said(&plan(&prof, &["windsurf"], &sb.env(Scope::User))));
}

/// In a project, AGENTS.md is shared with Codex, Cursor, Antigravity, opencode
/// and Grok Build: one managed block, in any order, in one plan or several.
#[test]
fn windsurf_shares_agents_md_with_the_other_targets() {
    let count = |text: &str| text.matches("<!-- kitbag:begin team -->").count();
    let prof = sample();
    let others = ["codex", "cursor", "antigravity", "opencode", "grok-build"];

    let sb = Sandbox::empty();
    let env = sb.env(Scope::Project);
    fs::write(sb.project().join("AGENTS.md"), "My own notes.\n").unwrap();
    let mut together = others.to_vec();
    together.push("windsurf");
    let p = apply(&sb, &prof, &together, &env);
    let agents = sb.project().join("AGENTS.md");
    assert_eq!(
        p.changes.iter().filter(|c| c.path == agents).count(),
        1,
        "AGENTS.md is planned once"
    );
    let text = sb.read("project/AGENTS.md");
    assert_eq!(count(&text), 1, "{text}");
    assert_eq!(text.matches("Use British English.").count(), 1, "{text}");
    assert!(text.starts_with("My own notes."));
    assert!(plan(&prof, &["windsurf"], &env).changes.is_empty());
    assert!(plan(&prof, &together, &env).changes.is_empty());

    // One after the other, Windsurf first and last.
    for order in [["windsurf", "codex"], ["codex", "windsurf"]] {
        let sb = Sandbox::empty();
        let env = sb.env(Scope::Project);
        apply(&sb, &prof, &[order[0]], &env);
        let earlier = sb.read("project/AGENTS.md");
        let second = apply(&sb, &prof, &[order[1]], &env);
        let text = sb.read("project/AGENTS.md");
        assert_eq!(count(&text), 1, "{order:?}: {text}");
        assert_eq!(text.matches("Use British English.").count(), 1, "{order:?}");
        assert!(plan(&prof, &order, &env).changes.is_empty(), "{order:?}");
        // Undoing the later apply, when it changed anything (Codex adds its
        // rules block, Windsurf adds nothing), leaves the earlier block as it was.
        if !second.changes.is_empty() {
            Store::new(sb.state()).undo(None, false).unwrap();
            assert_eq!(sb.read("project/AGENTS.md"), earlier, "{order:?}");
        }
    }
}

#[test]
fn windsurf_and_codex_agree_when_rules_are_folded() {
    // Codex folds the profile's rules into AGENTS.md next to the instructions
    // block; Windsurf must neither remove nor duplicate that.
    let sb = Sandbox::empty();
    let env = sb.env(Scope::Project);
    let prof = sample();
    apply(&sb, &prof, &["codex"], &env);
    let before = sb.read("project/AGENTS.md");
    assert!(before.contains("Strict mode."));
    assert!(plan(&prof, &["windsurf"], &env).changes.is_empty());
    assert!(plan(&prof, &["codex", "windsurf"], &env).changes.is_empty());
    assert_eq!(sb.read("project/AGENTS.md"), before);
}

#[test]
fn windsurf_to_claude_code_and_back() {
    let src = Sandbox::fixtures();
    let captured = capture(&["windsurf"], &src.env(Scope::User)).profile;
    let dir = src.path("profile");
    profile_io::save(&captured, &dir, false).unwrap();
    let loaded = profile_io::load(&dir).unwrap();
    assert_eq!(loaded, captured, "profile survives save + load unchanged");

    let dst = Sandbox::empty();
    apply(&dst, &loaded, &["claude-code"], &dst.env(Scope::User));
    let back = capture(&["claude-code"], &dst.env(Scope::User)).profile;
    for (name, s) in &loaded.mcp_servers {
        if s.disabled {
            assert!(!back.mcp_servers.contains_key(name), "disabled: {name}");
        } else {
            assert_eq!(&back.mcp_servers[name], s, "server {name}");
        }
    }
    assert_eq!(back.instructions, loaded.instructions);

    // And into Windsurf in a clean home: the same profile comes back.
    let again = Sandbox::empty();
    apply(&again, &loaded, &["windsurf"], &again.env(Scope::User));
    let round = capture(&["windsurf"], &again.env(Scope::User)).profile;
    assert_eq!(round.instructions, loaded.instructions);
    for (name, s) in &loaded.mcp_servers {
        if !s.disabled {
            assert_eq!(&round.mcp_servers[name], s, "server {name}");
        }
    }
    assert!(!round.mcp_servers.contains_key("time"));
}

#[test]
fn claude_code_to_windsurf_user() {
    let src = Sandbox::fixtures();
    let profile = capture(&["claude-code"], &src.env(Scope::User)).profile;
    let dst = Sandbox::empty();
    apply(&dst, &profile, &["windsurf"], &dst.env(Scope::User));
    let back = capture(&["windsurf"], &dst.env(Scope::User)).profile;
    for (name, s) in &profile.mcp_servers {
        if s.disabled || s.transport() == Transport::Sse {
            continue;
        }
        let mut want = s.clone();
        want.cwd = None;
        assert_eq!(&back.mcp_servers[name], &want, "server {name}");
    }
    assert_eq!(back.instructions, profile.instructions);
}

#[test]
fn apply_to_windsurf_then_undo_through_the_cli() {
    let sb = Sandbox::empty();
    sb.cli().args(["init", "prof"]).assert().success();
    fs::write(
        sb.path("prof/kitbag.toml"),
        "kitbag = 1\nname = \"prof\"\n\n[mcp_servers.docs]\nurl = \"https://docs.example.com/mcp\"\nheaders = { Authorization = \"Bearer ${DOCS_TOKEN}\" }\n",
    )
    .unwrap();
    sb.cli()
        .args(["apply", "prof", "--to", "windsurf", "--yes"])
        .env("DOCS_TOKEN", "fake-token-value")
        .assert()
        .success();
    let cfg = sb.json(CONFIG);
    assert_eq!(
        cfg["mcpServers"]["docs"],
        json!({"serverUrl": "https://docs.example.com/mcp", "headers": {"Authorization": "Bearer ${env:DOCS_TOKEN}"}})
    );
    assert!(!sb.read(CONFIG).contains("fake-token-value"));
    assert!(
        sb.read(GLOBAL_RULES)
            .contains("Write instructions every agent should follow")
    );
    sb.cli().arg("undo").assert().success();
    assert!(
        !sb.home().join(".codeium").exists(),
        "undo removes what apply created"
    );
}
