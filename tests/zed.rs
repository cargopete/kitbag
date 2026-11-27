//! Zed: MCP servers in `context_servers` of `settings.json`, instructions in
//! `AGENTS.md` (user) or the first of Zed's project instruction files, and
//! skills in `~/.agents/skills` and a project's `.agents/skills`.

mod common;

use std::fs;

use common::*;
use kitbag::adapters::{Os, Scope, SecretsMode};
use kitbag::model::{Command, McpServer, Profile, Rule, Skill, SkillFile, Transport};
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
            .any(|n| n.message.contains("AGENTS.md") && n.message.contains("not read"))
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
    assert!(r.notes.iter().any(|n| n.message.contains("AGENTS.md")
        && n.message.contains("not read")
        && n.message.contains(".rules")));
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

/// User-scope Zed paths relative to the sandbox root, for CLI tests.
/// The CLI uses the host OS (`APPDATA` is ignored under `--home`).
fn zed_cli_settings() -> &'static str {
    if cfg!(windows) {
        "home/AppData/Roaming/Zed/settings.json"
    } else {
        "home/.config/zed/settings.json"
    }
}

fn zed_cli_agents() -> &'static str {
    if cfg!(windows) {
        "home/AppData/Roaming/Zed/AGENTS.md"
    } else {
        "home/.config/zed/AGENTS.md"
    }
}

fn zed_cli_dir(sb: &Sandbox) -> std::path::PathBuf {
    if cfg!(windows) {
        sb.home().join("AppData").join("Roaming").join("Zed")
    } else {
        sb.home().join(".config").join("zed")
    }
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
    let cfg = sb.json(zed_cli_settings());
    assert_eq!(
        cfg["context_servers"]["docs"],
        json!({"url": "https://docs.example.com/mcp", "headers": {"Authorization": "Bearer fake-token-value"}})
    );
    assert!(
        sb.read(zed_cli_agents())
            .contains("Write instructions every agent should follow")
    );
    sb.cli().arg("undo").assert().success();
    assert!(
        !zed_cli_dir(&sb).exists(),
        "undo removes what apply created"
    );
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

fn warned(p: &kitbag::engine::Plan, what: &str) -> bool {
    p.notes.iter().any(|n| {
        n.target == "zed" && n.level == kitbag::adapters::Level::Warn && n.message.contains(what)
    })
}

#[test]
fn capture_reads_zed_skill_folders_and_says_what_it_leaves() {
    let sb = Sandbox::empty();
    put(
        &sb,
        "home/.agents/skills/mine/SKILL.md",
        "---\nname: mine\ndescription: Mine.\n---\n\nUser skill.\n",
    );
    put(&sb, "home/.agents/skills/mine/scripts/run.sh", "echo hi\n");
    // Folders Zed's docs don't name are not read.
    put(
        &sb,
        "home/.claude/skills/claude-only/SKILL.md",
        "---\nname: claude-only\ndescription: x\n---\n",
    );
    put(
        &sb,
        "home/.config/zed/skills/zed-config/SKILL.md",
        "---\nname: zed-config\ndescription: x\n---\n",
    );
    put(
        &sb,
        "project/.agents/skills/proj/SKILL.md",
        "---\nname: proj\ndescription: Project.\n---\n\nProject skill.\n",
    );
    put(
        &sb,
        "project/.claude/skills/claude-proj/SKILL.md",
        "---\nname: claude-proj\ndescription: x\n---\n",
    );

    let user = capture(&["zed"], &sb.env(Scope::User));
    assert_eq!(names(&user.profile), ["mine"]);
    let files: Vec<&str> = user.profile.skills[0]
        .files
        .iter()
        .map(|f| f.path.as_str())
        .collect();
    assert!(files.contains(&"scripts/run.sh"), "{files:?}");
    assert!(
        user.notes
            .iter()
            .any(|n| n.message.contains("Rules Library") && n.message.contains("never reads")),
        "capture names the Rules Library it leaves"
    );
    let project = capture(&["zed"], &sb.env(Scope::Project));
    assert_eq!(names(&project.profile), ["proj"]);
    assert!(
        !project
            .notes
            .iter()
            .any(|n| n.message.contains("Rules Library"))
    );
}

#[test]
fn apply_user_writes_skills_to_agents_skills_and_undo_removes_only_those() {
    let sb = Sandbox::empty();
    let env = sb.env(Scope::User);
    put(
        &sb,
        "home/.agents/skills/theirs/SKILL.md",
        "---\nname: theirs\ndescription: Not ours.\n---\n",
    );
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
    let p = apply(&sb, &prof, &["zed"], &env);

    let main = sb.read("home/.agents/skills/deploy-notes/SKILL.md");
    assert!(
        main.contains("name: deploy-notes") && main.contains("Body of deploy-notes."),
        "{main}"
    );
    assert_eq!(
        sb.read("home/.agents/skills/deploy-notes/scripts/run.sh"),
        "echo run\n"
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = fs::metadata(sb.path("home/.agents/skills/deploy-notes/scripts/run.sh"))
            .unwrap()
            .permissions()
            .mode();
        assert_ne!(mode & 0o111, 0, "the executable bit is kept");
    }
    // Nowhere else, and rules and commands are said to be skipped.
    assert!(!sb.path("home/.claude").exists());
    assert!(!sb.path("home/.config/zed/skills").exists());
    assert!(!sb.path("home/.config/zed/rules").exists());
    assert!(!sb.path("home/.config/zed/commands").exists());
    assert!(said(
        &p,
        "not supported by Zed at user scope, skipped: 1 rules, 1 commands"
    ));

    // A second apply is up to date, and capture reads it back with `theirs`.
    assert!(plan(&prof, &["zed"], &env).changes.is_empty());
    assert_eq!(
        names(&capture(&["zed"], &env).profile),
        ["deploy-notes", "theirs"]
    );

    Store::new(sb.state()).undo(None, false).unwrap();
    assert!(!sb.path("home/.agents/skills/deploy-notes").exists());
    assert!(
        sb.path("home/.agents/skills/theirs/SKILL.md").is_file(),
        "their skill stays"
    );
}

#[test]
fn apply_project_writes_agents_skills_and_notes_the_trust_prompt() {
    let sb = Sandbox::empty();
    let env = sb.env(Scope::Project);
    let prof = with_skill("deploy-notes");
    let p = apply(&sb, &prof, &["zed"], &env);
    assert!(
        sb.path("project/.agents/skills/deploy-notes/SKILL.md")
            .is_file()
    );
    assert!(said(&p, "only in a trusted worktree"));
    assert!(plan(&prof, &["zed"], &env).changes.is_empty());
    // Nothing is written to the other tools' skill folders.
    assert!(!sb.path("project/.claude").exists());
}

#[test]
fn apply_keeps_a_differing_skill_with_no_overwrite_and_adds_missing_frontmatter() {
    let sb = Sandbox::empty();
    let env = sb.env(Scope::User);
    put(
        &sb,
        "home/.agents/skills/deploy-notes/SKILL.md",
        "---\nname: deploy-notes\ndescription: Mine.\n---\n\nMy body.\n",
    );
    let prof = with_skill("deploy-notes");
    let mut o = opts(&prof);
    o.overwrite = false;
    let p = kitbag::engine::plan(&prof, &targets(&["zed"]), &env, &o).unwrap();
    assert!(warned(&p, "kept existing skill 'deploy-notes'"));
    assert!(planned(&p, &sb.path("home/.agents/skills/deploy-notes/SKILL.md")).is_none());

    // A SKILL.md with no frontmatter gets the name and description Zed requires.
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
    apply(&sb, &bare, &["zed"], &env);
    let text = sb.read("home/.agents/skills/bare/SKILL.md");
    assert!(text.starts_with("---\nname: bare\ndescription:"), "{text}");
}

#[test]
fn apply_skips_skill_names_zed_cannot_load_and_warns_about_the_rest() {
    let sb = Sandbox::empty();
    let env = sb.env(Scope::User);
    let mut prof = Profile::new("team");
    prof.skills.push(skill("Deploy_Notes", "Upper case."));
    prof.skills.push(skill("double--hyphen", "Two hyphens."));
    prof.skills.push(skill("-lead", "Leading hyphen."));
    prof.skills.push(skill("fine-one", &"x".repeat(1100)));
    let mut mismatch = skill("mismatch", "Name differs.");
    mismatch.files[0].content = b"---\nname: other\ndescription: Name differs.\n---\n".to_vec();
    prof.skills.push(mismatch);
    let p = apply(&sb, &prof, &["zed"], &env);
    for n in ["Deploy_Notes", "double--hyphen", "-lead"] {
        assert!(warned(&p, &format!("skill '{n}' skipped")), "{n}");
        assert!(
            !sb.path(&format!("home/.agents/skills/{n}")).exists(),
            "{n}"
        );
    }
    assert!(sb.path("home/.agents/skills/fine-one/SKILL.md").is_file());
    assert!(warned(&p, "skill 'fine-one': its description is over 1024"));
    assert!(warned(&p, "the name in its SKILL.md is 'other'"));
}

#[test]
fn skills_honour_targets_and_disabled_on() {
    let sb = Sandbox::empty();
    let env = sb.env(Scope::User);
    let mut prof = Profile::new("team");
    let mut only_cursor = skill("only-cursor", "Cursor only.");
    only_cursor.targeting.targets = Some(vec!["cursor".into()]);
    let mut not_zed = skill("not-zed", "Not for Zed.");
    not_zed.targeting.disabled_on = vec!["zed".into()];
    prof.skills
        .extend([only_cursor, not_zed, skill("everywhere", "All.")]);
    let p = plan(&prof, &["zed"], &env);
    let has =
        |n: &str| planned(&p, &sb.path(&format!("home/.agents/skills/{n}/SKILL.md"))).is_some();
    assert!(has("everywhere") && !has("only-cursor") && !has("not-zed"));
}

#[test]
fn zed_and_amp_write_the_same_project_skill_once() {
    let prof = with_skill("deploy-notes");
    for order in [["zed", "amp"], ["amp", "zed"]] {
        let sb = Sandbox::empty();
        let env = sb.env(Scope::Project);
        let p = apply(&sb, &prof, &order, &env);
        let file = sb.project().join(".agents/skills/deploy-notes/SKILL.md");
        assert_eq!(
            p.changes.iter().filter(|c| c.path == file).count(),
            1,
            "{order:?}"
        );
        assert!(plan(&prof, &order, &env).changes.is_empty(), "{order:?}");
    }
}

#[test]
fn skills_survive_zed_to_claude_code_and_back() {
    let src = Sandbox::empty();
    let prof = with_skill("deploy-notes");
    apply(&src, &prof, &["zed"], &src.env(Scope::User));
    let captured = capture(&["zed"], &src.env(Scope::User)).profile;
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
    apply(&again, &loaded, &["zed"], &again.env(Scope::User));
    assert_eq!(
        capture(&["zed"], &again.env(Scope::User)).profile.skills,
        captured.skills
    );
}

#[test]
fn apply_skills_to_zed_through_the_cli_then_remove_one_when_disabled() {
    let sb = Sandbox::empty();
    sb.cli().args(["init", "prof"]).assert().success();
    put(
        &sb,
        "prof/skills/notes/SKILL.md",
        "---\nname: notes\ndescription: Takes notes.\n---\n\nNotes body.\n",
    );
    sb.cli()
        .args(["apply", "prof", "--to", "zed", "--yes"])
        .assert()
        .success();
    assert!(
        sb.read("home/.agents/skills/notes/SKILL.md")
            .contains("Notes body.")
    );

    // Disabling it for Zed removes the folder kitbag wrote.
    put(
        &sb,
        "prof/skills/notes/SKILL.md",
        "---\nname: notes\ndescription: Takes notes.\ndisabled_on: [zed]\n---\n\nNotes body.\n",
    );
    sb.cli()
        .args(["apply", "prof", "--to", "zed", "--yes"])
        .assert()
        .success();
    assert!(!sb.path("home/.agents/skills/notes").exists());
    sb.cli().arg("undo").assert().success();
    assert!(sb.path("home/.agents/skills/notes/SKILL.md").is_file());
    sb.cli().arg("undo").assert().success();
    assert!(!sb.path("home/.agents/skills/notes").exists());
}

#[test]
fn zed_warns_when_amp_would_write_a_different_copy_of_a_shared_project_skill() {
    let mut prof = with_skill("deploy-notes");
    prof.skills[0].files[0].content =
        b"---\nname: deploy-notes\ndescription: Does things.\nmcpServers:\n  x:\n    command: npx\n---\n\nBody.\n"
            .to_vec();
    let sb = Sandbox::empty();
    let both = plan(&prof, &["zed", "amp"], &sb.env(Scope::Project));
    assert!(warned(&both, "Amp and Zed share .agents/skills"));
    let alone = plan(&prof, &["zed"], &sb.env(Scope::Project));
    assert!(!warned(&alone, "Amp and Zed share"));
}

/// Zed's list ends `AGENTS.md`, `CLAUDE.md`, `GEMINI.md`: a project with only
/// one of the last two gets the block there, `AGENTS.md` still wins over both,
/// and with none of the files `AGENTS.md` is created.
#[test]
fn project_instructions_consider_claude_md_and_gemini_md() {
    let prof = sample();
    for only in ["CLAUDE.md", "GEMINI.md"] {
        let sb = Sandbox::empty();
        let env = sb.env(Scope::Project);
        fs::write(sb.project().join(only), "Mine.\n").unwrap();
        let p = apply(&sb, &prof, &["zed"], &env);
        let text = sb.read(&format!("project/{only}"));
        assert!(text.starts_with("Mine.") && text.contains("Use British English."));
        assert!(!sb.project().join("AGENTS.md").exists(), "{only}");
        assert!(said(&p, &format!("go to {only} rather than AGENTS.md")));
        assert!(plan(&prof, &["zed"], &env).changes.is_empty());
    }

    // CLAUDE.md comes before GEMINI.md.
    let sb = Sandbox::empty();
    let env = sb.env(Scope::Project);
    fs::write(sb.project().join("CLAUDE.md"), "C.\n").unwrap();
    fs::write(sb.project().join("GEMINI.md"), "G.\n").unwrap();
    apply(&sb, &prof, &["zed"], &env);
    assert!(
        sb.read("project/CLAUDE.md")
            .contains("Use British English.")
    );
    assert_eq!(sb.read("project/GEMINI.md"), "G.\n");

    // AGENTS.md wins over both.
    let sb = Sandbox::empty();
    let env = sb.env(Scope::Project);
    for f in ["AGENTS.md", "CLAUDE.md", "GEMINI.md"] {
        fs::write(sb.project().join(f), "Mine.\n").unwrap();
    }
    apply(&sb, &prof, &["zed"], &env);
    assert!(
        sb.read("project/AGENTS.md")
            .contains("Use British English.")
    );
    assert_eq!(sb.read("project/CLAUDE.md"), "Mine.\n");
    assert_eq!(sb.read("project/GEMINI.md"), "Mine.\n");

    // None of them: AGENTS.md is created.
    let sb = Sandbox::empty();
    let env = sb.env(Scope::Project);
    apply(&sb, &prof, &["zed"], &env);
    assert!(
        sb.read("project/AGENTS.md")
            .contains("Use British English.")
    );
    assert!(!sb.project().join("CLAUDE.md").exists());
    assert!(!sb.project().join("GEMINI.md").exists());
}

#[test]
fn capture_reads_claude_md_or_gemini_md_when_zed_does() {
    for only in ["CLAUDE.md", "GEMINI.md"] {
        let sb = Sandbox::empty();
        let env = sb.env(Scope::Project);
        fs::write(sb.project().join(only), "Only here.\n").unwrap();
        let r = capture(&["zed"], &env);
        assert_eq!(
            r.profile.instructions.as_deref(),
            Some("Only here."),
            "{only}"
        );
        assert!(!r.notes.iter().any(|n| n.message.contains("not read")));
    }

    // CLAUDE.md is read, GEMINI.md is named as ignored.
    let sb = Sandbox::empty();
    let env = sb.env(Scope::Project);
    fs::write(sb.project().join("CLAUDE.md"), "From Claude.\n").unwrap();
    fs::write(sb.project().join("GEMINI.md"), "From Gemini.\n").unwrap();
    let r = capture(&["zed"], &env);
    assert_eq!(r.profile.instructions.as_deref(), Some("From Claude."));
    assert!(
        r.notes
            .iter()
            .any(|n| n.message.contains("GEMINI.md not read"))
    );

    // AGENTS.md beats both, and the other two are named.
    fs::write(sb.project().join("AGENTS.md"), "From agents.\n").unwrap();
    let r = capture(&["zed"], &env);
    assert_eq!(r.profile.instructions.as_deref(), Some("From agents."));
    assert!(
        r.notes
            .iter()
            .any(|n| n.message.contains("CLAUDE.md, GEMINI.md not read"))
    );
}

/// When the file Zed reads is CLAUDE.md or GEMINI.md and the tool that owns it
/// is a target too, the file gets one block, in any order.
#[test]
fn zed_shares_claude_md_and_gemini_md_with_their_tools() {
    let prof = sample();
    for (file, tool) in [("CLAUDE.md", "claude-code"), ("GEMINI.md", "gemini-cli")] {
        let sb = Sandbox::empty();
        let env = sb.env(Scope::Project);
        fs::write(sb.project().join(file), "Mine.\n").unwrap();
        for order in [["zed", tool], [tool, "zed"]] {
            let p = apply(&sb, &prof, &order, &env);
            let path = sb.project().join(file);
            assert!(
                p.changes.iter().filter(|c| c.path == path).count() <= 1,
                "{file} {order:?}"
            );
            let text = sb.read(&format!("project/{file}"));
            assert_eq!(
                text.matches("<!-- kitbag:begin team -->").count(),
                1,
                "{file} {order:?}: {text}"
            );
            assert_eq!(text.matches("Use British English.").count(), 1, "{file}");
            assert!(
                plan(&prof, &order, &env).changes.is_empty(),
                "{file} {order:?}"
            );
        }
    }
}
