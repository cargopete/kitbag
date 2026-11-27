//! A server the profile disables for a tool (`disabled_on`, `kitbag disable
//! --on`) is switched off with the tool's own flag or list where it has one,
//! instead of being removed: the entry stays, capture reads it back as off for
//! that tool, and enabling it again clears the switch.

mod common;

use std::fs;

use common::*;
use predicates::prelude::*;
use predicates::str::contains;
use serde_json::Value;

fn profile(name: &str, heavy_extra: &str) -> String {
    format!(
        "kitbag = 1\nname = \"{name}\"\n\n[mcp_servers.heavy]\ncommand = \"heavy-mcp\"\nargs = [\"--big\"]\n{heavy_extra}\n[mcp_servers.light]\ncommand = \"light-mcp\"\n"
    )
}

fn write_profile(sb: &Sandbox, dir: &str, text: &str) {
    fs::create_dir_all(sb.path(dir)).unwrap();
    fs::write(sb.path(&format!("{dir}/kitbag.toml")), text).unwrap();
}

fn apply_args(sb: &Sandbox, args: &[&str]) -> String {
    let out = sb.cli().args(args).output().unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    assert!(
        out.status.success(),
        "{stdout}{}",
        String::from_utf8_lossy(&out.stderr)
    );
    stdout
}

fn apply(sb: &Sandbox, to: &str) -> String {
    apply_args(sb, &["apply", "p", "--to", to, "--yes"])
}

fn apply_project(sb: &Sandbox, to: &str) -> String {
    apply_args(
        sb,
        &["apply", "p", "--project", "project", "--to", to, "--yes"],
    )
}

fn undo(sb: &Sandbox) {
    sb.cli().arg("undo").assert().success();
}

/// The captured profile of one tool, as text.
fn captured(sb: &Sandbox, from: &str, extra: &[&str]) -> String {
    let dir = sb.path("cap");
    let _ = fs::remove_dir_all(&dir);
    let mut args = vec!["capture", "--from", from, "-o", "cap"];
    args.extend_from_slice(extra);
    apply_args(sb, &args);
    sb.read("cap/kitbag.toml")
}

fn server<'a>(doc: &'a Value, path: &[&str], name: &str) -> &'a Value {
    let mut cur = doc;
    for k in path {
        cur = &cur[*k];
    }
    &cur[name]
}

/// A tool whose servers are objects in a JSON file, with a flag on the entry.
struct JsonTool {
    id: &'static str,
    file: &'static str,
    path: &'static [&'static str],
    flag: (&'static str, Value),
}


fn zed_settings() -> &'static str {
    // The CLI uses the real OS. Under KITBAG_HOME, Zed on Windows writes to
    // <home>/AppData/Roaming/Zed, not ~/.config/zed.
    if cfg!(windows) {
        "home/AppData/Roaming/Zed/settings.json"
    } else {
        "home/.config/zed/settings.json"
    }
}

fn json_tools() -> Vec<JsonTool> {
    vec![
        JsonTool {
            id: "cline",
            file: "home/.cline/data/settings/cline_mcp_settings.json",
            path: &["mcpServers"],
            flag: ("disabled", Value::Bool(true)),
        },
        JsonTool {
            id: "antigravity",
            file: "home/.gemini/config/mcp_config.json",
            path: &["mcpServers"],
            flag: ("disabled", Value::Bool(true)),
        },
        JsonTool {
            id: "zed",
            file: zed_settings(),
            path: &["context_servers"],
            flag: ("enabled", Value::Bool(false)),
        },
        JsonTool {
            id: "opencode",
            file: "home/.config/opencode/opencode.json",
            path: &["mcp", "servers"],
            flag: ("disabled", Value::Bool(true)),
        },
    ]
}

#[test]
fn a_disabled_server_is_turned_off_with_the_flag_of_each_json_tool() {
    for t in json_tools() {
        let sb = Sandbox::empty();
        write_profile(&sb, "p", &profile("shared", ""));
        apply(&sb, t.id);
        let before = sb.read(t.file);
        assert!(
            server(&sb.json(t.file), t.path, "heavy")
                .get(t.flag.0)
                .is_none(),
            "{}: {before}",
            t.id
        );

        // `kitbag disable` edits the profile; apply writes the flag, no removal.
        sb.cli()
            .args(["disable", "heavy", "--on", t.id, "p"])
            .assert()
            .success();
        let out = apply(&sb, t.id);
        assert!(
            out.contains("MCP server 'heavy' turned off in")
                && out.contains("MCP servers turned off")
                && !out.contains("removed from"),
            "{}: {out}",
            t.id
        );
        let doc = sb.json(t.file);
        assert_eq!(
            server(&doc, t.path, "heavy")[t.flag.0],
            t.flag.1,
            "{}: {doc}",
            t.id
        );
        assert!(
            server(&doc, t.path, "heavy")
                .to_string()
                .contains("heavy-mcp"),
            "{}: {doc}",
            t.id
        );
        assert!(
            server(&doc, t.path, "light").get(t.flag.0).is_none(),
            "{}: {doc}",
            t.id
        );
        let off = sb.read(t.file);

        // A second apply is up to date, and capture reads it back as off.
        let out = apply(&sb, t.id);
        assert!(out.contains("Up to date"), "{}: {out}", t.id);
        assert!(!out.contains("left in place"), "{}: {out}", t.id);
        assert_eq!(sb.read(t.file), off);
        let cap = captured(&sb, t.id, &[]);
        let heavy = cap.split("[mcp_servers.heavy]").nth(1).unwrap();
        let heavy = heavy.split("\n[").next().unwrap();
        assert!(heavy.contains("disabled = true"), "{}: {cap}", t.id);
        sb.cli()
            .args(["status", "p", "--to", t.id])
            .assert()
            .success()
            .stdout(contains("turned off in the tool"));

        // Undo puts the entry back as it was.
        undo(&sb);
        assert_eq!(sb.read(t.file), before, "{}", t.id);
        apply(&sb, t.id);
        assert_eq!(sb.read(t.file), off, "{}", t.id);

        // Enabling it again clears the flag: the file is as it first was.
        sb.cli()
            .args(["enable", "heavy", "--on", t.id, "p"])
            .assert()
            .success();
        let out = apply(&sb, t.id);
        assert!(out.contains("Wrote"), "{}: {out}", t.id);
        assert_eq!(sb.read(t.file), before, "{}", t.id);
        let out = apply(&sb, t.id);
        assert!(out.contains("Up to date"), "{}: {out}", t.id);
    }
}

#[test]
fn codex_and_grok_build_keep_the_entry_with_enabled_false() {
    for (id, file) in [
        ("codex", "home/.codex/config.toml"),
        ("grok-build", "home/.grok/config.toml"),
    ] {
        let sb = Sandbox::empty();
        write_profile(&sb, "p", &profile("shared", ""));
        apply(&sb, id);
        let before = sb.read(file);
        write_profile(
            &sb,
            "p",
            &profile("shared", &format!("disabled_on = [\"{id}\"]\n")),
        );
        let out = apply(&sb, id);
        assert!(
            out.contains("MCP server 'heavy' turned off in"),
            "{id}: {out}"
        );
        let text = sb.read(file);
        let heavy = text.split("[mcp_servers.heavy]").nth(1).unwrap();
        let heavy = heavy.split("\n[").next().unwrap();
        assert!(heavy.contains("enabled = false"), "{id}: {text}");
        assert!(heavy.contains("command = \"heavy-mcp\""), "{id}: {text}");
        assert!(apply(&sb, id).contains("Up to date"), "{id}");
        let cap = captured(&sb, id, &[]);
        assert!(cap.contains("disabled = true"), "{id}: {cap}");
        undo(&sb);
        assert_eq!(sb.read(file), before, "{id}");
        apply(&sb, id);
        write_profile(&sb, "p", &profile("shared", ""));
        apply(&sb, id);
        assert_eq!(sb.read(file), before, "{id}");
    }
}

#[test]
fn hermes_keeps_the_entry_with_enabled_false() {
    let sb = Sandbox::empty();
    write_profile(&sb, "p", &profile("shared", ""));
    apply(&sb, "hermes");
    let file = "home/.hermes/config.yaml";
    let before = sb.read(file);
    sb.cli()
        .args(["disable", "heavy", "--on", "hermes", "p"])
        .assert()
        .success();
    let out = apply(&sb, "hermes");
    assert!(out.contains("turned off in"), "{out}");
    assert!(sb.read(file).contains("enabled: false"));
    assert!(apply(&sb, "hermes").contains("Up to date"));
    undo(&sb);
    assert_eq!(sb.read(file), before);
}

#[test]
fn claude_code_rejects_a_project_server_in_local_settings() {
    const MCP: &str = "project/.mcp.json";
    const LOCAL: &str = "project/.claude/settings.local.json";
    let sb = Sandbox::empty();
    write_profile(&sb, "p", &profile("shared", ""));
    apply_project(&sb, "claude-code");
    let mcp_before = sb.read(MCP);

    // A name the user listed by hand is theirs and stays.
    fs::create_dir_all(sb.path("project/.claude")).unwrap();
    fs::write(
        sb.path(LOCAL),
        "{\n  \"disabledMcpjsonServers\": [\"mine\"],\n  \"model\": \"x\"\n}\n",
    )
    .unwrap();
    let local_before = sb.read(LOCAL);

    sb.cli()
        .args(["disable", "heavy", "--on", "claude-code", "p"])
        .assert()
        .success();
    let out = apply_project(&sb, "claude-code");
    assert!(
        out.contains("MCP server 'heavy' turned off") && !out.contains("removed from"),
        "{out}"
    );
    // The entry stays in the shared file, byte for byte.
    assert_eq!(sb.read(MCP), mcp_before);
    let local = sb.json(LOCAL);
    assert_eq!(
        local["disabledMcpjsonServers"],
        serde_json::json!(["mine", "heavy"])
    );
    assert_eq!(local["model"], "x");

    // A second apply is up to date; status and capture see it as off.
    let out = apply_project(&sb, "claude-code");
    assert!(out.contains("Up to date"), "{out}");
    let cap = captured(&sb, "claude-code", &["--project", "project"]);
    let heavy = cap.split("[mcp_servers.heavy]").nth(1).unwrap();
    assert!(
        heavy
            .split("\n[")
            .next()
            .unwrap()
            .contains("disabled = true"),
        "{cap}"
    );
    sb.cli()
        .args(["status", "p", "--project", "project", "--to", "claude-code"])
        .assert()
        .success()
        .stdout(contains("turned off in the tool"));

    // Undo restores the settings file.
    undo(&sb);
    assert_eq!(sb.read(LOCAL), local_before);
    apply_project(&sb, "claude-code");

    // Enabling it again takes only kitbag's name out.
    sb.cli()
        .args(["enable", "heavy", "--on", "claude-code", "p"])
        .assert()
        .success();
    let out = apply_project(&sb, "claude-code");
    assert!(out.contains("turned on again"), "{out}");
    assert_eq!(
        sb.json(LOCAL)["disabledMcpjsonServers"],
        serde_json::json!(["mine"])
    );
    assert_eq!(sb.read(MCP), mcp_before);
    assert!(apply_project(&sb, "claude-code").contains("Up to date"));
}

#[test]
fn claude_code_leaves_a_name_the_user_listed_alone_when_enabled() {
    const LOCAL: &str = "project/.claude/settings.local.json";
    let sb = Sandbox::empty();
    write_profile(&sb, "p", &profile("shared", ""));
    apply_project(&sb, "claude-code");
    // The user rejects `heavy` themselves; kitbag never wrote that.
    fs::create_dir_all(sb.path("project/.claude")).unwrap();
    fs::write(
        sb.path(LOCAL),
        "{\"disabledMcpjsonServers\": [\"heavy\"]}\n",
    )
    .unwrap();
    let out = apply_project(&sb, "claude-code");
    assert!(out.contains("Up to date"), "{out}");
    assert_eq!(
        sb.json(LOCAL)["disabledMcpjsonServers"],
        serde_json::json!(["heavy"])
    );
}

#[test]
fn claude_code_user_servers_have_no_global_switch_so_they_are_removed() {
    // The `/mcp` toggle of ~/.claude.json is kept per project, so kitbag
    // keeps removing a user-scope server the profile disables.
    let sb = Sandbox::empty();
    write_profile(&sb, "p", &profile("shared", ""));
    apply(&sb, "claude-code");
    let before = sb.read("home/.claude.json");
    write_profile(
        &sb,
        "p",
        &profile("shared", "disabled_on = [\"claude-code\"]\n"),
    );
    let out = apply(&sb, "claude-code");
    assert!(out.contains("MCP server 'heavy' removed from"), "{out}");
    assert!(
        sb.json("home/.claude.json")["mcpServers"]
            .get("heavy")
            .is_none()
    );
    assert!(!sb.path("home/.claude/settings.local.json").exists());
    undo(&sb);
    assert_eq!(sb.read("home/.claude.json"), before);
}

#[test]
fn gemini_cli_lists_a_disabled_server_in_mcp_excluded() {
    const SETTINGS: &str = "home/.gemini/settings.json";
    let sb = Sandbox::empty();
    write_profile(&sb, "p", &profile("shared", ""));
    apply(&sb, "gemini-cli");
    let before = sb.read(SETTINGS);
    // Gemini documents no `disabled` key on a server entry.
    assert!(!before.contains("disabled"), "{before}");

    sb.cli()
        .args(["disable", "heavy", "--on", "gemini-cli", "p"])
        .assert()
        .success();
    let out = apply(&sb, "gemini-cli");
    assert!(
        out.contains("MCP server 'heavy' turned off") && !out.contains("removed from"),
        "{out}"
    );
    let doc = sb.json(SETTINGS);
    assert_eq!(doc["mcp"]["excluded"], serde_json::json!(["heavy"]));
    assert_eq!(doc["mcpServers"]["heavy"]["command"], "heavy-mcp");
    assert!(apply(&sb, "gemini-cli").contains("Up to date"));
    let cap = captured(&sb, "gemini-cli", &[]);
    let heavy = cap.split("[mcp_servers.heavy]").nth(1).unwrap();
    assert!(
        heavy
            .split("\n[")
            .next()
            .unwrap()
            .contains("disabled = true"),
        "{cap}"
    );

    undo(&sb);
    assert_eq!(sb.read(SETTINGS), before);
    apply(&sb, "gemini-cli");

    sb.cli()
        .args(["enable", "heavy", "--on", "gemini-cli", "p"])
        .assert()
        .success();
    apply(&sb, "gemini-cli");
    let doc = sb.json(SETTINGS);
    assert!(doc.get("mcp").is_none(), "{doc}");
    assert_eq!(doc["mcpServers"]["heavy"]["command"], "heavy-mcp");
    assert_eq!(
        sb.json(SETTINGS),
        serde_json::from_str::<Value>(&before).unwrap()
    );
}

#[test]
fn copilot_lists_a_disabled_server_in_disabled_mcp_servers() {
    const MCP: &str = "home/.copilot/mcp-config.json";
    const SETTINGS: &str = "home/.copilot/settings.json";
    let sb = Sandbox::empty();
    write_profile(&sb, "p", &profile("shared", ""));
    apply(&sb, "copilot");
    let before = sb.read(MCP);
    sb.cli()
        .args(["disable", "heavy", "--on", "copilot", "p"])
        .assert()
        .success();
    let out = apply(&sb, "copilot");
    assert!(out.contains("MCP server 'heavy' turned off"), "{out}");
    assert_eq!(sb.read(MCP), before);
    assert_eq!(
        sb.json(SETTINGS)["disabledMcpServers"],
        serde_json::json!(["heavy"])
    );
    assert!(apply(&sb, "copilot").contains("Up to date"));
    let cap = captured(&sb, "copilot", &[]);
    assert!(cap.contains("disabled = true"), "{cap}");
    undo(&sb);
    assert!(!sb.path(SETTINGS).exists());
    apply(&sb, "copilot");
    sb.cli()
        .args(["enable", "heavy", "--on", "copilot", "p"])
        .assert()
        .success();
    apply(&sb, "copilot");
    assert!(sb.json(SETTINGS).get("disabledMcpServers").is_none());
}

#[test]
fn a_tool_with_no_flag_still_has_its_entry_removed_and_undo_restores_it() {
    let sb = Sandbox::empty();
    write_profile(&sb, "p", &profile("shared", ""));
    apply(&sb, "cursor");
    let before = sb.read("home/.cursor/mcp.json");
    sb.cli()
        .args(["disable", "heavy", "--on", "cursor", "p"])
        .assert()
        .success();
    let out = apply(&sb, "cursor");
    assert!(out.contains("MCP server 'heavy' removed from"), "{out}");
    assert!(!out.contains("turned off"), "{out}");
    assert!(
        sb.json("home/.cursor/mcp.json")["mcpServers"]
            .get("heavy")
            .is_none()
    );
    assert!(apply(&sb, "cursor").contains("Up to date"));
    undo(&sb);
    assert_eq!(sb.read("home/.cursor/mcp.json"), before);
}

#[test]
fn an_entry_that_was_not_added_by_kitbag_or_was_edited_is_not_flagged() {
    let sb = Sandbox::empty();
    // Added by hand: left as it is, with the old note.
    fs::create_dir_all(sb.path("home/.codex")).unwrap();
    let mine = "[mcp_servers.heavy]\ncommand = \"heavy-mcp\"\nargs = [\"--big\"]\n";
    fs::write(sb.path("home/.codex/config.toml"), mine).unwrap();
    write_profile(&sb, "p", &profile("shared", "disabled_on = [\"codex\"]\n"));
    let out = apply(&sb, "codex");
    assert!(out.contains("kitbag has no record of adding it"), "{out}");
    assert!(!sb.read("home/.codex/config.toml").contains("enabled"));

    // Edited since kitbag wrote it: left as it is.
    let sb = Sandbox::empty();
    write_profile(&sb, "p", &profile("shared", ""));
    apply(&sb, "cline");
    let file = "home/.cline/data/settings/cline_mcp_settings.json";
    let edited = sb.read(file).replace("--big", "--mine");
    fs::write(sb.path(file), &edited).unwrap();
    write_profile(&sb, "p", &profile("shared", "disabled_on = [\"cline\"]\n"));
    let out = apply(&sb, "cline");
    assert!(out.contains("it was edited in"), "{out}");
    assert_eq!(sb.read(file), edited);

    // Another profile's entry: left as it is.
    let sb = Sandbox::empty();
    write_profile(&sb, "p", &profile("shared", ""));
    apply(&sb, "cline");
    write_profile(&sb, "q", &profile("other", "disabled_on = [\"cline\"]\n"));
    let out = apply_args(&sb, &["apply", "q", "--to", "cline", "--yes"]);
    assert!(out.contains("profile 'shared' added it"), "{out}");
    assert!(
        server(&sb.json(file), &["mcpServers"], "heavy")
            .get("disabled")
            .is_none()
    );
}

#[test]
fn no_overwrite_keeps_a_server_on() {
    let sb = Sandbox::empty();
    write_profile(&sb, "p", &profile("shared", ""));
    apply(&sb, "zed");
    write_profile(&sb, "p", &profile("shared", "disabled_on = [\"zed\"]\n"));
    sb.cli()
        .args(["apply", "p", "--to", "zed", "--yes", "--no-overwrite"])
        .assert()
        .success()
        .stdout(contains("because --no-overwrite was given"));
    assert!(
        server(
            &sb.json(zed_settings()),
            &["context_servers"],
            "heavy"
        )
        .get("enabled")
        .is_none()
    );
}

#[test]
fn capture_keeps_tools_that_disagree_apart() {
    // Off in Codex, on in Cursor: off for Codex only, not for every tool.
    let sb = Sandbox::empty();
    write_profile(&sb, "p", &profile("shared", ""));
    apply(&sb, "codex,cursor");
    sb.cli()
        .args(["disable", "heavy", "--on", "codex", "p"])
        .assert()
        .success();
    apply(&sb, "codex,cursor");
    let cap = captured(&sb, "codex,cursor", &[]);
    let heavy = cap.split("[mcp_servers.heavy]").nth(1).unwrap();
    let heavy = heavy.split("\n[").next().unwrap();
    assert!(heavy.contains("disabled_on = [\"codex\"]"), "{cap}");
    assert!(!heavy.contains("disabled = true"), "{cap}");
    assert!(
        !cap.contains("differs from the one already captured"),
        "{cap}"
    );

    // Capturing the one tool again into the profile keeps the targeting and
    // does not turn the server off everywhere.
    apply_args(&sb, &["capture", "--from", "codex", "-o", "p", "--force"]);
    let again = sb.read("p/kitbag.toml");
    let heavy = again.split("[mcp_servers.heavy]").nth(1).unwrap();
    let heavy = heavy.split("\n[").next().unwrap();
    assert!(heavy.contains("disabled_on = [\"codex\"]"), "{again}");
    assert!(!heavy.contains("disabled = true"), "{again}");
}

#[test]
fn a_git_profile_cannot_turn_a_server_off_or_back_on_unseen() {
    if !has_git() {
        return;
    }
    let sb = Sandbox::empty();
    let v1 = sb.git_profile("repo", &[("kitbag.toml", &profile("shared", ""))]);
    let remote = |url: &str, extra: &[&str]| {
        let mut args = vec!["apply", url, "--to", "codex", "--yes"];
        args.extend_from_slice(extra);
        apply_args(&sb, &args)
    };
    remote(&v1, &["--allow-git-mcp", "heavy,light"]);
    let before = sb.read("home/.codex/config.toml");

    // The repository now disables it for Codex: held back, and said so.
    let v2 = sb.git_profile(
        "repo",
        &[(
            "kitbag.toml",
            &profile("shared", "disabled_on = [\"codex\"]\n"),
        )],
    );
    let out = remote(&v2, &[]);
    assert!(
        out.contains("MCP servers not removed")
            && out.contains("heavy  (codex)")
            && out.contains("--allow-git-mcp heavy")
            && !out.contains("Up to date"),
        "{out}"
    );
    assert_eq!(sb.read("home/.codex/config.toml"), before);
    let out = remote(&v2, &["--allow-git-mcp", "heavy"]);
    assert!(out.contains("MCP server 'heavy' turned off in"), "{out}");
    assert!(
        sb.read("home/.codex/config.toml")
            .contains("enabled = false")
    );

    // Re-enabling it shows up in the plan and needs the opt-in too.
    let v3 = sb.git_profile("repo", &[("kitbag.toml", &profile("shared", ""))]);
    let out = remote(&v3, &[]);
    assert!(
        out.contains("MCP servers not applied (from a git source)") && out.contains("heavy"),
        "{out}"
    );
    assert!(
        sb.read("home/.codex/config.toml")
            .contains("enabled = false")
    );
    let out = remote(&v3, &["--allow-git-mcp", "heavy"]);
    assert!(
        out.contains("Review the MCP servers it adds or changes"),
        "{out}"
    );
    assert_eq!(sb.read("home/.codex/config.toml"), before);
}

#[test]
fn a_git_profile_cannot_take_a_name_out_of_a_list_unseen() {
    if !has_git() {
        return;
    }
    const SETTINGS: &str = "home/.gemini/settings.json";
    let sb = Sandbox::empty();
    let v1 = sb.git_profile("repo", &[("kitbag.toml", &profile("shared", ""))]);
    let remote = |url: &str, extra: &[&str]| {
        let mut args = vec!["apply", url, "--to", "gemini-cli", "--yes"];
        args.extend_from_slice(extra);
        apply_args(&sb, &args)
    };
    remote(&v1, &["--allow-git-mcp", "heavy,light"]);
    let v2 = sb.git_profile(
        "repo",
        &[(
            "kitbag.toml",
            &profile("shared", "disabled_on = [\"gemini-cli\"]\n"),
        )],
    );
    let out = remote(&v2, &[]);
    assert!(out.contains("--allow-git-mcp heavy"), "{out}");
    assert!(sb.json(SETTINGS).get("mcp").is_none());
    remote(&v2, &["--allow-git-mcp", "heavy"]);
    assert_eq!(
        sb.json(SETTINGS)["mcp"]["excluded"],
        serde_json::json!(["heavy"])
    );
    let v3 = sb.git_profile("repo", &[("kitbag.toml", &profile("shared", ""))]);
    let out = remote(&v3, &[]);
    assert!(
        out.contains("MCP servers not applied (from a git source)") && out.contains("heavy"),
        "{out}"
    );
    assert_eq!(
        sb.json(SETTINGS)["mcp"]["excluded"],
        serde_json::json!(["heavy"])
    );
    remote(&v3, &["--allow-git-mcp", "heavy"]);
    assert!(sb.json(SETTINGS).get("mcp").is_none());
}

#[test]
fn doctor_and_diff_do_not_write_anything() {
    let sb = Sandbox::empty();
    write_profile(&sb, "p", &profile("shared", ""));
    apply(&sb, "codex");
    write_profile(&sb, "p", &profile("shared", "disabled_on = [\"codex\"]\n"));
    let before = sb.read("home/.codex/config.toml");
    sb.cli()
        .args(["diff", "p", "--to", "codex"])
        .assert()
        .success()
        .stdout(contains("enabled = false").and(contains("turned off")));
    assert_eq!(sb.read("home/.codex/config.toml"), before);
}

#[cfg(target_os = "linux")]
#[test]
fn roo_code_turns_a_disabled_server_off_with_disabled_true() {
    const ROO: &str = "home/.config/Code/User/globalStorage/rooveterinaryinc.roo-cline/settings/mcp_settings.json";
    let sb = Sandbox::empty();
    write_profile(&sb, "p", &profile("shared", ""));
    apply(&sb, "roo");
    let before = sb.read(ROO);
    sb.cli()
        .args(["disable", "heavy", "--on", "roo", "p"])
        .assert()
        .success();
    let out = apply(&sb, "roo");
    assert!(out.contains("MCP server 'heavy' turned off in"), "{out}");
    assert_eq!(sb.json(ROO)["mcpServers"]["heavy"]["disabled"], true);
    assert!(apply(&sb, "roo").contains("Up to date"));
    undo(&sb);
    assert_eq!(sb.read(ROO), before);
}
