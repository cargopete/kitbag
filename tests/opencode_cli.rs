//! opencode 2's `cli.json` settings (#142): captured and applied at user
//! scope only, bounded values only, never mixed into `opencode.json`.

mod common;

use std::fs;

use common::*;
use kitbag::adapters::Scope;
use kitbag::model::Profile;
use kitbag::policy;
use kitbag::txn::Store;
use serde_json::{Value, json};

const CLI: &str = "home/.config/opencode/cli.json";
const OC: &str = "home/.config/opencode/opencode.json";

fn put(sb: &Sandbox, rel: &str, text: &str) {
    let path = sb.path(rel);
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, text).unwrap();
}

fn with_settings(v: Value) -> Profile {
    let mut p = Profile::new("team");
    let Value::Object(m) = v else {
        panic!("object")
    };
    p.settings.insert("opencode".to_string(), m);
    p
}

#[test]
fn capture_takes_documented_bounded_keys_and_leaves_the_rest() {
    let sb = Sandbox::empty();
    put(
        &sb,
        CLI,
        &json!({
            "$schema": "https://opencode.ai/v2/cli.json",
            "theme": {"name": "tokyonight", "mode": "dark"},
            "animations": false,
            "cursor": {"style": "line", "blinking": true},
            "scroll": {"speed": 3.5, "acceleration": false},
            "session": {"thinking": "hide", "permissions": "prompt", "sidebar": "sideways"},
            "tabs": {"enabled": false, "layout": "vertical"},
            "attention": {"volume": 0.5, "sound_pack": "retro", "sounds": {"done": "/etc/x.wav"}},
            "mini": {"work_spinner": "seed", "replay_limit": 0, "footer": "show"},
            "keybinds": {"leader": "ctrl+x"},
            "plugins": ["evil-pkg"],
            "experimental": {"x": true},
            "debug": {"timing": true}
        })
        .to_string(),
    );
    put(
        &sb,
        OC,
        &json!({"model": "anthropic/m", "theme": "old"}).to_string(),
    );
    let r = capture(&["opencode"], &sb.env(Scope::User));
    assert_eq!(
        Value::Object(r.profile.settings["opencode"].clone()),
        json!({
            "model": "anthropic/m",
            "theme": {"name": "tokyonight", "mode": "dark"},
            "animations": false,
            "cursor": {"style": "line", "blinking": true},
            "scroll": {"speed": 3.5, "acceleration": false},
            "session": {"thinking": "hide", "permissions": "prompt"},
            "tabs": {"enabled": false, "layout": "vertical"},
            "attention": {"volume": 0.5, "sound_pack": "retro"},
            "mini": {"work_spinner": "seed", "footer": "show"}
        })
    );
    // No project-local cli.json.
    let project = capture(&["opencode"], &sb.env(Scope::Project));
    assert!(!project.profile.settings.contains_key("opencode"));
}

#[test]
fn apply_routes_keys_to_the_right_file_creates_schema_and_undoes() {
    let sb = Sandbox::empty();
    put(&sb, OC, &json!({"autoupdate": true}).to_string());
    let p = with_settings(json!({
        "model": "anthropic/m",
        "theme": {"name": "nord"},
        "mouse": false,
        "plugins": ["x"],
        "keybinds": {"leader": "a"},
        "scroll": {"speed": 0.0}
    }));
    let plan = apply(&sb, &p, &["opencode"], &sb.env(Scope::User));
    assert!(plan.notes.iter().any(|n| n.message.contains("plugins")));
    assert!(
        plan.notes
            .iter()
            .any(|n| n.message.contains("scroll.speed"))
    );
    assert_eq!(
        sb.json(CLI),
        json!({"$schema": "https://opencode.ai/v2/cli.json", "theme": {"name": "nord"}, "mouse": false})
    );
    assert_eq!(
        sb.json(OC),
        json!({"autoupdate": true, "model": "anthropic/m"})
    );
    Store::new(sb.state()).undo(None, false).unwrap();
    assert!(!sb.path(CLI).exists());
}

#[test]
fn apply_keeps_unknown_keys_and_schema_of_an_existing_file() {
    let sb = Sandbox::empty();
    put(
        &sb,
        CLI,
        &json!({"$schema": "https://opencode.ai/v2/cli.json", "keybinds": {"leader": "ctrl+a"}, "theme": {"name": "mine"}}).to_string(),
    );
    let p = with_settings(json!({"theme": {"mode": "light"}, "tabs": {"mode": "auto"}}));
    apply(&sb, &p, &["opencode"], &sb.env(Scope::User));
    assert_eq!(
        sb.json(CLI),
        json!({"$schema": "https://opencode.ai/v2/cli.json", "keybinds": {"leader": "ctrl+a"},
               "theme": {"name": "mine", "mode": "light"}, "tabs": {"mode": "auto"}})
    );
}

#[test]
fn xdg_config_home_moves_cli_json_and_project_scope_writes_none() {
    let sb = Sandbox::empty();
    let p = with_settings(json!({"highlights": false}));
    let mut env = sb.env(Scope::User);
    env.locations.insert(
        "XDG_CONFIG_HOME".into(),
        sb.path("xdg").display().to_string(),
    );
    let pl = plan_for(&p, &["opencode"], &env);
    assert!(planned(&pl, &sb.path("xdg").join("opencode").join("cli.json")).is_some());

    let pl = plan_for(&p, &["opencode"], &sb.env(Scope::Project));
    assert!(
        pl.notes
            .iter()
            .any(|n| n.message.contains("no project file"))
    );
    assert!(planned(&pl, &sb.path("project/.opencode/cli.json")).is_none());
}

fn plan_for(p: &Profile, ids: &[&str], env: &kitbag::adapters::Env) -> kitbag::engine::Plan {
    plan(p, ids, env)
}

#[test]
fn autoaccept_is_risky_and_dropped_for_git_sources() {
    let Value::Object(mut m) =
        json!({"session": {"permissions": "autoaccept", "tps": true}, "mouse": true})
    else {
        panic!()
    };
    let warns = policy::risky("opencode", &m);
    assert!(warns.iter().any(|w| w.contains("autoaccept")));
    let dropped = policy::drop_risky("opencode", &mut m);
    assert_eq!(
        dropped,
        vec!["session.permissions = autoaccept".to_string()]
    );
    assert_eq!(
        Value::Object(m),
        json!({"session": {"tps": true}, "mouse": true})
    );
    let Value::Object(safe) = json!({"session": {"permissions": "prompt"}}) else {
        panic!()
    };
    assert!(policy::risky("opencode", &safe).is_empty());
}

#[test]
fn autoaccept_from_a_git_source_needs_the_flag() {
    if !has_git() {
        eprintln!("git not installed; skipping");
        return;
    }
    let sb = Sandbox::empty();
    let url = sb.git_profile(
        "repo",
        &[(
            "kitbag.toml",
            "kitbag = 1\nname = \"team\"\n\n[settings.opencode]\nanimations = false\n\n[settings.opencode.session]\npermissions = \"autoaccept\"\ntps = true\n",
        )],
    );
    sb.cli()
        .args(["apply", &url, "--to", "opencode", "--yes"])
        .assert()
        .success();
    assert_eq!(
        sb.json(CLI),
        json!({"$schema": "https://opencode.ai/v2/cli.json", "animations": false, "session": {"tps": true}})
    );
    sb.cli().arg("undo").assert().success();
    sb.cli()
        .args([
            "apply",
            &url,
            "--to",
            "opencode",
            "--yes",
            "--allow-risky-settings",
        ])
        .assert()
        .success();
    assert_eq!(sb.json(CLI)["session"]["permissions"], "autoaccept");
}

#[test]
fn value_bounds_are_enforced() {
    let Value::Object(m) = json!({
        "theme": {"name": "../../etc/passwd"},
        "attention": {"volume": 1.5, "sound_pack": "a/b"},
        "mini": {"work_spinner": "not-a-spinner", "replay_limit": 5, "mono": "yes"},
        "mouse": "true"
    }) else {
        panic!()
    };
    let (allowed, dropped) = policy::filter("opencode", Some(&m));
    assert_eq!(Value::Object(allowed), json!({"mini": {"replay_limit": 5}}));
    for k in [
        "theme.name",
        "attention.volume",
        "attention.sound_pack",
        "mini.work_spinner",
        "mini.mono",
        "mouse",
    ] {
        assert!(dropped.contains(&k.to_string()), "{k} in {dropped:?}");
    }
}
