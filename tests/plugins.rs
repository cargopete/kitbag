//! Claude Code plugins, carried by reference: the `enabledPlugins` and
//! `extraKnownMarketplaces` settings, never plugin files. Everything here is
//! offline: kitbag only edits settings files, and the sandbox has no `claude`.

mod common;

use std::fs;

use common::*;
use serde_json::{Value, json};

fn run(sb: &Sandbox, args: &[&str]) -> (bool, String) {
    let out = sb.cli().args(args).output().unwrap();
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    (out.status.success(), text)
}

fn ok(sb: &Sandbox, args: &[&str]) -> String {
    let (good, text) = run(sb, args);
    assert!(good, "{args:?}: {text}");
    text
}

/// What Claude Code's user settings hold after `claude plugin marketplace add`
/// and some installs.
fn settings(home_path: &str) -> Value {
    json!({
        "model": "opus",
        "enabledPlugins": {
            "formatter@acme-tools": true,
            "turned-off@acme-tools": false,
            "notes@claude-plugins-official": true,
            "local-only@mine": true,
            "dev@inline": true,
            "synced-one@synced": true
        },
        "extraKnownMarketplaces": {
            "acme-tools": {
                "source": {"source": "github", "repo": "acme/tools", "ref": "v1.2.0"},
                "autoUpdate": false
            },
            "mine": {"source": {"source": "directory", "path": home_path}},
            "private": {"source": {"source": "url", "url": "https://example.com/m.json", "headers": {"X": "y"}}},
            "inline": {"source": {"source": "settings", "name": "x", "plugins": []}}
        }
    })
}

fn write_json(sb: &Sandbox, rel: &str, v: &Value) {
    let p = sb.path(rel);
    fs::create_dir_all(p.parent().unwrap()).unwrap();
    fs::write(p, serde_json::to_string_pretty(v).unwrap()).unwrap();
}

/// A user whose Claude Code has plugins.
fn claude_with_plugins() -> Sandbox {
    let sb = Sandbox::empty();
    let mine = sb.home().join("marketplaces").join("mine");
    write_json(
        &sb,
        "home/.claude/settings.json",
        &settings(&mine.display().to_string()),
    );
    write_json(
        &sb,
        "home/.claude/plugins/installed_plugins.json",
        &json!({"version": 2, "plugins": {
            "formatter@acme-tools": [{"scope": "user", "installPath": "/somewhere/cache", "version": "1.2.0"}],
            "notes@claude-plugins-official": [{"scope": "project", "version": "0.4.0"}]
        }}),
    );
    // The plugin's code, which must never reach a profile.
    fs::create_dir_all(sb.path("home/.claude/plugins/cache/acme-tools/formatter/1.2.0")).unwrap();
    fs::write(
        sb.path("home/.claude/plugins/cache/acme-tools/formatter/1.2.0/run.sh"),
        "echo PLUGIN-CODE\n",
    )
    .unwrap();
    sb
}

fn captured(sb: &Sandbox) -> kitbag::model::Profile {
    ok(sb, &["capture", "--from", "claude-code", "--out", "cap"]);
    kitbag::profile_io::load(&sb.path("cap")).unwrap()
}

fn profile_with(sb: &Sandbox, dir: &str, toml: &str) {
    fs::create_dir_all(sb.path(dir)).unwrap();
    fs::write(
        sb.path(&format!("{dir}/kitbag.toml")),
        format!("kitbag = 1\nname = \"p\"\n{toml}"),
    )
    .unwrap();
}

const PLUGINS: &str = r#"
[plugins.claude-code.marketplaces.acme-tools]
source = { source = "github", repo = "acme/tools", ref = "v1.2.0" }
autoUpdate = false

[plugins.claude-code.marketplaces.team]
source = { source = "git", url = "https://git.example.com/team/plugins.git" }

[plugins.claude-code.enabled."formatter@acme-tools"]
version = "1.2.0"

[plugins.claude-code.enabled."review@team"]
"#;

fn norm(s: &str) -> String {
    s.replace('\\', "/")
}

// ---------------------------------------------------------------- capture

#[test]
fn capture_carries_marketplaces_and_enabled_plugins_by_reference() {
    let sb = claude_with_plugins();
    let out = ok(&sb, &["capture", "--from", "claude-code", "--out", "cap"]);
    assert!(out.contains(", 3 plugin(s)"), "{out}");
    assert!(
        !out.contains("installed_plugins.json"),
        "plugins are no longer listed as not carried: {out}"
    );
    let p = kitbag::profile_io::load(&sb.path("cap")).unwrap();
    let set = &p.plugins["claude-code"];
    assert_eq!(
        set.enabled.keys().cloned().collect::<Vec<_>>(),
        [
            "formatter@acme-tools",
            "local-only@mine",
            "notes@claude-plugins-official"
        ]
    );
    // The version Claude Code recorded for the user-scope install is kept; a
    // project-scope install's is not this profile's.
    assert_eq!(
        set.enabled["formatter@acme-tools"].version.as_deref(),
        Some("1.2.0")
    );
    assert_eq!(set.enabled["notes@claude-plugins-official"].version, None);
    let acme = &set.marketplaces["acme-tools"];
    assert_eq!(
        Value::Object(acme.source.clone()),
        json!({"source": "github", "repo": "acme/tools", "ref": "v1.2.0"})
    );
    assert_eq!(acme.auto_update, Some(false));

    // A local marketplace path is written relative to the home directory.
    let mine = &set.marketplaces["mine"].source["path"];
    assert!(mine.as_str().unwrap().starts_with("~/"), "{mine}");

    // Said, not silently left: headers, inline marketplaces, other origins, disabled plugins.
    assert!(
        out.contains("'private' not captured") && out.contains("headers"),
        "{out}"
    );
    assert!(out.contains("'inline' not captured"), "{out}");
    assert!(
        out.contains("dev@inline") && out.contains("synced-one@synced"),
        "{out}"
    );
    assert!(out.contains("1 plugin(s) set to false"), "{out}");
    assert!(!set.marketplaces.contains_key("private"));
    assert!(!set.enabled.contains_key("turned-off@acme-tools"));

    // Only the reference is in the profile: no plugin code, no paths into the cache.
    let all = all_text_under(&sb.path("cap"));
    assert!(!all.contains("PLUGIN-CODE"), "{all}");
    assert!(!all.contains("/somewhere/cache"), "{all}");
    let files = kitbag::plan_fs::walk_files(&sb.path("cap"), 100).unwrap();
    assert_eq!(files, ["kitbag.toml"], "{files:?}");
}

#[test]
fn capture_keeps_the_marketplace_pin_and_settings_it_already_carried() {
    let sb = claude_with_plugins();
    let p = captured(&sb);
    // `model` still travels as a setting, and nothing else of the plugin keys does.
    assert_eq!(p.settings["claude-code"]["model"], "opus");
    assert!(!p.settings["claude-code"].contains_key("enabledPlugins"));
    assert!(!p.settings["claude-code"].contains_key("extraKnownMarketplaces"));
    let text = fs::read_to_string(sb.path("cap/kitbag.toml")).unwrap();
    assert!(
        text.contains("[plugins.claude-code.marketplaces.acme-tools]"),
        "{text}"
    );
    assert!(text.contains("ref = \"v1.2.0\""), "{text}");
    // Written and read back the same.
    assert_eq!(
        kitbag::profile_io::load(&sb.path("cap")).unwrap().plugins,
        p.plugins
    );
}

#[test]
fn project_capture_reads_the_projects_settings_and_no_version() {
    let sb = Sandbox::empty();
    write_json(
        &sb,
        "project/.claude/settings.json",
        &json!({
            "extraKnownMarketplaces": {"team": {"source": {"source": "github", "repo": "acme/team"}}},
            "enabledPlugins": {"review@team": true}
        }),
    );
    write_json(
        &sb,
        "home/.claude/plugins/installed_plugins.json",
        &json!({"plugins": {"review@team": [{"scope": "user", "version": "9.9.9"}]}}),
    );
    ok(
        &sb,
        &[
            "capture",
            "--from",
            "claude-code",
            "--project",
            "project",
            "--out",
            "cap",
        ],
    );
    let p = kitbag::profile_io::load(&sb.path("cap")).unwrap();
    let set = &p.plugins["claude-code"];
    assert!(set.marketplaces.contains_key("team"));
    assert_eq!(set.enabled["review@team"].version, None);
}

#[test]
fn a_marketplace_url_with_credentials_is_not_captured() {
    let sb = Sandbox::empty();
    write_json(
        &sb,
        "home/.claude/settings.json",
        &json!({"extraKnownMarketplaces": {"p": {"source": {"source": "git", "url": "https://me:hunter2token@git.example.com/x.git"}}}}),
    );
    let out = ok(&sb, &["capture", "--from", "claude-code", "--out", "cap"]);
    assert!(out.contains("'p' not captured"), "{out}");
    assert!(!out.contains("hunter2token"), "{out}");
    if sb.path("cap").exists() {
        assert!(!all_text_under(&sb.path("cap")).contains("hunter2token"));
    }
}

// ------------------------------------------------------------------ apply

#[test]
fn apply_writes_only_the_two_settings_keys_and_says_what_is_left() {
    let sb = Sandbox::empty();
    profile_with(&sb, "p", PLUGINS);
    write_json(
        &sb,
        "home/.claude/settings.json",
        &json!({"theme": "dark", "enabledPlugins": {"mine@elsewhere": true}}),
    );
    let out = ok(&sb, &["apply", "p", "--to", "claude-code", "--yes"]);
    let s = sb.json("home/.claude/settings.json");
    assert_eq!(s["theme"], "dark");
    assert_eq!(
        s["enabledPlugins"],
        json!({"mine@elsewhere": true, "formatter@acme-tools": true, "review@team": true})
    );
    assert_eq!(
        s["extraKnownMarketplaces"]["acme-tools"],
        json!({"source": {"source": "github", "repo": "acme/tools", "ref": "v1.2.0"}, "autoUpdate": false})
    );
    assert_eq!(
        s["extraKnownMarketplaces"]["team"]["source"]["url"],
        "https://git.example.com/team/plugins.git"
    );
    // The plan names each marketplace, its source and each plugin, and that
    // Claude Code (not kitbag) fetches them.
    assert!(out.contains("github acme/tools, ref v1.2.0"), "{out}");
    assert!(
        out.contains("git https://git.example.com/team/plugins.git"),
        "{out}"
    );
    assert!(
        out.contains("plugin 'formatter@acme-tools' (captured at version 1.2.0)"),
        "{out}"
    );
    assert!(out.contains("plugin 'review@team'"), "{out}");
    assert!(out.contains("kitbag doesn't download plugins"), "{out}");
    // Nothing else was created: no plugin cache, no checkouts.
    assert!(!sb.path("home/.claude/plugins").exists());
    assert!(!sb.path("project/.claude").exists());
}

#[test]
fn a_second_apply_is_up_to_date_and_undo_restores_the_file() {
    let sb = Sandbox::empty();
    profile_with(&sb, "p", PLUGINS);
    // A plugin the user installed some other way is there before and after.
    let original =
        "{\n  \"theme\": \"dark\",\n  \"enabledPlugins\": {\"mine@elsewhere\": true}\n}\n";
    fs::create_dir_all(sb.path("home/.claude")).unwrap();
    fs::write(sb.path("home/.claude/settings.json"), original).unwrap();
    ok(&sb, &["apply", "p", "--to", "claude-code", "--yes"]);
    let after = sb.read("home/.claude/settings.json");
    assert_ne!(after, original);
    let again = ok(&sb, &["apply", "p", "--to", "claude-code", "--yes"]);
    assert!(again.contains("Up to date"), "{again}");
    assert_eq!(sb.read("home/.claude/settings.json"), after);
    assert!(after.contains("mine@elsewhere") && after.contains("formatter@acme-tools"));
    ok(&sb, &["undo"]);
    // The apply that changed nothing wasn't recorded; this undid the first.
    assert_eq!(sb.read("home/.claude/settings.json"), original);
}

#[test]
fn undo_does_not_overwrite_plugins_the_user_added_afterwards() {
    let sb = Sandbox::empty();
    profile_with(&sb, "p", PLUGINS);
    ok(&sb, &["apply", "p", "--to", "claude-code", "--yes"]);
    // Claude Code (or the user) changes the file after kitbag wrote it.
    let mut s = sb.json("home/.claude/settings.json");
    s["enabledPlugins"]["by-hand@other"] = json!(true);
    write_json(&sb, "home/.claude/settings.json", &s);
    let (good, out) = run(&sb, &["undo"]);
    assert!(!good && out.contains("modified after"), "{out}");
    assert_eq!(
        sb.json("home/.claude/settings.json")["enabledPlugins"]["by-hand@other"],
        json!(true)
    );
}

#[test]
fn apply_replaces_a_marketplace_that_differs_but_keeps_other_keys_and_no_overwrite_keeps_it() {
    let sb = Sandbox::empty();
    profile_with(&sb, "p", PLUGINS);
    write_json(
        &sb,
        "home/.claude/settings.json",
        &json!({
            "extraKnownMarketplaces": {"acme-tools": {"source": {"source": "git", "url": "https://old.example.com/x.git"}, "note": "mine"}},
            "enabledPlugins": {"formatter@acme-tools": false}
        }),
    );
    let out = ok(
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
    assert!(
        out.contains("kept the existing extraKnownMarketplaces entry 'acme-tools'"),
        "{out}"
    );
    assert!(
        out.contains("kept enabledPlugins 'formatter@acme-tools'"),
        "{out}"
    );
    let s = sb.json("home/.claude/settings.json");
    assert_eq!(
        s["extraKnownMarketplaces"]["acme-tools"]["source"]["source"],
        "git"
    );
    assert_eq!(s["enabledPlugins"]["formatter@acme-tools"], json!(false));

    ok(&sb, &["apply", "p", "--to", "claude-code", "--yes"]);
    let s = sb.json("home/.claude/settings.json");
    // The source is replaced whole (no half git, half github entry); a key kitbag doesn't know stays.
    assert_eq!(
        s["extraKnownMarketplaces"]["acme-tools"],
        json!({"source": {"source": "github", "repo": "acme/tools", "ref": "v1.2.0"}, "autoUpdate": false, "note": "mine"})
    );
    assert_eq!(s["enabledPlugins"]["formatter@acme-tools"], json!(true));
}

#[test]
fn a_marketplace_autoupdate_toggled_in_claude_code_is_not_fought_over() {
    let sb = Sandbox::empty();
    profile_with(
        &sb,
        "p",
        "[plugins.claude-code.marketplaces.m]\nsource = { source = \"github\", repo = \"a/b\" }\n",
    );
    write_json(
        &sb,
        "home/.claude/settings.json",
        &json!({"extraKnownMarketplaces": {"m": {"source": {"source": "github", "repo": "a/b"}, "autoUpdate": true}}}),
    );
    let out = ok(&sb, &["apply", "p", "--to", "claude-code", "--yes"]);
    assert!(out.contains("Up to date"), "{out}");
}

#[test]
fn a_local_marketplace_path_is_expanded_for_this_machine() {
    let sb = Sandbox::empty();
    profile_with(
        &sb,
        "p",
        "[plugins.claude-code.marketplaces.mine]\nsource = { source = \"directory\", path = \"~/marketplaces/mine\" }\n",
    );
    ok(&sb, &["apply", "p", "--to", "claude-code", "--yes"]);
    let s = sb.json("home/.claude/settings.json");
    let path = s["extraKnownMarketplaces"]["mine"]["source"]["path"]
        .as_str()
        .unwrap();
    let want = sb.home().join("marketplaces").join("mine");
    assert_eq!(norm(path), norm(&want.display().to_string()));
}

#[test]
fn project_scope_writes_the_projects_settings_and_says_install_is_still_manual() {
    let sb = Sandbox::empty();
    profile_with(&sb, "p", PLUGINS);
    let out = ok(
        &sb,
        &[
            "apply",
            "p",
            "--to",
            "claude-code",
            "--project",
            "project",
            "--yes",
        ],
    );
    let s = sb.json("project/.claude/settings.json");
    assert_eq!(s["enabledPlugins"]["review@team"], json!(true));
    assert!(s["extraKnownMarketplaces"]["team"].is_object());
    assert!(out.contains("--scope project"), "{out}");
    assert!(out.contains("trusted"), "{out}");
    assert!(!sb.path("home/.claude/settings.json").exists());
    // The local (uncommitted) settings file is never touched.
    assert!(!sb.path("project/.claude/settings.local.json").exists());
}

#[test]
fn a_settings_file_with_a_non_object_plugins_key_is_left_alone() {
    let sb = Sandbox::empty();
    profile_with(&sb, "p", PLUGINS);
    let text = "{\"enabledPlugins\": [\"x\"]}\n";
    fs::create_dir_all(sb.path("home/.claude")).unwrap();
    fs::write(sb.path("home/.claude/settings.json"), text).unwrap();
    let (good, out) = run(&sb, &["apply", "p", "--to", "claude-code", "--yes"]);
    assert!(!good, "{out}");
    assert!(out.contains("enabledPlugins"), "{out}");
    assert_eq!(sb.read("home/.claude/settings.json"), text);
}

#[test]
fn apply_through_a_symlinked_project_settings_does_not_leave_the_project() {
    // The project's `.claude` is a link out of the project: no write follows it.
    #[cfg(unix)]
    {
        let sb = Sandbox::empty();
        profile_with(&sb, "p", PLUGINS);
        fs::create_dir_all(sb.path("elsewhere")).unwrap();
        std::os::unix::fs::symlink(sb.path("elsewhere"), sb.path("project/.claude")).unwrap();
        let (good, out) = run(
            &sb,
            &[
                "apply",
                "p",
                "--to",
                "claude-code",
                "--project",
                "project",
                "--yes",
            ],
        );
        assert!(!good && out.contains("symbolic link"), "{out}");
        assert!(!sb.path("elsewhere/settings.json").exists());
    }
}

// ---------------------------------------------------------- targets / off

#[test]
fn an_item_disabled_on_claude_code_or_targeted_elsewhere_is_not_applied() {
    let sb = Sandbox::empty();
    profile_with(
        &sb,
        "p",
        r#"
[plugins.claude-code.marketplaces.acme-tools]
source = { source = "github", repo = "acme/tools" }
disabled_on = ["claude"]

[plugins.claude-code.marketplaces.team]
source = { source = "github", repo = "acme/team" }
targets = ["claude-code"]

[plugins.claude-code.enabled."a@acme-tools"]
targets = ["codex"]

[plugins.claude-code.enabled."b@team"]

[plugins.claude-code.enabled."c@team"]
disabled_on = ["claude-code"]
"#,
    );
    let out = ok(&sb, &["apply", "p", "--to", "claude-code", "--yes"]);
    let s = sb.json("home/.claude/settings.json");
    assert_eq!(s["enabledPlugins"], json!({"b@team": true}));
    assert_eq!(
        s["extraKnownMarketplaces"]
            .as_object()
            .unwrap()
            .keys()
            .collect::<Vec<_>>(),
        ["team"]
    );
    assert!(
        out.contains("plugin marketplace 'acme-tools'")
            && out.contains("skipped: disabled in profile"),
        "{out}"
    );
    assert!(out.contains("plugin 'a@acme-tools' skipped"), "{out}");
    assert!(out.contains("plugin 'c@team' skipped"), "{out}");
}

#[test]
fn applying_to_another_tool_writes_no_claude_plugin_settings() {
    let sb = Sandbox::empty();
    profile_with(&sb, "p", PLUGINS);
    ok(&sb, &["apply", "p", "--to", "codex,cursor", "--yes"]);
    assert!(!sb.path("home/.claude").exists());
}

#[test]
fn a_plugin_section_for_a_tool_kitbag_does_not_carry_plugins_for_is_ignored_with_a_warning() {
    let sb = Sandbox::empty();
    profile_with(
        &sb,
        "p",
        "[plugins.codex.enabled.\"x@y\"]\n[plugins.claude-code.enabled.\"a@b\"]\n",
    );
    let p = kitbag::profile_io::load(&sb.path("p")).unwrap();
    assert!(
        p.warnings
            .iter()
            .any(|w| w.contains("plugins.codex") && w.contains("ignored")),
        "{:?}",
        p.warnings
    );
    let out = ok(&sb, &["apply", "p", "--to", "codex", "--yes"]);
    assert!(!out.contains("x@y"), "{out}");
    assert!(!sb.path("home/.claude").exists());
}

// ------------------------------------------------------------ validation

#[test]
fn a_manifest_with_a_bad_plugin_reference_is_refused() {
    let cases: &[(&str, &str)] = &[
        (
            "[plugins.claude-code.enabled.\"noat\"]\n",
            "<name>@<marketplace>",
        ),
        ("[plugins.claude-code.enabled.\"x@inline\"]\n", "reserved"),
        (
            "[plugins.claude-code.marketplaces.m]\nsource = { source = \"npm\", package = \"x\" }\n",
            "must be one of",
        ),
        (
            "[plugins.claude-code.marketplaces.m]\nsource = { source = \"url\", url = \"https://a/b\", headers = { A = \"b\" } }\n",
            "credentials",
        ),
        (
            "[plugins.claude-code.marketplaces.m]\nsource = { source = \"git\", url = \"https://u:tok@h/x.git\" }\n",
            "credentials in it",
        ),
        (
            "[plugins.claude-code.marketplaces.m]\nsource = { source = \"github\", repo = \"nope\" }\n",
            "owner/repo",
        ),
        (
            "[plugins.claude-code.marketplaces.\"a b\"]\nsource = { source = \"github\", repo = \"a/b\" }\n",
            "names may only contain",
        ),
        (
            "[plugins.claude-code.enabled.\"a@b\"]\ntargets = [\"codex\"]\ndisabled_on = [\"cursor\"]\n",
            "not both",
        ),
        ("[plugins.nonsense.enabled.\"a@b\"]\n", "unknown target"),
    ];
    for (toml, want) in cases {
        let sb = Sandbox::empty();
        profile_with(&sb, "p", toml);
        let (good, out) = run(&sb, &["apply", "p", "--to", "claude-code", "--yes"]);
        assert!(!good, "{toml}: {out}");
        assert!(out.contains(want), "{toml}: wanted {want:?} in {out}");
        assert!(!sb.path("home/.claude/settings.json").exists());
    }
}

// ------------------------------------------------------------- trust

fn remote_profile(sb: &Sandbox) -> String {
    sb.git_profile(
        "remote",
        &[(
            "kitbag.toml",
            &format!("kitbag = 1\nname = \"remote\"\n{PLUGINS}"),
        )],
    )
}

#[test]
fn from_a_git_source_plugins_and_marketplaces_need_trust_source() {
    if !has_git() {
        return;
    }
    let sb = Sandbox::empty();
    let url = remote_profile(&sb);
    let out = ok(&sb, &["apply", &url, "--to", "claude-code", "--yes"]);
    // Nothing written, and every marketplace and plugin is listed.
    assert!(!sb.path("home/.claude/settings.json").exists(), "{out}");
    for want in [
        "github acme/tools, ref v1.2.0",
        "https://git.example.com/team/plugins.git",
        "plugin 'formatter@acme-tools'",
        "plugin 'review@team'",
        "--trust-source",
    ] {
        assert!(out.contains(want), "{want}: {out}");
    }
    let out = ok(
        &sb,
        &[
            "apply",
            &url,
            "--to",
            "claude-code",
            "--yes",
            "--trust-source",
        ],
    );
    let s = sb.json("home/.claude/settings.json");
    assert_eq!(s["enabledPlugins"]["review@team"], json!(true));
    assert!(
        s["extraKnownMarketplaces"]["acme-tools"].is_object(),
        "{out}"
    );
}

#[test]
fn inspect_lists_what_the_tool_would_fetch_without_writing() {
    let sb = Sandbox::empty();
    profile_with(&sb, "p", PLUGINS);
    let out = ok(&sb, &["inspect", "p", "--to", "claude-code"]);
    assert!(out.contains("plugins (4)"), "{out}");
    assert!(out.contains("github acme/tools, ref v1.2.0"), "{out}");
    assert!(out.contains("hooks, MCP servers, commands"), "{out}");
    assert!(!sb.path("home/.claude").exists());
    let json = ok(&sb, &["inspect", "p", "--to", "claude-code", "--json"]);
    let v: Value = serde_json::from_str(&json).unwrap();
    assert_eq!(v["plugins"].as_array().unwrap().len(), 4, "{json}");
}

// ----------------------------------------------------------- re-capture

#[test]
fn capture_force_keeps_the_targeting_of_plugins_it_captures_again() {
    let sb = claude_with_plugins();
    ok(&sb, &["capture", "--from", "claude-code", "--out", "cap"]);
    let manifest = sb.path("cap/kitbag.toml");
    let text = fs::read_to_string(&manifest).unwrap();
    let text = text.replace(
        "[plugins.claude-code.enabled.\"formatter@acme-tools\"]\n",
        "[plugins.claude-code.enabled.\"formatter@acme-tools\"]\ndisabled_on = [\"cursor\"]\n",
    );
    assert!(text.contains("disabled_on"), "{text}");
    fs::write(&manifest, text).unwrap();
    let out = ok(
        &sb,
        &[
            "capture",
            "--from",
            "claude-code",
            "--out",
            "cap",
            "--force",
        ],
    );
    assert!(out.contains("plugin formatter@acme-tools"), "{out}");
    let p = kitbag::profile_io::load(&sb.path("cap")).unwrap();
    assert_eq!(
        p.plugins["claude-code"].enabled["formatter@acme-tools"].disabled_on,
        ["cursor"]
    );
}
