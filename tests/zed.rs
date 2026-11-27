//! Zed: MCP servers in `context_servers` of `settings.json` and instructions in
//! `AGENTS.md` (user) or the first of Zed's project instruction files.

mod common;

use std::fs;

use common::*;
use kitbag::adapters::{Os, Scope, SecretsMode};
use kitbag::model::{McpServer, Profile, Rule, Transport};
use kitbag::profile_io;
use kitbag::txn::Store;
use serde_json::json;

const SETTINGS: &str = "home/.config/zed/settings.json";
const PERSONAL: &str = "home/.config/zed/AGENTS.md";

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
    p
}

fn said(p: &kitbag::engine::Plan, what: &str) -> bool {
    p.notes
        .iter()
        .any(|n| n.target == "zed" && n.message.contains(what))
}

#[test]
fn capture_user_reads_jsonc_settings_and_personal_agents_md() {
    let sb = Sandbox::fixtures();
    let r = capture(&["zed"], &sb.env(Scope::User));
    let p = &r.profile;
    let gh = &p.mcp_servers["github"];
    assert_eq!(gh.command.as_deref(), Some("npx"));
    assert_eq!(
        gh.env["GITHUB_PERSONAL_ACCESS_TOKEN"], "${GITHUB_PERSONAL_ACCESS_TOKEN}",
        "a literal key is redacted"
    );
    let docs = &p.mcp_servers["docs"];
    assert_eq!(
        docs.url.as_deref(),
        Some("https://mcp.docs.example.com/mcp")
    );
    assert_eq!(docs.transport(), Transport::Http);
    assert_eq!(docs.headers["Authorization"], "Bearer ${DOCS_TOKEN}");
    assert!(p.mcp_servers["time"].disabled, "enabled: false");
    // Extension servers and the older object layout are left out, with a say.
    assert!(!p.mcp_servers.contains_key("postgres-context-server"));
    assert!(!p.mcp_servers.contains_key("old-style"));
    let say = |what: &str| r.notes.iter().any(|n| n.message.contains(what));
    assert!(say("postgres-context-server") && say("Zed extension"));
    assert!(say("old-style") && say("older Zed layout"));
    assert_eq!(
        p.instructions.as_deref(),
        Some("Answer in plain English.\n\nKeep diffs small.")
    );
    assert!(!r.redactions.is_empty());
}

#[test]
fn capture_project_reads_zed_settings_and_the_first_instructions_file() {
    let sb = Sandbox::fixtures();
    let r = capture(&["zed"], &sb.env(Scope::Project));
    assert_eq!(
        r.profile.mcp_servers["project-db"].command.as_deref(),
        Some("node")
    );
    // The fixture project has a .windsurfrules and a Copilot instructions
    // file, which Zed reads before AGENTS.md (the first one wins).
    let i = r.profile.instructions.as_deref().unwrap();
    assert_eq!(i, "Never commit generated files.");
    assert!(!i.contains("# Agents"), "AGENTS.md is not read: {i}");
    assert!(
        r.notes
            .iter()
            .any(|n| n.message.contains("AGENTS.md not read"))
    );

    // Without them, AGENTS.md is the file.
    fs::remove_file(sb.project().join(".windsurfrules")).unwrap();
    fs::remove_file(sb.project().join(".github/copilot-instructions.md")).unwrap();
    let r = capture(&["zed"], &sb.env(Scope::Project));
    assert!(
        r.profile
            .instructions
            .as_deref()
            .unwrap()
            .contains("Run `pnpm test` before committing.")
    );
}

#[test]
fn capture_project_reads_only_the_file_zed_reads() {
    let sb = Sandbox::fixtures();
    fs::write(sb.project().join(".rules"), "Zed-only rules.\n").unwrap();
    let r = capture(&["zed"], &sb.env(Scope::Project));
    assert_eq!(r.profile.instructions.as_deref(), Some("Zed-only rules."));
    assert!(
        r.notes
            .iter()
            .any(|n| n.message.contains("AGENTS.md not read") && n.message.contains(".rules"))
    );
}

#[test]
fn capture_leaves_out_rules_folded_by_other_tools() {
    let sb = Sandbox::empty();
    apply(&sb, &sample(), &["codex"], &sb.env(Scope::Project));
    let r = capture(&["zed"], &sb.env(Scope::Project));
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
fn detect_looks_for_zed_not_shared_files() {
    let sb = Sandbox::empty();
    let a = kitbag::adapters::get("zed").unwrap();
    for scope in [Scope::User, Scope::Project] {
        assert!(!a.detect(&sb.env(scope)));
    }
    fs::write(sb.project().join("AGENTS.md"), "x\n").unwrap();
    assert!(!a.detect(&sb.env(Scope::Project)), "AGENTS.md is shared");
    fs::create_dir_all(sb.home().join(".config/zed")).unwrap();
    assert!(a.detect(&sb.env(Scope::User)));
    fs::create_dir_all(sb.project().join(".zed")).unwrap();
    assert!(a.detect(&sb.env(Scope::Project)));
}

#[test]
fn config_dir_follows_the_os() {
    let sb = Sandbox::empty();
    let at = |os: Os, vars: &[(&str, &str)]| {
        let mut env = sb.env(Scope::User);
        env.os = os;
        for (k, v) in vars {
            env.locations.insert(k.to_string(), v.to_string());
        }
        kitbag::adapters::zed::config_dir(&env)
    };
    let home = sb.home();
    assert_eq!(at(Os::Linux, &[]), home.join(".config").join("zed"));
    assert_eq!(
        at(Os::Linux, &[("XDG_CONFIG_HOME", "/x/cfg")]),
        std::path::PathBuf::from("/x/cfg").join("zed")
    );
    // macOS uses ~/.config/zed whatever XDG_CONFIG_HOME says.
    assert_eq!(
        at(Os::MacOs, &[("XDG_CONFIG_HOME", "/x/cfg")]),
        home.join(".config").join("zed")
    );
    assert_eq!(
        at(Os::Windows, &[("APPDATA", "C:/Users/me/AppData/Roaming")]),
        std::path::PathBuf::from("C:/Users/me/AppData/Roaming").join("Zed")
    );
    assert_eq!(
        at(Os::Windows, &[]),
        home.join("AppData").join("Roaming").join("Zed")
    );
}

#[test]
fn apply_user_resolves_secrets_and_writes_them_owner_only() {
    let sb = Sandbox::empty();
    let env = sb.env_with(Scope::User, &[("GITHUB_TOKEN", "ghp_FAKEvalue123")]);
    let p = plan(&sample(), &["zed"], &env);
    let cfg: serde_json::Value =
        serde_json::from_str(&planned(&p, &sb.home().join(".config/zed/settings.json")).unwrap())
            .unwrap();
    // Zed documents no reference syntax: the value is resolved, and a missing
    // variable stays a placeholder.
    assert_eq!(
        cfg["context_servers"]["github"],
        json!({"command": "npx", "args": ["-y", "@modelcontextprotocol/server-github"], "env": {"GITHUB_TOKEN": "ghp_FAKEvalue123"}})
    );
    assert_eq!(
        cfg["context_servers"]["linear"],
        json!({"url": "https://mcp.linear.app/mcp", "headers": {"Authorization": "Bearer ${LINEAR_API_KEY}"}})
    );
    assert!(p.sensitive.contains("ghp_FAKEvalue123"));
    assert!(said(&p, "resolved from your environment"));
    assert!(said(&p, "unset env vars"));
    let md = planned(&p, &sb.home().join(".config/zed/AGENTS.md")).unwrap();
    assert!(md.contains("<!-- kitbag:begin team -->") && md.contains("Use British English."));
    assert!(!md.contains("kitbag:rule"), "rules are not supported");
    assert!(
        p.notes
            .iter()
            .any(|n| n.target == "zed" && n.message.contains("not supported"))
    );
    assert_eq!(p.changes.len(), 2, "{:?}", p.changes);

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        apply(&sb, &sample(), &["zed"], &env);
        let mode = |rel: &str| fs::metadata(sb.path(rel)).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode(SETTINGS), 0o600);
        assert_ne!(mode(PERSONAL), 0o600);
    }
}

#[test]
fn secrets_never_keeps_placeholders() {
    let sb = Sandbox::empty();
    let env = sb.env_with(Scope::User, &[("GITHUB_TOKEN", "ghp_FAKEvalue123")]);
    let prof = sample();
    let mut o = opts(&prof);
    o.secrets = SecretsMode::Never;
    let p = kitbag::engine::plan(&prof, &targets(&["zed"]), &env, &o).unwrap();
    let cfg = planned(&p, &sb.home().join(".config/zed/settings.json")).unwrap();
    assert!(cfg.contains("${GITHUB_TOKEN}") && !cfg.contains("ghp_FAKE"));
    assert!(p.sensitive.is_empty());
    assert!(said(&p, "placeholders"));
}

#[test]
fn apply_project_writes_zed_settings_and_warns_about_secrets() {
    let sb = Sandbox::empty();
    let env = sb.env_with(Scope::Project, &[("GITHUB_TOKEN", "ghp_FAKEvalue123")]);
    let p = plan(&sample(), &["zed"], &env);
    let cfg = planned(&p, &sb.project().join(".zed/settings.json")).unwrap();
    assert!(cfg.contains("\"context_servers\"") && cfg.contains("ghp_FAKEvalue123"));
    assert!(planned(&p, &sb.project().join("AGENTS.md")).is_some());
    assert!(said(&p, "usually committed"));
    assert!(said(&p, "trusted worktree"));
    // Nothing outside the project.
    assert!(p.changes.iter().all(|c| c.path.starts_with(sb.project())));
}

#[test]
fn apply_merges_into_jsonc_settings_and_keeps_the_rest() {
    let sb = Sandbox::empty();
    let cfg = sb.home().join(".config/zed/settings.json");
    fs::create_dir_all(cfg.parent().unwrap()).unwrap();
    fs::write(
        &cfg,
        r#"// my settings
{
  "theme": "One Dark", // keep
  "context_servers": {
    "mine": { "command": "mine-mcp", },
    "linear": { "url": "https://mcp.linear.app/mcp", "timeout": 30, "enabled": false },
  },
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
    let p = plan(&prof, &["zed"], &sb.env(Scope::User));
    let text = planned(&p, &cfg).unwrap();
    assert!(text.starts_with("// my settings") && text.contains("// keep"));
    let out = kitbag::edit::json::parse_value(Some(&text), &cfg).unwrap();
    assert_eq!(out["theme"], "One Dark");
    assert_eq!(
        out["context_servers"]["mine"],
        json!({"command": "mine-mcp"})
    );
    // Keys kitbag doesn't manage stay; `enabled` is the profile's to decide.
    let linear = &out["context_servers"]["linear"];
    assert_eq!(linear["timeout"], 30);
    assert!(linear.get("enabled").is_none());
    assert!(out["context_servers"].get("events").is_none());
    assert!(out["context_servers"]["github"].get("cwd").is_none());
    assert!(
        said(&p, "events skipped") && said(&p, "SSE"),
        "{:?}",
        p.notes
    );
    assert!(said(&p, "'cwd' is not supported by Zed"));
}

#[test]
fn a_disabled_server_is_written_with_enabled_false() {
    let sb = Sandbox::empty();
    let mut prof = sample();
    prof.mcp_servers.get_mut("linear").unwrap().disabled = true;
    let p = plan(&prof, &["zed"], &sb.env(Scope::User));
    let cfg: serde_json::Value =
        serde_json::from_str(&planned(&p, &sb.home().join(".config/zed/settings.json")).unwrap())
            .unwrap();
    assert_eq!(cfg["context_servers"]["linear"]["enabled"], false);
    assert!(cfg["context_servers"]["github"].get("enabled").is_none());
}

#[test]
fn personal_instructions_keep_text_around_the_block_and_apply_is_idempotent() {
    let sb = Sandbox::empty();
    let env = sb.env(Scope::User);
    let file = sb.home().join(".config/zed/AGENTS.md");
    fs::create_dir_all(file.parent().unwrap()).unwrap();
    fs::write(&file, "My own notes.\n").unwrap();
    let prof = sample();
    apply(&sb, &prof, &["zed"], &env);
    let text = sb.read(PERSONAL);
    assert!(text.starts_with("My own notes.") && text.contains("Use British English."));
    assert!(plan(&prof, &["zed"], &env).changes.is_empty());
    Store::new(sb.state()).undo(None, false).unwrap();
    assert_eq!(sb.read(PERSONAL), "My own notes.\n");
    assert!(!sb.path(SETTINGS).exists());
}

/// Zed reads the first of `.rules`, `.cursorrules` ... `AGENTS.md`, so kitbag
/// writes into the first one that exists.
#[test]
fn project_instructions_go_to_the_file_zed_reads() {
    let prof = sample();
    // Nothing yet: AGENTS.md.
    let sb = Sandbox::empty();
    let p = plan(&prof, &["zed"], &sb.env(Scope::Project));
    assert!(planned(&p, &sb.project().join("AGENTS.md")).is_some());
    assert!(!said(&p, "rather than AGENTS.md"));

    for first in [".rules", ".cursorrules", "AGENT.md"] {
        let sb = Sandbox::empty();
        let env = sb.env(Scope::Project);
        fs::write(sb.project().join(first), "Mine.\n").unwrap();
        fs::write(sb.project().join("AGENTS.md"), "Shared.\n").unwrap();
        let p = apply(&sb, &prof, &["zed"], &env);
        let text = sb.read(&format!("project/{first}"));
        assert!(text.starts_with("Mine.") && text.contains("Use British English."));
        assert_eq!(sb.read("project/AGENTS.md"), "Shared.\n", "{first}");
        assert!(said(&p, &format!("go to {first} rather than AGENTS.md")));
        assert!(plan(&prof, &["zed"], &env).changes.is_empty());
        let back = capture(&["zed"], &env).profile;
        assert_eq!(
            back.instructions.as_deref().map(str::trim),
            Some("Mine.\n\nUse British English.")
        );
    }

    // .github/copilot-instructions.md comes before AGENTS.md too.
    let sb = Sandbox::empty();
    fs::create_dir_all(sb.project().join(".github")).unwrap();
    fs::write(sb.project().join(".github/copilot-instructions.md"), "C.\n").unwrap();
    apply(&sb, &prof, &["zed"], &sb.env(Scope::Project));
    assert!(
        sb.read("project/.github/copilot-instructions.md")
            .contains("Use British English.")
    );
    assert!(!sb.project().join("AGENTS.md").exists());
}

/// With Copilot a target, `.github/copilot-instructions.md` will exist, and
/// Zed reads that before AGENTS.md: both targets share that one file and its block.
#[test]
fn zed_and_copilot_share_the_copilot_instructions_file() {
    let prof = sample();
    let sb = Sandbox::empty();
    let env = sb.env(Scope::Project);
    for order in [["zed", "copilot"], ["copilot", "zed"]] {
        let p = apply(&sb, &prof, &order, &env);
        let file = sb.project().join(".github/copilot-instructions.md");
        assert!(
            p.changes.iter().filter(|c| c.path == file).count() <= 1,
            "{order:?}"
        );
        let text = sb.read("project/.github/copilot-instructions.md");
        assert_eq!(
            text.matches("<!-- kitbag:begin team -->").count(),
            1,
            "{text}"
        );
        assert!(plan(&prof, &order, &env).changes.is_empty(), "{order:?}");
    }
}

/// In a project with no earlier file, AGENTS.md is shared with Codex, Cursor,
/// Antigravity, opencode, Grok Build and Windsurf: one managed block, in any
/// order, in one plan or several.
#[test]
fn zed_shares_agents_md_with_the_other_targets() {
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
    together.push("zed");
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
    assert!(plan(&prof, &["zed"], &env).changes.is_empty());
    assert!(plan(&prof, &together, &env).changes.is_empty());

    for order in [["zed", "codex"], ["codex", "zed"]] {
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
fn zed_to_claude_code_and_back() {
    let src = Sandbox::fixtures();
    let captured = capture(&["zed"], &src.env(Scope::User)).profile;
    let dir = src.path("profile");
    profile_io::save(&captured, &dir, false).unwrap();
    let loaded = profile_io::load(&dir).unwrap();
    assert_eq!(loaded, captured, "profile survives save + load unchanged");

    let dst = Sandbox::empty();
    apply(&dst, &loaded, &["claude-code"], &dst.env(Scope::User));
    let back = capture(&["claude-code"], &dst.env(Scope::User)).profile;
    for (name, s) in &loaded.mcp_servers {
        if !s.disabled {
            assert_eq!(&back.mcp_servers[name], s, "server {name}");
        }
    }
    assert_eq!(back.instructions, loaded.instructions);

    // And into Zed in a clean home: the same profile comes back, disabled included.
    let again = Sandbox::empty();
    apply(&again, &loaded, &["zed"], &again.env(Scope::User));
    let round = capture(&["zed"], &again.env(Scope::User)).profile;
    assert_eq!(round.instructions, loaded.instructions);
    assert_eq!(round.mcp_servers, loaded.mcp_servers);
}

#[test]
fn claude_code_to_zed_user() {
    let src = Sandbox::fixtures();
    let profile = capture(&["claude-code"], &src.env(Scope::User)).profile;
    let dst = Sandbox::empty();
    // ${VAR} references are resolved for Zed, so keep them with --secrets never.
    let mut o = opts(&profile);
    o.secrets = SecretsMode::Never;
    let p = kitbag::engine::plan(&profile, &targets(&["zed"]), &dst.env(Scope::User), &o).unwrap();
    kitbag::engine::commit(&p, "test", &dst.state()).unwrap();
    let back = capture(&["zed"], &dst.env(Scope::User)).profile;
    for (name, s) in &profile.mcp_servers {
        if s.transport() == Transport::Sse {
            continue;
        }
        let mut want = s.clone();
        want.cwd = None;
        assert_eq!(&back.mcp_servers[name], &want, "server {name}");
    }
    assert_eq!(back.instructions, profile.instructions);
}

#[test]
fn apply_to_zed_then_undo_through_the_cli() {
    let sb = Sandbox::empty();
    sb.cli().args(["init", "prof"]).assert().success();
    fs::write(
        sb.path("prof/kitbag.toml"),
        "kitbag = 1\nname = \"prof\"\n\n[mcp_servers.docs]\nurl = \"https://docs.example.com/mcp\"\nheaders = { Authorization = \"Bearer ${DOCS_TOKEN}\" }\n",
    )
    .unwrap();
    sb.cli()
        .args(["apply", "prof", "--to", "zed", "--yes"])
        .env("DOCS_TOKEN", "fake-token-value")
        .assert()
        .success();
    let cfg = sb.json(SETTINGS);
    assert_eq!(
        cfg["context_servers"]["docs"],
        json!({"url": "https://docs.example.com/mcp", "headers": {"Authorization": "Bearer fake-token-value"}})
    );
    assert!(
        sb.read(PERSONAL)
            .contains("Write instructions every agent should follow")
    );
    sb.cli().arg("undo").assert().success();
    assert!(
        !sb.home().join(".config/zed").exists(),
        "undo removes what apply created"
    );
}
