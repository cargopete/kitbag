//! `[editor.<id>]` (#19): VS Code's AI extensions and settings (default
//! profile, user scope) and Zed's AI keybindings. The editor's command line
//! is a stub on PATH; nothing here touches a real editor or the real home.

mod common;

use std::fs;
use std::path::PathBuf;

use common::*;
use kitbag::adapters::Scope;
use kitbag::model::Profile;
use kitbag::profile_io;
use kitbag::txn::Store;
use serde_json::json;

const VSCODE_SETTINGS: &str = "home/.config/Code/User/settings.json";
const ZED_KEYMAP: &str = "home/.config/zed/keymap.json";

/// Where the real OS has VS Code's `settings.json` under `KITBAG_HOME`.
/// Only used by the Unix CLI tests, which stub `code` on PATH.
#[cfg(unix)]
fn cli_vscode_settings() -> &'static str {
    if cfg!(target_os = "macos") {
        "home/Library/Application Support/Code/User/settings.json"
    } else if cfg!(windows) {
        "home/AppData/Roaming/Code/User/settings.json"
    } else {
        VSCODE_SETTINGS
    }
}

#[cfg(unix)]
fn cli_zed_keymap() -> &'static str {
    if cfg!(windows) {
        "home/AppData/Roaming/Zed/keymap.json"
    } else {
        ZED_KEYMAP
    }
}

fn write(sb: &Sandbox, rel: &str, text: &str) {
    let p = sb.path(rel);
    fs::create_dir_all(p.parent().unwrap()).unwrap();
    fs::write(p, text).unwrap();
}

fn manifest(toml: &str) -> Result<Profile, profile_io::ProfileError> {
    profile_io::parse_manifest(
        &format!("kitbag = 1\nname = \"p\"\n{toml}"),
        std::path::Path::new("kitbag.toml"),
    )
}

fn errors(toml: &str) -> String {
    manifest(toml).unwrap_err().issues.join("\n")
}

const PROFILE: &str = r#"
[editor.vscode]
extensions = ["Continue.continue", "saoudrizwan.claude-dev@3.0.0"]

[editor.vscode.settings]
"chat.agent.enabled" = true
"chat.agent.maxRequests" = 40
"github.copilot.enable" = { "*" = true, markdown = false }

[[editor.zed.keybindings]]
context = "Workspace"
bindings = { "ctrl-alt-a" = "agent::Toggle", "ctrl-alt-n" = "agent::NewThread" }
"#;

// ------------------------------------------------------------ the manifest

#[test]
fn the_editor_tables_load_and_round_trip() {
    let p = manifest(PROFILE).unwrap();
    assert!(p.warnings.is_empty(), "{:?}", p.warnings);
    let vs = &p.editor["vscode"];
    assert_eq!(
        vs.extensions,
        vec!["Continue.continue", "saoudrizwan.claude-dev@3.0.0"]
    );
    assert_eq!(vs.settings["chat.agent.maxRequests"], json!(40));
    assert_eq!(p.editor["zed"].keybindings[0].bindings.len(), 2);
    assert!(!p.is_empty());
    let again = manifest(
        profile_io::manifest_toml(&p)
            .split_once("name = \"p\"\n")
            .unwrap()
            .1,
    )
    .unwrap();
    assert_eq!(again.editor, p.editor, "{}", profile_io::manifest_toml(&p));
}

#[test]
fn ids_are_exact_publisher_dot_extension_never_paths_or_urls() {
    for bad in [
        "./team.vsix",
        "https://example.com/x.vsix",
        "C:\\\\x\\\\y.vsix",
        "continue",
        "a.b.c",
        "--force.x",
        "continue.continue@latest",
    ] {
        let e = errors(&format!("[editor.vscode]\nextensions = [\"{bad}\"]\n"));
        assert!(e.contains("editor.vscode.extensions"), "{bad}: {e}");
    }
}

#[test]
fn what_is_not_carried_is_refused_or_warned_not_guessed() {
    // Not in the carried list: loads, with a warning; apply drops it.
    let p = manifest(
        "[editor.vscode]\nextensions = [\"ms-python.python\"]\n[editor.vscode.settings]\n\"editor.fontSize\" = 40\n\"chat.tools.global.autoApprove\" = true\n",
    )
    .unwrap();
    let w = p.warnings.join("\n");
    assert!(w.contains("ms-python.python"), "{w}");
    assert!(w.contains("editor.fontSize"), "{w}");
    assert!(w.contains("chat.tools.global.autoApprove"), "{w}");
    // Refused outright.
    assert!(errors("[editor.cursor]\nextensions = []\n").contains("unknown editor"));
    assert!(errors("[editor.windsurf]\nextensions = []\n").contains("unknown editor"));
    assert!(
        errors("[[editor.vscode.keybindings]]\nbindings = { \"ctrl-k\" = \"x\" }\n")
            .contains("keybindings.json")
    );
    assert!(
        errors("[editor.zed]\nextensions = [\"Continue.continue\"]\n").contains("[plugins.zed]")
    );
    assert!(
        errors("[editor.zed.settings]\n\"chat.agent.enabled\" = true\n").contains("[settings.zed]")
    );
    assert!(
        errors("[editor.vscode.settings]\n\"chat.agent.maxRequests\" = 5000\n")
            .contains("1 to 1000")
    );
    assert!(
        errors("[editor.vscode.settings]\n\"chat.agent.enabled\" = \"yes\"\n")
            .contains("true or false")
    );
}

// ------------------------------------------------------------ VS Code settings

#[test]
fn settings_merge_into_user_settings_json_and_undo_restores_it() {
    let sb = Sandbox::empty();
    let file = sb.path(VSCODE_SETTINGS);
    write(
        &sb,
        VSCODE_SETTINGS,
        "{\n  // my own\n  \"editor.fontSize\": 15,\n  \"chat.agent.maxRequests\": 25,\n}\n",
    );
    let before = fs::read_to_string(&file).unwrap();
    let mut p = manifest(PROFILE).unwrap();
    p.editor.get_mut("vscode").unwrap().extensions.clear();
    p.editor
        .get_mut("vscode")
        .unwrap()
        .settings
        .insert("editor.fontSize".into(), json!(40));
    let env = sb.env(Scope::User);
    let plan = apply(&sb, &p, &["vscode"], &env);
    let after = fs::read_to_string(&file).unwrap();
    assert!(after.contains("// my own"), "{after}");
    assert!(
        after.contains("\"editor.fontSize\": 15"),
        "a general pref is not touched: {after}"
    );
    assert!(after.contains("\"chat.agent.enabled\": true"), "{after}");
    assert!(after.contains("\"chat.agent.maxRequests\": 40"), "{after}");
    assert!(after.contains("\"github.copilot.enable\""), "{after}");
    assert!(
        plan.notes
            .iter()
            .any(|n| n.target == "vscode" && n.message.contains("editor.fontSize")),
        "{:?}",
        plan.notes
    );
    // A second apply changes nothing.
    assert!(plan_again(&p, &env).changes.is_empty());
    Store::new(sb.state()).undo(None, false).unwrap();
    assert_eq!(
        fs::read_to_string(&file).unwrap(),
        before,
        "undo restores settings.json"
    );
}

fn plan_again(p: &Profile, env: &kitbag::adapters::Env) -> kitbag::engine::Plan {
    plan(p, &["vscode"], env)
}

#[test]
fn vscode_has_user_scope_only_and_never_touches_profile_folders() {
    let sb = Sandbox::empty();
    let p = manifest(PROFILE).unwrap();
    let plan = plan(&p, &["vscode"], &sb.env(Scope::Project));
    assert!(plan.changes.is_empty());
    assert!(
        plan.notes
            .iter()
            .any(|n| n.message.contains("no project scope")),
        "{:?}",
        plan.notes
    );
    let user = plan_user_paths(&sb);
    assert!(
        user.iter()
            .all(|p| !p.to_string_lossy().contains("profiles")),
        "{user:?}"
    );
}

fn plan_user_paths(sb: &Sandbox) -> Vec<PathBuf> {
    let mut p = manifest(PROFILE).unwrap();
    p.editor.get_mut("vscode").unwrap().extensions.clear();
    plan(&p, &["vscode"], &sb.env(Scope::User))
        .changes
        .iter()
        .map(|c| c.path.clone())
        .collect()
}

// ------------------------------------------------------------ Zed keybindings

const KEYMAP: &str = r#"[
  // mine
  {
    "context": "Workspace",
    "bindings": {
      "ctrl-alt-a": "agent::Toggle",
      "ctrl-alt-q": "workspace::SendKeystrokes",
      "ctrl-alt-z": ["task::Spawn", { "task_name": "x" }]
    }
  },
  { "bindings": { "ctrl-alt-i": "assistant::InlineAssist", "ctrl-alt-j": "agent::AllowAlways" } }
]
"#;

fn zed_profile() -> Profile {
    manifest(
        "[[editor.zed.keybindings]]\ncontext = \"Workspace\"\nbindings = { \"ctrl-alt-a\" = \"agent::Toggle\", \"ctrl-alt-n\" = \"agent::NewThread\", \"ctrl-alt-q\" = \"workspace::SendKeystrokes\" }\n",
    )
    .unwrap()
}

#[test]
fn zed_capture_takes_only_plain_ai_actions() {
    let sb = Sandbox::empty();
    write(&sb, ZED_KEYMAP, KEYMAP);
    let r = capture(&["zed"], &sb.env(Scope::User));
    let kb = &r.profile.editor["zed"].keybindings;
    let all: Vec<(&str, &str)> = kb
        .iter()
        .flat_map(|b| b.bindings.iter().map(|(k, v)| (k.as_str(), v.as_str())))
        .collect();
    assert_eq!(
        all,
        vec![
            ("ctrl-alt-a", "agent::Toggle"),
            ("ctrl-alt-i", "assistant::InlineAssist")
        ]
    );
    let toml = profile_io::manifest_toml(&r.profile);
    assert!(
        !toml.contains("SendKeystrokes")
            && !toml.contains("AllowAlways")
            && !toml.contains("task::Spawn"),
        "{toml}"
    );
}

#[test]
fn zed_apply_appends_bindings_keeps_the_keymap_and_undo_restores_it() {
    let sb = Sandbox::empty();
    write(&sb, ZED_KEYMAP, KEYMAP);
    let before = sb.read(ZED_KEYMAP);
    let p = zed_profile();
    let env = sb.env(Scope::User);
    let plan = apply(&sb, &p, &["zed"], &env);
    let after = sb.read(ZED_KEYMAP);
    assert!(
        after.starts_with(before.trim_end().trim_end_matches(']').trim_end()),
        "{after}"
    );
    assert!(after.contains("// mine"), "{after}");
    assert!(after.contains("agent::NewThread"), "{after}");
    assert_eq!(
        after.matches("SendKeystrokes").count(),
        before.matches("SendKeystrokes").count(),
        "an action off the list is not added: {after}"
    );
    // ctrl-alt-a is already bound to the same action here: not repeated.
    assert_eq!(after.matches("agent::Toggle").count(), 1, "{after}");
    let v: serde_json::Value = serde_json::from_str(&after.replace("// mine", "")).unwrap();
    assert_eq!(
        v.as_array().unwrap().len(),
        3,
        "one appended block: {after}"
    );
    assert!(
        plan.notes
            .iter()
            .any(|n| n.message.contains("not applied") && n.message.contains("SendKeystrokes")),
        "{:?}",
        plan.notes
    );
    assert!(
        self::plan(&p, &["zed"], &env)
            .changes
            .iter()
            .all(|c| !c.path.ends_with("keymap.json"))
    );
    Store::new(sb.state()).undo(None, false).unwrap();
    assert_eq!(sb.read(ZED_KEYMAP), before);
}

#[test]
fn zed_apply_creates_the_keymap_and_wins_over_a_conflicting_binding() {
    let sb = Sandbox::empty();
    let env = sb.env(Scope::User);
    apply(&sb, &zed_profile(), &["zed"], &env);
    let v = sb.json(ZED_KEYMAP);
    assert_eq!(
        v[0]["bindings"],
        json!({"ctrl-alt-a": "agent::Toggle", "ctrl-alt-n": "agent::NewThread"})
    );
    // Another binding of the same key: the profile's block comes last, where Zed lets it win.
    let sb = Sandbox::empty();
    write(
        &sb,
        ZED_KEYMAP,
        r#"[{"context": "Workspace", "bindings": {"ctrl-alt-a": "workspace::ToggleZoom"}}]"#,
    );
    let plan = apply(&sb, &zed_profile(), &["zed"], &sb.env(Scope::User));
    let v = sb.json(ZED_KEYMAP);
    assert_eq!(v[1]["bindings"]["ctrl-alt-a"], "agent::Toggle");
    assert!(
        plan.notes.iter().any(|n| n.message.contains("wins in Zed")),
        "{:?}",
        plan.notes
    );
}

// ------------------------------------------------------------ extensions, with a stub `code`

#[cfg(unix)]
mod with_stub {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    /// A stub `code` that keeps its "installed extensions" and every call in files.
    pub struct Stub {
        pub bin: PathBuf,
        pub log: PathBuf,
        pub installed: PathBuf,
    }

    impl Stub {
        pub fn new(sb: &Sandbox, preinstalled: &[&str]) -> Stub {
            let bin = sb.path("stubbin");
            fs::create_dir_all(&bin).unwrap();
            let log = sb.path("code.log");
            let installed = sb.path("code.installed");
            fs::write(&installed, preinstalled.join("\n") + "\n").unwrap();
            let script = format!(
                "#!/bin/sh\necho \"$@\" >> '{log}'\ncase \"$1\" in\n  --list-extensions) cat '{db}';;\n  --install-extension) echo \"$2\" >> '{db}'; echo \"Extension '$2' was successfully installed.\";;\n  *) echo \"unexpected: $*\" >&2; exit 2;;\nesac\n",
                log = log.display(),
                db = installed.display()
            );
            let f = bin.join("code");
            fs::write(&f, script).unwrap();
            fs::set_permissions(&f, fs::Permissions::from_mode(0o755)).unwrap();
            Stub {
                bin,
                log,
                installed,
            }
        }
        pub fn calls(&self) -> String {
            fs::read_to_string(&self.log).unwrap_or_default()
        }
    }

    /// The CLI with only the stub (and the system's basic tools) on PATH.
    pub fn cli(sb: &Sandbox, stub: Option<&Stub>) -> assert_cmd::Command {
        let mut c = sb.cli();
        let path = match stub {
            Some(s) => format!(
                "{}:{}",
                s.bin.display(),
                std::env::var("PATH").unwrap_or_default()
            ),
            None => sb.path("emptybin").display().to_string(),
        };
        fs::create_dir_all(sb.path("emptybin")).unwrap();
        c.env("PATH", path);
        c
    }

    pub fn run(c: &mut assert_cmd::Command, args: &[&str]) -> (bool, String) {
        let out = c.args(args).output().unwrap();
        (
            out.status.success(),
            format!(
                "{}{}",
                String::from_utf8_lossy(&out.stdout),
                String::from_utf8_lossy(&out.stderr)
            ),
        )
    }

    pub fn profile_dir(sb: &Sandbox) -> String {
        let mut p = super::manifest(super::PROFILE).unwrap();
        p.editor.remove("zed");
        write(sb, "prof/kitbag.toml", &profile_io::manifest_toml(&p));
        "prof".to_string()
    }

    #[test]
    fn capture_lists_extensions_by_id_and_carries_only_the_listed_ones() {
        let sb = Sandbox::empty();
        let stub = Stub::new(
            &sb,
            &[
                "ms-python.python",
                "Continue.continue",
                "esbenp.prettier-vscode",
            ],
        );
        write(
            &sb,
            super::cli_vscode_settings(),
            "{\n \"editor.fontSize\": 15,\n \"chat.agent.enabled\": true,\n \"chat.agent.maxRequests\": \"lots\",\n \"github.copilot.advanced\": {\"debug.overrideProxyUrl\": \"https://proxy.example/\"}\n}\n",
        );
        let (ok, out) = run(
            &mut cli(&sb, Some(&stub)),
            &["capture", "--from", "vscode", "--out", "captured"],
        );
        assert!(ok, "{out}");
        let toml = sb.read("captured/kitbag.toml");
        assert!(toml.contains("Continue.continue"), "{toml}");
        assert!(
            !toml.contains("ms-python") && !toml.contains("prettier"),
            "{toml}"
        );
        assert!(toml.contains("\"chat.agent.enabled\" = true"), "{toml}");
        assert!(
            !toml.contains("fontSize") && !toml.contains("maxRequests") && !toml.contains("proxy"),
            "{toml}"
        );
        assert_eq!(stub.calls().trim(), "--list-extensions");
    }

    #[test]
    fn capture_without_code_on_path_says_so() {
        let sb = Sandbox::empty();
        write(
            &sb,
            super::cli_vscode_settings(),
            "{\"chat.agent.enabled\": false}\n",
        );
        let (ok, out) = run(
            &mut cli(&sb, None),
            &["capture", "--from", "vscode", "--out", "captured"],
        );
        assert!(ok, "{out}");
        assert!(out.contains("`code` is not on PATH"), "{out}");
        assert!(
            sb.read("captured/kitbag.toml")
                .contains("chat.agent.enabled")
        );
    }

    #[test]
    fn apply_lists_every_extension_installs_only_the_missing_and_undo_keeps_them() {
        let sb = Sandbox::empty();
        let stub = Stub::new(&sb, &["continue.continue"]);
        let dir = profile_dir(&sb);
        write(
            &sb,
            super::cli_vscode_settings(),
            "{\n  \"editor.fontSize\": 15\n}\n",
        );
        let before = sb.read(super::cli_vscode_settings());
        let (ok, out) = run(
            &mut cli(&sb, Some(&stub)),
            &["apply", &dir, "--to", "vscode", "--yes"],
        );
        assert!(ok, "{out}");
        // The plan names each extension as publisher.extension (with its version).
        assert!(
            out.contains("continue.continue") || out.contains("Continue.continue"),
            "{out}"
        );
        assert!(
            out.contains("saoudrizwan.claude-dev (version 3.0.0)"),
            "{out}"
        );
        assert!(out.contains("already installed"), "{out}");
        // Only the missing one is installed, exactly as written in the profile.
        let calls = stub.calls();
        assert!(
            calls.contains("--install-extension saoudrizwan.claude-dev@3.0.0"),
            "{calls}"
        );
        assert!(
            !calls.contains("--install-extension Continue.continue"),
            "{calls}"
        );
        assert!(!calls.contains("uninstall"), "{calls}");
        let s = sb.read(super::cli_vscode_settings());
        assert!(
            s.contains("\"chat.agent.maxRequests\": 40") && s.contains("editor.fontSize"),
            "{s}"
        );
        // Undo restores settings.json and uninstalls nothing.
        let (ok, out) = run(&mut cli(&sb, Some(&stub)), &["undo"]);
        assert!(ok, "{out}");
        assert_eq!(sb.read(super::cli_vscode_settings()), before);
        assert!(!stub.calls().contains("uninstall"));
        assert!(
            fs::read_to_string(&stub.installed)
                .unwrap()
                .contains("saoudrizwan.claude-dev@3.0.0")
        );
        assert!(out.contains("Undid"), "{out}");
    }

    #[test]
    fn dry_run_and_diff_install_nothing_and_a_prompt_is_needed_otherwise() {
        let sb = Sandbox::empty();
        let stub = Stub::new(&sb, &[]);
        let dir = profile_dir(&sb);
        for args in [
            vec!["apply", &dir, "--to", "vscode", "--dry-run"],
            vec!["diff", &dir, "--to", "vscode"],
        ] {
            let (_, out) = run(&mut cli(&sb, Some(&stub)), &args);
            assert!(out.contains("saoudrizwan.claude-dev"), "{out}");
        }
        // No terminal and no --yes: refuses before writing or installing.
        let (ok, out) = run(
            &mut cli(&sb, Some(&stub)),
            &["apply", &dir, "--to", "vscode"],
        );
        assert!(!ok, "{out}");
        assert!(
            !stub.calls().contains("--install-extension"),
            "{}",
            stub.calls()
        );
        assert!(!sb.path(super::cli_vscode_settings()).exists());
    }

    #[test]
    fn without_code_on_path_the_plan_warns_and_nothing_is_called_installed() {
        let sb = Sandbox::empty();
        let dir = profile_dir(&sb);
        let (ok, out) = run(
            &mut cli(&sb, None),
            &["apply", &dir, "--to", "vscode", "--yes"],
        );
        assert!(ok, "{out}");
        assert!(out.contains("`code` is not on PATH"), "{out}");
        assert!(out.contains("not installed"), "{out}");
        assert!(!out.contains("Installed extension"), "{out}");
        // The settings still went in.
        assert!(
            sb.read(super::cli_vscode_settings())
                .contains("chat.agent.enabled")
        );
    }

    #[test]
    fn from_a_git_source_extensions_need_trust_source() {
        if !has_git() {
            return;
        }
        let sb = Sandbox::empty();
        let stub = Stub::new(&sb, &[]);
        let mut p = super::manifest(super::PROFILE).unwrap();
        p.editor.remove("zed");
        let url = sb.git_profile("remote", &[("kitbag.toml", &profile_io::manifest_toml(&p))]);
        let (ok, out) = run(
            &mut cli(&sb, Some(&stub)),
            &["apply", &url, "--to", "vscode", "--yes"],
        );
        assert!(ok, "{out}");
        assert!(out.contains("--trust-source"), "{out}");
        assert!(
            out.contains("Continue.continue") && out.contains("saoudrizwan.claude-dev"),
            "{out}"
        );
        assert!(
            !stub.calls().contains("--install-extension"),
            "{}",
            stub.calls()
        );
        // The AI settings are plain data and still apply.
        assert!(
            sb.read(super::cli_vscode_settings())
                .contains("chat.agent.enabled")
        );
        let (ok, out) = run(
            &mut cli(&sb, Some(&stub)),
            &["apply", &url, "--to", "vscode", "--yes", "--trust-source"],
        );
        assert!(ok, "{out}");
        let calls = stub.calls();
        assert!(
            calls.contains("--install-extension Continue.continue"),
            "{calls}"
        );
        assert!(
            calls.contains("--install-extension saoudrizwan.claude-dev@3.0.0"),
            "{calls}"
        );
    }

    #[test]
    fn extensions_not_on_the_list_are_never_installed() {
        let sb = Sandbox::empty();
        let stub = Stub::new(&sb, &[]);
        write(
            &sb,
            "prof/kitbag.toml",
            "kitbag = 1\nname = \"p\"\n[editor.vscode]\nextensions = [\"ms-python.python\", \"RooVeterinaryInc.roo-cline\"]\n",
        );
        let (ok, out) = run(
            &mut cli(&sb, Some(&stub)),
            &["apply", "prof", "--to", "vscode", "--yes"],
        );
        assert!(ok, "{out}");
        let calls = stub.calls();
        assert!(
            calls.contains("--install-extension RooVeterinaryInc.roo-cline"),
            "{calls}"
        );
        assert!(!calls.contains("ms-python"), "{calls}");
        assert!(
            out.contains("ms-python.python"),
            "the plan says what was left out: {out}"
        );
    }

    #[test]
    fn inspect_lists_the_extensions_as_code_that_runs() {
        let sb = Sandbox::empty();
        let dir = profile_dir(&sb);
        let (ok, out) = run(&mut cli(&sb, None), &["inspect", &dir]);
        assert!(ok, "{out}");
        assert!(out.contains("vscode: extension Continue.continue"), "{out}");
    }

    #[test]
    fn zed_keymap_through_the_cli_and_zed_has_no_extension_cli_calls() {
        let sb = Sandbox::empty();
        let stub = Stub::new(&sb, &[]);
        write(
            &sb,
            "prof/kitbag.toml",
            "kitbag = 1\nname = \"p\"\n[[editor.zed.keybindings]]\nbindings = { \"ctrl-alt-a\" = \"agent::Toggle\" }\n",
        );
        let (ok, out) = run(
            &mut cli(&sb, Some(&stub)),
            &["apply", "prof", "--to", "zed", "--yes"],
        );
        assert!(ok, "{out}");
        assert_eq!(
            sb.json(super::cli_zed_keymap())[0]["bindings"]["ctrl-alt-a"],
            "agent::Toggle"
        );
        assert_eq!(
            stub.calls(),
            "",
            "Zed installs its own extensions; no editor command is run"
        );
    }
}
