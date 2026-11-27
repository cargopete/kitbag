//! Windsurf: MCP servers in `~/.codeium/windsurf/mcp_config.json`,
//! instructions in `global_rules.md` (user) or `AGENTS.md` (project), rules and
//! skills in a project's `.windsurf/`, and workflows (commands) in
//! `~/.codeium/windsurf/global_workflows/` and `.windsurf/workflows/`.

mod common;

use std::fs;

use common::*;
use kitbag::adapters::{Feature, Scope, SecretsMode, Support};
use kitbag::model::{Command, McpServer, Profile, Rule, RuleMode, Skill, SkillFile, Transport};
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
    // Rules have no user-level folder; commands go to global_workflows.
    assert!(p.notes.iter().any(|n| n.target == "windsurf"
        && n.message.contains("not supported")
        && n.message.contains("1 rules")
        && !n.message.contains("commands")));
    assert!(
        planned(
            &p,
            &sb.home()
                .join(".codeium/windsurf/global_workflows/review.md")
        )
        .is_some()
    );
    assert_eq!(p.changes.len(), 3, "{:?}", p.changes);
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
    // AGENTS.md, the rule file and the workflow file.
    assert_eq!(p.changes.len(), 3, "{:?}", p.changes);
    assert!(
        planned(&p, &sb.project().join("AGENTS.md"))
            .unwrap()
            .contains("Use British English.")
    );
    assert!(p.notes.iter().any(|n| n.target == "windsurf"
        && n.message.contains("no project-level MCP config")
        && n.message.contains("2 MCP server(s) not written")));
    // No MCP file, and nothing under .codeium.
    assert!(
        p.changes
            .iter()
            .all(|c| !c.path.to_string_lossy().contains(".codeium")
                && !c.path.to_string_lossy().contains("mcp"))
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
    // Windsurf's own rule and workflow files are new; AGENTS.md is left as it is.
    let agents = sb.project().join("AGENTS.md");
    let only_windsurf = plan(&prof, &["windsurf"], &env);
    assert!(only_windsurf.changes.iter().all(|c| c.path != agents));
    let both = plan(&prof, &["codex", "windsurf"], &env);
    assert!(both.changes.iter().all(|c| c.path != agents));
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

// ---- rules, workflows (commands) and skills

/// A sandbox with Windsurf's rules, workflows and skills (not part of the
/// shared fixtures, so the other tools' tests are not affected by them).
fn files_sandbox() -> Sandbox {
    let sb = Sandbox::empty();
    let root = fixtures().join("windsurf_files");
    copy_dir(&root.join("home"), &sb.home());
    copy_dir(&root.join("project"), &sb.project());
    sb
}

fn put(sb: &Sandbox, rel: &str, text: &str) {
    let path = sb.path(rel);
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, text).unwrap();
}

fn said(p: &kitbag::engine::Plan, what: &str) -> bool {
    p.notes
        .iter()
        .any(|n| n.target == "windsurf" && n.message.contains(what))
}

fn cap_said(r: &kitbag::engine::CaptureResult, what: &str) -> bool {
    r.notes.iter().any(|n| n.message.contains(what))
}

fn items() -> Profile {
    let mut p = Profile::new("team");
    p.rules.push(Rule {
        name: "always".into(),
        body: "Be brief.".into(),
        ..Default::default()
    });
    p.rules.push(Rule {
        name: "ts".into(),
        description: Some("TypeScript".into()),
        globs: vec!["src/**/*.ts".into(), "*.tsx".into()],
        body: "Strict mode.".into(),
        ..Default::default()
    });
    p.rules.push(Rule {
        name: "db".into(),
        description: Some("Database work".into()),
        always_apply: Some(false),
        body: "Use migrations.".into(),
        ..Default::default()
    });
    p.commands.push(Command {
        name: "release".into(),
        description: Some("Cut a release".into()),
        body: "Tag it, then publish.".into(),
        ..Default::default()
    });
    p.commands.push(Command {
        name: "plain".into(),
        body: "Just do it.".into(),
        ..Default::default()
    });
    p.skills.push(Skill {
        name: "deploy-notes".into(),
        files: vec![
            SkillFile {
                path: "SKILL.md".into(),
                content: b"---\nname: deploy-notes\ndescription: Notes on deploying.\n---\n\nDeploy body.\n"
                    .to_vec(),
                executable: false,
            },
            SkillFile {
                path: "scripts/run.sh".into(),
                content: b"echo run\n".to_vec(),
                executable: true,
            },
        ],
        ..Default::default()
    });
    p
}

#[test]
fn support_matrix_names_only_what_is_implemented() {
    let a = kitbag::adapters::get("windsurf").unwrap();
    let get = |scope, f| {
        a.support(scope)
            .into_iter()
            .find(|(x, _)| *x == f)
            .unwrap()
            .1
    };
    assert_eq!(get(Scope::Project, Feature::Rules), Support::Native);
    assert_eq!(get(Scope::Project, Feature::Skills), Support::Native);
    assert_eq!(get(Scope::Project, Feature::Commands), Support::Native);
    assert_eq!(get(Scope::User, Feature::Commands), Support::Native);
    assert_eq!(get(Scope::User, Feature::Rules), Support::None);
    assert_eq!(get(Scope::User, Feature::Skills), Support::None);
    for scope in [Scope::User, Scope::Project] {
        assert_eq!(get(scope, Feature::Agents), Support::None);
    }
}

#[test]
fn capture_project_reads_rules_workflows_and_skills_and_lists_what_it_leaves() {
    let sb = files_sandbox();
    let r = capture(&["windsurf"], &sb.env(Scope::Project));
    let p = &r.profile;
    let by = |n: &str| p.rules.iter().find(|r| r.name == n);

    let always = by("always").unwrap();
    assert_eq!(always.mode(), RuleMode::Always);
    assert_eq!(always.description.as_deref(), Some("House style"));
    assert_eq!(always.body, "Keep diffs small.");
    let ts = by("ts").unwrap();
    assert_eq!(ts.mode(), RuleMode::Globs);
    assert_eq!(ts.globs, ["src/**/*.ts", "*.tsx"]);
    let db = by("db").unwrap();
    assert_eq!(db.mode(), RuleMode::OnDemand);
    assert_eq!(db.description.as_deref(), Some("Database work"));
    // manual, no trigger, a glob trigger without globs: left behind, and said so.
    for gone in ["manual", "bare", "noglobs"] {
        assert!(by(gone).is_none(), "{gone}");
        assert!(
            cap_said(&r, &format!("rule '{gone}' was not captured")),
            "{gone}"
        );
    }
    assert!(cap_said(&r, "`manual`"));
    assert!(cap_said(&r, "no `trigger`"));
    assert_eq!(p.rules.len(), 3);

    assert_eq!(p.commands.len(), 1);
    assert_eq!(p.commands[0].name, "deploy");
    assert_eq!(p.commands[0].body, "1. Build.\n2. Ship.");
    assert_eq!(p.skills.len(), 1);
    let triage = &p.skills[0];
    assert_eq!(triage.name, "triage");
    assert!(triage.files.iter().any(|f| f.path == "checklist.md"));

    // What stays behind is listed: the nested rule, hooks.json, Devin Desktop's folder.
    assert!(cap_said(&r, "nested/deep.md"), "{:?}", r.notes);
    assert!(cap_said(&r, "hooks.json"));
    assert!(cap_said(&r, "dev.md") && cap_said(&r, "not Devin Desktop's .devin"));
    assert!(
        !r.profile
            .rules
            .iter()
            .any(|r| r.body.contains("Devin Desktop rule"))
    );
}

#[test]
fn capture_user_reads_global_workflows_and_lists_memories_it_leaves() {
    let sb = files_sandbox();
    let r = capture(&["windsurf"], &sb.env(Scope::User));
    assert_eq!(r.profile.commands.len(), 1);
    let c = &r.profile.commands[0];
    assert_eq!(c.name, "release");
    assert_eq!(c.description.as_deref(), Some("Cut a release"));
    assert_eq!(c.body, "1. Bump the version.\n2. Tag it.");
    assert_eq!(
        r.profile.instructions.as_deref(),
        Some("Answer in plain English.")
    );
    assert!(r.profile.rules.is_empty() && r.profile.skills.is_empty());
    assert!(cap_said(&r, "notes.txt"));
    assert!(cap_said(&r, "auto-1234.md"));
}

#[test]
fn apply_project_writes_the_documented_shapes_and_nothing_for_devin() {
    let sb = Sandbox::empty();
    let env = sb.env(Scope::Project);
    let prof = items();
    let p = apply(&sb, &prof, &["windsurf"], &env);

    assert_eq!(
        sb.read("project/.windsurf/rules/always.md"),
        "---\ntrigger: always_on\n---\n\nBe brief.\n"
    );
    assert_eq!(
        sb.read("project/.windsurf/rules/ts.md"),
        "---\ntrigger: glob\ndescription: TypeScript\nglobs: \"src/**/*.ts, *.tsx\"\n---\n\nStrict mode.\n"
    );
    assert_eq!(
        sb.read("project/.windsurf/rules/db.md"),
        "---\ntrigger: model_decision\ndescription: Database work\n---\n\nUse migrations.\n"
    );
    assert!(said(
        &p,
        "rule 'ts': Windsurf's docs show a single `globs` pattern"
    ));
    assert_eq!(
        sb.read("project/.windsurf/workflows/release.md"),
        "---\ndescription: Cut a release\n---\n\nTag it, then publish.\n"
    );
    assert_eq!(
        sb.read("project/.windsurf/workflows/plain.md"),
        "Just do it.\n"
    );
    assert!(
        sb.read("project/.windsurf/skills/deploy-notes/SKILL.md")
            .contains("Deploy body.")
    );
    assert_eq!(
        sb.read("project/.windsurf/skills/deploy-notes/scripts/run.sh"),
        "echo run\n"
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = fs::metadata(sb.path("project/.windsurf/skills/deploy-notes/scripts/run.sh"))
            .unwrap()
            .permissions()
            .mode();
        assert_ne!(mode & 0o111, 0, "the executable bit is kept");
    }
    // Devin Desktop's own files are never written.
    assert!(!sb.path("project/.devin").exists());
    assert!(!sb.path("home/.config/devin").exists());
    assert!(!sb.path("home/.devin").exists());

    // A second apply is up to date, and capture reads it all back.
    assert!(plan(&prof, &["windsurf"], &env).changes.is_empty());
    let back = capture(&["windsurf"], &env).profile;
    let by = |n: &str| back.rules.iter().find(|r| r.name == n).unwrap();
    assert_eq!(by("always").mode(), RuleMode::Always);
    assert_eq!(by("ts").globs, ["src/**/*.ts", "*.tsx"]);
    assert_eq!(by("ts").description.as_deref(), Some("TypeScript"));
    assert_eq!(by("db").mode(), RuleMode::OnDemand);
    assert_eq!(by("db").body, "Use migrations.");
    assert_eq!(back.commands.len(), 2);
    let rel = back.commands.iter().find(|c| c.name == "release").unwrap();
    assert_eq!(rel.description.as_deref(), Some("Cut a release"));
    assert_eq!(rel.body, "Tag it, then publish.");
    assert_eq!(back.skills.len(), 1);
    assert_eq!(back.skills[0].files.len(), 2);
}

#[test]
fn apply_is_additive_and_undo_removes_only_kitbags_files() {
    let sb = Sandbox::empty();
    let env = sb.env(Scope::Project);
    put(
        &sb,
        "project/.windsurf/rules/mine.md",
        "---\ntrigger: always_on\n---\n\nMine.\n",
    );
    put(&sb, "project/.windsurf/workflows/mine.md", "My workflow.\n");
    put(
        &sb,
        "project/.windsurf/skills/theirs/SKILL.md",
        "---\nname: theirs\ndescription: Not ours.\n---\n",
    );
    apply(&sb, &items(), &["windsurf"], &env);
    for kept in [
        "project/.windsurf/rules/mine.md",
        "project/.windsurf/workflows/mine.md",
        "project/.windsurf/skills/theirs/SKILL.md",
    ] {
        assert!(sb.path(kept).is_file(), "{kept}");
    }
    assert!(sb.path("project/.windsurf/rules/ts.md").is_file());
    Store::new(sb.state()).undo(None, false).unwrap();
    assert!(sb.path("project/.windsurf/rules/mine.md").is_file());
    assert_eq!(
        sb.read("project/.windsurf/workflows/mine.md"),
        "My workflow.\n"
    );
    assert!(
        sb.path("project/.windsurf/skills/theirs/SKILL.md")
            .is_file()
    );
    for gone in [
        "project/.windsurf/rules/ts.md",
        "project/.windsurf/rules/always.md",
        "project/.windsurf/workflows/release.md",
        "project/.windsurf/skills/deploy-notes",
    ] {
        assert!(!sb.path(gone).exists(), "{gone}");
    }
}

#[test]
fn apply_user_writes_global_workflows_and_says_rules_and_skills_are_not_carried() {
    let sb = Sandbox::empty();
    let env = sb.env(Scope::User);
    let prof = items();
    let p = apply(&sb, &prof, &["windsurf"], &env);
    assert_eq!(
        sb.read("home/.codeium/windsurf/global_workflows/release.md"),
        "---\ndescription: Cut a release\n---\n\nTag it, then publish.\n"
    );
    assert!(
        sb.path("home/.codeium/windsurf/global_workflows/plain.md")
            .is_file()
    );
    assert!(said(
        &p,
        "not supported by Windsurf at user scope, skipped: 3 rules, 1 skills"
    ));
    assert!(!sb.path("home/.codeium/windsurf/rules").exists());
    assert!(!sb.path("home/.codeium/windsurf/skills").exists());
    assert!(!sb.path("home/.devin").exists());
    assert!(!sb.path("home/.config/devin").exists());
    assert!(!sb.path("home/.windsurf").exists());
    assert!(plan(&prof, &["windsurf"], &env).changes.is_empty());
    Store::new(sb.state()).undo(None, false).unwrap();
    assert!(!sb.path("home/.codeium").exists());
}

#[test]
fn rules_commands_and_skills_honour_targets_and_disabled_on() {
    let sb = Sandbox::empty();
    let mut prof = items();
    prof.rules[0].targeting.disabled_on = vec!["windsurf".into()];
    prof.rules[1].targeting.targets = Some(vec!["cursor".into()]);
    prof.commands[0].targeting.disabled_on = vec!["windsurf".into()];
    prof.skills[0].targeting.targets = Some(vec!["cursor".into()]);
    let p = plan(&prof, &["windsurf"], &sb.env(Scope::Project));
    let has = |rel: &str| planned(&p, &sb.path(rel)).is_some();
    assert!(!has("project/.windsurf/rules/always.md"));
    assert!(!has("project/.windsurf/rules/ts.md"));
    assert!(has("project/.windsurf/rules/db.md"));
    assert!(!has("project/.windsurf/workflows/release.md"));
    assert!(has("project/.windsurf/workflows/plain.md"));
    assert!(!has("project/.windsurf/skills/deploy-notes/SKILL.md"));
}

#[test]
fn oversized_workflow_and_dropped_argument_hint_are_said() {
    let sb = Sandbox::empty();
    let mut prof = Profile::new("team");
    prof.commands.push(Command {
        name: "big".into(),
        body: "x".repeat(12_001),
        ..Default::default()
    });
    prof.commands.push(Command {
        name: "hinted".into(),
        argument_hint: Some("<issue>".into()),
        body: "Fix it.".into(),
        ..Default::default()
    });
    let p = plan(&prof, &["windsurf"], &sb.env(Scope::User));
    assert!(said(&p, "command 'big' is"), "{:?}", p.notes);
    assert!(said(&p, "12000"));
    assert!(said(&p, "command 'hinted'"));
    let hinted = planned(
        &p,
        &sb.path("home/.codeium/windsurf/global_workflows/hinted.md"),
    )
    .unwrap();
    assert_eq!(hinted, "Fix it.\n");
    prof.commands.remove(0);
    let p = plan(&prof, &["windsurf"], &sb.env(Scope::User));
    assert!(!said(&p, "12000"));
}

#[test]
fn rule_notes_for_on_demand_without_description_and_codex_folding() {
    let sb = Sandbox::empty();
    let mut prof = Profile::new("team");
    prof.rules.push(Rule {
        name: "db".into(),
        always_apply: Some(false),
        body: "Use migrations.".into(),
        ..Default::default()
    });
    let env = sb.env(Scope::Project);
    let p = plan(&prof, &["windsurf", "codex"], &env);
    assert!(said(&p, "rule 'db' is on-demand but has no description"));
    assert!(said(&p, "sees the rules twice"));
    assert!(!said(
        &plan(&prof, &["windsurf"], &env),
        "sees the rules twice"
    ));
}

#[test]
fn a_skill_also_in_devin_or_agents_folders_is_noted() {
    let sb = Sandbox::empty();
    put(
        &sb,
        "project/.agents/skills/deploy-notes/SKILL.md",
        "---\nname: deploy-notes\ndescription: Theirs.\n---\n",
    );
    let p = plan(&items(), &["windsurf"], &sb.env(Scope::Project));
    assert!(said(&p, "skill 'deploy-notes' also exists in"));
    assert!(
        !sb.path("project/.agents/skills/deploy-notes/scripts")
            .exists()
    );
}

#[test]
fn rule_and_skill_locations_are_reported_per_scope() {
    let sb = Sandbox::empty();
    let a = kitbag::adapters::get("windsurf").unwrap();
    let user: Vec<_> = a
        .locations(&sb.env(Scope::User))
        .into_iter()
        .map(|(k, _)| k)
        .collect();
    assert_eq!(user, ["mcp", "instructions", "commands"]);
    let project = a.locations(&sb.env(Scope::Project));
    let kinds: Vec<_> = project.iter().map(|(k, _)| *k).collect();
    assert_eq!(kinds, ["instructions", "rules", "commands", "skills"]);
    let rules = &project.iter().find(|(k, _)| *k == "rules").unwrap().1;
    assert!(rules.ends_with(std::path::Path::new(".windsurf").join("rules")));
}

#[cfg(unix)]
#[test]
fn a_rules_folder_that_links_outside_the_project_is_not_read() {
    let sb = Sandbox::empty();
    put(
        &sb,
        "outside/leak.md",
        "---\ntrigger: always_on\n---\n\nOutside the project.\n",
    );
    fs::create_dir_all(sb.path("project/.windsurf")).unwrap();
    std::os::unix::fs::symlink(sb.path("outside"), sb.path("project/.windsurf/rules")).unwrap();
    let r = capture(&["windsurf"], &sb.env(Scope::Project));
    assert!(r.profile.rules.is_empty(), "{:?}", r.profile.rules);
    assert!(!format!("{:?}", r.profile).contains("Outside the project"));
}

#[test]
fn apply_through_the_cli_writes_files_and_undo_removes_them() {
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
        .current_dir(sb.project())
        .args([
            "apply",
            "../prof",
            "--to",
            "windsurf",
            "--project",
            ".",
            "--yes",
        ])
        .assert()
        .success();
    assert!(
        sb.read("project/.windsurf/rules/ts.md")
            .contains("trigger: glob")
    );
    assert!(sb.path("project/.windsurf/workflows/release.md").is_file());
    assert!(sb.path("project/.windsurf/skills/notes/SKILL.md").is_file());
    sb.cli().arg("undo").assert().success();
    assert!(!sb.path("project/.windsurf").exists());
}
