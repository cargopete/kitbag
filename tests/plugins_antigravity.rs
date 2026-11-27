//! Antigravity plugins, carried by reference: the plugin folders listed in the
//! `entries` of its `plugins.json` discovery manifest (`~/.gemini/config/` or
//! `.agents/`), never plugin files. Everything here is offline: kitbag only
//! edits the manifest, and the sandbox has no `agy` and no network.

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

fn write(sb: &Sandbox, rel: &str, text: &str) {
    let p = sb.path(rel);
    fs::create_dir_all(p.parent().unwrap()).unwrap();
    fs::write(p, text).unwrap();
}

fn write_json(sb: &Sandbox, rel: &str, v: &Value) {
    write(sb, rel, &serde_json::to_string_pretty(v).unwrap());
}

fn profile_with(sb: &Sandbox, dir: &str, toml: &str) {
    write(
        sb,
        &format!("{dir}/kitbag.toml"),
        &format!("kitbag = 1\nname = \"p\"\n{toml}"),
    );
}

/// A path as JSON holds it, with forward slashes.
fn fwd(rel: &str, sb: &Sandbox) -> String {
    sb.path(rel).to_string_lossy().replace('\\', "/")
}

const MANIFEST: &str = "home/.gemini/config/plugins.json";
const PROJECT_MANIFEST: &str = "project/.agents/plugins.json";

const PLUGINS: &str = r#"
[plugins.antigravity.enabled."~/team/plugins/formatter"]

[plugins.antigravity.enabled."~/team/plugins/linter"]
include_only = ["strict"]
exclude = ["slow"]
"#;

/// The paths of the entries of a plugins.json, with forward slashes.
fn entry_paths(v: &Value) -> Vec<String> {
    v["entries"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e["path"].as_str().unwrap().replace('\\', "/"))
        .collect()
}

fn capture(sb: &Sandbox, extra: &[&str]) -> String {
    let mut args = vec!["capture", "--from", "antigravity", "--out", "cap"];
    args.extend(extra);
    ok(sb, &args)
}

/// A user whose plugins.json lists folders in several forms, and who has
/// plugins installed by `agy plugin install`.
fn antigravity_with_plugins() -> Sandbox {
    let sb = Sandbox::empty();
    write_json(
        &sb,
        MANIFEST,
        &json!({
            "entries": [
                {"path": fwd("home/team/plugins/formatter", &sb)},
                {"path": fwd("home/team/plugins/linter", &sb), "include_only": ["strict"], "exclude": ["slow"]},
                {"path": "shared-plugins"},
                {"path": fwd("elsewhere/plugins", &sb)},
                {"path": "~/never-expanded"},
                {"include_only": ["no-path"]},
                {"path": "bad-filter", "include_only": "not-a-list"}
            ],
            "inherits": [{"path": "/somewhere/else/plugins.json"}],
            "exclude": ["old"]
        }),
    );
    // Installed plugins: their code must never reach a profile.
    write(
        &sb,
        "home/.gemini/config/plugins/installed-one/plugin.json",
        "{\"name\": \"installed-one\"}\n",
    );
    write(
        &sb,
        "home/.gemini/config/plugins/installed-one/hooks.json",
        "{\"note\": \"PLUGIN-CODE\"}\n",
    );
    write(
        &sb,
        "home/.gemini/config/plugins/installed-two/plugin.json",
        "{\"name\": \"installed-two\"}\n",
    );
    sb
}

// ---------------------------------------------------------------- capture

#[test]
fn capture_carries_plugin_folders_by_reference_and_nothing_else() {
    let sb = antigravity_with_plugins();
    let out = capture(&sb, &[]);
    let p = kitbag::profile_io::load(&sb.path("cap")).unwrap();
    let set = &p.plugins["antigravity"];
    assert!(set.marketplaces.is_empty());
    let mut keys: Vec<String> = set.enabled.keys().map(|k| k.replace('\\', "/")).collect();
    keys.sort_by_key(|k| (!k.starts_with('~'), k.clone()));
    let elsewhere = fwd("elsewhere/plugins", &sb);
    assert_eq!(
        keys,
        [
            "~/.gemini/config/shared-plugins",
            "~/team/plugins/formatter",
            "~/team/plugins/linter",
            elsewhere.as_str()
        ]
    );
    let linter = &set.enabled["~/team/plugins/linter"];
    assert_eq!(linter.include_only, ["strict"]);
    assert_eq!(linter.exclude, ["slow"]);
    assert!(out.contains(", 4 plugin(s)"), "{out}");
    // What isn't carried is said so.
    assert!(out.contains("doesn't expand '~'"), "{out}");
    assert!(out.contains("2 plugin entries"), "{out}");
    assert!(out.contains("'inherits'"), "{out}");
    assert!(out.contains("top-level 'exclude'"), "{out}");
    assert!(
        out.contains("2 plugin folder(s)") && out.contains("agy plugin install"),
        "{out}"
    );
    // No plugin code was read into the profile.
    assert!(!all_text_under(&sb.path("cap")).contains("PLUGIN-CODE"));
    assert!(!sb.path("cap/plugins").exists());
}

#[test]
fn capture_at_project_scope_keeps_only_paths_inside_the_project() {
    let sb = Sandbox::empty();
    write_json(
        &sb,
        PROJECT_MANIFEST,
        &json!({"entries": [
            {"path": "tools/plugins/fmt/"},
            {"path": "./tools/./plugins/lint", "exclude": ["slow"]},
            {"path": "../outside"},
            {"path": fwd("home/abs", &sb)}
        ]}),
    );
    write(&sb, "project/.agents/mcp_config.json", "{}\n");
    ok(
        &sb,
        &[
            "capture",
            "--from",
            "antigravity",
            "--out",
            "proj",
            "--project",
            "project",
        ],
    );
    let p = kitbag::profile_io::load(&sb.path("proj")).unwrap();
    assert_eq!(
        p.plugins["antigravity"]
            .enabled
            .keys()
            .cloned()
            .collect::<Vec<_>>(),
        ["tools/plugins/fmt", "tools/plugins/lint"]
    );
}

#[test]
fn a_machine_without_plugins_json_captures_none() {
    let sb = Sandbox::empty();
    write(
        &sb,
        "home/.gemini/config/mcp_config.json",
        "{\"mcpServers\": {\"fake\": {\"command\": \"echo\"}}}\n",
    );
    capture(&sb, &[]);
    let p = kitbag::profile_io::load(&sb.path("cap")).unwrap();
    assert!(p.plugins.is_empty());
}

#[test]
fn capture_force_keeps_the_targeting_of_plugins_it_captures_again() {
    let sb = antigravity_with_plugins();
    capture(&sb, &[]);
    let toml = sb.read("cap/kitbag.toml");
    let edited = toml.replace(
        "[plugins.antigravity.enabled.\"~/team/plugins/formatter\"]",
        "[plugins.antigravity.enabled.\"~/team/plugins/formatter\"]\ndisabled_on = [\"antigravity\"]",
    );
    assert_ne!(toml, edited, "{toml}");
    fs::write(sb.path("cap/kitbag.toml"), edited).unwrap();
    capture(&sb, &["--force"]);
    let p = kitbag::profile_io::load(&sb.path("cap")).unwrap();
    assert_eq!(
        p.plugins["antigravity"].enabled["~/team/plugins/formatter"].disabled_on,
        ["antigravity"]
    );
}

#[test]
fn capture_then_apply_on_a_clean_machine_writes_the_same_folders() {
    let sb = antigravity_with_plugins();
    capture(&sb, &[]);
    let other = Sandbox::empty();
    fs::create_dir_all(other.path("p")).unwrap();
    common::copy_dir(&sb.path("cap"), &other.path("p"));
    ok(&other, &["apply", "p", "--to", "antigravity", "--yes"]);
    let paths = entry_paths(&other.json(MANIFEST));
    assert_eq!(paths.len(), 4, "{paths:?}");
    assert!(
        paths.contains(&fwd("home/team/plugins/formatter", &other)),
        "{paths:?}"
    );
    assert!(
        paths.contains(&fwd("home/.gemini/config/shared-plugins", &other)),
        "{paths:?}"
    );
}

// ------------------------------------------------------------------ apply

#[test]
fn apply_creates_the_manifest_and_says_what_is_left() {
    let sb = Sandbox::empty();
    profile_with(&sb, "p", PLUGINS);
    let out = ok(&sb, &["apply", "p", "--to", "antigravity", "--yes"]);
    let v = sb.json(MANIFEST);
    assert_eq!(
        entry_paths(&v),
        [
            fwd("home/team/plugins/formatter", &sb),
            fwd("home/team/plugins/linter", &sb)
        ]
    );
    assert_eq!(v["entries"][1]["include_only"], json!(["strict"]));
    assert_eq!(v["entries"][1]["exclude"], json!(["slow"]));
    assert!(v["entries"][0].get("include_only").is_none());
    assert!(
        out.contains("plugin folder") && out.contains("team") && out.contains("formatter"),
        "{out}"
    );
    assert!(
        out.contains("kitbag doesn't install or enable plugins"),
        "{out}"
    );
    assert!(out.contains("doesn't exist on this machine"), "{out}");
    // No plugin folder, install or other config was written.
    assert!(!sb.path("home/.gemini/config/plugins").exists());
    assert!(!sb.path("home/team").exists());
    assert!(!sb.path("project/.agents").exists());
}

#[test]
fn a_folder_that_exists_draws_no_missing_warning() {
    let sb = Sandbox::empty();
    profile_with(
        &sb,
        "p",
        "[plugins.antigravity.enabled.\"~/team/plugins/formatter\"]\n",
    );
    fs::create_dir_all(sb.path("home/team/plugins/formatter")).unwrap();
    let out = ok(&sb, &["apply", "p", "--to", "antigravity", "--yes"]);
    assert!(!out.contains("doesn't exist on this machine"), "{out}");
}

#[test]
fn apply_appends_and_keeps_the_rest_of_the_file() {
    let sb = Sandbox::empty();
    profile_with(&sb, "p", PLUGINS);
    write_json(
        &sb,
        MANIFEST,
        &json!({
            "entries": [{"path": "mine", "exclude": ["x"]}],
            "inherits": [{"path": "/team/plugins.json"}]
        }),
    );
    ok(&sb, &["apply", "p", "--to", "antigravity", "--yes"]);
    let v = sb.json(MANIFEST);
    assert_eq!(entry_paths(&v).len(), 3);
    assert_eq!(v["entries"][0], json!({"path": "mine", "exclude": ["x"]}));
    assert_eq!(v["inherits"], json!([{"path": "/team/plugins.json"}]));
}

#[test]
fn a_second_apply_is_up_to_date_and_undo_restores_the_file() {
    let sb = Sandbox::empty();
    profile_with(&sb, "p", PLUGINS);
    let original = "{\n  \"entries\": [{\"path\": \"mine\"}]\n}\n";
    write(&sb, MANIFEST, original);
    ok(&sb, &["apply", "p", "--to", "antigravity", "--yes"]);
    assert_eq!(sb.json(MANIFEST)["entries"].as_array().unwrap().len(), 3);
    let again = ok(&sb, &["apply", "p", "--to", "antigravity", "--yes"]);
    assert!(again.to_lowercase().contains("up to date"), "{again}");
    assert!(!again.contains("kitbag doesn't install"), "{again}");
    ok(&sb, &["undo"]);
    assert_eq!(sb.read(MANIFEST), original);
}

#[test]
fn undo_after_a_fresh_apply_removes_the_manifest_kitbag_created() {
    let sb = Sandbox::empty();
    profile_with(&sb, "p", PLUGINS);
    ok(&sb, &["apply", "p", "--to", "antigravity", "--yes"]);
    ok(&sb, &["undo"]);
    assert!(!sb.path(MANIFEST).exists());
}

#[test]
fn undo_does_not_overwrite_entries_the_user_added_afterwards() {
    let sb = Sandbox::empty();
    profile_with(&sb, "p", PLUGINS);
    ok(&sb, &["apply", "p", "--to", "antigravity", "--yes"]);
    let mut v = sb.json(MANIFEST);
    v["entries"]
        .as_array_mut()
        .unwrap()
        .push(json!({"path": "by-hand"}));
    write_json(&sb, MANIFEST, &v);
    let (good, out) = run(&sb, &["undo"]);
    assert!(!good && out.contains("modified after"), "{out}");
    assert!(sb.read(MANIFEST).contains("by-hand"));
}

#[test]
fn a_folder_already_listed_is_left_alone_however_it_is_written() {
    let sb = Sandbox::empty();
    profile_with(&sb, "p", PLUGINS);
    // The same two folders: one absolute with a trailing slash, one relative
    // to ~/.gemini/config/ with `..` in it.
    write_json(
        &sb,
        MANIFEST,
        &json!({"entries": [
            {"path": format!("{}/", fwd("home/team/plugins/formatter", &sb))},
            {"path": "../../team/plugins/linter", "include_only": ["strict"], "exclude": ["slow"]}
        ]}),
    );
    let before = sb.read(MANIFEST);
    let out = ok(&sb, &["apply", "p", "--to", "antigravity", "--yes"]);
    assert!(out.to_lowercase().contains("up to date"), "{out}");
    assert_eq!(sb.read(MANIFEST), before);
}

#[test]
fn different_filters_replace_an_entry_but_not_with_no_overwrite() {
    let sb = Sandbox::empty();
    profile_with(&sb, "p", PLUGINS);
    let start = json!({"entries": [
        {"path": fwd("home/team/plugins/linter", &sb), "include_only": ["old"], "note": "mine"}
    ]});
    write_json(&sb, MANIFEST, &start);
    let out = ok(
        &sb,
        &[
            "apply",
            "p",
            "--to",
            "antigravity",
            "--yes",
            "--no-overwrite",
        ],
    );
    assert!(out.contains("--no-overwrite"), "{out}");
    assert_eq!(
        sb.json(MANIFEST)["entries"][0]["include_only"],
        json!(["old"])
    );
    ok(&sb, &["apply", "p", "--to", "antigravity", "--yes"]);
    let e = &sb.json(MANIFEST)["entries"][0];
    assert_eq!(e["include_only"], json!(["strict"]));
    assert_eq!(e["exclude"], json!(["slow"]));
    // The user's other keys stay.
    assert_eq!(e["note"], json!("mine"));
}

#[test]
fn a_manifest_with_a_non_list_entries_key_is_left_alone() {
    let sb = Sandbox::empty();
    profile_with(&sb, "p", PLUGINS);
    write(&sb, MANIFEST, "{\"entries\": {\"a\": 1}}\n");
    let (good, out) = run(&sb, &["apply", "p", "--to", "antigravity", "--yes"]);
    assert!(!good && out.contains("not a list"), "{out}");
    assert_eq!(sb.read(MANIFEST), "{\"entries\": {\"a\": 1}}\n");
}

#[test]
fn project_scope_writes_relative_paths_to_the_workspace_manifest_only() {
    let sb = Sandbox::empty();
    profile_with(
        &sb,
        "p",
        r#"
[plugins.antigravity.enabled."tools/plugins/fmt"]
exclude = ["slow"]

[plugins.antigravity.enabled."../outside"]

[plugins.antigravity.enabled."~/team/plugins/formatter"]
"#,
    );
    let out = ok(
        &sb,
        &[
            "apply",
            "p",
            "--to",
            "antigravity",
            "--yes",
            "--project",
            "project",
        ],
    );
    let v = sb.json(PROJECT_MANIFEST);
    assert_eq!(
        v,
        json!({"entries": [{"path": "tools/plugins/fmt", "exclude": ["slow"]}]})
    );
    assert!(out.contains("not written"), "{out}");
    assert!(out.contains("outside"), "{out}");
    assert!(!sb.path(MANIFEST).exists());
}

#[test]
fn a_relative_path_is_not_written_at_user_scope() {
    let sb = Sandbox::empty();
    profile_with(
        &sb,
        "p",
        "[plugins.antigravity.enabled.\"tools/plugins/fmt\"]\n",
    );
    let out = ok(&sb, &["apply", "p", "--to", "antigravity", "--yes"]);
    assert!(out.contains("not written"), "{out}");
    assert!(!sb.path(MANIFEST).exists());
}

#[cfg(unix)]
#[test]
fn apply_through_a_symlinked_project_manifest_does_not_leave_the_project() {
    let sb = Sandbox::empty();
    profile_with(
        &sb,
        "p",
        "[plugins.antigravity.enabled.\"tools/plugins/fmt\"]\n",
    );
    write(&sb, "outside/plugins.json", "{}\n");
    fs::create_dir_all(sb.path("project/.agents")).unwrap();
    std::os::unix::fs::symlink(
        sb.path("outside/plugins.json"),
        sb.path("project/.agents/plugins.json"),
    )
    .unwrap();
    let (good, out) = run(
        &sb,
        &[
            "apply",
            "p",
            "--to",
            "antigravity",
            "--yes",
            "--project",
            "project",
        ],
    );
    assert_eq!(sb.read("outside/plugins.json"), "{}\n", "{good} {out}");
}

#[test]
fn an_item_disabled_on_antigravity_or_targeted_elsewhere_is_not_applied() {
    let sb = Sandbox::empty();
    profile_with(
        &sb,
        "p",
        r#"
[plugins.antigravity.enabled."~/a"]
targets = ["codex"]

[plugins.antigravity.enabled."~/b"]

[plugins.antigravity.enabled."~/c"]
disabled_on = ["antigravity"]
"#,
    );
    let out = ok(&sb, &["apply", "p", "--to", "antigravity", "--yes"]);
    assert_eq!(entry_paths(&sb.json(MANIFEST)), [fwd("home/b", &sb)]);
    assert!(
        out.contains("plugin folder") && out.contains("skipped"),
        "{out}"
    );
}

#[test]
fn applying_to_another_tool_writes_no_antigravity_files() {
    let sb = Sandbox::empty();
    profile_with(&sb, "p", PLUGINS);
    ok(&sb, &["apply", "p", "--to", "codex,claude-code", "--yes"]);
    assert!(!sb.path("home/.gemini").exists());
    assert!(!sb.path("home/.claude/settings.json").exists());
}

// ------------------------------------------------------------ validation

#[test]
fn a_manifest_with_a_bad_antigravity_plugin_is_refused() {
    let cases: &[(&str, &str)] = &[
        (
            "[plugins.antigravity.enabled.\"~/x\"]\nversion = \"1.0.0\"\n",
            "version is not used",
        ),
        (
            "[plugins.antigravity.marketplaces.m]\nsource = { source = \"github\", repo = \"a/b\" }\n",
            "no marketplace setting",
        ),
        (
            "[plugins.antigravity.enabled.\"~someone/x\"]\n",
            "not a path",
        ),
        (
            "[plugins.antigravity.enabled.\"~/x\"]\ninclude_only = [\"\"]\n",
            "include_only must be",
        ),
        (
            "[plugins.opencode.enabled.\"opencode-x\"]\ninclude_only = [\"a\"]\n",
            "Antigravity plugins.json fields",
        ),
        (
            "[plugins.antigravity.enabled.\"~/x\"]\ntargets = [\"codex\"]\ndisabled_on = [\"cursor\"]\n",
            "not both",
        ),
    ];
    for (toml, want) in cases {
        let sb = Sandbox::empty();
        profile_with(&sb, "p", toml);
        let (good, out) = run(&sb, &["apply", "p", "--to", "antigravity", "--yes"]);
        assert!(!good, "{toml}: {out}");
        assert!(out.contains(want), "{toml}: wanted {want:?} in {out}");
        assert!(!sb.path(MANIFEST).exists());
    }
}

// ------------------------------------------------------------------ trust

#[test]
fn from_a_git_source_antigravity_plugins_need_trust_source() {
    if !has_git() {
        return;
    }
    let sb = Sandbox::empty();
    let url = sb.git_profile(
        "remote",
        &[(
            "kitbag.toml",
            &format!("kitbag = 1\nname = \"remote\"\n{PLUGINS}"),
        )],
    );
    let out = ok(&sb, &["apply", &url, "--to", "antigravity", "--yes"]);
    assert!(!sb.path(MANIFEST).exists(), "{out}");
    for want in [
        "plugin folder '~/team/plugins/formatter'",
        "plugin folder '~/team/plugins/linter'",
        "--trust-source",
    ] {
        assert!(out.contains(want), "{want}: {out}");
    }
    ok(
        &sb,
        &[
            "apply",
            &url,
            "--to",
            "antigravity",
            "--yes",
            "--trust-source",
        ],
    );
    assert_eq!(sb.json(MANIFEST)["entries"].as_array().unwrap().len(), 2);
}

#[test]
fn inspect_lists_what_antigravity_would_load_without_writing() {
    let sb = Sandbox::empty();
    profile_with(&sb, "p", PLUGINS);
    let out = ok(&sb, &["inspect", "p", "--to", "antigravity"]);
    assert!(out.contains("plugins (2)"), "{out}");
    assert!(out.contains("Antigravity loads as a plugin"), "{out}");
    assert!(!sb.path("home/.gemini").exists());
}
