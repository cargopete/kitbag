//! Regression tests for the second part of the #38 correctness fixes.

mod common;

use std::fs;

use common::*;
use predicates::prelude::*;
use predicates::str::contains;

const SERVER_PROFILE: &str = "kitbag = 1\nname = \"me\"\n\n[mcp_servers.docs]\ncommand = \"npx\"\nargs = [\"-y\", \"docs-mcp\"]\n";

fn server_profile(sb: &Sandbox) {
    fs::create_dir_all(sb.path("p")).unwrap();
    fs::write(sb.path("p/kitbag.toml"), SERVER_PROFILE).unwrap();
}

#[test]
fn duplicate_json_keys_are_refused_naming_file_and_key() {
    let sb = Sandbox::empty();
    server_profile(&sb);
    let config = "{\n  \"mcpServers\": {},\n  \"theme\": \"dark\",\n  \"mcpServers\": {\"old\": {\"command\": \"x\"}}\n}\n";
    fs::write(sb.home().join(".claude.json"), config).unwrap();
    for cmd in [
        vec!["diff", "p", "--to", "claude-code", "--exit-code"],
        vec!["apply", "p", "--to", "claude-code", "--yes"],
    ] {
        sb.cli()
            .args(&cmd)
            .assert()
            .failure()
            .stderr(contains(".claude.json"))
            .stderr(contains("key 'mcpServers' appears more than once"));
    }
    assert_eq!(sb.read("home/.claude.json"), config);

    // A duplicate inside a server definition is refused too.
    let config = "{\"mcpServers\": {\"docs\": {\"command\": \"a\", \"command\": \"b\"}}}\n";
    fs::write(sb.home().join(".claude.json"), config).unwrap();
    sb.cli()
        .args(["apply", "p", "--to", "claude-code", "--yes"])
        .assert()
        .failure()
        .stderr(contains(
            "key 'command' appears more than once in 'mcpServers.docs'",
        ));
    assert_eq!(sb.read("home/.claude.json"), config);
}

fn no_bare_lf(text: &str) -> bool {
    !text.replace("\r\n", "").contains('\n')
}

#[test]
fn crlf_files_keep_their_line_endings() {
    let sb = Sandbox::empty();
    server_profile(&sb);
    fs::write(sb.path("p/instructions.md"), "Be brief.\n\nUse tabs.\n").unwrap();
    let home = sb.home();
    fs::create_dir_all(home.join(".claude")).unwrap();
    fs::create_dir_all(home.join(".codex")).unwrap();
    fs::write(
        home.join(".claude.json"),
        "{\r\n  \"theme\": \"dark\"\r\n}\r\n",
    )
    .unwrap();
    fs::write(home.join(".claude/CLAUDE.md"), "# Mine\r\n\r\nKeep me.\r\n").unwrap();
    fs::write(
        home.join(".codex/config.toml"),
        "# mine\r\nmodel = \"o3\"\r\n",
    )
    .unwrap();
    fs::write(home.join(".codex/AGENTS.md"), "# Mine\r\n").unwrap();
    sb.cli()
        .args(["apply", "p", "--to", "claude-code,codex", "--yes"])
        .assert()
        .success();
    for rel in [
        "home/.claude.json",
        "home/.claude/CLAUDE.md",
        "home/.codex/config.toml",
        "home/.codex/AGENTS.md",
    ] {
        let text = sb.read(rel);
        assert!(no_bare_lf(&text), "{rel}: {text:?}");
        assert!(
            text.contains("docs-mcp") || text.contains("Use tabs."),
            "{rel}: {text:?}"
        );
    }
    assert!(
        sb.read("home/.claude/CLAUDE.md")
            .starts_with("# Mine\r\n\r\nKeep me.\r\n")
    );
    assert!(
        sb.read("home/.codex/config.toml")
            .starts_with("# mine\r\nmodel = \"o3\"\r\n")
    );
    // Re-applying changes nothing.
    sb.cli()
        .args(["diff", "p", "--to", "claude-code,codex", "--exit-code"])
        .assert()
        .success();
    sb.cli()
        .args(["status", "p", "--exit-code"])
        .assert()
        .success();
}
