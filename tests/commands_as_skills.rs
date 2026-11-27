//! Antigravity and Copilot (user scope) have no commands folder kitbag can
//! write, but their docs migrate commands into skills (Antigravity's
//! workflows-to-skills guide, VS Code's prompt-file migration), so a profile's
//! commands are written as skills there.

mod common;

use std::fs;

use common::*;
use kitbag::adapters::Scope;
use kitbag::model::{Command, Profile, Skill, SkillFile, Targeting};
use kitbag::txn::Store;
use predicates::prelude::*;
use predicates::str::contains;
use serde_json::{Map, Value};

fn commands() -> Profile {
    let mut p = Profile::new("team");
    p.commands.push(Command {
        name: "code-review".into(),
        description: Some("Review the diff".into()),
        argument_hint: Some("<files>".into()),
        body: "Review $ARGUMENTS carefully.".into(),
        ..Default::default()
    });
    p.commands.push(Command {
        name: "ship".into(),
        body: "Ship it.".into(),
        ..Default::default()
    });
    p
}

fn said(p: &kitbag::engine::Plan, target: &str, what: &str) -> bool {
    p.notes
        .iter()
        .any(|n| n.target == target && n.message.contains(what))
}

fn put(sb: &Sandbox, rel: &str, text: &str) {
    let p = sb.path(rel);
    fs::create_dir_all(p.parent().unwrap()).unwrap();
    fs::write(p, text).unwrap();
}

fn skill(name: &str) -> Skill {
    Skill {
        name: name.into(),
        files: vec![SkillFile {
            path: "SKILL.md".into(),
            content: format!("---\nname: {name}\ndescription: Mine\n---\n\nBody.\n").into_bytes(),
            executable: false,
        }],
        ..Default::default()
    }
}

// ---- Antigravity ----

#[test]
fn antigravity_writes_commands_as_skills_in_both_scopes() {
    let sb = Sandbox::empty();
    let prof = commands();
    let env = sb.env(Scope::User);
    let p = apply(&sb, &prof, &["antigravity"], &env);
    let text = sb.read("home/.gemini/config/skills/code-review/SKILL.md");
    assert_eq!(
        text,
        "---\nname: code-review\ndescription: Review the diff\n---\n\nReview $ARGUMENTS carefully.\n"
    );
    // No description: one is made up, because Antigravity requires it.
    let ship = sb.read("home/.gemini/config/skills/ship/SKILL.md");
    assert!(ship.contains("description: Instructions from the ship command"));
    // Never a workflows folder.
    assert!(!sb.path("home/.gemini/config/workflows").exists());
    assert!(!sb.path("home/.gemini/config/global_workflows").exists());
    assert!(!sb.path("home/.gemini/commands").exists());
    assert!(said(
        &p,
        "antigravity",
        "deprecated workflows in favour of skills"
    ));
    assert!(said(
        &p,
        "antigravity",
        "'code-review' becomes a skill and loses argument-hint"
    ));
    // A second apply changes nothing.
    assert!(plan(&prof, &["antigravity"], &env).changes.is_empty());

    apply(&sb, &prof, &["antigravity"], &sb.env(Scope::Project));
    assert!(
        sb.path("project/.agents/skills/code-review/SKILL.md")
            .is_file()
    );
    assert!(!sb.path("project/.agents/workflows").exists());
    assert!(!sb.path("project/.agents/commands").exists());
}

#[test]
fn antigravity_command_skills_are_additive_and_undo_removes_only_ours() {
    let sb = Sandbox::empty();
    put(
        &sb,
        "home/.gemini/config/skills/mine/SKILL.md",
        "---\nname: mine\ndescription: Mine\n---\n\nBody.\n",
    );
    put(
        &sb,
        "home/.gemini/config/workflows/old.md",
        "An old workflow.\n",
    );
    apply(&sb, &commands(), &["antigravity"], &sb.env(Scope::User));
    assert!(
        sb.path("home/.gemini/config/skills/ship/SKILL.md")
            .is_file()
    );
    Store::new(sb.state()).undo(None, false).unwrap();
    assert!(!sb.path("home/.gemini/config/skills/ship").exists());
    assert!(!sb.path("home/.gemini/config/skills/code-review").exists());
    assert!(
        sb.path("home/.gemini/config/skills/mine/SKILL.md")
            .is_file()
    );
    assert_eq!(
        sb.read("home/.gemini/config/workflows/old.md"),
        "An old workflow.\n"
    );
}

#[test]
fn a_command_named_like_a_skill_is_skipped_and_the_skill_kept() {
    let sb = Sandbox::empty();
    let mut prof = commands();
    prof.skills.push(skill("ship"));
    let p = apply(&sb, &prof, &["antigravity"], &sb.env(Scope::User));
    assert!(said(&p, "antigravity", "command 'ship' skipped"));
    assert!(
        sb.read("home/.gemini/config/skills/ship/SKILL.md")
            .contains("description: Mine")
    );
    assert!(
        sb.path("home/.gemini/config/skills/code-review/SKILL.md")
            .is_file()
    );
}

#[test]
fn command_skills_respect_targets_and_disabled_on() {
    let sb = Sandbox::empty();
    let mut prof = commands();
    prof.commands[0].targeting = Targeting {
        targets: None,
        disabled_on: vec!["antigravity".into(), "copilot".into()],
    };
    prof.commands[1].targeting = Targeting {
        targets: Some(vec!["codex".into()]),
        disabled_on: vec![],
    };
    apply(
        &sb,
        &prof,
        &["antigravity", "copilot"],
        &sb.env(Scope::User),
    );
    assert!(!sb.path("home/.gemini/config/skills").exists());
    assert!(!sb.path("home/.copilot/skills").exists());
}

#[test]
fn antigravity_and_amp_write_a_command_to_the_same_project_skill_once() {
    let sb = Sandbox::empty();
    let prof = commands();
    let env = sb.env(Scope::Project);
    apply(&sb, &prof, &["antigravity", "amp"], &env);
    assert!(sb.path("project/.agents/skills/ship/SKILL.md").is_file());
    assert!(
        plan(&prof, &["antigravity", "amp"], &env)
            .changes
            .is_empty()
    );
    assert!(plan(&prof, &["amp"], &env).changes.is_empty());
}

#[test]
fn antigravity_command_skills_capture_back_as_skills() {
    let sb = Sandbox::empty();
    apply(&sb, &commands(), &["antigravity"], &sb.env(Scope::User));
    let r = capture(&["antigravity"], &sb.env(Scope::User));
    assert!(r.profile.commands.is_empty());
    assert!(r.profile.skills.iter().any(|s| s.name == "code-review"));
}

#[test]
fn the_list_says_commands_are_folded_into_skills() {
    let sb = Sandbox::empty();
    let out = sb
        .cli()
        .args(["list-targets", "--json"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let v: Value = serde_json::from_slice(&out).unwrap();
    let find = |id: &str| {
        v.as_array()
            .unwrap()
            .iter()
            .find(|t| t["id"] == id)
            .unwrap()
            .clone()
    };
    assert_eq!(find("antigravity")["support"]["commands"], "folded");
    assert_eq!(find("copilot")["support"]["commands"], "folded");
}

// ---- Copilot ----

#[test]
fn copilot_user_scope_writes_commands_as_explicit_only_skills() {
    let sb = Sandbox::empty();
    let prof = commands();
    let env = sb.env(Scope::User);
    let p = apply(&sb, &prof, &["copilot"], &env);
    let text = sb.read("home/.copilot/skills/code-review/SKILL.md");
    assert_eq!(
        text,
        "---\nname: code-review\ndescription: Review the diff\nargument-hint: \"<files>\"\ndisable-model-invocation: true\n---\n\nReview $ARGUMENTS carefully.\n"
    );
    assert!(
        sb.read("home/.copilot/skills/ship/SKILL.md")
            .contains("disable-model-invocation: true")
    );
    assert!(!sb.path("home/.copilot/prompts").exists());
    assert!(said(&p, "copilot", "commands are written as skills"));
    assert!(!said(&p, "copilot", "loses argument-hint"));
    assert!(said(
        &p,
        "copilot",
        "'code-review' becomes a skill and loses $ARGUMENTS"
    ));
    assert!(plan(&prof, &["copilot"], &env).changes.is_empty());

    Store::new(sb.state()).undo(None, false).unwrap();
    assert!(!sb.path("home/.copilot").exists());
}

#[test]
fn copilot_project_scope_still_writes_prompt_files() {
    let sb = Sandbox::empty();
    apply(&sb, &commands(), &["copilot"], &sb.env(Scope::Project));
    assert!(
        sb.path("project/.github/prompts/code-review.prompt.md")
            .is_file()
    );
    assert!(!sb.path("project/.github/skills").exists());
}

#[test]
fn copilot_home_moves_the_command_skills() {
    let sb = Sandbox::empty();
    let mut env = sb.env(Scope::User);
    env.locations.insert(
        "COPILOT_HOME".into(),
        sb.path("elsewhere").to_string_lossy().into_owned(),
    );
    apply(&sb, &commands(), &["copilot"], &env);
    assert!(sb.path("elsewhere/skills/ship/SKILL.md").is_file());
    assert!(!sb.path("home/.copilot").exists());
}

#[test]
fn command_frontmatter_a_skill_cannot_hold_is_reported() {
    let sb = Sandbox::empty();
    let mut prof = Profile::new("team");
    let mut extra = Map::new();
    extra.insert("model".into(), Value::String("gpt".into()));
    prof.commands.push(Command {
        name: "tuned".into(),
        extra,
        body: "Do it.".into(),
        ..Default::default()
    });
    let p = plan(&prof, &["copilot"], &sb.env(Scope::User));
    assert!(said(
        &p,
        "copilot",
        "'tuned' becomes a skill and loses its other frontmatter"
    ));
    let text = planned(&p, &sb.home().join(".copilot/skills/tuned/SKILL.md")).unwrap();
    assert!(!text.contains("model:"), "{text}");
}

// ---- remote sources ----

#[test]
fn a_remote_command_with_a_shell_snippet_is_not_written_as_a_skill_without_trust() {
    if !has_git() {
        return;
    }
    let sb = Sandbox::empty();
    let url = sb.git_profile(
        "evil",
        &[
            ("kitbag.toml", "kitbag = 1\nname = \"evil\"\n"),
            (
                "commands/review.md",
                "---\ndescription: Review\n---\nRun !{id} first.\n",
            ),
            (
                "commands/plain.md",
                "---\ndescription: Plain\n---\nSummarise.\n",
            ),
        ],
    );
    sb.cli()
        .args(["apply", &url, "--to", "antigravity,copilot", "--yes"])
        .assert()
        .success()
        .stdout(contains("command 'review'").and(contains("--allow-risky-settings")));
    for dir in ["home/.gemini/config/skills", "home/.copilot/skills"] {
        assert!(!sb.path(&format!("{dir}/review")).exists(), "{dir}");
        assert!(
            sb.read(&format!("{dir}/plain/SKILL.md"))
                .contains("Summarise.")
        );
    }
    sb.cli().arg("undo").assert().success();
    sb.cli()
        .args([
            "apply",
            &url,
            "--to",
            "antigravity,copilot",
            "--yes",
            "--trust-source",
        ])
        .assert()
        .success();
    assert!(
        sb.read("home/.gemini/config/skills/review/SKILL.md")
            .contains("!{id}")
    );
    assert!(
        sb.read("home/.copilot/skills/review/SKILL.md")
            .contains("!{id}")
    );
}
