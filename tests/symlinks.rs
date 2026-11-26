//! `--force` overrides "this file changed since", never path confinement:
//! undo, init and capture must not delete or write through symbolic links
//! that appeared after the fact.
#![cfg(unix)]

mod common;

use std::fs;
use std::os::unix::fs::symlink;

use common::*;
use predicates::prelude::*;
use predicates::str::contains;

const SERVER: &str = "kitbag = 1\nname = \"p\"\n\n[mcp_servers.test]\ncommand = \"c\"\n";

/// `~/.codex` is a link to `real/`; after an apply, `real` is swapped for a
/// link to `outside/`, which holds a file of the same name.
fn swapped(sb: &Sandbox, extra_targets: &str) {
    fs::create_dir_all(sb.path("p")).unwrap();
    fs::write(sb.path("p/kitbag.toml"), SERVER).unwrap();
    fs::write(sb.path("p/instructions.md"), "Be brief.\n").unwrap();
    fs::create_dir_all(sb.path("real")).unwrap();
    symlink(sb.path("real"), sb.home().join(".codex")).unwrap();
    sb.cli()
        .args([
            "apply",
            "p",
            "--to",
            &format!("codex{extra_targets}"),
            "--yes",
        ])
        .assert()
        .success();
    assert!(sb.path("real/config.toml").exists());
    fs::rename(sb.path("real"), sb.path("real-orig")).unwrap();
    fs::create_dir_all(sb.path("outside")).unwrap();
    fs::write(sb.path("outside/config.toml"), "INNOCENT\n").unwrap();
    symlink(sb.path("outside"), sb.path("real")).unwrap();
}

#[test]
fn undo_force_does_not_follow_a_parent_swapped_for_a_link() {
    let sb = Sandbox::empty();
    swapped(&sb, "");
    sb.cli()
        .args(["undo", "--force"])
        .assert()
        .code(1)
        .stdout(contains("symbolic link").and(contains("left alone")));
    assert_eq!(sb.read("outside/config.toml"), "INNOCENT\n");
}

#[test]
fn undo_without_force_also_refuses() {
    let sb = Sandbox::empty();
    swapped(&sb, "");
    sb.cli().arg("undo").assert().failure();
    assert_eq!(sb.read("outside/config.toml"), "INNOCENT\n");
}

#[test]
fn undo_force_still_reverts_the_other_files() {
    let sb = Sandbox::empty();
    swapped(&sb, ",claude-code");
    assert!(sb.home().join(".claude/CLAUDE.md").exists());
    sb.cli()
        .args(["undo", "--force"])
        .assert()
        .code(1)
        .stdout(contains("removed").and(contains("CLAUDE.md")));
    assert!(!sb.home().join(".claude/CLAUDE.md").exists());
    assert_eq!(sb.read("outside/config.toml"), "INNOCENT\n");
}

#[test]
fn undo_refuses_when_the_same_content_sits_behind_a_new_link() {
    // Without an edit there is no drift, so only the path check can catch it.
    let sb = Sandbox::empty();
    fs::create_dir_all(sb.path("p")).unwrap();
    fs::write(sb.path("p/kitbag.toml"), SERVER).unwrap();
    fs::create_dir_all(sb.path("real")).unwrap();
    symlink(sb.path("real"), sb.home().join(".codex")).unwrap();
    sb.cli()
        .args(["apply", "p", "--to", "codex", "--yes"])
        .assert()
        .success();
    fs::rename(sb.path("real"), sb.path("outside")).unwrap();
    symlink(sb.path("outside"), sb.path("real")).unwrap();
    sb.cli()
        .arg("undo")
        .assert()
        .code(1)
        .stdout(contains("symbolic link"));
    assert!(sb.path("outside/config.toml").exists());
}

#[test]
fn init_force_replaces_links_instead_of_writing_through_them() {
    let sb = Sandbox::empty();
    fs::create_dir_all(sb.path("prof/rules")).unwrap();
    fs::create_dir_all(sb.path("outside/cmds")).unwrap();
    fs::write(sb.path("prof/kitbag.toml"), "kitbag = 1\nname = \"old\"\n").unwrap();
    fs::write(sb.path("outside/victim.md"), "ORIGINAL\n").unwrap();
    fs::write(sb.path("outside/cmds/review.md"), "MINE\n").unwrap();
    symlink(
        sb.path("outside/victim.md"),
        sb.path("prof/rules/example.md"),
    )
    .unwrap();
    symlink(sb.path("outside/cmds"), sb.path("prof/commands")).unwrap();
    symlink(
        sb.path("outside/nothing.md"),
        sb.path("prof/instructions.md"),
    )
    .unwrap();
    sb.cli()
        .args(["init", "prof", "--force", "--name", "audit"])
        .assert()
        .success();
    assert_eq!(sb.read("outside/victim.md"), "ORIGINAL\n");
    assert_eq!(sb.read("outside/cmds/review.md"), "MINE\n");
    assert!(!sb.path("outside/nothing.md").exists());
    for rel in [
        "prof/rules/example.md",
        "prof/commands",
        "prof/instructions.md",
    ] {
        let m = fs::symlink_metadata(sb.path(rel)).unwrap();
        assert!(!m.file_type().is_symlink(), "{rel} is still a link");
    }
    assert!(sb.read("prof/rules/example.md").contains("Example rule"));
    // The result is a valid profile.
    sb.cli()
        .args(["diff", "prof", "--to", "codex"])
        .assert()
        .success();
}

#[test]
fn init_without_force_does_not_follow_a_dangling_link() {
    let sb = Sandbox::empty();
    fs::create_dir_all(sb.path("prof")).unwrap();
    symlink(sb.path("outside.md"), sb.path("prof/instructions.md")).unwrap();
    sb.cli()
        .args(["init", "prof"])
        .assert()
        .code(2)
        .stderr(contains("--force"));
    assert!(!sb.path("outside.md").exists());
}

#[test]
fn capture_force_never_writes_through_links() {
    let sb = Sandbox::empty();
    fs::create_dir_all(sb.home().join(".codex")).unwrap();
    fs::write(sb.home().join(".codex/AGENTS.md"), "Be brief.\n").unwrap();
    sb.cli()
        .args(["capture", "--from", "codex", "-o", "prof"])
        .assert()
        .success();
    fs::write(sb.path("victim.md"), "ORIGINAL\n").unwrap();
    fs::remove_file(sb.path("prof/instructions.md")).unwrap();
    symlink(sb.path("victim.md"), sb.path("prof/instructions.md")).unwrap();
    // Refused (the old profile can't be read) or replaced: never written through.
    let _ = sb
        .cli()
        .args(["capture", "--from", "codex", "-o", "prof", "--force"])
        .output()
        .unwrap();
    assert_eq!(sb.read("victim.md"), "ORIGINAL\n");
}

/// With `--project`, a directory in the project swapped for a link to
/// somewhere else between planning and writing (while the plan waits for
/// confirmation) stops the apply before anything is written.
#[test]
fn project_apply_rechecks_confinement_when_it_writes() {
    use kitbag::adapters::Scope;
    let sb = Sandbox::empty();
    let profile = {
        let mut p = kitbag::model::Profile::new("p");
        p.instructions = Some("Be brief.\n".into());
        p.rules.push(kitbag::model::Rule {
            name: "style".into(),
            description: Some("Style".into()),
            body: "Use tabs.\n".into(),
            ..Default::default()
        });
        p
    };
    let env = sb.env(Scope::Project);
    let plan = plan(&profile, &["cursor"], &env);
    assert!(plan.confine_to.is_some());
    let rule = plan
        .changes
        .iter()
        .find(|c| c.path.starts_with(sb.project().join(".cursor")))
        .expect("a cursor rule in the project")
        .path
        .clone();

    // The plan was made; now `.cursor` becomes a link out of the project.
    fs::create_dir_all(sb.path("outside")).unwrap();
    symlink(sb.path("outside"), sb.project().join(".cursor")).unwrap();
    let e = kitbag::engine::commit(&plan, "test", &sb.state())
        .unwrap_err()
        .to_string();
    assert!(
        e.contains("outside") && e.contains("nothing was written"),
        "{e}"
    );
    assert!(!rule.exists());
    assert_eq!(fs::read_dir(sb.path("outside")).unwrap().count(), 0);
    assert!(!sb.project().join("AGENTS.md").exists());

    // Without the link the same plan still applies.
    fs::remove_file(sb.project().join(".cursor")).unwrap();
    kitbag::engine::commit(&plan, "test", &sb.state())
        .unwrap()
        .unwrap();
    assert!(rule.exists());
}

#[test]
fn copilot_agents_are_not_read_or_written_through_a_link_out_of_the_project() {
    use kitbag::adapters::Scope;
    let sb = Sandbox::empty();
    fs::create_dir_all(sb.path("outside")).unwrap();
    fs::write(sb.path("outside/leak.agent.md"), "Outside.\n").unwrap();
    fs::create_dir_all(sb.project().join(".github")).unwrap();
    symlink(sb.path("outside"), sb.project().join(".github/agents")).unwrap();
    let r = capture(&["copilot"], &sb.env(Scope::Project));
    assert!(r.profile.agents.is_empty());

    let mut p = kitbag::model::Profile::new("p");
    p.agents.push(kitbag::model::Agent {
        name: "reviewer".into(),
        description: Some("Reviews".into()),
        extra: Default::default(),
        body: "Review.".into(),
    });
    let env = sb.env(Scope::Project);
    let engine_opts = opts(&p);
    let planned = kitbag::engine::plan(&p, &targets(&["copilot"]), &env, &engine_opts);
    let e = format!("{:#}", planned.expect_err("the plan refuses the link"));
    assert!(
        e.contains(".github") && e.contains("agents") && e.contains("refusing to write through it"),
        "{e}"
    );
    assert!(!sb.path("outside/reviewer.agent.md").exists());
}
