//! `kitbag doctor --mcp-tokens`: what each MCP server's tool definitions cost (#51).
//!
//! The fake MCP server is this test binary itself: run with `KITBAG_FAKE_MCP`
//! set, the `fake_mcp_server` test below speaks MCP over stdin/stdout instead
//! of passing. libtest's own "running 1 test" line on stdout doubles as the
//! log noise real servers print, which kitbag must skip.

mod common;

use std::fs;
use std::io::{BufRead, Write};
use std::path::{Path, PathBuf};

use common::Sandbox;
use serde_json::{Value, json};

/// Text that only ever appears in the fake server's output, never in kitbag's.
const SERVER_ONLY: &str = "FAKE-SERVER-PRIVATE-TEXT";
/// A fake secret given to the server through a `${VAR}` reference.
const SECRET: &str = "fake-secret-value-for-tests-only";

/// The fake server's tools, in two `tools/list` pages.
fn pages() -> Vec<Vec<Value>> {
    vec![
        vec![json!({
            "name": "echo",
            "description": format!("Echo the input back. {SERVER_ONLY}"),
            "inputSchema": {
                "type": "object",
                "properties": {"text": {"type": "string", "description": "What to echo"}},
                "required": ["text"],
            },
            "annotations": {"readOnlyHint": true},
        })],
        vec![json!({
            "name": "add",
            "description": "Add two numbers",
            "inputSchema": {
                "type": "object",
                "properties": {"a": {"type": "number"}, "b": {"type": "number"}},
            },
        })],
    ]
}

fn reply(out: &mut impl Write, msg: Value) {
    writeln!(out, "{msg}").unwrap();
    out.flush().unwrap();
}

#[test]
fn fake_mcp_server() {
    let Ok(mode) = std::env::var("KITBAG_FAKE_MCP") else {
        return;
    };
    if let Ok(marker) = std::env::var("KITBAG_FAKE_MCP_MARKER") {
        fs::write(marker, "started").unwrap();
    }
    match mode.as_str() {
        "exit" => std::process::exit(3),
        "hang" => loop {
            std::thread::sleep(std::time::Duration::from_secs(60));
        },
        _ => {}
    }
    // The reference in the profile must arrive resolved.
    if std::env::var("FAKE_MCP_TOKEN").as_deref() != Ok(SECRET) {
        std::process::exit(4);
    }
    let mut out = std::io::stdout().lock();
    // Log noise, as some servers print. It also ends the line libtest left
    // open ("test fake_mcp_server ... "), so the replies start on fresh lines.
    writeln!(out, "starting up, {SERVER_ONLY} {SECRET}").unwrap();
    for line in std::io::stdin().lock().lines() {
        let Ok(msg) = serde_json::from_str::<Value>(&line.unwrap()) else {
            continue;
        };
        let Some(id) = msg.get("id").cloned() else {
            continue;
        };
        let result = match msg["method"].as_str() {
            Some("initialize") => json!({
                "protocolVersion": msg["params"]["protocolVersion"],
                "capabilities": {"tools": {}},
                "serverInfo": {"name": "fake", "version": "0"},
            }),
            Some("tools/list") => {
                // A notification first, which kitbag must skip.
                reply(
                    &mut out,
                    json!({"jsonrpc": "2.0", "method": "notifications/message", "params": {"data": SERVER_ONLY}}),
                );
                if msg["params"]["cursor"] == "page-2" {
                    json!({"tools": pages()[1]})
                } else {
                    json!({"tools": pages()[0], "nextCursor": "page-2"})
                }
            }
            _ => {
                reply(
                    &mut out,
                    json!({"jsonrpc": "2.0", "id": id, "error": {"code": -32601, "message": "no"}}),
                );
                continue;
            }
        };
        reply(
            &mut out,
            json!({"jsonrpc": "2.0", "id": id, "result": result}),
        );
    }
    std::process::exit(0);
}

/// The tokens kitbag should report for the fake server.
fn expected_tokens() -> usize {
    let tools: Vec<Value> = pages().into_iter().flatten().collect();
    kitbag::mcp_tokens::estimate(&tools).tokens
}

/// A stdio server definition that runs the fake server in `mode`, writing
/// `marker` when it starts.
fn fake(mode: &str, marker: &Path) -> toml::Table {
    let exe = std::env::current_exe().unwrap();
    let mut t = toml::Table::new();
    t.insert("command".into(), exe.display().to_string().into());
    t.insert(
        "args".into(),
        toml::Value::Array(
            [
                "fake_mcp_server",
                "--exact",
                "--nocapture",
                "--test-threads",
                "1",
            ]
            .map(Into::into)
            .to_vec(),
        ),
    );
    let mut env = toml::Table::new();
    env.insert("KITBAG_FAKE_MCP".into(), mode.into());
    env.insert(
        "KITBAG_FAKE_MCP_MARKER".into(),
        marker.display().to_string().into(),
    );
    env.insert("FAKE_MCP_TOKEN".into(), "${FAKE_MCP_TOKEN}".into());
    t.insert("env".into(), env.into());
    t
}

/// Write a profile with `servers` and return its directory.
fn profile(sb: &Sandbox, servers: Vec<(&str, toml::Table)>) -> PathBuf {
    let dir = sb.path("profile");
    fs::create_dir_all(&dir).unwrap();
    let mut mcp = toml::Table::new();
    for (name, s) in servers {
        mcp.insert(name.into(), s.into());
    }
    let mut manifest = toml::Table::new();
    manifest.insert("kitbag".into(), 1.into());
    manifest.insert("name".into(), "tokens".into());
    manifest.insert("mcp_servers".into(), mcp.into());
    fs::write(dir.join("kitbag.toml"), toml::to_string(&manifest).unwrap()).unwrap();
    dir
}

/// Run doctor with `args`; returns (exit code, stdout).
fn doctor(sb: &Sandbox, args: &[&str]) -> (i32, String) {
    let out = sb
        .cli()
        .arg("doctor")
        .args(args)
        .env("FAKE_MCP_TOKEN", SECRET)
        .output()
        .unwrap();
    let stdout = String::from_utf8(out.stdout).unwrap();
    let stderr = String::from_utf8_lossy(&out.stderr);
    for text in [stdout.as_str(), &stderr] {
        assert!(!text.contains(SECRET), "secret printed:\n{text}");
        assert!(
            !text.contains(SERVER_ONLY),
            "server output printed:\n{text}"
        );
    }
    (out.status.code().unwrap(), stdout)
}

/// The line for server `name` in the section for tool `tool`.
fn row<'a>(out: &'a str, tool: &str, name: &str) -> &'a str {
    let section = out
        .split_once("MCP tool definitions")
        .unwrap_or_else(|| panic!("no MCP section in:\n{out}"))
        .1;
    let start = section
        .find(&format!("\n  {tool}\n"))
        .unwrap_or_else(|| panic!("no {tool} in:\n{out}"));
    section[start + 1..]
        .lines()
        .skip(1)
        .take_while(|l| l.starts_with("    "))
        .find(|l| l.trim_start().starts_with(&format!("{name} ")))
        .unwrap_or_else(|| panic!("no {name} under {tool} in:\n{out}"))
}

#[test]
fn with_the_flag_doctor_counts_the_tools_of_a_local_server() {
    let sb = Sandbox::empty();
    let marker = sb.path("started");
    let dir = profile(&sb, vec![("fake", fake("serve", &marker))]);
    let (code, out) = doctor(
        &sb,
        &[dir.to_str().unwrap(), "--to", "claude", "--mcp-tokens"],
    );
    assert_eq!(code, 0, "{out}");
    assert!(marker.exists());
    let tokens = expected_tokens();
    assert!(tokens > 0);
    let line = row(&out, "Claude Code", "fake");
    // Both pages count; the annotations don't.
    assert!(line.contains(&format!("~{tokens} tokens")), "{line}");
    assert!(line.contains("2 tool(s)"), "{line}");
    let total = row(&out, "Claude Code", "total");
    assert!(
        total.contains(&format!("~{tokens} tokens from 1 server(s)")),
        "{total}"
    );
    assert!(out.contains("4 characters per token"), "{out}");
}

#[test]
fn without_the_flag_doctor_starts_no_server() {
    let sb = Sandbox::empty();
    let marker = sb.path("started");
    let dir = profile(&sb, vec![("fake", fake("serve", &marker))]);
    let (code, out) = doctor(&sb, &[dir.to_str().unwrap(), "--to", "claude"]);
    assert_eq!(code, 0, "{out}");
    assert!(!out.contains("MCP tool definitions"), "{out}");
    // Give a server that was started by mistake time to write its marker.
    std::thread::sleep(std::time::Duration::from_millis(500));
    assert!(!marker.exists(), "the server was started");
}

#[test]
fn a_failing_server_is_reported_and_the_others_still_count() {
    let sb = Sandbox::empty();
    let marker = sb.path("started");
    let mut missing = toml::Table::new();
    missing.insert(
        "command".into(),
        "kitbag-no-such-mcp-server-for-tests".into(),
    );
    let mut needs_var = fake("serve", &sb.path("never"));
    needs_var.insert(
        "args".into(),
        toml::Value::Array(vec!["${UNSET_FAKE_VAR}".into()]),
    );
    let dir = profile(
        &sb,
        vec![
            ("a-missing", missing),
            ("b-exits", fake("exit", &sb.path("exits"))),
            ("c-hangs", fake("hang", &sb.path("hangs"))),
            ("d-fake", fake("serve", &marker)),
            ("e-needs-var", needs_var),
        ],
    );
    let (code, out) = doctor(
        &sb,
        &[
            dir.to_str().unwrap(),
            "--to",
            "claude-code",
            "--mcp-tokens",
            "--mcp-timeout",
            "2",
        ],
    );
    assert_eq!(code, 0, "{out}");
    let tool = "Claude Code";
    assert!(
        row(&out, tool, "a-missing").contains("failed could not start it"),
        "{out}"
    );
    assert!(
        row(&out, tool, "b-exits").contains("failed it exited before listing its tools"),
        "{out}"
    );
    assert!(
        row(&out, tool, "c-hangs").contains("failed timed out after 2s"),
        "{out}"
    );
    assert!(
        row(&out, tool, "e-needs-var")
            .contains("skipped not started: needs ${UNSET_FAKE_VAR}, which is unset"),
        "{out}"
    );
    assert!(!sb.path("never").exists());
    let tokens = expected_tokens();
    assert!(
        row(&out, tool, "d-fake").contains(&format!("~{tokens} tokens")),
        "{out}"
    );
    assert!(
        row(&out, tool, "total").contains(&format!("~{tokens} tokens from 1 server(s)")),
        "{out}"
    );
}

/// A wrapper such as `sh -c` that times out: what it started is stopped too.
#[cfg(unix)]
#[test]
fn a_timed_out_wrappers_children_are_stopped_too() {
    let sb = Sandbox::empty();
    let pid_file = sb.path("grandchild.pid");
    let mut wrapper = toml::Table::new();
    wrapper.insert("command".into(), "sh".into());
    wrapper.insert(
        "args".into(),
        toml::Value::Array(vec![
            "-c".into(),
            format!(
                "sleep 4242 </dev/null & echo $! > '{}'; wait",
                pid_file.display()
            )
            .into(),
        ]),
    );
    let dir = profile(&sb, vec![("wrapper", wrapper)]);
    let (code, out) = doctor(
        &sb,
        &[
            dir.to_str().unwrap(),
            "--to",
            "claude",
            "--mcp-tokens",
            "--mcp-timeout",
            "1",
        ],
    );
    assert_eq!(code, 0, "{out}");
    assert!(
        row(&out, "Claude Code", "wrapper").contains("failed timed out after 1s"),
        "{out}"
    );
    let pid = fs::read_to_string(&pid_file).unwrap().trim().to_string();
    assert!(!pid.is_empty());
    let alive = || {
        std::process::Command::new("kill")
            .args(["-0", &pid])
            .stderr(std::process::Stdio::null())
            .status()
            .unwrap()
            .success()
    };
    // Killed at once; give init a moment to reap it.
    let gone = (0..50).any(|_| {
        if alive() {
            std::thread::sleep(std::time::Duration::from_millis(100));
            false
        } else {
            true
        }
    });
    if !gone {
        let _ = std::process::Command::new("kill").arg(&pid).status();
        panic!("the server's child {pid} outlived the timeout");
    }
}

#[test]
fn a_server_disabled_for_a_tool_is_not_counted_against_it() {
    let sb = Sandbox::empty();
    let marker = sb.path("started");
    let mut heavy = fake("serve", &marker);
    heavy.insert(
        "disabled_on".into(),
        toml::Value::Array(vec!["claude".into()]),
    );
    let mut remote = toml::Table::new();
    remote.insert("url".into(), "https://mcp.example.invalid/mcp".into());
    let dir = profile(&sb, vec![("heavy", heavy), ("remote", remote)]);
    let (code, out) = doctor(
        &sb,
        &[
            dir.to_str().unwrap(),
            "--to",
            "claude-code,cursor",
            "--mcp-tokens",
        ],
    );
    assert_eq!(code, 0, "{out}");
    let skipped = row(&out, "Claude Code", "heavy");
    assert!(
        skipped.contains(
            "skipped disabled for this tool in the profile (listed in 'disabled_on'); not counted"
        ),
        "{out}"
    );
    assert!(
        row(&out, "Claude Code", "total").contains("~0 tokens from 0 server(s)"),
        "{out}"
    );
    let tokens = expected_tokens();
    assert!(
        row(&out, "Cursor", "heavy").contains(&format!("~{tokens} tokens")),
        "{out}"
    );
    assert!(
        row(&out, "Cursor", "remote").contains("skipped remote (http) server, not contacted"),
        "{out}"
    );
    assert!(
        row(&out, "Cursor", "total").contains(&format!("~{tokens} tokens from 1 server(s)")),
        "{out}"
    );
}

#[test]
fn without_a_profile_the_servers_in_each_tools_config_are_counted() {
    let sb = Sandbox::empty();
    // Under the home directory, so capture turns it into ~/... and doctor
    // has to expand it back.
    let marker = sb.home().join("started");
    let s = fake("serve", &marker);
    let config = json!({"mcpServers": {
        "fake": {
            "type": "stdio",
            "command": s["command"].as_str().unwrap(),
            "args": s["args"].as_array().unwrap().iter().map(|a| a.as_str().unwrap()).collect::<Vec<_>>(),
            "env": {
                "KITBAG_FAKE_MCP": "serve",
                "KITBAG_FAKE_MCP_MARKER": marker.display().to_string(),
                "FAKE_MCP_TOKEN": "${FAKE_MCP_TOKEN}",
            },
        },
    }});
    fs::write(sb.home().join(".claude.json"), config.to_string()).unwrap();
    fs::create_dir_all(sb.home().join(".codex")).unwrap();
    fs::write(
        sb.home().join(".codex/config.toml"),
        "[mcp_servers.off]\ncommand = \"kitbag-no-such-mcp-server-for-tests\"\nenabled = false\n",
    )
    .unwrap();
    let (code, out) = doctor(&sb, &["--to", "claude,codex", "--mcp-tokens"]);
    assert_eq!(code, 0, "{out}");
    let tokens = expected_tokens();
    assert!(
        row(&out, "Claude Code", "fake").contains(&format!("~{tokens} tokens")),
        "{out}"
    );
    assert!(marker.exists());
    assert!(
        row(&out, "OpenAI Codex CLI", "off")
            .contains("skipped disabled in OpenAI Codex CLI's config; not counted"),
        "{out}"
    );
}

#[test]
fn the_flags_are_checked() {
    let sb = Sandbox::empty();
    sb.cli()
        .args(["doctor", "--mcp-timeout", "5"])
        .assert()
        .failure()
        .stderr(predicates::str::contains("--mcp-tokens"));
    sb.cli()
        .args(["doctor", "--why", "--mcp-tokens"])
        .assert()
        .failure();
    sb.cli()
        .args(["doctor", "--mcp-tokens", "--mcp-timeout", "0"])
        .assert()
        .failure();
    sb.cli()
        .args(["doctor", "--help"])
        .assert()
        .success()
        .stdout(predicates::str::contains("--mcp-tokens"));
}
