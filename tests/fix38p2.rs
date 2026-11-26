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
