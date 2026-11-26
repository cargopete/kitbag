//! Second machine: profiles carry home-relative paths, not this machine's
//! home directory or machine-specific state, and `status` shows how MCP
//! servers, commands and skills differ from the profile.

mod common;

use std::fs;

use common::*;
use predicates::prelude::*;
use predicates::str::contains;
use serde_json::json;

/// A Claude Code setup that refers to files in the home directory.
#[cfg(unix)]
fn home_paths(sb: &Sandbox) -> String {
    let home = sb.home().display().to_string();
    fs::create_dir_all(sb.home().join(".claude")).unwrap();
    fs::write(
        sb.home().join(".claude.json"),
        json!({
            "mcpServers": {"local": {
                "type": "stdio",
                "command": format!("{home}/bin/srv"),
                "args": [format!("--config={home}/.srv.toml"), format!("{home}/data"), "/opt/shared"],
                "env": {"DATA_DIR": format!("{home}/d")},
            }},
            // Machine-specific state Claude Code keeps per project.
            "projects": {format!("{home}/src/app"): {"allowedTools": [], "mcpServers": {"proj": {"command": "x"}}}},
        })
        .to_string(),
    )
    .unwrap();
    fs::write(
        sb.home().join(".claude/settings.json"),
        json!({"permissions": {
            "allow": [format!("Read(/{home}/notes/**)"), "Bash(git status)"],
            "additionalDirectories": [format!("{home}/notes"), "/srv/shared"],
        }})
        .to_string(),
    )
    .unwrap();
    home
}

#[cfg(unix)]
#[test]
fn capture_writes_home_relative_paths() {
    let sb = Sandbox::empty();
    let home = home_paths(&sb);
    sb.cli()
        .args(["capture", "--from", "claude-code", "-o", "p"])
        .assert()
        .success();
    let manifest = sb.read("p/kitbag.toml");
    assert!(!manifest.contains(&home), "{manifest}");
    for want in [
        r#"command = "~/bin/srv""#,
        r#""--config=~/.srv.toml""#,
        r#""~/data""#,
        r#""/opt/shared""#,
        r#"DATA_DIR = "~/d""#,
        r#""Read(~/notes/**)""#,
        r#""~/notes""#,
        r#""/srv/shared""#,
    ] {
        assert!(manifest.contains(want), "{want} missing:\n{manifest}");
    }
    // Per-project state stays on this machine.
    assert!(!manifest.contains("proj") && !manifest.contains("allowedTools"));

    // Applied back to the same machine, nothing changes.
    sb.cli()
        .args(["diff", "p", "--to", "claude-code", "--exit-code"])
        .assert()
        .success()
        .stdout(contains("Up to date"));
}

#[cfg(unix)]
#[test]
fn apply_expands_home_relative_paths_on_another_machine() {
    let first = Sandbox::empty();
    home_paths(&first);
    first
        .cli()
        .args(["capture", "--from", "claude-code", "-o", "p"])
        .assert()
        .success();
    let second = Sandbox::empty();
    copy_dir(&first.path("p"), &second.path("p"));
    let home = second.home().display().to_string();
    second
        .cli()
        .args(["apply", "p", "--to", "claude-code,codex", "--yes"])
        .assert()
        .success();
    let server = &second.json("home/.claude.json")["mcpServers"]["local"];
    assert_eq!(server["command"], format!("{home}/bin/srv"));
    assert_eq!(
        server["args"],
        json!([
            format!("--config={home}/.srv.toml"),
            format!("{home}/data"),
            "/opt/shared"
        ])
    );
    assert_eq!(server["env"]["DATA_DIR"], format!("{home}/d"));
    let settings = second.json("home/.claude/settings.json");
    assert_eq!(
        settings["permissions"]["additionalDirectories"],
        json!([format!("{home}/notes"), "/srv/shared"])
    );
    // Claude Code reads `~/` in permission rules itself.
    assert!(
        settings["permissions"]["allow"]
            .as_array()
            .unwrap()
            .contains(&json!("Read(~/notes/**)"))
    );
    let codex = second.read("home/.codex/config.toml");
    assert!(codex.contains(&format!("{home}/bin/srv")), "{codex}");
    assert!(!codex.contains("\"~/"), "{codex}");
}

#[test]
fn codex_project_trust_entries_stay_out_of_the_profile() {
    let sb = Sandbox::empty();
    let home = sb.home().display().to_string();
    fs::create_dir_all(sb.home().join(".codex")).unwrap();
    fs::write(
        sb.home().join(".codex/config.toml"),
        format!(
            "model = \"o3\"\n\n[projects.\"{home}/src/app\"]\ntrust_level = \"trusted\"\n\n[mcp_servers.docs]\ncommand = \"npx\"\nargs = [\"-y\", \"docs-mcp\"]\n"
        ),
    )
    .unwrap();
    sb.cli()
        .args(["capture", "--from", "codex", "-o", "p"])
        .assert()
        .success();
    let manifest = sb.read("p/kitbag.toml");
    assert!(manifest.contains("[mcp_servers.docs]"), "{manifest}");
    assert!(
        !manifest.contains("trust_level") && !manifest.contains("src/app"),
        "{manifest}"
    );
}

/// `kitbag status --json` rows as (target, item, state).
fn status(sb: &Sandbox, args: &[&str]) -> (Vec<(String, String, String)>, String) {
    let out = sb
        .cli()
        .arg("status")
        .args(args)
        .arg("--json")
        .output()
        .unwrap();
    let text = String::from_utf8(out.stdout).unwrap();
    let v: serde_json::Value = serde_json::from_str(&text).unwrap();
    let rows = v
        .as_array()
        .unwrap()
        .iter()
        .map(|r| {
            (
                r["target"].as_str().unwrap().to_string(),
                r["item"].as_str().unwrap().to_string(),
                r["state"].as_str().unwrap().to_string(),
            )
        })
        .collect();
    (rows, text)
}

#[test]
fn status_compares_servers_commands_and_skills() {
    let sb = Sandbox::empty();
    fs::create_dir_all(sb.path("p/commands")).unwrap();
    fs::create_dir_all(sb.path("p/skills/notes")).unwrap();
    fs::write(
        sb.path("p/kitbag.toml"),
        "kitbag = 1\nname = \"me\"\n[mcp_servers.a]\ncommand = \"a\"\n[mcp_servers.b]\ncommand = \"b\"\nenv = { B_TOKEN = \"${B_TOKEN}\" }\n",
    )
    .unwrap();
    fs::write(sb.path("p/commands/review.md"), "Review it.\n").unwrap();
    fs::write(
        sb.path("p/skills/notes/SKILL.md"),
        "---\nname: notes\ndescription: Notes\n---\nTake notes.\n",
    )
    .unwrap();
    sb.cli()
        .args(["apply", "p", "--to", "claude-code", "--yes"])
        .assert()
        .success();
    let (rows, _) = status(&sb, &["p", "--to", "claude-code"]);
    assert!(rows.iter().all(|r| r.2 == "in sync"), "{rows:?}");

    // The tool drifts: a server edited, one added by hand, a command deleted;
    // the profile gains a server.
    let mut cj = sb.json("home/.claude.json");
    cj["mcpServers"]["a"]["args"] = json!(["--other"]);
    cj["mcpServers"]["z"] = json!({"command": "z", "env": {"Z_TOKEN": "zz-literal-value-123"}});
    fs::write(sb.home().join(".claude.json"), cj.to_string()).unwrap();
    fs::remove_file(sb.home().join(".claude/commands/review.md")).unwrap();
    let manifest = sb.read("p/kitbag.toml") + "[mcp_servers.n]\ncommand = \"n\"\n";
    fs::write(sb.path("p/kitbag.toml"), manifest).unwrap();

    let (rows, text) = status(&sb, &["p", "--to", "claude-code"]);
    let state = |item: &str| {
        rows.iter()
            .find(|r| r.0 == "claude-code" && r.1 == item)
            .map(|r| r.2.as_str())
            .unwrap_or_else(|| panic!("no {item} in {rows:?}"))
            .to_string()
    };
    assert_eq!(state("mcp a"), "changed");
    assert_eq!(state("mcp b"), "in sync");
    assert_eq!(state("mcp n"), "added");
    assert_eq!(state("mcp z"), "removed");
    assert_eq!(state("command review"), "added");
    assert_eq!(state("skill notes"), "in sync");
    assert!(!text.contains("zz-literal-value-123"));
    sb.cli()
        .args(["status", "p", "--to", "claude-code", "--exit-code"])
        .assert()
        .code(1)
        .stdout(contains("zz-literal").not().and(contains("removed")));
    // Status never writes.
    assert!(!sb.home().join(".claude/commands/review.md").exists());
}
