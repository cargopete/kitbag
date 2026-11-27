//! Hooks on the tools besides Claude Code: Cursor, Codex, Gemini CLI, Grok
//! Build, Antigravity and GitHub Copilot. Opt-in capture, opt-in apply, trust
//! for git sources, a second apply, undo and doctor, and the tools that have
//! no hooks to write.

mod common;

use std::fs;
use std::path::Path;

use common::*;
use kitbag::profile_io;
use predicates::prelude::*;
use predicates::str::contains;
use serde_json::{Value, json};

/// One tool's hook file: where it is below the home directory, where its
/// scripts are, the event a pre-tool hook has in it, where the events are.
#[derive(Clone, Copy)]
struct Tool {
    id: &'static str,
    file: &'static str,
    /// Where the hook file kitbag writes is (the same as `file` unless the
    /// tool has a directory of hook files).
    written: &'static str,
    config_dir: &'static str,
    pre_tool: &'static str,
    root: &'static str,
}

const TOOLS: &[Tool] = &[
    Tool {
        id: "cursor",
        file: ".cursor/hooks.json",
        written: ".cursor/hooks.json",
        config_dir: ".cursor",
        pre_tool: "preToolUse",
        root: "hooks",
    },
    Tool {
        id: "codex",
        file: ".codex/hooks.json",
        written: ".codex/hooks.json",
        config_dir: ".codex",
        pre_tool: "PreToolUse",
        root: "hooks",
    },
    Tool {
        id: "gemini-cli",
        file: ".gemini/settings.json",
        written: ".gemini/settings.json",
        config_dir: ".gemini",
        pre_tool: "BeforeTool",
        root: "hooks",
    },
    Tool {
        id: "grok-build",
        file: ".grok/hooks/mine.json",
        written: ".grok/hooks/kitbag.json",
        config_dir: ".grok",
        pre_tool: "PreToolUse",
        root: "hooks",
    },
    Tool {
        id: "antigravity",
        file: ".gemini/config/hooks.json",
        written: ".gemini/config/hooks.json",
        config_dir: ".gemini/config",
        pre_tool: "PreToolUse",
        root: "kitbag",
    },
    Tool {
        id: "copilot",
        file: ".copilot/hooks/mine.json",
        written: ".copilot/hooks/kitbag.json",
        config_dir: ".copilot",
        pre_tool: "preToolUse",
        root: "hooks",
    },
];

fn slashes(p: &Path) -> String {
    p.display().to_string().replace('\\', "/")
}

fn write(path: &Path, content: &str) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, content).unwrap();
}

/// The tool's own hook file with a script hook (matcher, 20 seconds) and an
/// inline one, as its docs lay them out.
fn native(t: &Tool, script: &str) -> Value {
    match t.id {
        "cursor" => json!({"version": 1, "hooks": {
            "preToolUse": [{"matcher": "Shell", "command": script, "timeout": 20}],
            "sessionStart": [{"command": "echo started"}],
            "beforeShellExecution": [{"command": "echo cursor-only", "failClosed": true}],
        }}),
        "codex" => json!({"hooks": {
            "PreToolUse": [{"matcher": "Bash", "hooks": [
                {"type": "command", "command": script, "timeout": 20}]}],
            "SessionStart": [{"hooks": [{"type": "command", "command": "echo started"}]}],
        }}),
        "gemini-cli" => json!({"theme": "dark", "hooks": {
            "BeforeTool": [{"matcher": "run_shell_command", "hooks": [
                {"name": "check", "type": "command", "command": script, "timeout": 20000}]}],
            "SessionStart": [{"hooks": [{"type": "command", "command": "echo started"}]}],
        }}),
        "grok-build" => json!({"hooks": {
            "PreToolUse": [{"matcher": "Bash", "hooks": [
                {"type": "command", "command": script, "timeout": 20}]}],
            "SessionStart": [{"hooks": [{"type": "command", "command": "echo started"}]}],
        }}),
        "antigravity" => json!({"mine": {"enabled": true,
            "PreToolUse": [{"matcher": "run_command", "hooks": [
                {"type": "command", "command": script, "timeout": 20}]}],
            "SessionStart": [{"type": "command", "command": "echo started"}],
        }}),
        _ => json!({"version": 1, "hooks": {
            "preToolUse": [{"type": "command", "matcher": "bash", "bash": script, "timeoutSec": 20}],
            "sessionStart": [{"type": "command", "command": "echo started"}],
        }}),
    }
}

fn setup(sb: &Sandbox, t: &Tool) {
    let script = sb.home().join(t.config_dir).join("hooks/check.sh");
    write(&script, "#!/bin/sh\necho checking\n");
    write(
        &sb.home().join(t.file),
        &serde_json::to_string_pretty(&native(t, &slashes(&script))).unwrap(),
    );
}

fn capture(sb: &Sandbox, t: &Tool, include: bool) -> assert_cmd::assert::Assert {
    let mut c = sb.cli();
    c.args(["capture", "--from", t.id, "-o", "prof"]);
    if include {
        c.args(["--include", "hooks"]);
    }
    c.assert().success()
}

/// A profile with a script hook and an inline one, in a directory of its own.
fn profile(dir: &Path) -> String {
    write(
        &dir.join("kitbag.toml"),
        "kitbag = 1\nname = \"p\"\n\n[[hooks]]\nevent = \"pre-tool\"\nmatcher = \"Bash\"\ncommand = \"{{hooks_dir}}/check.sh\"\ntimeout = 20\n\n[[hooks]]\nevent = \"session-start\"\ncommand = \"echo started\"\n",
    );
    write(&dir.join("hooks/check.sh"), "#!/bin/sh\necho checking\n");
    dir.display().to_string()
}

fn written(sb: &Sandbox, t: &Tool) -> Value {
    sb.json(&format!("home/{}", t.written))
}

/// The handlers of an event in what kitbag wrote, in either layout.
fn handlers(v: &Value, t: &Tool, event: &str) -> Vec<Value> {
    v[t.root][event]
        .as_array()
        .unwrap_or_else(|| panic!("no {event} in {v}"))
        .iter()
        .flat_map(|g| match g.get("hooks").and_then(Value::as_array) {
            Some(inner) => inner.clone(),
            None => vec![g.clone()],
        })
        .collect()
}

#[test]
fn capture_needs_the_flag_and_takes_hooks_and_scripts() {
    for t in TOOLS {
        let sb = Sandbox::empty();
        setup(&sb, t);
        capture(&sb, t, false).stdout(contains("--include hooks"));
        // Nothing but hooks to capture: no profile, or one without them.
        if sb.path("prof/kitbag.toml").exists() {
            let toml = sb.read("prof/kitbag.toml");
            assert!(!toml.contains("hooks"), "{}: {toml}", t.id);
        }
        assert!(!sb.path("prof/hooks").exists(), "{}", t.id);

        let sb = Sandbox::empty();
        setup(&sb, t);
        capture(&sb, t, true).stdout(contains("hook(s)"));
        let p = profile_io::load(&sb.path("prof")).unwrap();
        let pre = p
            .hooks
            .iter()
            .find(|h| h.event == "pre-tool")
            .unwrap_or_else(|| panic!("{}: {:?}", t.id, p.hooks));
        assert_eq!(pre.command, "{{hooks_dir}}/check.sh", "{}", t.id);
        assert_eq!(pre.timeout, Some(20), "{}", t.id);
        assert!(pre.matcher.is_some(), "{}", t.id);
        assert!(
            p.hooks
                .iter()
                .any(|h| h.event == "session-start" && h.command == "echo started"),
            "{}",
            t.id
        );
        assert!(
            p.hook_files.iter().any(|f| f.path == "check.sh"),
            "{}",
            t.id
        );
        assert!(
            sb.read("prof/hooks/check.sh").contains("echo checking"),
            "{}",
            t.id
        );
    }
}

#[test]
fn apply_needs_apply_hooks_then_writes_once_and_undo_removes_it() {
    for t in TOOLS {
        let sb = Sandbox::empty();
        let prof = profile(&sb.path("p"));
        // Without the flag, even with --yes: listed, nothing written.
        sb.cli()
            .args(["apply", &prof, "--to", t.id, "--yes"])
            .assert()
            .success()
            .stdout(contains("--apply-hooks").and(contains("echo started")));
        assert!(!sb.home().join(t.written).exists(), "{}", t.id);
        assert!(!sb.home().join(t.config_dir).join("hooks/kitbag").exists());

        sb.cli()
            .args(["apply", &prof, "--to", t.id, "--apply-hooks", "--yes"])
            .assert()
            .success();
        let v = written(&sb, t);
        let pre = handlers(&v, t, t.pre_tool);
        let script = sb.home().join(t.config_dir).join("hooks/kitbag/check.sh");
        assert!(script.is_file(), "{}", t.id);
        assert_eq!(
            pre[0]["command"],
            slashes(&script).as_str(),
            "{}: {v}",
            t.id
        );
        // The timeout is in the tool's unit.
        let timeout = if t.id == "gemini-cli" { 20_000 } else { 20 };
        let key = if t.id == "copilot" {
            "timeoutSec"
        } else {
            "timeout"
        };
        assert_eq!(pre[0][key], timeout, "{}: {v}", t.id);
        let started: Vec<Value> = ["sessionStart", "SessionStart"]
            .iter()
            .filter(|e| v[t.root].get(**e).is_some())
            .flat_map(|e| handlers(&v, t, e))
            .collect();
        assert_eq!(started[0]["command"], "echo started", "{}", t.id);
        if matches!(t.id, "cursor" | "copilot") {
            assert_eq!(v["version"], 1, "{}", t.id);
        }

        // A second apply has nothing to do.
        sb.cli()
            .args(["apply", &prof, "--to", t.id, "--apply-hooks", "--yes"])
            .assert()
            .success()
            .stdout(contains("Up to date"));

        sb.cli()
            .args(["doctor", "--to", t.id])
            .assert()
            .success()
            .stdout(contains("Hooks kitbag applied"));

        sb.cli().arg("undo").assert().success();
        assert!(!script.exists(), "{}", t.id);
        let left = fs::read_to_string(sb.home().join(t.written)).ok();
        assert!(
            left.is_none_or(|l| !l.contains("echo started")),
            "{}: hooks left behind",
            t.id
        );
    }
}

#[test]
fn the_users_own_hooks_and_settings_stay_through_apply_and_undo() {
    for t in TOOLS {
        let sb = Sandbox::empty();
        // The tool's own hook file, with a hook of the user's and no kitbag one.
        let mine = Tool {
            file: t.written,
            ..*t
        };
        setup(&sb, &mine);
        let own = {
            let mut v: Value = sb.json(&format!("home/{}", t.written));
            let e = if t.id == "cursor" || t.id == "copilot" {
                "sessionStart"
            } else {
                "SessionStart"
            };
            let root = if t.id == "antigravity" {
                "mine"
            } else {
                t.root
            };
            let list = v[root][e].as_array_mut().unwrap();
            list.clear();
            list.push(match t.id {
                "cursor" | "copilot" | "antigravity" => json!({"command": "echo mine"}),
                _ => json!({"hooks": [{"type": "command", "command": "echo mine"}]}),
            });
            v
        };
        write(
            &sb.home().join(t.written),
            &serde_json::to_string_pretty(&own).unwrap(),
        );
        let before = sb.read(&format!("home/{}", t.written));
        let prof = profile(&sb.path("p"));
        sb.cli()
            .args(["apply", &prof, "--to", t.id, "--apply-hooks", "--yes"])
            .assert()
            .success();
        let after = sb.read(&format!("home/{}", t.written));
        assert!(
            after.contains("echo mine") && after.contains("echo started"),
            "{}",
            t.id
        );
        if t.id == "gemini-cli" {
            assert!(after.contains("\"theme\""));
        }
        sb.cli().arg("undo").assert().success();
        assert_eq!(sb.read(&format!("home/{}", t.written)), before, "{}", t.id);
    }
}

#[test]
fn a_profile_without_hooks_removes_none() {
    for t in TOOLS {
        let sb = Sandbox::empty();
        let mine = Tool {
            file: t.written,
            ..*t
        };
        setup(&sb, &mine);
        let before = sb.read(&format!("home/{}", t.written));
        write(&sb.path("p/kitbag.toml"), "kitbag = 1\nname = \"p\"\n");
        write(&sb.path("p/instructions.md"), "Be brief.\n");
        sb.cli()
            .args([
                "apply",
                &sb.path("p").display().to_string(),
                "--to",
                t.id,
                "--apply-hooks",
                "--yes",
            ])
            .assert()
            .success();
        assert_eq!(sb.read(&format!("home/{}", t.written)), before, "{}", t.id);
    }
}

#[test]
fn hooks_from_a_git_source_need_trust_source_and_apply_hooks() {
    if !has_git() {
        return;
    }
    for t in TOOLS {
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
        let wrote = |sb: &Sandbox| sb.home().join(t.written).exists();
        sb.cli()
            .args(["apply", &url, "--to", t.id, "--yes"])
            .assert()
            .success()
            .stdout(contains("remote repository").and(contains("echo started")));
        assert!(!wrote(&sb), "{}", t.id);
        sb.cli()
            .args(["apply", &url, "--to", t.id, "--yes", "--apply-hooks"])
            .assert()
            .success()
            .stdout(contains("--trust-source"));
        assert!(!wrote(&sb), "{}", t.id);
        sb.cli()
            .args(["apply", &url, "--to", t.id, "--yes", "--trust-source"])
            .assert()
            .success();
        assert!(!wrote(&sb), "{}", t.id);
        sb.cli()
            .args([
                "apply",
                &url,
                "--to",
                t.id,
                "--yes",
                "--trust-source",
                "--apply-hooks",
            ])
            .assert()
            .success();
        assert!(wrote(&sb), "{}", t.id);
    }
}

#[test]
fn doctor_flags_a_hook_that_changed_since_it_was_applied() {
    for t in TOOLS {
        let sb = Sandbox::empty();
        let prof = profile(&sb.path("p"));
        sb.cli()
            .args(["apply", &prof, "--to", t.id, "--apply-hooks", "--yes"])
            .assert()
            .success();
        sb.cli().args(["doctor", "--to", t.id]).assert().success();
        let path = sb.home().join(t.written);
        let text = fs::read_to_string(&path).unwrap();
        fs::write(&path, text.replace("20", "21")).unwrap();
        sb.cli()
            .args(["doctor", "--to", t.id])
            .assert()
            .code(1)
            .stdout(contains("no longer matches what kitbag applied"));
        // A script edited by hand is flagged too.
        fs::write(&path, text).unwrap();
        write(
            &sb.home().join(t.config_dir).join("hooks/kitbag/check.sh"),
            "#!/bin/sh\necho tampered\n",
        );
        sb.cli()
            .args(["doctor", "--to", t.id])
            .assert()
            .code(1)
            .stdout(contains("check.sh"));
    }
}

#[test]
fn events_a_tool_lacks_are_reported_and_its_own_are_carried() {
    let sb = Sandbox::empty();
    write(
        &sb.path("p/kitbag.toml"),
        "kitbag = 1\nname = \"p\"\n\n[[hooks]]\nevent = \"claude-code:FileChanged\"\ncommand = \"echo claude\"\n\n[[hooks]]\nevent = \"cursor:beforeShellExecution\"\ncommand = \"echo cursor\"\n\n[[hooks]]\nevent = \"post-compact\"\ncommand = \"echo compacted\"\n",
    );
    let prof = sb.path("p").display().to_string();
    sb.cli()
        .args([
            "apply",
            &prof,
            "--to",
            "cursor,gemini-cli",
            "--apply-hooks",
            "--yes",
        ])
        .assert()
        .success()
        .stdout(
            contains("no equivalent event in Cursor")
                .and(contains("no equivalent event in Gemini CLI")),
        );
    let c = sb.json("home/.cursor/hooks.json");
    assert_eq!(
        c["hooks"]["beforeShellExecution"][0]["command"],
        "echo cursor"
    );
    assert!(c["hooks"].get("FileChanged").is_none());
    // Cursor has no post-compact; Gemini CLI has neither of the others.
    assert!(c["hooks"].get("postCompact").is_none());
    assert!(!sb.home().join(".gemini/settings.json").exists());
}

#[test]
fn keys_a_tool_does_not_document_keep_the_hook_out_of_it() {
    let sb = Sandbox::empty();
    write(
        &sb.path("p/kitbag.toml"),
        "kitbag = 1\nname = \"p\"\n\n[[hooks]]\nevent = \"stop\"\ncommand = \"echo done\"\nextra = { async = true, statusMessage = \"Wrapping up\" }\n",
    );
    let prof = sb.path("p").display().to_string();
    sb.cli()
        .args([
            "apply",
            &prof,
            "--to",
            "cursor,codex",
            "--apply-hooks",
            "--yes",
        ])
        .assert()
        .success()
        .stdout(contains("'async' setting is not one Cursor documents"));
    assert!(!sb.home().join(".cursor/hooks.json").exists());
    let c = sb.json("home/.codex/hooks.json");
    let h = &c["hooks"]["Stop"][0]["hooks"][0];
    assert_eq!(h["async"], true);
    assert_eq!(h["statusMessage"], "Wrapping up");
}

#[test]
fn tools_with_no_hooks_to_write_warn_and_write_nothing() {
    let sb = Sandbox::empty();
    let prof = profile(&sb.path("p"));
    let out = sb
        .cli()
        .args([
            "apply",
            &prof,
            "--to",
            "opencode,windsurf,claude-desktop",
            "--apply-hooks",
            "--yes",
        ])
        .assert()
        .success()
        .stdout(
            contains("2 hook(s) not written to opencode")
                .and(contains("2 hook(s) not written to Windsurf"))
                .and(contains("2 hook(s) not written to Claude Desktop"))
                .and(contains("echo started")),
        )
        .get_output()
        .stdout
        .clone();
    assert!(String::from_utf8_lossy(&out).contains("JavaScript"));
    assert!(!sb.home().join(".config/opencode/hooks.json").exists());
    // The same without --apply-hooks: still said.
    sb.cli()
        .args(["apply", &prof, "--to", "opencode", "--yes"])
        .assert()
        .success()
        .stdout(contains("not written to opencode"));
}

#[test]
fn project_hooks_find_their_scripts() {
    // (tool, hook file in the project, the directory its commands name)
    let cases = [
        (
            "cursor",
            ".cursor/hooks.json",
            ".cursor/hooks/kitbag/check.sh",
        ),
        (
            "codex",
            ".codex/hooks.json",
            "\"$(git rev-parse --show-toplevel)/.codex/hooks/kitbag\"/check.sh",
        ),
        (
            "gemini-cli",
            ".gemini/settings.json",
            "\"${GEMINI_PROJECT_DIR}/.gemini/hooks/kitbag\"/check.sh",
        ),
        (
            "grok-build",
            ".grok/hooks/kitbag.json",
            "\"${GROK_WORKSPACE_ROOT}/.grok/hooks/kitbag\"/check.sh",
        ),
        (
            "copilot",
            ".github/hooks/kitbag.json",
            ".github/hooks/kitbag/check.sh",
        ),
    ];
    for (id, file, command) in cases {
        let sb = Sandbox::empty();
        let prof = profile(&sb.path("p"));
        sb.cli()
            .args([
                "apply",
                &prof,
                "--to",
                id,
                "--project",
                &sb.project().display().to_string(),
                "--apply-hooks",
                "--yes",
            ])
            .assert()
            .success();
        let text = fs::read_to_string(sb.project().join(file)).unwrap();
        let v: Value = serde_json::from_str(&text).unwrap();
        assert!(text.contains(&command.replace('"', "\\\"")), "{id}: {v}");
        let dir = file.rsplit_once('/').unwrap().0;
        let scripts = if id == "grok-build" || id == "copilot" {
            dir.to_string()
        } else {
            format!("{dir}/hooks")
        };
        assert!(
            sb.project()
                .join(&scripts)
                .join("kitbag/check.sh")
                .is_file(),
            "{id}: {scripts}"
        );
        if id == "copilot" {
            assert_eq!(v["hooks"]["preToolUse"][0]["cwd"], ".");
        }
    }
}

#[test]
fn antigravity_cannot_reach_a_project_script_but_carries_the_rest() {
    let sb = Sandbox::empty();
    let prof = profile(&sb.path("p"));
    sb.cli()
        .args([
            "apply",
            &prof,
            "--to",
            "antigravity",
            "--project",
            &sb.project().display().to_string(),
            "--apply-hooks",
            "--yes",
        ])
        .assert()
        .success()
        .stdout(contains(
            "don't say what a project hook command runs relative to",
        ));
    let v: Value =
        serde_json::from_str(&fs::read_to_string(sb.project().join(".agents/hooks.json")).unwrap())
            .unwrap();
    assert_eq!(v["kitbag"]["SessionStart"][0]["command"], "echo started");
    assert!(v["kitbag"].get("PreToolUse").is_none());
    assert!(!sb.project().join(".agents/hooks").exists());
}

#[test]
fn claude_code_hooks_still_round_trip_and_carry_over_to_another_tool() {
    let sb = Sandbox::empty();
    let home = slashes(&sb.home());
    write(
        &sb.home().join(".claude/settings.json"),
        &serde_json::to_string_pretty(&json!({"hooks": {
            "PreToolUse": [{"matcher": "Bash", "hooks": [
                {"type": "command", "command": format!("{home}/.claude/hooks/check.sh"), "timeout": 20}]}],
            "FileChanged": [{"matcher": ".envrc", "hooks": [
                {"type": "command", "command": "echo changed"}]}],
        }}))
        .unwrap(),
    );
    write(
        &sb.home().join(".claude/hooks/check.sh"),
        "#!/bin/sh\necho checking\n",
    );
    sb.cli()
        .args([
            "capture",
            "--from",
            "claude-code",
            "-o",
            "prof",
            "--include",
            "hooks",
        ])
        .assert()
        .success();
    // Another machine: Claude Code gets the same hooks back, Codex what it has.
    let other = Sandbox::empty();
    let prof = sb.path("prof").display().to_string();
    other
        .cli()
        .args([
            "apply",
            &prof,
            "--to",
            "claude-code,codex",
            "--apply-hooks",
            "--yes",
        ])
        .assert()
        .success()
        .stdout(contains("no equivalent event in Codex"));
    let c = other.json("home/.claude/settings.json");
    assert_eq!(
        c["hooks"]["FileChanged"][0]["hooks"][0]["command"],
        "echo changed"
    );
    assert_eq!(c["hooks"]["PreToolUse"][0]["matcher"], "Bash");
    let x = other.json("home/.codex/hooks.json");
    assert_eq!(x["hooks"]["PreToolUse"][0]["matcher"], "Bash");
    assert_eq!(x["hooks"]["PreToolUse"][0]["hooks"][0]["timeout"], 20);
    assert!(x["hooks"].get("FileChanged").is_none());
}

#[test]
fn status_lists_hooks_and_what_drifted() {
    let sb = Sandbox::empty();
    let prof = profile(&sb.path("p"));
    let args = |extra: &[&str]| {
        let mut v = vec!["status", &prof as &str, "--to", "cursor,codex,opencode"];
        v.extend_from_slice(extra);
        v.iter().map(|s| s.to_string()).collect::<Vec<_>>()
    };
    // Not applied yet.
    sb.cli()
        .args(args(&["--exit-code"]))
        .assert()
        .code(1)
        .stdout(
            contains("not applied")
                .and(contains("echo started"))
                .and(contains("unsupported"))
                .and(contains("JavaScript")),
        );
    sb.cli()
        .args([
            "apply",
            &prof,
            "--to",
            "cursor,codex",
            "--apply-hooks",
            "--yes",
        ])
        .assert()
        .success();
    sb.cli()
        .args(["status", &prof, "--to", "cursor,codex", "--exit-code"])
        .assert()
        .success()
        .stdout(contains("in sync"));
    let path = sb.home().join(".cursor/hooks.json");
    let text = fs::read_to_string(&path).unwrap();
    fs::write(&path, text.replace("20", "21")).unwrap();
    sb.cli()
        .args(["status", &prof, "--to", "cursor,codex", "--exit-code"])
        .assert()
        .code(1)
        .stdout(contains("changed"));
}

#[test]
fn status_does_not_call_a_hook_apply_refused_in_sync() {
    let sb = Sandbox::empty();
    let dir = sb.path("p");
    write(
        &dir.join("kitbag.toml"),
        "kitbag = 1\nname = \"p\"\n\n[[hooks]]\nevent = \"session-start\"\ncommand = \"echo started\"\n\n[[hooks]]\nevent = \"session-start\"\ncommand = \"curl -H 'x-api-key: [REDACTED:anthropic-key]' https://example.invalid\"\n",
    );
    let prof = dir.display().to_string();
    // Apply warns about the one it will not write and writes the other.
    sb.cli()
        .args(["apply", &prof, "--to", "cursor", "--apply-hooks", "--yes"])
        .assert()
        .success()
        .stdout(contains("hook not applied").and(contains("[REDACTED")));
    let written = sb.read("home/.cursor/hooks.json");
    assert!(written.contains("echo started"), "{written}");
    assert!(!written.contains("REDACTED"), "{written}");
    // Status says so too: the refused hooks are not in sync and count as drift.
    let out = sb
        .cli()
        .args(["status", &prof, "--to", "cursor", "--exit-code"])
        .assert()
        .code(1)
        .stdout(contains("not applied"))
        .get_output()
        .stdout
        .clone();
    let out = String::from_utf8(out).unwrap();
    for line in out.lines().filter(|l| l.contains("hook")) {
        let in_sync = line.contains("in sync");
        assert_eq!(
            in_sync,
            line.contains("echo started"),
            "only the hook that was written is in sync: {line}"
        );
    }
    assert!(out.contains("REDACTED"), "{out}");
}

#[test]
fn project_hooks_are_captured_with_their_scripts() {
    let sb = Sandbox::empty();
    write(
        &sb.project().join(".cursor/hooks.json"),
        &serde_json::to_string_pretty(&json!({"version": 1, "hooks": {
            "stop": [{"command": ".cursor/hooks/done.sh"}],
            "sessionStart": [{"command": "./.cursor/hooks/start.sh"}],
        }}))
        .unwrap(),
    );
    write(
        &sb.project().join(".cursor/hooks/done.sh"),
        "#!/bin/sh\necho done\n",
    );
    write(
        &sb.project().join(".cursor/hooks/start.sh"),
        "#!/bin/sh\necho start\n",
    );
    sb.cli()
        .args([
            "capture",
            "--from",
            "cursor",
            "--project",
            &sb.project().display().to_string(),
            "--include",
            "hooks",
            "-o",
            "prof",
        ])
        .assert()
        .success();
    let p = profile_io::load(&sb.path("prof")).unwrap();
    let stop = p.hooks.iter().find(|h| h.event == "stop").unwrap();
    assert_eq!(stop.command, "{{hooks_dir}}/done.sh");
    let start = p.hooks.iter().find(|h| h.event == "session-start").unwrap();
    assert_eq!(start.command, "{{hooks_dir}}/start.sh", "{:?}", p.hooks);
    assert!(p.hook_files.iter().any(|f| f.path == "done.sh"));
}

#[test]
fn a_cursor_user_hook_may_name_its_script_relative_to_the_cursor_directory() {
    let sb = Sandbox::empty();
    write(
        &sb.home().join(".cursor/hooks.json"),
        &serde_json::to_string_pretty(&json!({"version": 1, "hooks": {
            "stop": [{"command": "./hooks/done.sh"}],
        }}))
        .unwrap(),
    );
    write(
        &sb.home().join(".cursor/hooks/done.sh"),
        "#!/bin/sh\necho done\n",
    );
    sb.cli()
        .args([
            "capture",
            "--from",
            "cursor",
            "--include",
            "hooks",
            "-o",
            "prof",
        ])
        .assert()
        .success();
    let p = profile_io::load(&sb.path("prof")).unwrap();
    assert_eq!(p.hooks[0].command, "{{hooks_dir}}/done.sh");
    assert!(p.hook_files.iter().any(|f| f.path == "done.sh"));
}
