//! `kitbag secrets commit` (#138): the opt-in way to keep the encrypted
//! `secrets.age` in git. Every repository here is a temporary `git init`;
//! every secret and passphrase is a made-up one.

mod common;

use std::fs;
use std::path::Path;

use age::secrecy::ExposeSecret;
use common::*;
use predicates::str::contains;
use serde_json::json;

const TOKEN: &str = "ghp_FAKEfakeFAKE0123456789abcdefghijkl";
const PASS: &str = "fake-test-passphrase-not-real";

fn write(sb: &Sandbox, rel: &str, text: &str) {
    let p = sb.path(rel);
    fs::create_dir_all(p.parent().unwrap()).unwrap();
    fs::write(p, text).unwrap();
}

/// Capture a setup with one fake secret into `prof`, with `extra` flags.
fn capture_bundle(sb: &Sandbox, extra: &[&str]) {
    write(
        sb,
        "home/.gemini/settings.json",
        &json!({"mcpServers": {"github": {
            "command": "npx",
            "args": ["-y", "@modelcontextprotocol/server-github"],
            "env": {"GITHUB_PERSONAL_ACCESS_TOKEN": TOKEN}
        }}})
        .to_string(),
    );
    write(sb, "pass.txt", &format!("{PASS}\n"));
    sb.cli()
        .args(["capture", "--from", "gemini-cli", "-o"])
        .arg(sb.path("prof"))
        .args(["--include", "secrets"])
        .args(extra)
        .assert()
        .success();
}

fn key_bundle(sb: &Sandbox) {
    let id = age::x25519::Identity::generate();
    write(
        sb,
        "id.txt",
        &format!("{}\n", id.to_string().expose_secret()),
    );
    capture_bundle(sb, &["--secrets-recipient", &id.to_public().to_string()]);
}

fn pass_bundle(sb: &Sandbox) {
    let pass = sb.path("pass.txt");
    capture_bundle(sb, &["--secrets-passphrase-file", pass.to_str().unwrap()]);
}

fn init_repo(dir: &Path) {
    git(dir, &["init", "-q"]);
}

fn staged(dir: &Path) -> Vec<String> {
    git_out(dir, &["diff", "--cached", "--name-only"])
        .lines()
        .map(str::to_string)
        .collect()
}

fn commit_cmd(sb: &Sandbox) -> assert_cmd::Command {
    let mut c = sb.cli();
    c.args(["secrets", "commit"]).arg(sb.path("prof"));
    c
}

#[test]
fn capture_still_gitignores_the_bundle() {
    let sb = Sandbox::empty();
    key_bundle(&sb);
    let ignore = sb.read("prof/.gitignore");
    assert!(ignore.lines().any(|l| l == "secrets.age"), "{ignore}");
}

#[test]
fn a_key_bundle_is_unignored_and_staged_but_not_committed() {
    if !has_git() {
        return;
    }
    let sb = Sandbox::empty();
    key_bundle(&sb);
    let prof = sb.path("prof");
    fs::write(
        prof.join(".gitignore"),
        "target\n# kitbag: the encrypted secrets bundle stays out of git\nsecrets.age\n*.log\n",
    )
    .unwrap();
    // A passphrase file and a plaintext-looking neighbour that must not be staged.
    fs::write(prof.join("identity.txt"), "AGE-SECRET-KEY-FAKE\n").unwrap();
    init_repo(&prof);
    commit_cmd(&sb)
        .assert()
        .success()
        .stdout(contains("git commit"));
    assert_eq!(
        sb.read("prof/.gitignore"),
        "target\n*.log\n",
        "only kitbag's rule goes"
    );
    let mut got = staged(&prof);
    got.sort();
    assert_eq!(got, [".gitignore", "secrets.age"]);
    // Nothing was committed.
    assert!(
        !std::process::Command::new("git")
            .args(["rev-parse", "--verify", "-q", "HEAD"])
            .current_dir(&prof)
            .status()
            .unwrap()
            .success()
    );
    // Idempotent once committed.
    git(&prof, &["commit", "-q", "-m", "bundle"]);
    commit_cmd(&sb)
        .assert()
        .success()
        .stdout(contains("already tracked"));
    assert!(staged(&prof).is_empty());
}

#[test]
fn a_passphrase_bundle_is_refused_without_the_flag_and_warns_with_it() {
    if !has_git() {
        return;
    }
    let sb = Sandbox::empty();
    pass_bundle(&sb);
    let prof = sb.path("prof");
    init_repo(&prof);
    let before = sb.read("prof/.gitignore");
    // There is no --yes here, and none would count.
    commit_cmd(&sb)
        .assert()
        .failure()
        .stderr(contains("WARNING").and(contains("--allow-passphrase-commit")));
    assert_eq!(sb.read("prof/.gitignore"), before);
    assert!(staged(&prof).is_empty());

    commit_cmd(&sb)
        .arg("--allow-passphrase-commit")
        .assert()
        .success()
        .stderr(contains("WARNING").and(contains("offline")));
    assert!(staged(&prof).contains(&"secrets.age".to_string()));
}

#[test]
fn plaintext_and_foreign_files_are_never_staged_or_accepted() {
    if !has_git() {
        return;
    }
    let sb = Sandbox::empty();
    key_bundle(&sb);
    let prof = sb.path("prof");
    init_repo(&prof);
    fs::write(
        prof.join("identity.txt"),
        "AGE-SECRET-KEY-FAKE
",
    )
    .unwrap();
    fs::write(
        prof.join("pass.txt"),
        format!(
            "{PASS}
"
        ),
    )
    .unwrap();
    // A plaintext sidecar blocks the command and changes nothing.
    fs::write(
        prof.join(".env"),
        "TOKEN=fake
",
    )
    .unwrap();
    let before = sb.read("prof/.gitignore");
    commit_cmd(&sb)
        .assert()
        .failure()
        .stderr(contains("plaintext"));
    assert_eq!(sb.read("prof/.gitignore"), before);
    assert!(staged(&prof).is_empty());
    fs::remove_file(prof.join(".env")).unwrap();
    // Without it, only secrets.age and .gitignore are staged.
    commit_cmd(&sb).assert().success();
    for f in staged(&prof) {
        assert!(f == "secrets.age" || f == ".gitignore", "staged {f}");
    }
    // A missing file, a non-age file and a git URL are refused.
    fs::write(prof.join("secrets.age"), "{\"A\":\"b\"}").unwrap();
    commit_cmd(&sb)
        .assert()
        .failure()
        .stderr(contains("not an age"));
    fs::remove_file(prof.join("secrets.age")).unwrap();
    commit_cmd(&sb)
        .assert()
        .failure()
        .stderr(contains("does not exist"));
    sb.cli()
        .args(["secrets", "commit", "https://example.com/p.git"])
        .assert()
        .failure()
        .stderr(contains("git source"));
}

#[test]
fn without_a_repository_the_gitignore_is_updated_and_add_is_skipped() {
    let sb = Sandbox::empty();
    key_bundle(&sb);
    commit_cmd(&sb)
        .assert()
        .success()
        .stdout(contains("not in a git repository"));
    assert!(!sb.read("prof/.gitignore").contains("secrets.age"));
}

#[test]
fn a_wider_ignore_pattern_is_left_alone() {
    let sb = Sandbox::empty();
    key_bundle(&sb);
    fs::write(sb.path("prof/.gitignore"), "*.age\n").unwrap();
    commit_cmd(&sb).assert().failure().stderr(contains("*.age"));
    assert_eq!(sb.read("prof/.gitignore"), "*.age\n");
}

fn doctor(sb: &Sandbox) -> (bool, String) {
    let out = sb
        .cli()
        .arg("doctor")
        .arg(sb.path("prof"))
        .assert()
        .get_output()
        .clone();
    (
        out.status.success(),
        String::from_utf8_lossy(&out.stdout).into_owned(),
    )
}

#[test]
fn doctor_warns_only_when_an_untracked_bundle_is_not_ignored() {
    if !has_git() {
        return;
    }
    let sb = Sandbox::empty();
    key_bundle(&sb);
    let prof = sb.path("prof");
    init_repo(&prof);
    // Ignored: no warning.
    let (_, shown) = doctor(&sb);
    assert!(!shown.contains("not in .gitignore"), "{shown}");
    // Not ignored and not tracked: today's warning.
    commit_cmd(&sb).assert().success();
    git(&prof, &["reset", "-q"]);
    let (_, shown) = doctor(&sb);
    assert!(shown.contains("not in .gitignore"), "{shown}");
    // Tracked and encrypted to keys: no nag, no failure.
    git(&prof, &["add", "secrets.age"]);
    let (ok, shown) = doctor(&sb);
    assert!(ok, "{shown}");
    assert!(!shown.contains("not in .gitignore"), "{shown}");
    assert!(shown.contains("tracked in git"), "{shown}");
    assert!(!shown.contains("passphrase"), "{shown}");
}

#[test]
fn doctor_warns_about_a_tracked_passphrase_bundle_and_fails_a_tracked_plain_one() {
    if !has_git() {
        return;
    }
    let sb = Sandbox::empty();
    pass_bundle(&sb);
    let prof = sb.path("prof");
    init_repo(&prof);
    commit_cmd(&sb)
        .arg("--allow-passphrase-commit")
        .assert()
        .success();
    let (ok, shown) = doctor(&sb);
    assert!(ok, "a passphrase warning alone is not a failure: {shown}");
    assert!(
        shown.contains("recipients") || shown.contains("encrypt it to keys"),
        "{shown}"
    );
    // Tracked but not encrypted: still a failure.
    fs::write(prof.join("secrets.age"), "{\"A\":\"b\"}").unwrap();
    git(&prof, &["add", "secrets.age"]);
    let (ok, shown) = doctor(&sb);
    assert!(!ok, "{shown}");
    assert!(shown.contains("not an age-encrypted file"), "{shown}");
}

use predicates::prelude::*;
