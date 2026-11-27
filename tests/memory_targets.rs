//! Per-note `targets` / `disabled_on` and `fold_into_instructions`.

mod common;

use std::fs;
use std::path::Path;

use common::*;
use predicates::prelude::*;
use predicates::str::contains;

const PROJECT: &str = "-work-demo";

fn write(path: &Path, content: &str) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, content).unwrap();
}

/// A profile with one Claude note (`extra` frontmatter lines added), one
/// Hermes note and one Codex note, captured for real so it is a valid profile.
fn profile(extra: &str) -> (Sandbox, String) {
    let sb = Sandbox::empty();
    let h = sb.home();
    write(&h.join(".claude/settings.json"), "{}\n");
    write(
        &h.join(format!(".claude/projects/{PROJECT}/memory/role.md")),
        "---\nname: role\ntype: user\n---\nA data scientist.\n",
    );
    write(&h.join(".hermes/config.yaml"), "model:\n  default: x\n");
    write(&h.join(".hermes/memories/USER.md"), "Name: Test User\n");
    write(&h.join(".codex/config.toml"), "model = \"gpt-5\"\n");
    write(&h.join(".codex/memories_v2/summary.md"), "Small PRs.\n");
    sb.cli()
        .args([
            "capture",
            "--from",
            "claude-code,codex,hermes",
            "-o",
            "prof",
            "--include",
            "memory",
        ])
        .assert()
        .success();
    let prof = sb.path("prof").display().to_string();
    if !extra.is_empty() {
        let p = sb.path(&format!("prof/memory/claude-code/{PROJECT}/role.md"));
        let text = fs::read_to_string(&p).unwrap();
        fs::write(
            &p,
            text.replacen("type: user\n", &format!("type: user\n{extra}"), 1),
        )
        .unwrap();
    }
    (sb, prof)
}

fn target_box() -> Sandbox {
    let sb = Sandbox::empty();
    let h = sb.home();
    write(&h.join(".claude/settings.json"), "{}\n");
    fs::create_dir_all(h.join(format!(".claude/projects/{PROJECT}"))).unwrap();
    write(&h.join(".hermes/config.yaml"), "model:\n  default: x\n");
    sb
}

fn apply(sb: &Sandbox, prof: &str, to: &str, extra: &[&str]) -> assert_cmd::assert::Assert {
    sb.cli()
        .args(["apply", prof, "--to", to, "--yes"])
        .args(extra)
        .assert()
        .success()
}

fn note(sb: &Sandbox) -> std::path::PathBuf {
    sb.home()
        .join(format!(".claude/projects/{PROJECT}/memory/role.md"))
}

#[test]
fn without_targeting_a_note_applies_only_to_its_own_tool() {
    let (_cap, prof) = profile("");
    let sb = target_box();
    apply(&sb, &prof, "claude-code,hermes", &[]);
    assert!(note(&sb).is_file());
    // Hermes got its own USER.md only, not the Claude note.
    assert!(!sb.home().join(".hermes/memories/role.md").exists());
    assert_eq!(
        fs::read_to_string(note(&sb))
            .unwrap()
            .matches("A data")
            .count(),
        1
    );
}

#[test]
fn targets_and_disabled_on_decide_whether_the_note_applies() {
    // `targets` without the note's own tool: skipped, and not copied to the
    // other store listed.
    let (_cap, prof) = profile("targets: [hermes]\n");
    let sb = target_box();
    apply(&sb, &prof, "claude-code,hermes", &[]).stdout(contains("skipped on claude-code"));
    assert!(!note(&sb).exists());
    assert!(!sb.home().join(".hermes/memories/role.md").exists());

    let (_cap, prof) = profile("disabled_on: [claude-code]\n");
    let sb = target_box();
    apply(&sb, &prof, "claude-code", &[]).stdout(contains("skipped on claude-code"));
    assert!(!note(&sb).exists());

    let (_cap, prof) = profile("disabled_on: [hermes]\n");
    let sb = target_box();
    apply(&sb, &prof, "claude-code", &[]);
    assert!(note(&sb).is_file());

    // Older names are accepted.
    let (_cap, prof) = profile("only: [codex]\n");
    let sb = target_box();
    apply(&sb, &prof, "claude-code", &[]).stdout(contains("skipped on claude-code"));
    assert!(!note(&sb).exists());
}

#[test]
fn kitbag_keys_are_not_written_to_the_tools_note() {
    let (_cap, prof) = profile("targets: [claude-code, cursor]\nfold_into_instructions: true\n");
    let sb = target_box();
    apply(&sb, &prof, "claude-code", &[]);
    let text = fs::read_to_string(note(&sb)).unwrap();
    assert!(text.contains("type: user") && text.contains("name: role"));
    for key in ["targets", "disabled_on", "fold_into_instructions", "cursor"] {
        assert!(!text.contains(key), "{key} leaked into {text}");
    }
}

#[test]
fn bad_targeting_is_reported() {
    let (_cap, prof) = profile("targets: [hermes]\ndisabled_on: [codex]\n");
    let sb = Sandbox::empty();
    sb.cli()
        .args(["apply", &prof, "--to", "claude-code", "--yes"])
        .assert()
        .failure();
    let (_cap, prof) = profile("targets: [claude-code, nonesuch]\n");
    let sb = target_box();
    sb.cli()
        .args(["apply", &prof, "--to", "claude-code", "--yes"])
        .assert()
        .success()
        .stderr(contains("unknown target 'nonesuch'"));
    assert!(note(&sb).is_file());
    let (_cap, prof) = profile("fold_into_instructions: maybe\n");
    sb.cli()
        .args(["apply", &prof, "--to", "claude-code", "--yes"])
        .assert()
        .failure();
}

#[test]
fn a_recapture_keeps_the_notes_settings() {
    let (sb, _prof) = profile("disabled_on: [hermes]\nfold_into_instructions: true\n");
    sb.cli()
        .args([
            "capture",
            "--from",
            "claude-code",
            "-o",
            "prof",
            "--include",
            "memory",
            "--force",
        ])
        .assert()
        .success();
    let text = sb.read(&format!("prof/memory/claude-code/{PROJECT}/role.md"));
    assert!(text.contains("disabled_on") && text.contains("fold_into_instructions: true"));
    assert!(text.contains("A data scientist."));
}

fn gemini_instructions(sb: &Sandbox) -> String {
    fs::read_to_string(sb.home().join(".gemini/GEMINI.md")).unwrap_or_default()
}

#[test]
fn a_folded_note_goes_into_the_existing_instructions_of_a_tool_without_a_store() {
    let (_cap, prof) = profile("fold_into_instructions: true\n");
    let sb = Sandbox::empty();
    write(&sb.home().join(".gemini/GEMINI.md"), "My own rules.\n");
    apply(&sb, &prof, "gemini-cli", &[]).stdout(contains("folds memory/"));
    let text = gemini_instructions(&sb);
    assert!(text.contains("My own rules."), "{text}");
    assert!(text.contains("A data scientist."), "{text}");
    assert!(!text.contains("fold_into_instructions") && !text.contains("type: user"));
    // Nothing else is created: no memory folder, no Codex note.
    assert!(!sb.home().join(".gemini/memory").exists());
    assert!(!text.contains("Small PRs"));
    // Again changes nothing; undo restores the file.
    apply(&sb, &prof, "gemini-cli", &[]);
    assert_eq!(gemini_instructions(&sb), text);
    sb.cli().arg("undo").assert().success();
    assert_eq!(gemini_instructions(&sb), "My own rules.\n");
}

#[test]
fn notes_are_not_folded_unless_marked_or_where_targeting_excludes() {
    let (_cap, prof) = profile("");
    let sb = Sandbox::empty();
    apply(&sb, &prof, "gemini-cli", &[]);
    assert!(!gemini_instructions(&sb).contains("A data scientist."));

    let (_cap, prof) = profile("fold_into_instructions: true\ndisabled_on: [gemini-cli]\n");
    let sb = Sandbox::empty();
    apply(&sb, &prof, "gemini-cli", &[]).stdout(contains("not folded"));
    assert!(!gemini_instructions(&sb).contains("A data scientist."));

    let (_cap, prof) = profile("fold_into_instructions: true\ntargets: [cursor]\n");
    let sb = Sandbox::empty();
    apply(&sb, &prof, "gemini-cli", &[]);
    assert!(!gemini_instructions(&sb).contains("A data scientist."));
}

#[test]
fn tools_with_a_memory_store_are_not_folded() {
    let (_cap, prof) = profile("fold_into_instructions: true\n");
    let sb = target_box();
    apply(&sb, &prof, "claude-code", &[]);
    assert!(note(&sb).is_file());
    assert!(
        !fs::read_to_string(sb.home().join(".claude/CLAUDE.md"))
            .unwrap_or_default()
            .contains("A data scientist.")
    );
}

#[test]
fn codex_generated_notes_are_not_folded() {
    let (cap, prof) = profile("");
    let p = cap.path("prof/memory/codex/user/memories_v2/summary.md");
    let text = fs::read_to_string(&p).unwrap();
    fs::write(
        &p,
        format!("---\nfold_into_instructions: true\n---\n{text}"),
    )
    .unwrap();
    let sb = Sandbox::empty();
    apply(&sb, &prof, "gemini-cli", &[]).stdout(contains("generated state"));
    assert!(!gemini_instructions(&sb).contains("Small PRs"));
}

#[test]
fn redacted_notes_are_not_folded() {
    let (cap, prof) = profile("fold_into_instructions: true\n");
    let p = cap.path(&format!("prof/memory/claude-code/{PROJECT}/role.md"));
    let text = fs::read_to_string(&p).unwrap();
    fs::write(&p, format!("{text}key: [REDACTED:api-key]\n")).unwrap();
    let sb = Sandbox::empty();
    apply(&sb, &prof, "gemini-cli", &[]).stdout(contains("[REDACTED] markers"));
    assert!(!gemini_instructions(&sb).contains("A data scientist."));
}

#[test]
fn a_git_source_folds_nothing_without_trust_source() {
    if !has_git() {
        return;
    }
    let (cap, _) = profile("fold_into_instructions: true\n");
    let rel = format!("memory/claude-code/{PROJECT}/role.md");
    let files = [
        (rel.as_str(), cap.read(&format!("prof/{rel}"))),
        ("kitbag.toml", cap.read("prof/kitbag.toml")),
    ];
    let files: Vec<(&str, &str)> = files.iter().map(|(a, b)| (*a, b.as_str())).collect();
    let sb = Sandbox::empty();
    let url = sb.git_profile("repo", &files);
    sb.cli()
        .args(["apply", &url, "--to", "gemini-cli", "--yes"])
        .assert()
        .success()
        .stdout(contains("memory note(s) not applied").and(contains("--trust-source")));
    assert!(!gemini_instructions(&sb).contains("A data scientist."));
    sb.cli()
        .args([
            "apply",
            &url,
            "--to",
            "gemini-cli",
            "--yes",
            "--trust-source",
        ])
        .assert()
        .success();
    assert!(gemini_instructions(&sb).contains("A data scientist."));
}
