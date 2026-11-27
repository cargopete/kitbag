//! Git sources pinned to a commit, tag or branch: what is fetched, what is
//! recorded, and the warning when a source moved since the last apply.

mod common;

use common::*;
use predicates::prelude::*;
use predicates::str::contains;

/// A repository with two commits on `main`, the first tagged `v1`. Returns
/// its URL and the two full SHAs.
fn two_commits(sb: &Sandbox) -> (String, String, String) {
    let url = sb.git_profile(
        "repo",
        &[
            ("kitbag.toml", "kitbag = 1\nname = \"pinned\"\n"),
            ("instructions.md", "First.\n"),
        ],
    );
    let repo = sb.path("repo");
    let first = git_out(&repo, &["rev-parse", "HEAD"]);
    git(&repo, &["tag", "v1"]);
    std::fs::write(repo.join("instructions.md"), "Second.\n").unwrap();
    git(&repo, &["commit", "-q", "-am", "second"]);
    let second = git_out(&repo, &["rev-parse", "HEAD"]);
    (url, first, second)
}

fn claude_md(sb: &Sandbox) -> String {
    sb.read("home/.claude/CLAUDE.md")
}

/// The `git` objects of the transactions recorded, oldest first.
fn recorded(sb: &Sandbox) -> Vec<serde_json::Value> {
    let mut txs: Vec<serde_json::Value> = std::fs::read_dir(sb.state().join("backups"))
        .unwrap()
        .map(|e| {
            let text = std::fs::read_to_string(e.unwrap().path().join("transaction.json")).unwrap();
            serde_json::from_str(&text).unwrap()
        })
        .collect();
    txs.sort_by_key(|t| t["created_ms"].as_u64());
    txs.into_iter().map(|t| t["git"].clone()).collect()
}

#[test]
fn a_source_pinned_to_a_commit_applies_that_commit_and_records_it() {
    if !has_git() {
        return;
    }
    let sb = Sandbox::empty();
    let (url, first, _) = two_commits(&sb);
    for pin in [first.clone(), first[..8].to_string()] {
        sb.cli()
            .args(["diff", &format!("{url}@{pin}"), "--to", "claude-code"])
            .assert()
            .success()
            .stdout(contains(format!("commit {first}")).and(contains("First.")));
    }
    sb.cli()
        .args([
            "apply",
            &format!("{url}@{first}"),
            "--to",
            "claude-code",
            "--yes",
        ])
        .assert()
        .success()
        .stdout(contains(format!("commit {first}")));
    assert!(claude_md(&sb).contains("First."));
    let git = &recorded(&sb)[0];
    assert_eq!(git["commit"], first.as_str(), "full SHA recorded");
    assert!(git["source"].as_str().unwrap().starts_with("file:///"));
    assert!(
        !git["source"].as_str().unwrap().contains('@'),
        "no ref in the source"
    );
    // status shows the commit it was applied at.
    sb.cli()
        .args(["status", &format!("{url}#{first}"), "--to", "claude-code"])
        .assert()
        .success()
        .stdout(
            contains(format!("commit   {first}"))
                .and(contains(format!("applied  {first}")))
                .and(contains("moved").not()),
        );
    sb.cli()
        .arg("status")
        .assert()
        .success()
        .stdout(contains(format!("@ {first}")));
    // Undo still works on a transaction with a recorded commit.
    sb.cli().arg("undo").assert().success();
    assert!(!sb.home().join(".claude/CLAUDE.md").exists());
    sb.cli().arg("history").assert().success();
}

#[test]
fn tags_branches_and_the_default_branch_still_resolve() {
    if !has_git() {
        return;
    }
    let sb = Sandbox::empty();
    let (url, first, second) = two_commits(&sb);
    for (src, sha, text) in [
        (format!("{url}@v1"), &first, "First."),
        (format!("{url}#v1"), &first, "First."),
        (format!("{url}#main"), &second, "Second."),
        (format!("{url}@main"), &second, "Second."),
        (url.clone(), &second, "Second."),
    ] {
        sb.cli()
            .args(["diff", &src, "--to", "claude-code"])
            .assert()
            .success()
            .stdout(contains(format!("commit {sha}")).and(contains(text)));
    }
    sb.cli()
        .args(["diff", &format!("{url}@deadbeef"), "--to", "claude-code"])
        .assert()
        .code(2)
        .stderr(contains("could not clone").and(contains("deadbeef")));
    sb.cli()
        .args(["diff", &format!("{url}@v1#main"), "--to", "claude-code"])
        .assert()
        .code(2)
        .stderr(contains("two refs"));
}

#[test]
fn a_source_that_moved_since_the_last_apply_is_reported() {
    if !has_git() {
        return;
    }
    let sb = Sandbox::empty();
    let (url, first, second) = two_commits(&sb);
    let repo = sb.path("repo");
    // Apply main while it is at the first commit.
    git(&repo, &["checkout", "-q", "-b", "old", &first]);
    git(&repo, &["branch", "-q", "-f", "main", &first]);
    git(&repo, &["checkout", "-q", "main"]);
    let src = format!("{url}@main");
    sb.cli()
        .args(["apply", &src, "--to", "claude-code", "--yes"])
        .assert()
        .success()
        .stdout(contains("moved").not());
    // main moves on.
    git(&repo, &["reset", "-q", "--hard", &second]);
    sb.cli()
        .args(["status", &src, "--to", "claude-code", "--exit-code"])
        .assert()
        .code(1)
        .stdout(
            contains("source         commit")
                .and(contains(format!("applied {first}, now {second}")))
                .and(contains(format!("applied  {first}"))),
        );
    let out = sb
        .cli()
        .args(["status", &src, "--to", "claude-code", "--json"])
        .output()
        .unwrap();
    let rows: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(rows[0]["target"], "source");
    assert_eq!(rows[0]["state"], "profile changed");
    // apply warns with both SHAs, and still applies with --yes.
    sb.cli()
        .args(["apply", &src, "--to", "claude-code", "--yes"])
        .assert()
        .success()
        .stdout(
            contains("moved since it was last applied on this machine")
                .and(contains(format!("applied  {first}")))
                .and(contains(format!("now      {second}"))),
        );
    assert!(claude_md(&sb).contains("Second."));
    let git = recorded(&sb);
    assert_eq!(git[1]["commit"], second.as_str());
    // Now in step again.
    sb.cli()
        .args(["status", &src, "--to", "claude-code", "--exit-code"])
        .assert()
        .success()
        .stdout(contains("moved").not());
}

#[test]
fn require_pin_refuses_unpinned_git_sources() {
    if !has_git() {
        return;
    }
    let sb = Sandbox::empty();
    let (url, first, _) = two_commits(&sb);
    for cmd in ["apply", "diff", "sync", "inspect", "status", "doctor"] {
        let out = sb
            .cli()
            .args([cmd, &url, "--to", "claude-code", "--require-pin"])
            .output()
            .unwrap();
        let all = format!(
            "{}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
        assert!(all.contains("is not pinned"), "{cmd}: {all}");
        assert!(all.contains("@<sha|tag|branch>"), "{cmd}: {all}");
        assert!(!out.status.success() || cmd == "doctor", "{cmd}");
    }
    for pin in [format!("@{first}"), "@v1".into(), "#main".into()] {
        sb.cli()
            .args([
                "diff",
                &format!("{url}{pin}"),
                "--to",
                "claude-code",
                "--require-pin",
            ])
            .assert()
            .success();
    }
    // A local directory needs no pin.
    sb.cli()
        .arg("diff")
        .arg(sb.path("repo"))
        .args(["--to", "claude-code", "--require-pin"])
        .assert()
        .success();
}

/// Transactions written before commits were recorded still load, undo and
/// leave status without an applied commit.
#[test]
fn old_transactions_without_a_commit_still_work() {
    if !has_git() {
        return;
    }
    let sb = Sandbox::empty();
    let (url, _, second) = two_commits(&sb);
    let dir = sb.state().join("backups/1700000000000-abcdef");
    std::fs::create_dir_all(&dir).unwrap();
    let tx = serde_json::json!({
        "id": "1700000000000-abcdef", "created_ms": 1_700_000_000_000u64, "status": "applied",
        "description": format!("apply {url} @ abc1234 -> codex (user scope)"),
        "targets": ["codex"], "entries": [], "created_dirs": []
    });
    std::fs::write(dir.join("transaction.json"), tx.to_string()).unwrap();
    sb.cli()
        .arg("history")
        .assert()
        .success()
        .stdout(contains("1700000000000-abcdef"));
    sb.cli()
        .args(["status", &url, "--to", "claude-code"])
        .assert()
        .success()
        .stdout(contains("no recorded apply of this source").and(contains("moved").not()));
    sb.cli()
        .args(["apply", &url, "--to", "claude-code", "--yes"])
        .assert()
        .success();
    assert_eq!(recorded(&sb)[1]["commit"], second.as_str());
}
