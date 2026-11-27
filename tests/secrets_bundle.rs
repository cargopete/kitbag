//! The opt-in encrypted secrets bundle (#9, first slice): `capture --include
//! secrets` writes the redacted values only to `secrets.age`; `apply
//! --secrets-from` and `secrets list` open it in memory and show names only.
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
fn apply_with_secrets_from_checks_the_bundle_and_writes_no_value_anywhere() {
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
            .stdout(contains("no value was written"))
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
