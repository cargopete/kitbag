//! `autoMemoryDirectory`: capture and apply follow it from the documented
//! settings scopes, expand `~/`, and honour the workspace-trust rule.

mod common;

use std::fs;
use std::path::Path;

use common::*;
use predicates::str::contains;
use serde_json::json;

const SCOPE: &str = "auto-memory-directory";

fn write(path: &Path, content: &str) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, content).unwrap();
}

fn settings(path: &Path, v: serde_json::Value) {
    write(path, &serde_json::to_string_pretty(&v).unwrap());
}

fn capture(sb: &Sandbox) -> assert_cmd::assert::Assert {
    sb.cli()
        .current_dir(sb.project())
        .env("KITBAG_MANAGED_SETTINGS_DIR", sb.path("managed"))
        .args([
            "capture",
            "--from",
            "claude-code",
            "-o",
            sb.path("prof").to_str().unwrap(),
            "--include",
            "memory",
        ])
        .assert()
        .success()
}

fn captured(sb: &Sandbox) -> bool {
    sb.path(&format!("prof/memory/claude-code/{SCOPE}/MEMORY.md"))
        .exists()
}

fn notes(dir: &Path) {
    write(&dir.join("MEMORY.md"), "- [Role](user_role.md)\n");
    write(&dir.join("user_role.md"), "A data scientist.\n");
}

#[test]
fn user_setting_with_a_tilde_is_followed() {
    let sb = Sandbox::empty();
    settings(
        &sb.home().join(".claude/settings.json"),
        json!({"autoMemoryDirectory": "~/my-notes"}),
    );
    notes(&sb.home().join("my-notes"));
    capture(&sb).stdout(contains("following autoMemoryDirectory"));
    assert!(captured(&sb));
    assert!(
        sb.path(&format!("prof/memory/claude-code/{SCOPE}/user_role.md"))
            .exists()
    );
}

#[test]
fn managed_policy_wins_over_user_and_a_relative_value_is_skipped() {
    let sb = Sandbox::empty();
    settings(
        &sb.home().join(".claude/settings.json"),
        json!({"autoMemoryDirectory": sb.path("user-dir").to_str().unwrap()}),
    );
    notes(&sb.path("user-dir"));
    settings(
        &sb.path("managed/managed-settings.json"),
        json!({"autoMemoryDirectory": "relative/dir"}),
    );
    capture(&sb).stdout(contains("not an absolute path and does not start with ~/"));
    assert!(!captured(&sb));
    // A valid managed value takes over from the user's.
    settings(
        &sb.path("managed/managed-settings.json"),
        json!({"autoMemoryDirectory": sb.path("policy-dir").to_str().unwrap()}),
    );
    write(&sb.path("policy-dir/MEMORY.md"), "- policy\n");
    fs::remove_dir_all(sb.path("prof")).ok();
    capture(&sb);
    assert_eq!(
        sb.read(&format!("prof/memory/claude-code/{SCOPE}/MEMORY.md")),
        "- policy\n"
    );
}

#[test]
fn project_settings_follow_the_trust_rule() {
    let sb = Sandbox::empty();
    let dir = sb.path("repo-chosen");
    notes(&dir);
    settings(
        &sb.project().join(".claude/settings.json"),
        json!({"autoMemoryDirectory": dir.to_str().unwrap(),
               "permissions": {"blockReadsOutsideWorkingDirectories": true}}),
    );
    capture(&sb).stdout(contains("chosen by a repository's settings file"));
    assert!(!captured(&sb));
    // The same file with the block off is followed.
    settings(
        &sb.project().join(".claude/settings.json"),
        json!({"autoMemoryDirectory": dir.to_str().unwrap()}),
    );
    capture(&sb);
    assert!(captured(&sb));
    // The block set in the user's own settings still covers a local file's choice.
    fs::remove_dir_all(sb.path("prof")).unwrap();
    settings(
        &sb.home().join(".claude/settings.json"),
        json!({"permissions": {"blockReadsOutsideWorkingDirectories": true}}),
    );
    settings(
        &sb.project().join(".claude/settings.local.json"),
        json!({"autoMemoryDirectory": dir.to_str().unwrap()}),
    );
    capture(&sb).stdout(contains("chosen by a repository's settings file"));
    assert!(!captured(&sb));
}

#[test]
fn a_user_setting_is_not_a_repository_file_even_with_the_block_on() {
    let sb = Sandbox::empty();
    let dir = sb.path("mine");
    notes(&dir);
    settings(
        &sb.home().join(".claude/settings.json"),
        json!({"autoMemoryDirectory": dir.to_str().unwrap(),
               "permissions": {"blockReadsOutsideWorkingDirectories": true}}),
    );
    capture(&sb);
    assert!(captured(&sb));
}

fn apply(sb: &Sandbox) -> assert_cmd::assert::Assert {
    sb.cli()
        .current_dir(sb.project())
        .env("KITBAG_MANAGED_SETTINGS_DIR", sb.path("managed"))
        .args([
            "apply",
            sb.path("prof").to_str().unwrap(),
            "--to",
            "claude-code",
            "--yes",
        ])
        .assert()
        .success()
}

fn captured_profile() -> Sandbox {
    let src = Sandbox::empty();
    let dir = src.path("src-notes");
    notes(&dir);
    settings(
        &src.home().join(".claude/settings.json"),
        json!({"autoMemoryDirectory": dir.to_str().unwrap()}),
    );
    capture(&src);
    src
}

#[test]
fn apply_adds_missing_notes_to_the_custom_directory_and_keeps_differing_ones() {
    let src = captured_profile();
    let prof = src.path("prof");
    let sb = Sandbox::empty();
    copy_dir(&prof, &sb.path("prof"));
    // Not set here: nothing is written.
    write(&sb.home().join(".claude/settings.json"), "{}\n");
    apply(&sb).stdout(contains("no autoMemoryDirectory is set here"));
    // Set, but the directory is missing: not created.
    let dest = sb.home().join("notes-here");
    settings(
        &sb.home().join(".claude/settings.json"),
        json!({"autoMemoryDirectory": "~/notes-here"}),
    );
    apply(&sb).stdout(contains("doesn't exist here"));
    assert!(!dest.exists());
    // Present: the missing note is added, the differing one kept.
    write(&dest.join("user_role.md"), "my own version\n");
    apply(&sb).stdout(contains("kept 1 note(s)"));
    assert_eq!(
        fs::read_to_string(dest.join("user_role.md")).unwrap(),
        "my own version\n"
    );
    assert_eq!(
        fs::read_to_string(dest.join("MEMORY.md")).unwrap(),
        "- [Role](user_role.md)\n"
    );
}

#[test]
fn apply_never_writes_into_the_managed_policy_folder() {
    let src = captured_profile();
    let sb = Sandbox::empty();
    copy_dir(&src.path("prof"), &sb.path("prof"));
    let managed = sb.path("managed");
    fs::create_dir_all(managed.join("memory")).unwrap();
    settings(
        &managed.join("managed-settings.json"),
        json!({"autoMemoryDirectory": managed.join("memory").to_str().unwrap()}),
    );
    apply(&sb).stdout(contains("managed-policy folder"));
    assert!(!managed.join("memory/MEMORY.md").exists());
}

#[test]
fn apply_skips_a_repository_chosen_directory_when_reads_are_blocked() {
    let src = captured_profile();
    let sb = Sandbox::empty();
    copy_dir(&src.path("prof"), &sb.path("prof"));
    let dest = sb.path("repo-dir");
    fs::create_dir_all(&dest).unwrap();
    settings(
        &sb.project().join(".claude/settings.local.json"),
        json!({"autoMemoryDirectory": dest.to_str().unwrap(),
               "permissions": {"blockReadsOutsideWorkingDirectories": true}}),
    );
    apply(&sb).stdout(contains("chosen by a repository's settings file"));
    assert!(!dest.join("MEMORY.md").exists());
}
