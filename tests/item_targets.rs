//! Per-tool targeting of rules, commands, skills and agents (`targets` /
//! `disabled_on` in their frontmatter): they are only applied where the
//! profile enables them, and one kitbag applied to a tool earlier is removed
//! from it once the profile disables it there.

mod common;

use std::fs;

use common::*;
use predicates::prelude::*;
use predicates::str::contains;

/// One item type, and where it lands in Claude Code and in a second tool.
struct Kind {
    /// `rule`, `command`, `skill` or `agent`.
    name: &'static str,
    /// The item's file in the profile.
    file: &'static str,
    /// Where Claude Code gets it (a skill: its SKILL.md).
    claude: &'static str,
    /// The second tool and where it would get it; `None`: folded into a file.
    other: &'static str,
    other_file: &'static str,
}

const KINDS: [Kind; 4] = [
    Kind {
        name: "rule",
        file: "rules/x.md",
        claude: "home/.claude/rules/x.md",
        other: "codex",
        other_file: "home/.codex/AGENTS.md",
    },
    Kind {
        name: "command",
        file: "commands/x.md",
        claude: "home/.claude/commands/x.md",
        other: "cursor",
        other_file: "home/.cursor/commands/x.md",
    },
    Kind {
        name: "skill",
        file: "skills/x/SKILL.md",
        claude: "home/.claude/skills/x/SKILL.md",
        other: "cursor",
        other_file: "home/.cursor/skills/x/SKILL.md",
    },
    Kind {
        name: "agent",
        file: "agents/x.md",
        claude: "home/.claude/agents/x.md",
        other: "copilot",
        other_file: "home/.copilot/agents/x.agent.md",
    },
];

impl Kind {
    /// The item's markdown, with `extra` frontmatter lines.
    fn text(&self, extra: &str) -> String {
        let name = if self.name == "skill" {
            "name: x\n"
        } else {
            ""
        };
        format!(
            "---\n{name}description: The {}\n{extra}---\n\nDo the {} thing.\n",
            self.name, self.name
        )
    }

    fn write(&self, sb: &Sandbox, dir: &str, name: &str, extra: &str) {
        let manifest = sb.path(&format!("{dir}/kitbag.toml"));
        fs::create_dir_all(sb.path(dir)).unwrap();
        fs::write(manifest, format!("kitbag = 1\nname = \"{name}\"\n")).unwrap();
        let file = sb.path(&format!("{dir}/{}", self.file));
        fs::create_dir_all(file.parent().unwrap()).unwrap();
        fs::write(file, self.text(extra)).unwrap();
    }

    fn to(&self) -> String {
        format!("claude-code,{}", self.other)
    }

    fn label(&self) -> String {
        format!("{} x", self.name)
    }
}

fn run(sb: &Sandbox, args: &[&str]) -> (bool, String, String) {
    let out = sb.cli().args(args).output().unwrap();
    (
        out.status.success(),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

fn apply(sb: &Sandbox, src: &str, to: &str) -> String {
    let (ok, out, err) = run(sb, &["apply", src, "--to", to, "--yes"]);
    assert!(ok, "{out}{err}");
    out
}

fn exists(sb: &Sandbox, rel: &str) -> bool {
    sb.path(rel).exists()
}

#[test]
fn items_are_only_applied_where_enabled() {
    for k in &KINDS {
        // The allow-list, spelled with the `claude` alias.
        let sb = Sandbox::empty();
        k.write(&sb, "p", "shared", "targets: [claude]\n");
        let out = apply(&sb, "p", &k.to());
        assert!(
            out.contains(&format!(
                "{} 'x' skipped on {}: disabled in profile (not in its 'targets')",
                k.name, k.other
            )),
            "{}: {out}",
            k.name
        );
        assert!(exists(&sb, k.claude), "{}", k.name);
        assert!(!exists(&sb, k.other_file) || !sb.read(k.other_file).contains("Do the"));
        // The tool-side copy carries no kitbag fields.
        assert!(!sb.read(k.claude).contains("targets"), "{}", k.name);

        // The deny-list.
        let sb = Sandbox::empty();
        k.write(&sb, "p", "shared", "disabled_on:\n  - claude-code\n");
        let out = apply(&sb, "p", &k.to());
        assert!(
            out.contains(&format!(
                "{} 'x' skipped on claude-code: disabled in profile (listed in 'disabled_on')",
                k.name
            )),
            "{}: {out}",
            k.name
        );
        assert!(!exists(&sb, k.claude), "{}", k.name);
        assert!(exists(&sb, k.other_file), "{}", k.name);
        assert!(!sb.read(k.other_file).contains("disabled_on"));
    }
}

#[test]
fn bad_targeting_is_an_error_and_unknown_targets_a_warning() {
    for k in &KINDS {
        let sb = Sandbox::empty();
        let fail = |sb: &Sandbox, extra: &str, want: &str| {
            k.write(sb, "p", "shared", extra);
            let (ok, out, err) = run(sb, &["diff", "p", "--to", "claude-code"]);
            assert!(!ok, "{}: {out}", k.name);
            assert!(err.contains(want), "{}: {err}", k.name);
        };
        fail(
            &sb,
            "targets: [cursor]\ndisabled_on: [codex]\n",
            "set either 'targets'",
        );
        fail(
            &sb,
            "only: [cursor]\ntargets: [cursor]\n",
            "both 'only' and 'targets' are set",
        );
        fail(
            &sb,
            "except: [cursor]\ndisabled_on: [codex]\n",
            "both 'except' and 'disabled_on' are set",
        );
        fail(&sb, "targets: []\n", "'targets' is empty");
        fail(
            &sb,
            "targets: 3\n",
            "'targets' must be a list of tool names",
        );

        // An unknown tool only warns, and matches nothing.
        k.write(&sb, "p", "shared", "disabled_on: [vscode]\n");
        let (ok, _, err) = run(&sb, &["apply", "p", "--to", "claude-code", "--yes"]);
        assert!(ok, "{err}");
        assert!(
            err.contains("unknown target 'vscode' in 'disabled_on' is ignored"),
            "{}: {err}",
            k.name
        );
        assert!(exists(&sb, k.claude));
    }
}

#[test]
fn older_names_and_duplicates_load() {
    for k in &KINDS {
        let sb = Sandbox::empty();
        k.write(&sb, "p", "shared", "only:\n  - claude\n  - claude-code\n");
        let out = apply(&sb, "p", &k.to());
        assert!(
            out.contains(&format!("{} 'x' skipped on {}", k.name, k.other)),
            "{}: {out}",
            k.name
        );
        assert!(exists(&sb, k.claude));
        let sb = Sandbox::empty();
        k.write(&sb, "p", "shared", "except: [claude, claude-code]\n");
        apply(&sb, "p", &k.to());
        assert!(!exists(&sb, k.claude), "{}", k.name);
    }
}

#[test]
fn disabling_removes_what_kitbag_applied_and_undo_restores_it() {
    for k in &KINDS {
        let sb = Sandbox::empty();
        k.write(&sb, "p", "shared", "");
        apply(&sb, "p", &k.to());
        assert!(exists(&sb, k.claude));
        let before = sb.read(k.claude);
        let other_before = sb.read(k.other_file);

        k.write(&sb, "p", "shared", "disabled_on: [claude-code]\n");
        sb.cli()
            .args(["diff", "p", "--to", "claude-code"])
            .assert()
            .success()
            .stdout(
                contains("Rules, commands, skills and agents removed")
                    .and(contains(format!("{}  (claude-code)", k.label()))),
            );
        sb.cli()
            .args(["status", "p", "--to", "claude-code"])
            .assert()
            .success()
            .stdout(contains(k.label()).and(contains("apply removes it")));
        // A dry run changes nothing.
        assert!(exists(&sb, k.claude));

        let out = apply(&sb, "p", &k.to());
        assert!(
            out.contains(&format!("{} 'x' removed from", k.name)),
            "{}: {out}",
            k.name
        );
        assert!(!exists(&sb, k.claude), "{}", k.name);
        // The other tool is untouched.
        assert_eq!(sb.read(k.other_file), other_before);
        if k.name == "skill" {
            assert!(!exists(&sb, "home/.claude/skills/x"), "empty skill folder");
        }

        // Applying again has nothing left to do.
        let out = apply(&sb, "p", &k.to());
        assert!(out.contains("Up to date"), "{}: {out}", k.name);

        // Undo puts it back, byte for byte, and the record of the first apply stands again.
        sb.cli().arg("undo").assert().success();
        assert_eq!(sb.read(k.claude), before, "{}", k.name);
        apply(&sb, "p", &k.to());
        assert!(!exists(&sb, k.claude), "{}", k.name);

        // Enabling it again is an ordinary write.
        k.write(&sb, "p", "shared", "");
        apply(&sb, "p", &k.to());
        assert!(exists(&sb, k.claude), "{}", k.name);
    }
}

#[test]
fn edited_hand_added_and_other_profiles_items_are_left_alone() {
    for k in &KINDS {
        // Edited in the tool since kitbag wrote it.
        let sb = Sandbox::empty();
        k.write(&sb, "p", "shared", "");
        apply(&sb, "p", &k.to());
        let mut edited = sb.read(k.claude);
        edited.push_str("My own line.\n");
        fs::write(sb.path(k.claude), &edited).unwrap();
        k.write(&sb, "p", "shared", "disabled_on: [claude-code]\n");
        let out = apply(&sb, "p", &k.to());
        assert!(
            out.contains("it was edited in") && out.contains("since kitbag added it"),
            "{}: {out}",
            k.name
        );
        assert_eq!(sb.read(k.claude), edited, "{}", k.name);
        sb.cli()
            .args(["status", "p", "--to", "claude-code"])
            .assert()
            .stdout(contains(k.label()).and(contains("apply leaves it")));

        // Added by hand: kitbag has no record of it.
        let sb = Sandbox::empty();
        let mine = "---\ndescription: Mine\n---\n\nMy own.\n";
        fs::create_dir_all(sb.path(k.claude).parent().unwrap()).unwrap();
        fs::write(sb.path(k.claude), mine).unwrap();
        k.write(&sb, "p", "shared", "disabled_on: [claude-code]\n");
        let out = apply(&sb, "p", &k.to());
        assert!(
            out.contains(&format!(
                "{} 'x' is disabled for claude-code in the profile, but kitbag has no record of adding it to",
                k.name
            )),
            "{}: {out}",
            k.name
        );
        assert_eq!(sb.read(k.claude), mine, "{}", k.name);

        // Applied by another profile.
        let sb = Sandbox::empty();
        k.write(&sb, "p", "shared", "");
        apply(&sb, "p", &k.to());
        k.write(&sb, "q", "other", "disabled_on: [claude-code]\n");
        let out = apply(&sb, "q", "claude-code");
        assert!(
            out.contains("profile 'shared' added it"),
            "{}: {out}",
            k.name
        );
        assert!(exists(&sb, k.claude), "{}", k.name);

        // --no-overwrite keeps it too.
        let sb = Sandbox::empty();
        k.write(&sb, "p", "shared", "");
        apply(&sb, "p", &k.to());
        k.write(&sb, "p", "shared", "disabled_on: [claude-code]\n");
        let (ok, out, _) = run(
            &sb,
            &[
                "apply",
                "p",
                "--to",
                "claude-code",
                "--yes",
                "--no-overwrite",
            ],
        );
        assert!(ok);
        assert!(out.contains("because --no-overwrite was given"), "{out}");
        assert!(exists(&sb, k.claude), "{}", k.name);
    }
}

#[test]
fn a_skill_with_files_of_your_own_keeps_them() {
    let sb = Sandbox::empty();
    let k = &KINDS[2];
    k.write(&sb, "p", "shared", "");
    fs::write(sb.path("p/skills/x/notes.txt"), "from the profile\n").unwrap();
    apply(&sb, "p", "claude-code");
    fs::write(sb.path("home/.claude/skills/x/mine.txt"), "mine\n").unwrap();
    k.write(&sb, "p", "shared", "disabled_on: [claude-code]\n");
    let out = apply(&sb, "p", "claude-code");
    assert!(out.contains("skill 'x' removed from"), "{out}");
    assert!(!exists(&sb, "home/.claude/skills/x/SKILL.md"));
    assert!(!exists(&sb, "home/.claude/skills/x/notes.txt"));
    assert_eq!(sb.read("home/.claude/skills/x/mine.txt"), "mine\n");
    // Undo brings the skill back next to it.
    sb.cli().arg("undo").assert().success();
    assert!(exists(&sb, "home/.claude/skills/x/SKILL.md"));
    assert!(exists(&sb, "home/.claude/skills/x/notes.txt"));
}

#[test]
fn a_remote_profile_lists_the_removal_in_the_plan() {
    if !has_git() {
        return;
    }
    for k in &KINDS {
        let sb = Sandbox::empty();
        let manifest = "kitbag = 1\nname = \"shared\"\n";
        let v1 = sb.git_profile("repo", &[("kitbag.toml", manifest), (k.file, &k.text(""))]);
        apply(&sb, &v1, "claude-code");
        assert!(exists(&sb, k.claude), "{}", k.name);
        let v2 = sb.git_profile("repo", &[(k.file, &k.text("disabled_on: [claude-code]\n"))]);
        let (ok, out, err) = run(&sb, &["diff", &v2, "--to", "claude-code"]);
        assert!(ok, "{out}{err}");
        assert!(
            out.contains("Rules, commands, skills and agents removed")
                && out.contains(&k.label())
                && out.contains("delete"),
            "{}: {out}",
            k.name
        );
        assert!(exists(&sb, k.claude), "diff writes nothing");
        apply(&sb, &v2, "claude-code");
        assert!(!exists(&sb, k.claude), "{}", k.name);

        // Another source with the same profile name doesn't get to remove it.
        let sb = Sandbox::empty();
        let v1 = sb.git_profile("repo", &[("kitbag.toml", manifest), (k.file, &k.text(""))]);
        apply(&sb, &v1, "claude-code");
        k.write(&sb, "q", "shared", "disabled_on: [claude-code]\n");
        let out = apply(&sb, "q", "claude-code");
        assert!(
            out.contains("added it to") && out.contains("from"),
            "{}: {out}",
            k.name
        );
        assert!(exists(&sb, k.claude), "{}", k.name);
    }
}

#[test]
fn capture_force_keeps_targeting() {
    for k in &KINDS {
        let sb = Sandbox::empty();
        k.write(&sb, "p", "shared", "disabled_on: [cursor, codex]\n");
        apply(&sb, "p", "claude-code");
        let (ok, out, err) = run(
            &sb,
            &["capture", "--from", "claude-code", "-o", "p", "--force"],
        );
        assert!(ok, "{out}{err}");
        assert!(
            out.contains("kept 'targets' / 'disabled_on' from the existing profile for:")
                && out.contains(&k.label()),
            "{}: {out}",
            k.name
        );
        let text = sb.read(&format!("p/{}", k.file));
        assert!(
            text.contains("disabled_on:\n  - cursor\n  - codex\n"),
            "{}: {text}",
            k.name
        );
        // And the profile still loads and means the same.
        let (ok, out, _) = run(&sb, &["apply", "p", "--to", "cursor", "--yes"]);
        assert!(ok);
        assert!(!out.contains("Error"), "{out}");

        // An item kept for a tool not installed here stays unless --prune.
        let sb = Sandbox::empty();
        fs::create_dir_all(sb.path("home/.claude")).unwrap();
        fs::write(sb.path("home/.claude/CLAUDE.md"), "Be kind.\n").unwrap();
        k.write(&sb, "p", "shared", "targets: [gemini-cli]\n");
        let (ok, out, err) = run(
            &sb,
            &["capture", "--from", "claude-code", "-o", "p", "--force"],
        );
        assert!(ok, "{out}{err}");
        assert!(
            out.contains("kept rule(s), command(s)"),
            "{}: {out}",
            k.name
        );
        assert!(sb.read(&format!("p/{}", k.file)).contains("gemini-cli"));
        let (ok, out, err) = run(
            &sb,
            &[
                "capture",
                "--from",
                "claude-code",
                "-o",
                "p",
                "--force",
                "--prune",
            ],
        );
        assert!(ok, "{out}{err}");
        assert!(!sb.path(&format!("p/{}", k.file)).exists(), "{}", k.name);
    }
}

#[test]
fn items_without_targeting_are_captured_as_before() {
    let sb = Sandbox::empty();
    for k in &KINDS {
        k.write(&sb, "p", "shared", "");
    }
    apply(&sb, "p", "claude-code");
    let (ok, out, err) = run(&sb, &["capture", "--from", "claude-code", "-o", "cap"]);
    assert!(ok, "{out}{err}");
    for k in &KINDS {
        let text = sb.read(&format!("cap/{}", k.file));
        assert!(
            !text.contains("targets") && !text.contains("disabled_on"),
            "{}: {text}",
            k.name
        );
    }
    // Capturing over it again, with no targeting to keep, says nothing about keeping any.
    let (ok, out, err) = run(
        &sb,
        &["capture", "--from", "claude-code", "-o", "cap", "--force"],
    );
    assert!(ok, "{out}{err}");
    assert!(!out.contains("kept 'targets'"), "{out}");
}

#[test]
fn status_shows_items_the_profile_keeps_off_a_tool() {
    let sb = Sandbox::empty();
    let k = &KINDS[1];
    k.write(&sb, "p", "shared", "");
    apply(&sb, "p", &k.to());
    k.write(&sb, "p", "shared", "disabled_on: [cursor]\n");
    sb.cli()
        .args(["status", "p", "--to", "cursor"])
        .assert()
        .success()
        .stdout(contains("command x").and(contains("apply removes it")));
    // Where the item isn't in the tool, there is nothing to show.
    sb.cli()
        .args(["status", "p", "--to", "claude-code"])
        .assert()
        .success()
        .stdout(contains("command x").and(contains("in sync")));
}

#[test]
fn unreadable_records_are_a_warning_and_nothing_is_removed() {
    let sb = Sandbox::empty();
    let k = &KINDS[1];
    k.write(&sb, "p", "shared", "");
    apply(&sb, "p", "claude-code");
    fs::write(sb.path("state/applied-files.json"), "not json").unwrap();
    k.write(&sb, "p", "shared", "disabled_on: [claude-code]\n");
    let out = apply(&sb, "p", "claude-code");
    assert!(out.contains("could not read"), "{out}");
    assert!(out.contains("applied-files.json.bak"), "{out}");
    assert!(exists(&sb, k.claude));
}
