//! Hooks: opt-in capture, opt-in apply, trust for git sources, undo, doctor.

mod common;

use std::fs;
use std::path::Path;

use common::*;
use kitbag::adapters::Scope;
use kitbag::profile_io;
use predicates::prelude::*;
use predicates::str::contains;
use serde_json::{Value, json};

const TOKEN: &str = "ghp_FAKE0123456789abcdefghijklmnopqrstuv";

fn slashes(p: &Path) -> String {
    p.display().to_string().replace('\\', "/")
}

fn write(path: &Path, content: &str) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, content).unwrap();
}

/// A Claude Code setup with hooks: a script hook with a matcher and timeout,
/// an inline one, one with a secret, a Claude-only event, and an http hook.
fn setup_claude(sb: &Sandbox, with_hooks: bool) {
    let home = slashes(&sb.home());
    let mut settings = json!({"model": "opus"});
    if with_hooks {
        settings["hooks"] = json!({
            "PreToolUse": [{"matcher": "Bash", "hooks": [
                {"type": "command", "command": format!("{home}/.claude/hooks/check.sh"), "timeout": 20}
            ]}],
            "SessionStart": [{"hooks": [
                {"type": "command", "command": "echo started", "statusMessage": "Starting"}
            ]}],
            "Notification": [{"hooks": [
                {"type": "command", "command": format!("curl -s -H \"Authorization: Bearer {TOKEN}\" https://hooks.example.com/n")}
            ]}],
            "FileChanged": [{"matcher": ".envrc", "hooks": [
                {"type": "command", "command": format!("bash {home}/.claude/hooks/lint/env.sh --fix")}
            ]}],
            "Stop": [{"hooks": [
                {"type": "http", "url": "https://hooks.example.com/stop"},
                {"type": "command", "command": format!("sh {home}/.claude/hooks/secret.sh")}
            ]}],
        });
    }
    write(
        &sb.home().join(".claude/settings.json"),
        &serde_json::to_string_pretty(&settings).unwrap(),
    );
    write(
        &sb.home().join(".claude/hooks/check.sh"),
        "#!/bin/sh\necho checking\n",
    );
    write(
        &sb.home().join(".claude/hooks/secret.sh"),
        &format!("#!/bin/sh\nexport DEPLOY_TOKEN={TOKEN}\n"),
    );
    write(
        &sb.home().join(".claude/hooks/lint/env.sh"),
        "#!/bin/sh\necho linting \"$1\"\n",
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        for rel in [
            ".claude/hooks/check.sh",
            ".claude/hooks/lint/env.sh",
            ".claude/hooks/secret.sh",
        ] {
            let p = sb.home().join(rel);
            fs::set_permissions(&p, fs::Permissions::from_mode(0o755)).unwrap();
        }
    }
}

/// Capture the Claude Code setup of `sb` into `<sb>/prof`, with or without hooks.
fn capture(sb: &Sandbox, include: bool) -> assert_cmd::assert::Assert {
    let mut c = sb.cli();
    c.args(["capture", "--from", "claude-code", "-o", "prof"]);
    if include {
        c.args(["--include", "hooks"]);
    }
    c.assert().success()
}

/// A captured profile with hooks, in its own sandbox.
fn profile_with_hooks() -> (Sandbox, std::path::PathBuf) {
    let sb = Sandbox::empty();
    setup_claude(&sb, true);
    capture(&sb, true);
    let dir = sb.path("prof");
    (sb, dir)
}

fn settings(sb: &Sandbox) -> Value {
    sb.json("home/.claude/settings.json")
}

fn files_of(dir: &Path) -> Vec<(String, String)> {
    kitbag::plan_fs::walk_files(dir, 1000)
        .unwrap()
        .into_iter()
        .map(|rel| {
            let t = fs::read_to_string(kitbag::plan_fs::join_rel(dir, &rel)).unwrap();
            (rel, t)
        })
        .collect()
}

#[test]
fn capture_without_the_flag_notes_hooks_and_carries_none() {
    let sb = Sandbox::empty();
    setup_claude(&sb, true);
    capture(&sb, false).stdout(contains("--include hooks"));
    let toml = sb.read("prof/kitbag.toml");
    assert!(!toml.contains("hooks"), "{toml}");
    assert!(!sb.path("prof/hooks").exists());

    // The profile is byte for byte the one from a setup without hooks.
    let plain = Sandbox::empty();
    setup_claude(&plain, false);
    capture(&plain, false);
    assert_eq!(files_of(&sb.path("prof")), files_of(&plain.path("prof")));
}

#[test]
fn capture_with_the_flag_takes_hooks_and_scripts_and_redacts_secrets() {
    let sb = Sandbox::empty();
    setup_claude(&sb, true);
    capture(&sb, true)
        .stdout(contains("5 hook(s)"))
        .stdout(contains("http hook"));
    let p = profile_io::load(&sb.path("prof")).unwrap();
    assert_eq!(p.hooks.len(), 5, "{:?}", p.hooks);
    let pre = p.hooks.iter().find(|h| h.event == "pre-tool").unwrap();
    assert_eq!(pre.matcher.as_deref(), Some("Bash"));
    assert_eq!(pre.timeout, Some(20));
    assert_eq!(pre.command, "{{hooks_dir}}/check.sh");
    let start = p.hooks.iter().find(|h| h.event == "session-start").unwrap();
    assert_eq!(start.command, "echo started");
    assert_eq!(start.extra["statusMessage"], "Starting");
    let changed = p
        .hooks
        .iter()
        .find(|h| h.event == "claude-code:FileChanged")
        .unwrap();
    assert_eq!(changed.command, "bash {{hooks_dir}}/lint/env.sh --fix");
    let paths: Vec<&str> = p.hook_files.iter().map(|f| f.path.as_str()).collect();
    assert_eq!(paths, ["check.sh", "lint/env.sh", "secret.sh"]);
    #[cfg(unix)]
    assert!(p.hook_files.iter().all(|f| f.executable));
    // No secret anywhere in the profile, scripts included.
    assert!(!all_text_under(&sb.path("prof")).contains(TOKEN));
    let notify = p.hooks.iter().find(|h| h.event == "notification").unwrap();
    assert!(notify.command.contains("[REDACTED"), "{}", notify.command);
    // Nothing of the machine's own paths is left.
    assert!(!all_text_under(&sb.path("prof")).contains(&slashes(&sb.home())));
}

#[test]
fn recapturing_without_the_flag_keeps_the_profiles_hooks() {
    let (sb, dir) = profile_with_hooks();
    sb.cli()
        .args(["capture", "--from", "claude-code", "-o", "prof", "--force"])
        .assert()
        .success()
        .stdout(contains("kept the 5 hook(s)"));
    assert_eq!(profile_io::load(&dir).unwrap().hooks.len(), 5);
    assert!(dir.join("hooks/check.sh").is_file());
}

#[test]
fn a_local_profile_needs_apply_hooks_and_shows_every_command() {
    let (_a, prof) = profile_with_hooks();
    let sb = Sandbox::empty();
    let prof = prof.display().to_string();
    sb.cli()
        .args(["apply", &prof, "--to", "claude-code", "--yes"])
        .assert()
        .success()
        .stdout(
            contains("--apply-hooks")
                .and(contains("echo started"))
                .and(contains("{{hooks_dir}}/check.sh"))
                .and(contains("hooks/check.sh")),
        );
    assert!(!sb.home().join(".claude/hooks").exists());
    assert!(settings(&sb).get("hooks").is_none());

    // A dry run with the flag still writes nothing.
    sb.cli()
        .args([
            "apply",
            &prof,
            "--to",
            "claude-code",
            "--apply-hooks",
            "--dry-run",
        ])
        .assert()
        .success()
        .stdout(contains("nothing written"));
    assert!(!sb.home().join(".claude/hooks").exists());
    assert!(settings(&sb).get("hooks").is_none());

    sb.cli()
        .args([
            "apply",
            &prof,
            "--to",
            "claude-code",
            "--apply-hooks",
            "--yes",
        ])
        .assert()
        .success();
    let s = settings(&sb);
    let pre = &s["hooks"]["PreToolUse"][0];
    assert_eq!(pre["matcher"], "Bash");
    assert_eq!(pre["hooks"][0]["type"], "command");
    assert_eq!(pre["hooks"][0]["timeout"], 20);
    assert_eq!(
        pre["hooks"][0]["command"],
        format!("{}/.claude/hooks/kitbag/check.sh", slashes(&sb.home()))
    );
    assert_eq!(
        s["hooks"]["SessionStart"][0]["hooks"][0]["command"],
        "echo started"
    );
    assert_eq!(
        s["hooks"]["SessionStart"][0]["hooks"][0]["statusMessage"],
        "Starting"
    );
    // What capture redacted a secret from is not applied, nor is its script.
    assert!(s["hooks"].get("Notification").is_none());
    assert!(s["hooks"].get("Stop").is_none());
    assert!(!sb.path("home/.claude/hooks/kitbag/secret.sh").exists());
    assert_eq!(
        s["hooks"]["FileChanged"][0]["hooks"][0]["command"],
        format!(
            "bash {}/.claude/hooks/kitbag/lint/env.sh --fix",
            slashes(&sb.home())
        )
    );
    assert!(
        sb.read("home/.claude/hooks/kitbag/check.sh")
            .contains("echo checking")
    );
    assert!(sb.path("home/.claude/hooks/kitbag/lint/env.sh").is_file());
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = fs::metadata(sb.path("home/.claude/hooks/kitbag/check.sh"))
            .unwrap()
            .permissions()
            .mode();
        assert_ne!(mode & 0o100, 0, "script is executable");
    }

    // Applying again changes nothing.
    sb.cli()
        .args([
            "apply",
            &prof,
            "--to",
            "claude-code",
            "--apply-hooks",
            "--yes",
        ])
        .assert()
        .success()
        .stdout(contains("Up to date"));

    // Undo takes the hooks and the scripts away again.
    sb.cli().arg("undo").assert().success();
    assert!(settings(&sb).get("hooks").is_none());
    assert!(!sb.home().join(".claude/hooks").exists());
}

#[test]
fn applying_adds_to_the_users_own_hooks_and_undo_restores_them() {
    let (_a, prof) = profile_with_hooks();
    let sb = Sandbox::empty();
    let own = json!({
        "theme": "dark",
        "hooks": {
            "PreToolUse": [{"matcher": "Bash", "hooks": [{"type": "command", "command": "my-own-check"}]}],
            "Stop": [{"hooks": [{"type": "command", "command": "say done"}]}],
        },
    });
    let own_text = serde_json::to_string_pretty(&own).unwrap() + "\n";
    write(&sb.home().join(".claude/settings.json"), &own_text);
    let prof = prof.display().to_string();
    sb.cli()
        .args([
            "apply",
            &prof,
            "--to",
            "claude-code",
            "--apply-hooks",
            "--yes",
        ])
        .assert()
        .success();
    let s = settings(&sb);
    assert_eq!(s["theme"], "dark");
    let list = s["hooks"]["PreToolUse"][0]["hooks"].as_array().unwrap();
    assert_eq!(list[0]["command"], "my-own-check");
    assert_eq!(list.len(), 2, "kitbag's hook joins the Bash group");
    assert_eq!(s["hooks"]["Stop"][0]["hooks"][0]["command"], "say done");

    sb.cli().arg("undo").assert().success();
    assert_eq!(sb.read("home/.claude/settings.json"), own_text);
    assert!(!sb.home().join(".claude/hooks").exists());
}

#[test]
fn a_profile_without_hooks_leaves_the_users_hooks_alone() {
    let sb = Sandbox::empty();
    setup_claude(&sb, true);
    let before = sb.read("home/.claude/settings.json");
    sb.cli().args(["init", "plain"]).assert().success();
    sb.cli()
        .args([
            "apply",
            "plain",
            "--to",
            "claude-code",
            "--apply-hooks",
            "--yes",
        ])
        .assert()
        .success();
    assert_eq!(
        settings(&sb)["hooks"],
        serde_json::from_str::<Value>(&before).unwrap()["hooks"]
    );
}

fn git_url_of(sb: &Sandbox, prof: &Path) -> String {
    let files = files_of(prof);
    let files: Vec<(&str, &str)> = files
        .iter()
        .map(|(a, b)| (a.as_str(), b.as_str()))
        .collect();
    sb.git_profile("repo", &files)
}

#[test]
fn hooks_from_a_git_source_need_trust_source_and_apply_hooks() {
    if !has_git() {
        return;
    }
    let (_a, prof) = profile_with_hooks();
    let sb = Sandbox::empty();
    let url = git_url_of(&sb, &prof);
    let has_hooks = |sb: &Sandbox| {
        sb.home().join(".claude/hooks").exists()
            || sb.home().join(".claude/settings.json").exists()
                && settings(sb).get("hooks").is_some()
    };

    // No flags: withheld, listed in full.
    sb.cli()
        .args(["apply", &url, "--to", "claude-code", "--yes"])
        .assert()
        .success()
        .stdout(contains("remote repository").and(contains("echo started")));
    assert!(!has_hooks(&sb));

    // --apply-hooks alone does not trust the source.
    sb.cli()
        .args([
            "apply",
            &url,
            "--to",
            "claude-code",
            "--yes",
            "--apply-hooks",
        ])
        .assert()
        .success()
        .stdout(contains("--trust-source"));
    assert!(!has_hooks(&sb));

    // --trust-source alone does not apply hooks.
    sb.cli()
        .args([
            "apply",
            &url,
            "--to",
            "claude-code",
            "--yes",
            "--trust-source",
        ])
        .assert()
        .success()
        .stdout(contains("--apply-hooks"));
    assert!(!has_hooks(&sb));
    sb.cli().arg("undo").assert().success();

    // Both: applied.
    sb.cli()
        .args([
            "apply",
            &url,
            "--to",
            "claude-code",
            "--yes",
            "--trust-source",
            "--apply-hooks",
        ])
        .assert()
        .success();
    assert_eq!(
        settings(&sb)["hooks"]["SessionStart"][0]["hooks"][0]["command"],
        "echo started"
    );
    assert!(sb.path("home/.claude/hooks/kitbag/check.sh").is_file());
}

#[test]
fn doctor_flags_a_changed_hook_but_not_the_users_own() {
    let (_a, prof) = profile_with_hooks();
    let sb = Sandbox::empty();
    let prof = prof.display().to_string();
    sb.cli()
        .args([
            "apply",
            &prof,
            "--to",
            "claude-code",
            "--apply-hooks",
            "--yes",
        ])
        .assert()
        .success();
    sb.cli()
        .args(["doctor", "--to", "claude-code"])
        .assert()
        .success()
        .stdout(contains("Hooks kitbag applied"));

    // A hook added by hand is not looked at.
    let mut s = settings(&sb);
    s["hooks"]["Stop"] = json!([{"hooks": [{"type": "command", "command": "say done"}]}]);
    s["hooks"]["PreToolUse"][0]["hooks"]
        .as_array_mut()
        .unwrap()
        .push(json!({"type": "command", "command": "mine"}));
    write(
        &sb.home().join(".claude/settings.json"),
        &serde_json::to_string_pretty(&s).unwrap(),
    );
    sb.cli()
        .args(["doctor", "--to", "claude-code"])
        .assert()
        .success();

    // A script kitbag wrote, edited.
    let script = sb.home().join(".claude/hooks/kitbag/check.sh");
    write(&script, "#!/bin/sh\necho tampered\n");
    sb.cli()
        .args(["doctor", "--to", "claude-code"])
        .assert()
        .code(1)
        .stdout(contains("no longer matches what kitbag applied").and(contains("check.sh")));
    // Put it back, then change a hook's timeout.
    sb.cli()
        .args([
            "apply",
            &prof,
            "--to",
            "claude-code",
            "--apply-hooks",
            "--yes",
        ])
        .assert()
        .success();
    sb.cli()
        .args(["doctor", "--to", "claude-code"])
        .assert()
        .success();
    let mut s = settings(&sb);
    s["hooks"]["SessionStart"][0]["hooks"][0]["timeout"] = json!(1);
    write(
        &sb.home().join(".claude/settings.json"),
        &serde_json::to_string_pretty(&s).unwrap(),
    );
    sb.cli()
        .args(["doctor", "--to", "claude-code"])
        .assert()
        .code(1)
        .stdout(contains("echo started").and(contains("no longer matches")));
    // And a removed one.
    let mut s = settings(&sb);
    s["hooks"].as_object_mut().unwrap().remove("SessionStart");
    write(
        &sb.home().join(".claude/settings.json"),
        &serde_json::to_string_pretty(&s).unwrap(),
    );
    sb.cli()
        .args(["doctor", "--to", "claude-code"])
        .assert()
        .code(1)
        .stdout(contains("echo started"));
}

#[test]
fn project_hooks_use_the_project_variable() {
    let sb = Sandbox::empty();
    write(
        &sb.project().join(".claude/settings.json"),
        &serde_json::to_string_pretty(&json!({"hooks": {"PostToolUse": [{"matcher": "Edit|Write", "hooks": [
            {"type": "command", "command": "\"${CLAUDE_PROJECT_DIR}/.claude/hooks/typecheck.sh\""}
        ]}]}}))
        .unwrap(),
    );
    write(
        &sb.project().join(".claude/hooks/typecheck.sh"),
        "#!/bin/sh\ntsc --noEmit\n",
    );
    let project = sb.project().display().to_string();
    sb.cli()
        .args([
            "capture",
            "--from",
            "claude-code",
            "--project",
            &project,
            "-o",
            "prof",
            "--include",
            "hooks",
        ])
        .assert()
        .success();
    let p = profile_io::load(&sb.path("prof")).unwrap();
    assert_eq!(p.hooks[0].event, "post-tool");
    assert_eq!(p.hooks[0].command, "\"{{hooks_dir}}/typecheck.sh\"");
    assert_eq!(p.hook_files[0].path, "typecheck.sh");

    let other = sb.path("other");
    fs::create_dir_all(&other).unwrap();
    let other_s = other.display().to_string();
    sb.cli()
        .args([
            "apply",
            &sb.path("prof").display().to_string(),
            "--to",
            "claude-code",
            "--project",
            &other_s,
            "--apply-hooks",
            "--yes",
        ])
        .assert()
        .success();
    let s: Value =
        serde_json::from_str(&fs::read_to_string(other.join(".claude/settings.json")).unwrap())
            .unwrap();
    assert_eq!(
        s["hooks"]["PostToolUse"][0]["hooks"][0]["command"],
        "\"${CLAUDE_PROJECT_DIR}/.claude/hooks/kitbag/typecheck.sh\""
    );
    assert!(other.join(".claude/hooks/kitbag/typecheck.sh").is_file());
}

#[test]
fn tools_without_hooks_say_so_instead_of_dropping_them() {
    let (_a, prof) = profile_with_hooks();
    let sb = Sandbox::empty();
    sb.cli()
        .args([
            "apply",
            &prof.display().to_string(),
            "--to",
            "opencode,windsurf,claude-desktop",
            "--project",
            &sb.project().display().to_string(),
            "--apply-hooks",
            "--yes",
        ])
        .assert()
        .success()
        .stdout(
            contains("not written to opencode")
                .and(contains("not written to Windsurf"))
                .and(contains("echo started")),
        );
    assert!(!sb.project().join(".opencode").exists());
}

#[test]
fn a_hook_cannot_name_a_script_the_profile_lacks() {
    let sb = Sandbox::empty();
    write(
        &sb.path("p/kitbag.toml"),
        "kitbag = 1\nname = \"p\"\n\n[[hooks]]\nevent = \"stop\"\ncommand = \"{{hooks_dir}}/nope.sh\"\n",
    );
    sb.cli()
        .args(["doctor", "p"])
        .assert()
        .code(1)
        .stdout(contains("nope.sh"));
}

#[test]
fn hooks_round_trip_through_a_saved_profile() {
    let sb = Sandbox::empty();
    let mut p = kitbag::model::Profile::new("rt");
    let mut extra = serde_json::Map::new();
    extra.insert("args".into(), json!(["{{hooks_dir}}/a.js", "--x"]));
    extra.insert("async".into(), json!(true));
    p.hooks.push(kitbag::model::Hook {
        event: "claude-code:PostToolBatch".into(),
        matcher: Some("a|b".into()),
        command: "node".into(),
        timeout: Some(5),
        extra,
    });
    p.hook_files.push(kitbag::model::SkillFile {
        path: "a.js".into(),
        content: b"console.log(1)\n".to_vec(),
        executable: false,
    });
    profile_io::save(&p, &sb.path("rt"), false).unwrap();
    let back = profile_io::load(&sb.path("rt")).unwrap();
    assert_eq!(back.hooks, p.hooks);
    assert_eq!(back.hook_files, p.hook_files);
    // The environment of the plan: a profile without hooks saves without a trace.
    let none = kitbag::model::Profile::new("none");
    assert!(!profile_io::manifest_toml(&none).contains("hooks"));
    let _ = Scope::User;
}

#[test]
fn scripts_outside_the_claude_dir_are_not_copied() {
    let sb = Sandbox::empty();
    let home = slashes(&sb.home());
    write(&sb.home().join("bin/lint.sh"), "#!/bin/sh\necho lint\n");
    write(
        &sb.home().join(".claude/settings.json"),
        &serde_json::to_string(&json!({"hooks": {"Stop": [{"hooks": [
            {"type": "command", "command": format!("{home}/bin/lint.sh")}
        ]}]}}))
        .unwrap(),
    );
    capture(&sb, true).stdout(contains("outside"));
    assert!(!sb.path("prof/hooks").exists());
    let p = profile_io::load(&sb.path("prof")).unwrap();
    assert_eq!(p.hooks.len(), 1);
    assert!(p.hook_files.is_empty());
}

#[cfg(unix)]
#[test]
fn a_project_script_linking_outside_the_project_is_not_copied() {
    let sb = Sandbox::empty();
    write(&sb.path("elsewhere/steal.sh"), "#!/bin/sh\necho outside\n");
    fs::create_dir_all(sb.project().join(".claude/hooks")).unwrap();
    std::os::unix::fs::symlink(
        sb.path("elsewhere/steal.sh"),
        sb.project().join(".claude/hooks/steal.sh"),
    )
    .unwrap();
    write(
        &sb.project().join(".claude/settings.json"),
        &serde_json::to_string(&json!({"hooks": {"Stop": [{"hooks": [
            {"type": "command", "command": "${CLAUDE_PROJECT_DIR}/.claude/hooks/steal.sh"}
        ]}]}}))
        .unwrap(),
    );
    sb.cli()
        .args([
            "capture",
            "--from",
            "claude-code",
            "--project",
            &sb.project().display().to_string(),
            "-o",
            "prof",
            "--include",
            "hooks",
        ])
        .assert()
        .success()
        .stdout(contains("symbolic link"));
    assert!(!all_text_under(&sb.path("prof")).contains("echo outside"));
}

#[test]
fn exec_form_hooks_keep_their_args_and_follow_the_script() {
    let sb = Sandbox::empty();
    let home = slashes(&sb.home());
    write(
        &sb.home().join(".claude/hooks/fmt.js"),
        "console.log('fmt')\n",
    );
    write(
        &sb.home().join(".claude/settings.json"),
        &serde_json::to_string(&json!({"hooks": {"PostToolUse": [{"matcher": "Edit", "hooks": [
            {"type": "command", "command": "node", "args": [format!("{home}/.claude/hooks/fmt.js"), "--fix"], "async": true}
        ]}]}}))
        .unwrap(),
    );
    capture(&sb, true);
    let p = profile_io::load(&sb.path("prof")).unwrap();
    assert_eq!(
        p.hooks[0].extra["args"],
        json!(["{{hooks_dir}}/fmt.js", "--fix"])
    );
    assert_eq!(p.hooks[0].extra["async"], json!(true));
    let target = Sandbox::empty();
    target
        .cli()
        .args([
            "apply",
            &sb.path("prof").display().to_string(),
            "--to",
            "claude-code",
            "--apply-hooks",
            "--yes",
        ])
        .assert()
        .success();
    let h = &settings(&target)["hooks"]["PostToolUse"][0]["hooks"][0];
    assert_eq!(h["command"], "node");
    assert_eq!(
        h["args"][0],
        format!("{}/.claude/hooks/kitbag/fmt.js", slashes(&target.home()))
    );
    assert_eq!(h["async"], true);
}

#[test]
fn inspect_lists_hooks_in_full() {
    let (_a, prof) = profile_with_hooks();
    let sb = Sandbox::empty();
    sb.cli()
        .args([
            "inspect",
            &prof.display().to_string(),
            "--to",
            "claude-code",
        ])
        .assert()
        .success()
        .stdout(
            contains("hooks (5)")
                .and(contains("echo started"))
                .and(contains("--apply-hooks")),
        );
}
