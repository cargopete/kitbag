//! Continue: MCP servers and instructions in `~/.continue/config.yaml`, MCP
//! block files and rules in a project's `.continue/`.

mod common;

use std::fs;

use common::*;
use kitbag::adapters::{Scope, SecretsMode};
use kitbag::engine;
use kitbag::model::{McpServer, Profile, Rule, Transport};
use serde_json::{Value, json};

const CONFIG: &str = "home/.continue/config.yaml";
const KITBAG_YAML: &str = "project/.continue/mcpServers/kitbag.yaml";

/// A sandbox holding the Continue fixtures (and nothing else).
fn sandbox() -> Sandbox {
    let sb = Sandbox::empty();
    let from = fixtures().join("continue");
    copy_dir(&from.join("home"), &sb.home());
    copy_dir(&from.join("project"), &sb.project());
    // A checkout with autocrlf has CRLF fixtures; the tests compare text.
    for root in [sb.home(), sb.project()] {
        for rel in kitbag::plan_fs::walk_files(&root, 100).unwrap() {
            let path = kitbag::plan_fs::join_rel(&root, &rel);
            let text = fs::read_to_string(&path).unwrap();
            fs::write(&path, text.replace("\r\n", "\n")).unwrap();
        }
    }
    sb
}

/// A fixture file as the sandbox has it.
fn fixture(rel: &str) -> String {
    fs::read_to_string(fixtures().join("continue").join(rel))
        .unwrap()
        .replace("\r\n", "\n")
}

fn yaml(text: &str) -> Value {
    serde_norway::from_str(text).unwrap()
}

fn said(notes: &[kitbag::adapters::Note], what: &str) -> bool {
    notes.iter().any(|n| n.message.contains(what))
}

fn server(command: &str, args: &[&str]) -> McpServer {
    McpServer {
        command: Some(command.into()),
        args: args.iter().map(|a| a.to_string()).collect(),
        ..Default::default()
    }
}

fn profile() -> Profile {
    let mut p = Profile::new("team");
    p.mcp_servers.insert(
        "github".into(),
        McpServer {
            env: [("GITHUB_TOKEN".to_string(), "${GITHUB_TOKEN}".to_string())]
                .into_iter()
                .collect(),
            ..server("npx", &["-y", "@modelcontextprotocol/server-github"])
        },
    );
    p.mcp_servers.insert(
        "linear".into(),
        McpServer {
            url: Some("https://mcp.linear.app/mcp".into()),
            headers: [(
                "Authorization".to_string(),
                "Bearer ${LINEAR_KEY}".to_string(),
            )]
            .into_iter()
            .collect(),
            ..Default::default()
        },
    );
    p.instructions = Some("Use British English.".into());
    p
}

#[test]
fn capture_user_config() {
    let sb = sandbox();
    let r = capture(&["continue"], &sb.env(Scope::User));
    let p = &r.profile;
    assert_eq!(
        p.mcp_servers.keys().collect::<Vec<_>>(),
        vec!["Docs", "Events", "GitHub", "Remote", "browser-search"]
    );
    assert_eq!(
        p.mcp_servers["browser-search"].command.as_deref(),
        Some("npx")
    );
    // Continue's secret syntax comes back as a plain reference.
    assert_eq!(
        p.mcp_servers["GitHub"].env["GITHUB_PERSONAL_ACCESS_TOKEN"],
        "${GITHUB_PERSONAL_ACCESS_TOKEN}"
    );
    let remote = &p.mcp_servers["Remote"];
    assert_eq!(remote.transport(), Transport::Http);
    assert_eq!(remote.headers["Authorization"], "Bearer ${REMOTE_TOKEN}");
    assert_eq!(p.mcp_servers["Events"].transport(), Transport::Sse);
    assert_eq!(p.mcp_servers["Docs"].cwd.as_deref(), Some("tools"));
    // Plain-text rules are the instructions; a Hub block is not.
    assert_eq!(
        p.instructions.as_deref(),
        Some("Always answer in British English.")
    );
    assert!(said(
        &r.notes,
        "'Browser search' is captured as 'browser-search'"
    ));
    assert!(said(
        &r.notes,
        "Continue-specific keys not captured: connectionTimeout"
    ));
    assert!(said(&r.notes, "requestOptions not captured: timeout"));
    assert!(said(&r.notes, "`uses:`"));
    assert!(said(
        &r.notes,
        "models, prompts in config.yaml not captured"
    ));
}

#[test]
fn capture_project_reads_block_files_json_and_rules() {
    let sb = sandbox();
    let r = capture(&["continue"], &sb.env(Scope::Project));
    let p = &r.profile;
    assert_eq!(
        p.mcp_servers.keys().collect::<Vec<_>>(),
        vec!["browser-search", "sqlite"]
    );
    assert_eq!(
        p.mcp_servers["sqlite"].args,
        vec!["mcp-server-sqlite", "--db-path", "./test.db"]
    );
    assert_eq!(p.rules.len(), 2);
    let ts = p.rules.iter().find(|r| r.name == "typescript").unwrap();
    assert_eq!(ts.globs, vec!["**/*.ts", "**/*.tsx"]);
    assert_eq!(ts.body, "- Prefer interfaces for object shapes.");
    let docs = p.rules.iter().find(|r| r.name == "docs").unwrap();
    assert_eq!(
        docs.description.as_deref(),
        Some("Standards for writing docs")
    );
    // alwaysApply: false next to globs only restates what globs mean.
    assert_eq!(docs.always_apply, None);
    assert!(said(&r.notes, "`regex` is Continue-specific"));
    assert!(said(&r.notes, "prompts in .continue/ not captured"));
}

#[test]
fn capture_notes_a_json_only_config() {
    let sb = Sandbox::empty();
    fs::create_dir_all(sb.home().join(".continue")).unwrap();
    fs::write(sb.home().join(".continue/config.json"), "{\"models\": []}").unwrap();
    let r = capture(&["continue"], &sb.env(Scope::User));
    assert!(r.profile.mcp_servers.is_empty());
    assert!(said(&r.notes, "deprecated JSON config"));
}

#[test]
fn apply_edits_config_yaml_in_place() {
    let sb = sandbox();
    let before = sb.read(CONFIG);
    let env = sb.env(Scope::User);
    let mut p = profile();
    // Already in the file (as "Browser search"): kept, comments and all.
    p.mcp_servers.insert(
        "browser-search".into(),
        server("npx", &["@playwright/mcp@latest"]),
    );
    p.instructions = Some("Always answer in British English.\n\nPrefer small diffs.".into());
    let plan = apply(&sb, &p, &["continue"], &env);
    let after = sb.read(CONFIG);

    // Everything that was there is still there, byte for byte, up to the additions.
    for kept in [
        "# My Continue config",
        "  - name: Llama   # runs locally",
        "  # browser automation\n  - name: Browser search\n    command: npx\n    args:\n      - \"@playwright/mcp@latest\"\n    connectionTimeout: 5000\n",
        "      GITHUB_PERSONAL_ACCESS_TOKEN: ${{ secrets.GITHUB_PERSONAL_ACCESS_TOKEN }}\n",
        "prompts:\n  - name: hello\n    prompt: Say hello\n",
        "  - Always answer in British English.\n  - uses: sanity/sanity-opinionated\n",
    ] {
        assert!(after.contains(kept), "lost {kept:?}:\n{after}");
    }
    let doc = yaml(&after);
    let old = yaml(&before);
    assert_eq!(doc["models"], old["models"]);
    assert_eq!(doc["prompts"], old["prompts"]);
    let servers = doc["mcpServers"].as_array().unwrap();
    let names: Vec<&str> = servers
        .iter()
        .map(|s| s["name"].as_str().unwrap())
        .collect();
    assert_eq!(
        names,
        [
            "Browser search",
            "GitHub",
            "Remote",
            "Events",
            "Docs",
            "github",
            "linear"
        ]
    );
    // `github` is a new entry next to the `GitHub` the user has.
    let gh = &servers[5];
    assert_eq!(gh["env"]["GITHUB_TOKEN"], "${{ secrets.GITHUB_TOKEN }}");
    assert_eq!(
        gh["args"],
        json!(["-y", "@modelcontextprotocol/server-github"])
    );
    // A remote server: type, url and headers under requestOptions.
    let linear = &servers[6];
    assert_eq!(linear["type"], "streamable-http");
    assert_eq!(linear["url"], "https://mcp.linear.app/mcp");
    assert_eq!(
        linear["requestOptions"]["headers"]["Authorization"],
        "Bearer ${LINEAR_KEY}"
    );
    // Instructions: the plain rule stays, only the new paragraph is added.
    let rules = doc["rules"].as_array().unwrap();
    assert_eq!(rules.len(), 3);
    assert_eq!(rules[2]["name"], "kitbag-instructions");
    assert_eq!(rules[2]["rule"], "Prefer small diffs.");
    assert_eq!(rules[2]["alwaysApply"], true);
    assert!(said(
        &plan.notes,
        "MCP server 'github' uses ${{ secrets.GITHUB_TOKEN }}"
    ));

    // Applying again changes nothing; undo restores the file exactly.
    let again = plan_of(&p, &env);
    assert!(again.changes.is_empty(), "{:?}", again.changes);
    let store = kitbag::txn::Store::new(sb.state());
    store.undo(None, false).unwrap();
    assert_eq!(sb.read(CONFIG), before);
}

fn plan_of(p: &Profile, env: &kitbag::adapters::Env) -> engine::Plan {
    plan(p, &["continue"], env)
}

#[test]
fn an_existing_secrets_reference_in_a_header_is_kept() {
    let sb = sandbox();
    let env = sb.env(Scope::User);
    let mut p = Profile::new("team");
    p.mcp_servers.insert(
        "Remote".into(),
        McpServer {
            url: Some("https://mcp.example.com/mcp".into()),
            headers: [(
                "Authorization".to_string(),
                "Bearer ${REMOTE_TOKEN}".to_string(),
            )]
            .into_iter()
            .collect(),
            ..Default::default()
        },
    );
    // The file already says this with `${{ secrets.REMOTE_TOKEN }}`; with the
    // variable set, kitbag must not replace it with the secret's value.
    let env = kitbag::adapters::Env {
        vars: [("REMOTE_TOKEN".to_string(), "fake-token-1234".to_string())]
            .into_iter()
            .collect(),
        ..env
    };
    let plan = plan(&p, &["continue"], &env);
    assert!(plan.changes.is_empty(), "{:?}", plan.changes);
    assert!(plan.sensitive.is_empty());
}

#[test]
fn a_new_config_yaml_gets_the_required_header() {
    let sb = Sandbox::empty();
    let env = sb.env(Scope::User);
    let p = apply(&sb, &profile(), &["continue"], &env);
    let text = sb.read(CONFIG);
    assert!(text.starts_with(
        "name: Local Config\nversion: 1.0.0\nschema: v1\nmcpServers:\n  - name: \"github\"\n"
    ));
    let doc = yaml(&text);
    assert_eq!(doc["schema"], "v1");
    assert_eq!(doc["rules"][0]["rule"], "Use British English.");
    assert!(said(&p.notes, "holds no models"));
    // Instructions come back through capture.
    let back = capture(&["continue"], &env).profile;
    assert_eq!(back.instructions.as_deref(), Some("Use British English."));
    assert_eq!(back.mcp_servers["github"], profile().mcp_servers["github"]);
}

#[test]
fn multi_line_instructions_are_written_as_a_block_scalar() {
    let sb = Sandbox::empty();
    let env = sb.env(Scope::User);
    let mut p = Profile::new("team");
    p.instructions =
        Some("# Style\n\n- Be brief.\n- Use \"quotes\": fine.\n  - nested: yes".into());
    apply(&sb, &p, &["continue"], &env);
    let text = sb.read(CONFIG);
    assert!(
        text.contains("    rule: |-\n      # Style\n\n      - Be brief.\n"),
        "{text}"
    );
    let back = capture(&["continue"], &env).profile;
    assert_eq!(back.instructions, p.instructions);
}

#[test]
fn a_config_json_without_config_yaml_is_not_shadowed() {
    let sb = Sandbox::empty();
    fs::create_dir_all(sb.home().join(".continue")).unwrap();
    fs::write(sb.home().join(".continue/config.json"), "{\"models\": []}").unwrap();
    let env = sb.env(Scope::User);
    let plan = plan(&profile(), &["continue"], &env);
    assert!(plan.changes.is_empty());
    assert!(said(
        &plan.notes,
        "was not created: Continue loads config.yaml instead of its deprecated config.json"
    ));
}

#[test]
fn a_layout_that_cannot_be_edited_is_reported_and_left_alone() {
    let sb = Sandbox::empty();
    let flow = "name: x\nversion: 1.0.0\nschema: v1\nmcpServers: [{name: a, command: b}]\n";
    fs::create_dir_all(sb.home().join(".continue")).unwrap();
    fs::write(sb.home().join(".continue/config.yaml"), flow).unwrap();
    let env = sb.env(Scope::User);
    let plan = plan(&profile(), &["continue"], &env);
    assert!(plan.changes.is_empty(), "{:?}", plan.changes);
    assert!(said(
        &plan.notes,
        "nothing written to ~/.continue/config.yaml"
    ));
    assert_eq!(sb.read(CONFIG), flow);
}

#[test]
fn a_server_the_profile_disables_for_continue_is_removed_only_if_kitbag_added_it() {
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
    let cfg = sb.path(CONFIG);
    fs::create_dir_all(cfg.parent().unwrap()).unwrap();
    let start = "# top\nname: Local Config\nversion: 1.0.0\nschema: v1\nmcpServers:\n  - name: mine\n    command: mine\n";
    fs::write(&cfg, start).unwrap();
    write("");
    sb.cli()
        .args(["apply", "prof", "--to", "continue", "--yes"])
        .assert()
        .success();
    assert!(sb.read(CONFIG).contains("name: \"docs\""));
    write("disabled_on = [\"continue\"]\n");
    sb.cli()
        .args(["apply", "prof", "--to", "continue", "--yes"])
        .assert()
        .success();
    assert_eq!(sb.read(CONFIG), start, "docs is removed, the rest stays");
    // `mine` was the user's own: disabling it leaves it, with a note.
    let both = fs::read_to_string(sb.path("prof/kitbag.toml"))
        .unwrap()
        .replace(
            "command = \"mine\"\n",
            "command = \"mine\"\ndisabled_on = [\"continue\"]\n",
        );
    fs::write(sb.path("prof/kitbag.toml"), both).unwrap();
    sb.cli()
        .args(["apply", "prof", "--to", "continue", "--yes"])
        .assert()
        .success()
        .stdout(predicates::str::contains("no record of adding it"));
    assert_eq!(sb.read(CONFIG), start);
}

#[test]
fn a_server_the_profile_marks_disabled_is_not_written() {
    let mut q = Profile::new("team");
    q.mcp_servers.insert(
        "off".into(),
        McpServer {
            disabled: true,
            ..server("x", &[])
        },
    );
    let sb = Sandbox::empty();
    let pl = plan(&q, &["continue"], &sb.env(Scope::User));
    assert!(pl.changes.is_empty());
    assert!(said(
        &pl.notes,
        "'off' is disabled in the profile; not written"
    ));
}

#[test]
fn secrets_outside_args_and_env_are_resolved_into_a_private_file() {
    let sb = Sandbox::empty();
    let env = sb.env_with(Scope::User, &[("LINEAR_KEY", "fake-linear-key-0001")]);
    let p = plan(&profile(), &["continue"], &env);
    let text = planned(&p, &sb.home().join(".continue/config.yaml")).unwrap();
    // The header is the one place Continue documents no secrets syntax for.
    assert!(text.contains("Bearer fake-linear-key-0001"));
    assert!(p.sensitive.contains("fake-linear-key-0001"));
    assert!(p.private.contains(&sb.home().join(".continue/config.yaml")));
    assert!(said(
        &p.notes,
        "headers.Authorization resolved from your environment"
    ));

    // Not with --secrets never.
    let mut o = opts(&profile());
    o.secrets = SecretsMode::Never;
    let p = engine::plan(&profile(), &targets(&["continue"]), &env, &o).unwrap();
    let text = planned(&p, &sb.home().join(".continue/config.yaml")).unwrap();
    assert!(text.contains("Bearer ${LINEAR_KEY}") && !text.contains("fake-linear"));
    assert!(p.private.is_empty());

    // In a project the plan warns that the file is committed.
    let env = sb.env_with(Scope::Project, &[("LINEAR_KEY", "fake-linear-key-0001")]);
    let p = plan(&profile(), &["continue"], &env);
    assert!(said(&p.notes, "a secret was written to"));
}

#[cfg(unix)]
#[test]
fn a_file_with_a_resolved_secret_is_written_0600() {
    use std::os::unix::fs::PermissionsExt;
    let sb = Sandbox::empty();
    let env = sb.env_with(Scope::User, &[("LINEAR_KEY", "fake-linear-key-0001")]);
    apply(&sb, &profile(), &["continue"], &env);
    let mode = fs::metadata(sb.path(CONFIG)).unwrap().permissions().mode();
    assert_eq!(mode & 0o777, 0o600);
}

#[test]
fn project_servers_go_to_the_file_that_defines_them_or_to_kitbag_yaml() {
    let sb = sandbox();
    let env = sb.env(Scope::Project);
    let mut p = profile();
    // In playwright-mcp.yaml already: changed there, in place.
    p.mcp_servers.insert(
        "browser-search".into(),
        server("npx", &["@playwright/mcp@latest", "--headless"]),
    );
    // Defined in mcp.json with the same command: left alone, no warning.
    p.mcp_servers.insert(
        "sqlite".into(),
        server("uvx", &["mcp-server-sqlite", "--db-path", "./test.db"]),
    );
    let plan = apply(&sb, &p, &["continue"], &env);
    let block = sb.read("project/.continue/mcpServers/playwright-mcp.yaml");
    assert!(block.starts_with("name: Playwright mcpServer\nversion: 0.0.1\nschema: v1\nmcpServers:\n  # the browser\n  - name: \"Browser search\"\n"));
    assert_eq!(
        yaml(&block)["mcpServers"][0]["args"],
        json!(["@playwright/mcp@latest", "--headless"])
    );
    let kb = sb.read(KITBAG_YAML);
    assert!(kb.starts_with("name: kitbag\nversion: 0.0.1\nschema: v1\nmcpServers:\n"));
    let names: Vec<String> = yaml(&kb)["mcpServers"]
        .as_array()
        .unwrap()
        .iter()
        .map(|s| s["name"].as_str().unwrap().to_string())
        .collect();
    assert_eq!(names, vec!["github", "linear"]);
    // The JSON file is untouched.
    assert_eq!(
        sb.read("project/.continue/mcpServers/mcp.json"),
        fixture("project/.continue/mcpServers/mcp.json")
    );
    assert!(said(&plan.notes, "'sqlite' is already defined in"));

    // A different sqlite is not written over the JSON file, and says so.
    p.mcp_servers
        .insert("sqlite".into(), server("uvx", &["other"]));
    let plan = self::plan(&p, &["continue"], &env);
    assert!(said(&plan.notes, "a JSON file kitbag leaves alone"));
    assert!(plan.changes.iter().all(|c| !c.path.ends_with("mcp.json")));

    // Undo removes kitbag.yaml and restores the block file.
    kitbag::txn::Store::new(sb.state())
        .undo(None, false)
        .unwrap();
    assert!(!sb.path(KITBAG_YAML).exists());
    assert_eq!(
        sb.read("project/.continue/mcpServers/playwright-mcp.yaml"),
        fixture("project/.continue/mcpServers/playwright-mcp.yaml")
    );
}

#[test]
fn project_rules_and_instructions_go_to_continue_rules_and_leave_agents_md_alone() {
    let sb = sandbox();
    let env = sb.env(Scope::Project);
    let mut p = profile();
    p.rules.push(Rule {
        name: "py".into(),
        description: Some("Python style".into()),
        globs: vec!["**/*.py".into()],
        body: "Use type hints.".into(),
        ..Default::default()
    });
    p.rules.push(Rule {
        name: "always".into(),
        body: "Be kind.".into(),
        ..Default::default()
    });
    p.rules.push(Rule {
        name: "demand".into(),
        description: Some("When asked".into()),
        always_apply: Some(false),
        body: "Only on request.".into(),
        ..Default::default()
    });
    apply(&sb, &p, &["continue"], &env);
    let rules = sb.path("project/.continue/rules");
    assert_eq!(
        fs::read_to_string(rules.join("py.md")).unwrap(),
        "---\nname: py\ndescription: Python style\nglobs:\n  - \"**/*.py\"\n---\n\nUse type hints.\n"
    );
    assert!(
        fs::read_to_string(rules.join("always.md"))
            .unwrap()
            .contains("alwaysApply: true")
    );
    assert!(
        fs::read_to_string(rules.join("demand.md"))
            .unwrap()
            .contains("alwaysApply: false")
    );
    let instr = fs::read_to_string(rules.join("kitbag-instructions.md")).unwrap();
    assert!(instr.contains("Use British English."));
    assert!(!sb.path("project/AGENTS.md").exists());

    // The Continue rule files the project had are untouched, and capture reads
    // everything back (the instructions file is not a rule).
    let back = capture(&["continue"], &env).profile;
    assert_eq!(back.instructions.as_deref(), Some("Use British English."));
    for r in &p.rules {
        let got = back.rules.iter().find(|x| x.name == r.name).unwrap();
        assert_eq!(
            (&got.description, &got.globs, &got.body),
            (&r.description, &r.globs, &r.body),
            "{}",
            r.name
        );
        assert_eq!(got.mode(), r.mode(), "{}", r.name);
    }
    assert!(back.rules.iter().any(|r| r.name == "typescript"));
    assert!(!back.rules.iter().any(|r| r.name.contains("instructions")));
}

#[test]
fn applying_with_codex_keeps_agents_md_separate() {
    let sb = Sandbox::empty();
    fs::write(sb.project().join("AGENTS.md"), "My own notes.\n").unwrap();
    let env = sb.env(Scope::Project);
    apply(&sb, &profile(), &["codex", "continue"], &env);
    let agents = sb.read("project/AGENTS.md");
    assert!(agents.starts_with("My own notes.\n"));
    assert_eq!(agents.matches("kitbag:begin").count(), 1);
    let instr = sb.read("project/.continue/rules/kitbag-instructions.md");
    assert_eq!(instr.matches("kitbag:begin").count(), 1);
    // Both read back.
    let back = capture(&["continue"], &env).profile;
    assert_eq!(back.instructions.as_deref(), Some("Use British English."));
}

#[test]
fn rules_are_not_written_at_user_scope() {
    let sb = Sandbox::empty();
    let mut p = profile();
    p.rules.push(Rule {
        name: "py".into(),
        body: "x".into(),
        ..Default::default()
    });
    let plan = apply(&sb, &p, &["continue"], &sb.env(Scope::User));
    assert!(said(
        &plan.notes,
        "apply with --project to write the profile's rules"
    ));
    assert!(!sb.path("home/.continue/rules").exists());
}

#[test]
fn no_overwrite_keeps_an_entry_that_differs() {
    let sb = sandbox();
    let env = sb.env(Scope::User);
    let mut p = Profile::new("team");
    p.mcp_servers
        .insert("Docs".into(), server("uvx", &["mcp-docs", "--new"]));
    let mut o = opts(&p);
    o.overwrite = false;
    let pl = engine::plan(&p, &targets(&["continue"]), &env, &o).unwrap();
    assert!(pl.changes.is_empty(), "{:?}", pl.changes);
    assert!(said(&pl.notes, "kept existing MCP server 'Docs'"));
}

#[test]
fn continue_is_listed_and_detected() {
    let sb = sandbox();
    let out = sb
        .cli()
        .args(["list-targets", "--json"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let v: Value = serde_json::from_slice(&out).unwrap();
    let c = v
        .as_array()
        .unwrap()
        .iter()
        .find(|t| t["id"] == "continue")
        .unwrap();
    assert_eq!(c["detected"], true);
    assert_eq!(c["support"]["mcp"], "native");
    assert_eq!(c["support"]["instructions"], "native");
    assert_eq!(c["support"]["rules"], "-", "user scope has no rule files");
}
