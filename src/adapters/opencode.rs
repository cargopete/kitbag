//! opencode: MCP servers and instructions.
//!
//! * Config: `~/.config/opencode/opencode.json(c)` (`$XDG_CONFIG_HOME/opencode`
//!   when that is set) at user scope; `opencode.json(c)` or
//!   `.opencode/opencode.json(c)` in a project. Capture reads every one that
//!   exists; apply writes the first that exists, or creates `opencode.json`.
//!   The files are JSONC, so comments and trailing commas are kept.
//! * MCP: opencode 2 keeps servers under `mcp.servers`; opencode 1 kept them
//!   directly under `mcp`, and opencode 2 still reads that. Local servers are
//!   `type: local` with `command` (an array: the program and its arguments),
//!   `environment` and `cwd`; remote servers are `type: remote` with `url`
//!   (Streamable HTTP) and `headers`. opencode 2 turns a server off with
//!   `disabled: true`, opencode 1 with `enabled: false`. Apply writes the
//!   layout the file already uses (`mcp.servers` for a new file), so a file
//!   opencode 1 still reads stays readable. opencode expands `{env:VAR}` in
//!   config strings, so references are written that way and secrets never
//!   touch disk. SSE servers are skipped.
//! * Instructions: `~/.config/opencode/AGENTS.md` (user) and `AGENTS.md`
//!   (project, shared with Codex, Cursor and Antigravity). opencode reads
//!   `~/.claude/CLAUDE.md` / a project's `CLAUDE.md` only while there is no
//!   `AGENTS.md`; those belong to Claude Code and are not captured here.
//! * Rules, commands, skills, agents and plugins are not supported yet.
//!
//! Docs: <https://opencode.ai/docs/config/>, <https://opencode.ai/docs/mcp-servers/>,
//! <https://opencode.ai/docs/rules/>, <https://opencode.ai/docs/migrate-v1/>.

use std::path::{Path, PathBuf};

use anyhow::Result;
use serde_json::{Map, Value, json};

use super::common::{
    capture_instructions, import_server, merge_json_servers, render_server, server_entries,
    servers_for, str_of, string_list, string_map, write_instructions,
};
use super::{Adapter, ApplyOptions, Captured, Env, Feature, Scope, Support, Work};
use crate::edit::json::parse_value;
use crate::model::{McpServer, Profile, Transport};
use crate::secrets::{RefStyle, normalize_brace_refs};
use crate::ui::tildify;

const ID: &str = "opencode";
/// Server keys kitbag writes; anything else (`timeout`, `oauth`, `codemode`,
/// `protocol` ...) is preserved. Both on/off keys are listed so turning a
/// server back on removes whichever one turned it off.
const MANAGED_KEYS: &[&str] = &[
    "type",
    "command",
    "environment",
    "cwd",
    "url",
    "headers",
    "disabled",
    "enabled",
];

pub struct OpenCode;

struct Paths {
    /// Config files opencode reads at this scope, in the order kitbag picks
    /// one to write.
    configs: Vec<PathBuf>,
    instructions: PathBuf,
    /// What opencode reads instead of `instructions` while that doesn't exist.
    fallback: PathBuf,
}

fn config_dir(env: &Env) -> PathBuf {
    env.location_var("XDG_CONFIG_HOME")
        .unwrap_or_else(|| env.home.join(".config"))
        .join("opencode")
}

fn paths(env: &Env) -> Paths {
    match env.scope {
        Scope::User => {
            let d = config_dir(env);
            Paths {
                configs: vec![d.join("opencode.json"), d.join("opencode.jsonc")],
                instructions: d.join("AGENTS.md"),
                fallback: env.home.join(".claude").join("CLAUDE.md"),
            }
        }
        Scope::Project => {
            let p = &env.project;
            let o = p.join(".opencode");
            Paths {
                configs: vec![
                    p.join("opencode.json"),
                    p.join("opencode.jsonc"),
                    o.join("opencode.json"),
                    o.join("opencode.jsonc"),
                ],
                instructions: p.join("AGENTS.md"),
                fallback: p.join("CLAUDE.md"),
            }
        }
    }
}

/// Where servers live in a config file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Layout {
    /// `mcp.servers.<name>`, with `disabled` (opencode 2).
    Servers,
    /// `mcp.<name>`, with `enabled` (opencode 1, still read by opencode 2).
    Legacy,
}

/// An opencode 1 server entry directly under `mcp`: an object with a `type`.
fn is_legacy_entry(name: &str, def: &Value) -> bool {
    name != "servers" && def.get("type").is_some()
}

/// The layout a config file already uses; `mcp.servers` unless it only has
/// opencode 1 entries.
fn layout(doc: &Value) -> Layout {
    let Some(Value::Object(mcp)) = doc.get("mcp") else {
        return Layout::Servers;
    };
    if mcp.get("servers").is_some_and(Value::is_object) {
        return Layout::Servers;
    }
    if mcp.iter().any(|(k, v)| is_legacy_entry(k, v)) {
        Layout::Legacy
    } else {
        Layout::Servers
    }
}

fn import(cap: &mut Captured, file: &Path, name: &str, def: &Map<String, Value>) {
    let mut s = McpServer::default();
    match def.get("type").and_then(Value::as_str) {
        Some("local") => {
            let cmd = match def.get("command") {
                Some(Value::String(c)) => vec![c.clone()],
                v => string_list(v),
            };
            if let Some((first, rest)) = cmd.split_first() {
                s.command = Some(first.clone());
                s.args = rest.to_vec();
            }
            s.env = string_map(def.get("environment"));
            s.cwd = str_of(def.get("cwd"));
        }
        Some("remote") => {
            s.url = str_of(def.get("url"));
            s.headers = string_map(def.get("headers"));
        }
        other => {
            let found = other.map_or("no type".to_string(), |t| format!("type '{t}'"));
            cap.warn(
                ID,
                format!(
                    "skipped MCP server '{name}' in {}: expected type 'local' or 'remote', found {found}",
                    file.display()
                ),
            );
            return;
        }
    }
    s.disabled = def.get("disabled") == Some(&Value::Bool(true))
        || def.get("enabled") == Some(&Value::Bool(false));
    if Value::Object(def.clone()).to_string().contains("{file:") {
        cap.warn(
            ID,
            format!("MCP server '{name}' uses opencode's {{file:...}} references; other tools won't expand them"),
        );
    }
    let s = s.map_strings(normalize_brace_refs);
    import_server(cap, ID, name, s, false);
}

impl Adapter for OpenCode {
    fn id(&self) -> &'static str {
        ID
    }
    fn name(&self) -> &'static str {
        "opencode"
    }
    fn docs(&self) -> &'static str {
        "https://opencode.ai/docs/mcp-servers/"
    }
    fn scopes(&self) -> &'static [Scope] {
        &[Scope::User, Scope::Project]
    }
    fn support(&self, _scope: Scope) -> [(Feature, Support); 6] {
        Feature::ALL.map(|f| {
            (
                f,
                if matches!(f, Feature::Mcp | Feature::Instructions) {
                    Support::Native
                } else {
                    Support::None
                },
            )
        })
    }

    fn locations(&self, env: &Env) -> Vec<(&'static str, PathBuf)> {
        let p = paths(env);
        let config = p
            .configs
            .iter()
            .find(|c| c.exists())
            .unwrap_or(&p.configs[0])
            .clone();
        vec![("mcp", config), ("instructions", p.instructions)]
    }

    fn detect(&self, env: &Env) -> bool {
        match env.scope {
            Scope::User => config_dir(env).exists(),
            Scope::Project => {
                env.project.join(".opencode").exists()
                    || paths(env).configs.iter().any(|c| c.exists())
            }
        }
    }

    fn capture(&self, env: &Env) -> Result<Captured> {
        let mut cap = Captured::for_env(env);
        let p = paths(env);
        for file in &p.configs {
            let Some(text) = cap.read_config(file, ID)? else {
                continue;
            };
            cap.sources.push(file.clone());
            let doc = parse_value(Some(&text), file)?;
            match doc.get("mcp") {
                None | Some(Value::Null) => {}
                Some(Value::Object(mcp)) => {
                    for (name, def) in
                        server_entries(&mut cap, ID, file, "mcp.servers", mcp.get("servers"))
                    {
                        import(&mut cap, file, name, def);
                    }
                    for (name, def) in mcp {
                        if let (true, Value::Object(d)) = (is_legacy_entry(name, def), def) {
                            import(&mut cap, file, name, d);
                        }
                    }
                }
                Some(_) => cap.warn(
                    ID,
                    format!("ignored 'mcp' in {}: expected an object", file.display()),
                ),
            }
            if doc
                .get("instructions")
                .and_then(Value::as_array)
                .is_some_and(|a| !a.is_empty())
            {
                cap.info(
                    ID,
                    format!(
                        "{} lists more instruction files under 'instructions'; only {} was captured",
                        file.display(),
                        p.instructions.display()
                    ),
                );
            }
        }
        capture_instructions(&mut cap, &p.instructions, ID)?;
        // Rules another tool folded into a shared AGENTS.md (Codex) come back
        // out of capture_instructions; opencode doesn't carry rules yet.
        if !cap.profile.rules.is_empty() {
            let names: Vec<String> = cap.profile.rules.drain(..).map(|r| r.name).collect();
            cap.info(
                ID,
                format!(
                    "rules folded into opencode's AGENTS.md by another tool were not captured ({}); capture that tool for them",
                    names.join(", ")
                ),
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
        let p = paths(env);
        let mut existing = Vec::new();
        for c in &p.configs {
            if work.fs.read(c)?.is_some() {
                existing.push(c.clone());
            }
        }
        let file = existing.first().unwrap_or(&p.configs[0]).clone();
        let layout = layout(&parse_value(work.fs.read(&file)?.as_deref(), &file)?);

        let mut servers = Vec::new();
        let mut sse = Vec::new();
        for (name, s) in servers_for(profile, ID, work, true) {
            let s = render_server(s, RefStyle::EnvBrace, env, work);
            let mut def = Map::new();
            match s.transport() {
                Transport::Stdio => {
                    def.insert("type".into(), json!("local"));
                    let mut cmd = vec![s.command.clone().unwrap_or_default()];
                    cmd.extend(s.args.iter().cloned());
                    def.insert("command".into(), json!(cmd));
                    if !s.env.is_empty() {
                        def.insert("environment".into(), json!(s.env));
                    }
                    if let Some(cwd) = &s.cwd {
                        def.insert("cwd".into(), json!(cwd));
                    }
                }
                Transport::Http => {
                    def.insert("type".into(), json!("remote"));
                    def.insert("url".into(), json!(s.url));
                    if !s.headers.is_empty() {
                        def.insert("headers".into(), json!(s.headers));
                    }
                }
                Transport::Sse => {
                    sse.push(name.as_str());
                    continue;
                }
            }
            if s.disabled {
                match layout {
                    Layout::Servers => def.insert("disabled".into(), json!(true)),
                    Layout::Legacy => def.insert("enabled".into(), json!(false)),
                };
            }
            servers.push((name.clone(), Value::Object(def)));
        }
        if !sse.is_empty() {
            work.warn(
                ID,
                format!(
                    "MCP server(s) {} skipped: opencode connects to remote servers over Streamable HTTP, not SSE",
                    sse.join(", ")
                ),
            );
        }
        if !servers.is_empty() && existing.len() > 1 {
            let others: Vec<String> = existing[1..]
                .iter()
                .map(|c| tildify(c, &work.home))
                .collect();
            work.info(
                ID,
                format!(
                    "opencode also reads {}; MCP servers were written to {} only",
                    others.join(", "),
                    tildify(&file, &work.home)
                ),
            );
        }
        let key_path: &[&str] = match layout {
            Layout::Servers => &["mcp", "servers"],
            Layout::Legacy => &["mcp"],
        };
        merge_json_servers(work, &file, key_path, &servers, MANAGED_KEYS, opts, ID)?;

        let had_instructions = work.fs.read(&p.instructions)?.is_some();
        write_instructions(work, &p.instructions, profile, opts, false, ID)?;
        if !had_instructions
            && work.fs.read(&p.instructions)?.is_some()
            && work.fs.read(&p.fallback)?.is_some()
        {
            work.info(
                ID,
                format!(
                    "opencode reads {} only while {} doesn't exist; with the new file it reads that instead",
                    tildify(&p.fallback, &work.home),
                    tildify(&p.instructions, &work.home)
                ),
            );
        }
        if profile.settings.contains_key(ID) {
            work.info(
                ID,
                "kitbag doesn't manage opencode settings; settings.opencode was not applied",
            );
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn layout_follows_the_file() {
        assert_eq!(layout(&json!({})), Layout::Servers);
        assert_eq!(
            layout(&json!({"mcp": {"timeout": {"catalog": 1000}}})),
            Layout::Servers
        );
        assert_eq!(
            layout(&json!({"mcp": {"a": {"type": "local", "command": ["x"]}}})),
            Layout::Legacy
        );
        assert_eq!(
            layout(&json!({"mcp": {"servers": {}, "a": {"type": "local"}}})),
            Layout::Servers
        );
    }
}
