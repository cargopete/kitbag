//! VS Code, the editor itself (not Copilot, Continue, Cline or Roo, which have
//! their own targets): a few AI extensions and AI settings, from the
//! `[editor.vscode]` table of a profile. See [`crate::editor`] for what is
//! carried and why.
//!
//! * Settings: the AI keys of [`crate::editor::VSCODE_SETTINGS`], merged into
//!   `User/settings.json` of the default profile (Windows
//!   `%APPDATA%\Code\User`, macOS `~/Library/Application Support/Code/User`,
//!   Linux `~/.config/Code/User`; <https://code.visualstudio.com/docs/configure/settings>,
//!   checked 2026-11-27). `User/profiles/<id>/` is never written: VS Code's
//!   docs don't say how to tell which profile is active.
//! * Project scope (#144): the same allow-listed keys, merged into the
//!   project's `.vscode/settings.json` (the workspace settings file named on
//!   the same page). Nothing else is read or written there: no extensions
//!   (they are a user-level `code --install-extension` concern) and never
//!   `User/settings.json`; at user scope `.vscode/settings.json` is not touched.
//! * Extensions: `code --list-extensions` and `code --install-extension
//!   publisher.extension[@version]`
//!   (<https://code.visualstudio.com/docs/configure/command-line>). The
//!   installs run after the files are written, from the command line, and
//!   only when the plan was confirmed.
//! * `keybindings.json` is not carried: VS Code's keybindings page names no
//!   path for it (checked 2026-11-27).
//! * MCP servers, instructions, rules and the rest are not this target's:
//!   Copilot (`copilot`) covers VS Code's MCP file and instructions.

use std::path::PathBuf;

use anyhow::Result;
use serde_json::Value;

use super::common::merge_json_settings;
use super::{Adapter, ApplyOptions, Captured, Env, Feature, Os, Scope, Support, Work};
use crate::edit::json::parse_value;
use crate::editor::{self, EditorSet};
use crate::model::Profile;

const ID: &str = "vscode";

pub struct VsCode;

/// `User/` of VS Code's default profile.
pub fn user_dir(env: &Env) -> PathBuf {
    match env.os {
        Os::Windows => env
            .location_var("APPDATA")
            .unwrap_or_else(|| env.home.join("AppData").join("Roaming"))
            .join("Code")
            .join("User"),
        Os::MacOs => env
            .home
            .join("Library")
            .join("Application Support")
            .join("Code")
            .join("User"),
        Os::Linux => env.home.join(".config").join("Code").join("User"),
    }
}

/// The project's workspace settings file.
fn workspace_file(env: &Env) -> PathBuf {
    env.project.join(".vscode").join("settings.json")
}

fn settings_file(env: &Env) -> PathBuf {
    match env.scope {
        Scope::Project => workspace_file(env),
        Scope::User => user_dir(env).join("settings.json"),
    }
}

impl Adapter for VsCode {
    fn id(&self) -> &'static str {
        ID
    }
    fn name(&self) -> &'static str {
        "VS Code"
    }
    fn docs(&self) -> &'static str {
        "https://code.visualstudio.com/docs/configure/settings"
    }
    fn scopes(&self) -> &'static [Scope] {
        &[Scope::User, Scope::Project]
    }
    fn support(&self, _scope: Scope) -> [(Feature, Support); 8] {
        Feature::ALL.map(|f| {
            let s = match f {
                Feature::Settings => Support::Native,
                _ => Support::None,
            };
            (f, s)
        })
    }

    fn locations(&self, env: &Env) -> Vec<(&'static str, PathBuf)> {
        vec![("settings", settings_file(env))]
    }

    fn detect(&self, env: &Env) -> bool {
        match env.scope {
            Scope::Project => env.project.join(".vscode").is_dir(),
            Scope::User => user_dir(env).is_dir(),
        }
    }

    fn capture(&self, env: &Env) -> Result<Captured> {
        let mut cap = Captured::for_env(env);
        let mut set = EditorSet::default();
        let file = settings_file(env);
        if let Some(text) = cap.read_config(&file, ID)? {
            cap.sources.push(file.clone());
            if let Value::Object(doc) = parse_value(Some(&text), &file)? {
                for (k, _) in editor::VSCODE_SETTINGS {
                    if let Some(v) = doc.get(*k) {
                        let mut one = EditorSet::default();
                        one.settings.insert((*k).to_string(), v.clone());
                        // Only a value of the documented type is carried.
                        let (ok, _) = editor::allowed(ID, &one);
                        if ok.settings.is_empty() {
                            cap.warn(
                                ID,
                                format!("setting {k} does not hold a value of the type VS Code documents; not captured"),
                            );
                        } else {
                            set.settings.insert((*k).to_string(), v.clone());
                        }
                    }
                }
            }
        }
        // Without VS Code's folder in this home there is nothing to read, and
        // `code` would list the extensions of whoever runs kitbag instead.
        if env.scope == Scope::Project {
            if !set.is_empty() {
                cap.profile.editor.insert(ID.to_string(), set);
            }
            return Ok(cap);
        }
        let cli = if self.detect(env) {
            editor::find_cli()
        } else {
            return Ok(cap);
        };
        match cli {
            None => cap.info(
                ID,
                "`code` is not on PATH, so VS Code's extensions were not listed; add it to PATH (macOS: 'Shell Command: Install code in PATH') to capture them",
            ),
            Some(cli) => match editor::list_installed(&cli) {
                Err(e) => cap.warn(ID, format!("extensions not captured: {e}")),
                Ok(list) => {
                    let (mut carried, mut other) = (0, 0);
                    for spec in list {
                        if editor::extension_allowed(&spec)
                            && !set.extensions.iter().any(|e| e.eq_ignore_ascii_case(&spec))
                        {
                            set.extensions.push(spec);
                            carried += 1;
                        } else {
                            other += 1;
                        }
                    }
                    set.extensions.sort();
                    cap.info(
                        ID,
                        format!(
                            "{carried} AI extension(s) captured by id ({other} other installed extension(s) are not carried); extension files are not copied"
                        ),
                    );
                }
            },
        }
        if !set.is_empty() {
            cap.profile.editor.insert(ID.to_string(), set);
        }
        Ok(cap)
    }

    fn apply(
        &self,
        profile: &Profile,
        env: &Env,
        opts: &ApplyOptions,
        work: &mut Work,
    ) -> Result<()> {
        let Some(set) = profile.editor.get(ID) else {
            return Ok(());
        };
        let (set, dropped) = editor::allowed(ID, set);
        if !dropped.is_empty() {
            work.warn(
                ID,
                format!(
                    "not applied (not on kitbag's allow-list of AI items): {}",
                    dropped.join(", ")
                ),
            );
        }
        merge_json_settings(work, &settings_file(env), &set.settings, opts, ID)?;
        if env.scope == Scope::Project {
            if !set.extensions.is_empty() {
                work.info(
                    ID,
                    "extensions are not installed at project scope (they are a user-level `code --install-extension`); apply at user scope to install them",
                );
            }
            work.info(
                ID,
                "project scope: only the allow-listed AI settings in .vscode/settings.json; User/settings.json is not touched",
            );
            return Ok(());
        }
        if !set.extensions.is_empty() {
            let cli = editor::find_cli();
            let (plans, warning) = editor::plan_extensions(ID, &set, cli.as_deref());
            if let Some(w) = warning {
                work.warn(ID, format!("could not list the installed extensions: {w}"));
            }
            if cli.is_none() {
                work.warn(
                    ID,
                    "`code` is not on PATH, so no extension was installed; add it to PATH (macOS: 'Shell Command: Install code in PATH') and apply again",
                );
            }
            work.extensions.extend(plans);
            work.info(
                ID,
                "extensions installed by this apply stay installed when it is undone ('kitbag undo' restores settings.json only); remove one with `code --uninstall-extension publisher.extension`",
            );
        }
        work.info(
            ID,
            "VS Code's default profile only (User/settings.json); other profiles, workspace settings and keybindings.json are not touched",
        );
        Ok(())
    }
}
