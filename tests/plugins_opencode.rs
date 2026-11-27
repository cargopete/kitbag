//! opencode plugins, carried by reference: the npm packages listed in the
//! `plugins` array (`plugin` in opencode 1) of its config, never plugin files.
//! Everything here is offline: kitbag only edits the config, and the sandbox
//! has no `opencode` and no network.

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

const CONFIG: &str = "home/.config/opencode/opencode.json";

const PLUGINS: &str = r#"
[plugins.opencode.enabled."opencode-notify"]

[plugins.opencode.enabled."@acme/opencode-plugin@1.2.0"]
"#;

/// A user whose opencode config lists plugins in every form.
fn opencode_with_plugins() -> Sandbox {
    let sb = Sandbox::empty();
    write_json(
        &sb,
        CONFIG,
        &json!({
            "model": "anthropic/claude-sonnet-5-5",
            "plugins": [
                "opencode-notify",
                "@acme/opencode-plugin@1.2.0",
                {"package": "opencode-keyed", "options": {"apiKey": "FAKE-KEY-123"}},
                {"package": "opencode-plain"},
                "./plugins/local",
                "file:///home/me/plugins/mine",
                "-opencode-off",
                "opencode-notify"
            ],
            "plugin": [["opencode-tuple", {"token": "FAKE-TOKEN-456"}]]
        }),
    );
    // Plugin code, which must never reach a profile.
    write(
        &sb,
        "home/.config/opencode/plugins/mine.ts",
        "export const Mine = 'PLUGIN-CODE'\n",
    );
    sb
}

fn capture(sb: &Sandbox, extra: &[&str]) -> String {
    let mut args = vec!["capture", "--from", "opencode", "--out", "cap"];
    args.extend(extra);
    ok(sb, &args)
}

// ---------------------------------------------------------------- capture

#[test]
fn capture_carries_npm_plugins_by_reference_and_nothing_else() {
    let sb = opencode_with_plugins();
    let out = capture(&sb, &[]);
    let p = kitbag::profile_io::load(&sb.path("cap")).unwrap();
    let set = &p.plugins["opencode"];
    assert!(set.marketplaces.is_empty());
    assert_eq!(
        set.enabled.keys().cloned().collect::<Vec<_>>(),
        [
            "@acme/opencode-plugin@1.2.0",
            "opencode-keyed",
            "opencode-notify",
            "opencode-plain",
            "opencode-tuple"
        ]
    );
    assert!(out.contains(", 5 plugin(s)"), "{out}");
    // Local paths, a '-' entry and options are left out, and the plan says so.
    assert!(
        out.contains("not carried") && out.contains("./plugins/local"),
        "{out}"
    );
    assert!(out.contains("file:///home/me/plugins/mine"), "{out}");
    assert!(out.contains("turned off with a '-' prefix"), "{out}");
    assert!(out.contains("options of 2 plugin(s)"), "{out}");
    assert!(
        out.contains("plugins/") && out.contains("not copied"),
        "{out}"
    );
    // Neither the options (they can hold keys) nor the plugin's code is in the profile.
    for (rel, text) in kitbag::profile_io::profile_files(&p) {
        let text = String::from_utf8_lossy(&text);
        for secret in ["FAKE-KEY-123", "FAKE-TOKEN-456", "PLUGIN-CODE", "apiKey"] {
            assert!(!text.contains(secret), "{rel} has {secret}");
        }
    }
    assert!(!sb.path("cap/plugins").exists());
}

#[test]
fn capture_reads_opencode_1s_singular_key_and_a_project_config() {
    let sb = Sandbox::empty();
    write_json(&sb, CONFIG, &json!({"plugin": ["opencode-old@0.3.0"]}));
    write_json(
        &sb,
        "project/.opencode/opencode.json",
        &json!({"plugins": ["opencode-proj"]}),
    );
    capture(&sb, &[]);
    let p = kitbag::profile_io::load(&sb.path("cap")).unwrap();
    assert_eq!(
        p.plugins["opencode"]
            .enabled
            .keys()
            .cloned()
            .collect::<Vec<_>>(),
        ["opencode-old@0.3.0"]
    );
    ok(
        &sb,
        &[
            "capture",
            "--from",
            "opencode",
            "--out",
            "proj",
            "--project",
            "project",
        ],
    );
    let p = kitbag::profile_io::load(&sb.path("proj")).unwrap();
    assert_eq!(
        p.plugins["opencode"]
            .enabled
            .keys()
            .cloned()
            .collect::<Vec<_>>(),
        ["opencode-proj"]
    );
}

#[test]
fn a_config_without_plugins_captures_none() {
    let sb = Sandbox::empty();
    write_json(&sb, CONFIG, &json!({"model": "x"}));
    let out = capture(&sb, &[]);
    assert!(!out.contains("plugin"), "{out}");
}

#[test]
fn capture_force_keeps_the_targeting_of_plugins_it_captures_again() {
    let sb = opencode_with_plugins();
    capture(&sb, &[]);
    let toml = sb.read("cap/kitbag.toml");
    let header = "[plugins.opencode.enabled.opencode-notify]";
    assert!(toml.contains(header), "{toml}");
    write(
        &sb,
        "cap/kitbag.toml",
        &toml.replace(header, &format!("{header}\ndisabled_on = [\"cursor\"]")),
    );
    let before = kitbag::profile_io::load(&sb.path("cap")).unwrap();
    assert_eq!(
        before.plugins["opencode"].enabled["opencode-notify"].disabled_on,
        ["cursor"]
    );
    ok(
        &sb,
        &["capture", "--from", "opencode", "--out", "cap", "--force"],
    );
    let after = kitbag::profile_io::load(&sb.path("cap")).unwrap();
    assert_eq!(
        after.plugins["opencode"].enabled["opencode-notify"].disabled_on,
        ["cursor"]
    );
}

// ------------------------------------------------------------------ apply

#[test]
fn apply_creates_the_plugins_array_and_says_what_is_left() {
    let sb = Sandbox::empty();
    profile_with(&sb, "p", PLUGINS);
    let out = ok(&sb, &["apply", "p", "--to", "opencode", "--yes"]);
    assert_eq!(
        sb.json(CONFIG),
        json!({"plugins": ["@acme/opencode-plugin@1.2.0", "opencode-notify"]})
    );
    assert!(out.contains("plugin 'opencode-notify'"), "{out}");
    assert!(out.contains("kitbag doesn't install plugins"), "{out}");
    // No plugin folder, cache or install happened.
    assert!(!sb.path("home/.config/opencode/plugins").exists());
    assert!(!sb.path("home/.cache").exists());
    assert!(!sb.path("project/.opencode").exists());
}

#[test]
fn apply_appends_to_the_array_in_use_and_keeps_the_rest_of_the_file() {
    let sb = Sandbox::empty();
    profile_with(&sb, "p", PLUGINS);
    write(
        &sb,
        CONFIG,
        "{\n  // my model\n  \"model\": \"x\",\n  \"plugins\": [\"mine-already\", \"opencode-notify\"]\n}\n",
    );
    ok(&sb, &["apply", "p", "--to", "opencode", "--yes"]);
    let text = sb.read(CONFIG);
    assert!(text.contains("// my model"), "{text}");
    assert_eq!(
        kitbag::edit::json::parse_value(Some(&text), &sb.path(CONFIG)).unwrap()["plugins"],
        json!([
            "mine-already",
            "opencode-notify",
            "@acme/opencode-plugin@1.2.0"
        ])
    );
}

#[test]
fn apply_writes_opencode_1s_singular_key_when_the_file_uses_it() {
    let sb = Sandbox::empty();
    profile_with(&sb, "p", PLUGINS);
    write_json(&sb, CONFIG, &json!({"plugin": ["mine"]}));
    ok(&sb, &["apply", "p", "--to", "opencode", "--yes"]);
    let v = sb.json(CONFIG);
    assert_eq!(
        v["plugin"],
        json!(["mine", "@acme/opencode-plugin@1.2.0", "opencode-notify"])
    );
    assert!(v.get("plugins").is_none());

    // So does a file whose MCP servers are in opencode 1's layout.
    let sb = Sandbox::empty();
    profile_with(&sb, "p", PLUGINS);
    write_json(
        &sb,
        CONFIG,
        &json!({"mcp": {"old": {"type": "local", "command": ["x"]}}}),
    );
    ok(&sb, &["apply", "p", "--to", "opencode", "--yes"]);
    let v = sb.json(CONFIG);
    assert_eq!(v["plugin"].as_array().unwrap().len(), 2);
    assert!(v.get("plugins").is_none());
}

#[test]
fn a_second_apply_is_up_to_date_and_undo_restores_the_file() {
    let sb = Sandbox::empty();
    profile_with(&sb, "p", PLUGINS);
    let original = "{\n  \"model\": \"x\",\n  \"plugins\": [\"mine-already\"]\n}\n";
    write(&sb, CONFIG, original);
    ok(&sb, &["apply", "p", "--to", "opencode", "--yes"]);
    assert_eq!(sb.json(CONFIG)["plugins"].as_array().unwrap().len(), 3);
    let again = ok(&sb, &["apply", "p", "--to", "opencode", "--yes"]);
    assert!(again.to_lowercase().contains("up to date"), "{again}");
    assert!(!again.contains("kitbag doesn't install plugins"), "{again}");
    ok(&sb, &["undo"]);
    assert_eq!(sb.read(CONFIG), original);
}

#[test]
fn undo_after_a_fresh_apply_removes_the_config_kitbag_created() {
    let sb = Sandbox::empty();
    profile_with(&sb, "p", PLUGINS);
    ok(&sb, &["apply", "p", "--to", "opencode", "--yes"]);
    ok(&sb, &["undo"]);
    assert!(!sb.path(CONFIG).exists());
}

#[test]
fn undo_does_not_overwrite_plugins_the_user_added_afterwards() {
    let sb = Sandbox::empty();
    profile_with(&sb, "p", PLUGINS);
    ok(&sb, &["apply", "p", "--to", "opencode", "--yes"]);
    let mut v = sb.json(CONFIG);
    v["plugins"].as_array_mut().unwrap().push(json!("by-hand"));
    write_json(&sb, CONFIG, &v);
    let (good, out) = run(&sb, &["undo"]);
    assert!(!good && out.contains("modified after"), "{out}");
    assert!(sb.read(CONFIG).contains("by-hand"));
}

#[test]
fn a_package_already_listed_is_left_alone_in_whatever_form() {
    let sb = Sandbox::empty();
    profile_with(&sb, "p", PLUGINS);
    let original = json!({"plugins": [
        {"package": "opencode-notify", "options": {"quiet": true}},
        ["@acme/opencode-plugin", {"x": 1}]
    ]});
    write_json(&sb, CONFIG, &original);
    let out = ok(&sb, &["apply", "p", "--to", "opencode", "--yes"]);
    assert_eq!(sb.json(CONFIG), original);
    assert!(out.to_lowercase().contains("up to date"), "{out}");
}

#[test]
fn a_different_pin_replaces_a_plain_entry_but_not_with_no_overwrite_or_options() {
    let sb = Sandbox::empty();
    profile_with(&sb, "p", PLUGINS);
    write_json(
        &sb,
        CONFIG,
        &json!({"plugins": ["@acme/opencode-plugin@1.0.0", "opencode-notify@2.0.0"]}),
    );
    // --no-overwrite keeps the user's pin and says so.
    let out = ok(
        &sb,
        &["apply", "p", "--to", "opencode", "--yes", "--no-overwrite"],
    );
    assert_eq!(
        sb.json(CONFIG)["plugins"],
        json!(["@acme/opencode-plugin@1.0.0", "opencode-notify@2.0.0"])
    );
    assert!(out.contains("--no-overwrite"), "{out}");
    // Without it, the profile's pin wins; an unpinned profile entry never touches a pin.
    ok(&sb, &["apply", "p", "--to", "opencode", "--yes"]);
    assert_eq!(
        sb.json(CONFIG)["plugins"],
        json!(["@acme/opencode-plugin@1.2.0", "opencode-notify@2.0.0"])
    );

    // An entry with options is never rewritten.
    let sb = Sandbox::empty();
    profile_with(&sb, "p", PLUGINS);
    let original = json!({"plugins": [{"package": "@acme/opencode-plugin@1.0.0", "options": {"a": 1}}, "opencode-notify"]});
    write_json(&sb, CONFIG, &original);
    let out = ok(&sb, &["apply", "p", "--to", "opencode", "--yes"]);
    assert_eq!(sb.json(CONFIG), original);
    assert!(out.contains("options"), "{out}");
}

#[test]
fn a_plugin_the_user_turned_off_with_a_dash_is_not_turned_back_on() {
    let sb = Sandbox::empty();
    profile_with(&sb, "p", PLUGINS);
    write_json(&sb, CONFIG, &json!({"plugins": ["-opencode-notify"]}));
    let out = ok(&sb, &["apply", "p", "--to", "opencode", "--yes"]);
    assert_eq!(
        sb.json(CONFIG)["plugins"],
        json!(["-opencode-notify", "@acme/opencode-plugin@1.2.0"])
    );
    assert!(out.contains("turned off there"), "{out}");
}

#[test]
fn project_scope_writes_the_projects_config_only() {
    let sb = Sandbox::empty();
    profile_with(&sb, "p", PLUGINS);
    ok(
        &sb,
        &[
            "apply",
            "p",
            "--to",
            "opencode",
            "--yes",
            "--project",
            "project",
        ],
    );
    assert_eq!(
        sb.json("project/opencode.json")["plugins"],
        json!(["@acme/opencode-plugin@1.2.0", "opencode-notify"])
    );
    assert!(!sb.path(CONFIG).exists());
}

#[test]
fn a_config_with_a_non_list_plugins_key_is_left_alone() {
    let sb = Sandbox::empty();
    profile_with(&sb, "p", PLUGINS);
    write(&sb, CONFIG, "{\"plugins\": {\"a\": 1}}\n");
    let (good, out) = run(&sb, &["apply", "p", "--to", "opencode", "--yes"]);
    assert!(!good && out.contains("not a list"), "{out}");
    assert_eq!(sb.read(CONFIG), "{\"plugins\": {\"a\": 1}}\n");
}

#[cfg(unix)]
#[test]
fn apply_through_a_symlinked_project_config_does_not_leave_the_project() {
    let sb = Sandbox::empty();
    profile_with(&sb, "p", PLUGINS);
    write(&sb, "outside/opencode.json", "{}\n");
    fs::create_dir_all(sb.path("project")).unwrap();
    std::os::unix::fs::symlink(
        sb.path("outside/opencode.json"),
        sb.path("project/opencode.json"),
    )
    .unwrap();
    let (good, out) = run(
        &sb,
        &[
            "apply",
            "p",
            "--to",
            "opencode",
            "--yes",
            "--project",
            "project",
        ],
    );
    assert_eq!(sb.read("outside/opencode.json"), "{}\n", "{good} {out}");
}

#[test]
fn an_item_disabled_on_opencode_or_targeted_elsewhere_is_not_applied() {
    let sb = Sandbox::empty();
    profile_with(
        &sb,
        "p",
        r#"
[plugins.opencode.enabled."a"]
targets = ["codex"]

[plugins.opencode.enabled."b"]

[plugins.opencode.enabled."c"]
disabled_on = ["opencode"]
"#,
    );
    let out = ok(&sb, &["apply", "p", "--to", "opencode", "--yes"]);
    assert_eq!(sb.json(CONFIG)["plugins"], json!(["b"]));
    assert!(out.contains("plugin 'a' skipped"), "{out}");
    assert!(out.contains("plugin 'c' skipped"), "{out}");
}

#[test]
fn applying_to_another_tool_writes_no_opencode_config() {
    let sb = Sandbox::empty();
    profile_with(&sb, "p", PLUGINS);
    ok(&sb, &["apply", "p", "--to", "codex,claude-code", "--yes"]);
    assert!(!sb.path("home/.config/opencode").exists());
    assert!(!sb.path("home/.claude/settings.json").exists());
}

#[test]
fn a_profile_captured_from_opencode_applies_back_to_a_clean_machine() {
    let sb = opencode_with_plugins();
    capture(&sb, &[]);
    let other = Sandbox::empty();
    let toml = sb.read("cap/kitbag.toml");
    write(&other, "p/kitbag.toml", &toml);
    ok(&other, &["apply", "p", "--to", "opencode", "--yes"]);
    let v = other.json(CONFIG);
    assert_eq!(v["plugins"].as_array().unwrap().len(), 5);
    assert!(
        !other.read(CONFIG).contains("FAKE-"),
        "options are not carried"
    );
    assert!(!other.path("home/.config/opencode/plugins").exists());
    let again = ok(&other, &["apply", "p", "--to", "opencode", "--yes"]);
    assert!(again.to_lowercase().contains("up to date"), "{again}");
}

// ------------------------------------------------------------ validation

#[test]
fn a_manifest_with_a_bad_opencode_plugin_is_refused() {
    let cases: &[(&str, &str)] = &[
        (
            "[plugins.opencode.enabled.\"./plugins/local\"]\n",
            "not an npm package spec",
        ),
        (
            "[plugins.opencode.enabled.\"file:///x\"]\n",
            "not an npm package spec",
        ),
        (
            "[plugins.opencode.enabled.\"x@github:u/r\"]\n",
            "not an npm package spec",
        ),
        (
            "[plugins.opencode.enabled.\"opencode-x\"]\nversion = \"1.0.0\"\n",
            "version is not used",
        ),
        (
            "[plugins.opencode.marketplaces.m]\nsource = { source = \"github\", repo = \"a/b\" }\n",
            "no plugin marketplaces",
        ),
        (
            "[plugins.opencode.enabled.\"x\"]\ntargets = [\"codex\"]\ndisabled_on = [\"cursor\"]\n",
            "not both",
        ),
    ];
    for (toml, want) in cases {
        let sb = Sandbox::empty();
        profile_with(&sb, "p", toml);
        let (good, out) = run(&sb, &["apply", "p", "--to", "opencode", "--yes"]);
        assert!(!good, "{toml}: {out}");
        assert!(out.contains(want), "{toml}: wanted {want:?} in {out}");
        assert!(!sb.path(CONFIG).exists());
    }
}

// ------------------------------------------------------------------ trust

#[test]
fn from_a_git_source_opencode_plugins_need_trust_source() {
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
    let out = ok(&sb, &["apply", &url, "--to", "opencode", "--yes"]);
    assert!(!sb.path(CONFIG).exists(), "{out}");
    for want in [
        "plugin 'opencode-notify'",
        "plugin '@acme/opencode-plugin@1.2.0'",
        "--trust-source",
    ] {
        assert!(out.contains(want), "{want}: {out}");
    }
    ok(
        &sb,
        &["apply", &url, "--to", "opencode", "--yes", "--trust-source"],
    );
    assert_eq!(sb.json(CONFIG)["plugins"].as_array().unwrap().len(), 2);
}

#[test]
fn inspect_lists_what_opencode_would_install_without_writing() {
    let sb = Sandbox::empty();
    profile_with(&sb, "p", PLUGINS);
    let out = ok(&sb, &["inspect", "p", "--to", "opencode"]);
    assert!(out.contains("plugins (2)"), "{out}");
    assert!(
        out.contains("npm package opencode installs itself"),
        "{out}"
    );
    assert!(!sb.path("home/.config/opencode").exists());
}
