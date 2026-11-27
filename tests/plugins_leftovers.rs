//! The plugin systems carried by reference after Claude Code, opencode and
//! Antigravity: Codex (`[marketplaces.*]` and `[plugins."name@market"]` in
//! `config.toml`), Copilot CLI (`enabledPlugins` and `extraKnownMarketplaces`
//! in `settings.json`), Zed (`auto_install_extensions`) and Grok Build
//! (`[plugins] paths`). Everything is offline: kitbag only edits settings
//! files, and the sandbox has no `codex`, `copilot`, `zed` or `grok`.

mod common;

use std::fs;

use common::*;
use kitbag::adapters::Scope;
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

fn profile_with(sb: &Sandbox, dir: &str, toml: &str) {
    write(
        sb,
        &format!("{dir}/kitbag.toml"),
        &format!("kitbag = 1\nname = \"p\"\n{toml}"),
    );
}

/// A path as text holds it, with forward slashes.
fn fwd(rel: &str, sb: &Sandbox) -> String {
    sb.path(rel).to_string_lossy().replace('\\', "/")
}

fn toml_of(sb: &Sandbox, rel: &str) -> toml::Table {
    sb.read(rel).parse().unwrap()
}

fn capture_from(sb: &Sandbox, tool: &str) -> (String, kitbag::model::Profile) {
    let out = ok(sb, &["capture", "--from", tool, "--out", "cap"]);
    (out, kitbag::profile_io::load(&sb.path("cap")).unwrap())
}

// ===================================================================== Codex

const CODEX: &str = "home/.codex/config.toml";

const CODEX_PLUGINS: &str = r#"
[plugins.codex.marketplaces.acme]
source = { source_type = "git", source = "https://github.com/acme/plugins.git", ref = "v1.2.0", sparse_paths = [".agents/plugins", "plugins/fmt"] }

[plugins.codex.marketplaces.mine]
source = { source_type = "local", source = "~/team/marketplace" }

[plugins.codex.enabled."fmt@acme"]

[plugins.codex.enabled."lint@mine"]

[plugins.codex.enabled."sec@openai-curated"]
"#;

fn codex_with_plugins() -> Sandbox {
    let sb = Sandbox::empty();
    write(
        &sb,
        CODEX,
        &format!(
            r#"# my codex
model = "gpt-5"

[marketplaces.acme]
source_type = "git"
source = "https://github.com/acme/plugins.git"
ref = "v1.2.0"
sparse_paths = [".agents/plugins"]
last_updated = "2026-01-01T00:00:00Z"

[marketplaces.mine]
source_type = "local"
source = "{}"

[marketplaces.leaky]
source_type = "git"
source = "https://user:tok3n@git.example/x.git"

[marketplaces.odd]
source_type = "svn"
source = "x"

[plugins."fmt@acme"]
enabled = true

[plugins."lint@mine"]
enabled = true

[plugins."off@acme"]
enabled = false

[plugins."sec@openai-curated"]
enabled = true

[plugins."noswitch@acme"]
note = 1

[plugins."withmcp@acme"]
enabled = true
[plugins."withmcp@acme".mcp_servers.docs]
enabled = false

[plugins._default]
enabled = false
"#,
            fwd("home/team/marketplace", &sb)
        ),
    );
    sb
}

#[test]
fn codex_capture_carries_marketplaces_and_enabled_plugins_by_reference() {
    let sb = codex_with_plugins();
    let (out, p) = capture_from(&sb, "codex");
    let set = &p.plugins["codex"];
    assert_eq!(
        set.marketplaces.keys().cloned().collect::<Vec<_>>(),
        ["acme", "mine"]
    );
    assert_eq!(
        set.marketplaces["acme"].source,
        json!({
            "source_type": "git",
            "source": "https://github.com/acme/plugins.git",
            "ref": "v1.2.0",
            "sparse_paths": [".agents/plugins"]
        })
        .as_object()
        .unwrap()
        .clone()
    );
    // A local marketplace under the home directory is written as ~/...
    assert_eq!(
        set.marketplaces["mine"].source["source"],
        json!("~/team/marketplace")
    );
    assert_eq!(
        set.enabled.keys().cloned().collect::<Vec<_>>(),
        [
            "fmt@acme",
            "lint@mine",
            "sec@openai-curated",
            "withmcp@acme"
        ]
    );
    for want in [
        "'leaky' not captured",
        "credentials",
        "'odd' not captured",
        "1 plugin(s) set to enabled = false",
        "no enabled key",
        "per-plugin MCP server settings",
    ] {
        assert!(out.contains(want), "{want}: {out}");
    }
    // The built-in marketplace needs no declaration; nothing else leaks.
    assert!(
        !out.contains("sec@openai-curated': its marketplace"),
        "{out}"
    );
    let all = all_text_under(&sb.path("cap"));
    assert!(!all.contains("tok3n"), "{all}");
    assert!(!all.contains("last_updated"), "{all}");
}

#[test]
fn codex_apply_writes_tables_and_a_second_apply_is_up_to_date_and_undo_restores() {
    let sb = Sandbox::empty();
    profile_with(&sb, "p", CODEX_PLUGINS);
    let out = ok(&sb, &["apply", "p", "--to", "codex", "--yes"]);
    for want in [
        "plugin 'fmt@acme' added to config.toml",
        "plugin marketplace 'acme' (git https://github.com/acme/plugins.git, ref v1.2.0) added",
        "kitbag doesn't install plugins",
    ] {
        assert!(out.contains(want), "{want}: {out}");
    }
    let t = toml_of(&sb, CODEX);
    assert_eq!(
        t["marketplaces"]["acme"]["source_type"].as_str(),
        Some("git")
    );
    assert_eq!(t["marketplaces"]["acme"]["ref"].as_str(), Some("v1.2.0"));
    assert_eq!(
        t["marketplaces"]["acme"]["sparse_paths"]
            .as_array()
            .unwrap()
            .len(),
        2
    );
    // `~/` is expanded for this machine: Codex needs an absolute path.
    let local = t["marketplaces"]["mine"]["source"].as_str().unwrap();
    assert_eq!(local.replace('\\', "/"), fwd("home/team/marketplace", &sb));
    assert_eq!(t["plugins"]["fmt@acme"]["enabled"].as_bool(), Some(true));
    assert_eq!(
        t["plugins"]["sec@openai-curated"]["enabled"].as_bool(),
        Some(true)
    );
    assert!(
        sb.read(CODEX).contains("[plugins.\"fmt@acme\"]"),
        "{}",
        sb.read(CODEX)
    );
    let before = sb.read(CODEX);
    let again = ok(&sb, &["apply", "p", "--to", "codex", "--yes"]);
    assert!(again.to_lowercase().contains("up to date"), "{again}");
    assert_eq!(sb.read(CODEX), before);
    ok(&sb, &["undo"]);
    assert!(!sb.path(CODEX).exists());
}

#[test]
fn codex_capture_then_apply_on_a_clean_machine_writes_the_same_plugins() {
    let src = codex_with_plugins();
    let (_, p) = capture_from(&src, "codex");
    let sb = Sandbox::empty();
    let dir = src.path("cap");
    ok(
        &sb,
        &["apply", dir.to_str().unwrap(), "--to", "codex", "--yes"],
    );
    let (_, back) = capture_from(&sb, "codex");
    assert_eq!(back.plugins["codex"].enabled, p.plugins["codex"].enabled);
    assert_eq!(
        back.plugins["codex"]
            .marketplaces
            .keys()
            .collect::<Vec<_>>(),
        p.plugins["codex"].marketplaces.keys().collect::<Vec<_>>()
    );
}

#[test]
fn codex_apply_keeps_the_file_and_existing_choices() {
    let sb = Sandbox::empty();
    profile_with(&sb, "p", CODEX_PLUGINS);
    write(
        &sb,
        CODEX,
        r#"# mine
model = "gpt-5"

[marketplaces.acme]
source_type = "git"
source = "https://github.com/acme/old.git"
last_updated = "2026-01-01T00:00:00Z"

[plugins."fmt@acme"]
enabled = false
"#,
    );
    let out = ok(
        &sb,
        &["apply", "p", "--to", "codex", "--yes", "--no-overwrite"],
    );
    assert!(out.contains("--no-overwrite"), "{out}");
    let t = toml_of(&sb, CODEX);
    assert_eq!(
        t["marketplaces"]["acme"]["source"].as_str(),
        Some("https://github.com/acme/old.git")
    );
    assert_eq!(t["plugins"]["fmt@acme"]["enabled"].as_bool(), Some(false));
    // Without --no-overwrite the profile's source and switch win, and the
    // keys Codex keeps itself stay.
    ok(&sb, &["apply", "p", "--to", "codex", "--yes"]);
    let text = sb.read(CODEX);
    let t = toml_of(&sb, CODEX);
    assert!(text.starts_with("# mine\nmodel = \"gpt-5\"\n"), "{text}");
    assert_eq!(
        t["marketplaces"]["acme"]["source"].as_str(),
        Some("https://github.com/acme/plugins.git")
    );
    assert_eq!(
        t["marketplaces"]["acme"]["last_updated"].as_str(),
        Some("2026-01-01T00:00:00Z")
    );
    assert_eq!(t["plugins"]["fmt@acme"]["enabled"].as_bool(), Some(true));
}

#[test]
fn codex_project_scope_writes_the_project_config() {
    let sb = Sandbox::empty();
    profile_with(&sb, "p", CODEX_PLUGINS);
    let out = ok(
        &sb,
        &[
            "apply",
            "p",
            "--to",
            "codex",
            "--yes",
            "--project",
            "project",
        ],
    );
    assert!(out.contains("marked as trusted"), "{out}");
    let t = toml_of(&sb, "project/.codex/config.toml");
    assert_eq!(t["plugins"]["lint@mine"]["enabled"].as_bool(), Some(true));
    assert!(!sb.path(CODEX).exists());
}

#[test]
fn a_bad_codex_plugin_section_is_refused() {
    for (toml, want) in [
        (
            "[plugins.codex.marketplaces.a]\nsource = { source_type = \"svn\", source = \"x\" }\n",
            "source_type must be git or local",
        ),
        (
            "[plugins.codex.marketplaces.a]\nsource = { source_type = \"git\", source = \"https://u:pw@h/r.git\" }\n",
            "credentials",
        ),
        (
            "[plugins.codex.marketplaces.a]\nsource = { source_type = \"local\", source = \"relative/dir\" }\n",
            "absolute path",
        ),
        (
            "[plugins.codex.marketplaces.a]\nsource = { source_type = \"git\", source = \"o/r\", token = \"x\" }\n",
            "not a key",
        ),
        (
            "[plugins.codex.marketplaces.a]\nsource = { source_type = \"git\", source = \"o/r\" }\nautoUpdate = true\n",
            "autoUpdate",
        ),
        (
            "[plugins.codex.enabled.\"noMarket\"]\n",
            "<name>@<marketplace>",
        ),
        (
            "[plugins.codex.enabled.\"a@b\"]\nversion = \"1\"\n",
            "version is not used for codex",
        ),
    ] {
        let sb = Sandbox::empty();
        profile_with(&sb, "p", toml);
        let (good, out) = run(&sb, &["apply", "p", "--to", "codex", "--yes"]);
        assert!(!good && out.contains(want), "{want}: {out}");
        assert!(!sb.path(CODEX).exists());
    }
}

#[test]
fn from_a_git_source_codex_plugins_need_trust_source() {
    if !has_git() {
        return;
    }
    let sb = Sandbox::empty();
    let url = sb.git_profile(
        "remote",
        &[(
            "kitbag.toml",
            &format!("kitbag = 1\nname = \"remote\"\n{CODEX_PLUGINS}"),
        )],
    );
    let out = ok(&sb, &["apply", &url, "--to", "codex", "--yes"]);
    assert!(!sb.path(CODEX).exists(), "{out}");
    for want in [
        "plugin 'fmt@acme'",
        "plugin marketplace 'acme'",
        "--trust-source",
    ] {
        assert!(out.contains(want), "{want}: {out}");
    }
    ok(
        &sb,
        &["apply", &url, "--to", "codex", "--yes", "--trust-source"],
    );
    assert!(toml_of(&sb, CODEX).contains_key("plugins"));
}

#[test]
fn a_plugin_disabled_on_codex_is_not_applied_and_inspect_lists_the_rest() {
    let sb = Sandbox::empty();
    profile_with(
        &sb,
        "p",
        r#"
[plugins.codex.enabled."a@acme"]
disabled_on = ["codex"]

[plugins.codex.enabled."b@acme"]
"#,
    );
    let out = ok(&sb, &["inspect", "p", "--to", "codex"]);
    assert!(out.contains("plugins (2)"), "{out}");
    assert!(!sb.path(CODEX).exists());
    ok(&sb, &["apply", "p", "--to", "codex", "--yes"]);
    let t = toml_of(&sb, CODEX);
    assert!(t["plugins"].as_table().unwrap().get("a@acme").is_none());
    assert_eq!(t["plugins"]["b@acme"]["enabled"].as_bool(), Some(true));
}

// ============================================================== Copilot CLI

const COPILOT: &str = "home/.copilot/settings.json";
const COPILOT_PROJECT: &str = "project/.github/copilot/settings.json";

const COPILOT_PLUGINS: &str = r#"
[plugins.copilot.marketplaces.acme-tools]
source = { source = "github", repo = "acme/tools", ref = "v1.2.0" }
autoUpdate = true

[plugins.copilot.marketplaces.mine]
source = { source = "directory", path = "~/team/marketplace" }

[plugins.copilot.enabled."formatter@acme-tools"]

[plugins.copilot.enabled."linter@mine"]

[plugins.copilot.enabled."docs@awesome-copilot"]
"#;

#[test]
fn copilot_capture_carries_plugins_and_only_the_sources_it_reads() {
    let sb = Sandbox::empty();
    write(
        &sb,
        COPILOT,
        &serde_json::to_string_pretty(&json!({
            "model": "auto",
            "enabledPlugins": {
                "formatter@acme-tools": true,
                "linter@mine": true,
                "docs@awesome-copilot": true,
                "off@acme-tools": false
            },
            "extraKnownMarketplaces": {
                "acme-tools": {"source": {"source": "github", "repo": "acme/tools", "ref": "v1.2.0"}, "autoUpdate": true},
                "mine": {"source": {"source": "directory", "path": fwd("home/team/marketplace", &sb)}},
                "feed": {"source": {"source": "url", "url": "https://example.com/m.json"}}
            }
        }))
        .unwrap(),
    );
    let (out, p) = capture_from(&sb, "copilot");
    let set = &p.plugins["copilot"];
    assert_eq!(
        set.marketplaces.keys().cloned().collect::<Vec<_>>(),
        ["acme-tools", "mine"]
    );
    assert_eq!(set.marketplaces["acme-tools"].auto_update, Some(true));
    assert_eq!(
        set.marketplaces["mine"].source["path"],
        json!("~/team/marketplace")
    );
    assert_eq!(
        set.enabled.keys().cloned().collect::<Vec<_>>(),
        [
            "docs@awesome-copilot",
            "formatter@acme-tools",
            "linter@mine"
        ]
    );
    assert!(out.contains("'feed' not captured"), "{out}");
    assert!(out.contains("1 plugin(s) set to false"), "{out}");
    // Copilot's own built-in marketplace needs no declaration.
    assert!(!out.contains("'awesome-copilot' is not declared"), "{out}");
    assert!(!out.contains("claude plugin"), "{out}");
}

#[test]
fn copilot_apply_adds_the_two_keys_and_keeps_the_rest_then_undo() {
    let sb = Sandbox::empty();
    profile_with(&sb, "p", COPILOT_PLUGINS);
    write(
        &sb,
        COPILOT,
        &serde_json::to_string_pretty(&json!({
            "model": "auto",
            "enabledPlugins": {"mine@else": true}
        }))
        .unwrap(),
    );
    let start = sb.read(COPILOT);
    let out = ok(&sb, &["apply", "p", "--to", "copilot", "--yes"]);
    assert!(
        out.contains("plugin 'formatter@acme-tools' added to settings.json"),
        "{out}"
    );
    assert!(out.contains("installs the enabledPlugins entries"), "{out}");
    let v = sb.json(COPILOT);
    assert_eq!(v["model"], json!("auto"));
    assert_eq!(v["enabledPlugins"]["mine@else"], json!(true));
    assert_eq!(v["enabledPlugins"]["formatter@acme-tools"], json!(true));
    assert_eq!(
        v["extraKnownMarketplaces"]["acme-tools"],
        json!({"source": {"source": "github", "repo": "acme/tools", "ref": "v1.2.0"}, "autoUpdate": true})
    );
    assert_eq!(
        v["extraKnownMarketplaces"]["mine"]["source"]["path"]
            .as_str()
            .unwrap()
            .replace('\\', "/"),
        fwd("home/team/marketplace", &sb)
    );
    let after = sb.read(COPILOT);
    let again = ok(&sb, &["apply", "p", "--to", "copilot", "--yes"]);
    assert!(again.to_lowercase().contains("up to date"), "{again}");
    assert_eq!(sb.read(COPILOT), after);
    ok(&sb, &["undo"]);
    assert_eq!(sb.read(COPILOT), start);
}

#[test]
fn copilot_project_scope_writes_the_repository_settings() {
    let sb = Sandbox::empty();
    profile_with(&sb, "p", COPILOT_PLUGINS);
    let out = ok(
        &sb,
        &[
            "apply",
            "p",
            "--to",
            "copilot",
            "--yes",
            "--project",
            "project",
        ],
    );
    assert!(out.contains("repository default"), "{out}");
    let v = sb.json(COPILOT_PROJECT);
    assert_eq!(v["enabledPlugins"]["linter@mine"], json!(true));
    assert!(!sb.path(COPILOT).exists());
    // And capture reads it back.
    write(&sb, "project/.github/copilot-instructions.md", "Hi.\n");
    ok(
        &sb,
        &[
            "capture",
            "--from",
            "copilot",
            "--out",
            "proj",
            "--project",
            "project",
        ],
    );
    let p = kitbag::profile_io::load(&sb.path("proj")).unwrap();
    assert!(
        p.plugins["copilot"]
            .enabled
            .contains_key("formatter@acme-tools")
    );
}

#[test]
fn copilot_refuses_a_source_it_does_not_read_and_a_non_object_key() {
    let sb = Sandbox::empty();
    profile_with(
        &sb,
        "p",
        "[plugins.copilot.marketplaces.feed]\nsource = { source = \"url\", url = \"https://example.com/m.json\" }\n",
    );
    let (good, out) = run(&sb, &["apply", "p", "--to", "copilot", "--yes"]);
    assert!(!good && out.contains("isn't one copilot reads"), "{out}");
    let sb = Sandbox::empty();
    profile_with(&sb, "p", COPILOT_PLUGINS);
    write(&sb, COPILOT, "{\"enabledPlugins\": [1]}\n");
    let (good, out) = run(&sb, &["apply", "p", "--to", "copilot", "--yes"]);
    assert!(!good && out.contains("not an object"), "{out}");
    assert_eq!(sb.read(COPILOT), "{\"enabledPlugins\": [1]}\n");
}

#[test]
fn from_a_git_source_copilot_plugins_need_trust_source() {
    if !has_git() {
        return;
    }
    let sb = Sandbox::empty();
    let url = sb.git_profile(
        "remote",
        &[(
            "kitbag.toml",
            &format!("kitbag = 1\nname = \"remote\"\n{COPILOT_PLUGINS}"),
        )],
    );
    let out = ok(&sb, &["apply", &url, "--to", "copilot", "--yes"]);
    assert!(!sb.path(COPILOT).exists(), "{out}");
    assert!(
        out.contains("--trust-source") && out.contains("plugin 'formatter@acme-tools'"),
        "{out}"
    );
    ok(
        &sb,
        &["apply", &url, "--to", "copilot", "--yes", "--trust-source"],
    );
    assert!(sb.json(COPILOT)["enabledPlugins"]["formatter@acme-tools"] == json!(true));
}

#[test]
fn plugins_for_one_tool_are_not_written_for_another() {
    let sb = Sandbox::empty();
    profile_with(&sb, "p", COPILOT_PLUGINS);
    ok(&sb, &["apply", "p", "--to", "claude-code,codex", "--yes"]);
    assert!(!sb.path(COPILOT).exists());
    assert!(!sb.path("home/.claude/settings.json").exists());
    assert!(!sb.path(CODEX).exists());
}

// ======================================================================== Zed

const ZED: &str = "home/.config/zed/settings.json";

fn zed_profile(sb: &Sandbox, toml: &str) -> kitbag::model::Profile {
    profile_with(sb, "p", toml);
    kitbag::profile_io::load(&sb.path("p")).unwrap()
}

const ZED_PLUGINS: &str = r#"
[plugins.zed.enabled."html"]

[plugins.zed.enabled."toml"]
"#;

fn said(p: &kitbag::engine::Plan, target: &str, what: &str) -> bool {
    p.notes
        .iter()
        .any(|n| n.target == target && n.message.contains(what))
}

#[test]
fn zed_capture_reads_auto_install_extensions_and_apply_adds_them() {
    let sb = Sandbox::empty();
    write(
        &sb,
        ZED,
        "// my zed\n{\n  \"theme\": \"One\",\n  \"auto_install_extensions\": {\"html\": true, \"docker-compose\": false, \"bad id\": true}\n}\n",
    );
    let env = sb.env(Scope::User);
    let got = capture(&["zed"], &env);
    assert_eq!(
        got.profile.plugins["zed"]
            .enabled
            .keys()
            .collect::<Vec<_>>(),
        ["html"]
    );
    assert!(
        got.notes
            .iter()
            .any(|n| n.message.contains("never install"))
    );

    let profile = zed_profile(&sb, ZED_PLUGINS);
    let p = apply(&sb, &profile, &["zed"], &env);
    assert!(
        said(&p, "zed", "Zed extension 'toml' added to settings.json"),
        "{:?}",
        p.notes
    );
    assert!(said(&p, "zed", "downloads the extensions above"));
    let text = sb.read(ZED);
    assert!(text.starts_with("// my zed"), "{text}");
    let v: Value = jsonc(&text);
    assert_eq!(v["theme"], json!("One"));
    assert_eq!(v["auto_install_extensions"]["toml"], json!(true));
    assert_eq!(v["auto_install_extensions"]["html"], json!(true));
    assert_eq!(v["auto_install_extensions"]["docker-compose"], json!(false));
    // A second apply changes nothing.
    let again = plan(&profile, &["zed"], &env);
    assert!(planned(&again, &sb.path(ZED)).is_none());
    kitbag::txn::Store::new(sb.state())
        .undo(None, false)
        .unwrap();
    assert!(sb.read(ZED).contains("\"html\": true, \"docker-compose\""));
}

fn jsonc(text: &str) -> Value {
    let body: String = text
        .lines()
        .filter(|l| !l.trim_start().starts_with("//"))
        .collect::<Vec<_>>()
        .join("\n");
    serde_json::from_str(&body).unwrap()
}

#[test]
fn zed_keeps_a_never_install_entry_with_no_overwrite_and_refuses_a_non_object() {
    let sb = Sandbox::empty();
    write(
        &sb,
        ZED,
        "{\"auto_install_extensions\": {\"toml\": false}}\n",
    );
    let env = sb.env(Scope::User);
    let profile = zed_profile(&sb, ZED_PLUGINS);
    let mut o = opts(&profile);
    o.overwrite = false;
    let p = kitbag::engine::plan(&profile, &targets(&["zed"]), &env, &o).unwrap();
    assert!(said(&p, "zed", "--no-overwrite"), "{:?}", p.notes);
    let after = planned(&p, &sb.path(ZED)).unwrap();
    assert_eq!(
        jsonc(&after)["auto_install_extensions"]["toml"],
        json!(false)
    );
    assert_eq!(
        jsonc(&after)["auto_install_extensions"]["html"],
        json!(true)
    );

    write(&sb, ZED, "{\"auto_install_extensions\": [\"html\"]}\n");
    let err = kitbag::engine::plan(&profile, &targets(&["zed"]), &env, &opts(&profile))
        .map(|_| ())
        .expect_err("a non-object auto_install_extensions is refused");
    assert!(format!("{err:#}").contains("not an object"), "{err:#}");
}

#[test]
fn zed_extensions_are_written_at_user_scope_only() {
    let sb = Sandbox::empty();
    let profile = zed_profile(&sb, ZED_PLUGINS);
    let env = sb.env(Scope::Project);
    let p = apply(&sb, &profile, &["zed"], &env);
    assert!(said(&p, "zed", "user scope only"), "{:?}", p.notes);
    assert!(!sb.path("project/.zed/settings.json").exists());
    assert!(!sb.path(ZED).exists());
}

#[test]
fn zed_rejects_marketplaces_and_bad_ids() {
    for (toml, want) in [
        (
            "[plugins.zed.marketplaces.m]\nsource = { source = \"github\", repo = \"a/b\" }\n",
            "installs extensions from its gallery",
        ),
        ("[plugins.zed.enabled.\"has space\"]\n", "extension id"),
        (
            "[plugins.zed.enabled.\"x\"]\nversion = \"1\"\n",
            "version is not used for zed",
        ),
    ] {
        let sb = Sandbox::empty();
        profile_with(&sb, "p", toml);
        let err = kitbag::profile_io::load(&sb.path("p")).unwrap_err();
        assert!(format!("{err:#}").contains(want), "{want}: {err:#}");
    }
}

#[test]
fn from_a_git_source_zed_extensions_need_trust_source() {
    if !has_git() {
        return;
    }
    let sb = Sandbox::empty();
    let url = sb.git_profile(
        "remote",
        &[(
            "kitbag.toml",
            &format!("kitbag = 1\nname = \"remote\"\n{ZED_PLUGINS}"),
        )],
    );
    let out = ok(&sb, &["apply", &url, "--to", "zed", "--yes"]);
    assert!(
        !sb.path(ZED).exists() || !sb.read(ZED).contains("auto_install"),
        "{out}"
    );
    assert!(
        out.contains("Zed extension 'html'") && out.contains("--trust-source"),
        "{out}"
    );
}

// ================================================================= Grok Build

const GROK: &str = "home/.grok/config.toml";

const GROK_PLUGINS: &str = r#"
[plugins.grok-build.enabled."~/team/plugins/formatter"]

[plugins.grok-build.enabled."~/team/plugins/linter"]
"#;

#[test]
fn grok_capture_carries_absolute_plugin_folders_only() {
    let sb = Sandbox::empty();
    write(
        &sb,
        GROK,
        &format!(
            "[plugins]\npaths = [\"{}\", \"{}\", \"relative/dir\", \"~/tilde/plugin\"]\nenabled = [\"a\"]\ndisabled = [\"b\"]\n\n[mcp_servers.x]\ncommand = \"x\"\n",
            fwd("home/team/plugins/formatter", &sb),
            fwd("elsewhere/plugins", &sb),
        ),
    );
    let (out, p) = capture_from(&sb, "grok-build");
    let mut keys: Vec<String> = p.plugins["grok-build"]
        .enabled
        .keys()
        .map(|k| k.replace('\\', "/"))
        .collect();
    keys.sort();
    let mut want = vec![
        fwd("elsewhere/plugins", &sb),
        "~/team/plugins/formatter".to_string(),
        "~/tilde/plugin".to_string(),
    ];
    want.sort();
    assert_eq!(keys, want);
    assert!(out.contains("relative/dir"), "{out}");
    assert!(out.contains("2 plugin name(s)"), "{out}");
}

#[test]
fn grok_apply_expands_home_appends_and_undo_restores() {
    let sb = Sandbox::empty();
    profile_with(&sb, "p", GROK_PLUGINS);
    write(
        &sb,
        GROK,
        "# mine\n[models]\ndefault = \"grok-build\"\n\n[plugins]\nenabled = [\"x\"]\n",
    );
    let start = sb.read(GROK);
    let out = ok(&sb, &["apply", "p", "--to", "grok-build", "--yes"]);
    assert!(out.contains("plugin folder"), "{out}");
    assert!(out.contains("doesn't exist on this machine"), "{out}");
    let t = toml_of(&sb, GROK);
    let paths: Vec<String> = t["plugins"]["paths"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_str().unwrap().replace('\\', "/"))
        .collect();
    assert_eq!(
        paths,
        [
            fwd("home/team/plugins/formatter", &sb),
            fwd("home/team/plugins/linter", &sb)
        ]
    );
    assert_eq!(t["plugins"]["enabled"].as_array().unwrap().len(), 1);
    assert!(sb.read(GROK).starts_with("# mine\n"));
    let after = sb.read(GROK);
    let again = ok(&sb, &["apply", "p", "--to", "grok-build", "--yes"]);
    assert!(again.to_lowercase().contains("up to date"), "{again}");
    assert_eq!(sb.read(GROK), after);
    ok(&sb, &["undo"]);
    assert_eq!(sb.read(GROK), start);
}

#[test]
fn grok_leaves_a_folder_that_is_already_listed_however_it_is_written() {
    let sb = Sandbox::empty();
    profile_with(&sb, "p", GROK_PLUGINS);
    write(
        &sb,
        GROK,
        &format!(
            "[plugins]\npaths = [\"{}/\", \"~/team/plugins/linter\"]\n",
            fwd("home/team/plugins/formatter", &sb)
        ),
    );
    let before = sb.read(GROK);
    let out = ok(&sb, &["apply", "p", "--to", "grok-build", "--yes"]);
    assert!(out.to_lowercase().contains("up to date"), "{out}");
    assert_eq!(sb.read(GROK), before);
}

#[test]
fn grok_project_scope_writes_nothing_for_plugins_and_says_why() {
    let sb = Sandbox::empty();
    profile_with(&sb, "p", GROK_PLUGINS);
    let out = ok(
        &sb,
        &[
            "apply",
            "p",
            "--to",
            "grok-build",
            "--yes",
            "--project",
            "project",
        ],
    );
    assert!(out.contains("not written to .grok/config.toml"), "{out}");
    assert!(!sb.path("project/.grok/config.toml").exists());
    assert!(!sb.path(GROK).exists());
}

#[test]
fn grok_refuses_relative_folders_marketplaces_and_non_list_paths() {
    for (toml, want) in [
        (
            "[plugins.grok-build.enabled.\"rel/dir\"]\n",
            "absolute path",
        ),
        (
            "[plugins.grok-build.marketplaces.m]\nsource = { source = \"github\", repo = \"a/b\" }\n",
            "no documented layout",
        ),
        (
            "[plugins.grok-build.enabled.\"~/x\"]\nversion = \"1\"\n",
            "version is not used",
        ),
    ] {
        let sb = Sandbox::empty();
        profile_with(&sb, "p", toml);
        let (good, out) = run(&sb, &["apply", "p", "--to", "grok-build", "--yes"]);
        assert!(!good && out.contains(want), "{want}: {out}");
    }
    let sb = Sandbox::empty();
    profile_with(&sb, "p", GROK_PLUGINS);
    write(&sb, GROK, "[plugins]\npaths = \"nope\"\n");
    let (good, out) = run(&sb, &["apply", "p", "--to", "grok-build", "--yes"]);
    assert!(!good && out.contains("not a list"), "{out}");
    assert_eq!(sb.read(GROK), "[plugins]\npaths = \"nope\"\n");
}

#[test]
fn from_a_git_source_grok_plugins_need_trust_source_and_inspect_says_what_runs() {
    if !has_git() {
        return;
    }
    let sb = Sandbox::empty();
    let out = ok(
        &sb,
        &["inspect", &{
            profile_with(&sb, "p", GROK_PLUGINS);
            "p".to_string()
        }],
    );
    assert!(out.contains("Grok Build loads as a plugin"), "{out}");
    let url = sb.git_profile(
        "remote",
        &[(
            "kitbag.toml",
            &format!("kitbag = 1\nname = \"remote\"\n{GROK_PLUGINS}"),
        )],
    );
    let out = ok(&sb, &["apply", &url, "--to", "grok-build", "--yes"]);
    assert!(!sb.path(GROK).exists(), "{out}");
    assert!(out.contains("--trust-source"), "{out}");
}
