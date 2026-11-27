//! Editor extensions, AI settings and AI keybindings (`[editor.<id>]`, #19).
//!
//! A small, allow-listed slice of what an editor holds, not a copy of the
//! editor: no theme, font, window or formatting settings, no snippets and no
//! Settings Sync. What is carried, and where each item comes from (docs read
//! 2026-11-27):
//!
//! * `[editor.vscode]`, VS Code's default profile at user scope, and the project's `.vscode/settings.json` at project scope (settings only, #144)
//!   (`User/settings.json`, <https://code.visualstudio.com/docs/configure/settings>;
//!   `User/profiles/<id>/` is never touched, and kitbag cannot tell which
//!   profile is active, see <https://code.visualstudio.com/docs/configure/profiles>):
//!   - `extensions`: `publisher.extension` ids, optionally `@version`, from
//!     the list [`EXTENSIONS`], installed with `code --install-extension`
//!     (<https://code.visualstudio.com/docs/configure/command-line>);
//!   - `settings`: the AI keys of [`VSCODE_SETTINGS`], all from
//!     <https://code.visualstudio.com/docs/copilot/reference/copilot-settings>,
//!     with the type shown there.
//!
//!   VS Code's `keybindings.json` is **not** carried: its keybindings page
//!   names no path for the file (checked 2026-11-27).
//! * `[editor.zed]`: `keybindings`, merged into `keymap.json` in Zed's config
//!   directory (<https://zed.dev/docs/key-bindings>). Only bindings of plain
//!   actions named in [`ZED_ACTIONS`] (from <https://zed.dev/docs/all-actions>).
//!   Zed's extensions are `[plugins.zed]` and its agent settings are
//!   `[settings.zed]`; neither is repeated here.
//! * Skipped, with the reason in the README: Cursor's editor, Windsurf,
//!   VSCodium, VS Code Insiders, and the extensions of GitHub Copilot.
//!
//! Extensions run code, so they are listed in every plan, and a profile from a
//! git source installs none of them without `--trust-source`.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

pub const VSCODE: &str = "vscode";
pub const ZED: &str = "zed";
/// The editors `[editor.<id>]` may name.
pub const EDITORS: &[&str] = &[VSCODE, ZED];

/// Extensions kitbag carries: the exact `publisher.extension` id as the
/// extension's own official docs or site publish it, and where.
pub const EXTENSIONS: &[(&str, &str)] = &[
    (
        "Continue.continue",
        "https://docs.continue.dev/ide-extensions/install",
    ),
    ("saoudrizwan.claude-dev", "https://cline.bot/install"),
    (
        "RooVeterinaryInc.roo-cline",
        "https://roocodeinc.github.io/Roo-Code/getting-started/installing",
    ),
];

/// What a setting's value may be.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Bool,
    /// A whole number from 1 to 1000.
    Count,
    /// An object of `language id` -> true/false.
    BoolMap,
}

/// The VS Code settings kitbag carries, all documented (with these types) at
/// <https://code.visualstudio.com/docs/copilot/reference/copilot-settings> on
/// 2026-11-27. Nothing flagged Experimental or Preview there, nothing about
/// auto-approval, terminal commands, sandboxes, endpoints or models.
pub const VSCODE_SETTINGS: &[(&str, Kind)] = &[
    ("chat.agent.enabled", Kind::Bool),
    ("chat.agent.maxRequests", Kind::Count),
    ("chat.checkpoints.enabled", Kind::Bool),
    ("chat.commandCenter.enabled", Kind::Bool),
    ("chat.disableAIFeatures", Kind::Bool),
    ("chat.useAgentSkills", Kind::Bool),
    ("chat.useAgentsMdFile", Kind::Bool),
    ("chat.useClaudeMdFile", Kind::Bool),
    ("github.copilot.editor.enableCodeActions", Kind::Bool),
    ("github.copilot.enable", Kind::BoolMap),
    ("github.copilot.nextEditSuggestions.enabled", Kind::Bool),
    (
        "github.copilot.chat.codeGeneration.useInstructionFiles",
        Kind::Bool,
    ),
];

/// The Zed actions whose keybindings kitbag carries, as
/// <https://zed.dev/docs/all-actions> spells them (2026-11-27): opening and
/// focusing the agent panel, threads, model and profile pickers, reviewing
/// changes and inline assist. Not the ones that approve a tool call
/// (`agent::AllowAlways`, `agent::AllowOnce`, `agent::AuthorizeToolCall`),
/// and none that take arguments.
pub const ZED_ACTIONS: &[&str] = &[
    "agent::AddSelectionToThread",
    "agent::CycleFavoriteModels",
    "agent::ExpandMessageEditor",
    "agent::Keep",
    "agent::KeepAll",
    "agent::NewThread",
    "agent::OpenAgentDiff",
    "agent::Reject",
    "agent::RejectAll",
    "agent::Toggle",
    "agent::ToggleFocus",
    "agent::ToggleModelSelector",
    "agent::ToggleNewThreadMenu",
    "agent::ToggleProfileSelector",
    "agent::ToggleThinkingMode",
    "agent::UndoLastReject",
    "assistant::InlineAssist",
];

/// One editor's part of a profile.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EditorSet {
    /// `publisher.extension`, or `publisher.extension@version`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub extensions: Vec<String>,
    #[serde(default, skip_serializing_if = "Map::is_empty")]
    pub settings: Map<String, Value>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub keybindings: Vec<KeyBlock>,
}

impl EditorSet {
    pub fn is_empty(&self) -> bool {
        self.extensions.is_empty() && self.settings.is_empty() && self.keybindings.is_empty()
    }
}

/// One entry of Zed's `keymap.json`: bindings of keystrokes to actions, in a context.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct KeyBlock {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context: Option<String>,
    pub bindings: BTreeMap<String, String>,
}

/// `publisher.extension` and the version, if any.
pub fn split_spec(spec: &str) -> (&str, Option<&str>) {
    match spec.split_once('@') {
        Some((id, v)) => (id, Some(v)),
        None => (spec, None),
    }
}

fn id_part_ok(s: &str, allow_dot_underscore: bool) -> bool {
    !s.is_empty()
        && s.len() <= 64
        && s.starts_with(|c: char| c.is_ascii_alphanumeric())
        && s.chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || (allow_dot_underscore && c == '_'))
}

/// What is wrong with an extension spec, if anything: it must be exactly
/// `publisher.extension` (or with `@version`), never a path, a URL or a VSIX.
pub fn spec_issue(spec: &str) -> Option<String> {
    let shown = crate::ui::visible_line(spec);
    if spec.contains('/')
        || spec.contains('\\')
        || spec.contains(':')
        || spec.to_ascii_lowercase().ends_with(".vsix")
    {
        return Some(format!(
            "'{shown}' is a path or URL; only exact publisher.extension ids are carried, never a VSIX or a link"
        ));
    }
    let (id, version) = split_spec(spec);
    let ok = id
        .split_once('.')
        .is_some_and(|(p, n)| id_part_ok(p, false) && id_part_ok(n, true));
    if !ok {
        return Some(format!(
            "'{shown}' is not a publisher.extension id (letters, digits and '-', one '.')"
        ));
    }
    if let Some(v) = version
        && !(v.len() <= 64
            && v.starts_with(|c: char| c.is_ascii_digit())
            && v.chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '+')))
    {
        return Some(format!("'{shown}': the version after '@' is not valid"));
    }
    None
}

/// Is `spec`'s id one of [`EXTENSIONS`]? (VS Code ids are case-insensitive;
/// the spelling in the profile is kept as it is.)
pub fn extension_allowed(spec: &str) -> bool {
    let (id, _) = split_spec(spec);
    EXTENSIONS.iter().any(|(e, _)| e.eq_ignore_ascii_case(id))
}

/// `publisher.extension`, with the version when there is one, for plans.
pub fn describe_extension(spec: &str) -> String {
    let (id, v) = split_spec(spec);
    match v {
        Some(v) => format!("{id} (version {v})"),
        None => id.to_string(),
    }
}

fn setting_kind(key: &str) -> Option<Kind> {
    VSCODE_SETTINGS
        .iter()
        .find(|(k, _)| *k == key)
        .map(|(_, k)| *k)
}

/// Why `v` doesn't fit `kind`, if it doesn't.
fn value_issue(kind: Kind, v: &Value) -> Option<&'static str> {
    match kind {
        Kind::Bool if v.is_boolean() => None,
        Kind::Bool => Some("must be true or false"),
        Kind::Count => match v.as_u64() {
            Some(1..=1000) => None,
            _ => Some("must be a whole number from 1 to 1000"),
        },
        Kind::BoolMap => match v {
            Value::Object(m)
                if m.len() <= 200
                    && m.iter().all(|(k, x)| {
                        x.is_boolean()
                            && !k.is_empty()
                            && k.len() <= 64
                            && !k.chars().any(char::is_control)
                    }) =>
            {
                None
            }
            _ => Some("must be a table of language id = true/false"),
        },
    }
}

fn action_allowed(action: &str) -> bool {
    ZED_ACTIONS.contains(&action)
}

fn key_text_ok(s: &str) -> bool {
    !s.trim().is_empty() && s.len() <= 200 && !s.chars().any(char::is_control)
}

/// The part of `set` kitbag carries for `editor`, and what was left out and why.
pub fn allowed(editor: &str, set: &EditorSet) -> (EditorSet, Vec<String>) {
    let mut out = EditorSet::default();
    let mut dropped = Vec::new();
    for spec in &set.extensions {
        if spec_issue(spec).is_some() || editor != VSCODE {
            dropped.push(format!("extension {}", crate::ui::visible_line(spec)));
        } else if !extension_allowed(spec) {
            dropped.push(format!(
                "extension {} (not one kitbag carries)",
                crate::ui::visible_line(spec)
            ));
        } else if !out.extensions.contains(spec) {
            out.extensions.push(spec.clone());
        }
    }
    for (k, v) in &set.settings {
        match setting_kind(k) {
            Some(kind) if editor == VSCODE && value_issue(kind, v).is_none() => {
                out.settings.insert(k.clone(), v.clone());
            }
            _ => dropped.push(format!("setting {}", crate::ui::visible_line(k))),
        }
    }
    for block in &set.keybindings {
        let mut kept = KeyBlock {
            context: block.context.clone(),
            bindings: BTreeMap::new(),
        };
        if block.context.as_deref().is_some_and(|c| !key_text_ok(c)) {
            dropped.push("a keybinding block with an invalid context".to_string());
            continue;
        }
        for (key, action) in &block.bindings {
            if editor == ZED && key_text_ok(key) && action_allowed(action) {
                kept.bindings.insert(key.clone(), action.clone());
            } else {
                dropped.push(format!(
                    "keybinding {} = {}",
                    crate::ui::visible_line(key),
                    crate::ui::visible_line(action)
                ));
            }
        }
        if !kept.bindings.is_empty() {
            out.keybindings.push(kept);
        }
    }
    (out, dropped)
}

/// Check the `[editor]` tables of a manifest: (errors, warnings).
pub fn validate(editor: &BTreeMap<String, EditorSet>) -> (Vec<String>, Vec<String>) {
    let mut issues = Vec::new();
    let mut warnings = Vec::new();
    for (id, set) in editor {
        if !EDITORS.contains(&id.as_str()) {
            issues.push(format!(
                "editor.{}: unknown editor (kitbag carries: {})",
                crate::ui::visible_line(id),
                EDITORS.join(", ")
            ));
            continue;
        }
        if id == ZED {
            if !set.extensions.is_empty() {
                issues.push(
                    "editor.zed.extensions: Zed's extensions are carried in [plugins.zed]"
                        .to_string(),
                );
            }
            if !set.settings.is_empty() {
                issues.push(
                    "editor.zed.settings: Zed's agent settings are carried in [settings.zed]"
                        .to_string(),
                );
            }
        } else if !set.keybindings.is_empty() {
            issues.push(
                "editor.vscode.keybindings: not carried; VS Code's docs name no path for keybindings.json"
                    .to_string(),
            );
        }
        for spec in &set.extensions {
            if let Some(i) = spec_issue(spec) {
                issues.push(format!("editor.{id}.extensions: {i}"));
            }
        }
        for (k, v) in &set.settings {
            if id != VSCODE {
                continue;
            }
            match setting_kind(k) {
                Some(kind) => {
                    if let Some(why) = value_issue(kind, v) {
                        issues.push(format!("editor.{id}.settings.{k}: {why}"));
                    }
                }
                None => warnings.push(format!(
                    "editor.{id}.settings.{}: not one of the AI settings kitbag carries; it won't be applied",
                    crate::ui::visible_line(k)
                )),
            }
        }
        for (n, block) in set.keybindings.iter().enumerate() {
            if let Some(c) = &block.context
                && !key_text_ok(c)
            {
                issues.push(format!(
                    "editor.{id}.keybindings[{n}].context: must be text without control characters"
                ));
            }
            if block.bindings.is_empty() {
                issues.push(format!("editor.{id}.keybindings[{n}]: no bindings"));
            }
            for (key, action) in &block.bindings {
                if !key_text_ok(key) {
                    issues.push(format!(
                        "editor.{id}.keybindings[{n}]: a keystroke must be text without control characters"
                    ));
                } else if !action_allowed(action) {
                    warnings.push(format!(
                        "editor.{id}.keybindings[{n}]: '{}' is not an AI action kitbag carries; it won't be applied",
                        crate::ui::visible_line(action)
                    ));
                }
            }
        }
        for spec in set.extensions.iter().filter(|s| spec_issue(s).is_none()) {
            if !extension_allowed(spec) {
                warnings.push(format!(
                    "editor.{id}.extensions: {} is not an extension kitbag carries; it won't be installed",
                    crate::ui::visible_line(spec)
                ));
            }
        }
    }
    (issues, warnings)
}

/// What a plan does about one extension.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExtState {
    /// Not installed yet; installed after the files are written.
    Install,
    /// Already installed: left alone, and never removed by `undo`.
    Installed,
    /// The editor's command is not on PATH: nothing is installed.
    NoCli,
}

/// One extension a plan lists.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExtensionPlan {
    pub target: String,
    /// `publisher.extension[@version]`, as in the profile.
    pub spec: String,
    pub state: ExtState,
}

impl ExtensionPlan {
    /// One line of the plan.
    pub fn line(&self) -> String {
        let what = match self.state {
            ExtState::Install => "will be installed with `code --install-extension`",
            ExtState::Installed => "already installed, left as it is",
            ExtState::NoCli => "not installed: `code` is not on PATH",
        };
        format!(
            "{}: {} {what}",
            self.target,
            crate::ui::visible_line(&describe_extension(&self.spec))
        )
    }
}

/// The command that starts VS Code's CLI, found on PATH.
pub fn find_cli() -> Option<PathBuf> {
    let name = if cfg!(windows) { "code.cmd" } else { "code" };
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path)
        .map(|d| d.join(name))
        .find(|c| is_runnable(c))
}

#[cfg(unix)]
fn is_runnable(p: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    p.metadata()
        .is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
}

#[cfg(not(unix))]
fn is_runnable(p: &Path) -> bool {
    p.is_file()
}

/// The extensions VS Code has installed (`code --list-extensions`), as printed.
pub fn list_installed(cli: &Path) -> Result<Vec<String>, String> {
    let out = Command::new(cli)
        .arg("--list-extensions")
        .stdin(Stdio::null())
        .output()
        .map_err(|e| format!("could not run {}: {e}", cli.display()))?;
    if !out.status.success() {
        return Err(format!(
            "{} --list-extensions failed ({})",
            cli.display(),
            out.status
        ));
    }
    Ok(String::from_utf8_lossy(&out.stdout)
        .lines()
        .map(str::trim)
        .filter(|l| spec_issue(l).is_none())
        .map(str::to_string)
        .collect())
}

/// Install one extension (`code --install-extension publisher.extension[@version]`).
pub fn install(cli: &Path, spec: &str) -> Result<(), String> {
    if spec_issue(spec).is_some() {
        return Err("not a publisher.extension id".to_string());
    }
    let out = Command::new(cli)
        .arg("--install-extension")
        .arg(spec)
        .stdin(Stdio::null())
        .output()
        .map_err(|e| format!("could not run {}: {e}", cli.display()))?;
    if out.status.success() {
        Ok(())
    } else {
        let err = String::from_utf8_lossy(&out.stderr);
        let first = err.lines().next().unwrap_or("").trim();
        Err(format!(
            "{} failed ({}){}",
            cli.display(),
            out.status,
            if first.is_empty() {
                String::new()
            } else {
                format!(": {}", crate::ui::visible_line(first))
            }
        ))
    }
}

/// Plan the extensions of `set` (already allow-listed): which are installed,
/// which would be installed, or that the CLI is missing.
pub fn plan_extensions(
    target: &str,
    set: &EditorSet,
    cli: Option<&Path>,
) -> (Vec<ExtensionPlan>, Option<String>) {
    let mut warning = None;
    let installed = match cli {
        Some(c) => match list_installed(c) {
            Ok(l) => Some(l),
            Err(e) => {
                warning = Some(e);
                None
            }
        },
        None => None,
    };
    let plans = set
        .extensions
        .iter()
        .map(|spec| {
            let (id, _) = split_spec(spec);
            let state = match &installed {
                None => ExtState::NoCli,
                Some(l) if l.iter().any(|i| split_spec(i).0.eq_ignore_ascii_case(id)) => {
                    ExtState::Installed
                }
                Some(_) => ExtState::Install,
            };
            ExtensionPlan {
                target: target.to_string(),
                spec: spec.clone(),
                state,
            }
        })
        .collect();
    (plans, warning)
}

/// Take every extension out of `profile`, for a profile from a git source
/// without `--trust-source`: (editor, description) of each.
pub fn drop_untrusted(profile: &mut crate::model::Profile) -> Vec<(String, String)> {
    let mut out = Vec::new();
    for (id, set) in &mut profile.editor {
        for spec in std::mem::take(&mut set.extensions) {
            out.push((id.clone(), describe_extension(&spec)));
        }
    }
    out
}

/// Every extension the profile holds, `editor: publisher.extension`, for `inspect`.
pub fn list(profile: &crate::model::Profile) -> Vec<String> {
    profile
        .editor
        .iter()
        .flat_map(|(id, set)| {
            set.extensions
                .iter()
                .map(move |s| format!("{id}: extension {}", describe_extension(s)))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn specs_are_exact_ids() {
        for ok in ["Continue.continue", "saoudrizwan.claude-dev@3.1.0", "a.b_c"] {
            assert!(spec_issue(ok).is_none(), "{ok}");
        }
        for bad in [
            "",
            "continue",
            "-x.y",
            "a.b.c",
            "a.b@",
            "a.b@latest",
            "https://x/y.vsix",
            "./a.vsix",
            "a.vsix",
            "C:\\a.b",
            "a b.c",
            "--force.x",
        ] {
            assert!(spec_issue(bad).is_some(), "{bad}");
        }
    }

    #[test]
    fn allow_list_is_case_insensitive_but_keeps_the_spelling() {
        let set = EditorSet {
            extensions: vec!["continue.continue".into(), "ms-python.python".into()],
            ..EditorSet::default()
        };
        let (kept, dropped) = allowed(VSCODE, &set);
        assert_eq!(kept.extensions, vec!["continue.continue".to_string()]);
        assert_eq!(dropped.len(), 1);
        assert!(dropped[0].contains("ms-python.python"));
    }

    #[test]
    fn settings_are_typed_and_listed() {
        let mut set = EditorSet::default();
        set.settings
            .insert("chat.agent.enabled".into(), json!(true));
        set.settings
            .insert("chat.agent.maxRequests".into(), json!(5000));
        set.settings
            .insert("chat.tools.global.autoApprove".into(), json!(true));
        set.settings.insert("editor.fontSize".into(), json!(40));
        let (kept, dropped) = allowed(VSCODE, &set);
        assert_eq!(kept.settings.len(), 1);
        assert_eq!(dropped.len(), 3);
    }

    #[test]
    fn only_listed_plain_actions_are_bound() {
        let set = EditorSet {
            keybindings: vec![KeyBlock {
                context: Some("Workspace".into()),
                bindings: BTreeMap::from([
                    ("ctrl-alt-a".to_string(), "agent::Toggle".to_string()),
                    ("ctrl-alt-b".to_string(), "agent::AllowAlways".to_string()),
                    (
                        "ctrl-alt-c".to_string(),
                        "workspace::SendKeystrokes".to_string(),
                    ),
                ]),
            }],
            ..EditorSet::default()
        };
        let (kept, dropped) = allowed(ZED, &set);
        assert_eq!(kept.keybindings[0].bindings.len(), 1);
        assert_eq!(dropped.len(), 2);
        // VS Code has no keybindings in this slice.
        let (none, _) = allowed(VSCODE, &set);
        assert!(none.keybindings.is_empty());
    }
}
