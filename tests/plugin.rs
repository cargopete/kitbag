//! Plugin sources: a directory with `rules/<name>.md` at its root and no
//! `kitbag.toml`. The six rules in `fixtures/scoped_rules` follow the
//! `scoped_rules` fixtures of cardene777/agent-shared (MIT, see the NOTICE
//! there) and agentplugins/agent-plugins-spec discussion #100.

mod common;

use std::fs;

use common::*;
use predicates::prelude::PredicateBooleanExt;
use predicates::str::contains;

const USER_AGENTS: &str =
    "# My notes\r\nKeep this line exactly.\n\nAnd this one, no newline at the end";
const USER_CLAUDE: &str = "My own Claude instructions.\n";

/// A sandbox with the plugin copied to `plugin/` and the user's own lines in
/// `~/.codex/AGENTS.md` and `~/.claude/CLAUDE.md`.
fn plugin_sandbox() -> Sandbox {
    let sb = Sandbox::empty();
    copy_dir(
        &fixtures().join("scoped_rules/rules"),
        &sb.path("plugin/rules"),
    );
    fs::create_dir_all(sb.home().join(".codex")).unwrap();
    fs::create_dir_all(sb.home().join(".claude")).unwrap();
    fs::write(sb.home().join(".codex/AGENTS.md"), USER_AGENTS).unwrap();
    fs::write(sb.home().join(".claude/CLAUDE.md"), USER_CLAUDE).unwrap();
    sb
}

fn frontmatter_keys(text: &str) -> Vec<String> {
    let Some(rest) = text.strip_prefix("---\n") else {
        return Vec::new();
    };
    let end = rest.find("\n---\n").unwrap();
    rest[..end]
        .lines()
        .filter(|l| !l.starts_with(' ') && l.contains(':'))
        .map(|l| l.split(':').next().unwrap().to_string())
        .collect()
}

#[test]
fn the_six_scoped_rules_load_as_a_plugin() {
    let dir = fixtures().join("scoped_rules");
    assert!(!dir.join("kitbag.toml").exists());
    let p = kitbag::profile_io::load(&dir).unwrap();
    assert_eq!(p.name, "scoped_rules");
    assert!(p.mcp_servers.is_empty() && p.settings.is_empty() && p.instructions.is_none());
    let names: Vec<&str> = p.rules.iter().map(|r| r.name.as_str()).collect();
    assert_eq!(
        names,
        [
            "always",
            "by-apply-to",
            "by-description",
            "by-globs",
            "by-paths",
            "plain"
        ]
    );
    let rule = |n: &str| p.rules.iter().find(|r| r.name == n).unwrap();
    let want = ["src/**/*.ts", "test/**/*.ts"];
    for n in ["by-globs", "by-paths", "by-apply-to"] {
        assert_eq!(rule(n).globs, want, "{n}");
        assert_eq!(rule(n).always_apply, None, "{n}");
    }
    assert_eq!(rule("always").always_apply, Some(true));
    assert_eq!(rule("by-description").always_apply, Some(false));
    assert!(rule("by-description").globs.is_empty());
    assert!(rule("plain").globs.is_empty() && rule("plain").always_apply.is_none());
    assert_eq!(rule("plain").body, "# Plain\nRule plain.");
}

#[test]
fn apply_a_plugin_then_again_then_undo_without_it() {
    let sb = plugin_sandbox();
    sb.cli()
        .args([
            "apply",
            "plugin",
            "--to",
            "claude-code,codex,gemini-cli",
            "--yes",
        ])
        .assert()
        .success()
        .stdout(contains("Profile 'plugin'").and(contains("Wrote")))
        .stderr(predicates::str::is_empty());
    assert!(
        !sb.path("plugin/kitbag.toml").exists(),
        "nothing is written into the plugin"
    );

    // Claude Code: scoped rules get `paths:`, the rest none.
    let rules = sb.home().join(".claude/rules");
    for n in ["by-globs", "by-paths", "by-apply-to"] {
        let text = fs::read_to_string(rules.join(format!("{n}.md"))).unwrap();
        assert_eq!(frontmatter_keys(&text), ["paths"], "{n}: {text}");
        assert!(
            text.contains("  - \"src/**/*.ts\"\n  - \"test/**/*.ts\"\n"),
            "{n}: {text}"
        );
    }
    for n in ["always", "plain"] {
        let text = fs::read_to_string(rules.join(format!("{n}.md"))).unwrap();
        assert!(frontmatter_keys(&text).is_empty(), "{n}: {text}");
    }
    let described = fs::read_to_string(rules.join("by-description.md")).unwrap();
    assert_eq!(frontmatter_keys(&described), ["description"], "{described}");
    // Instructions the user wrote are not touched; the plugin has none.
    assert_eq!(sb.read("home/.claude/CLAUDE.md"), USER_CLAUDE);

    // Codex and Gemini CLI fold every rule in, always on, scoped ones labelled.
    for file in ["home/.codex/AGENTS.md", "home/.gemini/GEMINI.md"] {
        let text = sb.read(file);
        let section = |n: &str| {
            let start = text.find(&format!("\"name\":\"{n}\"")).unwrap();
            let end = start + text[start..].find("kitbag:rule-end").unwrap();
            text[start..end].to_string()
        };
        let label = "_Applies when working with files matching: `src/**/*.ts`, `test/**/*.ts`_";
        for n in ["by-globs", "by-paths", "by-apply-to"] {
            assert!(section(n).contains(label), "{file} {n}: {text}");
        }
        for n in ["always", "plain"] {
            assert!(!section(n).contains("_Applies when"), "{file} {n}: {text}");
            assert!(!section(n).contains("_Use when"), "{file} {n}: {text}");
        }
        let d = section("by-description");
        assert!(
            d.contains("## Use when writing tests.")
                && d.contains("_Use when relevant to the task._"),
            "{file}: {d}"
        );
        assert!(text.contains("Rule plain.") && text.contains("Rule always."));
    }
    let agents = sb.read("home/.codex/AGENTS.md");
    assert!(agents.starts_with(USER_AGENTS), "{agents:?}");
    assert!(agents.contains("<!-- kitbag:begin plugin:rules -->"));

    // A second apply has nothing to do.
    sb.cli()
        .args([
            "apply",
            "plugin",
            "--to",
            "claude-code,codex,gemini-cli",
            "--yes",
        ])
        .assert()
        .success()
        .stdout(contains("Up to date").and(contains("Wrote").not()));
    sb.cli()
        .args([
            "diff",
            "plugin",
            "--to",
            "claude-code,codex,gemini-cli",
            "--exit-code",
        ])
        .assert()
        .success();
    sb.cli()
        .args(["status", "plugin", "--to", "codex"])
        .assert()
        .success()
        .stdout(contains("in sync"));

    // Without the plugin directory, undo still takes its rules out and leaves
    // the user's own lines byte for byte.
    fs::remove_dir_all(sb.path("plugin")).unwrap();
    sb.cli().args(["undo"]).assert().success();
    assert_eq!(
        fs::read(sb.home().join(".codex/AGENTS.md")).unwrap(),
        USER_AGENTS.as_bytes()
    );
    assert_eq!(sb.read("home/.claude/CLAUDE.md"), USER_CLAUDE);
    assert!(!sb.home().join(".gemini/GEMINI.md").exists());
    assert!(fs::read_dir(&rules).map_or(true, |mut d| d.next().is_none()));
}

#[test]
fn undo_keeps_instructions_from_a_profile_applied_before() {
    // The user's own profile writes instructions; a plugin applied on top
    // adds only its rules block, and undoing it leaves the profile's block.
    let sb = plugin_sandbox();
    sb.cli().args(["init", "mine"]).assert().success();
    sb.cli()
        .args(["apply", "mine", "--to", "codex", "--yes"])
        .assert()
        .success();
    let before = sb.read("home/.codex/AGENTS.md");
    assert!(before.contains("kitbag:begin mine"));
    sb.cli()
        .args(["apply", "plugin", "--to", "codex", "--yes"])
        .assert()
        .success();
    let after = sb.read("home/.codex/AGENTS.md");
    assert!(after.starts_with(&before) && after.contains("kitbag:begin plugin:rules"));
    sb.cli().args(["undo"]).assert().success();
    assert_eq!(sb.read("home/.codex/AGENTS.md"), before);
}

#[test]
fn cursor_gets_mdc_rules_with_globs() {
    let sb = plugin_sandbox();
    sb.cli()
        .args([
            "apply",
            "plugin",
            "--project",
            "project",
            "--to",
            "cursor",
            "--yes",
        ])
        .assert()
        .success();
    let rule = |n: &str| sb.read(&format!("project/.cursor/rules/{n}.mdc"));
    for n in ["by-globs", "by-paths", "by-apply-to"] {
        let text = rule(n);
        assert!(
            text.contains("\nglobs: src/**/*.ts,test/**/*.ts\n"),
            "{n}: {text}"
        );
        assert!(text.contains("\nalwaysApply: false\n"), "{n}: {text}");
    }
    for n in ["always", "plain"] {
        assert!(rule(n).contains("\nalwaysApply: true\n"), "{n}");
    }
    let d = rule("by-description");
    assert!(d.contains("description: Use when writing tests.") && d.contains("alwaysApply: false"));
    sb.cli()
        .args([
            "apply",
            "plugin",
            "--project",
            "project",
            "--to",
            "cursor",
            "--yes",
        ])
        .assert()
        .success()
        .stdout(contains("Up to date"));
}

#[test]
fn plugin_names_come_from_the_manifest() {
    let sb = plugin_sandbox();
    fs::create_dir_all(sb.path("plugin/.claude-plugin")).unwrap();
    fs::write(
        sb.path("plugin/.claude-plugin/plugin.json"),
        r#"{"name": "scoped", "version": "1.0.0"}"#,
    )
    .unwrap();
    sb.cli()
        .args(["apply", "plugin", "--to", "codex", "--yes"])
        .assert()
        .success()
        .stdout(contains("Profile 'scoped'"));
    assert!(
        sb.read("home/.codex/AGENTS.md")
            .contains("kitbag:begin scoped:rules")
    );
}

#[test]
fn doctor_and_sync_take_a_plugin() {
    let sb = plugin_sandbox();
    sb.cli()
        .args(["doctor", "plugin", "--to", "codex"])
        .assert()
        .success()
        .stdout(contains("'plugin' from").and(contains("6 rule(s)")));
    sb.cli()
        .args(["sync", "plugin", "--to", "codex", "--yes"])
        .assert()
        .success()
        .stdout(contains("Wrote"));
    // Edits can't be pulled back into a plugin: that would write a profile.
    let edited = sb
        .read("home/.codex/AGENTS.md")
        .replace("Rule plain.", "Rule edited.");
    fs::write(sb.home().join(".codex/AGENTS.md"), edited).unwrap();
    sb.cli()
        .args([
            "sync", "plugin", "--to", "codex", "--from", "codex", "--yes",
        ])
        .assert()
        .code(2)
        .stderr(contains("plugin"));
    assert!(!sb.path("plugin/kitbag.toml").exists());
}

#[test]
fn a_bare_allow_git_mcp_does_not_swallow_a_plugin() {
    let sb = plugin_sandbox();
    sb.cli()
        .args([
            "apply",
            "--allow-git-mcp",
            "plugin",
            "--to",
            "codex",
            "--yes",
        ])
        .assert()
        .success()
        .stdout(contains("Profile 'plugin'"));
}

#[test]
fn neither_a_profile_nor_a_plugin_fails_clearly() {
    let sb = Sandbox::empty();
    fs::create_dir_all(sb.path("empty/rules")).unwrap();
    sb.cli()
        .args(["apply", "empty", "--to", "codex", "--yes"])
        .assert()
        .code(2)
        .stderr(contains("kitbag init").and(contains("rules/")));
}

#[test]
fn plugins_from_git() {
    if !has_git() {
        return;
    }
    let sb = Sandbox::empty();
    let files: Vec<(String, String)> = fs::read_dir(fixtures().join("scoped_rules/rules"))
        .unwrap()
        .map(|e| {
            let e = e.unwrap();
            (
                format!("plugins/scoped/rules/{}", e.file_name().to_string_lossy()),
                fs::read_to_string(e.path()).unwrap(),
            )
        })
        .collect();
    let refs: Vec<(&str, &str)> = files
        .iter()
        .map(|(a, b)| (a.as_str(), b.as_str()))
        .collect();
    let url = sb.git_profile("repo", &refs);
    let src = format!("{url}//plugins/scoped");
    sb.cli()
        .args(["apply", &src, "--to", "codex", "--yes"])
        .assert()
        .success()
        .stdout(contains("Profile 'scoped'").and(contains("from file://")));
    assert!(
        sb.read("home/.codex/AGENTS.md")
            .contains("kitbag:begin scoped:rules")
    );
    sb.cli()
        .args(["apply", &src, "--to", "codex", "--yes"])
        .assert()
        .success()
        .stdout(contains("Up to date"));

    // Without a sub-directory the plugin is named after the repository, not
    // the temporary clone.
    let root = sb.git_profile("my-plugin", &[("rules/r.md", "Rule r.\n")]);
    sb.cli()
        .args(["apply", &root, "--to", "codex", "--yes"])
        .assert()
        .success()
        .stdout(contains("Profile 'my-plugin'"));

    // A plugin from git is still a git source for kitbag's policy.
    sb.cli()
        .args(["sync", &root, "--to", "codex", "--from", "codex", "--yes"])
        .assert()
        .code(2)
        .stderr(contains("git source"));
    // A directory with neither a kitbag.toml nor rules/ is refused.
    sb.cli()
        .args(["diff", &url, "--to", "codex"])
        .assert()
        .code(2)
        .stderr(contains("no kitbag.toml").and(contains("rules/")));
}
