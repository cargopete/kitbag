//! Hermes Agent's shell hooks (the `hooks:` block of `~/.hermes/config.yaml`):
//! opt-in capture and apply, git sources, a second apply, undo, doctor, status,
//! and the hook systems kitbag leaves alone (outbound webhooks, gateway and
//! plugin hooks).

mod common;

use std::fs;
use std::path::Path;

use common::*;
use kitbag::profile_io;
use predicates::prelude::*;
use predicates::str::contains;
use serde_json::{Value, json};

const CONFIG: &str = "home/.hermes/config.yaml";

fn slashes(p: &Path) -> String {
    p.display().to_string().replace('\\', "/")
}

fn write(path: &Path, content: &str) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, content).unwrap();
}

fn yaml(text: &str) -> Value {
    serde_norway::from_str(text).unwrap()
}

fn config(sb: &Sandbox) -> Value {
    yaml(&sb.read(CONFIG))
}

/// What Hermes's own tooling writes: a list at the key's indentation, the
/// user's own hooks, other keys, comments, an MCP server and outbound webhooks.
const USERS_CONFIG: &str = "\
# my hermes config
model: some-model  # keep this comment
mcp_servers:
  git:
    command: uvx
hooks:
  pre_tool_call:
  - matcher: \"terminal\"
    command: \"echo mine\"
    timeout: 5
  post_llm_call:
    - command: \"echo mine too\"
  outbound:
  - name: ci
    url: https://ci.example.com/hermes
    events: [on_session_end]
hooks_auto_accept: false
";

/// A profile with a script hook (matcher, timeout, fail_closed), an inline
/// one, and events Hermes names differently, or not at all.
fn profile(dir: &Path) -> String {
    write(
        &dir.join("kitbag.toml"),
        "kitbag = 1\nname = \"p\"\n\n\
         [[hooks]]\nevent = \"pre-tool\"\nmatcher = \"terminal|patch\"\ncommand = \"{{hooks_dir}}/check.sh\"\ntimeout = 20\nextra = { fail_closed = true }\n\n\
         [[hooks]]\nevent = \"session-start\"\ncommand = \"echo started\"\n",
    );
    write(&dir.join("hooks/check.sh"), "#!/bin/sh\necho checking\n");
    dir.display().to_string()
}

fn apply_hooks(sb: &Sandbox, prof: &str) -> assert_cmd::assert::Assert {
    sb.cli()
        .args(["apply", prof, "--to", "hermes", "--apply-hooks", "--yes"])
        .assert()
        .success()
}

fn hooks_of<'a>(v: &'a Value, event: &str) -> &'a Vec<Value> {
    v["hooks"][event]
        .as_array()
        .unwrap_or_else(|| panic!("no {event} in {v}"))
}

#[test]
fn capture_needs_the_flag_takes_shell_hooks_and_scripts_and_reports_the_rest() {
    let sb = Sandbox::empty();
    let script = sb.home().join(".hermes/agent-hooks/fmt.sh");
    write(&script, "#!/bin/sh\necho formatting\n");
    write(
        &sb.home().join(".hermes/config.yaml"),
        &format!(
            "model: x\nhooks:\n  post_tool_call:\n    - matcher: \"write_file|patch\"\n      command: \"{}\"\n      timeout: 30\n  on_session_start:\n    - command: \"echo started\"\n  outbound:\n    - name: ci\n      url: https://ci.example.com/x\n      events: [on_session_end]\nhooks_auto_accept: true\n",
            slashes(&script)
        ),
    );
    // Credentials in the Hermes home are never read.
    write(
        &sb.home().join(".hermes/.env"),
        "TOKEN=fake-not-a-secret-1\n",
    );
    write(
        &sb.home().join(".hermes/hooks/mygw/HOOK.yaml"),
        "name: mygw\nevents: [agent:start]\n",
    );
    write(
        &sb.home().join(".hermes/plugins/audit/__init__.py"),
        "def register(ctx):\n    ctx.register_hook(\"pre_tool_call\", lambda **k: None)\n",
    );

    sb.cli()
        .args(["capture", "--from", "hermes", "-o", "prof"])
        .assert()
        .success()
        .stdout(
            contains("--include hooks")
                .and(contains("2 hook(s) were not captured"))
                .and(contains("gateway hooks (mygw)"))
                .and(contains(
                    "plugin hooks (ctx.register_hook() in plugin audit)",
                )),
        );
    if sb.path("prof/kitbag.toml").exists() {
        assert!(!sb.read("prof/kitbag.toml").contains("[[hooks]]"));
    }
    assert!(!sb.path("prof/hooks").exists());

    sb.cli()
        .args([
            "capture",
            "--from",
            "hermes",
            "-o",
            "prof2",
            "--include",
            "hooks",
        ])
        .assert()
        .success()
        .stdout(
            contains("captured 2 hook(s) and 1 script(s)")
                .and(contains("outbound webhooks (hooks.outbound, 1 entry)")),
        );
    let p = profile_io::load(&sb.path("prof2")).unwrap();
    let post = p
        .hooks
        .iter()
        .find(|h| h.event == "post-tool")
        .unwrap_or_else(|| panic!("{:?}", p.hooks));
    assert_eq!(post.command, "{{hooks_dir}}/fmt.sh");
    assert_eq!(post.matcher.as_deref(), Some("write_file|patch"));
    assert_eq!(post.timeout, Some(30));
    assert!(
        p.hooks
            .iter()
            .any(|h| h.event == "session-start" && h.command == "echo started")
    );
    // Outbound webhooks and the auto-accept switch are not hooks of the profile.
    assert_eq!(p.hooks.len(), 2, "{:?}", p.hooks);
    assert!(sb.read("prof2/hooks/fmt.sh").contains("echo formatting"));
    let all = all_text_under(&sb.path("prof2"));
    assert!(!all.contains("fake-not-a-secret"), "{all}");
    assert!(!all.contains("ci.example.com"), "{all}");
    assert!(!all.contains("hooks_auto_accept"), "{all}");
}

#[test]
fn apply_needs_apply_hooks_then_edits_in_place_once_and_undo_restores() {
    let sb = Sandbox::empty();
    write(&sb.home().join(".hermes/config.yaml"), USERS_CONFIG);
    let prof = profile(&sb.path("p"));

    // Without the flag, even with --yes: listed, nothing written.
    sb.cli()
        .args(["apply", &prof, "--to", "hermes", "--yes"])
        .assert()
        .success()
        .stdout(contains("--apply-hooks").and(contains("echo started")));
    assert_eq!(sb.read(CONFIG), USERS_CONFIG);
    assert!(!sb.home().join(".hermes/agent-hooks").exists());

    apply_hooks(&sb, &prof).stdout(contains("hermes hooks list"));
    let text = sb.read(CONFIG);
    let v = yaml(&text);
    // The user's hooks, other keys, comments, MCP server and outbound list stay.
    assert!(text.starts_with("# my hermes config\nmodel: some-model  # keep this comment\n"));
    assert_eq!(v["mcp_servers"]["git"]["command"], "uvx");
    assert_eq!(v["hooks_auto_accept"], false);
    assert_eq!(
        v["hooks"]["post_llm_call"],
        json!([{"command": "echo mine too"}])
    );
    assert_eq!(
        v["hooks"]["outbound"],
        json!([{"name": "ci", "url": "https://ci.example.com/hermes", "events": ["on_session_end"]}])
    );
    assert!(text.contains("  outbound:\n  - name: ci\n    url: https://ci.example.com/hermes\n"));
    let script = sb.home().join(".hermes/agent-hooks/kitbag/check.sh");
    assert!(script.is_file());
    // Not in hooks/, where Hermes loads gateway hooks from.
    assert!(!sb.home().join(".hermes/hooks").exists());
    let pre = hooks_of(&v, "pre_tool_call");
    assert_eq!(pre.len(), 2, "{v}");
    assert_eq!(pre[0]["command"], "echo mine");
    assert_eq!(pre[1]["command"], slashes(&script).as_str());
    assert_eq!(pre[1]["matcher"], "terminal|patch");
    assert_eq!(pre[1]["timeout"], 20);
    assert_eq!(pre[1]["fail_closed"], true);
    assert!(pre[1].get("type").is_none(), "{v}");
    let start = hooks_of(&v, "on_session_start");
    assert_eq!(start[0]["command"], "echo started");
    // Hermes's consent file is never written by kitbag.
    assert!(
        !sb.home()
            .join(".hermes/shell-hooks-allowlist.json")
            .exists()
    );

    // A second apply has nothing to do.
    sb.cli()
        .args(["apply", &prof, "--to", "hermes", "--apply-hooks", "--yes"])
        .assert()
        .success()
        .stdout(contains("Up to date"));
    assert_eq!(sb.read(CONFIG), text);

    sb.cli()
        .args(["doctor", "--to", "hermes"])
        .assert()
        .success()
        .stdout(contains("Hooks kitbag applied"));
    sb.cli()
        .args(["status", &prof, "--to", "hermes"])
        .assert()
        .success()
        .stdout(contains("in sync").and(contains("not applied").not()));

    sb.cli().arg("undo").assert().success();
    assert_eq!(sb.read(CONFIG), USERS_CONFIG);
    assert!(!script.exists());
}

#[test]
fn a_profile_without_hooks_removes_none_of_the_users_shell_hooks() {
    let sb = Sandbox::empty();
    write(&sb.home().join(".hermes/config.yaml"), USERS_CONFIG);
    write(
        &sb.path("p/kitbag.toml"),
        "kitbag = 1\nname = \"p\"\n\n[mcp_servers.fs]\ncommand = \"npx\"\n",
    );
    sb.cli()
        .args([
            "apply",
            &sb.path("p").display().to_string(),
            "--to",
            "hermes",
            "--apply-hooks",
            "--yes",
        ])
        .assert()
        .success();
    let v = config(&sb);
    assert_eq!(v["mcp_servers"]["fs"]["command"], "npx");
    assert_eq!(hooks_of(&v, "pre_tool_call").len(), 1);
    assert_eq!(hooks_of(&v, "post_llm_call").len(), 1);
    assert_eq!(v["hooks"]["outbound"][0]["name"], "ci");
    assert!(sb.read(CONFIG).contains("# keep this comment"));
}

#[test]
fn hooks_mcp_servers_and_instructions_apply_together_and_undo_together() {
    let sb = Sandbox::empty();
    write(&sb.home().join(".hermes/config.yaml"), "model: x\n");
    write(
        &sb.home().join(".hermes/SOUL.md"),
        "I am a careful agent.\n",
    );
    write(
        &sb.path("p/kitbag.toml"),
        "kitbag = 1\nname = \"p\"\n\n[mcp_servers.fs]\ncommand = \"npx\"\n\n[[hooks]]\nevent = \"session-start\"\ncommand = \"echo started\"\n",
    );
    write(&sb.path("p/instructions.md"), "Use British English.\n");
    let prof = sb.path("p");

    apply_hooks(&sb, &prof.display().to_string());
    let v = config(&sb);
    assert_eq!(v["mcp_servers"]["fs"]["command"], "npx");
    assert_eq!(
        hooks_of(&v, "on_session_start")[0]["command"],
        "echo started"
    );
    let soul = sb.read("home/.hermes/SOUL.md");
    assert!(soul.starts_with("I am a careful agent."), "{soul}");
    assert!(soul.contains("Use British English."), "{soul}");

    sb.cli().arg("undo").assert().success();
    assert_eq!(sb.read(CONFIG), "model: x\n");
    assert_eq!(sb.read("home/.hermes/SOUL.md"), "I am a careful agent.\n");
}

#[test]
fn events_hermes_names_differently_or_lacks_are_reported_not_guessed() {
    let sb = Sandbox::empty();
    write(
        &sb.path("p/kitbag.toml"),
        "kitbag = 1\nname = \"p\"\n\n\
         [[hooks]]\nevent = \"hermes:on_session_finalize\"\ncommand = \"echo end\"\n\n\
         [[hooks]]\nevent = \"prompt-submit\"\ncommand = \"echo prompt\"\n\n\
         [[hooks]]\nevent = \"hermes:post_compaction\"\ncommand = \"echo compacted\"\n\n\
         [[hooks]]\nevent = \"session-end\"\ncommand = \"echo not guessed\"\n\n\
         [[hooks]]\nevent = \"post-compact\"\ncommand = \"echo not guessed either\"\n\n\
         [[hooks]]\nevent = \"hermes:on_session_end\"\ncommand = \"echo turn\"\n\n\
         [[hooks]]\nevent = \"session-start\"\nmatcher = \"startup\"\ncommand = \"echo start\"\n\n\
         [[hooks]]\nevent = \"post-tool-failure\"\ncommand = \"echo failed\"\n\n\
         [[hooks]]\nevent = \"claude-code:FileChanged\"\ncommand = \"echo claude only\"\n",
    );
    let prof = sb.path("p").display().to_string();
    apply_hooks(&sb, &prof).stdout(
        contains("'post-tool-failure' has no equivalent event in Hermes Agent")
            .and(contains("'session-end' has no equivalent event"))
            .and(contains("'post-compact' has no equivalent event"))
            .and(contains(
                "'claude-code:FileChanged' has no equivalent event",
            ))
            .and(contains(
                "matcher left off: Hermes Agent ignores matchers on on_session_start",
            )),
    );
    let v = config(&sb);
    assert_eq!(
        hooks_of(&v, "on_session_finalize")[0]["command"],
        "echo end"
    );
    assert_eq!(hooks_of(&v, "pre_llm_call")[0]["command"], "echo prompt");
    assert_eq!(
        hooks_of(&v, "post_compaction")[0]["command"],
        "echo compacted"
    );
    assert_eq!(hooks_of(&v, "on_session_end")[0]["command"], "echo turn");
    let start = &hooks_of(&v, "on_session_start")[0];
    assert_eq!(start["command"], "echo start");
    assert!(start.get("matcher").is_none(), "{start}");
    assert_eq!(v["hooks"].as_object().unwrap().len(), 5, "{v}");

    // Status says which hooks Hermes can't have.
    sb.cli()
        .args(["status", &prof, "--to", "hermes"])
        .assert()
        .stdout(contains("no equivalent event in Hermes Agent"));
}

#[test]
fn hooks_from_a_git_source_need_trust_source_and_apply_hooks() {
    if !has_git() {
        return;
    }
    let sb = Sandbox::empty();
    let prof = sb.path("p");
    profile(&prof);
    let url = sb.git_profile(
        "repo",
        &[
            (
                "kitbag.toml",
                &fs::read_to_string(prof.join("kitbag.toml")).unwrap(),
            ),
            ("hooks/check.sh", "#!/bin/sh\necho checking\n"),
        ],
    );
    let wrote = |sb: &Sandbox| sb.home().join(".hermes/config.yaml").exists();
    sb.cli()
        .args(["apply", &url, "--to", "hermes", "--yes"])
        .assert()
        .success()
        .stdout(contains("remote repository").and(contains("echo started")));
    assert!(!wrote(&sb));
    sb.cli()
        .args(["apply", &url, "--to", "hermes", "--yes", "--apply-hooks"])
        .assert()
        .success()
        .stdout(contains("--trust-source"));
    assert!(!wrote(&sb));
    sb.cli()
        .args(["apply", &url, "--to", "hermes", "--yes", "--trust-source"])
        .assert()
        .success();
    assert!(!wrote(&sb));
    sb.cli()
        .args([
            "apply",
            &url,
            "--to",
            "hermes",
            "--yes",
            "--trust-source",
            "--apply-hooks",
        ])
        .assert()
        .success();
    assert!(wrote(&sb));
    assert_eq!(
        hooks_of(&config(&sb), "on_session_start")[0]["command"],
        "echo started"
    );
}

#[test]
fn a_layout_the_editor_cannot_write_is_reported_and_left_alone() {
    let sb = Sandbox::empty();
    let own = "hooks: {on_session_start: [{command: \"echo mine\"}]}\n";
    write(&sb.home().join(".hermes/config.yaml"), own);
    let prof = profile(&sb.path("p"));
    apply_hooks(&sb, &prof)
        .stdout(contains("hooks not written to").and(contains("add them by hand")));
    assert_eq!(sb.read(CONFIG), own);
    // Status does not call them in sync.
    sb.cli()
        .args(["status", &prof, "--to", "hermes"])
        .assert()
        .stdout(contains("not applied"));
}

#[test]
fn project_scope_writes_no_hooks_and_says_why() {
    let sb = Sandbox::empty();
    let prof = profile(&sb.path("p"));
    sb.cli()
        .current_dir(sb.project())
        .args([
            "apply",
            &prof,
            "--to",
            "hermes",
            "--scope",
            "project",
            "--apply-hooks",
            "--yes",
        ])
        .assert()
        .success()
        .stdout(
            contains("2 hook(s) not written to Hermes Agent").and(contains("no project hook file")),
        );
    assert!(!sb.home().join(".hermes").exists());
    assert!(!sb.project().join(".hermes").exists());
}

#[test]
fn doctor_flags_a_hermes_hook_that_changed_since_it_was_applied() {
    let sb = Sandbox::empty();
    let prof = profile(&sb.path("p"));
    apply_hooks(&sb, &prof);
    sb.cli()
        .args(["doctor", "--to", "hermes"])
        .assert()
        .success();
    let path = sb.home().join(".hermes/config.yaml");
    let text = fs::read_to_string(&path).unwrap();
    assert!(text.contains("timeout: 20"));
    fs::write(&path, text.replace("timeout: 20", "timeout: 21")).unwrap();
    sb.cli()
        .args(["doctor", "--to", "hermes"])
        .assert()
        .code(1)
        .stdout(contains("no longer matches what kitbag applied"));
    fs::write(&path, text).unwrap();
    write(
        &sb.home().join(".hermes/agent-hooks/kitbag/check.sh"),
        "#!/bin/sh\necho tampered\n",
    );
    sb.cli()
        .args(["doctor", "--to", "hermes"])
        .assert()
        .code(1)
        .stdout(contains("check.sh"));
}
