//! `kitbag enable` / `kitbag disable`: they edit `targets` / `disabled_on` in a
//! local profile, apply nothing, and what they write is what `apply` already
//! understands.

mod common;

use std::fs;

use common::*;
use predicates::prelude::*;
use predicates::str::contains;

const SKILL: &str = "---\nname: review\ndescription: Review code\n---\n\nReview it.\n";
const MANIFEST: &str = "kitbag = 1\nname = \"p\"\n\n# The docs server\n[mcp_servers.docs]\ncommand = \"docs-mcp\" # runs locally\n";

fn profile(sb: &Sandbox) {
    fs::create_dir_all(sb.path("p/skills/review")).unwrap();
    fs::create_dir_all(sb.path("p/rules")).unwrap();
    fs::create_dir_all(sb.path("p/commands")).unwrap();
    fs::create_dir_all(sb.path("p/agents")).unwrap();
    fs::write(sb.path("p/kitbag.toml"), MANIFEST).unwrap();
    fs::write(sb.path("p/skills/review/SKILL.md"), SKILL).unwrap();
    fs::write(
        sb.path("p/commands/ship.md"),
        "---\ndescription: Ship\n---\n\nShip it.\n",
    )
    .unwrap();
    fs::write(
        sb.path("p/agents/helper.md"),
        "---\ndescription: Help\n---\n\nHelp.\n",
    )
    .unwrap();
}

fn run(sb: &Sandbox, args: &[&str]) -> (bool, String, String) {
    let out = sb.cli().args(args).output().unwrap();
    (
        out.status.success(),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

fn ok(sb: &Sandbox, args: &[&str]) -> String {
    let (good, out, err) = run(sb, args);
    assert!(good, "{args:?}\n{out}{err}");
    out
}

fn fails(sb: &Sandbox, args: &[&str]) -> String {
    let (good, out, err) = run(sb, args);
    assert!(!good, "{args:?} should fail\n{out}{err}");
    err
}

#[test]
fn disable_a_skill_on_one_tool_then_enable_it() {
    let sb = Sandbox::empty();
    profile(&sb);
    let out = ok(&sb, &["disable", "review", "p", "--on", "claude"]);
    assert!(
        out.contains("Disabled skill 'review': off on claude-code"),
        "{out}"
    );
    assert!(out.contains("Nothing was applied"), "{out}");
    assert_eq!(
        sb.read("p/skills/review/SKILL.md"),
        "---\nname: review\ndescription: Review code\ndisabled_on:\n  - claude-code\n---\n\nReview it.\n"
    );
    // Nothing reached a tool.
    assert!(!sb.path("home/.claude").exists());

    // Apply does what it already does for the field.
    let out = ok(&sb, &["apply", "p", "--to", "claude-code,cursor", "--yes"]);
    assert!(
        out.contains("skill 'review' skipped on claude-code"),
        "{out}"
    );
    assert!(!sb.path("home/.claude/skills/review").exists());
    assert!(sb.path("home/.cursor/skills/review/SKILL.md").exists());

    let out = ok(&sb, &["enable", "skill:review", "p", "--on", "claude-code"]);
    assert!(
        out.contains("Enabled skill 'review': on every tool"),
        "{out}"
    );
    assert_eq!(sb.read("p/skills/review/SKILL.md"), SKILL);
    ok(&sb, &["apply", "p", "--to", "claude-code,cursor", "--yes"]);
    assert!(sb.path("home/.claude/skills/review/SKILL.md").exists());
}

#[test]
fn disable_an_mcp_server_keeps_the_manifest_as_it_was() {
    let sb = Sandbox::empty();
    profile(&sb);
    ok(&sb, &["disable", "docs", "p", "--on", "cursor,codex"]);
    let text = sb.read("p/kitbag.toml");
    assert!(text.contains("# The docs server\n"), "{text}");
    assert!(
        text.contains("command = \"docs-mcp\" # runs locally"),
        "{text}"
    );
    assert!(
        text.contains("disabled_on = [\"cursor\", \"codex\"]"),
        "{text}"
    );

    let out = ok(&sb, &["apply", "p", "--to", "claude-code,cursor", "--yes"]);
    assert!(out.contains("MCP server 'docs' skipped on cursor"), "{out}");
    assert!(sb.read("home/.claude.json").contains("docs-mcp"));
    assert!(!sb.path("home/.cursor/mcp.json").exists());

    ok(&sb, &["enable", "mcp:docs", "p", "--on", "cursor"]);
    assert!(
        sb.read("p/kitbag.toml")
            .contains("disabled_on = [\"codex\"]")
    );
    ok(&sb, &["enable", "docs", "p", "--on", "codex"]);
    assert_eq!(sb.read("p/kitbag.toml"), MANIFEST);
    ok(&sb, &["apply", "p", "--to", "claude-code,cursor", "--yes"]);
    assert!(sb.read("home/.cursor/mcp.json").contains("docs-mcp"));
}

#[test]
fn disable_everywhere_and_enable_everywhere() {
    for (spec, file) in [
        ("command:ship", "p/commands/ship.md"),
        ("agent:helper", "p/agents/helper.md"),
        ("docs", "p/kitbag.toml"),
    ] {
        let sb = Sandbox::empty();
        profile(&sb);
        let original = sb.read(file);
        ok(&sb, &["apply", "p", "--to", "claude-code", "--yes"]);
        let out = ok(&sb, &["disable", spec, "p"]);
        assert!(out.contains("off on every tool"), "{spec}: {out}");
        let off = sb.read(file);
        for id in kitbag::adapters::ids() {
            assert!(off.contains(id), "{spec}: {id} missing from {off}");
        }
        // Apply removes what it wrote, and undo puts it back.
        let out = ok(&sb, &["apply", "p", "--to", "claude-code", "--yes"]);
        assert!(out.contains("skipped on claude-code"), "{spec}: {out}");
        ok(&sb, &["undo"]);
        ok(&sb, &["enable", spec, "p"]);
        assert_eq!(sb.read(file), original, "{spec}");
    }
}

#[test]
fn enabling_where_an_allow_list_leaves_it_out_adds_it() {
    let sb = Sandbox::empty();
    profile(&sb);
    fs::write(
        sb.path("p/commands/ship.md"),
        "---\ndescription: Ship\nonly:\n  - claude\n---\n\nShip it.\n",
    )
    .unwrap();
    ok(&sb, &["enable", "ship", "p", "--on", "codex"]);
    let text = sb.read("p/commands/ship.md");
    assert!(!text.contains("only"), "{text}");
    assert!(
        text.contains("targets:\n  - claude-code\n  - codex\n"),
        "{text}"
    );
    // Disabling the last one out of an allow-list turns it off everywhere.
    ok(&sb, &["disable", "ship", "p", "--on", "claude-code,codex"]);
    let text = sb.read("p/commands/ship.md");
    assert!(!text.contains("targets"), "{text}");
    assert!(text.contains("disabled_on:\n  - claude-code\n"), "{text}");
    let out = ok(&sb, &["diff", "p", "--to", "claude-code,cursor"]);
    assert!(out.contains("command 'ship' skipped on cursor"), "{out}");
}

#[test]
fn nothing_to_change_leaves_the_file_alone() {
    let sb = Sandbox::empty();
    profile(&sb);
    let out = ok(&sb, &["enable", "review", "p", "--on", "codex"]);
    assert!(out.contains("already on every tool"), "{out}");
    ok(&sb, &["disable", "review", "p", "--on", "codex"]);
    let out = ok(&sb, &["disable", "review", "p", "--on", "codex"]);
    assert!(out.contains("already off on codex"), "{out}");
    assert_eq!(
        sb.read("p/skills/review/SKILL.md").matches("codex").count(),
        1
    );
}

#[test]
fn ambiguous_unknown_and_bad_arguments_are_errors() {
    let sb = Sandbox::empty();
    profile(&sb);
    fs::write(
        sb.path("p/rules/review.md"),
        "---\ndescription: R\n---\n\nRule.\n",
    )
    .unwrap();
    let before = sb.read("p/skills/review/SKILL.md");

    let err = fails(&sb, &["disable", "review", "p"]);
    assert!(
        err.contains("rule:review, skill:review") && err.contains("Say which"),
        "{err}"
    );
    // A prefix picks one.
    ok(&sb, &["disable", "rule:review", "p", "--on", "codex"]);
    assert_eq!(sb.read("p/skills/review/SKILL.md"), before);
    assert!(sb.read("p/rules/review.md").contains("disabled_on"));

    let err = fails(&sb, &["disable", "nothing", "p"]);
    assert!(
        err.contains("no MCP server, rule, command, skill or agent 'nothing'"),
        "{err}"
    );
    assert!(err.contains("skill:review"), "{err}");
    let err = fails(&sb, &["disable", "agent:review", "p"]);
    assert!(err.contains("has no agent 'review'"), "{err}");

    let err = fails(&sb, &["disable", "docs", "p", "--on", "emacs"]);
    assert!(
        err.contains("unknown target 'emacs'") && err.contains("known targets"),
        "{err}"
    );
    assert_eq!(sb.read("p/kitbag.toml"), MANIFEST);

    let err = fails(&sb, &["disable", "hook:fmt", "p"]);
    assert!(
        err.contains("hooks can't be enabled or disabled per tool"),
        "{err}"
    );
}

#[test]
fn a_git_source_or_a_missing_directory_is_refused() {
    let sb = Sandbox::empty();
    for src in ["github:you/dotagents", "https://example.com/a/b.git"] {
        let err = fails(&sb, &["disable", "docs", src]);
        assert!(err.contains("clone or copy"), "{src}: {err}");
    }
    let err = fails(&sb, &["enable", "docs", "nowhere"]);
    assert!(err.contains("is not a directory"), "{err}");
}

#[test]
fn the_profile_defaults_to_the_current_directory() {
    let sb = Sandbox::empty();
    profile(&sb);
    sb.cli()
        .current_dir(sb.path("p"))
        .args(["disable", "ship", "--on", "cursor"])
        .assert()
        .success()
        .stdout(contains("Disabled command 'ship': off on cursor"));
    assert!(sb.read("p/commands/ship.md").contains("disabled_on"));
}

#[test]
fn a_broken_profile_is_not_edited() {
    let sb = Sandbox::empty();
    profile(&sb);
    fs::write(
        sb.path("p/commands/ship.md"),
        "---\ndescription: Ship\ntargets: [codex]\ndisabled_on: [cursor]\n---\n\nShip.\n",
    )
    .unwrap();
    let before = sb.read("p/commands/ship.md");
    let err = fails(&sb, &["disable", "review", "p"]);
    assert!(err.contains("set either 'targets'"), "{err}");
    assert_eq!(sb.read("p/commands/ship.md"), before);
    sb.cli()
        .args(["enable", "review", "p"])
        .assert()
        .failure()
        .stderr(contains("not a valid kitbag profile").or(contains("set either")));
}
