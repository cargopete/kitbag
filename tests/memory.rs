//! Memory notes: opt-in capture, native apply only where documented, trust
//! for git sources, undo.

mod common;

use std::fs;
use std::path::Path;

use common::*;
use kitbag::profile_io;
use predicates::prelude::*;
use predicates::str::contains;

const TOKEN: &str = "ghp_FAKE0123456789abcdefghijklmnopqrstuv";
const PROJECT: &str = "-work-demo";

fn write(path: &Path, content: &str) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, content).unwrap();
}

/// Claude Code with auto memory for one project, Codex with generated
/// memories, and Hermes with its two files and a named profile.
fn setup(sb: &Sandbox) {
    let h = sb.home();
    write(&h.join(".claude/settings.json"), "{\"model\": \"opus\"}\n");
    let mem = h.join(format!(".claude/projects/{PROJECT}/memory"));
    write(
        &mem.join("MEMORY.md"),
        "- [Role](user_role.md) - who you are\n",
    );
    write(
        &mem.join("user_role.md"),
        "---\nname: role\ntype: user\nmodified: 2026-11-01T10:00:00Z\n---\nA data scientist.\n",
    );
    write(
        &mem.join("reference_staging.md"),
        &format!("---\nname: staging\ntype: reference\n---\nStaging key {TOKEN}\n"),
    );
    write(&h.join(".codex/config.toml"), "model = \"gpt-5\"\n");
    write(
        &h.join(".codex/memories_v2/summary.md"),
        "Prefers small PRs.\n",
    );
    write(&h.join(".hermes/config.yaml"), "model:\n  default: x\n");
    write(
        &h.join(".hermes/memories/MEMORY.md"),
        "Uses uv, not pip\n§\nRuns tests with just\n",
    );
    write(&h.join(".hermes/memories/USER.md"), "Name: Test User\n");
    write(&h.join(".hermes/memories/scratch.txt"), "not a note\n");
    write(
        &h.join(".hermes/profiles/work/memories/MEMORY.md"),
        "Work laptop notes\n",
    );
}

fn capture(sb: &Sandbox, include: bool) -> assert_cmd::assert::Assert {
    let mut c = sb.cli();
    c.args([
        "capture",
        "--from",
        "claude-code,codex,hermes",
        "-o",
        "prof",
    ]);
    if include {
        c.args(["--include", "memory"]);
    }
    c.assert().success()
}

fn memory_files(sb: &Sandbox) -> Vec<String> {
    let mut out = Vec::new();
    fn walk(dir: &Path, base: &Path, out: &mut Vec<String>) {
        let Ok(rd) = fs::read_dir(dir) else { return };
        for e in rd.flatten() {
            if e.path().is_dir() {
                walk(&e.path(), base, out);
            } else {
                out.push(
                    e.path()
                        .strip_prefix(base)
                        .unwrap()
                        .to_string_lossy()
                        .replace('\\', "/"),
                );
            }
        }
    }
    let base = sb.path("prof/memory");
    walk(&base, &base, &mut out);
    out.sort();
    out
}

#[test]
fn a_plain_capture_takes_no_memory() {
    let sb = Sandbox::empty();
    setup(&sb);
    capture(&sb, false);
    assert!(!sb.path("prof/memory").exists());
    let p = profile_io::load(&sb.path("prof")).unwrap();
    assert!(p.memory.is_empty());
}

#[test]
fn capture_with_the_flag_takes_notes_redacted_and_keeps_their_metadata() {
    let sb = Sandbox::empty();
    setup(&sb);
    capture(&sb, true)
        .stdout(contains("memory note(s)"))
        .stdout(contains("type: user, modified: 2026-11-01T10:00:00Z"))
        .stdout(contains("project folder name"))
        .stdout(contains("read-only copy"));
    assert_eq!(
        memory_files(&sb),
        [
            format!("claude-code/{PROJECT}/MEMORY.md"),
            format!("claude-code/{PROJECT}/reference_staging.md"),
            format!("claude-code/{PROJECT}/user_role.md"),
            "codex/user/memories_v2/summary.md".to_string(),
            "hermes/default/MEMORY.md".to_string(),
            "hermes/default/USER.md".to_string(),
            "hermes/profile-work/MEMORY.md".to_string(),
        ]
    );
    let note = sb.read(&format!("prof/memory/claude-code/{PROJECT}/user_role.md"));
    assert!(note.contains("type: user") && note.contains("modified: 2026-11-01T10:00:00Z"));
    let secret = sb.read(&format!(
        "prof/memory/claude-code/{PROJECT}/reference_staging.md"
    ));
    assert!(!secret.contains(TOKEN), "{secret}");
    assert!(secret.contains("[REDACTED") && secret.contains("type: reference"));
    // The saved profile still loads, and the secret scan is clean.
    let p = profile_io::load(&sb.path("prof")).unwrap();
    assert_eq!(p.memory.len(), 7);
}

#[test]
fn recapturing_without_the_flag_keeps_the_notes() {
    let sb = Sandbox::empty();
    setup(&sb);
    capture(&sb, true);
    sb.cli()
        .args(["capture", "--from", "claude-code", "-o", "prof", "--force"])
        .assert()
        .success()
        .stdout(contains("--include memory"));
    assert_eq!(memory_files(&sb).len(), 7);
}

#[test]
fn project_scope_captures_no_memory() {
    let sb = Sandbox::empty();
    setup(&sb);
    sb.cli()
        .args([
            "capture",
            "--from",
            "claude-code",
            "--project",
            &sb.project().display().to_string(),
            "-o",
            "prof",
            "--include",
            "memory",
        ])
        .assert()
        .success()
        .stdout(contains("only captured at user scope"));
    assert!(!sb.path("prof/memory").exists());
}

#[test]
fn tools_without_a_store_say_why() {
    let sb = Sandbox::empty();
    write(
        &sb.home().join(".cursor/mcp.json"),
        "{\"mcpServers\": {}}\n",
    );
    sb.cli()
        .args([
            "capture",
            "--from",
            "cursor",
            "-o",
            "prof",
            "--include",
            "memory",
        ])
        .assert()
        .success()
        .stdout(contains("Memories UI"));
}

fn captured_profile() -> (Sandbox, String) {
    let sb = Sandbox::empty();
    setup(&sb);
    capture(&sb, true);
    let prof = sb.path("prof").display().to_string();
    (sb, prof)
}

fn apply_to(prof: &str, target: &str) -> Vec<String> {
    ["apply", prof, "--to", target, "--yes"]
        .iter()
        .map(|s| s.to_string())
        .collect()
}

#[test]
fn apply_writes_claude_notes_only_into_a_project_folder_that_exists() {
    let (_cap, prof) = captured_profile();
    let sb = Sandbox::empty();
    write(&sb.home().join(".claude/settings.json"), "{}\n");
    // No project folder: nothing is created.
    sb.cli()
        .args(apply_to(&prof, "claude-code"))
        .assert()
        .success()
        .stdout(contains("has no such folder here"));
    assert!(!sb.home().join(".claude/projects").exists());
    // The folder exists (Claude Code has run there): notes go in, kept ones stay.
    let mem = sb.home().join(format!(".claude/projects/{PROJECT}/memory"));
    write(&mem.join("user_role.md"), "my own version\n");
    sb.cli()
        .args(apply_to(&prof, "claude-code"))
        .assert()
        .success()
        .stdout(contains("kept 1 note(s) that already exist"));
    assert_eq!(
        fs::read_to_string(mem.join("user_role.md")).unwrap(),
        "my own version\n"
    );
    assert!(
        fs::read_to_string(mem.join("MEMORY.md"))
            .unwrap()
            .contains("user_role.md")
    );
    // Nothing else of the profile landed in the tool: Codex notes aren't written.
    assert!(!sb.home().join(".codex").exists());
    sb.cli().arg("undo").assert().success();
    assert!(!mem.join("MEMORY.md").exists());
    assert_eq!(
        fs::read_to_string(mem.join("user_role.md")).unwrap(),
        "my own version\n"
    );
}

#[test]
fn notes_with_redaction_markers_are_not_written_to_claude() {
    let (_cap, prof) = captured_profile();
    let sb = Sandbox::empty();
    let mem = sb.home().join(format!(".claude/projects/{PROJECT}/memory"));
    fs::create_dir_all(&mem).unwrap();
    sb.cli()
        .args(apply_to(&prof, "claude-code"))
        .assert()
        .success()
        .stdout(contains("[REDACTED] markers"));
    assert!(mem.join("MEMORY.md").is_file());
    assert!(mem.join("user_role.md").is_file());
    assert!(!mem.join("reference_staging.md").exists());
}

#[test]
fn apply_adds_missing_hermes_entries_and_undo_restores_the_file() {
    let (_cap, prof) = captured_profile();
    let sb = Sandbox::empty();
    write(
        &sb.home().join(".hermes/config.yaml"),
        "model:\n  default: x\n",
    );
    write(
        &sb.home().join(".hermes/memories/MEMORY.md"),
        "Uses uv, not pip\n",
    );
    let before = sb.read("home/.hermes/memories/MEMORY.md");
    sb.cli()
        .args(apply_to(&prof, "hermes"))
        .assert()
        .success()
        .stdout(contains("adds 1 entry"));
    let text = sb.read("home/.hermes/memories/MEMORY.md");
    assert_eq!(text, "Uses uv, not pip\n§\nRuns tests with just\n");
    assert!(
        sb.read("home/.hermes/memories/USER.md")
            .contains("Test User")
    );
    // The named profile has no folder here, so its note is not written.
    assert!(!sb.home().join(".hermes/profiles").exists());
    // Applying again changes nothing.
    sb.cli().args(apply_to(&prof, "hermes")).assert().success();
    assert_eq!(sb.read("home/.hermes/memories/MEMORY.md"), text);
    sb.cli().arg("undo").assert().success();
    assert_eq!(sb.read("home/.hermes/memories/MEMORY.md"), before);
    assert!(!sb.home().join(".hermes/memories/USER.md").exists());
}

#[test]
fn hermes_entries_over_the_documented_limit_are_left_out() {
    let sb = Sandbox::empty();
    write(
        &sb.home().join(".hermes/config.yaml"),
        "model:\n  default: x\n",
    );
    write(
        &sb.home().join(".hermes/memories/USER.md"),
        &"x".repeat(1300),
    );
    let big = "y".repeat(100);
    let prof = sb.path("p");
    sb.cli().args(["init", "p"]).assert().success();
    write(&prof.join("memory/hermes/default/USER.md"), &big);
    sb.cli()
        .args(["apply", "p", "--to", "hermes", "--yes"])
        .assert()
        .success()
        .stdout(contains("1375 characters"));
    assert_eq!(sb.read("home/.hermes/memories/USER.md"), "x".repeat(1300));
}

#[test]
fn codex_notes_are_never_written() {
    let (_cap, prof) = captured_profile();
    let sb = Sandbox::empty();
    write(&sb.home().join(".codex/config.toml"), "model = \"gpt-5\"\n");
    sb.cli()
        .args(apply_to(&prof, "codex"))
        .assert()
        .success()
        .stdout(contains("generated state"));
    assert!(!sb.home().join(".codex/memories_v2").exists());
    assert!(!sb.home().join(".codex/memories").exists());
}

#[test]
fn notes_for_a_tool_that_is_not_a_target_are_not_applied() {
    let (_cap, prof) = captured_profile();
    let sb = Sandbox::empty();
    write(
        &sb.home().join(".hermes/config.yaml"),
        "model:\n  default: x\n",
    );
    sb.cli()
        .args(apply_to(&prof, "hermes"))
        .assert()
        .success()
        .stdout(contains("not among the targets"));
}

#[test]
fn a_profile_with_an_unsafe_note_path_does_not_load() {
    let sb = Sandbox::empty();
    sb.cli().args(["init", "p"]).assert().success();
    write(&sb.path("p/memory/claude-code/p/a b.md"), "x");
    let e = profile_io::load(&sb.path("p")).unwrap_err();
    assert!(
        format!("{e:#}").contains("memory/claude-code/p/a b.md"),
        "{e:#}"
    );
    fs::remove_file(sb.path("p/memory/claude-code/p/a b.md")).unwrap();
    write(&sb.path("p/memory/claude-code/p/x.txt"), "x");
    assert!(profile_io::load(&sb.path("p")).is_err());
}

#[test]
fn the_secret_scan_covers_notes() {
    let sb = Sandbox::empty();
    sb.cli().args(["init", "p"]).assert().success();
    write(
        &sb.path(&format!("p/memory/claude-code/{PROJECT}/a.md")),
        &format!("key {TOKEN}\n"),
    );
    let p = profile_io::load(&sb.path("p")).unwrap();
    let found = profile_io::scan_for_secrets(&p);
    assert!(found.iter().any(|f| f.contains("memory/")), "{found:?}");
}

#[test]
fn memory_from_a_git_source_needs_trust_source() {
    if !has_git() {
        return;
    }
    let (cap, _) = captured_profile();
    let mut files = Vec::new();
    for rel in memory_files(&cap) {
        files.push((
            format!("memory/{rel}"),
            cap.read(&format!("prof/memory/{rel}")),
        ));
    }
    files.push(("kitbag.toml".to_string(), cap.read("prof/kitbag.toml")));
    let files: Vec<(&str, &str)> = files
        .iter()
        .map(|(a, b)| (a.as_str(), b.as_str()))
        .collect();
    let sb = Sandbox::empty();
    let url = sb.git_profile("repo", &files);
    write(
        &sb.home().join(".hermes/config.yaml"),
        "model:\n  default: x\n",
    );
    sb.cli()
        .args(["apply", &url, "--to", "hermes", "--yes"])
        .assert()
        .success()
        .stdout(contains("memory note(s) not applied").and(contains("--trust-source")));
    assert!(!sb.home().join(".hermes/memories").exists());
    sb.cli()
        .args(["apply", &url, "--to", "hermes", "--yes", "--trust-source"])
        .assert()
        .success();
    assert!(
        sb.read("home/.hermes/memories/MEMORY.md")
            .contains("Uses uv")
    );
}

#[test]
fn inspect_lists_the_notes() {
    let (_cap, prof) = captured_profile();
    let sb = Sandbox::empty();
    sb.cli()
        .args(["inspect", &prof, "--to", "claude-code"])
        .assert()
        .success()
        .stdout(contains("memory (7)").and(contains("type: user")));
}

#[test]
fn project_scope_apply_skips_memory() {
    let (_cap, prof) = captured_profile();
    let sb = Sandbox::empty();
    sb.cli()
        .args([
            "apply",
            &prof,
            "--to",
            "claude-code",
            "--project",
            &sb.project().display().to_string(),
            "--yes",
        ])
        .assert()
        .success()
        .stdout(contains("user scope only"));
}
