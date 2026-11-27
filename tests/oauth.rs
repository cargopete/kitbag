//! MCP OAuth client configuration and the sign-in checklist (#18, phase 1).
//!
//! Only configuration is carried: never a token, never a token store, never a
//! literal client secret. Every value here is a made-up one.

mod common;

use std::fs;

use common::*;
use kitbag::adapters::Scope;
use kitbag::model::{McpOauth, McpServer, Profile};
use kitbag::profile_io;
use predicates::prelude::*;
use predicates::str::contains;
use serde_json::{Value, json};

const LITERAL: &str = "fake-literal-client-secret-0123";
const URL: &str = "https://mcp.example.com/mcp";

fn oauth() -> McpOauth {
    McpOauth {
        client_id: Some("pub-client-id".into()),
        client_secret: Some("${LINEAR_CLIENT_SECRET}".into()),
        scopes: vec!["read".into(), "write".into()],
        callback_port: Some(8080),
        ..Default::default()
    }
}

fn profile() -> Profile {
    let mut p = Profile::new("team");
    p.mcp_servers.insert(
        "linear".into(),
        McpServer {
            url: Some(URL.into()),
            oauth: Some(oauth()),
            ..Default::default()
        },
    );
    p.mcp_servers.insert(
        "local".into(),
        McpServer {
            command: Some("npx".into()),
            args: vec!["-y".into(), "some-server".into()],
            ..Default::default()
        },
    );
    p
}

fn said(p: &kitbag::engine::Plan, target: &str, what: &str) -> bool {
    p.notes
        .iter()
        .any(|n| n.target == target && n.message.contains(what))
}

fn write(sb: &Sandbox, rel: &str, text: &str) {
    let path = sb.path(rel);
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, text).unwrap();
}

#[test]
fn claude_code_gets_the_documented_oauth_keys_and_no_secret() {
    let sb = Sandbox::empty();
    let env = sb.env(Scope::User);
    let p = apply(&sb, &profile(), &["claude-code"], &env);
    let doc = sb.json("home/.claude.json");
    assert_eq!(
        doc["mcpServers"]["linear"]["oauth"],
        json!({"clientId": "pub-client-id", "callbackPort": 8080, "scopes": "read write"})
    );
    assert!(doc["mcpServers"]["local"].get("oauth").is_none());
    // Claude Code keeps the secret in its keychain or a credentials file.
    assert!(!doc.to_string().contains("LINEAR_CLIENT_SECRET"));
    assert!(said(&p, "claude-code", "client secret was not written"));
}

#[test]
fn claude_code_oauth_comes_back_on_capture() {
    let sb = Sandbox::empty();
    write(
        &sb,
        "home/.claude.json",
        &json!({"mcpServers": {"linear": {
            "type": "http", "url": URL,
            "oauth": {"clientId": "pub-client-id", "callbackPort": 8080, "scopes": "read write",
                       "authServerMetadataUrl": "https://auth.example.com/.well-known/openid-configuration"}
        }}})
        .to_string(),
    );
    let got = capture(&["claude-code"], &sb.env(Scope::User)).profile;
    let o = got.mcp_servers["linear"].oauth.clone().unwrap();
    assert_eq!(o.client_id.as_deref(), Some("pub-client-id"));
    assert_eq!(o.callback_port, Some(8080));
    assert_eq!(o.scopes, ["read", "write"]);
    assert_eq!(
        o.auth_server_metadata_url.as_deref(),
        Some("https://auth.example.com/.well-known/openid-configuration")
    );
}

#[test]
fn gemini_gets_camel_case_keys_and_an_env_ref_secret() {
    let sb = Sandbox::empty();
    let mut p = profile();
    p.mcp_servers.get_mut("linear").unwrap().oauth = Some(McpOauth {
        enabled: Some(true),
        authorization_url: Some("https://auth.example.com/authorize".into()),
        token_url: Some("https://auth.example.com/token".into()),
        redirect_uri: Some("http://localhost:7777/oauth/callback".into()),
        ..oauth()
    });
    apply(&sb, &p, &["gemini-cli"], &sb.env(Scope::User));
    let doc = sb.json("home/.gemini/settings.json");
    assert_eq!(
        doc["mcpServers"]["linear"]["oauth"],
        json!({
            "enabled": true,
            "clientId": "pub-client-id",
            "clientSecret": "${LINEAR_CLIENT_SECRET}",
            "authorizationUrl": "https://auth.example.com/authorize",
            "tokenUrl": "https://auth.example.com/token",
            "scopes": ["read", "write"],
            "redirectUri": "http://localhost:7777/oauth/callback"
        })
    );
    // And it reads back the same.
    let got = capture(&["gemini-cli"], &sb.env(Scope::User)).profile;
    let o = got.mcp_servers["linear"].oauth.clone().unwrap();
    assert_eq!(o.client_secret.as_deref(), Some("${LINEAR_CLIENT_SECRET}"));
    assert_eq!(o.scopes, ["read", "write"]);
    assert_eq!(
        o.token_url.as_deref(),
        Some("https://auth.example.com/token")
    );
}

#[test]
fn opencode_gets_snake_case_keys_and_its_own_env_syntax() {
    let sb = Sandbox::empty();
    let mut p = profile();
    p.mcp_servers
        .get_mut("linear")
        .unwrap()
        .oauth
        .as_mut()
        .unwrap()
        .redirect_uri = Some("http://127.0.0.1:8080/callback".into());
    apply(&sb, &p, &["opencode"], &sb.env(Scope::User));
    let doc = sb.json("home/.config/opencode/opencode.json");
    let linear = doc["mcp"]["servers"]["linear"]
        .as_object()
        .or_else(|| doc["mcp"]["linear"].as_object())
        .unwrap()
        .clone();
    assert_eq!(
        Value::Object(linear.clone())["oauth"],
        json!({
            "client_id": "pub-client-id",
            "client_secret": "{env:LINEAR_CLIENT_SECRET}",
            "scope": "read write",
            "callback_port": 8080,
            "redirect_uri": "http://127.0.0.1:8080/callback"
        })
    );
    let got = capture(&["opencode"], &sb.env(Scope::User)).profile;
    let o = got.mcp_servers["linear"].oauth.clone().unwrap();
    assert_eq!(o.client_secret.as_deref(), Some("${LINEAR_CLIENT_SECRET}"));
    assert_eq!(o.scopes, ["read", "write"]);
}

#[test]
fn opencode_oauth_false_is_kept_and_not_on_the_checklist() {
    let sb = Sandbox::empty();
    write(
        &sb,
        "home/.config/opencode/opencode.json",
        &json!({"mcp": {"servers": {
            "keyed": {"type": "remote", "url": "https://keyed.example.com/mcp", "oauth": false},
            "open": {"type": "remote", "url": "https://open.example.com/mcp"}
        }}})
        .to_string(),
    );
    let got = capture(&["opencode"], &sb.env(Scope::User)).profile;
    assert_eq!(
        got.mcp_servers["keyed"].oauth.as_ref().unwrap().enabled,
        Some(false)
    );
    assert!(got.mcp_servers["open"].oauth.is_none());
    let dst = Sandbox::empty();
    let p = apply(&dst, &got, &["opencode"], &dst.env(Scope::User));
    let names: Vec<&str> = p.signins.iter().map(|s| s.server.as_str()).collect();
    // opencode's docs: remote servers use OAuth unless `oauth` is false.
    assert_eq!(names, ["open"]);
    assert_eq!(p.signins[0].command, "opencode mcp auth open");
    let out = dst.json("home/.config/opencode/opencode.json");
    let keyed = &out["mcp"]["servers"]["keyed"];
    assert_eq!(keyed["oauth"], json!(false));
}

#[test]
fn codex_gets_client_id_port_scopes_and_never_a_secret() {
    let sb = Sandbox::empty();
    let p = apply(&sb, &profile(), &["codex"], &sb.env(Scope::User));
    let text = sb.read("home/.codex/config.toml");
    let doc: toml::Table = toml::from_str(&text).unwrap();
    let linear = &doc["mcp_servers"]["linear"];
    assert_eq!(linear["oauth"]["client_id"].as_str(), Some("pub-client-id"));
    assert_eq!(linear["oauth"]["callback_port"].as_integer(), Some(8080));
    assert_eq!(
        linear["scopes"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(toml::Value::as_str)
            .collect::<Vec<_>>(),
        ["read", "write"]
    );
    assert!(!text.contains("client_secret") && !text.contains("LINEAR_CLIENT_SECRET"));
    assert!(said(&p, "codex", "client secret was not written"));
    let got = capture(&["codex"], &sb.env(Scope::User)).profile;
    let o = got.mcp_servers["linear"].oauth.clone().unwrap();
    assert_eq!(o.callback_port, Some(8080));
    assert_eq!(o.scopes, ["read", "write"]);
}

#[test]
fn cursor_gets_its_auth_object_with_an_env_ref() {
    let sb = Sandbox::empty();
    apply(&sb, &profile(), &["cursor"], &sb.env(Scope::User));
    let doc = sb.json("home/.cursor/mcp.json");
    assert_eq!(
        doc["mcpServers"]["linear"]["auth"],
        json!({"CLIENT_ID": "pub-client-id", "CLIENT_SECRET": "${env:LINEAR_CLIENT_SECRET}"})
    );
}

#[test]
fn a_literal_client_secret_never_reaches_the_profile_or_any_output() {
    let sb = Sandbox::empty();
    write(
        &sb,
        "home/.gemini/settings.json",
        &json!({"mcpServers": {"linear": {
            "httpUrl": URL,
            "oauth": {"enabled": true, "clientId": "pub-client-id", "clientSecret": LITERAL}
        }}})
        .to_string(),
    );
    let r = capture(&["gemini-cli"], &sb.env(Scope::User));
    let o = r.profile.mcp_servers["linear"].oauth.clone().unwrap();
    assert_eq!(o.client_id.as_deref(), Some("pub-client-id"));
    assert_eq!(o.client_secret, None);
    assert!(r.redactions.is_empty());
    let warned: Vec<&str> = r
        .notes
        .iter()
        .filter(|n| n.message.contains("client_secret"))
        .map(|n| n.message.as_str())
        .collect();
    assert_eq!(warned.len(), 1, "{warned:?}");
    assert!(warned[0].contains("left out of the profile") && warned[0].contains("#9"));
    assert!(r.notes.iter().all(|n| !n.message.contains(LITERAL)));
    // Saved profile files hold no trace of it.
    let dir = sb.path("saved");
    profile_io::save(&r.profile, &dir, false).unwrap();
    assert!(!all_text_under(&dir).contains(LITERAL));
    // Applied elsewhere, it isn't there either.
    let dst = Sandbox::empty();
    let p = apply(
        &dst,
        &r.profile,
        &["claude-code", "opencode", "cursor"],
        &dst.env(Scope::User),
    );
    assert!(!all_text_under(&dst.home()).contains(LITERAL));
    assert!(p.changes.iter().all(|c| {
        c.after
            .as_ref()
            .is_none_or(|b| !String::from_utf8_lossy(b).contains(LITERAL))
    }));
    assert!(!all_text_under(&dst.state()).contains(LITERAL));
}

#[test]
fn a_profile_with_a_literal_client_secret_is_refused() {
    let sb = Sandbox::empty();
    let dir = sb.path("prof");
    fs::create_dir_all(&dir).unwrap();
    fs::write(
        dir.join("kitbag.toml"),
        format!(
            "kitbag = 1\nname = \"x\"\n[mcp_servers.linear]\nurl = \"{URL}\"\n[mcp_servers.linear.oauth]\nclient_id = \"c\"\nclient_secret = \"{LITERAL}\"\n"
        ),
    )
    .unwrap();
    let err = profile_io::load(&dir).unwrap_err();
    let text = format!("{err:?}");
    assert!(
        text.contains("client_secret must be an env reference"),
        "{text}"
    );
    assert!(!text.contains(LITERAL));
    // A reference with a fallback would write the fallback; it is refused too.
    fs::write(
        dir.join("kitbag.toml"),
        format!(
            "kitbag = 1\nname = \"x\"\n[mcp_servers.linear]\nurl = \"{URL}\"\n[mcp_servers.linear.oauth]\nclient_secret = \"${{S:-{LITERAL}}}\"\n"
        ),
    )
    .unwrap();
    assert!(profile_io::load(&dir).is_err());
    // An env reference loads and survives a save and load.
    let mut p = profile();
    p.mcp_servers.get_mut("linear").unwrap().oauth = Some(oauth());
    fs::remove_file(dir.join("kitbag.toml")).unwrap();
    profile_io::save(&p, &dir, false).unwrap();
    let back = profile_io::load(&dir).unwrap();
    assert_eq!(back.mcp_servers["linear"], p.mcp_servers["linear"]);
    let toml = fs::read_to_string(dir.join("kitbag.toml")).unwrap();
    assert!(
        toml.contains("client_secret = \"${LINEAR_CLIENT_SECRET}\""),
        "{toml}"
    );
}

#[test]
fn a_literal_secret_that_got_into_a_profile_is_not_written_by_any_tool() {
    // Built in code, so past the loader's check: the adapters refuse it too.
    let mut p = profile();
    p.mcp_servers
        .get_mut("linear")
        .unwrap()
        .oauth
        .as_mut()
        .unwrap()
        .client_secret = Some(LITERAL.into());
    let sb = Sandbox::empty();
    let plan = apply(
        &sb,
        &p,
        &[
            "claude-code",
            "gemini-cli",
            "opencode",
            "codex",
            "cursor",
            "hermes",
        ],
        &sb.env(Scope::User),
    );
    assert!(!all_text_under(&sb.home()).contains(LITERAL));
    assert!(!all_text_under(&sb.state()).contains(LITERAL));
    assert!(plan.notes.iter().all(|n| !n.message.contains(LITERAL)));
}

#[test]
fn token_stores_are_neither_read_nor_written() {
    let sb = Sandbox::empty();
    // Where the tools keep tokens. Each is a directory here, so any attempt to
    // read it as a file fails, and what is inside must not change or leak.
    let stores = [
        "home/.gemini/mcp-oauth-tokens.json",
        "home/.codex/.credentials.json",
        "home/.hermes/auth.json",
        "home/.hermes/mcp-tokens",
    ];
    for s in stores {
        write(
            &sb,
            &format!("{s}/inner.txt"),
            "FAKE-ACCESS-TOKEN-do-not-read",
        );
    }
    write(
        &sb,
        "home/.hermes/config.yaml",
        "mcp_servers:\n  linear:\n    url: https://mcp.example.com/mcp\n    auth: oauth\n",
    );
    let env = sb.env(Scope::User);
    let cap = capture(
        &["claude-code", "gemini-cli", "codex", "hermes", "opencode"],
        &env,
    );
    assert!(cap.profile.mcp_servers.contains_key("linear"));
    let p = apply(
        &sb,
        &profile(),
        &["claude-code", "gemini-cli", "codex", "hermes"],
        &env,
    );
    for s in stores {
        assert_eq!(
            sb.read(&format!("{s}/inner.txt")),
            "FAKE-ACCESS-TOKEN-do-not-read"
        );
        assert!(
            p.changes.iter().all(|c| !c.path.starts_with(sb.path(s))),
            "{s}"
        );
    }
    assert!(!all_text_under(&sb.state()).contains("FAKE-ACCESS-TOKEN"));
    let mut said_it = String::new();
    for n in p.notes.iter().chain(cap.notes.iter()) {
        said_it.push_str(&n.message);
    }
    assert!(!said_it.contains("FAKE-ACCESS-TOKEN"));
    assert!(!all_text_under(&sb.home().join(".claude.json")).contains("TOKEN"));
}

#[test]
fn the_plan_lists_the_sign_in_commands_each_tool_documents() {
    let sb = Sandbox::empty();
    let env = sb.env(Scope::User);
    let p = plan(
        &profile(),
        &[
            "claude-code",
            "gemini-cli",
            "opencode",
            "codex",
            "hermes",
            "cursor",
            "zed",
            "amp",
        ],
        &env,
    );
    let got: Vec<(&str, &str, &str)> = p
        .signins
        .iter()
        .map(|s| (s.target.as_str(), s.server.as_str(), s.command.as_str()))
        .collect();
    assert_eq!(
        got,
        [
            ("claude-code", "linear", "claude mcp login linear"),
            ("gemini-cli", "linear", "/mcp auth linear"),
            ("opencode", "linear", "opencode mcp auth linear"),
            ("codex", "linear", "codex mcp login linear"),
            ("hermes", "linear", "hermes mcp login linear"),
        ]
    );
    // Local servers are never listed; Cursor, Zed and Amp name no command.
}

#[test]
fn dry_run_and_apply_print_the_checklist() {
    let sb = Sandbox::empty();
    profile_io::save(&profile(), &sb.path("prof"), false).unwrap();
    sb.cli()
        .args([
            "apply",
            "prof",
            "--to",
            "claude-code,gemini-cli",
            "--dry-run",
        ])
        .assert()
        .success()
        .stdout(
            contains("Sign in to these MCP servers")
                .and(contains("claude mcp login linear"))
                .and(contains("/mcp auth linear"))
                .and(
                    contains("local")
                        .not()
                        .or(contains("claude mcp login local").not()),
                ),
        );
    assert!(!sb.home().join(".claude.json").exists());
    sb.cli()
        .args(["apply", "prof", "--to", "claude-code,gemini-cli", "--yes"])
        .assert()
        .success()
        .stdout(
            contains("Next, sign in to these MCP servers")
                .and(contains("claude mcp login linear"))
                .and(contains("/mcp auth linear"))
                .and(contains(LITERAL).not())
                .and(contains("LINEAR_CLIENT_SECRET").not()),
        );
}

#[test]
fn a_changed_url_gets_no_oauth_config_and_loses_the_old_entrys() {
    let sb = Sandbox::empty();
    write(
        &sb,
        "home/.claude.json",
        &json!({"mcpServers": {"linear": {
            "type": "http", "url": "https://someone-else.example.com/mcp",
            "oauth": {"clientId": "their-client-id"}, "timeout": 5
        }}})
        .to_string(),
    );
    let p = apply(&sb, &profile(), &["claude-code"], &sb.env(Scope::User));
    let entry = &sb.json("home/.claude.json")["mcpServers"]["linear"];
    assert_eq!(entry["url"], URL);
    assert!(entry.get("oauth").is_none(), "{entry}");
    assert!(entry.get("timeout").is_none());
    assert!(!sb.read("home/.claude.json").contains("their-client-id"));
    assert!(said(&p, "claude-code", "different URL"));
    // The same holds for Codex's TOML and Cursor's `auth`.
    write(
        &sb,
        "home/.codex/config.toml",
        "[mcp_servers.linear]\nurl = \"https://someone-else.example.com/mcp\"\nscopes = [\"theirs\"]\n[mcp_servers.linear.oauth]\nclient_id = \"their-client-id\"\n",
    );
    write(
        &sb,
        "home/.cursor/mcp.json",
        &json!({"mcpServers": {"linear": {"url": "https://someone-else.example.com/mcp",
            "auth": {"CLIENT_ID": "their-client-id"}}}})
        .to_string(),
    );
    let p = apply(&sb, &profile(), &["codex", "cursor"], &sb.env(Scope::User));
    let codex = sb.read("home/.codex/config.toml");
    assert!(
        !codex.contains("their-client-id") && !codex.contains("pub-client-id"),
        "{codex}"
    );
    assert!(!codex.contains("theirs"));
    assert!(!sb.read("home/.cursor/mcp.json").contains("-client-id"));
    assert!(said(&p, "codex", "different URL") && said(&p, "cursor", "different URL"));
}

#[test]
fn an_unchanged_url_keeps_other_oauth_keys_and_tool_settings() {
    let sb = Sandbox::empty();
    write(
        &sb,
        "home/.gemini/settings.json",
        &json!({"mcpServers": {"linear": {
            "httpUrl": URL, "timeout": 9000,
            "oauth": {"audiences": ["api"], "tokenParamName": "access_token", "clientId": "old"}
        }}})
        .to_string(),
    );
    apply(&sb, &profile(), &["gemini-cli"], &sb.env(Scope::User));
    let entry = &sb.json("home/.gemini/settings.json")["mcpServers"]["linear"];
    assert_eq!(entry["timeout"], 9000);
    assert_eq!(entry["oauth"]["audiences"], json!(["api"]));
    assert_eq!(entry["oauth"]["tokenParamName"], "access_token");
    assert_eq!(entry["oauth"]["clientId"], "pub-client-id");
}

#[test]
fn hermes_oauth_marker_is_captured_and_listed_but_nothing_is_signed_in() {
    let sb = Sandbox::empty();
    write(
        &sb,
        "home/.hermes/config.yaml",
        "mcp_servers:\n  asana:\n    url: https://mcp.asana.com/sse\n    auth: oauth\n    oauth:\n      client_id: ${ASANA_CLIENT_ID}\n      client_secret: ${ASANA_CLIENT_SECRET}\n      redirect_port: 9999\n",
    );
    let got = capture(&["hermes"], &sb.env(Scope::User)).profile;
    let o = got.mcp_servers["asana"].oauth.clone().unwrap();
    assert_eq!(o.enabled, Some(true));
    assert_eq!(o.client_id.as_deref(), Some("${ASANA_CLIENT_ID}"));
    assert_eq!(o.client_secret.as_deref(), Some("${ASANA_CLIENT_SECRET}"));
    let dst = Sandbox::empty();
    let p = apply(&dst, &got, &["gemini-cli", "hermes"], &dst.env(Scope::User));
    let cmds: Vec<&str> = p.signins.iter().map(|s| s.command.as_str()).collect();
    assert_eq!(cmds, ["/mcp auth asana", "hermes mcp login asana"]);
    let hermes = dst.read("home/.hermes/config.yaml");
    assert!(
        hermes.contains("auth: \"oauth\"") && hermes.contains("${ASANA_CLIENT_SECRET}"),
        "{hermes}"
    );
}

#[test]
fn tools_without_documented_oauth_keys_get_none() {
    // Copilot, Zed, Amp and Claude Desktop have no documented OAuth config
    // keys (checked 2026-11-27): the entry is written without any.
    let sb = Sandbox::empty();
    let p = apply(
        &sb,
        &profile(),
        &["zed", "amp", "claude-desktop", "copilot"],
        &sb.env(Scope::User),
    );
    for c in &p.changes {
        if let Some(b) = &c.after {
            let t = String::from_utf8_lossy(b);
            assert!(
                !t.contains("pub-client-id") && !t.contains("callbackPort"),
                "{}",
                c.path.display()
            );
        }
    }
    assert!(p.signins.is_empty());
}

#[test]
fn oauth_on_a_local_server_is_an_error() {
    let sb = Sandbox::empty();
    let dir = sb.path("prof");
    fs::create_dir_all(&dir).unwrap();
    fs::write(
        dir.join("kitbag.toml"),
        "kitbag = 1\nname = \"x\"\n[mcp_servers.a]\ncommand = \"a\"\n[mcp_servers.a.oauth]\nclient_id = \"c\"\n",
    )
    .unwrap();
    let err = format!("{:?}", profile_io::load(&dir).unwrap_err());
    assert!(
        err.contains("'oauth' only applies to remote servers"),
        "{err}"
    );
}

#[test]
fn two_tools_that_each_name_some_oauth_keys_make_one_server() {
    let sb = Sandbox::empty();
    write(
        &sb,
        "home/.claude.json",
        &json!({"mcpServers": {"linear": {"type": "http", "url": URL,
            "oauth": {"clientId": "pub-client-id", "callbackPort": 8080}}}})
        .to_string(),
    );
    write(
        &sb,
        "home/.codex/config.toml",
        &format!("[mcp_servers.linear]\nurl = \"{URL}\"\nscopes = [\"read\"]\n"),
    );
    let r = capture(&["claude-code", "codex"], &sb.env(Scope::User));
    assert!(
        r.notes.iter().all(|n| !n.message.contains("differs")),
        "{:?}",
        r.notes
    );
    let o = r.profile.mcp_servers["linear"].oauth.clone().unwrap();
    assert_eq!(
        (o.client_id.as_deref(), o.callback_port),
        (Some("pub-client-id"), Some(8080))
    );
    assert_eq!(o.scopes, ["read"]);
}

#[test]
fn no_source_file_names_a_token_store() {
    // The only mention is Hermes's list of credential files it never touches.
    let mut stack = vec![std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src")];
    while let Some(d) = stack.pop() {
        for e in fs::read_dir(d).unwrap() {
            let p = e.unwrap().path();
            if p.is_dir() {
                stack.push(p);
                continue;
            }
            let text = fs::read_to_string(&p).unwrap();
            for name in ["mcp-oauth-tokens", ".credentials.json", "keyring"] {
                assert!(!text.contains(name), "{} names {name}", p.display());
            }
            if !p.ends_with("hermes.rs") {
                assert!(!text.contains("auth.json"), "{}", p.display());
            }
        }
    }
}
