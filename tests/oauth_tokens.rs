//! MCP OAuth token stores in the encrypted secrets bundle (#18, second phase).
//!
//! Gemini CLI's documented `~/.gemini/mcp-oauth-tokens.json` travels only
//! inside `secrets.age`, as opaque bytes, and is written back only to Gemini
//! CLI, only when the server URLs it was captured with are unchanged, and
//! never over an existing token file. Every token here is a made-up one; the
//! tests look for each in every place a leak could land: the profile
//! directory (ciphertext included), the state (undo) directory, the other
//! tools' files, and stdout and stderr.

mod common;

use std::fs;
use std::path::{Path, PathBuf};

use common::*;
use kitbag::bundle;
use predicates::prelude::*;
use predicates::str::contains;
use serde_json::json;

const ACCESS: &str = "ya29.FAKE-gemini-access-zzzz";
const REFRESH: &str = "fake-refresh-token-gemini-9999";
const PASS: &str = "fake-test-passphrase-not-real";
const LINEAR: &str = "https://mcp.example.com/mcp";
const NOTION: &str = "https://notion.example.com/mcp";
const TOKENS_REL: &str = "home/.gemini/mcp-oauth-tokens.json";

const VALUES: [&str; 3] = [ACCESS, REFRESH, PASS];

fn write(sb: &Sandbox, rel: &str, text: &str) {
    let p = sb.path(rel);
    fs::create_dir_all(p.parent().unwrap()).unwrap();
    fs::write(p, text).unwrap();
}

/// A made-up token file. Its layout is invented for the test only; kitbag
/// never parses it.
fn token_file() -> String {
    json!({"linear": {"token": {"accessToken": ACCESS, "refreshToken": REFRESH}}}).to_string()
}

/// A Gemini CLI setup with two remote servers and a token file. Returns
/// the passphrase file.
fn source_setup(sb: &Sandbox) -> PathBuf {
    write(
        sb,
        "home/.gemini/settings.json",
        &json!({"mcpServers": {
            "linear": {"httpUrl": LINEAR, "oauth": {"enabled": true, "clientId": "pub-client-id"}},
            "notion": {"httpUrl": NOTION}
        }})
        .to_string(),
    );
    write(sb, TOKENS_REL, &token_file());
    write(sb, "pass.txt", &format!("{PASS}\n"));
    sb.path("pass.txt")
}

fn bytes_under(dir: &Path) -> Vec<(PathBuf, Vec<u8>)> {
    fn walk(dir: &Path, out: &mut Vec<(PathBuf, Vec<u8>)>) {
        let Ok(rd) = fs::read_dir(dir) else { return };
        for e in rd.flatten() {
            let p = e.path();
            if e.file_type().unwrap().is_dir() {
                walk(&p, out);
            } else if let Ok(b) = fs::read(&p) {
                out.push((p, b));
            }
        }
    }
    let mut out = Vec::new();
    walk(dir, &mut out);
    out
}

fn has(hay: &[u8], needle: &str) -> bool {
    hay.windows(needle.len()).any(|w| w == needle.as_bytes())
}

/// None of the fake tokens (or the passphrase) in any file under `dir`,
/// except the files in `allowed`.
#[track_caller]
fn assert_absent_under(dir: &Path, allowed: &[PathBuf]) {
    for (p, b) in bytes_under(dir) {
        if allowed.contains(&p) {
            continue;
        }
        for n in VALUES {
            assert!(!has(&b, n), "{} holds a token or passphrase", p.display());
        }
    }
}

#[track_caller]
fn assert_silent(out: &std::process::Output) {
    for n in VALUES {
        assert!(!has(&out.stdout, n), "stdout shows a token or passphrase");
        assert!(!has(&out.stderr, n), "stderr shows a token or passphrase");
    }
}

fn stdout(out: &std::process::Output) -> String {
    String::from_utf8_lossy(&out.stdout).into_owned()
}

fn capture_with_bundle(sb: &Sandbox, pass: &Path) -> std::process::Output {
    sb.cli()
        .args(["capture", "--from", "gemini-cli", "-o"])
        .arg(sb.path("prof"))
        .args(["--include", "secrets", "--secrets-passphrase-file"])
        .arg(pass)
        .assert()
        .success()
        .get_output()
        .clone()
}

fn open_bundle(sb: &Sandbox) -> bundle::Bundle {
    bundle::open_file(
        &sb.path("prof/secrets.age"),
        &bundle::Unlock {
            passphrase: Some(age::secrecy::SecretString::from(PASS.to_string())),
            identities: vec![],
        },
    )
    .unwrap()
}

/// `kitbag apply <src>/prof --to <to> --secrets-from ... <extra>` on `dst`.
fn apply(
    dst: &Sandbox,
    src: &Sandbox,
    pass: &Path,
    to: &str,
    extra: &[&str],
) -> assert_cmd::assert::Assert {
    dst.cli()
        .args(["apply"])
        .arg(src.path("prof"))
        .args(["--to", to, "--secrets-from"])
        .arg(src.path("prof/secrets.age"))
        .arg("--secrets-passphrase-file")
        .arg(pass)
        .args(extra)
        .assert()
}

#[test]
fn a_bare_capture_leaves_the_token_file_alone() {
    let sb = Sandbox::empty();
    source_setup(&sb);
    let out = sb
        .cli()
        .args(["capture", "--from", "gemini-cli", "-o"])
        .arg(sb.path("prof"))
        .assert()
        .success()
        .get_output()
        .clone();
    assert!(!sb.path("prof/secrets.age").exists());
    assert_absent_under(&sb.path("prof"), &[]);
    assert_absent_under(&sb.state(), &[]);
    assert_silent(&out);
}

#[test]
fn include_secrets_carries_the_gemini_token_file_only_in_the_bundle() {
    let sb = Sandbox::empty();
    let pass = source_setup(&sb);
    let out = capture_with_bundle(&sb, &pass);
    assert_silent(&out);
    let shown = stdout(&out);
    assert!(
        shown.contains(
            "gemini-cli MCP OAuth tokens (~/.gemini/mcp-oauth-tokens.json) goes to secrets.age"
        ),
        "{shown}"
    );

    // The profile directory, ciphertext included, and the state directory
    // hold no token; the manifest doesn't mention the store.
    assert_absent_under(&sb.path("prof"), &[]);
    assert_absent_under(&sb.state(), &[]);
    assert!(!sb.read("prof/kitbag.toml").contains("mcp-oauth-tokens"));

    // Inside: the exact bytes, tagged with the tool, and the server URLs
    // from Gemini's config.
    let b = open_bundle(&sb);
    assert_eq!(b.len(), 0, "no variable was invented for the store");
    let stores = b.token_stores();
    assert_eq!(stores.len(), 1);
    assert_eq!(stores[0].tool, "gemini-cli");
    assert_eq!(stores[0].kind, "gemini-mcp-oauth-tokens");
    assert_eq!(stores[0].content.expose(), token_file());
    assert_eq!(
        stores[0].servers.get("linear").map(String::as_str),
        Some(LINEAR)
    );
    assert_eq!(
        stores[0].servers.get("notion").map(String::as_str),
        Some(NOTION)
    );
    assert!(!format!("{b:?}").contains(ACCESS));

    // Capturing from another tool never reads Gemini's file.
    let other = Sandbox::empty();
    let pass = source_setup(&other);
    write(
        &other,
        "home/.cursor/mcp.json",
        &json!({"mcpServers": {"x": {"url": LINEAR}}}).to_string(),
    );
    other
        .cli()
        .args(["capture", "--from", "cursor", "-o"])
        .arg(other.path("prof"))
        .args(["--include", "secrets", "--secrets-passphrase-file"])
        .arg(&pass)
        .assert()
        .success()
        .stdout(contains("mcp-oauth-tokens").not());
    assert!(!other.path("prof/secrets.age").exists());
}

#[test]
fn apply_writes_the_store_only_to_geminis_documented_path_and_never_to_undo() {
    let sb = Sandbox::empty();
    let pass = source_setup(&sb);
    capture_with_bundle(&sb, &pass);
    let dst = Sandbox::empty();
    let dest = dst.path(TOKENS_REL);

    // The plan names the tool and path, never a token, and writes nothing.
    let out = apply(&dst, &sb, &pass, "gemini-cli", &["--dry-run"])
        .success()
        .get_output()
        .clone();
    assert_silent(&out);
    let shown = stdout(&out);
    assert!(
        shown.contains("~/.gemini/mcp-oauth-tokens.json")
            && shown.contains("not in the undo record"),
        "{shown}"
    );
    assert!(!dest.exists());
    assert_absent_under(&dst.home(), &[]);

    let out = apply(&dst, &sb, &pass, "gemini-cli", &["--yes"])
        .success()
        .get_output()
        .clone();
    assert_silent(&out);
    assert!(stdout(&out).contains("Wrote gemini-cli MCP OAuth tokens"));
    assert_eq!(fs::read_to_string(&dest).unwrap(), token_file());
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = fs::metadata(&dest).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600);
    }
    // Only that file holds the tokens; the undo record holds none.
    assert_absent_under(&dst.home(), std::slice::from_ref(&dest));
    assert!(
        !bytes_under(&dst.state()).is_empty(),
        "an undo record was written"
    );
    assert_absent_under(&dst.state(), &[]);

    // Undo reverts the files and leaves the token store, as documented.
    let out = dst
        .cli()
        .args(["undo", "--force"])
        .assert()
        .get_output()
        .clone();
    assert_silent(&out);
    assert_eq!(fs::read_to_string(&dest).unwrap(), token_file());
    assert_absent_under(&dst.state(), &[]);

    // Applying again doesn't overwrite the file it wrote.
    apply(&dst, &sb, &pass, "gemini-cli", &["--yes"])
        .success()
        .stdout(contains("already exists and is not empty"));

    // With every file already matching, the store alone is still written,
    // and not without confirmation.
    apply(&dst, &sb, &pass, "gemini-cli", &["--yes"]).success();
    fs::remove_file(&dest).unwrap();
    apply(&dst, &sb, &pass, "gemini-cli", &[])
        .failure()
        .stderr(contains("not writing without confirmation"));
    assert!(!dest.exists());
    let out = apply(&dst, &sb, &pass, "gemini-cli", &["--yes"])
        .success()
        .stdout(contains("Up to date"))
        .stdout(contains("Wrote gemini-cli MCP OAuth tokens"))
        .get_output()
        .clone();
    assert_silent(&out);
    assert_eq!(fs::read_to_string(&dest).unwrap(), token_file());
    assert_absent_under(&dst.state(), &[]);
}

#[test]
fn an_empty_token_file_is_filled_but_a_non_empty_one_is_kept() {
    let sb = Sandbox::empty();
    let pass = source_setup(&sb);
    capture_with_bundle(&sb, &pass);

    let dst = Sandbox::empty();
    write(&dst, TOKENS_REL, "");
    apply(&dst, &sb, &pass, "gemini-cli", &["--yes"]).success();
    assert_eq!(dst.read(TOKENS_REL), token_file());

    let dst = Sandbox::empty();
    let theirs = json!({"other-server": {"token": "fake-other-machine-token"}}).to_string();
    write(&dst, TOKENS_REL, &theirs);
    let out = apply(&dst, &sb, &pass, "gemini-cli", &["--yes"])
        .success()
        .get_output()
        .clone();
    assert_silent(&out);
    let shown = stdout(&out);
    assert!(shown.contains("not merged or overwritten"), "{shown}");
    assert!(shown.contains("/mcp auth notion"), "{shown}");
    assert_eq!(dst.read(TOKENS_REL), theirs);
    assert_absent_under(&dst.home(), &[]);
    assert_absent_under(&dst.state(), &[]);
}

#[test]
fn another_tool_never_gets_the_gemini_store() {
    let sb = Sandbox::empty();
    let pass = source_setup(&sb);
    capture_with_bundle(&sb, &pass);
    let dst = Sandbox::empty();
    let out = apply(
        &dst,
        &sb,
        &pass,
        "cursor,claude-code,codex,opencode",
        &["--yes"],
    )
    .success()
    .get_output()
    .clone();
    assert_silent(&out);
    let shown = stdout(&out);
    assert!(shown.contains("only ever written to gemini-cli"), "{shown}");
    assert!(!dst.path(TOKENS_REL).exists());
    assert!(dst.path("home/.cursor/mcp.json").exists());
    assert_absent_under(&dst.home(), &[]);
    assert_absent_under(&dst.state(), &[]);
}

#[test]
fn a_changed_url_refuses_the_write_and_asks_for_a_sign_in() {
    let sb = Sandbox::empty();
    let pass = source_setup(&sb);
    capture_with_bundle(&sb, &pass);

    // In the profile.
    let toml = sb.read("prof/kitbag.toml");
    assert!(toml.contains(NOTION), "{toml}");
    fs::write(
        sb.path("prof/kitbag.toml"),
        toml.replace(NOTION, "https://elsewhere.example.com/mcp"),
    )
    .unwrap();
    let dst = Sandbox::empty();
    let out = apply(&dst, &sb, &pass, "gemini-cli", &["--yes"])
        .success()
        .get_output()
        .clone();
    assert_silent(&out);
    let shown = stdout(&out);
    assert!(
        shown.contains("the URL of MCP server(s) notion changed since capture"),
        "{shown}"
    );
    assert!(shown.contains("/mcp auth notion"), "{shown}");
    assert!(!dst.path(TOKENS_REL).exists());
    assert_absent_under(&dst.home(), &[]);
    assert_absent_under(&dst.state(), &[]);

    // In the destination's own config (kept there with --no-overwrite).
    fs::write(sb.path("prof/kitbag.toml"), &toml).unwrap();
    let dst = Sandbox::empty();
    write(
        &dst,
        "home/.gemini/settings.json",
        &json!({"mcpServers": {"linear": {"httpUrl": "https://other.example.com/mcp"}}})
            .to_string(),
    );
    apply(&dst, &sb, &pass, "gemini-cli", &["--yes", "--no-overwrite"])
        .success()
        .stdout(contains(
            "the URL of MCP server(s) linear changed since capture",
        ))
        .stdout(contains("/mcp auth linear"));
    assert!(!dst.path(TOKENS_REL).exists());
}

#[test]
fn secrets_list_names_the_store_and_never_its_tokens() {
    let sb = Sandbox::empty();
    let pass = source_setup(&sb);
    capture_with_bundle(&sb, &pass);
    let out = sb
        .cli()
        .args(["secrets", "list"])
        .arg(sb.path("prof"))
        .arg("--secrets-passphrase-file")
        .arg(&pass)
        .assert()
        .success()
        .stdout(contains("1 token store(s)"))
        .stdout(contains(
            "gemini-cli MCP OAuth tokens (~/.gemini/mcp-oauth-tokens.json)",
        ))
        .get_output()
        .clone();
    assert_silent(&out);
}

#[test]
fn git_sources_and_remote_still_refuse_the_bundle() {
    let sb = Sandbox::empty();
    let pass = source_setup(&sb);
    capture_with_bundle(&sb, &pass);
    let bundle_arg = sb.path("prof/secrets.age");
    for src in [
        "https://example.invalid/profile.git",
        "github:someone/profile",
    ] {
        let out = sb
            .cli()
            .args([
                "apply",
                src,
                "--to",
                "gemini-cli",
                "--yes",
                "--secrets-from",
            ])
            .arg(&bundle_arg)
            .arg("--secrets-passphrase-file")
            .arg(&pass)
            .assert()
            .failure()
            .stderr(contains("git source"))
            .get_output()
            .clone();
        assert_silent(&out);
    }
    let out = sb
        .cli()
        .args(["apply"])
        .arg(sb.path("prof"))
        .args([
            "--to",
            "gemini-cli",
            "--remote",
            "user@example.invalid",
            "--secrets-from",
        ])
        .arg(&bundle_arg)
        .arg("--secrets-passphrase-file")
        .arg(&pass)
        .assert()
        .failure()
        .stderr(contains("--remote"))
        .get_output()
        .clone();
    assert_silent(&out);
    // The source machine's own token file is the only copy outside the bundle.
    assert_absent_under(&sb.home(), &[sb.path(TOKENS_REL)]);
    assert_absent_under(&sb.state(), &[]);
}

#[test]
fn diff_and_inspect_never_open_the_bundle() {
    let sb = Sandbox::empty();
    let pass = source_setup(&sb);
    capture_with_bundle(&sb, &pass);
    let dst = Sandbox::empty();
    for cmd in ["diff", "inspect"] {
        let out = dst
            .cli()
            .arg(cmd)
            .arg(sb.path("prof"))
            .args(["--to", "gemini-cli"])
            .assert()
            .get_output()
            .clone();
        assert_silent(&out);
        assert!(!stdout(&out).contains("mcp-oauth-tokens"));
    }
    assert!(!dst.path(TOKENS_REL).exists());
}
