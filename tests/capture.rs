//! Capturing from real, messy setups: odd encodings, symlinks, huge files,
//! broken configs and things kitbag doesn't carry.

mod common;

use std::fs;

use common::*;
use kitbag::adapters::Scope;
use kitbag::profile_io;
use predicates::prelude::*;
use predicates::str::contains;

fn write(sb: &Sandbox, rel: &str, content: impl AsRef<[u8]>) -> std::path::PathBuf {
    let p = sb.home().join(rel);
    fs::create_dir_all(p.parent().unwrap()).unwrap();
    fs::write(&p, content).unwrap();
    p
}

fn utf16(text: &str, big_endian: bool) -> Vec<u8> {
    let mut out = if big_endian {
        vec![0xFE, 0xFF]
    } else {
        vec![0xFF, 0xFE]
    };
    for u in text.encode_utf16() {
        out.extend(if big_endian {
            u.to_be_bytes()
        } else {
            u.to_le_bytes()
        });
    }
    out
}

fn warned(r: &kitbag::engine::CaptureResult, needle: &str) -> bool {
    r.notes.iter().any(|n| n.message.contains(needle))
}

#[test]
fn text_items_in_other_encodings_are_decoded_or_skipped() {
    let sb = Sandbox::empty();
    write(
        &sb,
        ".claude/rules/le.md",
        utf16("Little endian rule.\n", false),
    );
    write(
        &sb,
        ".claude/rules/be.md",
        utf16("Big endian rule.\n", true),
    );
    write(&sb, ".claude/rules/bom.md", b"\xEF\xBB\xBFBOM rule.\n");
    write(&sb, ".claude/rules/latin1.md", b"Caf\xE9 au lait.\n");
    write(&sb, ".claude/CLAUDE.md", b"\xEF\xBB\xBFBe brief.\n");
    write(
        &sb,
        ".claude/commands/c.md",
        utf16("Do $ARGUMENTS.\n", false),
    );
    write(
        &sb,
        ".claude/skills/s16/SKILL.md",
        utf16(
            "---\nname: s16\ndescription: UTF-16 skill\n---\nBody.\n",
            false,
        ),
    );
    write(&sb, ".claude/skills/s16/notes.txt", b"latin-1 caf\xE9\n");

    let r = capture(&["claude-code"], &sb.env(Scope::User));
    let rules: Vec<(&str, &str)> = r
        .profile
        .rules
        .iter()
        .map(|r| (r.name.as_str(), r.body.as_str()))
        .collect();
    assert_eq!(
        rules,
        vec![
            ("be", "Big endian rule."),
            ("bom", "BOM rule."),
            ("le", "Little endian rule."),
        ]
    );
    assert!(warned(&r, "latin1.md: not valid UTF-8"));
    assert_eq!(r.profile.instructions.as_deref(), Some("Be brief."));
    assert_eq!(r.profile.commands[0].body, "Do $ARGUMENTS.");
    let skill = &r.profile.skills[0];
    assert_eq!(skill.description.as_deref(), Some("UTF-16 skill"));
    let notes = skill.files.iter().find(|f| f.path == "notes.txt").unwrap();
    assert_eq!(
        notes.content, b"latin-1 caf\xE9\n",
        "bytes kept as they are"
    );
}

#[test]
fn oversize_files_are_skipped_so_the_profile_stays_loadable() {
    let sb = Sandbox::empty();
    let big = vec![b'x'; 3 * 1024 * 1024];
    write(&sb, ".claude/CLAUDE.md", &big);
    write(&sb, ".claude/rules/huge.md", &big);
    write(&sb, ".claude/rules/ok.md", "Fine.\n");
    write(
        &sb,
        ".claude/skills/tool/SKILL.md",
        "---\nname: tool\ndescription: Tool\n---\nUse it.\n",
    );
    write(&sb, ".claude/skills/tool/bin/tool.bin", &big);
    sb.cli()
        .args(["capture", "--from", "claude-code", "-o", "prof"])
        .assert()
        .success()
        .stdout(
            contains("huge.md: 3072 KiB is over the 2 MiB limit")
                .and(contains(
                    "tool/bin/tool.bin': 3072 KiB is over the 2 MiB limit",
                ))
                .and(contains("CLAUDE.md: 3072 KiB")),
        );
    let p = profile_io::load(&sb.path("prof")).unwrap();
    assert_eq!(p.rules.len(), 1);
    assert!(p.instructions.is_none());
    assert_eq!(p.skills[0].files.len(), 1);
    sb.cli()
        .args(["apply", "prof", "--to", "codex", "--yes"])
        .assert()
        .success();
}

#[test]
fn utf8_bom_in_json_and_toml_configs_is_read_and_kept() {
    let sb = Sandbox::empty();
    let claude_json = write(
        &sb,
        ".claude.json",
        b"\xEF\xBB\xBF{\n  \"mcpServers\": {\"a\": {\"command\": \"a-mcp\"}}\n}\n",
    );
    let codex = write(
        &sb,
        ".codex/config.toml",
        b"\xEF\xBB\xBF[mcp_servers.b]\ncommand = \"b-mcp\"\n",
    );
    let before = (fs::read(&claude_json).unwrap(), fs::read(&codex).unwrap());
    let r = capture(&["claude-code", "codex"], &sb.env(Scope::User));
    assert_eq!(
        r.profile.mcp_servers.keys().collect::<Vec<_>>(),
        vec!["a", "b"]
    );
    profile_io::save(&r.profile, &sb.path("prof"), false).unwrap();

    // Each tool gets the other's server; both files keep their byte-order mark.
    sb.cli()
        .args(["apply", "prof", "--to", "claude-code,codex", "--yes"])
        .assert()
        .success();
    let cj = fs::read(&claude_json).unwrap();
    let cx = fs::read(&codex).unwrap();
    assert!(cj.starts_with(b"\xEF\xBB\xBF{") && !cj[3..].starts_with(b"\xEF\xBB\xBF"));
    assert!(cx.starts_with(b"\xEF\xBB\xBF["));
    assert!(String::from_utf8_lossy(&cj).contains("b-mcp"));
    assert!(String::from_utf8_lossy(&cx).contains("a-mcp"));
    sb.cli()
        .args(["diff", "prof", "--to", "claude-code,codex", "--exit-code"])
        .assert()
        .success();
    sb.cli().arg("undo").assert().success();
    assert_eq!(
        (fs::read(&claude_json).unwrap(), fs::read(&codex).unwrap()),
        before
    );
}

#[cfg(unix)]
#[test]
fn skill_symlinks_are_followed_only_inside_the_skill() {
    use std::os::unix::fs::symlink;
    let sb = Sandbox::empty();
    // Secrets outside the skill that links point at.
    let outside = sb.path("outside");
    fs::create_dir_all(outside.join("aws")).unwrap();
    fs::write(outside.join("aws/config.md"), "region = eu").unwrap();
    fs::write(outside.join("notes.md"), "private notes").unwrap();

    let skill = sb.home().join(".claude/skills/loop");
    write(
        &sb,
        ".claude/skills/loop/SKILL.md",
        "---\nname: loop\ndescription: Loops\n---\nBody.\n",
    );
    write(&sb, ".claude/skills/loop/docs/a.md", "A");
    symlink(&outside, skill.join("etc")).unwrap();
    symlink("..", skill.join("up")).unwrap();
    symlink(".", skill.join("self")).unwrap();
    symlink("missing", skill.join("dangling")).unwrap();
    symlink(outside.join("notes.md"), skill.join("notes.md")).unwrap();
    symlink("docs", skill.join("docs-link")).unwrap();
    symlink("docs/a.md", skill.join("a-link.md")).unwrap();
    fs::create_dir_all(skill.join("locked")).unwrap();
    fs::write(skill.join("locked/x.md"), "x").unwrap();
    fs::set_permissions(
        skill.join("locked"),
        std::os::unix::fs::PermissionsExt::from_mode(0o000),
    )
    .unwrap();
    let permissions_enforced = fs::read_dir(skill.join("locked")).is_err();

    // A whole skill symlinked into a git checkout, and a symlinked CLAUDE.md, still work.
    let checkout = sb.path("Projects/claude-skills/ext");
    fs::create_dir_all(checkout.join("ref")).unwrap();
    fs::write(
        checkout.join("SKILL.md"),
        "---\nname: ext\ndescription: Ext\n---\nBody.\n",
    )
    .unwrap();
    fs::write(checkout.join("ref/cli.md"), "reference").unwrap();
    symlink(&checkout, sb.home().join(".claude/skills/ext")).unwrap();
    fs::write(sb.path("dotfiles-CLAUDE.md"), "From dotfiles.\n").unwrap();
    symlink(
        sb.path("dotfiles-CLAUDE.md"),
        sb.home().join(".claude/CLAUDE.md"),
    )
    .unwrap();

    let r = capture(&["claude-code"], &sb.env(Scope::User));
    fs::set_permissions(
        skill.join("locked"),
        std::os::unix::fs::PermissionsExt::from_mode(0o755),
    )
    .unwrap();

    let files = |name: &str| -> Vec<String> {
        r.profile
            .skills
            .iter()
            .find(|s| s.name == name)
            .unwrap()
            .files
            .iter()
            .map(|f| f.path.clone())
            .collect()
    };
    let mut want = vec!["SKILL.md", "a-link.md", "docs-link/a.md", "docs/a.md"];
    if !permissions_enforced {
        want.push("locked/x.md");
    }
    assert_eq!(files("loop"), want);
    assert_eq!(files("ext"), vec!["SKILL.md", "ref/cli.md"]);
    assert_eq!(r.profile.instructions.as_deref(), Some("From dotfiles."));
    for (entry, why) in [
        ("etc", "outside the skill"),
        ("up", "outside the skill"),
        ("notes.md", "outside the skill"),
        ("self", "symbolic link loop"),
        ("dangling", "dangling symbolic link"),
    ] {
        assert!(warned(&r, &format!("'loop/{entry}: ")), "{entry}");
        assert!(warned(&r, why), "{why}");
    }
    if permissions_enforced {
        assert!(warned(&r, "'loop/locked: "));
    }
    let text = format!("{:?}", r.profile);
    assert!(!text.contains("private notes") && !text.contains("region = eu"));
}

#[test]
fn reports_what_is_not_carried_and_drops_custom_output_styles() {
    let sb = Sandbox::empty();
    write(
        &sb,
        ".claude/agents/reviewer.md",
        "---\nname: reviewer\n---\nReview.\n",
    );
    write(
        &sb,
        ".claude/agents/nested/tester.md",
        "---\nname: tester\n---\nTest.\n",
    );
    write(
        &sb,
        ".claude/output-styles/Jenny.md",
        "---\nname: Jenny\n---\nBe Jenny.\n",
    );
    write(
        &sb,
        ".claude/plugins/installed_plugins.json",
        r#"{"version": 2, "plugins": {"a@mkt": [{"scope": "user"}], "b@mkt": [{"scope": "user"}]}}"#,
    );
    let settings = write(
        &sb,
        ".claude/settings.json",
        r#"{"outputStyle": "Jenny", "model": "opus", "hooks": {}}"#,
    );
    let r = capture(&["claude-code"], &sb.env(Scope::User));
    assert!(
        warned(
            &r,
            "not carried by kitbag (no portable form yet): 2 subagents (agents/), 1 output style (output-styles/), 2 plugins (plugins/installed_plugins.json)"
        ),
        "{:?}",
        r.notes
    );
    assert!(warned(&r, "dropped setting outputStyle = \"Jenny\""));
    assert!(warned(&r, "hooks"));
    assert_eq!(
        serde_json::Value::Object(r.profile.settings["claude-code"].clone()),
        serde_json::json!({"model": "opus"})
    );

    // Built-in styles are carried.
    fs::write(&settings, r#"{"outputStyle": "Explanatory"}"#).unwrap();
    let r = capture(&["claude-code"], &sb.env(Scope::User));
    assert_eq!(
        r.profile.settings["claude-code"]["outputStyle"],
        "Explanatory"
    );
    assert!(!warned(&r, "dropped setting"));
}

#[test]
fn malformed_server_entries_are_reported() {
    let sb = Sandbox::empty();
    write(
        &sb,
        ".claude.json",
        r#"{"mcpServers": {"a": "notanobject", "b": {"command": "b-mcp"}, "c": {}}}"#,
    );
    write(&sb, ".cursor/mcp.json", r#"{"mcpServers": []}"#);
    write(&sb, ".codex/config.toml", "mcp_servers = 3\n");
    let r = capture(&["claude-code", "cursor", "codex"], &sb.env(Scope::User));
    assert_eq!(r.profile.mcp_servers.keys().collect::<Vec<_>>(), vec!["b"]);
    assert!(warned(&r, "skipped MCP server 'a' in"));
    assert!(warned(&r, "expected an object, found a string"));
    assert!(warned(&r, "skipped MCP server 'c': no command or url"));
    assert!(warned(&r, "expected an object of servers, found an array"));
    assert!(warned(&r, "ignored 'mcp_servers' in"));
}

#[test]
fn one_broken_tool_does_not_stop_a_multi_tool_capture() {
    let sb = Sandbox::empty();
    write(
        &sb,
        ".claude.json",
        r#"{"mcpServers": {"a": {"command": "a-mcp"}}}"#,
    );
    write(&sb, ".codex/config.toml", "[mcp_servers.b\ncommand = 1\n");
    for from in ["all", "detected", "claude-code,codex"] {
        sb.cli()
            .args(["capture", "--from", from, "--dry-run"])
            .assert()
            .success()
            .stdout(contains("skipped OpenAI Codex CLI").and(contains("invalid TOML")))
            .stdout(contains("1 MCP server(s)"));
    }
    // Asking for the broken tool alone still fails loudly.
    sb.cli()
        .args(["capture", "--from", "codex", "--dry-run"])
        .assert()
        .failure()
        .stderr(contains("invalid TOML"));
    // So does a capture where nothing could be read.
    fs::write(sb.home().join(".claude.json"), "{ nope").unwrap();
    sb.cli()
        .args(["capture", "--from", "claude-code,codex", "--dry-run"])
        .assert()
        .failure()
        .stderr(contains("none of the selected tools could be read"));
}
