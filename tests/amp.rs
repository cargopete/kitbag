//! Amp: MCP servers under `amp.mcpServers` in `settings.json[c]` (user and
//! workspace) and instructions in `AGENTS.md` (user: `~/.config/amp/AGENTS.md`).

mod common;

use std::fs;

use common::*;
use kitbag::adapters::{Os, Scope, SecretsMode};
use kitbag::model::{McpServer, Profile, Rule, Transport};
use kitbag::profile_io;
use kitbag::txn::Store;
use serde_json::{Value, json};

const SETTINGS: &str = "home/.config/amp/settings.json";
const PERSONAL: &str = "home/.config/amp/AGENTS.md";

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

/// A sandbox with the Amp fixtures (a user config dir and a workspace).
fn amp_sandbox() -> Sandbox {
    let sb = Sandbox::empty();
    copy_dir(&fixtures().join("amp/home"), &sb.home());
    copy_dir(&fixtures().join("amp/project"), &sb.project());
    sb
}

fn said(p: &kitbag::engine::Plan, what: &str) -> bool {
    p.notes
        .iter()
        .any(|n| n.target == "amp" && n.message.contains(what))
}

fn parse(text: &str) -> Value {
    kitbag::edit::json::parse_value(Some(text), std::path::Path::new("settings.json")).unwrap()
}

#[test]
fn capture_user_reads_jsonc_settings_and_amp_agents_md() {
    let sb = amp_sandbox();
    let r = capture(&["amp"], &sb.env(Scope::User));
    let p = &r.profile;
    let pw = &p.mcp_servers["playwright"];
    assert_eq!(pw.command.as_deref(), Some("npx"));
    assert_eq!(pw.args, ["-y", "@playwright/mcp@latest", "--headless"]);
    assert_eq!(pw.env["REGION"], "eu");
    assert_eq!(
        pw.env["API_KEY"], "${API_KEY}",
        "a literal key is redacted into a reference"
    );
    // ${VAR} stays a reference, in a URL and in a header.
    let sg = &p.mcp_servers["sourcegraph"];
    assert_eq!(sg.url.as_deref(), Some("${SRC_ENDPOINT}/.api/mcp/v1"));
    assert_eq!(sg.headers["Authorization"], "token ${SRC_ACCESS_TOKEN}");
    assert_eq!(sg.transport(), Transport::Http);
    assert_eq!(
        p.mcp_servers["linear"].url.as_deref(),
        Some("https://mcp.linear.app/sse")
    );
    // The Amp ID token never leaves an orb: that server is not carried.
    assert!(!p.mcp_servers.contains_key("orb-only"));
    let say = |what: &str| r.notes.iter().any(|n| n.message.contains(what));
    assert!(say("orb-only") && say("amp:id-token"));
    // Keys kitbag has no field for are named, not silently lost.
    assert!(say("includeTools not captured"));
    assert!(say("oauth not captured"));
    assert!(say("ampcode.com"));
    assert_eq!(
        p.instructions.as_deref(),
        Some("Answer in plain English.\n\nKeep diffs small.")
    );
    assert!(!r.redactions.is_empty());
    let all = format!("{:?}", r.profile);
    assert!(!all.contains("sk-FAKEplaywright"), "{all}");
}

#[test]
fn capture_project_reads_the_workspace_settings_and_agents_md() {
    let sb = amp_sandbox();
    let r = capture(&["amp"], &sb.env(Scope::Project));
    let p = &r.profile;
    // The workspace's own definitions, not merged with the user's: a
    // workspace overrides a user server of the same name in Amp.
    assert_eq!(
        p.mcp_servers["playwright"].args,
        ["-y", "@playwright/mcp@latest"]
    );
    assert_eq!(p.mcp_servers["project-db"].env["DB_URL"], "${DB_URL}");
    assert_eq!(p.mcp_servers.len(), 2);
    let i = p.instructions.as_deref().unwrap();
    assert!(i.contains("Run `pnpm test` before committing."));
    assert!(
        !i.contains("Claude-only"),
        "CLAUDE.md is ignored by Amp: {i}"
    );
    assert!(
        r.notes
            .iter()
            .any(|n| n.message.contains("CLAUDE.md not read"))
    );
}

#[test]
fn capture_project_falls_back_to_claude_md_like_amp() {
    let sb = amp_sandbox();
    fs::remove_file(sb.project().join("AGENTS.md")).unwrap();
    let r = capture(&["amp"], &sb.env(Scope::Project));
    assert_eq!(
        r.profile.instructions.as_deref(),
        Some("Claude-only notes.")
    );
    fs::write(sb.project().join("AGENT.md"), "Singular.\n").unwrap();
    let r = capture(&["amp"], &sb.env(Scope::Project));
    assert_eq!(r.profile.instructions.as_deref(), Some("Singular."));
}

#[test]
fn capture_reads_settings_json_when_both_spellings_exist_and_says_so() {
    let sb = amp_sandbox();
    fs::write(
        sb.path(SETTINGS),
        r#"{"amp.mcpServers": {"only-in-json": {"command": "x"}}}"#,
    )
    .unwrap();
    let r = capture(&["amp"], &sb.env(Scope::User));
    assert!(r.profile.mcp_servers.contains_key("only-in-json"));
    assert!(!r.profile.mcp_servers.contains_key("playwright"));
    assert!(r.notes.iter().any(|n| n.message.contains("which one wins")));
}

#[test]
fn capture_names_other_instructions_and_leaves_out_folded_rules() {
    let sb = Sandbox::empty();
    fs::create_dir_all(sb.home().join(".config/amp")).unwrap();
    fs::write(
        sb.home().join(".config/AGENTS.md"),
        "Shared across tools.\n",
    )
    .unwrap();
    apply(&sb, &sample(), &["codex"], &sb.env(Scope::Project));
    let r = capture(&["amp"], &sb.env(Scope::User));
    assert!(r.profile.instructions.is_none());
    assert!(
        r.notes
            .iter()
            .any(|n| n.message.contains("also read by Amp"))
    );
    let r = capture(&["amp"], &sb.env(Scope::Project));
    assert_eq!(
        r.profile.instructions.as_deref(),
        Some("Use British English.")
    );
    assert!(r.profile.rules.is_empty());
    assert!(r.notes.iter().any(|n| n.message.contains("(ts)")));
}

#[test]
fn detect_looks_for_amp_not_shared_files() {
    let sb = Sandbox::empty();
    let a = kitbag::adapters::get("amp").unwrap();
    for scope in [Scope::User, Scope::Project] {
        assert!(!a.detect(&sb.env(scope)));
    }
    fs::write(sb.project().join("AGENTS.md"), "x\n").unwrap();
    assert!(!a.detect(&sb.env(Scope::Project)), "AGENTS.md is shared");
    fs::create_dir_all(sb.home().join(".config/amp")).unwrap();
    assert!(a.detect(&sb.env(Scope::User)));
    fs::create_dir_all(sb.project().join(".amp")).unwrap();
    assert!(a.detect(&sb.env(Scope::Project)));
}

#[test]
fn config_dir_is_dot_config_amp_under_home_on_every_os() {
    let sb = Sandbox::empty();
    for os in [Os::Linux, Os::MacOs, Os::Windows] {
        let mut env = sb.env(Scope::User);
        env.os = os;
        // The docs name no variable that moves it, so XDG_CONFIG_HOME and
        // APPDATA are not followed.
        env.locations
            .insert("XDG_CONFIG_HOME".into(), "/x/cfg".into());
        env.locations.insert("APPDATA".into(), "C:/x".into());
        assert_eq!(
            kitbag::adapters::amp::config_dir(&env),
            sb.home().join(".config").join("amp"),
            "{os:?}"
        );
    }
}

#[test]
fn apply_user_keeps_references_and_writes_no_secret() {
    let sb = Sandbox::empty();
    let env = sb.env_with(
        Scope::User,
        &[
            ("GITHUB_TOKEN", "ghp_FAKEvalue123"),
            ("LINEAR_API_KEY", "lin_FAKEvalue456"),
        ],
    );
    let p = plan(&sample(), &["amp"], &env);
    let text = planned(&p, &sb.home().join(".config/amp/settings.json")).unwrap();
    let cfg = parse(&text);
    // Amp expands ${VAR} itself, so the value is never written.
    assert_eq!(
        cfg["amp.mcpServers"]["github"],
        json!({"command": "npx", "args": ["-y", "@modelcontextprotocol/server-github"], "env": {"GITHUB_TOKEN": "${GITHUB_TOKEN}"}})
    );
    assert_eq!(
        cfg["amp.mcpServers"]["linear"],
        json!({"url": "https://mcp.linear.app/mcp", "headers": {"Authorization": "Bearer ${LINEAR_API_KEY}"}})
    );
    assert!(!text.contains("ghp_FAKE") && !text.contains("lin_FAKE"));
    assert!(p.sensitive.is_empty());
    let md = planned(&p, &sb.home().join(".config/amp/AGENTS.md")).unwrap();
    assert!(md.contains("<!-- kitbag:begin team -->") && md.contains("Use British English."));
    assert!(!md.contains("kitbag:rule"), "rules are not supported");
    assert!(
        p.notes
            .iter()
            .any(|n| n.target == "amp" && n.message.contains("not supported"))
    );
    assert_eq!(p.changes.len(), 2, "{:?}", p.changes);

    // Also with --secrets never: the file is the same.
    let prof = sample();
    let mut o = opts(&prof);
    o.secrets = SecretsMode::Never;
    let never = kitbag::engine::plan(&prof, &targets(&["amp"]), &env, &o).unwrap();
    assert_eq!(
        planned(&never, &sb.home().join(".config/amp/settings.json")).unwrap(),
        text
    );
}

#[test]
fn apply_project_writes_workspace_settings_and_leaves_approval_to_amp() {
    let sb = Sandbox::empty();
    let env = sb.env(Scope::Project);
    let p = plan(&sample(), &["amp"], &env);
    let text = planned(&p, &sb.project().join(".amp/settings.json")).unwrap();
    let cfg = parse(&text);
    assert!(cfg["amp.mcpServers"]["github"].is_object());
    assert!(planned(&p, &sb.project().join("AGENTS.md")).is_some());
    assert!(said(&p, "amp mcp approve"), "{:?}", p.notes);
    // Nothing that approves, trusts or allows a server is written.
    let obj = cfg.as_object().unwrap();
    assert_eq!(obj.keys().collect::<Vec<_>>(), ["amp.mcpServers"]);
    for banned in ["trust", "approv", "permissions", "allow"] {
        assert!(!text.to_lowercase().contains(banned), "{banned}: {text}");
    }
    // Nothing outside the project.
    assert!(p.changes.iter().all(|c| c.path.starts_with(sb.project())));
}

#[test]
fn kitbag_leaves_a_workspaces_trust_settings_as_they_are() {
    let sb = amp_sandbox();
    let file = sb.project().join(".amp/settings.json");
    let before = parse(&fs::read_to_string(&file).unwrap());
    let mut prof = sample();
    prof.mcp_servers.insert(
        "project-db".into(),
        McpServer {
            command: Some("node".into()),
            args: vec!["tools/other.js".into()],
            ..Default::default()
        },
    );
    apply(&sb, &prof, &["amp"], &sb.env(Scope::Project));
    let after = parse(&fs::read_to_string(&file).unwrap());
    assert_eq!(
        after["amp.mcpTrustedServers"],
        before["amp.mcpTrustedServers"]
    );
    assert_eq!(
        after.as_object().unwrap().len(),
        before.as_object().unwrap().len()
    );
    // A server that changed what it runs gets no inherited keys; the rest of
    // the entry is the profile's.
    assert_eq!(
        after["amp.mcpServers"]["project-db"],
        json!({"command": "node", "args": ["tools/other.js"]})
    );
}

#[test]
fn apply_merges_into_jsonc_settings_and_keeps_the_rest() {
    let sb = amp_sandbox();
    let cfg = sb.home().join(".config/amp/settings.jsonc");
    let mut prof = sample();
    prof.mcp_servers.insert(
        "playwright".into(),
        McpServer {
            command: Some("npx".into()),
            args: vec![
                "-y".into(),
                "@playwright/mcp@latest".into(),
                "--headless".into(),
            ],
            env: [("API_KEY".to_string(), "${API_KEY}".to_string())]
                .into_iter()
                .collect(),
            ..Default::default()
        },
    );
    prof.mcp_servers.insert(
        "events".into(),
        McpServer {
            url: Some("https://events.example.com/sse".into()),
            transport: Some(Transport::Sse),
            ..Default::default()
        },
    );
    prof.mcp_servers.get_mut("github").unwrap().cwd = Some("tools".into());
    // The same URL as the user's own entry, so its `oauth` stays.
    prof.mcp_servers.get_mut("linear").unwrap().url = Some("https://mcp.linear.app/sse".into());
    prof.mcp_servers.insert(
        "off".into(),
        McpServer {
            command: Some("off-mcp".into()),
            disabled: true,
            ..Default::default()
        },
    );
    let p = plan(&prof, &["amp"], &sb.env(Scope::User));
    let text = planned(&p, &cfg).unwrap();
    assert!(text.starts_with("// My Amp settings") && text.contains("// keep"));
    assert!(
        planned(&p, &sb.home().join(".config/amp/settings.json")).is_none(),
        "the existing .jsonc is edited, no .json is created"
    );
    let out = parse(&text);
    assert_eq!(out["amp.notifications.enabled"], true);
    // The user's own servers stay, and keys kitbag doesn't manage on a server
    // it still runs the same way.
    assert_eq!(
        out["amp.mcpServers"]["sourcegraph"]["url"],
        "${SRC_ENDPOINT}/.api/mcp/v1"
    );
    assert_eq!(
        out["amp.mcpServers"]["playwright"]["includeTools"],
        json!(["browser_*"])
    );
    assert_eq!(
        out["amp.mcpServers"]["linear"]["oauth"],
        json!({"clientId": "abc"})
    );
    // The secret that was in the file stays the user's business; the plan
    // only replaces the entry kitbag owns with a reference.
    assert_eq!(
        out["amp.mcpServers"]["playwright"]["env"],
        json!({"API_KEY": "${API_KEY}"})
    );
    assert!(out["amp.mcpServers"]["events"].get("transport").is_none());
    assert_eq!(
        out["amp.mcpServers"]["events"],
        json!({"url": "https://events.example.com/sse"})
    );
    assert!(out["amp.mcpServers"]["github"].get("cwd").is_none());
    assert!(out["amp.mcpServers"].get("off").is_none());
    assert!(said(&p, "'cwd' is not supported by Amp"));
    assert!(said(&p, "events") && said(&p, "plain url"));
    assert!(!said(&p, "settings of the old entry"));
    assert!(said(&p, "'off' is disabled in the profile"));
}

#[test]
fn a_server_that_now_connects_elsewhere_loses_the_oauth_setting_it_had() {
    let sb = amp_sandbox();
    let mut prof = Profile::new("team");
    prof.mcp_servers.insert(
        "linear".into(),
        McpServer {
            url: Some("https://evil.example.com/sse".into()),
            ..Default::default()
        },
    );
    let p = plan(&prof, &["amp"], &sb.env(Scope::User));
    let cfg = planned(&p, &sb.home().join(".config/amp/settings.jsonc")).unwrap();
    let out = parse(&cfg);
    assert_eq!(
        out["amp.mcpServers"]["linear"],
        json!({"url": "https://evil.example.com/sse"})
    );
    assert!(said(&p, "settings of the old entry were not kept: oauth"));
}

#[test]
fn a_server_using_the_amp_id_token_is_not_written() {
    let sb = Sandbox::empty();
    let mut prof = sample();
    prof.mcp_servers.insert(
        "orb".into(),
        McpServer {
            url: Some("https://mcp.example.com/mcp".into()),
            headers: [(
                "Authorization".to_string(),
                "Bearer ${amp:id-token}".to_string(),
            )]
            .into_iter()
            .collect(),
            ..Default::default()
        },
    );
    let p = plan(&prof, &["amp"], &sb.env(Scope::User));
    let text = planned(&p, &sb.home().join(".config/amp/settings.json")).unwrap();
    assert!(
        !text.contains("orb") && !text.contains("id-token"),
        "{text}"
    );
    assert!(said(&p, "'orb' skipped") && said(&p, "amp:id-token"));
}

#[test]
fn apply_is_idempotent_and_undo_restores_the_files() {
    let sb = Sandbox::empty();
    let env = sb.env(Scope::User);
    let file = sb.home().join(".config/amp/AGENTS.md");
    fs::create_dir_all(file.parent().unwrap()).unwrap();
    fs::write(&file, "My own notes.\n").unwrap();
    let before_settings = "// mine\n{ \"amp.mcpServers\": { \"mine\": { \"command\": \"m\" } } }\n";
    fs::write(sb.path(SETTINGS), before_settings).unwrap();
    let prof = sample();
    apply(&sb, &prof, &["amp"], &env);
    let text = sb.read(PERSONAL);
    assert!(text.starts_with("My own notes.") && text.contains("Use British English."));
    let settings = sb.read(SETTINGS);
    assert!(settings.starts_with("// mine"));
    assert_eq!(
        parse(&settings)["amp.mcpServers"]["mine"],
        json!({"command": "m"})
    );
    assert!(plan(&prof, &["amp"], &env).changes.is_empty());
    Store::new(sb.state()).undo(None, false).unwrap();
    assert_eq!(sb.read(PERSONAL), "My own notes.\n");
    assert_eq!(sb.read(SETTINGS), before_settings);
}

#[test]
fn undo_removes_files_apply_created() {
    let sb = Sandbox::empty();
    apply(&sb, &sample(), &["amp"], &sb.env(Scope::User));
    assert!(sb.path(SETTINGS).exists() && sb.path(PERSONAL).exists());
    Store::new(sb.state()).undo(None, false).unwrap();
    assert!(!sb.path(SETTINGS).exists());
    assert!(!sb.path(PERSONAL).exists());
}

#[test]
fn workspace_and_user_servers_of_the_same_name_are_called_out() {
    let sb = amp_sandbox();
    let mut prof = Profile::new("team");
    prof.mcp_servers.insert(
        "playwright".into(),
        McpServer {
            command: Some("npx".into()),
            args: vec!["-y".into(), "@playwright/mcp@latest".into()],
            ..Default::default()
        },
    );
    let at_project = plan(&prof, &["amp"], &sb.env(Scope::Project));
    assert!(
        said(
            &at_project,
            "overrides the server of the same name in your user settings"
        ),
        "{:?}",
        at_project.notes
    );
    let at_user = plan(&prof, &["amp"], &sb.env(Scope::User));
    assert!(
        said(
            &at_user,
            "which overrides these user settings in that workspace"
        ),
        "{:?}",
        at_user.notes
    );
    prof.mcp_servers.clear();
    prof.mcp_servers.insert(
        "unrelated".into(),
        McpServer {
            command: Some("x".into()),
            ..Default::default()
        },
    );
    let p = plan(&prof, &["amp"], &sb.env(Scope::Project));
    assert!(!said(&p, "overrides"));
}

/// Amp reads the first of AGENTS.md, AGENT.md, CLAUDE.md in a directory, so
/// kitbag writes into that one and never creates an AGENTS.md that would hide
/// the others from Amp.
#[test]
fn project_instructions_go_to_the_file_amp_reads() {
    let prof = sample();
    let sb = Sandbox::empty();
    let p = plan(&prof, &["amp"], &sb.env(Scope::Project));
    assert!(planned(&p, &sb.project().join("AGENTS.md")).is_some());
    assert!(!said(&p, "Amp reads only"));

    for fallback in ["AGENT.md", "CLAUDE.md"] {
        let sb = Sandbox::empty();
        let env = sb.env(Scope::Project);
        fs::write(sb.project().join(fallback), "Mine.\n").unwrap();
        let p = apply(&sb, &prof, &["amp"], &env);
        let text = sb.read(&format!("project/{fallback}"));
        assert!(text.starts_with("Mine.") && text.contains("Use British English."));
        assert!(!sb.project().join("AGENTS.md").exists(), "{fallback}");
        assert!(said(&p, &format!("Amp reads only {fallback}")));
        assert!(plan(&prof, &["amp"], &env).changes.is_empty());
        let back = capture(&["amp"], &env).profile;
        assert_eq!(
            back.instructions.as_deref().map(str::trim),
            Some("Mine.\n\nUse British English.")
        );
    }

    // With AGENTS.md there, it is the file, and the others are left alone.
    let sb = Sandbox::empty();
    fs::write(sb.project().join("AGENTS.md"), "Shared.\n").unwrap();
    fs::write(sb.project().join("CLAUDE.md"), "Claude.\n").unwrap();
    apply(&sb, &prof, &["amp"], &sb.env(Scope::Project));
    assert!(
        sb.read("project/AGENTS.md")
            .contains("Use British English.")
    );
    assert_eq!(sb.read("project/CLAUDE.md"), "Claude.\n");
}

#[test]
fn amp_and_claude_code_share_claude_md_when_there_is_no_agents_md() {
    let prof = sample();
    let sb = Sandbox::empty();
    let env = sb.env(Scope::Project);
    let p = apply(&sb, &prof, &["claude-code", "amp"], &env);
    let claude = sb.project().join("CLAUDE.md");
    assert!(p.changes.iter().filter(|c| c.path == claude).count() <= 1);
    assert!(!sb.project().join("AGENTS.md").exists());
    let text = sb.read("project/CLAUDE.md");
    assert_eq!(
        text.matches("<!-- kitbag:begin team -->").count(),
        1,
        "{text}"
    );
    assert!(
        plan(&prof, &["claude-code", "amp"], &env)
            .changes
            .is_empty()
    );
}

/// In a project with no earlier file, AGENTS.md is shared with Codex, Cursor,
/// Antigravity, opencode, Grok Build, Windsurf and Zed: one managed block, in
/// any order, in one plan or several.
#[test]
fn amp_shares_agents_md_with_the_other_targets() {
    let count = |text: &str| text.matches("<!-- kitbag:begin team -->").count();
    let prof = sample();
    let others = [
        "codex",
        "cursor",
        "antigravity",
        "opencode",
        "grok-build",
        "windsurf",
        "hermes",
    ];

    let sb = Sandbox::empty();
    let env = sb.env(Scope::Project);
    fs::write(sb.project().join("AGENTS.md"), "My own notes.\n").unwrap();
    let mut together = others.to_vec();
    together.push("amp");
    let p = apply(&sb, &prof, &together, &env);
    let agents = sb.project().join("AGENTS.md");
    assert_eq!(p.changes.iter().filter(|c| c.path == agents).count(), 1);
    let text = sb.read("project/AGENTS.md");
    assert_eq!(count(&text), 1, "{text}");
    assert_eq!(text.matches("Use British English.").count(), 1, "{text}");
    assert!(text.starts_with("My own notes."));
    assert!(plan(&prof, &["amp"], &env).changes.is_empty());
    assert!(plan(&prof, &together, &env).changes.is_empty());

    for order in [["amp", "codex"], ["codex", "amp"]] {
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
fn amp_to_claude_code_and_back() {
    let src = amp_sandbox();
    let captured = capture(&["amp"], &src.env(Scope::User)).profile;
    let dir = src.path("profile");
    profile_io::save(&captured, &dir, false).unwrap();
    let loaded = profile_io::load(&dir).unwrap();
    assert_eq!(loaded, captured, "profile survives save + load unchanged");

    let dst = Sandbox::empty();
    apply(&dst, &loaded, &["claude-code"], &dst.env(Scope::User));
    let back = capture(&["claude-code"], &dst.env(Scope::User)).profile;
    for (name, s) in &loaded.mcp_servers {
        assert_eq!(&back.mcp_servers[name], s, "server {name}");
    }
    assert_eq!(back.instructions, loaded.instructions);

    // And into Amp in a clean home: the same profile comes back.
    let again = Sandbox::empty();
    apply(&again, &loaded, &["amp"], &again.env(Scope::User));
    let round = capture(&["amp"], &again.env(Scope::User)).profile;
    assert_eq!(round.instructions, loaded.instructions);
    assert_eq!(round.mcp_servers, loaded.mcp_servers);
}

#[test]
fn claude_code_to_amp_user() {
    let src = Sandbox::fixtures();
    let profile = capture(&["claude-code"], &src.env(Scope::User)).profile;
    let dst = Sandbox::empty();
    apply(&dst, &profile, &["amp"], &dst.env(Scope::User));
    let back = capture(&["amp"], &dst.env(Scope::User)).profile;
    for (name, s) in &profile.mcp_servers {
        let mut want = s.clone();
        want.cwd = None;
        want.disabled = false;
        if want.transport() == Transport::Sse {
            // Written as a plain url; Amp has no transport key.
            want.transport = None;
        }
        if s.disabled {
            assert!(!back.mcp_servers.contains_key(name), "{name}");
            continue;
        }
        assert_eq!(&back.mcp_servers[name], &want, "server {name}");
    }
    assert_eq!(back.instructions, profile.instructions);
}

#[test]
fn apply_to_amp_then_undo_through_the_cli() {
    let sb = Sandbox::empty();
    sb.cli().args(["init", "prof"]).assert().success();
    fs::write(
        sb.path("prof/kitbag.toml"),
        "kitbag = 1\nname = \"prof\"\n\n[mcp_servers.docs]\nurl = \"https://docs.example.com/mcp\"\nheaders = { Authorization = \"Bearer ${DOCS_TOKEN}\" }\n",
    )
    .unwrap();
    sb.cli()
        .args(["apply", "prof", "--to", "amp", "--yes"])
        .env("DOCS_TOKEN", "fake-token-value")
        .assert()
        .success();
    // --home moves Amp's config dir, on every OS.
    let cfg = sb.json(SETTINGS);
    assert_eq!(
        cfg["amp.mcpServers"]["docs"],
        json!({"url": "https://docs.example.com/mcp", "headers": {"Authorization": "Bearer ${DOCS_TOKEN}"}})
    );
    assert!(!sb.read(SETTINGS).contains("fake-token-value"));
    assert!(
        sb.read(PERSONAL)
            .contains("Write instructions every agent should follow")
    );
    sb.cli().arg("undo").assert().success();
    assert!(
        !sb.home().join(".config/amp").exists(),
        "undo removes what apply created"
    );
}

#[test]
fn list_targets_names_amp() {
    let sb = Sandbox::empty();
    let out = sb.cli().args(["list-targets", "--json"]).output().unwrap();
    let v: Value = serde_json::from_slice(&out.stdout).unwrap();
    let amp = v
        .as_array()
        .unwrap()
        .iter()
        .find(|t| t["id"] == "amp")
        .unwrap();
    assert_eq!(amp["support"]["mcp"], "native");
    assert_eq!(amp["support"]["instructions"], "native");
    assert_eq!(amp["support"]["skills"], "-");
}
