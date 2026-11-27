//! Amp (Sourcegraph's coding agent): MCP servers and instructions.
//!
//! * MCP: `amp.mcpServers` (a flat, dotted key) in Amp's settings file: user
//!   settings `~/.config/amp/settings.json` or `settings.jsonc` (the same path
//!   under `%USERPROFILE%` on Windows), and the workspace's `.amp/settings.json`
//!   or `settings.jsonc`. Local servers take `command`, `args` and `env`;
//!   remote servers take `url` and `headers`; Amp picks the transport from
//!   which of the two is there. Amp expands `${VAR}` in these values, so
//!   references are written as they are and no secret touches disk. A
//!   workspace server has to be approved by the user before it runs
//!   (`amp mcp approve <name>`); kitbag never approves one, and writes no
//!   `amp.mcpTrustedServers` or `amp.mcpPermissions`. When a server is defined
//!   in both, the workspace one wins over the user one (then skills' own).
//! * `${amp:id-token}` is an Amp-only reference that lets an orb send its Amp
//!   ID token to a server; a server that uses it is not captured and not written.
//! * Instructions: `~/.config/amp/AGENTS.md` (Amp also reads
//!   `~/.config/AGENTS.md`, which kitbag leaves alone). In a project, Amp reads
//!   `AGENTS.md`, or `AGENT.md` / `CLAUDE.md` when the directory has no
//!   `AGENTS.md`; kitbag writes to the one Amp reads. `AGENTS.md` is shared with
//!   Codex, Cursor, Antigravity, opencode, Grok Build, Windsurf and Hermes.
//! * Not carried: MCP servers defined on ampcode.com (`amp mcp remote`), servers
//!   bundled in a skill's `mcp.json`, `includeTools`, skills, permissions and the
//!   other `amp.*` settings.
//!
//! Docs: <https://ampcode.com/docs/customize/mcp>,
//! <https://ampcode.com/docs/cli/settings>,
//! <https://ampcode.com/docs/customize/agents-md>.

use std::path::{Path, PathBuf};

use anyhow::Result;
use serde_json::{Map, Value, json};

use super::common::{
    capture_instructions, import_server, merge_json_servers, render_server, server_entries,
    server_from_json, servers_for, write_instructions,
};
use super::{Adapter, ApplyOptions, Captured, Env, Feature, Scope, Support, Work};
use crate::edit::json::parse_value;
use crate::model::{McpServer, Profile, Transport};
use crate::secrets::RefStyle;

const ID: &str = "amp";
/// The settings key Amp keeps MCP servers under (one key, dots and all).
const SERVERS_KEY: &str = "amp.mcpServers";
/// Server keys kitbag writes; anything else (`includeTools` ...) is preserved.
const MANAGED_KEYS: &[&str] = &["command", "args", "env", "url", "headers"];
/// Server keys kitbag reads; any other key on an entry is not captured.
const CAPTURED_KEYS: &[&str] = &["command", "args", "env", "url", "headers"];
/// Where Amp's docs put the Amp ID token reference.
const ID_TOKEN_REF: &str = "${amp:";
/// Project instruction files in the order Amp falls back through them: it reads
/// the first one the directory has.
const PROJECT_FILES: &[&str] = &["AGENTS.md", "AGENT.md", "CLAUDE.md"];

pub struct Amp;

/// Amp's user config directory: `~/.config/amp`, on every OS (Windows:
/// `%USERPROFILE%\.config\amp`). The docs name no environment variable for it.
pub fn config_dir(env: &Env) -> PathBuf {
    env.home.join(".config").join("amp")
}

/// The directory holding `settings.json[c]` in this scope.
fn settings_dir(env: &Env) -> PathBuf {
    match env.scope {
        Scope::User => config_dir(env),
        Scope::Project => env.project.join(".amp"),
    }
}

/// Both spellings of a settings file in `dir`, in the order Amp's docs give them.
fn settings_candidates(dir: &Path) -> [PathBuf; 2] {
    [dir.join("settings.json"), dir.join("settings.jsonc")]
}

/// The settings file of a directory: `settings.json`, else an existing
/// `settings.jsonc`, else `settings.json` to create.
fn settings_in(dir: &Path) -> PathBuf {
    let [json, jsonc] = settings_candidates(dir);
    if !json.is_file() && jsonc.is_file() {
        jsonc
    } else {
        json
    }
}

fn settings_file(env: &Env) -> PathBuf {
    settings_in(&settings_dir(env))
}

/// Say so when both spellings exist: the docs don't say which one wins.
fn note_both_spellings(dir: &Path, report: &mut dyn FnMut(String)) {
    let [json, jsonc] = settings_candidates(dir);
    if json.is_file() && jsonc.is_file() {
        report(format!(
            "both {} and {} exist and Amp's docs don't say which one wins; kitbag uses settings.json",
            json.display(),
            jsonc.display()
        ));
    }
}

fn project_file(env: &Env, name: &str) -> PathBuf {
    env.project.join(name)
}

/// The instructions file Amp reads in this scope; in a project, the first of
/// `AGENTS.md`, `AGENT.md`, `CLAUDE.md` for which `exists` is true, else `AGENTS.md`.
fn instructions_file(env: &Env, exists: impl Fn(&Path) -> bool) -> PathBuf {
    match env.scope {
        Scope::User => config_dir(env).join("AGENTS.md"),
        Scope::Project => {
            let name = PROJECT_FILES
                .iter()
                .find(|n| exists(&project_file(env, n)))
                .unwrap_or(&PROJECT_FILES[0]);
            project_file(env, name)
        }
    }
}

/// Does the server use Amp's ID-token reference anywhere?
fn uses_id_token(s: &McpServer) -> bool {
    let mut hit = false;
    let _ = s.map_strings(|v| {
        hit |= v.contains(ID_TOKEN_REF);
        v.to_string()
    });
    hit
}

/// Names of the servers under `amp.mcpServers` in the settings file of `dir`,
/// read leniently: a file that isn't there or doesn't parse has none.
fn server_names(dir: &Path) -> Vec<String> {
    let file = settings_in(dir);
    let Ok(Some(text)) = crate::plan_fs::read_text_opt(&file) else {
        return Vec::new();
    };
    match parse_value(Some(&text), &file) {
        Ok(doc) => doc
            .get(SERVERS_KEY)
            .and_then(Value::as_object)
            .map(|m| m.keys().cloned().collect())
            .unwrap_or_default(),
        Err(_) => Vec::new(),
    }
}

impl Adapter for Amp {
    fn id(&self) -> &'static str {
        ID
    }
    fn name(&self) -> &'static str {
        "Amp"
    }
    fn docs(&self) -> &'static str {
        "https://ampcode.com/docs/customize/mcp"
    }
    fn scopes(&self) -> &'static [Scope] {
        &[Scope::User, Scope::Project]
    }
    fn support(&self, _scope: Scope) -> [(Feature, Support); 7] {
        Feature::ALL.map(|f| {
            let s = match f {
                Feature::Mcp | Feature::Instructions => Support::Native,
                _ => Support::None,
            };
            (f, s)
        })
    }

    fn locations(&self, env: &Env) -> Vec<(&'static str, PathBuf)> {
        vec![
            ("mcp", settings_file(env)),
            ("instructions", instructions_file(env, |p| p.is_file())),
        ]
    }

    fn detect(&self, env: &Env) -> bool {
        // AGENTS.md is shared with other tools and says nothing about Amp.
        settings_dir(env).is_dir()
    }

    fn capture(&self, env: &Env) -> Result<Captured> {
        let mut cap = Captured::for_env(env);
        let dir = settings_dir(env);
        let file = settings_file(env);
        let mut both = Vec::new();
        note_both_spellings(&dir, &mut |m| both.push(m));
        for m in both {
            cap.warn(ID, m);
        }

        if let Some(text) = cap.read_config(&file, ID)? {
            cap.sources.push(file.clone());
            let doc = parse_value(Some(&text), &file)?;
            for (name, def) in
                server_entries(&mut cap, ID, &file, SERVERS_KEY, doc.get(SERVERS_KEY))
            {
                let s = server_from_json(def);
                if uses_id_token(&s) {
                    cap.warn(
                        ID,
                        format!(
                            "skipped MCP server '{name}': it uses ${{amp:id-token}}, which only works in an Amp orb and is never carried"
                        ),
                    );
                    continue;
                }
                let other: Vec<&str> = def
                    .keys()
                    .map(String::as_str)
                    .filter(|k| !CAPTURED_KEYS.contains(k))
                    .collect();
                if !other.is_empty() {
                    cap.info(
                        ID,
                        format!(
                            "MCP server '{name}': {} not captured (kitbag carries command, args, env, url and headers)",
                            other.join(", ")
                        ),
                    );
                }
                import_server(&mut cap, ID, name, s, false);
            }
        }
        if env.scope == Scope::User || file.is_file() {
            cap.info(
                ID,
                "MCP servers defined on ampcode.com (amp mcp remote) or bundled in a skill's mcp.json are not captured",
            );
        }

        let instructions = instructions_file(env, |p| p.is_file());
        capture_instructions(&mut cap, &instructions, ID)?;
        // Rules folded into a shared file by another target (Codex's AGENTS.md)
        // come back out of capture_instructions.
        if !cap.profile.rules.is_empty() {
            let names: Vec<String> = cap.profile.rules.drain(..).map(|r| r.name).collect();
            cap.info(
                ID,
                format!(
                    "rules folded into Amp's instructions file by another tool were not captured ({}); capture that tool for them",
                    names.join(", ")
                ),
            );
        }
        match env.scope {
            Scope::User => {
                let shared = env.home.join(".config").join("AGENTS.md");
                if shared.is_file() {
                    cap.info(
                        ID,
                        format!(
                            "{} is also read by Amp and was not captured; kitbag reads only {}",
                            shared.display(),
                            instructions.display()
                        ),
                    );
                }
            }
            Scope::Project => {
                // Amp reads one file per directory; say which of the others it ignores.
                let ignored: Vec<&str> = PROJECT_FILES
                    .iter()
                    .copied()
                    .filter(|n| {
                        let p = project_file(env, n);
                        p != instructions && p.is_file()
                    })
                    .collect();
                if !ignored.is_empty() {
                    cap.info(
                        ID,
                        format!(
                            "{} not read: Amp uses AGENTS.md, else AGENT.md, else CLAUDE.md, and found {}",
                            ignored.join(", "),
                            instructions
                                .strip_prefix(&env.project)
                                .unwrap_or(&instructions)
                                .display()
                        ),
                    );
                }
            }
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
        let dir = settings_dir(env);
        let file = settings_file(env);
        let mut both = Vec::new();
        note_both_spellings(&dir, &mut |m| both.push(m));
        for m in both {
            work.warn(ID, m);
        }

        let mut servers = Vec::new();
        // Amp documents no way to switch a server off, so a disabled one isn't written.
        for (name, s) in servers_for(profile, ID, work, false) {
            if uses_id_token(s) {
                work.warn(
                    ID,
                    format!(
                        "MCP server '{name}' skipped: it uses ${{amp:id-token}}, which kitbag doesn't write"
                    ),
                );
                continue;
            }
            if s.cwd.is_some() {
                work.warn(
                    ID,
                    format!(
                        "MCP server '{name}': 'cwd' is not supported by Amp (servers start in the thread's working directory) and was dropped"
                    ),
                );
            }
            if s.transport() == Transport::Sse {
                work.warn(
                    ID,
                    format!(
                        "MCP server '{name}': Amp documents no transport setting, so it is written as a plain url (its own example is an /sse URL) and Amp decides how to connect"
                    ),
                );
            }
            // Amp expands ${VAR} itself; `${VAR:-default}` isn't documented, so
            // defaults are resolved now.
            let s = render_server(s, RefStyle::DollarPlain, env, work);
            let mut def = Map::new();
            if s.transport() == Transport::Stdio {
                def.insert("command".into(), json!(s.command));
                if !s.args.is_empty() {
                    def.insert("args".into(), json!(s.args));
                }
                if !s.env.is_empty() {
                    def.insert("env".into(), json!(s.env));
                }
            } else {
                def.insert("url".into(), json!(s.url));
                if !s.headers.is_empty() {
                    def.insert("headers".into(), json!(s.headers));
                }
            }
            servers.push((name.clone(), Value::Object(def)));
        }
        merge_json_servers(
            work,
            &file,
            &[SERVERS_KEY],
            &servers,
            MANAGED_KEYS,
            opts,
            ID,
        )?;

        if !servers.is_empty() {
            let names: Vec<&str> = servers.iter().map(|(n, _)| n.as_str()).collect();
            match env.scope {
                Scope::Project => {
                    work.info(
                        ID,
                        "Amp asks you to approve the MCP servers of a workspace's .amp/settings.json before it runs them (`amp mcp approve <name>`; `amp mcp doctor` lists those awaiting approval); kitbag leaves that to you and writes no approval or trust setting",
                    );
                    let user = server_names(&config_dir(env));
                    let both: Vec<&str> = names
                        .iter()
                        .copied()
                        .filter(|n| user.iter().any(|u| u == n))
                        .collect();
                    if !both.is_empty() {
                        work.info(
                            ID,
                            format!(
                                "in this workspace, the entry for {} overrides the server of the same name in your user settings (Amp: workspace over user)",
                                both.join(", ")
                            ),
                        );
                    }
                }
                Scope::User => {
                    let ws = server_names(&env.project.join(".amp"));
                    let both: Vec<&str> = names
                        .iter()
                        .copied()
                        .filter(|n| ws.iter().any(|w| w == n))
                        .collect();
                    if !both.is_empty() {
                        work.info(
                            ID,
                            format!(
                                "{} is also defined in {}, which overrides these user settings in that workspace (Amp: workspace over user)",
                                both.join(", "),
                                settings_in(&env.project.join(".amp")).display()
                            ),
                        );
                    }
                }
            }
        }

        // Amp reads one project file, so write where it will look: the first of
        // AGENTS.md, AGENT.md, CLAUDE.md that exists, counting what this plan
        // creates (Claude Code writes CLAUDE.md, Codex AGENTS.md).
        let exists = |p: &Path| p.is_file() || work.fs.read(p).ok().flatten().is_some();
        let path = instructions_file(env, exists);
        if env.scope == Scope::Project
            && path != project_file(env, PROJECT_FILES[0])
            && profile.instructions.is_some()
        {
            work.info(
                ID,
                format!(
                    "Amp reads only {} here (it uses AGENT.md or CLAUDE.md when there is no AGENTS.md), so the instructions go there; creating an AGENTS.md would hide it from Amp",
                    path.strip_prefix(&env.project).unwrap_or(&path).display()
                ),
            );
        }
        // At project scope this is AGENTS.md or one of its fallbacks, with the
        // same block id as the other tools sharing the file: applying several
        // plans the same block more than once, which leaves one copy.
        write_instructions(work, &path, profile, opts, false, ID)?;
        if env.scope == Scope::User
            && profile.instructions.is_some()
            && env.home.join(".config").join("AGENTS.md").is_file()
        {
            work.info(
                ID,
                "Amp also reads ~/.config/AGENTS.md, which kitbag leaves alone; the same text there would reach Amp twice",
            );
        }
        if profile.settings.contains_key(ID) {
            work.info(
                ID,
                "kitbag doesn't manage Amp settings; settings.amp was not applied",
            );
        }
        Ok(())
    }
}
