//! `kitbag doctor --why DIR`: which instruction files each tool loads (#44).

mod common;

use std::fs;
use std::path::Path;

use common::Sandbox;
use predicates::prelude::*;
use predicates::str::contains;

/// A `~/...` display path with the platform's separator.
fn shown(path: &str) -> String {
    path.replace('/', std::path::MAIN_SEPARATOR_STR)
}

fn write(path: &Path, content: &str) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, content).unwrap();
}

/// `doctor --why DIR --to TO` in the sandbox: its output, after checking it succeeded.
fn why(sb: &Sandbox, dir: &Path, to: &str) -> String {
    let out = sb
        .cli()
        .args(["doctor", "--why"])
        .arg(dir)
        .args(["--to", to])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8(out.stdout).unwrap()
}

/// `path` as kitbag prints it (`sb.path("a/b")` keeps its `/` on Windows).
fn disp(path: &Path) -> String {
    shown(&path.display().to_string())
}

/// The output line that mentions `path`, with `word` on it.
fn line_with<'a>(out: &'a str, path: &Path, word: &str) -> &'a str {
    let p = disp(path);
    out.lines()
        .find(|l| l.contains(&p) && l.contains(word))
        .unwrap_or_else(|| panic!("no line with {p} and {word:?} in:\n{out}"))
}

#[test]
fn a_claude_md_in_a_parent_directory_shadows_agents_md_for_claude_code() {
    let sb = Sandbox::empty();
    let proj = sb.path("ws/proj");
    write(&proj.join("AGENTS.md"), "# agents\n");
    write(&sb.home().join(".claude/CLAUDE.md"), "# me\n");

    // Without a CLAUDE.md in or above the project, AGENTS.md loads.
    let out = why(&sb, &proj, "claude");
    assert!(out.contains("Claude Code"), "{out}");
    assert!(
        out.contains(&format!("load 1     {}", shown("~/.claude/CLAUDE.md"))),
        "{out}"
    );
    line_with(&out, &proj.join("AGENTS.md"), "load 2");

    // A CLAUDE.md in the parent directory turns it off, and the report says why.
    write(&sb.path("ws/CLAUDE.md"), "# parent\n");
    let out = why(&sb, &proj, "claude");
    line_with(&out, &sb.path("ws/CLAUDE.md"), "load 2");
    let skip = line_with(&out, &proj.join("AGENTS.md"), "skip");
    assert!(skip.contains("shadowed"), "{skip}");
    assert!(skip.contains(&disp(&sb.path("ws/CLAUDE.md"))), "{skip}");
    assert!(out.contains("claude-md-and-agents-md"), "{out}");

    // Reading both is a user setting.
    write(
        &sb.home().join(".claude/settings.json"),
        r#"{"pluginConfigs": {"cc-plugin-agents-md@builtin": {"options": {"instructionFiles": "claude-md-and-agents-md"}}}}"#,
    );
    let out = why(&sb, &proj, "claude");
    line_with(&out, &proj.join("AGENTS.md"), "load 3");
}

fn claude_instruction_files(sb: &Sandbox, value: &str) {
    write(
        &sb.home().join(".claude/settings.json"),
        &format!(
            r#"{{"pluginConfigs": {{"cc-plugin-agents-md@builtin": {{"options": {{"instructionFiles": "{value}"}}}}}}}}"#
        ),
    );
}

#[test]
fn claude_code_reads_each_directorys_agents_md_after_its_claude_md() {
    let sb = Sandbox::empty();
    let proj = sb.path("ws/proj");
    write(&sb.path("ws/CLAUDE.md"), "# parent\n");
    write(&sb.path("ws/AGENTS.md"), "# parent agents\n");
    write(&proj.join("CLAUDE.md"), "# project\n");
    write(&proj.join("AGENTS.md"), "# project agents\n");
    claude_instruction_files(&sb, "claude-md-and-agents-md");
    let out = why(&sb, &proj, "claude");
    line_with(&out, &sb.path("ws/CLAUDE.md"), "load 1");
    line_with(&out, &sb.path("ws/AGENTS.md"), "load 2");
    line_with(&out, &proj.join("CLAUDE.md"), "load 3");
    line_with(&out, &proj.join("AGENTS.md"), "load 4");
    assert!(!out.contains("unverified"), "{out}");

    // Without any CLAUDE.md, the order of AGENTS.md files is still open.
    fs::remove_file(sb.path("ws/CLAUDE.md")).unwrap();
    fs::remove_file(proj.join("CLAUDE.md")).unwrap();
    claude_instruction_files(&sb, "claude-md-or-agents-md");
    let out = why(&sb, &proj, "claude");
    line_with(&out, &sb.path("ws/AGENTS.md"), "load 1");
    line_with(&out, &proj.join("AGENTS.md"), "load 2");
    assert!(out.contains("unverified"), "{out}");
    assert!(out.contains("come in the load order"), "{out}");
}

#[test]
fn claude_code_managed_only_skips_everything_but_the_managed_file() {
    let sb = Sandbox::empty();
    let proj = sb.path("ws/proj");
    write(&sb.home().join(".claude/CLAUDE.md"), "# me\n");
    write(&sb.home().join(".claude/rules/mine.md"), "Be brief.\n");
    write(&sb.path("ws/CLAUDE.md"), "# parent\n");
    write(&proj.join("CLAUDE.md"), "# project\n");
    write(&proj.join("CLAUDE.local.md"), "# local\n");
    write(&proj.join("AGENTS.md"), "# agents\n");
    write(&proj.join(".claude/rules/style.md"), "Use tabs.\n");
    claude_instruction_files(&sb, "managed-only");
    let out = why(&sb, &proj, "claude");
    assert!(!out.contains("load 1"), "{out}");
    for f in ["~/.claude/CLAUDE.md", "~/.claude/rules/mine.md"] {
        let f = shown(f);
        let skip = out
            .lines()
            .find(|l| l.contains(&f) && l.contains("skip"))
            .unwrap_or_else(|| panic!("no skip line for {f} in:\n{out}"));
        assert!(skip.contains("managed-only"), "{skip}");
    }
    for f in [
        sb.path("ws/CLAUDE.md"),
        proj.join("CLAUDE.md"),
        proj.join("CLAUDE.local.md"),
        proj.join("AGENTS.md"),
        proj.join(".claude/rules/style.md"),
    ] {
        let skip = line_with(&out, &f, "skip");
        assert!(skip.contains("managed-only"), "{skip}");
    }
    assert!(!out.contains("unverified"), "{out}");
    assert!(out.contains("auto memory"), "{out}");
}

#[test]
fn claude_code_flags_agents_md_under_home_and_path_scoped_rules() {
    let sb = Sandbox::empty();
    let proj = sb.home().join("work/app");
    write(&proj.join("AGENTS.md"), "# agents\n");
    write(&sb.home().join(".claude/CLAUDE.md"), "# me\n");
    write(
        &proj.join(".claude/rules/rust.md"),
        "---\npaths:\n  - \"**/*.rs\"\n---\nUse ?.\n",
    );
    write(&proj.join(".claude/rules/always.md"), "Be brief.\n");
    let out = why(&sb, &proj, "claude-code");
    assert!(out.contains("anthropics/claude-code#103458"), "{out}");
    assert!(
        out.contains(&format!(
            "{}  project rule with paths",
            shown("~/work/app/.claude/rules/rust.md")
        )),
        "{out}"
    );
    assert!(
        out.contains(&shown("~/work/app/.claude/rules/always.md  project rule")),
        "{out}"
    );
}

#[cfg(unix)]
#[test]
fn a_user_claude_md_linked_to_the_projects_agents_md_is_loaded_twice() {
    let sb = Sandbox::empty();
    let proj = sb.home().join("work");
    write(&proj.join("AGENTS.md"), "# shared\n");
    fs::create_dir_all(sb.home().join(".claude")).unwrap();
    std::os::unix::fs::symlink(proj.join("AGENTS.md"), sb.home().join(".claude/CLAUDE.md"))
        .unwrap();
    let out = why(&sb, &proj, "claude");
    assert!(out.contains("loaded twice"), "{out}");
    assert!(out.contains("anthropics/claude-code#102369"), "{out}");
}

#[test]
fn codex_agents_md_under_and_over_the_32_kib_cap() {
    let sb = Sandbox::empty();
    let proj = sb.path("repo");
    fs::create_dir_all(proj.join(".git")).unwrap();
    write(&proj.join("AGENTS.md"), "# small\n");
    let out = why(&sb, &proj, "codex");
    line_with(&out, &proj.join("AGENTS.md"), "load 1");
    assert!(!out.contains("truncated"), "{out}");

    write(&proj.join("AGENTS.md"), &"a".repeat(40 * 1024));
    let out = why(&sb, &proj, "codex");
    line_with(&out, &proj.join("AGENTS.md"), "load 1");
    let trap = line_with(&out, &proj.join("AGENTS.md"), "truncated");
    assert!(trap.contains("first 32.0 KiB of its 40.0 KiB"), "{trap}");

    // The cap is shared: the root's file uses it up, so the subdirectory's is not read.
    let sub = proj.join("crates/core");
    write(&sub.join("AGENTS.md"), "# core\n");
    let out = why(&sb, &sub, "codex");
    let skip = line_with(&out, &sub.join("AGENTS.md"), "skip");
    assert!(skip.contains("over the cap"), "{skip}");

    // project_doc_max_bytes raises it.
    write(
        &sb.home().join(".codex/config.toml"),
        "project_doc_max_bytes = 65536\n",
    );
    let out = why(&sb, &sub, "codex");
    assert!(!out.contains("truncated"), "{out}");
    line_with(&out, &sub.join("AGENTS.md"), "load 2");
}

#[test]
fn codex_override_and_global_files() {
    let sb = Sandbox::empty();
    let proj = sb.path("p");
    write(&proj.join("AGENTS.md"), "# agents\n");
    write(&proj.join("AGENTS.override.md"), "# override\n");
    write(&sb.home().join(".codex/AGENTS.md"), "# global\n");
    let out = why(&sb, &proj, "codex");
    assert!(
        out.contains(&format!("load 1     {}", shown("~/.codex/AGENTS.md"))),
        "{out}"
    );
    line_with(&out, &proj.join("AGENTS.override.md"), "load 2");
    let skip = line_with(&out, &proj.join("AGENTS.md"), "skip");
    assert!(skip.contains("one file per directory"), "{skip}");
}

/// A TOML literal string holding `path` (no escapes, so Windows paths work).
fn toml_path(path: &Path) -> String {
    format!("'{}'", path.display())
}

#[test]
fn codex_ignores_an_untrusted_projects_config() {
    let sb = Sandbox::empty();
    let proj = sb.path("p");
    write(&proj.join("AGENTS.md"), "# agents\n");
    write(&proj.join(".codex/config.toml"), "model = \"o3\"\n");
    let out = why(&sb, &proj, "codex");
    let trap = line_with(&out, &proj.join(".codex/config.toml"), "trap");
    assert!(trap.contains("not marked trusted"), "{trap}");
    line_with(&out, &proj.join("AGENTS.md"), "load 1");

    let config = sb.home().join(".codex/config.toml");
    write(
        &config,
        &format!(
            "[projects.{}]\ntrust_level = \"trusted\"\n",
            toml_path(&proj)
        ),
    );
    let out = why(&sb, &proj, "codex");
    assert!(out.contains("(trusted project)"), "{out}");
    assert!(!out.contains("trap"), "{out}");

    // Explicitly untrusted: no project AGENTS.md either.
    write(
        &config,
        &format!(
            "[projects.{}]\ntrust_level = \"untrusted\"\n",
            toml_path(&proj)
        ),
    );
    let out = why(&sb, &proj, "codex");
    let skip = line_with(&out, &proj.join("AGENTS.md"), "skip");
    assert!(skip.contains("marked untrusted"), "{skip}");
}

#[test]
fn cursor_user_rules_are_not_on_disk() {
    let sb = Sandbox::empty();
    let proj = sb.project();
    write(
        &proj.join(".cursor/rules/rust.mdc"),
        "---\nglobs: \"*.rs\"\nalwaysApply: false\n---\nUse ?.\n",
    );
    write(
        &proj.join(".cursor/rules/style.mdc"),
        "---\nalwaysApply: true\n---\nBe brief.\n",
    );
    write(&proj.join(".cursor/rules/notes.md"), "not a rule\n");
    write(&proj.join(".cursorrules"), "old\n");
    let out = why(&sb, &proj, "cursor");
    assert!(
        out.contains("User Rules live in Cursor's settings (Customize > Rules) in app storage, not in files on disk"),
        "{out}"
    );
    line_with(&out, &proj.join(".cursor/rules/style.mdc"), "load 1");
    line_with(&out, &proj.join(".cursor/rules/rust.mdc"), "later");
    let skip = line_with(&out, &proj.join(".cursor/rules/notes.md"), "skip");
    assert!(skip.contains(".mdc"), "{skip}");
    line_with(&out, &proj.join(".cursorrules"), "unverified");
}

#[test]
fn gemini_follows_context_file_name() {
    let sb = Sandbox::empty();
    let proj = sb.project();
    write(&proj.join("GEMINI.md"), "# gemini\n");
    write(&proj.join("AGENTS.md"), "# agents\n");
    let out = why(&sb, &proj, "gemini-cli");
    line_with(&out, &proj.join("GEMINI.md"), "load 1");
    line_with(&out, &proj.join("AGENTS.md"), "skip");

    write(
        &sb.home().join(".gemini/settings.json"),
        r#"{"context": {"fileName": ["AGENTS.md"]}}"#,
    );
    let out = why(&sb, &proj, "gemini-cli");
    line_with(&out, &proj.join("AGENTS.md"), "load 1");
    line_with(&out, &proj.join("GEMINI.md"), "skip");
}

#[test]
fn opencode_falls_back_to_claude_md_per_directory() {
    let sb = Sandbox::empty();
    let proj = sb.home().join("code/app");
    write(&proj.join("CLAUDE.md"), "# claude\n");
    write(&sb.home().join("code/AGENTS.md"), "# parent\n");
    write(&sb.home().join("code/CLAUDE.md"), "# parent claude\n");
    let out = why(&sb, &proj, "opencode");
    assert!(
        out.contains(&shown(
            "~/code/app/CLAUDE.md  project (CLAUDE.md, as there is no AGENTS.md)"
        )),
        "{out}"
    );
    assert!(
        out.contains(&shown("~/code/AGENTS.md  parent directory")),
        "{out}"
    );
    assert!(
        out.contains(&shown("~/code/CLAUDE.md  shadowed by AGENTS.md")),
        "{out}"
    );
}

#[test]
fn why_honours_to_and_defaults_to_every_tool_with_instructions() {
    let sb = Sandbox::empty();
    let out = why(&sb, &sb.project(), "codex");
    assert!(out.contains("Codex CLI"), "{out}");
    assert!(
        !out.contains("Claude Code") && !out.contains("Cursor"),
        "{out}"
    );

    let out = sb
        .cli()
        .args(["doctor", "--why"])
        .arg(sb.project())
        .output()
        .unwrap();
    let out = String::from_utf8(out.stdout).unwrap();
    for name in [
        "Claude Code",
        "Cursor",
        "Codex CLI",
        "Gemini CLI",
        "Google Antigravity",
        "opencode",
    ] {
        assert!(out.contains(name), "{name}: {out}");
    }
    assert!(!out.contains("Claude Desktop"), "{out}");

    // Without DIR, the current directory.
    write(&sb.path("AGENTS.md"), "# here\n");
    let out = sb
        .cli()
        .args(["doctor", "--why", "--to", "codex"])
        .output()
        .unwrap();
    let out = String::from_utf8(out.stdout).unwrap();
    assert!(out.contains("AGENTS.md  project"), "{out}");

    sb.cli()
        .args(["doctor", "--why"])
        .arg(sb.project())
        .args(["--to", "nope"])
        .assert()
        .failure()
        .stdout(contains("Instruction files").not());
}

#[test]
fn plain_doctor_is_unchanged_and_why_takes_no_profile() {
    let sb = Sandbox::empty();
    sb.cli()
        .args(["doctor"])
        .assert()
        .stdout(contains("Tools").and(contains("Instruction files").not()));
    sb.cli()
        .args(["doctor", "prof", "--why", "project"])
        .assert()
        .failure()
        .stderr(contains("cannot be used with"));
    for extra in [["--path", "sub"].as_slice(), &["--allow-insecure-source"]] {
        sb.cli()
            .args(["doctor", "--why", "."])
            .args(extra)
            .assert()
            .failure()
            .stderr(contains("cannot be used with"));
    }
    sb.cli()
        .args(["doctor", "--why", "missing"])
        .assert()
        .failure()
        .stderr(contains("is not a directory"));
}

#[cfg(target_os = "linux")]
#[test]
fn a_file_with_the_wrong_case_is_reported() {
    let sb = Sandbox::empty();
    let proj = sb.project();
    write(&proj.join("agents.md"), "# lower case\n");
    let out = why(&sb, &proj, "codex");
    let skip = line_with(&out, &proj.join("agents.md"), "skip");
    assert!(skip.contains("must be called AGENTS.md"), "{skip}");
}

/// Grok Build: home files first ($GROK_HOME, then Claude's and Cursor's),
/// then every directory from the git root down; every name that exists loads.
#[test]
fn grok_build_loads_home_then_root_down_and_every_instruction_file() {
    let sb = Sandbox::empty();
    let repo = sb.path("ws/repo");
    let sub = repo.join("sub");
    fs::create_dir_all(repo.join(".git")).unwrap();
    write(&sb.home().join(".grok/AGENTS.md"), "# me\n");
    write(&sb.home().join(".grok/rules/b.md"), "b\n");
    write(&sb.home().join(".grok/rules/a.md"), "a\n");
    write(&sb.home().join(".claude/CLAUDE.md"), "# claude me\n");
    write(&sb.home().join(".cursor/rules/c.md"), "c\n");
    write(&sb.path("ws/AGENTS.md"), "# above the repo\n");
    write(&repo.join("AGENTS.md"), "# repo\n");
    write(&repo.join("CLAUDE.md"), "# repo claude\n");
    write(
        &repo.join(".claude/rules/scoped.md"),
        "---\npaths:\n  - \"src/**\"\n---\n\nOnly for src.\n",
    );
    write(&sub.join("AGENTS.md"), "# sub\n");
    write(&sub.join(".grok/rules/s.md"), "s\n");

    let out = why(&sb, &sub, "grok");
    let order = [
        std::path::PathBuf::from("~/.grok/AGENTS.md"),
        std::path::PathBuf::from("~/.grok/rules/a.md"),
        std::path::PathBuf::from("~/.grok/rules/b.md"),
        std::path::PathBuf::from("~/.claude/CLAUDE.md"),
        std::path::PathBuf::from("~/.cursor/rules/c.md"),
        repo.join("CLAUDE.md"),
        repo.join("AGENTS.md"),
        repo.join(".claude/rules/scoped.md"),
        sub.join("AGENTS.md"),
        sub.join(".grok/rules/s.md"),
    ];
    let mut last = 0;
    for (i, p) in order.iter().enumerate() {
        let line = line_with(&out, p, "load");
        assert!(line.contains(&format!("load {}", i + 1)), "{line}\n{out}");
        let at = out.find(line).unwrap();
        assert!(at >= last, "{} out of order:\n{out}", p.display());
        last = at;
    }
    // Above the git root nothing is read.
    assert!(!out.contains(&disp(&sb.path("ws/AGENTS.md"))), "{out}");
    // A Claude rule scoped by frontmatter loads for every file in Grok.
    line_with(&out, &repo.join(".claude/rules/scoped.md"), "trap");
    assert!(out.contains("trusted"), "{out}");
}

#[test]
fn grok_build_compat_cells_skip_claude_files() {
    let sb = Sandbox::empty();
    let repo = sb.path("repo");
    fs::create_dir_all(repo.join(".git")).unwrap();
    write(
        &sb.home().join(".grok/config.toml"),
        "[compat.claude]\nrules = false\n",
    );
    write(&sb.home().join(".claude/CLAUDE.md"), "# claude me\n");
    write(&sb.home().join(".claude/rules/r.md"), "r\n");
    write(&repo.join(".claude/CLAUDE.md"), "# project claude\n");
    write(&repo.join("CLAUDE.md"), "# top-level claude\n");

    // config.toml turns rules off; the variable turns named files off.
    let out = sb
        .cli()
        .env("GROK_CLAUDE_AGENTS_ENABLED", "false")
        .args(["doctor", "--why"])
        .arg(&repo)
        .args(["--to", "grok-build"])
        .output()
        .unwrap();
    let out = String::from_utf8(out.stdout).unwrap();
    line_with(&out, Path::new("~/.claude/rules/r.md"), "rules is off");
    line_with(&out, Path::new("~/.claude/CLAUDE.md"), "agents is off");
    line_with(&out, &repo.join(".claude/CLAUDE.md"), "agents is off");
    // A top-level CLAUDE.md is always recognized.
    line_with(&out, &repo.join("CLAUDE.md"), "load 1");

    // With the defaults all of them load.
    let out = why(&sb, &repo, "grok-build");
    line_with(&out, &repo.join(".claude/CLAUDE.md"), "load");
    line_with(&out, Path::new("~/.claude/CLAUDE.md"), "load");
}

#[test]
fn grok_build_outside_git_reads_only_the_directory() {
    let sb = Sandbox::empty();
    let dir = sb.path("plain/inner");
    write(&sb.path("plain/AGENTS.md"), "# parent\n");
    write(&dir.join("AGENTS.md"), "# here\n");
    let out = why(&sb, &dir, "grok-build");
    line_with(&out, &dir.join("AGENTS.md"), "load 1");
    assert!(!out.contains(&disp(&sb.path("plain/AGENTS.md"))), "{out}");
    assert!(out.contains("not inside a git repository"), "{out}");
}
