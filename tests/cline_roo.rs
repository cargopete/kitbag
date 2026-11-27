//! Cline and Roo Code: MCP servers, instructions, rules, commands (Cline's
//! workflows) and skills.
//!
//! Cline: `cline_mcp_settings.json` in `~/.cline/data/settings/`, an
//! instructions file in `~/.cline/rules/` (user), `AGENTS.md` (project), and
//! rules, workflows and skills in `~/.cline/` and `.cline/` (and the older
//! layouts Cline also reads).
//! Roo Code: `mcp_settings.json` in the editor's global storage and
//! `~/.roo/rules/` (user), `.roo/mcp.json` and `AGENTS.md` (project), with
//! rules, commands and skills in `~/.roo/` and `.roo/`.

mod common;

use std::fs;
use std::path::PathBuf;

use common::*;
use kitbag::adapters::{Os, Scope, SecretsMode};
use kitbag::model::{Command, McpServer, Profile, Rule, RuleMode, Skill, SkillFile, Transport};
use kitbag::txn::Store;
use serde_json::{Value, json};

const CLINE_MCP: &str = "home/.cline/data/settings/cline_mcp_settings.json";
const CLINE_RULES: &str = "home/.cline/rules/kitbag.md";
const ROO_STORE: &str = "home/.config/Code/User/globalStorage/rooveterinaryinc.roo-cline/settings";
const ROO_MCP: &str =
    "home/.config/Code/User/globalStorage/rooveterinaryinc.roo-cline/settings/mcp_settings.json";
const ROO_RULES: &str = "home/.roo/rules/kitbag.md";

/// A sandbox with the Cline and Roo Code fixtures (not part of the shared
/// fixtures, so the other tools' tests are not affected by them).
fn fixture_sandbox() -> Sandbox {
    let sb = Sandbox::empty();
    let root = fixtures().join("cline_roo");
    copy_dir(&root.join("home"), &sb.home());
    copy_dir(&root.join("project"), &sb.project());
    sb
}

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
    p.mcp_servers.insert(
        "events".into(),
        McpServer {
            url: Some("https://events.example.com/sse".into()),
            transport: Some(Transport::Sse),
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

fn said(p: &kitbag::engine::Plan, target: &str, what: &str) -> bool {
    p.notes
        .iter()
        .any(|n| n.target == target && n.message.contains(what))
}

fn cap_said(r: &kitbag::engine::CaptureResult, what: &str) -> bool {
    r.notes.iter().any(|n| n.message.contains(what))
}

fn planned_json(p: &kitbag::engine::Plan, path: &std::path::Path) -> Value {
    serde_json::from_str(&planned(p, path).unwrap()).unwrap()
}

fn count_blocks(text: &str) -> usize {
    text.matches("<!-- kitbag:begin team -->").count()
}

// ---------------------------------------------------------------- Cline

#[test]
fn cline_capture_reads_mcp_settings_and_the_rules_file() {
    let sb = fixture_sandbox();
    let r = capture(&["cline"], &sb.env(Scope::User));
    let p = &r.profile;

    let gh = &p.mcp_servers["github"];
    assert_eq!(gh.command.as_deref(), Some("npx"));
    assert_eq!(
        gh.env["GITHUB_PERSONAL_ACCESS_TOKEN"], "${GITHUB_PERSONAL_ACCESS_TOKEN}",
        "a literal key is redacted"
    );
    let docs = &p.mcp_servers["docs"];
    assert_eq!(docs.transport(), Transport::Http, "type: streamableHttp");
    assert_eq!(docs.headers["Authorization"], "Bearer ${DOCS_TOKEN}");
    assert_eq!(p.mcp_servers["events"].transport(), Transport::Sse);
    // Cline's docs: a URL with no `type` is SSE.
    assert_eq!(p.mcp_servers["legacy-remote"].transport(), Transport::Sse);
    assert!(p.mcp_servers["time"].disabled);
    assert!(!p.mcp_servers.contains_key("odd"));
    assert!(cap_said(&r, "unknown type 'websocket'"));
    assert!(!r.redactions.is_empty());

    assert_eq!(
        p.instructions.as_deref(),
        Some("Always answer in plain English.")
    );
    // The rule files are rules; the instructions file is not one.
    let names: Vec<&str> = p.rules.iter().map(|r| r.name.as_str()).collect();
    assert_eq!(names, ["style", "typescript"], "{:?}", r.notes);
    assert_eq!(p.rules[0].body, "Prefer small diffs.");
    assert_eq!(p.rules[0].mode(), RuleMode::Always);
    assert_eq!(p.rules[1].globs, ["**/*.ts"], "paths frontmatter");
    assert_eq!(p.rules[1].body, "Use strict mode.");
    assert!(!cap_said(&r, "were not captured"), "{:?}", r.notes);
}

#[test]
fn cline_never_reads_the_provider_settings_next_to_the_mcp_file() {
    let sb = fixture_sandbox();
    let r = capture(&["cline"], &sb.env(Scope::User));
    let dir = sb.path("profile");
    kitbag::profile_io::save(&r.profile, &dir, false).unwrap();
    let text = all_text_under(&dir);
    assert!(!text.contains("FAKEprovidersKey"), "{text}");
    assert!(r.sources.iter().all(|s| !s.ends_with("providers.json")));
}

#[test]
fn cline_capture_project_reads_agents_md_and_names_clinerules_files() {
    let sb = fixture_sandbox();
    let r = capture(&["cline"], &sb.env(Scope::Project));
    assert!(r.profile.mcp_servers.is_empty(), "no project MCP file");
    assert!(
        r.profile
            .instructions
            .as_deref()
            .unwrap()
            .contains("Run `pnpm test` before committing.")
    );
    let names: Vec<&str> = r.profile.rules.iter().map(|r| r.name.as_str()).collect();
    assert_eq!(names, ["lint", "tabs"], "{:?}", r.notes);
    assert_eq!(r.profile.rules[0].globs, ["src/**/*.rs"]);
    assert_eq!(r.profile.rules[1].body, "Use tabs.");
    // What .clinerules/ holds besides rule files: workflows and skills are read,
    // hooks are left behind (issue #13) and said so.
    assert!(cap_said(&r, "pre-commit.sh"), "{:?}", r.notes);
    assert!(!cap_said(&r, "triage.md"), "{:?}", r.notes);
}

#[test]
fn cline_apply_writes_documented_shapes() {
    let sb = Sandbox::empty();
    let env = sb.env_with(Scope::User, &[("GITHUB_TOKEN", "ghp_FAKEvalue123")]);
    let mut prof = sample();
    prof.mcp_servers.get_mut("github").unwrap().cwd = Some("tools".into());
    prof.mcp_servers.insert(
        "off".into(),
        McpServer {
            command: Some("off-mcp".into()),
            disabled: true,
            ..Default::default()
        },
    );
    let p = plan(&prof, &["cline"], &env);
    let cfg = planned_json(&p, &sb.path(CLINE_MCP));
    // No env syntax in Cline's docs: resolved, with a missing variable left as a placeholder.
    assert_eq!(
        cfg["mcpServers"]["github"],
        json!({"command": "npx", "args": ["-y", "@modelcontextprotocol/server-github"], "env": {"GITHUB_TOKEN": "ghp_FAKEvalue123"}})
    );
    assert_eq!(
        cfg["mcpServers"]["linear"],
        json!({"url": "https://mcp.linear.app/mcp", "type": "streamableHttp", "headers": {"Authorization": "Bearer ${LINEAR_API_KEY}"}})
    );
    assert_eq!(
        cfg["mcpServers"]["events"],
        json!({"url": "https://events.example.com/sse", "type": "sse"})
    );
    assert_eq!(cfg["mcpServers"]["off"]["disabled"], true);
    assert!(p.sensitive.contains("ghp_FAKEvalue123"));
    assert!(said(&p, "cline", "resolved from your environment"));
    assert!(said(&p, "cline", "unset env vars"));
    assert!(said(&p, "cline", "'cwd' is not documented by Cline"));

    let md = planned(&p, &sb.path(CLINE_RULES)).unwrap();
    assert!(md.contains("<!-- kitbag:begin team -->") && md.contains("Use British English."));
    assert!(
        !md.contains("Strict mode."),
        "rules are files, not folded: {md}"
    );
    assert_eq!(
        planned(&p, &sb.path("home/.cline/rules/ts.md")).as_deref(),
        Some("Strict mode.\n")
    );
    assert_eq!(p.changes.len(), 3, "{:?}", p.changes);

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        apply(&sb, &prof, &["cline"], &env);
        let mode = |rel: &str| fs::metadata(sb.path(rel)).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode(CLINE_MCP), 0o600, "resolved secrets: owner only");
        assert_ne!(mode(CLINE_RULES), 0o600);
    }
}

#[test]
fn cline_secrets_never_keeps_placeholders() {
    let sb = Sandbox::empty();
    let env = sb.env_with(Scope::User, &[("GITHUB_TOKEN", "ghp_FAKEvalue123")]);
    let prof = sample();
    let mut o = opts(&prof);
    o.secrets = SecretsMode::Never;
    let p = kitbag::engine::plan(&prof, &targets(&["cline"]), &env, &o).unwrap();
    let cfg = planned(&p, &sb.path(CLINE_MCP)).unwrap();
    assert!(cfg.contains("${GITHUB_TOKEN}") && !cfg.contains("ghp_FAKE"));
    assert!(p.sensitive.is_empty());
    assert!(said(&p, "cline", "placeholders"));
}

#[test]
fn cline_merge_keeps_the_users_servers_comments_and_own_keys() {
    let sb = fixture_sandbox();
    let cfg = sb.path(CLINE_MCP);
    // Keep the fixture's github entry (with autoApprove and timeout) and add comments.
    let text = fs::read_to_string(&cfg).unwrap();
    fs::write(&cfg, format!("// my cline servers\n{text}")).unwrap();
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
        "new".into(),
        McpServer {
            command: Some("new-mcp".into()),
            ..Default::default()
        },
    );
    let env = sb.env_with(
        Scope::User,
        &[("GITHUB_PERSONAL_ACCESS_TOKEN", "ghp_FAKEvalue123")],
    );
    let p = plan(&prof, &["cline"], &env);
    let out_text = planned(&p, &cfg).unwrap();
    assert!(out_text.starts_with("// my cline servers"));
    let out = kitbag::edit::json::parse_value(Some(&out_text), &cfg).unwrap();
    // The user's other servers stay; so do Cline's own keys on a server kitbag updates.
    for keep in ["docs", "events", "legacy-remote", "time", "odd"] {
        assert!(out["mcpServers"].get(keep).is_some(), "{keep}");
    }
    assert_eq!(
        out["mcpServers"]["github"]["autoApprove"][0],
        "search_repositories"
    );
    assert_eq!(out["mcpServers"]["github"]["timeout"], 120);
    assert_eq!(out["mcpServers"]["new"], json!({"command": "new-mcp"}));
}

#[test]
fn cline_a_server_that_changes_what_it_runs_loses_the_old_auto_approve() {
    let sb = fixture_sandbox();
    let mut prof = Profile::new("team");
    prof.mcp_servers.insert(
        "github".into(),
        McpServer {
            command: Some("npx".into()),
            args: vec!["-y".into(), "other-package".into()],
            ..Default::default()
        },
    );
    let p = plan(&prof, &["cline"], &sb.env(Scope::User));
    let out = planned_json(&p, &sb.path(CLINE_MCP));
    assert!(out["mcpServers"]["github"].get("autoApprove").is_none());
    assert!(said(&p, "cline", "autoApprove"));
}

#[test]
fn cline_locations_follow_the_documented_variables() {
    let sb = Sandbox::empty();
    let a = kitbag::adapters::get("cline").unwrap();
    let mcp = |vars: &[(&str, &str)]| {
        let mut env = sb.env(Scope::User);
        for (k, v) in vars {
            env.locations.insert(k.to_string(), v.to_string());
        }
        a.locations(&env)
            .into_iter()
            .find(|(k, _)| *k == "mcp")
            .unwrap()
            .1
    };
    let file = PathBuf::from("settings").join("cline_mcp_settings.json");
    assert_eq!(mcp(&[]), sb.home().join(".cline").join("data").join(&file));
    assert_eq!(
        mcp(&[("CLINE_DIR", "/x/cline")]),
        PathBuf::from("/x/cline").join("data").join(&file)
    );
    assert_eq!(
        mcp(&[("CLINE_DATA_DIR", "/x/data")]),
        PathBuf::from("/x/data").join(&file)
    );
    assert_eq!(
        mcp(&[("CLINE_MCP_SETTINGS_PATH", "/x/mcp.json")]),
        PathBuf::from("/x/mcp.json")
    );
    // Same on every OS: the layout is under the home directory.
    let mut env = sb.env(Scope::User);
    for os in [Os::MacOs, Os::Windows] {
        env.os = os;
        assert_eq!(
            a.locations(&env)[0].1,
            sb.home().join(".cline").join("data").join(&file)
        );
    }
}

#[test]
fn cline_honours_cline_dir_for_both_files() {
    let sb = Sandbox::empty();
    let mut env = sb.env(Scope::User);
    env.locations.insert(
        "CLINE_DIR".into(),
        sb.path("elsewhere").display().to_string(),
    );
    let p = apply(&sb, &sample(), &["cline"], &env);
    assert!(
        sb.path("elsewhere/data/settings/cline_mcp_settings.json")
            .is_file()
    );
    assert!(sb.path("elsewhere/rules/kitbag.md").is_file());
    assert!(!sb.path("home/.cline").exists());
    assert!(!p.changes.is_empty());
}

#[test]
fn cline_project_scope_has_no_mcp_file_and_uses_agents_md() {
    let sb = Sandbox::empty();
    let env = sb.env(Scope::Project);
    let p = plan(&sample(), &["cline"], &env);
    assert!(said(&p, "cline", "no project-level MCP config"));
    assert_eq!(p.changes.len(), 2, "AGENTS.md and the rule file");
    let md = planned(&p, &sb.project().join("AGENTS.md")).unwrap();
    assert!(md.contains("Use British English.") && !md.contains("Strict mode."));
    assert_eq!(
        planned(&p, &sb.project().join(".cline/rules/ts.md")).as_deref(),
        Some("Strict mode.\n")
    );
    assert_eq!(p.changes.len(), 2);
    assert!(p.changes.iter().all(|c| c.path.starts_with(sb.project())));
}

#[test]
fn cline_apply_is_idempotent_and_undo_restores() {
    let sb = Sandbox::empty();
    let env = sb.env(Scope::User);
    let rules = sb.path(CLINE_RULES);
    fs::create_dir_all(rules.parent().unwrap()).unwrap();
    fs::write(&rules, "My own notes.\n").unwrap();
    let prof = sample();
    apply(&sb, &prof, &["cline"], &env);
    let text = sb.read(CLINE_RULES);
    assert!(text.starts_with("My own notes.") && text.contains("Use British English."));
    assert!(plan(&prof, &["cline"], &env).changes.is_empty());
    let back = capture(&["cline"], &env).profile;
    assert_eq!(back.rules.len(), 1, "folded rules come back");
    assert_eq!(back.mcp_servers["linear"].transport(), Transport::Http);
    assert_eq!(back.mcp_servers["events"].transport(), Transport::Sse);
    assert_eq!(
        back.instructions.as_deref().map(str::trim),
        Some("My own notes.\n\nUse British English.")
    );
    Store::new(sb.state()).undo(None, false).unwrap();
    assert_eq!(sb.read(CLINE_RULES), "My own notes.\n");
    assert!(!sb.path(CLINE_MCP).exists());
}

#[test]
fn cline_detect() {
    let sb = Sandbox::empty();
    let a = kitbag::adapters::get("cline").unwrap();
    for scope in [Scope::User, Scope::Project] {
        assert!(!a.detect(&sb.env(scope)));
    }
    fs::write(sb.project().join("AGENTS.md"), "x\n").unwrap();
    assert!(!a.detect(&sb.env(Scope::Project)), "AGENTS.md is shared");
    fs::create_dir_all(sb.home().join(".cline")).unwrap();
    assert!(a.detect(&sb.env(Scope::User)));
    fs::create_dir_all(sb.project().join(".clinerules")).unwrap();
    assert!(a.detect(&sb.env(Scope::Project)));
}

// ------------------------------------------------------------------ Roo

#[test]
fn roo_capture_user_reads_global_storage_and_rules() {
    let sb = fixture_sandbox();
    let r = capture(&["roo"], &sb.env(Scope::User));
    let p = &r.profile;
    let gh = &p.mcp_servers["github"];
    assert_eq!(
        gh.args[2], "--token=${GITHUB_TOKEN}",
        "${{env:X}} is read as ${{X}}"
    );
    assert_eq!(gh.cwd.as_deref(), Some("/work/repo"));
    assert_eq!(
        gh.env["GITHUB_PERSONAL_ACCESS_TOKEN"], "${GITHUB_PERSONAL_ACCESS_TOKEN}",
        "a literal key is redacted"
    );
    let docs = &p.mcp_servers["docs"];
    assert_eq!(docs.transport(), Transport::Http, "type: streamable-http");
    assert_eq!(docs.headers["Authorization"], "Bearer ${DOCS_TOKEN}");
    assert_eq!(p.mcp_servers["events"].transport(), Transport::Sse);
    assert!(p.mcp_servers["time"].disabled);
    // Roo rejects a URL with no type.
    assert!(!p.mcp_servers.contains_key("typeless"));
    assert!(cap_said(&r, "typeless") && cap_said(&r, "no type"));
    assert!(!r.redactions.is_empty());
    assert_eq!(
        p.instructions.as_deref(),
        Some("Always answer in plain English.")
    );
    let names: Vec<&str> = p.rules.iter().map(|r| r.name.as_str()).collect();
    assert_eq!(names, ["10-style"], "{:?}", r.notes);
    // Mode-specific rules are listed, not read.
    assert!(cap_said(&r, "plan.md"), "{:?}", r.notes);
    assert!(cap_said(&r, "mode-specific rules"), "{:?}", r.notes);
}

#[test]
fn roo_capture_project_reads_mcp_json_agents_md_and_names_rule_files() {
    let sb = fixture_sandbox();
    let r = capture(&["roo"], &sb.env(Scope::Project));
    let db = &r.profile.mcp_servers["project-db"];
    assert_eq!(db.env["DB_URL"], "${DB_URL}");
    assert_eq!(db.command.as_deref(), Some("node"));
    assert!(
        r.profile
            .instructions
            .as_deref()
            .unwrap()
            .contains("Run `pnpm test` before committing.")
    );
    let names: Vec<&str> = r.profile.rules.iter().map(|r| r.name.as_str()).collect();
    assert_eq!(names, ["style"], "{:?}", r.notes);
    assert!(cap_said(&r, "naming.md"), "{:?}", r.notes);
    assert!(cap_said(&r, ".roorules-debug"), "{:?}", r.notes);
}

#[test]
fn roo_apply_writes_env_references_and_documented_types() {
    let sb = Sandbox::empty();
    // The secret is in the environment, but Roo expands ${env:VAR} itself.
    let env = sb.env_with(Scope::User, &[("GITHUB_TOKEN", "ghp_FAKEvalue123")]);
    let mut prof = sample();
    prof.mcp_servers.get_mut("github").unwrap().cwd = Some("tools".into());
    prof.mcp_servers.get_mut("linear").unwrap().cwd = Some("ignored".into());
    prof.mcp_servers.insert(
        "off".into(),
        McpServer {
            command: Some("off-mcp".into()),
            disabled: true,
            ..Default::default()
        },
    );
    let p = plan(&prof, &["roo"], &env);
    let cfg = planned_json(&p, &sb.path(ROO_MCP));
    assert_eq!(
        cfg["mcpServers"]["github"],
        json!({"command": "npx", "args": ["-y", "@modelcontextprotocol/server-github"], "env": {"GITHUB_TOKEN": "${env:GITHUB_TOKEN}"}, "cwd": "tools"})
    );
    assert_eq!(
        cfg["mcpServers"]["linear"],
        json!({"url": "https://mcp.linear.app/mcp", "type": "streamable-http", "headers": {"Authorization": "Bearer ${env:LINEAR_API_KEY}"}})
    );
    assert_eq!(
        cfg["mcpServers"]["events"],
        json!({"url": "https://events.example.com/sse", "type": "sse"})
    );
    assert_eq!(
        cfg["mcpServers"]["off"],
        json!({"command": "off-mcp", "disabled": true})
    );
    assert!(p.sensitive.is_empty(), "no secret reaches the file");
    assert!(!planned(&p, &sb.path(ROO_MCP)).unwrap().contains("ghp_FAKE"));
    assert!(said(&p, "roo", "'cwd' only applies to local servers"));
    // Roo isn't installed anywhere: it says where it wrote.
    assert!(said(&p, "roo", "storage was not found"), "{:?}", p.notes);

    let md = planned(&p, &sb.path(ROO_RULES)).unwrap();
    assert!(md.contains("Use British English.") && !md.contains("Strict mode."));
    assert_eq!(
        planned(&p, &sb.path("home/.roo/rules/ts.md")).as_deref(),
        Some("Strict mode.\n")
    );
    assert_eq!(p.changes.len(), 3);
}

#[test]
fn roo_user_mcp_file_follows_the_editor_and_the_os() {
    let sb = Sandbox::empty();
    let a = kitbag::adapters::get("roo").unwrap();
    let mcp = |os: Os, vars: &[(&str, &str)]| {
        let mut env = sb.env(Scope::User);
        env.os = os;
        for (k, v) in vars {
            env.locations.insert(k.to_string(), v.to_string());
        }
        a.locations(&env)[0].1.clone()
    };
    let tail: PathBuf = [
        "globalStorage",
        "rooveterinaryinc.roo-cline",
        "settings",
        "mcp_settings.json",
    ]
    .iter()
    .collect();
    let home = sb.home();
    assert_eq!(
        mcp(Os::Linux, &[]),
        home.join(".config").join("Code").join("User").join(&tail)
    );
    assert_eq!(
        mcp(Os::Linux, &[("XDG_CONFIG_HOME", "/x/cfg")]),
        PathBuf::from("/x/cfg")
            .join("Code")
            .join("User")
            .join(&tail)
    );
    assert_eq!(
        mcp(Os::MacOs, &[]),
        home.join("Library")
            .join("Application Support")
            .join("Code")
            .join("User")
            .join(&tail)
    );
    assert_eq!(
        mcp(Os::Windows, &[("APPDATA", "C:/Users/me/AppData/Roaming")]),
        PathBuf::from("C:/Users/me/AppData/Roaming")
            .join("Code")
            .join("User")
            .join(&tail)
    );
    assert_eq!(
        mcp(Os::Windows, &[]),
        home.join("AppData")
            .join("Roaming")
            .join("Code")
            .join("User")
            .join(&tail)
    );

    // Another editor that has the extension's folder wins over the VS Code default.
    let cursor = home
        .join(".config")
        .join("Cursor")
        .join("User")
        .join("globalStorage")
        .join("rooveterinaryinc.roo-cline");
    fs::create_dir_all(&cursor).unwrap();
    assert_eq!(
        mcp(Os::Linux, &[]),
        cursor.join("settings").join("mcp_settings.json")
    );
    // VS Code comes first when both have it.
    fs::create_dir_all(
        home.join(".config")
            .join("Code")
            .join("User")
            .join("globalStorage")
            .join("rooveterinaryinc.roo-cline"),
    )
    .unwrap();
    assert_eq!(
        mcp(Os::Linux, &[]),
        home.join(".config").join("Code").join("User").join(&tail)
    );
    let r = capture(&["roo"], &sb.env(Scope::User));
    assert!(cap_said(&r, "more than one editor"));
}

#[test]
fn roo_apply_into_cursors_storage_when_that_is_where_roo_is() {
    let sb = Sandbox::empty();
    let dir = sb
        .home()
        .join(".config/Cursor/User/globalStorage/rooveterinaryinc.roo-cline");
    fs::create_dir_all(&dir).unwrap();
    let p = apply(&sb, &sample(), &["roo"], &sb.env(Scope::User));
    assert!(dir.join("settings/mcp_settings.json").is_file());
    assert!(!sb.home().join(".config/Code").exists());
    assert!(said(&p, "roo", "Cursor's extension storage"));
    assert!(!said(&p, "roo", "storage was not found"));
}

#[test]
fn roo_merge_keeps_the_users_servers_comments_and_own_keys() {
    let sb = fixture_sandbox();
    let cfg = sb.path(ROO_MCP);
    let mut prof = Profile::new("team");
    prof.mcp_servers.insert(
        "github".into(),
        McpServer {
            command: Some("npx".into()),
            args: vec![
                "-y".into(),
                "@modelcontextprotocol/server-github".into(),
                "--token=${GITHUB_TOKEN}".into(),
            ],
            env: [(
                "GITHUB_PERSONAL_ACCESS_TOKEN".to_string(),
                "${GITHUB_PERSONAL_ACCESS_TOKEN}".to_string(),
            )]
            .into_iter()
            .collect(),
            ..Default::default()
        },
    );
    let p = plan(&prof, &["roo"], &sb.env(Scope::User));
    let text = planned(&p, &cfg).unwrap();
    assert!(text.contains("// Roo's own file, hand edited"));
    let out = kitbag::edit::json::parse_value(Some(&text), &cfg).unwrap();
    for keep in ["docs", "events", "typeless", "time"] {
        assert!(out["mcpServers"].get(keep).is_some(), "{keep}");
    }
    let gh = &out["mcpServers"]["github"];
    assert_eq!(gh["alwaysAllow"][0], "search_repositories");
    assert_eq!(gh["timeout"], 90);
    // `cwd` is the profile's to decide: the profile has none.
    assert!(gh.get("cwd").is_none());
    assert_eq!(
        gh["env"]["GITHUB_PERSONAL_ACCESS_TOKEN"],
        "${env:GITHUB_PERSONAL_ACCESS_TOKEN}"
    );
}

#[test]
fn roo_apply_project_writes_mcp_json_and_agents_md_inside_the_project() {
    let sb = Sandbox::empty();
    let env = sb.env(Scope::Project);
    let p = plan(&sample(), &["roo"], &env);
    let cfg = planned_json(&p, &sb.project().join(".roo/mcp.json"));
    assert_eq!(cfg["mcpServers"]["events"]["type"], "sse");
    assert!(planned(&p, &sb.project().join("AGENTS.md")).is_some());
    assert!(p.changes.iter().all(|c| c.path.starts_with(sb.project())));
    assert!(said(&p, "roo", "alwaysAllow"));
    assert!(!said(&p, "roo", "storage was not found"));
}

#[test]
fn roo_apply_is_idempotent_and_undo_restores() {
    let sb = Sandbox::empty();
    let env = sb.env(Scope::User);
    let rules = sb.path(ROO_RULES);
    fs::create_dir_all(rules.parent().unwrap()).unwrap();
    fs::write(&rules, "My own notes.\n").unwrap();
    let prof = sample();
    apply(&sb, &prof, &["roo"], &env);
    assert!(plan(&prof, &["roo"], &env).changes.is_empty());
    let back = capture(&["roo"], &env).profile;
    assert_eq!(back.rules.len(), 1);
    assert_eq!(back.mcp_servers["linear"].transport(), Transport::Http);
    assert_eq!(back.mcp_servers["events"].transport(), Transport::Sse);
    assert_eq!(
        back.mcp_servers["github"].env["GITHUB_TOKEN"],
        "${GITHUB_TOKEN}"
    );
    Store::new(sb.state()).undo(None, false).unwrap();
    assert_eq!(sb.read(ROO_RULES), "My own notes.\n");
    assert!(!sb.path(ROO_STORE).join("mcp_settings.json").exists());
}

#[test]
fn roo_detect() {
    let sb = Sandbox::empty();
    let a = kitbag::adapters::get("roo").unwrap();
    for scope in [Scope::User, Scope::Project] {
        assert!(!a.detect(&sb.env(scope)));
    }
    fs::write(sb.project().join("AGENTS.md"), "x\n").unwrap();
    assert!(!a.detect(&sb.env(Scope::Project)), "AGENTS.md is shared");
    fs::create_dir_all(sb.path(ROO_STORE)).unwrap();
    assert!(a.detect(&sb.env(Scope::User)));
    fs::create_dir_all(sb.project().join(".roo")).unwrap();
    assert!(a.detect(&sb.env(Scope::Project)));
}

#[test]
fn roo_code_is_an_alias_for_roo() {
    assert_eq!(
        kitbag::adapters::get("roo-code").map(|a| a.id()),
        Some("roo")
    );
    assert_eq!(kitbag::adapters::canonical_id("roo-code"), "roo");
}

// ------------------------------------------------------ shared files

/// At project scope both tools write AGENTS.md, as Codex, Cursor, Antigravity,
/// opencode, Grok Build, Windsurf and Zed do: one file, one managed block.
#[test]
fn cline_and_roo_share_agents_md_with_the_other_targets() {
    let prof = sample();
    let others = [
        "codex",
        "cursor",
        "antigravity",
        "opencode",
        "grok-build",
        "windsurf",
        "zed",
        "hermes",
    ];
    let sb = Sandbox::empty();
    let env = sb.env(Scope::Project);
    fs::write(sb.project().join("AGENTS.md"), "My own notes.\n").unwrap();
    let mut together = others.to_vec();
    together.extend(["cline", "roo"]);
    let p = apply(&sb, &prof, &together, &env);
    let agents = sb.project().join("AGENTS.md");
    assert_eq!(
        p.changes.iter().filter(|c| c.path == agents).count(),
        1,
        "AGENTS.md is planned once"
    );
    let text = sb.read("project/AGENTS.md");
    assert_eq!(count_blocks(&text), 1, "{text}");
    assert_eq!(text.matches("Use British English.").count(), 1, "{text}");
    assert!(text.starts_with("My own notes."));
    assert!(plan(&prof, &together, &env).changes.is_empty());

    for order in [
        ["cline", "roo"],
        ["roo", "cline"],
        ["codex", "roo"],
        ["cline", "codex"],
    ] {
        let sb = Sandbox::empty();
        let env = sb.env(Scope::Project);
        apply(&sb, &prof, &[order[0]], &env);
        let earlier = sb.read("project/AGENTS.md");
        let second = apply(&sb, &prof, &[order[1]], &env);
        let text = sb.read("project/AGENTS.md");
        assert_eq!(count_blocks(&text), 1, "{order:?}: {text}");
        assert!(plan(&prof, &order, &env).changes.is_empty(), "{order:?}");
        assert!(
            !text.contains("kitbag:begin team -->\n<!-- kitbag:begin"),
            "{order:?}"
        );
        if !second.changes.is_empty() {
            Store::new(sb.state()).undo(None, false).unwrap();
            assert_eq!(sb.read("project/AGENTS.md"), earlier, "{order:?}");
        }
    }
}

/// At user scope Cline and Roo each have a file of their own, so neither
/// touches the other's or another tool's.
#[test]
fn cline_and_roo_user_files_are_separate() {
    let sb = Sandbox::empty();
    let env = sb.env(Scope::User);
    let prof = sample();
    let p = apply(&sb, &prof, &["cline", "roo", "windsurf", "zed"], &env);
    let mut paths: Vec<_> = p.changes.iter().map(|c| c.path.clone()).collect();
    let n = paths.len();
    paths.sort();
    paths.dedup();
    assert_eq!(paths.len(), n, "no file planned twice");
    assert_eq!(count_blocks(&sb.read(CLINE_RULES)), 1);
    assert_eq!(count_blocks(&sb.read(ROO_RULES)), 1);
    assert!(
        plan(&prof, &["cline", "roo", "windsurf", "zed"], &env)
            .changes
            .is_empty()
    );
}

// ------------------------------------------------------------ round trips

#[test]
fn cline_to_roo_and_back() {
    let src = fixture_sandbox();
    let captured = capture(&["cline"], &src.env(Scope::User)).profile;
    let dir = src.path("profile");
    kitbag::profile_io::save(&captured, &dir, false).unwrap();
    let loaded = kitbag::profile_io::load(&dir).unwrap();
    assert_eq!(loaded, captured, "profile survives save + load unchanged");

    let dst = Sandbox::empty();
    apply(&dst, &loaded, &["roo"], &dst.env(Scope::User));
    let back = capture(&["roo"], &dst.env(Scope::User)).profile;
    assert_eq!(back.instructions, loaded.instructions);
    for (name, s) in &loaded.mcp_servers {
        assert_eq!(&back.mcp_servers[name], s, "server {name}");
    }

    // And into Cline in a clean home: the same profile comes back, disabled included.
    let again = Sandbox::empty();
    apply(&again, &back, &["cline"], &again.env_with(Scope::User, &[]));
    let round = capture(&["cline"], &again.env(Scope::User)).profile;
    assert_eq!(round.instructions, loaded.instructions);
    assert_eq!(round.mcp_servers.len(), loaded.mcp_servers.len());
}

#[test]
fn claude_code_to_cline_and_roo() {
    let src = Sandbox::fixtures();
    let profile = capture(&["claude-code"], &src.env(Scope::User)).profile;
    let dst = Sandbox::empty();
    let mut o = opts(&profile);
    o.secrets = SecretsMode::Never;
    let env = dst.env(Scope::User);
    let p = kitbag::engine::plan(&profile, &targets(&["cline", "roo"]), &env, &o).unwrap();
    kitbag::engine::commit(&p, "test", &dst.state()).unwrap();
    let roo = capture(&["roo"], &env).profile;
    let cline = capture(&["cline"], &env).profile;
    for (name, s) in &profile.mcp_servers {
        if s.disabled {
            continue;
        }
        // Roo has `${env:VAR}` and `cwd`: the profile comes back unchanged.
        assert_eq!(&roo.mcp_servers[name], s, "roo server {name}");
        // Cline has no env syntax and no cwd.
        let mut want = s.clone();
        want.cwd = None;
        assert_eq!(&cline.mcp_servers[name], &want, "cline server {name}");
    }
    assert_eq!(roo.instructions, profile.instructions);
    assert_eq!(cline.instructions, profile.instructions);
}

// ------------------------------------------------------------------- CLI

#[test]
fn apply_to_cline_and_roo_then_undo_through_the_cli() {
    let sb = Sandbox::empty();
    sb.cli().args(["init", "prof"]).assert().success();
    fs::write(
        sb.path("prof/kitbag.toml"),
        "kitbag = 1\nname = \"prof\"\n\n[mcp_servers.docs]\nurl = \"https://docs.example.com/mcp\"\nheaders = { Authorization = \"Bearer ${DOCS_TOKEN}\" }\n",
    )
    .unwrap();
    sb.cli()
        .args(["apply", "prof", "--to", "cline,roo-code", "--yes"])
        .env("DOCS_TOKEN", "fake-token-value")
        .assert()
        .success();
    let cline = sb.json(CLINE_MCP);
    assert_eq!(
        cline["mcpServers"]["docs"],
        json!({"url": "https://docs.example.com/mcp", "type": "streamableHttp", "headers": {"Authorization": "Bearer fake-token-value"}})
    );
    // Roo's user file is under the editor's user data directory, which depends on the OS.
    let roo_text = std::fs::read_to_string(
        sb.home().join(
            if cfg!(target_os = "macos") {
                "Library/Application Support/Code/User/globalStorage/rooveterinaryinc.roo-cline/settings/mcp_settings.json"
            } else if cfg!(windows) {
                "AppData/Roaming/Code/User/globalStorage/rooveterinaryinc.roo-cline/settings/mcp_settings.json"
            } else {
                ".config/Code/User/globalStorage/rooveterinaryinc.roo-cline/settings/mcp_settings.json"
            },
        ),
    )
    .unwrap();
    let roo: Value = serde_json::from_str(&roo_text).unwrap();
    assert_eq!(
        roo["mcpServers"]["docs"]["headers"]["Authorization"],
        "Bearer ${env:DOCS_TOKEN}"
    );
    assert!(!roo_text.contains("fake-token-value"));
    assert!(
        sb.read(CLINE_RULES)
            .contains("Write instructions every agent should follow")
    );
    sb.cli().arg("undo").assert().success();
    assert!(
        !sb.path("home/.cline").exists(),
        "undo removes what apply created"
    );
    assert!(!sb.path("home/.roo").exists());
}

#[test]
fn list_targets_names_both() {
    let sb = Sandbox::empty();
    let out = sb.cli().args(["list-targets", "--json"]).output().unwrap();
    let v: Value = serde_json::from_slice(&out.stdout).unwrap();
    let find = |id: &str| {
        v.as_array()
            .unwrap()
            .iter()
            .find(|t| t["id"] == id)
            .unwrap_or_else(|| panic!("{id} listed"))
            .clone()
    };
    for id in ["cline", "roo"] {
        let t = find(id);
        assert_eq!(t["support"]["mcp"], "native");
        assert_eq!(t["support"]["instructions"], "native");
        assert_eq!(t["support"]["rules"], "native");
        assert_eq!(t["support"]["skills"], "native");
        assert_eq!(t["support"]["commands"], "native");
    }
}

// ------------------------------------------- rules, workflows and skills

fn put(sb: &Sandbox, rel: &str, text: &str) {
    let path = sb.path(rel);
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, text).unwrap();
}

fn rule(name: &str, body: &str) -> Rule {
    Rule {
        name: name.into(),
        body: body.into(),
        ..Default::default()
    }
}

fn skill(name: &str, body: &str) -> Skill {
    Skill {
        name: name.into(),
        files: vec![
            SkillFile {
                path: "SKILL.md".into(),
                content: format!(
                    "---\nname: {name}\ndescription: Does {name} things.\n---\n\n{body}\n"
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

/// A profile with a rule of each kind, a command and a skill, and no servers.
fn items() -> Profile {
    let mut p = Profile::new("team");
    p.instructions = Some("Use British English.".into());
    p.rules.push(rule("always", "Be brief."));
    p.rules.push(Rule {
        globs: vec!["src/**/*.ts".into(), "*.tsx".into()],
        ..rule("ts", "Strict mode.")
    });
    p.rules.push(Rule {
        always_apply: Some(false),
        description: Some("Database work".into()),
        ..rule("db", "Use migrations.")
    });
    p.commands.push(Command {
        name: "release".into(),
        description: Some("Cut a release".into()),
        body: "Tag it, then publish.".into(),
        ..Default::default()
    });
    p.skills.push(skill("deploy-notes", "Deploy body."));
    p
}

/// A skill's files without the executable bit, which Windows doesn't have.
fn contents(files: &[SkillFile]) -> Vec<(&str, &[u8])> {
    files
        .iter()
        .map(|f| (f.path.as_str(), f.content.as_slice()))
        .collect()
}

fn names<T>(items: &[T], name: impl Fn(&T) -> &str) -> Vec<String> {
    let mut v: Vec<String> = items.iter().map(|i| name(i).to_string()).collect();
    v.sort();
    v
}

// ---- Cline

#[test]
fn cline_capture_reads_workflows_and_skills_from_every_documented_folder() {
    let sb = fixture_sandbox();
    let user = capture(&["cline"], &sb.env(Scope::User));
    assert_eq!(
        names(&user.profile.commands, |c| &c.name),
        ["deploy", "release"],
        "~/.cline/workflows and ~/Documents/Cline/Workflows: {:?}",
        user.notes
    );
    let release = user
        .profile
        .commands
        .iter()
        .find(|c| c.name == "release")
        .unwrap();
    assert!(release.body.starts_with("Run the release checklist."));
    assert_eq!(
        names(&user.profile.skills, |s| &s.name),
        ["pdf-notes", "shared-notes"],
        "~/.cline/skills and ~/.agents/skills"
    );
    let pdf = user
        .profile
        .skills
        .iter()
        .find(|s| s.name == "pdf-notes")
        .unwrap();
    assert!(pdf.files.iter().any(|f| f.path == "docs/advanced.md"));
    assert!(user.sources.iter().all(|s| !s.ends_with("providers.json")));

    let project = capture(&["cline"], &sb.env(Scope::Project));
    assert_eq!(
        names(&project.profile.commands, |c| &c.name),
        ["triage"],
        ".clinerules/workflows"
    );
    assert_eq!(
        names(&project.profile.skills, |s| &s.name),
        ["changelog", "repo-guide", "triage-helper"],
        ".cline/skills, .clinerules/skills and .agents/skills: {:?}",
        project.notes
    );
}

#[test]
fn cline_capture_leaves_other_tools_files_and_odd_layouts_alone() {
    let sb = fixture_sandbox();
    // Files other tools own, which Cline also detects.
    put(&sb, "project/.cursorrules", "Cursor-only rule.\n");
    put(&sb, "project/.windsurfrules", "Windsurf-only rule.\n");
    // A rule that never applies, a nested file and a non-markdown file.
    put(
        &sb,
        "project/.cline/rules/off.md",
        "---\npaths: []\n---\nNever applies.\n",
    );
    put(&sb, "project/.cline/rules/sub/deep.md", "Deep.\n");
    put(&sb, "project/.cline/rules/notes.txt", "Text notes.\n");
    let r = capture(&["cline"], &sb.env(Scope::Project));
    let rules = names(&r.profile.rules, |r| &r.name);
    assert_eq!(rules, ["lint", "tabs"], "{:?}", r.notes);
    let all = format!("{:?}", r.profile);
    assert!(!all.contains("Cursor-only") && !all.contains("Windsurf-only"));
    assert!(cap_said(&r, "rule 'off' has `paths: []`"), "{:?}", r.notes);
    assert!(
        cap_said(&r, "sub/deep.md") && cap_said(&r, "notes.txt"),
        "{:?}",
        r.notes
    );

    // The compatibility folders under the home directory.
    put(&sb, "home/Cline/Rules/extra.md", "Extra rule.\n");
    let u = capture(&["cline"], &sb.env(Scope::User));
    assert!(
        u.profile.rules.iter().any(|r| r.name == "extra"),
        "~/Cline/Rules"
    );

    // `.clinerules` as one file is not the documented layout.
    let sb2 = Sandbox::empty();
    put(&sb2, "project/.clinerules", "One file.\n");
    let r = capture(&["cline"], &sb2.env(Scope::Project));
    assert!(r.profile.rules.is_empty());
    assert!(cap_said(&r, "a .clinerules file was not read"));
}

#[test]
fn cline_apply_user_writes_rules_workflows_and_skills_and_undo_removes_only_those() {
    let sb = Sandbox::empty();
    let env = sb.env(Scope::User);
    put(&sb, "home/.cline/rules/mine.md", "My own rule.\n");
    put(
        &sb,
        "home/.agents/skills/theirs/SKILL.md",
        "---\nname: theirs\ndescription: Not ours.\n---\n",
    );
    let prof = items();
    let p = apply(&sb, &prof, &["cline"], &env);

    assert_eq!(sb.read("home/.cline/rules/always.md"), "Be brief.\n");
    assert_eq!(
        sb.read("home/.cline/rules/ts.md"),
        "---\npaths:\n  - \"src/**/*.ts\"\n  - \"*.tsx\"\n---\n\nStrict mode.\n"
    );
    let db = sb.read("home/.cline/rules/db.md");
    assert!(
        db.starts_with("_Use when relevant to the task._\n\nUse migrations."),
        "{db}"
    );
    assert!(said(&p, "cline", "rule 'db' is on-demand"));
    // Workflows are plain markdown, no frontmatter invented.
    assert_eq!(
        sb.read("home/.cline/workflows/release.md"),
        "Tag it, then publish.\n"
    );
    assert!(
        sb.read("home/.cline/skills/deploy-notes/SKILL.md")
            .contains("Deploy body.")
    );
    assert_eq!(
        sb.read("home/.cline/skills/deploy-notes/scripts/run.sh"),
        "echo run\n"
    );
    // Instructions stay in kitbag.md, without the rules.
    let md = sb.read(CLINE_RULES);
    assert!(
        md.contains("Use British English.") && !md.contains("Strict mode."),
        "{md}"
    );
    assert!(
        !sb.path("home/.agents/skills/deploy-notes").exists(),
        "never into .agents"
    );

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = fs::metadata(sb.path("home/.cline/skills/deploy-notes/scripts/run.sh"))
            .unwrap()
            .permissions()
            .mode();
        assert_ne!(mode & 0o111, 0, "the executable bit is kept");
    }

    // A second apply is up to date, and capture reads it all back.
    assert!(plan(&prof, &["cline"], &env).changes.is_empty());
    let back = capture(&["cline"], &env).profile;
    assert_eq!(
        names(&back.rules, |r| &r.name),
        ["always", "db", "mine", "ts"]
    );
    let ts = back.rules.iter().find(|r| r.name == "ts").unwrap();
    assert_eq!(ts.globs, ["src/**/*.ts", "*.tsx"]);
    let db = back.rules.iter().find(|r| r.name == "db").unwrap();
    assert_eq!(db.mode(), RuleMode::OnDemand, "the lead line is read back");
    assert_eq!(db.body, "Use migrations.");
    assert_eq!(back.commands.len(), 1);
    assert_eq!(back.commands[0].body, "Tag it, then publish.");
    let s = back
        .skills
        .iter()
        .find(|s| s.name == "deploy-notes")
        .unwrap();
    assert_eq!(contents(&s.files), contents(&prof.skills[0].files));

    Store::new(sb.state()).undo(None, false).unwrap();
    assert_eq!(sb.read("home/.cline/rules/mine.md"), "My own rule.\n");
    assert!(sb.path("home/.agents/skills/theirs/SKILL.md").is_file());
    for gone in [
        "home/.cline/rules/always.md",
        "home/.cline/rules/ts.md",
        "home/.cline/workflows",
        "home/.cline/skills",
        CLINE_RULES,
    ] {
        assert!(!sb.path(gone).exists(), "{gone} is removed");
    }
}

#[test]
fn cline_a_rule_named_kitbag_does_not_replace_the_instructions_file() {
    let sb = Sandbox::empty();
    let env = sb.env(Scope::User);
    let mut prof = items();
    prof.rules.push(rule("kitbag", "Sneaky."));
    let p = plan(&prof, &["cline"], &env);
    assert!(said(&p, "cline", "rule 'kitbag' skipped"));
    let md = planned(&p, &sb.path(CLINE_RULES)).unwrap();
    assert!(!md.contains("Sneaky.") && md.contains("Use British English."));
}

#[test]
fn cline_project_writes_to_cline_dirs_and_updates_the_older_layout_in_place() {
    let sb = Sandbox::empty();
    let env = sb.env(Scope::Project);
    put(&sb, "project/.clinerules/ts.md", "Old strict mode.\n");
    put(
        &sb,
        "project/.clinerules/workflows/release.md",
        "Old release.\n",
    );
    put(
        &sb,
        "project/.agents/skills/deploy-notes/SKILL.md",
        "---\nname: deploy-notes\ndescription: Another tool's copy.\n---\n",
    );
    let prof = items();
    let p = apply(&sb, &prof, &["cline"], &env);

    // Already in the older layout: updated there, not copied.
    assert!(
        sb.read("project/.clinerules/ts.md")
            .contains("Strict mode.")
    );
    assert!(!sb.path("project/.cline/rules/ts.md").exists());
    assert_eq!(
        sb.read("project/.clinerules/workflows/release.md"),
        "Tag it, then publish.\n"
    );
    // New ones go to .cline/.
    assert_eq!(sb.read("project/.cline/rules/always.md"), "Be brief.\n");
    assert!(
        sb.path("project/.cline/skills/deploy-notes/SKILL.md")
            .is_file()
    );
    // The other tool's copy is untouched, and the user is told about the clash.
    assert!(
        sb.read("project/.agents/skills/deploy-notes/SKILL.md")
            .contains("Another tool's copy.")
    );
    assert!(said(&p, "cline", "also exists in"), "{:?}", p.notes);
    assert!(p.changes.iter().all(|c| c.path.starts_with(sb.project())));
    assert!(plan(&prof, &["cline"], &env).changes.is_empty());
}

#[test]
fn cline_user_apply_removes_the_rules_an_older_kitbag_folded_into_the_instructions() {
    let sb = Sandbox::empty();
    let env = sb.env(Scope::User);
    let prof = items();
    // What the previous version wrote: instructions and folded rules.
    let folded = kitbag::edit::block::fold_rules(&prof.rules);
    let text = kitbag::edit::block::upsert(None, "team", Some("Use British English."));
    let text = kitbag::edit::block::upsert(Some(&text), "team:rules", folded.as_deref());
    put(&sb, CLINE_RULES, &text);
    assert_eq!(
        capture(&["cline"], &env).profile.rules.len(),
        3,
        "still readable"
    );

    apply(&sb, &prof, &["cline"], &env);
    let md = sb.read(CLINE_RULES);
    assert!(
        !md.contains("Be brief.") && md.contains("Use British English."),
        "{md}"
    );
    let back = capture(&["cline"], &env).profile;
    assert_eq!(back.rules.len(), 3, "each rule once");
}

#[test]
fn cline_project_apply_leaves_the_rules_codex_folded_into_agents_md() {
    let sb = Sandbox::empty();
    let env = sb.env(Scope::Project);
    let prof = items();
    apply(&sb, &prof, &["codex"], &env);
    let before = sb.read("project/AGENTS.md");
    assert!(before.contains("Strict mode."));
    let p = apply(&sb, &prof, &["cline"], &env);
    assert_eq!(
        sb.read("project/AGENTS.md"),
        before,
        "Cline does not touch codex's block"
    );
    assert!(!said(&p, "cline", "sees the rules twice"), "{:?}", p.notes);
    // Planned together, Cline says that Codex's folded copy is read as well.
    let both = plan(&prof, &["codex", "cline"], &env);
    assert!(
        said(&both, "cline", "sees the rules twice"),
        "{:?}",
        both.notes
    );
    // And the other order.
    let sb = Sandbox::empty();
    apply(&sb, &prof, &["cline"], &env_for(&sb));
    apply(&sb, &prof, &["codex"], &env_for(&sb));
    assert!(
        plan(&prof, &["cline", "codex"], &env_for(&sb))
            .changes
            .is_empty()
    );
}

fn env_for(sb: &Sandbox) -> kitbag::adapters::Env {
    sb.env(Scope::Project)
}

#[test]
fn cline_and_roo_honour_targets_and_disabled_on_for_the_new_items() {
    let sb = Sandbox::empty();
    let env = sb.env(Scope::User);
    let mut prof = items();
    prof.rules[0].targeting.disabled_on = vec!["cline".into()];
    prof.rules[1].targeting.targets = Some(vec!["roo".into()]);
    prof.commands[0].targeting.disabled_on = vec!["roo".into()];
    prof.skills[0].targeting.targets = Some(vec!["cline".into()]);
    let p = plan(&prof, &["cline", "roo"], &env);
    let has = |rel: &str| planned(&p, &sb.path(rel)).is_some();
    assert!(!has("home/.cline/rules/always.md") && has("home/.roo/rules/always.md"));
    assert!(!has("home/.cline/rules/ts.md") && has("home/.roo/rules/ts.md"));
    assert!(has("home/.cline/workflows/release.md") && !has("home/.roo/commands/release.md"));
    assert!(has("home/.cline/skills/deploy-notes/SKILL.md"));
    assert!(!has("home/.roo/skills/deploy-notes/SKILL.md"));
}

#[test]
fn cline_dir_moves_rules_workflows_and_skills_too() {
    let sb = Sandbox::empty();
    let mut env = sb.env(Scope::User);
    env.locations.insert(
        "CLINE_DIR".into(),
        sb.path("elsewhere").display().to_string(),
    );
    let p = plan(&items(), &["cline"], &env);
    for rel in [
        "elsewhere/rules/always.md",
        "elsewhere/workflows/release.md",
        "elsewhere/skills/deploy-notes/SKILL.md",
    ] {
        assert!(planned(&p, &sb.path(rel)).is_some(), "{rel}");
    }
    assert!(
        p.changes
            .iter()
            .all(|c| !c.path.starts_with(sb.path("home/.cline")))
    );
}

#[test]
fn cline_capture_redacts_secrets_in_rules_workflows_and_skills() {
    let sb = Sandbox::empty();
    put(
        &sb,
        "home/.cline/rules/deploy.md",
        "Use token ghp_FAKE0123456789abcdefghijklmnopqrstuv here.\n",
    );
    put(
        &sb,
        "home/.cline/workflows/push.md",
        "Push with ghp_FAKE0123456789abcdefghijklmnopqrstuv.\n",
    );
    put(
        &sb,
        "home/.cline/skills/pusher/SKILL.md",
        "---\nname: pusher\ndescription: Push.\n---\n\nToken ghp_FAKE0123456789abcdefghijklmnopqrstuv.\n",
    );
    let r = capture(&["cline"], &sb.env(Scope::User));
    let dir = sb.path("profile");
    kitbag::profile_io::save(&r.profile, &dir, false).unwrap();
    assert!(!all_text_under(&dir).contains("ghp_FAKE"));
    assert_eq!(
        r.profile.rules.len() + r.profile.commands.len() + r.profile.skills.len(),
        3
    );
}

// ---- Roo Code

#[test]
fn roo_capture_reads_commands_skills_and_lists_mode_specific_files() {
    let sb = fixture_sandbox();
    let user = capture(&["roo"], &sb.env(Scope::User));
    assert_eq!(names(&user.profile.commands, |c| &c.name), ["review"]);
    let review = &user.profile.commands[0];
    assert_eq!(review.description.as_deref(), Some("Review the diff"));
    assert_eq!(review.argument_hint.as_deref(), Some("<file>"));
    assert_eq!(review.extra["mode"], "architect", "Roo's mode key is kept");
    assert_eq!(review.body, "Review the changes carefully.");
    assert_eq!(
        names(&user.profile.skills, |s| &s.name),
        ["shared-notes", "summarise"],
        "~/.roo/skills and ~/.agents/skills (the mode folder is not read)"
    );
    assert!(
        cap_said(&user, "refactor"),
        "skills-code is listed: {:?}",
        user.notes
    );
    assert!(cap_said(&user, "mode-specific skills"));

    let project = capture(&["roo"], &sb.env(Scope::Project));
    assert_eq!(names(&project.profile.commands, |c| &c.name), ["ship"]);
    assert_eq!(
        names(&project.profile.skills, |s| &s.name),
        ["release-notes", "repo-guide"],
        ".roo/skills and .agents/skills"
    );
    assert!(project.sources.iter().all(|s| !s.ends_with("naming.md")));
}

#[test]
fn roo_capture_reads_nested_rules_and_lists_the_files_it_leaves() {
    let sb = Sandbox::empty();
    put(
        &sb,
        "project/.roo/rules/team/naming.md",
        "Name things well.\n",
    );
    put(&sb, "project/.roo/rules/notes.txt", "Text notes.\n");
    put(&sb, "project/.roorules", "Fallback rules.\n");
    let r = capture(&["roo"], &sb.env(Scope::Project));
    assert_eq!(
        names(&r.profile.rules, |r| &r.name),
        ["team-naming"],
        "{:?}",
        r.notes
    );
    assert!(cap_said(&r, "notes.txt"), "{:?}", r.notes);
    assert!(
        cap_said(&r, "a .roorules file was not read"),
        "{:?}",
        r.notes
    );
}

#[test]
fn roo_apply_user_writes_rules_commands_and_skills_and_undo_removes_only_those() {
    let sb = Sandbox::empty();
    let env = sb.env(Scope::User);
    put(&sb, "home/.roo/rules/mine.md", "My own rule.\n");
    let mut prof = items();
    prof.commands[0]
        .extra
        .insert("mode".into(), json!("architect"));
    prof.commands[0].argument_hint = Some("<version>".into());
    prof.skills.push(skill("Bad_Name", "Invalid for Roo."));
    let p = apply(&sb, &prof, &["roo"], &env);

    // Plain markdown, no frontmatter Roo's docs don't describe.
    assert_eq!(sb.read("home/.roo/rules/always.md"), "Be brief.\n");
    assert_eq!(
        sb.read("home/.roo/rules/ts.md"),
        "_Applies when working with files matching: `src/**/*.ts`, `*.tsx`_\n\nStrict mode.\n"
    );
    assert!(
        sb.read("home/.roo/rules/db.md")
            .starts_with("_Use when relevant to the task._\n\nUse migrations.")
    );
    assert!(
        said(&p, "roo", "rule 'ts' is file-scoped") && said(&p, "roo", "rule 'db' is on-demand")
    );
    assert_eq!(
        sb.read("home/.roo/commands/release.md"),
        "---\ndescription: Cut a release\nargument-hint: \"<version>\"\nmode: architect\n---\n\nTag it, then publish.\n"
    );
    assert!(said(
        &p,
        "roo",
        "its mode 'architect' is written as Roo Code's `mode`"
    ));
    assert!(sb.path("home/.roo/skills/deploy-notes/SKILL.md").is_file());
    assert!(!sb.path("home/.roo/skills/Bad_Name").exists());
    assert!(said(&p, "roo", "skill 'Bad_Name' skipped"), "{:?}", p.notes);
    let md = sb.read(ROO_RULES);
    assert!(md.contains("Use British English.") && !md.contains("Strict mode."));

    assert!(
        plan(&prof, &["roo"], &env).changes.is_empty(),
        "second apply is up to date"
    );
    let back = capture(&["roo"], &env).profile;
    assert_eq!(
        names(&back.rules, |r| &r.name),
        ["always", "db", "mine", "ts"]
    );
    let ts = back.rules.iter().find(|r| r.name == "ts").unwrap();
    assert_eq!(
        ts.globs,
        ["src/**/*.ts", "*.tsx"],
        "the lead line is read back"
    );
    assert_eq!(ts.body, "Strict mode.");
    let db = back.rules.iter().find(|r| r.name == "db").unwrap();
    assert_eq!(db.mode(), RuleMode::OnDemand);
    assert_eq!(back.commands[0].extra["mode"], "architect");
    assert_eq!(back.commands[0].argument_hint.as_deref(), Some("<version>"));
    assert_eq!(back.skills.len(), 1);
    assert_eq!(
        contents(&back.skills[0].files),
        contents(&prof.skills[0].files)
    );

    Store::new(sb.state()).undo(None, false).unwrap();
    assert_eq!(sb.read("home/.roo/rules/mine.md"), "My own rule.\n");
    for gone in [
        "home/.roo/rules/always.md",
        "home/.roo/commands",
        "home/.roo/skills",
        ROO_RULES,
    ] {
        assert!(!sb.path(gone).exists(), "{gone} is removed");
    }
}

#[test]
fn roo_a_rule_named_kitbag_does_not_replace_the_instructions_file() {
    let sb = Sandbox::empty();
    let mut prof = items();
    prof.rules.push(rule("kitbag", "Sneaky."));
    let p = plan(&prof, &["roo"], &sb.env(Scope::User));
    assert!(said(&p, "roo", "rule 'kitbag' skipped"));
    assert!(
        !planned(&p, &sb.path(ROO_RULES))
            .unwrap()
            .contains("Sneaky.")
    );
}

#[test]
fn roo_project_creates_no_rules_folder_without_rules_and_warns_when_roorules_would_be_hidden() {
    let sb = Sandbox::empty();
    let env = sb.env(Scope::Project);
    put(&sb, "project/.roorules", "Fallback rules.\n");
    // Instructions only: AGENTS.md, and nothing in .roo/rules/.
    let mut bare = Profile::new("team");
    bare.instructions = Some("Use British English.".into());
    let p = apply(&sb, &bare, &["roo"], &env);
    assert!(
        !sb.path("project/.roo").exists(),
        "no empty .roo/rules that would hide .roorules"
    );
    assert!(!said(&p, "roo", ".roorules"));

    // A rule file there makes Roo ignore .roorules: say so.
    let p = plan(&items(), &["roo"], &env);
    assert!(said(&p, "roo", "make Roo ignore it"), "{:?}", p.notes);
    // Not when .roo/rules/ already has files, or there is no .roorules.
    put(&sb, "project/.roo/rules/existing.md", "Existing.\n");
    let p = plan(&items(), &["roo"], &env);
    assert!(!said(&p, "roo", "make Roo ignore it"));
    let sb2 = Sandbox::empty();
    let p = plan(&items(), &["roo"], &sb2.env(Scope::Project));
    assert!(!said(&p, "roo", "make Roo ignore it"));
    assert!(p.changes.iter().all(|c| c.path.starts_with(sb2.project())));
}

#[test]
fn roo_project_apply_writes_to_roo_dirs_and_never_into_agents_skills() {
    let sb = Sandbox::empty();
    let env = sb.env(Scope::Project);
    put(
        &sb,
        "project/.agents/skills/deploy-notes/SKILL.md",
        "---\nname: deploy-notes\ndescription: Another tool's copy.\n---\n",
    );
    let prof = items();
    let p = apply(&sb, &prof, &["roo"], &env);
    assert!(sb.path("project/.roo/rules/ts.md").is_file());
    assert!(sb.path("project/.roo/commands/release.md").is_file());
    assert!(
        sb.path("project/.roo/skills/deploy-notes/SKILL.md")
            .is_file()
    );
    assert!(
        sb.read("project/.agents/skills/deploy-notes/SKILL.md")
            .contains("Another tool's copy.")
    );
    assert!(said(&p, "roo", "also exists in"), "{:?}", p.notes);
    assert!(!sb.path("project/.roo/rules/kitbag.md").exists());
    assert!(
        sb.read("project/AGENTS.md")
            .contains("Use British English.")
    );
    assert!(plan(&prof, &["roo"], &env).changes.is_empty());
    assert!(p.changes.iter().all(|c| c.path.starts_with(sb.project())));
}

#[test]
fn roo_user_apply_removes_the_rules_an_older_kitbag_folded_into_the_instructions() {
    let sb = Sandbox::empty();
    let env = sb.env(Scope::User);
    let prof = items();
    let folded = kitbag::edit::block::fold_rules(&prof.rules);
    let text = kitbag::edit::block::upsert(None, "team", Some("Use British English."));
    let text = kitbag::edit::block::upsert(Some(&text), "team:rules", folded.as_deref());
    put(&sb, ROO_RULES, &text);
    apply(&sb, &prof, &["roo"], &env);
    assert!(!sb.read(ROO_RULES).contains("Be brief."));
    assert_eq!(
        capture(&["roo"], &env).profile.rules.len(),
        3,
        "each rule once"
    );
}

#[test]
fn roo_capture_redacts_secrets_in_rules_commands_and_skills() {
    let sb = Sandbox::empty();
    put(
        &sb,
        "home/.roo/rules/deploy.md",
        "Use token ghp_FAKE0123456789abcdefghijklmnopqrstuv here.\n",
    );
    put(
        &sb,
        "home/.roo/commands/push.md",
        "Push with ghp_FAKE0123456789abcdefghijklmnopqrstuv.\n",
    );
    put(
        &sb,
        "home/.roo/skills/pusher/SKILL.md",
        "---\nname: pusher\ndescription: Push.\n---\n\nToken ghp_FAKE0123456789abcdefghijklmnopqrstuv.\n",
    );
    let r = capture(&["roo"], &sb.env(Scope::User));
    let dir = sb.path("profile");
    kitbag::profile_io::save(&r.profile, &dir, false).unwrap();
    assert!(!all_text_under(&dir).contains("ghp_FAKE"));
    assert_eq!(
        r.profile.rules.len() + r.profile.commands.len() + r.profile.skills.len(),
        3
    );
}

// ---- both

#[test]
fn rules_workflows_and_skills_go_from_cline_to_roo_and_back() {
    let src = fixture_sandbox();
    let captured = capture(&["cline"], &src.env(Scope::User)).profile;
    let dir = src.path("profile");
    kitbag::profile_io::save(&captured, &dir, false).unwrap();
    let loaded = kitbag::profile_io::load(&dir).unwrap();
    assert_eq!(loaded, captured);
    assert_eq!(loaded.rules.len(), 2);
    assert_eq!(loaded.commands.len(), 2);
    assert_eq!(loaded.skills.len(), 2);

    let dst = Sandbox::empty();
    apply(&dst, &loaded, &["roo"], &dst.env(Scope::User));
    let roo = capture(&["roo"], &dst.env(Scope::User)).profile;
    assert_eq!(names(&roo.rules, |r| &r.name), ["style", "typescript"]);
    let ts = roo.rules.iter().find(|r| r.name == "typescript").unwrap();
    assert_eq!(
        ts.globs,
        ["**/*.ts"],
        "globs survive through Roo's lead line"
    );
    assert_eq!(names(&roo.commands, |c| &c.name), ["deploy", "release"]);
    assert_eq!(
        names(&roo.skills, |s| &s.name),
        ["pdf-notes", "shared-notes"]
    );

    let again = Sandbox::empty();
    apply(&again, &roo, &["cline"], &again.env(Scope::User));
    let round = capture(&["cline"], &again.env(Scope::User)).profile;
    assert_eq!(names(&round.rules, |r| &r.name), ["style", "typescript"]);
    assert_eq!(
        round
            .rules
            .iter()
            .find(|r| r.name == "typescript")
            .unwrap()
            .globs,
        ["**/*.ts"]
    );
    assert_eq!(names(&round.commands, |c| &c.name), ["deploy", "release"]);
    assert_eq!(round.skills, roo.skills);
}

#[test]
fn applying_to_cline_and_roo_through_the_cli_writes_the_new_items_and_undo_removes_them() {
    let sb = Sandbox::empty();
    sb.cli().args(["init", "prof"]).assert().success();
    put(
        &sb,
        "prof/rules/ts.md",
        "---\nglobs:\n  - \"**/*.ts\"\n---\nStrict mode.\n",
    );
    put(&sb, "prof/commands/release.md", "Tag it.\n");
    put(
        &sb,
        "prof/skills/notes/SKILL.md",
        "---\nname: notes\ndescription: Take notes.\n---\n\nBody.\n",
    );
    sb.cli()
        .args(["apply", "prof", "--to", "cline,roo-code", "--yes"])
        .assert()
        .success();
    assert!(sb.read("home/.cline/rules/ts.md").contains("paths:"));
    assert!(sb.read("home/.roo/rules/ts.md").contains("`**/*.ts`"));
    assert!(sb.path("home/.cline/workflows/release.md").is_file());
    assert!(sb.path("home/.roo/commands/release.md").is_file());
    assert!(sb.path("home/.cline/skills/notes/SKILL.md").is_file());
    assert!(sb.path("home/.roo/skills/notes/SKILL.md").is_file());
    sb.cli().arg("undo").assert().success();
    assert!(!sb.path("home/.cline").exists());
    assert!(!sb.path("home/.roo").exists());
}
