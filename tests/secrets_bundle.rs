//! The opt-in encrypted secrets bundle (#9, first slice): `capture --include
//! secrets` writes the redacted values only to `secrets.age`; `apply
//! --secrets-from` and `secrets list` open it in memory and show names only;
//! `secrets set` and `secrets rotate` (#136) rewrite it, still encrypted;
//! apply fills only the tools that can't read env vars from it, never
//! reaching the undo record (#137).
//! Every secret and passphrase here is a made-up one; the tests look for each
//! in every place a leak could land: the profile directory, the state
//! (undo) directory, the tools' config files, and stdout and stderr.

mod common;

use std::fs;
use std::path::{Path, PathBuf};

use age::secrecy::ExposeSecret;
use common::*;
use kitbag::adapters::Scope;
use kitbag::bundle;
use predicates::prelude::*;
use predicates::str::contains;
use serde_json::json;

const TOKEN: &str = "ghp_FAKEfakeFAKE0123456789abcdefghijkl";
const BEARER: &str = "fake-local-token-value-1234";
const CLIENT_SECRET: &str = "fake-literal-client-secret-0123";
const PASS: &str = "fake-test-passphrase-not-real";
const URL: &str = "https://mcp.example.com/mcp";

const VALUES: [&str; 3] = [TOKEN, BEARER, CLIENT_SECRET];

fn write(sb: &Sandbox, rel: &str, text: &str) {
    let p = sb.path(rel);
    fs::create_dir_all(p.parent().unwrap()).unwrap();
    fs::write(p, text).unwrap();
}

/// A Gemini CLI setup holding three literal secrets: an env token, a bearer
/// header and an OAuth client secret. Returns the passphrase file's path.
fn source_setup(sb: &Sandbox) -> PathBuf {
    write(
        sb,
        "home/.gemini/settings.json",
        &json!({"mcpServers": {
            "github": {
                "command": "npx",
                "args": ["-y", "@modelcontextprotocol/server-github"],
                "env": {"GITHUB_PERSONAL_ACCESS_TOKEN": TOKEN}
            },
            "linear": {
                "httpUrl": URL,
                "headers": {"Authorization": format!("Bearer {BEARER}")},
                "oauth": {"enabled": true, "clientId": "pub-client-id", "clientSecret": CLIENT_SECRET}
            }
        }})
        .to_string(),
    );
    write(sb, "pass.txt", &format!("{PASS}\n"));
    sb.path("pass.txt")
}

/// Every byte of every file under `dir` (hidden files and binaries too).
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

/// Assert none of `needles` is in any file under `dir`, naming the file.
#[track_caller]
fn assert_absent_under(dir: &Path, needles: &[&str]) {
    for (p, b) in bytes_under(dir) {
        for n in needles {
            assert!(!has(&b, n), "{} holds a secret or passphrase", p.display());
        }
    }
}

#[track_caller]
fn assert_silent(out: &std::process::Output, needles: &[&str]) {
    for n in needles {
        assert!(!has(&out.stdout, n), "stdout shows a secret or passphrase");
        assert!(!has(&out.stderr, n), "stderr shows a secret or passphrase");
    }
}

fn everything() -> Vec<&'static str> {
    let mut v = VALUES.to_vec();
    v.push(PASS);
    v
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

#[test]
fn a_bare_capture_writes_no_bundle_and_no_value() {
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
    assert!(!sb.path("prof/.gitignore").exists());
    assert_absent_under(&sb.path("prof"), &everything());
    assert_silent(&out, &everything());
    // The client secret is still left out, with a pointer to the flag.
    let toml = sb.read("prof/kitbag.toml");
    assert!(!toml.contains("client_secret"));
    assert!(String::from_utf8_lossy(&out.stdout).contains("--include secrets"));
}

#[test]
fn include_secrets_puts_values_only_in_the_encrypted_file() {
    let sb = Sandbox::empty();
    let pass = source_setup(&sb);
    let out = capture_with_bundle(&sb, &pass);

    // Only references in the profile; a bundle, a .gitignore, nothing plaintext.
    let toml = sb.read("prof/kitbag.toml");
    assert!(toml.contains("${GITHUB_PERSONAL_ACCESS_TOKEN}"), "{toml}");
    assert!(
        toml.contains("client_secret = \"${LINEAR_CLIENT_SECRET}\""),
        "{toml}"
    );
    let mut files: Vec<String> = fs::read_dir(sb.path("prof"))
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    files.sort();
    assert_eq!(files, [".gitignore", "kitbag.toml", "secrets.age"]);
    assert!(
        sb.read("prof/.gitignore")
            .lines()
            .any(|l| l == "secrets.age")
    );

    // No value, and not the passphrase, anywhere a leak could land: the
    // profile directory (ciphertext included), the state directory, stdout, stderr.
    assert_absent_under(&sb.path("prof"), &everything());
    assert!(
        !sb.state().exists()
            || bytes_under(&sb.state())
                .iter()
                .all(|(_, b)| everything().iter().all(|n| !has(b, n)))
    );
    assert_silent(&out, &everything());
    let shown = String::from_utf8_lossy(&out.stdout);
    assert!(
        shown.contains("secrets.age") && shown.contains("encrypted"),
        "{shown}"
    );

    // It is age's format, and holds each value under its variable.
    let raw = fs::read(sb.path("prof/secrets.age")).unwrap();
    assert!(bundle::looks_like_age(&raw) && bundle::needs_passphrase(&raw));
    let b = open_bundle(&sb);
    assert_eq!(b.len(), 3);
    assert_eq!(
        b.get("GITHUB_PERSONAL_ACCESS_TOKEN").unwrap().expose(),
        TOKEN
    );
    assert_eq!(
        b.get("LINEAR_CLIENT_SECRET").unwrap().expose(),
        CLIENT_SECRET
    );
    assert!(
        b.names()
            .iter()
            .any(|(n, _)| n.starts_with("LINEAR") && b.get(n).unwrap().expose() == BEARER)
    );

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = fs::metadata(sb.path("prof/secrets.age"))
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o600);
    }
}

#[test]
fn secrets_list_shows_names_and_never_values() {
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
        .stdout(contains("GITHUB_PERSONAL_ACCESS_TOKEN"))
        .stdout(contains("LINEAR_CLIENT_SECRET"))
        .stdout(contains("3 variable(s)"))
        .get_output()
        .clone();
    assert_silent(&out, &everything());

    // A wrong passphrase fails without echoing anything.
    write(&sb, "wrong.txt", "fake-wrong-passphrase-xyz\n");
    let out = sb
        .cli()
        .args(["secrets", "list"])
        .arg(sb.path("prof/secrets.age"))
        .arg("--secrets-passphrase-file")
        .arg(sb.path("wrong.txt"))
        .assert()
        .failure()
        .get_output()
        .clone();
    let mut hidden = everything();
    hidden.push("fake-wrong-passphrase-xyz");
    assert_silent(&out, &hidden);

    // Without any way to open it, and not on a terminal, it says what to give.
    sb.cli()
        .args(["secrets", "list"])
        .arg(sb.path("prof"))
        .assert()
        .failure()
        .stderr(contains("--secrets-passphrase-file"));
    // Never a git source.
    sb.cli()
        .args(["secrets", "list", "https://example.invalid/profile.git"])
        .assert()
        .failure()
        .stderr(contains("never from a git source"));
}

#[test]
fn apply_with_secrets_from_gives_tools_that_read_env_vars_references_only() {
    let sb = Sandbox::empty();
    let pass = source_setup(&sb);
    capture_with_bundle(&sb, &pass);

    // A second, empty machine.
    let dst = Sandbox::empty();
    let mut outputs = Vec::new();
    for extra in [&["--dry-run"][..], &["--yes"][..]] {
        let out = dst
            .cli()
            .args(["apply"])
            .arg(sb.path("prof"))
            .args([
                "--to",
                "cursor,claude-code,gemini-cli,codex",
                "--secrets-from",
            ])
            .arg(sb.path("prof/secrets.age"))
            .arg("--secrets-passphrase-file")
            .arg(&pass)
            .args(extra)
            .assert()
            .success()
            .stdout(contains("holds 3 variable(s)"))
            .stdout(contains("decrypted in memory only"))
            .get_output()
            .clone();
        outputs.push(out);
    }
    for out in &outputs {
        assert_silent(out, &everything());
    }
    // The tools got references, never values; the undo record holds none.
    assert_absent_under(&dst.home(), &everything());
    assert_absent_under(&dst.state(), &everything());
    assert!(
        bytes_under(&dst.state()).len() > 1,
        "an undo record was written"
    );
    let cursor = dst.read("home/.cursor/mcp.json");
    assert!(
        cursor.contains("${env:GITHUB_PERSONAL_ACCESS_TOKEN}"),
        "{cursor}"
    );

    // Undo prints and restores without showing anything either.
    let out = dst
        .cli()
        .args(["undo", "--force"])
        .assert()
        .get_output()
        .clone();
    assert_silent(&out, &everything());
    assert_absent_under(&dst.state(), &everything());
    assert_absent_under(&dst.home(), &everything());
}

#[test]
fn apply_says_which_of_the_profiles_variables_the_bundle_lacks() {
    let sb = Sandbox::empty();
    let pass = source_setup(&sb);
    capture_with_bundle(&sb, &pass);
    // Another profile that needs a variable the bundle doesn't hold.
    write(
        &sb,
        "other/kitbag.toml",
        "kitbag = 1\nname = \"other\"\n[mcp_servers.x]\nurl = \"https://mcp.example.com/x\"\nheaders = { Authorization = \"Bearer ${NOT_IN_BUNDLE}\" }\n",
    );
    sb.cli()
        .args(["apply"])
        .arg(sb.path("other"))
        .args(["--to", "cursor", "--dry-run", "--secrets-from"])
        .arg(sb.path("prof/secrets.age"))
        .arg("--secrets-passphrase-file")
        .arg(&pass)
        .assert()
        .success()
        .stdout(contains(
            "no value for the profile's variable(s): NOT_IN_BUNDLE",
        ));
}

#[test]
fn a_key_encrypted_bundle_opens_with_its_identity_only() {
    let sb = Sandbox::empty();
    source_setup(&sb);
    let id = age::x25519::Identity::generate();
    write(
        &sb,
        "id.txt",
        &format!("{}\n", id.to_string().expose_secret()),
    );
    let out = sb
        .cli()
        .args(["capture", "--from", "gemini-cli", "-o"])
        .arg(sb.path("prof"))
        .args([
            "--include",
            "secrets",
            "--secrets-recipient",
            &id.to_public().to_string(),
        ])
        .assert()
        .success()
        .get_output()
        .clone();
    assert_silent(&out, &VALUES);
    assert_absent_under(&sb.path("prof"), &VALUES);
    let raw = fs::read(sb.path("prof/secrets.age")).unwrap();
    assert!(bundle::looks_like_age(&raw) && !bundle::needs_passphrase(&raw));

    let out = sb
        .cli()
        .args(["secrets", "list"])
        .arg(sb.path("prof"))
        .arg("--secrets-identity")
        .arg(sb.path("id.txt"))
        .assert()
        .success()
        .stdout(contains("LINEAR_CLIENT_SECRET"))
        .get_output()
        .clone();
    assert_silent(&out, &VALUES);
    // Another key can't open it.
    let other = age::x25519::Identity::generate();
    write(
        &sb,
        "other-id.txt",
        &format!("{}\n", other.to_string().expose_secret()),
    );
    sb.cli()
        .args(["secrets", "list"])
        .arg(sb.path("prof"))
        .arg("--secrets-identity")
        .arg(sb.path("other-id.txt"))
        .assert()
        .failure();
}

#[test]
fn bad_flag_combinations_write_nothing() {
    let sb = Sandbox::empty();
    let pass = source_setup(&sb);
    let id = age::x25519::Identity::generate().to_public().to_string();
    let prof = sb.path("prof");
    let capture = |sb: &Sandbox| {
        let mut c = sb.cli();
        c.args(["capture", "--from", "gemini-cli", "-o"]).arg(&prof);
        c
    };
    // The encryption flags mean nothing without --include secrets.
    capture(&sb)
        .arg("--secrets-passphrase-file")
        .arg(&pass)
        .assert()
        .failure()
        .stderr(contains("--include secrets"));
    // age encrypts to a passphrase or to keys, not both.
    capture(&sb)
        .args([
            "--include",
            "secrets",
            "--secrets-recipient",
            &id,
            "--secrets-passphrase-file",
        ])
        .arg(&pass)
        .assert()
        .failure()
        .stderr(contains("can't be combined"));
    // Not a key.
    capture(&sb)
        .args(["--include", "secrets", "--secrets-recipient", "not-a-key"])
        .assert()
        .failure();
    // No way to encrypt and no terminal: refused before anything is written.
    capture(&sb)
        .args(["--include", "secrets"])
        .assert()
        .failure()
        .stderr(contains("--secrets-passphrase-file"));
    assert!(!prof.exists());
    // A dry run writes nothing and asks nothing.
    let out = capture(&sb)
        .args(["--include", "secrets", "--dry-run"])
        .assert()
        .success()
        .get_output()
        .clone();
    assert_silent(&out, &everything());
    assert!(!prof.exists());
}

#[test]
fn secrets_from_is_refused_for_git_sources_and_remote_and_never_read_from_a_checkout() {
    let sb = Sandbox::empty();
    let pass = source_setup(&sb);
    capture_with_bundle(&sb, &pass);
    let bundle_arg = sb.path("prof/secrets.age");

    // A git URL, however it is spelled, never gets a bundle.
    for src in [
        "https://example.invalid/profile.git",
        "github:someone/profile",
        "git@example.invalid:x/y.git",
    ] {
        sb.cli()
            .args(["apply", src, "--to", "cursor", "--secrets-from"])
            .arg(&bundle_arg)
            .arg("--secrets-passphrase-file")
            .arg(&pass)
            .assert()
            .failure()
            .stderr(contains("git source"));
    }
    // Not with --remote either: nothing is sent to the host.
    sb.cli()
        .args(["apply"])
        .arg(sb.path("prof"))
        .args(["--remote", "user@example.invalid", "--secrets-from"])
        .arg(&bundle_arg)
        .arg("--secrets-passphrase-file")
        .arg(&pass)
        .assert()
        .failure()
        .stderr(contains("--remote"));

    // A secrets.age inside a git checkout is just a file: not read, not applied.
    let url = sb.git_profile(
        "repo",
        &[
            ("kitbag.toml", "kitbag = 1\nname = \"theirs\"\n"),
            ("instructions.md", "Be brief.\n"),
            (
                "secrets.age",
                "age-encryption.org/v1\nnot really a bundle\n",
            ),
        ],
    );
    let dst = Sandbox::empty();
    dst.cli()
        .args(["apply", &url, "--to", "claude-code", "--yes"])
        .assert()
        .success()
        .stdout(contains("secrets bundle").not());
    assert!(
        !bytes_under(&dst.home())
            .iter()
            .any(|(_, b)| has(b, "not really a bundle"))
    );
    dst.cli()
        .args(["doctor", &url])
        .assert()
        .stdout(contains("secrets.age in a git source is ignored"));
}

#[test]
fn doctor_fails_on_plaintext_secrets_files_and_a_literal_in_the_manifest() {
    let sb = Sandbox::empty();
    let pass = source_setup(&sb);
    capture_with_bundle(&sb, &pass);
    let ok = sb
        .cli()
        .args(["doctor"])
        .arg(sb.path("prof"))
        .assert()
        .get_output()
        .clone();
    let shown = String::from_utf8_lossy(&ok.stdout).into_owned();
    assert!(
        shown.contains("secrets.age") && shown.contains("encrypted"),
        "{shown}"
    );
    assert!(!shown.contains("plaintext secrets file"), "{shown}");
    assert_silent(&ok, &everything());

    // A decrypted bundle left in the profile directory.
    for name in ["secrets.json", "secrets.toml", "secrets.decrypted"] {
        write(&sb, &format!("prof/{name}"), "{\"GITHUB_TOKEN\": \"x\"}");
        let out = sb
            .cli()
            .args(["doctor"])
            .arg(sb.path("prof"))
            .assert()
            .failure()
            .get_output()
            .clone();
        let shown = String::from_utf8_lossy(&out.stdout).into_owned();
        assert!(
            shown.contains(name) && shown.contains("plaintext secrets file"),
            "{shown}"
        );
        fs::remove_file(sb.path(&format!("prof/{name}"))).unwrap();
    }

    // A secrets.age that is not age-encrypted.
    fs::write(sb.path("prof/secrets.age"), "{\"A\": \"b\"}").unwrap();
    sb.cli()
        .args(["doctor"])
        .arg(sb.path("prof"))
        .assert()
        .failure()
        .stdout(contains("not an age-encrypted file"));

    // A literal secret in kitbag.toml itself.
    let sb = Sandbox::empty();
    write(
        &sb,
        "prof/kitbag.toml",
        &format!(
            "kitbag = 1\nname = \"p\"\n[mcp_servers.gh]\ncommand = \"npx\"\nenv = {{ GITHUB_TOKEN = \"{TOKEN}\" }}\n"
        ),
    );
    let out = sb
        .cli()
        .args(["doctor"])
        .arg(sb.path("prof"))
        .assert()
        .failure()
        .get_output()
        .clone();
    assert!(
        !has(&out.stdout, TOKEN),
        "doctor must not print the literal"
    );
}

#[test]
fn captured_values_stay_off_every_library_output() {
    let sb = Sandbox::empty();
    source_setup(&sb);
    let a = kitbag::adapters::get("gemini-cli").unwrap();
    let mut env = sb.env(Scope::User);
    env.include_secrets = true;
    let r = kitbag::engine::capture(&[a], &env, "x").unwrap();
    // They are on the redactions for the bundle, and Debug never shows them.
    assert_eq!(r.redactions.iter().filter(|x| x.value.is_some()).count(), 3);
    let shown = format!("{:?} {:?} {:?}", r.redactions, r.notes, r.profile);
    for v in VALUES {
        assert!(!shown.contains(v), "{v} shown");
    }
    // Without the flag, nothing keeps a value.
    let r = kitbag::engine::capture(&[a], &sb.env(Scope::User), "x").unwrap();
    assert!(r.redactions.iter().all(|x| x.value.is_none()));
}

// ---- `secrets set` and `secrets rotate` (#136) ----

const SET_VALUE: &str = "fake-set-value-0000-not-real";
const SET_VALUE_2: &str = "fake-replaced-value-1111-not-real";
const STORE_TOKEN: &str = "fake-oauth-access-token-2222";
const NEW_PASS: &str = "fake-new-passphrase-not-real";

/// Every fake value and passphrase these tests use.
fn all_fakes() -> Vec<&'static str> {
    let mut v = everything();
    v.extend([SET_VALUE, SET_VALUE_2, STORE_TOKEN, NEW_PASS]);
    v
}

fn unlock_pass(p: &str) -> bundle::Unlock {
    bundle::Unlock {
        passphrase: Some(age::secrecy::SecretString::from(p.to_string())),
        identities: vec![],
    }
}

fn unlock_key(id: &age::x25519::Identity) -> bundle::Unlock {
    bundle::Unlock {
        passphrase: None,
        identities: vec![Box::new(id.clone())],
    }
}

/// The file names in `dir`, sorted.
fn names_in(dir: &Path) -> Vec<String> {
    let mut v: Vec<String> = fs::read_dir(dir)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    v.sort();
    v
}

/// No fake value in the profile (ciphertext, temps and all), the state
/// directory, the home, or the command's output.
#[track_caller]
fn assert_no_leak(sb: &Sandbox, out: &std::process::Output) {
    assert_absent_under(&sb.path("prof"), &all_fakes());
    assert_absent_under(&sb.state(), &all_fakes());
    assert_absent_under(&sb.home(), &[SET_VALUE, SET_VALUE_2, STORE_TOKEN, NEW_PASS]);
    assert_silent(out, &all_fakes());
}

#[cfg(unix)]
#[track_caller]
fn assert_private(p: &Path) {
    use std::os::unix::fs::PermissionsExt;
    assert_eq!(fs::metadata(p).unwrap().permissions().mode() & 0o777, 0o600);
}

#[cfg(not(unix))]
fn assert_private(_: &Path) {}

#[test]
fn secrets_set_adds_and_replaces_a_value_in_a_passphrase_bundle() {
    let sb = Sandbox::empty();
    let pass = source_setup(&sb);
    capture_with_bundle(&sb, &pass);
    let before = open_bundle(&sb).names();
    write(&sb, "vals/new.txt", &format!("{SET_VALUE}\n"));

    // Add a name, from a file.
    let out = sb
        .cli()
        .args(["secrets", "set", "NEW_API_KEY"])
        .arg(sb.path("prof"))
        .arg("--value-file")
        .arg(sb.path("vals/new.txt"))
        .arg("--secrets-passphrase-file")
        .arg(&pass)
        .assert()
        .success()
        .stdout(contains("Added NEW_API_KEY"))
        .stdout(contains("4 variable(s)"))
        .get_output()
        .clone();
    assert_no_leak(&sb, &out);
    assert_eq!(
        names_in(&sb.path("prof")),
        [".gitignore", "kitbag.toml", "secrets.age"]
    );
    assert_private(&sb.path("prof/secrets.age"));
    let raw = fs::read(sb.path("prof/secrets.age")).unwrap();
    assert!(bundle::looks_like_age(&raw) && bundle::needs_passphrase(&raw));
    let b = open_bundle(&sb);
    assert_eq!(b.len(), 4);
    assert_eq!(b.get("NEW_API_KEY").unwrap().expose(), SET_VALUE);
    assert_eq!(
        b.get("GITHUB_PERSONAL_ACCESS_TOKEN").unwrap().expose(),
        TOKEN
    );
    assert_eq!(
        b.get("LINEAR_CLIENT_SECRET").unwrap().expose(),
        CLIENT_SECRET
    );

    // Replace a captured one, from stdin: its capture location stays.
    let out = sb
        .cli()
        .args(["secrets", "set", "GITHUB_PERSONAL_ACCESS_TOKEN", "prof"])
        .args(["--value-file", "-", "--secrets-passphrase-file"])
        .arg(&pass)
        .write_stdin(format!("{SET_VALUE_2}\n"))
        .assert()
        .success()
        .stdout(contains("Replaced GITHUB_PERSONAL_ACCESS_TOKEN"))
        .get_output()
        .clone();
    assert_no_leak(&sb, &out);
    let b = open_bundle(&sb);
    assert_eq!(b.len(), 4);
    assert_eq!(
        b.get("GITHUB_PERSONAL_ACCESS_TOKEN").unwrap().expose(),
        SET_VALUE_2
    );
    assert_eq!(b.get("NEW_API_KEY").unwrap().expose(), SET_VALUE);
    let after = b.names();
    for (name, at) in &before {
        assert!(after.contains(&(name.clone(), at.clone())), "{name} moved");
    }
    assert!(after.contains(&("NEW_API_KEY".to_string(), "secrets set".to_string())));

    // list still shows names only.
    let out = sb
        .cli()
        .args(["secrets", "list", "prof", "--secrets-passphrase-file"])
        .arg(&pass)
        .assert()
        .success()
        .stdout(contains("NEW_API_KEY"))
        .stdout(contains("4 variable(s)"))
        .get_output()
        .clone();
    assert_no_leak(&sb, &out);
}

#[test]
fn secrets_set_creates_a_key_encrypted_bundle_and_rewrites_it_with_keys() {
    let sb = Sandbox::empty();
    write(&sb, "prof/kitbag.toml", "kitbag = 1\nname = \"p\"\n");
    write(&sb, "vals/one.txt", SET_VALUE);
    write(&sb, "vals/two.txt", &format!("{SET_VALUE_2}\r\n"));
    let id = age::x25519::Identity::generate();
    write(
        &sb,
        "id.txt",
        &format!("{}\n", id.to_string().expose_secret()),
    );
    let key = id.to_public().to_string();

    // A missing bundle is created, private, and kept out of git.
    let out = sb
        .cli()
        .args([
            "secrets",
            "set",
            "FIRST",
            "prof/secrets.age",
            "--value-file",
        ])
        .arg(sb.path("vals/one.txt"))
        .args(["--secrets-recipient", &key])
        .assert()
        .success()
        .stdout(contains("Added FIRST"))
        .get_output()
        .clone();
    assert_no_leak(&sb, &out);
    assert_eq!(
        names_in(&sb.path("prof")),
        [".gitignore", "kitbag.toml", "secrets.age"]
    );
    assert!(bundle::gitignore_covers(&sb.read("prof/.gitignore")));
    assert_private(&sb.path("prof/secrets.age"));
    let raw = fs::read(sb.path("prof/secrets.age")).unwrap();
    assert!(bundle::looks_like_age(&raw) && !bundle::needs_passphrase(&raw));

    // Rewriting a key-encrypted bundle needs the keys to encrypt it to.
    for extra in [
        &["--secrets-identity", "id.txt"][..],
        &[
            "--secrets-identity",
            "id.txt",
            "--secrets-passphrase-file",
            "pass.txt",
        ][..],
    ] {
        write(&sb, "pass.txt", &format!("{PASS}\n"));
        let out = sb
            .cli()
            .args(["secrets", "set", "SECOND", "prof", "--value-file"])
            .arg(sb.path("vals/two.txt"))
            .args(extra)
            .assert()
            .failure()
            .get_output()
            .clone();
        assert_no_leak(&sb, &out);
        assert_eq!(fs::read(sb.path("prof/secrets.age")).unwrap(), raw);
    }

    // With identity and recipient, the first name stays.
    let out = sb
        .cli()
        .args(["secrets", "set", "SECOND", "prof", "--value-file"])
        .arg(sb.path("vals/two.txt"))
        .args(["--secrets-identity", "id.txt", "--secrets-recipient", &key])
        .assert()
        .success()
        .get_output()
        .clone();
    assert_no_leak(&sb, &out);
    let b = bundle::open_file(&sb.path("prof/secrets.age"), &unlock_key(&id)).unwrap();
    assert_eq!(b.len(), 2);
    assert_eq!(b.get("FIRST").unwrap().expose(), SET_VALUE);
    // One trailing newline (here CRLF) is dropped, nothing else.
    assert_eq!(b.get("SECOND").unwrap().expose(), SET_VALUE_2);
    assert_eq!(
        names_in(&sb.path("prof")),
        [".gitignore", "kitbag.toml", "secrets.age"]
    );
}

#[test]
fn secrets_set_refuses_bad_input_and_writes_nothing() {
    let sb = Sandbox::empty();
    let pass = source_setup(&sb);
    capture_with_bundle(&sb, &pass);
    write(&sb, "vals/new.txt", SET_VALUE);
    write(&sb, "vals/empty.txt", "\n");
    let raw = fs::read(sb.path("prof/secrets.age")).unwrap();
    let key = age::x25519::Identity::generate().to_public().to_string();
    let v = sb.path("vals/new.txt");
    let v = v.to_str().unwrap();
    let p = pass.to_str().unwrap();
    let cases: Vec<(Vec<&str>, &str)> = vec![
        // Not a variable name.
        (
            vec![
                "1BAD",
                "prof",
                "--value-file",
                v,
                "--secrets-passphrase-file",
                p,
            ],
            "usable variable name",
        ),
        // The value never comes from the command line.
        (
            vec!["NEW", "prof", "--secrets-passphrase-file", p],
            "--value-file",
        ),
        // A passphrase and keys together.
        (
            vec![
                "NEW",
                "prof",
                "--value-file",
                v,
                "--secrets-passphrase-file",
                p,
                "--secrets-recipient",
                &key,
            ],
            "can't be combined",
        ),
        // Keys for a passphrase bundle: that is rotate's job.
        (
            vec![
                "NEW",
                "prof",
                "--value-file",
                v,
                "--secrets-recipient",
                &key,
            ],
            "secrets rotate",
        ),
        // An empty value.
        (
            vec![
                "NEW",
                "prof",
                "--value-file",
                "vals/empty.txt",
                "--secrets-passphrase-file",
                p,
            ],
            "empty",
        ),
        // No passphrase and no terminal.
        (
            vec!["NEW", "prof", "--value-file", v],
            "--secrets-passphrase-file",
        ),
        // A wrong passphrase.
        (
            vec![
                "NEW",
                "prof",
                "--value-file",
                v,
                "--secrets-passphrase-file",
                "vals/new.txt",
            ],
            "decrypt",
        ),
        // Git-looking places are never written.
        (
            vec![
                "NEW",
                "https://example.invalid/profile.git",
                "--value-file",
                v,
                "--secrets-passphrase-file",
                p,
            ],
            "git source",
        ),
        (
            vec![
                "NEW",
                "git@example.invalid:x/y.git",
                "--value-file",
                v,
                "--secrets-passphrase-file",
                p,
            ],
            "git source",
        ),
        (
            vec![
                "NEW",
                "github:someone/profile",
                "--value-file",
                v,
                "--secrets-passphrase-file",
                p,
            ],
            "git source",
        ),
    ];
    for (args, says) in cases {
        let out = sb
            .cli()
            .args(["secrets", "set"])
            .args(&args)
            .assert()
            .failure()
            .stderr(contains(says))
            .get_output()
            .clone();
        assert_no_leak(&sb, &out);
        assert_eq!(
            fs::read(sb.path("prof/secrets.age")).unwrap(),
            raw,
            "{args:?}"
        );
        assert_eq!(
            names_in(&sb.path("prof")),
            [".gitignore", "kitbag.toml", "secrets.age"]
        );
    }
    // A new bundle: an identity opens nothing, so it is refused.
    let out = sb
        .cli()
        .args(["secrets", "set", "NEW", "fresh.age", "--value-file", v])
        .args(["--secrets-identity", "vals/new.txt"])
        .assert()
        .failure()
        .get_output()
        .clone();
    assert_silent(&out, &all_fakes());
    assert!(!sb.path("fresh.age").exists());
    assert!(!sb.state().exists() || bytes_under(&sb.state()).is_empty());
}

/// A passphrase bundle holding two variables and a token store, written with
/// the library as capture would.
fn bundle_with_store(sb: &Sandbox) -> PathBuf {
    let mut b = bundle::Bundle::default();
    b.insert(
        "ALPHA",
        bundle::Secret::new(TOKEN),
        "mcp_servers.a.env.ALPHA",
    )
    .unwrap();
    b.insert("BETA", bundle::Secret::new(BEARER), "mcp_servers.b.headers")
        .unwrap();
    b.add_token_store(bundle::TokenStore {
        tool: "gemini-cli".to_string(),
        kind: kitbag::oauth_tokens::GEMINI_KIND.to_string(),
        servers: [("linear".to_string(), URL.to_string())].into(),
        content: bundle::Secret::new(&format!("{{\"access_token\":\"{STORE_TOKEN}\"}}")),
    });
    let sealed = bundle::seal(
        &b,
        &bundle::Seal::Passphrase(age::secrecy::SecretString::from(PASS.to_string())),
    )
    .unwrap();
    write(sb, "prof/kitbag.toml", "kitbag = 1\nname = \"p\"\n");
    write(sb, "pass.txt", &format!("{PASS}\n"));
    write(sb, "new-pass.txt", &format!("{NEW_PASS}\n"));
    bundle::write(&sb.path("prof"), &sealed).unwrap()
}

#[track_caller]
fn assert_same_contents(b: &bundle::Bundle) {
    assert_eq!(b.len(), 2);
    assert_eq!(b.get("ALPHA").unwrap().expose(), TOKEN);
    assert_eq!(b.get("BETA").unwrap().expose(), BEARER);
    assert_eq!(
        b.names()[0],
        ("ALPHA".to_string(), "mcp_servers.a.env.ALPHA".to_string())
    );
    let stores = b.token_stores();
    assert_eq!(stores.len(), 1);
    assert_eq!(stores[0].tool, "gemini-cli");
    assert!(stores[0].content.expose().contains(STORE_TOKEN));
    assert_eq!(
        stores[0].servers.get("linear").map(String::as_str),
        Some(URL)
    );
}

#[test]
fn secrets_set_keeps_token_stores() {
    let sb = Sandbox::empty();
    let file = bundle_with_store(&sb);
    write(&sb, "vals/new.txt", SET_VALUE);
    let out = sb
        .cli()
        .args([
            "secrets",
            "set",
            "GAMMA",
            "prof",
            "--value-file",
            "vals/new.txt",
        ])
        .args(["--secrets-passphrase-file", "pass.txt"])
        .assert()
        .success()
        .get_output()
        .clone();
    assert_no_leak(&sb, &out);
    let b = bundle::open_file(&file, &unlock_pass(PASS)).unwrap();
    assert_eq!(b.get("GAMMA").unwrap().expose(), SET_VALUE);
    assert_eq!(b.token_stores().len(), 1);
    assert!(b.token_stores()[0].content.expose().contains(STORE_TOKEN));
}

#[test]
fn secrets_rotate_changes_the_passphrase_and_keeps_everything_else() {
    let sb = Sandbox::empty();
    let file = bundle_with_store(&sb);
    let old = fs::read(&file).unwrap();
    let out = sb
        .cli()
        .args(["secrets", "rotate", "prof"])
        .args(["--secrets-passphrase-file", "pass.txt"])
        .args(["--new-secrets-passphrase-file", "new-pass.txt"])
        .assert()
        .success()
        .stdout(contains("2 variable(s) and 1 token store(s)"))
        .get_output()
        .clone();
    assert_no_leak(&sb, &out);
    // Only the new ciphertext: no plaintext sidecar, backup or temp file.
    assert_eq!(names_in(&sb.path("prof")), ["kitbag.toml", "secrets.age"]);
    assert_private(&file);
    let raw = fs::read(&file).unwrap();
    assert_ne!(raw, old);
    assert!(bundle::looks_like_age(&raw) && bundle::needs_passphrase(&raw));
    assert!(bundle::open(&raw, &unlock_pass(PASS)).is_err());
    assert_same_contents(&bundle::open(&raw, &unlock_pass(NEW_PASS)).unwrap());

    // list opens it with the new passphrase only, and shows names only.
    sb.cli()
        .args([
            "secrets",
            "list",
            "prof",
            "--secrets-passphrase-file",
            "pass.txt",
        ])
        .assert()
        .failure();
    let out = sb
        .cli()
        .args(["secrets", "list", "prof"])
        .args(["--secrets-passphrase-file", "new-pass.txt"])
        .assert()
        .success()
        .stdout(contains("ALPHA"))
        .stdout(contains("BETA"))
        .stdout(contains("gemini-cli MCP OAuth tokens"))
        .get_output()
        .clone();
    assert_no_leak(&sb, &out);
}

#[test]
fn secrets_rotate_moves_between_passphrase_and_keys() {
    let sb = Sandbox::empty();
    let file = bundle_with_store(&sb);
    let ids: Vec<age::x25519::Identity> =
        (0..2).map(|_| age::x25519::Identity::generate()).collect();
    for (i, id) in ids.iter().enumerate() {
        write(
            &sb,
            &format!("id{i}.txt"),
            &format!("{}\n", id.to_string().expose_secret()),
        );
    }
    let key = |i: usize| ids[i].to_public().to_string();

    // Passphrase -> key 0.
    let out = sb
        .cli()
        .args(["secrets", "rotate", "prof/secrets.age"])
        .args(["--secrets-passphrase-file", "pass.txt"])
        .args(["--new-secrets-recipient", &key(0)])
        .assert()
        .success()
        .get_output()
        .clone();
    assert_no_leak(&sb, &out);
    let raw = fs::read(&file).unwrap();
    assert!(!bundle::needs_passphrase(&raw));
    assert_same_contents(&bundle::open(&raw, &unlock_key(&ids[0])).unwrap());

    // Key 0 -> key 1: the old key no longer opens it.
    let out = sb
        .cli()
        .args(["secrets", "rotate", "prof", "--secrets-identity", "id0.txt"])
        .args(["--new-secrets-recipient", &key(1)])
        .assert()
        .success()
        .get_output()
        .clone();
    assert_no_leak(&sb, &out);
    let raw = fs::read(&file).unwrap();
    assert!(bundle::open(&raw, &unlock_key(&ids[0])).is_err());
    assert_same_contents(&bundle::open(&raw, &unlock_key(&ids[1])).unwrap());

    // Key 1 -> the new passphrase.
    let out = sb
        .cli()
        .args(["secrets", "rotate", "prof", "--secrets-identity", "id1.txt"])
        .args(["--new-secrets-passphrase-file", "new-pass.txt"])
        .assert()
        .success()
        .get_output()
        .clone();
    assert_no_leak(&sb, &out);
    assert_same_contents(&bundle::open_file(&file, &unlock_pass(NEW_PASS)).unwrap());
    assert_eq!(names_in(&sb.path("prof")), ["kitbag.toml", "secrets.age"]);
}

#[test]
fn a_refused_or_failed_rotate_leaves_the_bundle_as_it_was() {
    let sb = Sandbox::empty();
    let file = bundle_with_store(&sb);
    let raw = fs::read(&file).unwrap();
    write(&sb, "wrong.txt", "fake-wrong-passphrase-xyz\n");
    let key = age::x25519::Identity::generate().to_public().to_string();
    let cases: Vec<(Vec<&str>, &str)> = vec![
        // No new encryption: rotate never keeps the old one.
        (
            vec!["prof", "--secrets-passphrase-file", "pass.txt"],
            "--new-secrets",
        ),
        // A new passphrase and new keys together.
        (
            vec![
                "prof",
                "--secrets-passphrase-file",
                "pass.txt",
                "--new-secrets-passphrase-file",
                "new-pass.txt",
                "--new-secrets-recipient",
                &key,
            ],
            "can't be combined",
        ),
        // The same passphrase again.
        (
            vec![
                "prof",
                "--secrets-passphrase-file",
                "pass.txt",
                "--new-secrets-passphrase-file",
                "pass.txt",
            ],
            "same as the current one",
        ),
        // The wrong current passphrase.
        (
            vec![
                "prof",
                "--secrets-passphrase-file",
                "wrong.txt",
                "--new-secrets-passphrase-file",
                "new-pass.txt",
            ],
            "decrypt",
        ),
        // No current passphrase and no terminal.
        (
            vec!["prof", "--new-secrets-passphrase-file", "new-pass.txt"],
            "--secrets-passphrase-file",
        ),
        // Not a key.
        (
            vec![
                "prof",
                "--secrets-passphrase-file",
                "pass.txt",
                "--new-secrets-recipient",
                "not-a-key",
            ],
            "--secrets-recipient",
        ),
        // Git-looking places.
        (
            vec![
                "https://example.invalid/profile.git",
                "--secrets-passphrase-file",
                "pass.txt",
                "--new-secrets-passphrase-file",
                "new-pass.txt",
            ],
            "git source",
        ),
        (
            vec![
                "git@example.invalid:x/y.git",
                "--secrets-passphrase-file",
                "pass.txt",
                "--new-secrets-passphrase-file",
                "new-pass.txt",
            ],
            "git source",
        ),
        (
            vec![
                "gh:someone/profile",
                "--secrets-passphrase-file",
                "pass.txt",
                "--new-secrets-passphrase-file",
                "new-pass.txt",
            ],
            "git source",
        ),
        // No bundle there.
        (
            vec![
                "missing.age",
                "--secrets-passphrase-file",
                "pass.txt",
                "--new-secrets-passphrase-file",
                "new-pass.txt",
            ],
            "missing.age",
        ),
    ];
    for (args, says) in cases {
        let out = sb
            .cli()
            .args(["secrets", "rotate"])
            .args(&args)
            .assert()
            .failure()
            .stderr(contains(says))
            .get_output()
            .clone();
        let mut hidden = all_fakes();
        hidden.push("fake-wrong-passphrase-xyz");
        assert_silent(&out, &hidden);
        assert_absent_under(&sb.path("prof"), &hidden);
        assert_eq!(fs::read(&file).unwrap(), raw, "{args:?}");
        assert_eq!(names_in(&sb.path("prof")), ["kitbag.toml", "secrets.age"]);
    }
    assert!(!sb.path("missing.age").exists());
    assert!(!sb.state().exists() || bytes_under(&sb.state()).is_empty());
    assert_same_contents(&bundle::open(&raw, &unlock_pass(PASS)).unwrap());
}

// ---------------------------------------------------------------------------
// #137: bundle values resolved into the tools that can't read env vars.

const RESOLVE_VALUE: &str = "fake-bundle-resolved-value-3333";
const RESOLVE_VALUE_2: &str = "fake-bundle-rotated-value-4444";
const ENV_SHADOW: &str = "fake-env-shadowed-value-5555";
const ENV_ONLY: &str = "fake-env-only-value-6666";

/// Every fake that must never reach the plan, the output, the state
/// directory or the profile directory as plaintext.
fn resolve_fakes() -> Vec<&'static str> {
    vec![RESOLVE_VALUE, RESOLVE_VALUE_2, PASS]
}

/// A hand-written profile whose local server needs `GITHUB_TOKEN` (in the
/// bundle, and also set in the environment to another value),
/// `ONLY_IN_ENV` (environment only) and `NOT_ANYWHERE` (neither), with a
/// bundle made by `secrets set`. Returns the passphrase file.
fn resolve_setup(sb: &Sandbox, args: &str) -> PathBuf {
    write_resolve_profile(sb, args);
    write(sb, "pass.txt", &format!("{PASS}\n"));
    write(sb, "value.txt", &format!("{RESOLVE_VALUE}\n"));
    sb.cli()
        .args(["secrets", "set", "GITHUB_TOKEN"])
        .arg(sb.path("prof"))
        .arg("--value-file")
        .arg(sb.path("value.txt"))
        .arg("--secrets-passphrase-file")
        .arg(sb.path("pass.txt"))
        .assert()
        .success();
    fs::remove_file(sb.path("value.txt")).unwrap();
    sb.path("pass.txt")
}

fn write_resolve_profile(sb: &Sandbox, args: &str) {
    write(
        sb,
        "prof/kitbag.toml",
        &format!(
            "kitbag = 1\nname = \"res\"\n\n[mcp_servers.github]\ncommand = \"npx\"\nargs = [{args}]\nenv = {{ GITHUB_TOKEN = \"${{GITHUB_TOKEN}}\", OTHER = \"${{ONLY_IN_ENV}}\", MISSING = \"${{NOT_ANYWHERE}}\" }}\n"
        ),
    );
}

/// `kitbag apply` of the sandbox's `prof` with the environment variables of
/// [`resolve_setup`] set.
fn apply_res(sb: &Sandbox, to: &str, pass: Option<&Path>, extra: &[&str]) -> std::process::Output {
    let mut c = sb.cli();
    c.env("GITHUB_TOKEN", ENV_SHADOW)
        .env("ONLY_IN_ENV", ENV_ONLY)
        .arg("apply")
        .arg(sb.path("prof"))
        .args(["--to", to]);
    if let Some(p) = pass {
        c.arg("--secrets-from")
            .arg(sb.path("prof/secrets.age"))
            .arg("--secrets-passphrase-file")
            .arg(p);
    }
    c.args(extra).assert().success().get_output().clone()
}

/// The files under the home that hold `needle`, by file name.
fn files_holding(sb: &Sandbox, needle: &str) -> Vec<String> {
    let mut v: Vec<String> = bytes_under(&sb.home())
        .into_iter()
        .filter(|(_, b)| has(b, needle))
        .map(|(p, _)| p.file_name().unwrap().to_string_lossy().into_owned())
        .collect();
    v.sort();
    v
}

/// The Claude Desktop config under the sandbox's home, wherever this OS keeps it.
fn desktop_config(sb: &Sandbox) -> PathBuf {
    bytes_under(&sb.home())
        .into_iter()
        .map(|(p, _)| p)
        .find(|p| p.ends_with("claude_desktop_config.json"))
        .expect("Claude Desktop config written")
}

#[track_caller]
fn assert_nothing_leaked(sb: &Sandbox, outs: &[&std::process::Output]) {
    for out in outs {
        assert_silent(out, &resolve_fakes());
    }
    assert_absent_under(&sb.state(), &resolve_fakes());
    // The profile directory, `secrets.age` included: only its ciphertext.
    assert_absent_under(&sb.path("prof"), &resolve_fakes());
    assert!(bundle::looks_like_age(
        &fs::read(sb.path("prof/secrets.age")).unwrap()
    ));
}

#[test]
fn bundle_values_go_only_into_the_tools_that_cant_read_env_vars() {
    let sb = Sandbox::empty();
    let pass = resolve_setup(&sb, "\"-y\", \"server-github\"");
    let targets = "claude-desktop,zed,claude-code,cursor,gemini-cli,codex";

    // The plan and its diff name the variables and mask the value.
    let dry = apply_res(&sb, targets, Some(&pass), &["--dry-run"]);
    let stdout = String::from_utf8_lossy(&dry.stdout).into_owned();
    assert!(
        stdout.contains("resolved from the secrets bundle"),
        "{stdout}"
    );
    assert!(
        stdout.contains("••••"),
        "the diff masks the value: {stdout}"
    );
    assert!(stdout.contains("NOT_ANYWHERE"), "{stdout}");
    assert!(
        !sb.home().join(".claude.json").exists(),
        "dry run wrote nothing"
    );

    let out = apply_res(&sb, targets, Some(&pass), &["--yes"]);
    assert_nothing_leaked(&sb, &[&dry, &out]);
    assert!(!has(&out.stdout, ENV_SHADOW) && !has(&out.stdout, ENV_ONLY));

    // Only the two tools without interpolation hold the bundle's value: it
    // wins over the environment's, which goes nowhere.
    assert_eq!(
        files_holding(&sb, RESOLVE_VALUE),
        vec!["claude_desktop_config.json", "settings.json"]
    );
    assert!(files_holding(&sb, ENV_SHADOW).is_empty());
    // A variable the bundle lacks still comes from the environment there
    // (and in Codex, whose `env` can't forward a differently named one),
    // and one set nowhere stays a placeholder.
    assert_eq!(
        files_holding(&sb, ENV_ONLY),
        vec!["claude_desktop_config.json", "config.toml", "settings.json"]
    );
    let desktop = fs::read_to_string(desktop_config(&sb)).unwrap();
    assert!(desktop.contains("${NOT_ANYWHERE}"), "{desktop}");
    assert_private(&desktop_config(&sb));
    let zed = bytes_under(&sb.home())
        .into_iter()
        .find(|(p, b)| p.ends_with("settings.json") && has(b, RESOLVE_VALUE))
        .unwrap()
        .0;
    assert_private(&zed);

    // Tools that read env references keep getting them.
    let claude = sb.read("home/.claude.json");
    assert!(claude.contains("${GITHUB_TOKEN}"), "{claude}");
    let cursor = sb.read("home/.cursor/mcp.json");
    assert!(cursor.contains("${env:GITHUB_TOKEN}"), "{cursor}");
    let gemini = sb.read("home/.gemini/settings.json");
    assert!(gemini.contains("${GITHUB_TOKEN}"), "{gemini}");
    let codex = sb.read("home/.codex/config.toml");
    assert!(codex.contains("GITHUB_TOKEN"), "{codex}");

    // `kitbag diff` never opens the bundle: nothing to show, and no value.
    let diff = sb
        .cli()
        .args(["diff"])
        .arg(sb.path("prof"))
        .args(["--to", targets])
        .assert()
        .get_output()
        .clone();
    assert_nothing_leaked(&sb, &[&diff]);
}

#[test]
fn secrets_never_with_a_bundle_writes_no_value() {
    let sb = Sandbox::empty();
    let pass = resolve_setup(&sb, "\"-y\", \"server-github\"");
    let out = apply_res(
        &sb,
        "claude-desktop,zed",
        Some(&pass),
        &["--yes", "--secrets", "never"],
    );
    assert_nothing_leaked(&sb, &[&out]);
    assert!(files_holding(&sb, RESOLVE_VALUE).is_empty());
    assert!(files_holding(&sb, ENV_ONLY).is_empty());
    let desktop = fs::read_to_string(desktop_config(&sb)).unwrap();
    assert!(desktop.contains("${GITHUB_TOKEN}"), "{desktop}");
}

#[test]
fn a_file_holding_a_bundle_value_is_never_backed_up() {
    let sb = Sandbox::empty();
    let pass = resolve_setup(&sb, "\"-y\", \"server-github\"");
    let to = "claude-desktop";

    // 0. Without the bundle, with placeholders: nothing secret yet.
    apply_res(&sb, to, None, &["--yes", "--secrets", "never"]);
    let clean = fs::read(desktop_config(&sb)).unwrap();
    assert!(!has(&clean, RESOLVE_VALUE));

    // 1. With the bundle: the file held no value, so its backup (those old
    // bytes) is kept and undo puts it back.
    let first = apply_res(&sb, to, Some(&pass), &["--yes"]);
    assert!(has(
        &first.stdout,
        "its backup keeps only what it holds now"
    ));
    assert!(has(&fs::read(desktop_config(&sb)).unwrap(), RESOLVE_VALUE));
    assert_nothing_leaked(&sb, &[&first]);
    let undo = sb
        .cli()
        .args(["undo"])
        .assert()
        .success()
        .get_output()
        .clone();
    assert_nothing_leaked(&sb, &[&undo]);
    assert_eq!(fs::read(desktop_config(&sb)).unwrap(), clean);
    assert!(
        !sb.state().join("bundle-files.json").exists(),
        "undo forgot the file"
    );

    // 2. Apply again, then change the profile: the file now holds the value,
    // so the next write keeps it out of the undo record.
    apply_res(&sb, to, Some(&pass), &["--yes"]);
    write_resolve_profile(&sb, "\"-y\", \"server-github\", \"--changed\"");
    let second = apply_res(&sb, to, Some(&pass), &["--yes"]);
    let stdout = String::from_utf8_lossy(&second.stdout).into_owned();
    assert!(stdout.contains("left out of the undo record"), "{stdout}");
    assert!(stdout.contains("not in the undo record"), "{stdout}");
    assert_nothing_leaked(&sb, &[&second]);
    let held = fs::read(desktop_config(&sb)).unwrap();
    assert!(has(&held, "--changed") && has(&held, RESOLVE_VALUE));

    // 3. Without the bundle too (kitbag remembers the path, not the value),
    // and with another value in the bundle after `secrets set`.
    write_resolve_profile(&sb, "\"-y\", \"server-github\", \"--third\"");
    let third = apply_res(&sb, to, None, &["--yes", "--secrets", "never"]);
    assert!(has(&third.stdout, "left out of the undo record"));
    assert_nothing_leaked(&sb, &[&third]);
    write(&sb, "value.txt", &format!("{RESOLVE_VALUE_2}\n"));
    sb.cli()
        .args(["secrets", "set", "GITHUB_TOKEN"])
        .arg(sb.path("prof"))
        .arg("--value-file")
        .arg(sb.path("value.txt"))
        .arg("--secrets-passphrase-file")
        .arg(&pass)
        .assert()
        .success();
    fs::remove_file(sb.path("value.txt")).unwrap();
    let fourth = apply_res(&sb, to, Some(&pass), &["--yes"]);
    assert_nothing_leaked(&sb, &[&fourth]);
    let now = fs::read(desktop_config(&sb)).unwrap();
    assert!(has(&now, RESOLVE_VALUE_2));

    // Undo leaves a file it holds no backup of as it is.
    let undo = sb
        .cli()
        .args(["undo"])
        .assert()
        .success()
        .get_output()
        .clone();
    assert_nothing_leaked(&sb, &[&undo]);
    assert_eq!(fs::read(desktop_config(&sb)).unwrap(), now);
    assert!(sb.state().join("bundle-files.json").exists());
}
