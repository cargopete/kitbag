//! Roo Code (the VS Code extension `RooVeterinaryInc.roo-cline`): MCP servers
//! and instructions.
//!
//! * MCP, user: `mcp_settings.json` in the extension's `settings/` folder of
//!   the editor's global storage (`<user data>/User/globalStorage/
//!   rooveterinaryinc.roo-cline/settings/`). Roo's docs don't give the path;
//!   the file name and the `settings` folder are from its source
//!   (`GlobalFileNames.mcpSettings`, `getSettingsDirectoryPath`). Roo is an
//!   extension, so the folder lives under the host editor's user data
//!   directory: `~/.config/Code` (`$XDG_CONFIG_HOME/Code`) on Linux,
//!   `~/Library/Application Support/Code` on macOS, `%APPDATA%\Code` on Windows.
//!   kitbag uses the first of VS Code, VS Code Insiders, VSCodium and Cursor
//!   whose folder for the extension exists, and VS Code when none does. It
//!   can't see the `roo-cline.customStoragePath` setting.
//! * MCP, project: `.roo/mcp.json`, which wins over the user file for a server
//!   of the same name. Both hold `mcpServers`. Local servers take `command`,
//!   `args`, `env` and `cwd` (`type` is optional); remote servers take `url`
//!   and `headers` and need `type: "streamable-http"` or `"sse"`. `disabled`,
//!   `timeout`, `alwaysAllow`, `watchPaths` and `disabledTools` are Roo's own
//!   keys on an entry and are kept. Roo expands `${env:VAR}`, so references are
//!   written that way and no secret reaches the file.
//! * Instructions: a managed block in `~/.roo/rules/kitbag.md` (Roo reads every
//!   file in `~/.roo/rules/`, alphabetically), and `AGENTS.md` at the project
//!   root, which Roo reads and which is shared with Codex, Cursor,
//!   Antigravity, opencode, Grok Build and Windsurf. Rules are folded into
//!   the same block as labelled sections. Roo's own rule files
//!   (`.roo/rules/`, `.roo/rules-<mode>/`, `.roorules`) are not read; capture
//!   says so.
//! * Custom modes, commands, skills, hooks and settings are not carried.
//!
//! Docs: <https://roocodeinc.github.io/Roo-Code/features/mcp/using-mcp-in-roo>,
//! <https://roocodeinc.github.io/Roo-Code/features/custom-instructions>.

use std::path::PathBuf;

use anyhow::Result;
use serde_json::{Map, Value, json};

use super::common::{
    capture_instructions, import_server, merge_json_servers, note_unread_rule_files, render_server,
    server_entries, server_from_json, servers_for, write_instructions,
};
use super::{Adapter, ApplyOptions, Captured, Env, Feature, Os, Scope, Support, Work};
use crate::edit::json::parse_value;
use crate::model::{Profile, Transport};
use crate::secrets::RefStyle;

const ID: &str = "roo";
/// Server keys kitbag writes; anything else (`alwaysAllow`, `timeout`,
/// `watchPaths`, `disabledTools` ...) is kept.
const MANAGED_KEYS: &[&str] = &[
    "command", "args", "env", "cwd", "url", "headers", "type", "disabled",
];
/// The file kitbag writes in the global rules directory.
const RULES_FILE: &str = "kitbag.md";
/// The extension's folder in a VS Code user data directory (lower-cased id).
const EXTENSION_DIR: &str = "rooveterinaryinc.roo-cline";
/// Editors Roo runs in, as the name of their user data directory.
const HOSTS: &[&str] = &["Code", "Code - Insiders", "VSCodium", "Cursor"];

pub struct Roo;

/// An editor's `User` directory.
fn user_data_dir(env: &Env, host: &str) -> PathBuf {
    match env.os {
        Os::Windows => env
            .location_var("APPDATA")
            .unwrap_or_else(|| env.home.join("AppData").join("Roaming"))
            .join(host)
            .join("User"),
        Os::MacOs => env
            .home
            .join("Library")
            .join("Application Support")
            .join(host)
            .join("User"),
        Os::Linux => env
            .location_var("XDG_CONFIG_HOME")
            .unwrap_or_else(|| env.home.join(".config"))
            .join(host)
            .join("User"),
    }
}

fn storage_dir(env: &Env, host: &str) -> PathBuf {
    user_data_dir(env, host)
        .join("globalStorage")
        .join(EXTENSION_DIR)
}

/// The editors that have Roo's global storage on this machine.
fn hosts_with_roo(env: &Env) -> Vec<&'static str> {
    HOSTS
        .iter()
        .copied()
        .filter(|h| storage_dir(env, h).is_dir())
        .collect()
}

/// The user-level MCP file: in the first editor that has Roo, else VS Code's.
fn user_mcp_file(env: &Env) -> PathBuf {
    let host = hosts_with_roo(env).first().copied().unwrap_or(HOSTS[0]);
    storage_dir(env, host)
        .join("settings")
        .join("mcp_settings.json")
}

fn mcp_file(env: &Env) -> PathBuf {
    match env.scope {
        Scope::User => user_mcp_file(env),
        Scope::Project => env.project.join(".roo").join("mcp.json"),
    }
}

fn instructions_file(env: &Env) -> PathBuf {
    match env.scope {
        Scope::User => env.home.join(".roo").join("rules").join(RULES_FILE),
        Scope::Project => env.project.join("AGENTS.md"),
    }
}

/// `rules/` and the mode-specific `rules-<mode>/` next to it, at this scope.
fn rule_dirs(env: &Env) -> Vec<PathBuf> {
    let base = match env.scope {
        Scope::User => env.home.join(".roo"),
        Scope::Project => env.project.join(".roo"),
    };
    let mut out = vec![base.join("rules")];
    if let Ok(names) = crate::plan_fs::subdirs(&base) {
        out.extend(
            names
                .into_iter()
                .filter(|n| n.starts_with("rules-"))
                .map(|n| base.join(n)),
        );
    }
    out
}

impl Adapter for Roo {
    fn id(&self) -> &'static str {
        ID
    }
    fn name(&self) -> &'static str {
        "Roo Code"
    }
    fn docs(&self) -> &'static str {
        "https://roocodeinc.github.io/Roo-Code/features/mcp/using-mcp-in-roo"
    }
    fn scopes(&self) -> &'static [Scope] {
        &[Scope::User, Scope::Project]
    }
    fn support(&self, _scope: Scope) -> [(Feature, Support); 7] {
        Feature::ALL.map(|f| {
            let s = match f {
                Feature::Mcp | Feature::Instructions => Support::Native,
                Feature::Rules => Support::Folded,
                _ => Support::None,
            };
            (f, s)
        })
    }

    fn locations(&self, env: &Env) -> Vec<(&'static str, PathBuf)> {
        vec![
            ("mcp", mcp_file(env)),
            ("instructions", instructions_file(env)),
        ]
    }

    fn detect(&self, env: &Env) -> bool {
        match env.scope {
            Scope::User => env.home.join(".roo").is_dir() || !hosts_with_roo(env).is_empty(),
            // AGENTS.md is shared with other tools and says nothing about Roo.
            Scope::Project => {
                env.project.join(".roo").is_dir() || env.project.join(".roorules").exists()
            }
        }
    }

    fn capture(&self, env: &Env) -> Result<Captured> {
        let mut cap = Captured::for_env(env);
        let file = mcp_file(env);

        if let Some(text) = cap.read_config(&file, ID)? {
            cap.sources.push(file.clone());
            let doc = parse_value(Some(&text), &file)?;
            for (name, def) in
                server_entries(&mut cap, ID, &file, "mcpServers", doc.get("mcpServers"))
            {
                let mut s = server_from_json(def);
                match def.get("type").and_then(Value::as_str) {
                    Some("streamable-http") => s.transport = Some(Transport::Http),
                    Some("sse") => s.transport = Some(Transport::Sse),
                    Some("stdio") => {}
                    None if s.url.is_some() => {
                        cap.warn(
                            ID,
                            format!(
                                "skipped MCP server '{name}': it has a url but no type, which Roo rejects (it needs streamable-http or sse)"
                            ),
                        );
                        continue;
                    }
                    None => {}
                    Some(other) => {
                        cap.warn(
                            ID,
                            format!(
                                "skipped MCP server '{name}': unknown type '{other}' (Roo's docs list stdio, streamable-http and sse)"
                            ),
                        );
                        continue;
                    }
                }
                s.disabled = def.get("disabled") == Some(&Value::Bool(true));
                import_server(&mut cap, ID, name, s, false);
            }
        }
        if env.scope == Scope::User {
            let hosts = hosts_with_roo(env);
            if hosts.len() > 1 {
                cap.info(
                    ID,
                    format!(
                        "Roo Code is installed in more than one editor ({}); only {}'s MCP settings were read",
                        hosts.join(", "),
                        hosts[0]
                    ),
                );
            }
        }

        let own = instructions_file(env);
        capture_instructions(&mut cap, &own, ID)?;
        for dir in rule_dirs(env) {
            note_unread_rule_files(&mut cap, ID, &dir, Some(&own), &env.home);
        }
        if env.scope == Scope::Project && env.project.join(".roorules").is_file() {
            cap.info(
                ID,
                "a .roorules file was not read: kitbag doesn't carry Roo's own rule files yet",
            );
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
        let file = mcp_file(env);
        let mut servers = Vec::new();
        for (name, s) in servers_for(profile, ID, work, true) {
            let s = render_server(s, RefStyle::EnvColon, env, work);
            let mut def = Map::new();
            if s.transport() == Transport::Stdio {
                def.insert("command".into(), json!(s.command));
                if !s.args.is_empty() {
                    def.insert("args".into(), json!(s.args));
                }
                if !s.env.is_empty() {
                    def.insert("env".into(), json!(s.env));
                }
                if let Some(cwd) = &s.cwd {
                    def.insert("cwd".into(), json!(cwd));
                }
            } else {
                def.insert("url".into(), json!(s.url));
                let kind = if s.transport() == Transport::Sse {
                    "sse"
                } else {
                    "streamable-http"
                };
                def.insert("type".into(), json!(kind));
                if !s.headers.is_empty() {
                    def.insert("headers".into(), json!(s.headers));
                }
                if s.cwd.is_some() {
                    work.warn(
                        ID,
                        format!("MCP server '{name}': 'cwd' only applies to local servers in Roo and was dropped"),
                    );
                }
            }
            if s.disabled {
                def.insert("disabled".into(), json!(true));
            }
            servers.push((name.clone(), Value::Object(def)));
        }
        merge_json_servers(
            work,
            &file,
            &["mcpServers"],
            &servers,
            MANAGED_KEYS,
            opts,
            ID,
        )?;
        if env.scope == Scope::User && !servers.is_empty() {
            let hosts = hosts_with_roo(env);
            if hosts.is_empty() {
                work.warn(
                    ID,
                    format!(
                        "Roo Code's storage was not found in VS Code, VS Code Insiders, VSCodium or Cursor; the servers go to {}. If Roo is in another editor, or you set roo-cline.customStoragePath, copy that file to where Roo reads it",
                        crate::ui::tildify(&file, &env.home)
                    ),
                );
            } else {
                work.info(
                    ID,
                    format!(
                        "Roo Code's global MCP settings are in {}'s extension storage; kitbag can't see a roo-cline.customStoragePath setting",
                        hosts[0]
                    ),
                );
            }
        }
        if env.scope == Scope::Project && !servers.is_empty() {
            work.info(
                ID,
                "kitbag writes no alwaysAllow, so Roo's own tool approval applies to these servers; a project's .roo/mcp.json wins over the user file for a server of the same name",
            );
        }

        // At project scope this is AGENTS.md, with the same block id as Codex,
        // Cursor, Antigravity, opencode, Windsurf and Grok Build.
        write_instructions(work, &instructions_file(env), profile, opts, true, ID)?;
        if profile.settings.contains_key(ID) {
            work.info(
                ID,
                "kitbag doesn't manage Roo Code settings; settings.roo was not applied",
            );
        }
        Ok(())
    }
}
