//! Google Antigravity (app, IDE and CLI): MCP servers and instructions.
//!
//! * MCP: `~/.gemini/config/mcp_config.json` (user) or
//!   `.agents/mcp_config.json` (workspace), under `mcpServers`. Local servers
//!   take `command`, `args`, `env` and `cwd`; remote servers take `serverUrl`
//!   (Streamable HTTP only; `url` is also read) and `headers`; `disabled` turns
//!   a server off. The docs describe no env-reference syntax, so references
//!   are resolved at apply time, or kept as placeholders with `--secrets never`,
//!   like Claude Desktop.
//! * Instructions: Antigravity combines every rules file it finds. At user
//!   scope it reads `~/.gemini/{AGENTS,GEMINI}.md` and
//!   `~/.gemini/config/{AGENTS,GEMINI}.md`; kitbag writes `~/.gemini/GEMINI.md`,
//!   the file Gemini CLI uses too, so applying to both writes one managed
//!   block that both read, not two copies. In a workspace it reads
//!   `{AGENTS,GEMINI}.md` and `.agents/{AGENTS,GEMINI}.md`; kitbag writes
//!   `AGENTS.md`, shared with Codex and Cursor.
//! * Rules (`.agents/rules/`, `~/.gemini/config/rules/`), skills and workflows
//!   are not supported yet.
//!
//! Docs: <https://antigravity.google/docs/mcp>, <https://antigravity.google/docs/rules>.

use std::path::PathBuf;

use anyhow::Result;
use serde_json::{Map, Value, json};

use super::common::{
    Resolver, capture_instructions, import_server, merge_json_servers, server_entries,
    server_from_json, servers_for, str_of, write_instructions,
};
use super::{Adapter, ApplyOptions, Captured, Env, Feature, Scope, Support, Work};
use crate::edit::json::parse_value;
use crate::model::{Profile, Transport};

const ID: &str = "antigravity";
const TOOL: &str = "Antigravity";
/// Server keys kitbag writes; anything else (`disabledTools`, `timeoutSeconds`,
/// `oauth` ...) is preserved. `url` is listed so an old `url` entry gives way
/// to `serverUrl` instead of sitting next to it.
const MANAGED_KEYS: &[&str] = &[
    "command",
    "args",
    "env",
    "cwd",
    "serverUrl",
    "url",
    "headers",
    "disabled",
];

pub struct Antigravity;

struct Paths {
    mcp: PathBuf,
    /// The file kitbag writes instructions into.
    instructions: PathBuf,
    /// Every instructions file Antigravity reads at this scope, for capture.
    read: Vec<PathBuf>,
}

fn paths(env: &Env) -> Paths {
    match env.scope {
        Scope::User => {
            let g = env.home.join(".gemini");
            let c = g.join("config");
            Paths {
                mcp: c.join("mcp_config.json"),
                instructions: g.join("GEMINI.md"),
                read: vec![
                    g.join("GEMINI.md"),
                    g.join("AGENTS.md"),
                    c.join("GEMINI.md"),
                    c.join("AGENTS.md"),
                ],
            }
        }
        Scope::Project => {
            let p = &env.project;
            let a = p.join(".agents");
            Paths {
                mcp: a.join("mcp_config.json"),
                instructions: p.join("AGENTS.md"),
                read: vec![
                    p.join("AGENTS.md"),
                    p.join("GEMINI.md"),
                    a.join("AGENTS.md"),
                    a.join("GEMINI.md"),
                ],
            }
        }
    }
}

impl Adapter for Antigravity {
    fn id(&self) -> &'static str {
        ID
    }
    fn name(&self) -> &'static str {
        "Google Antigravity"
    }
    fn docs(&self) -> &'static str {
        "https://antigravity.google/docs/mcp"
    }
    fn scopes(&self) -> &'static [Scope] {
        &[Scope::User, Scope::Project]
    }
    fn support(&self, _scope: Scope) -> [(Feature, Support); 7] {
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
        vec![("mcp", p.mcp), ("instructions", p.instructions)]
    }

    fn detect(&self, env: &Env) -> bool {
        // `~/.gemini` alone belongs to Gemini CLI, and `.agents/` to Codex skills.
        match env.scope {
            Scope::User => {
                let g = env.home.join(".gemini");
                g.join("config").exists() || g.join("antigravity-cli").exists()
            }
            Scope::Project => {
                let a = env.project.join(".agents");
                a.join("mcp_config.json").exists() || a.join("rules").exists()
            }
        }
    }

    fn capture(&self, env: &Env) -> Result<Captured> {
        let mut cap = Captured::for_env(env);
        let p = paths(env);
        if let Some(text) = cap.read_config(&p.mcp, ID)? {
            cap.sources.push(p.mcp.clone());
            let doc = parse_value(Some(&text), &p.mcp)?;
            for (name, def) in
                server_entries(&mut cap, ID, &p.mcp, "mcpServers", doc.get("mcpServers"))
            {
                let mut s = server_from_json(def);
                if let Some(url) = str_of(def.get("serverUrl")) {
                    s.url = Some(url);
                }
                s.disabled = def.get("disabled") == Some(&Value::Bool(true));
                import_server(&mut cap, ID, name, s, false);
            }
        }
        for path in &p.read {
            capture_instructions(&mut cap, path, ID)?;
        }
        // Rules folded into a shared file by another target (Gemini CLI's
        // GEMINI.md, Codex's AGENTS.md) come back out of capture_instructions.
        if !cap.profile.rules.is_empty() {
            let names: Vec<String> = cap.profile.rules.drain(..).map(|r| r.name).collect();
            cap.info(
                ID,
                format!(
                    "rules folded into Antigravity's instructions files by another tool were not captured ({}); capture that tool for them",
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
        let mut servers = Vec::new();
        let mut sse = Vec::new();
        for (name, s) in servers_for(profile, ID, work, true) {
            let mut res = Resolver::new(env, opts);
            let mut def = Map::new();
            match s.transport() {
                Transport::Stdio => {
                    let cmd = s.command.as_deref().unwrap_or_default();
                    def.insert("command".into(), json!(res.value(cmd, "command", work)));
                    if !s.args.is_empty() {
                        let args: Vec<String> = s
                            .args
                            .iter()
                            .enumerate()
                            .map(|(i, a)| res.value(a, &format!("args[{i}]"), work))
                            .collect();
                        def.insert("args".into(), json!(args));
                    }
                    if !s.env.is_empty() {
                        let e: Map<String, Value> = s
                            .env
                            .iter()
                            .map(|(k, v)| {
                                (k.clone(), json!(res.value(v, &format!("env.{k}"), work)))
                            })
                            .collect();
                        def.insert("env".into(), Value::Object(e));
                    }
                    if let Some(cwd) = &s.cwd {
                        def.insert("cwd".into(), json!(res.value(cwd, "cwd", work)));
                    }
                }
                Transport::Http => {
                    let url = s.url.as_deref().unwrap_or_default();
                    def.insert("serverUrl".into(), json!(res.value(url, "url", work)));
                    if !s.headers.is_empty() {
                        let h: Map<String, Value> = s
                            .headers
                            .iter()
                            .map(|(k, v)| {
                                (
                                    k.clone(),
                                    json!(res.value(v, &format!("headers.{k}"), work)),
                                )
                            })
                            .collect();
                        def.insert("headers".into(), Value::Object(h));
                    }
                }
                Transport::Sse => {
                    sse.push(name.as_str());
                    continue;
                }
            }
            if s.disabled {
                def.insert("disabled".into(), json!(true));
            }
            res.finish(work, ID, TOOL, name, "mcp_config.json");
            servers.push((name.clone(), Value::Object(def)));
        }
        if !sse.is_empty() {
            work.warn(
                ID,
                format!(
                    "MCP server(s) {} skipped: Antigravity connects to remote servers over Streamable HTTP only, not SSE",
                    sse.join(", ")
                ),
            );
        }
        merge_json_servers(
            work,
            &p.mcp,
            &["mcpServers"],
            &servers,
            MANAGED_KEYS,
            opts,
            ID,
        )?;
        // Same file and block id as Gemini CLI at user scope: applying both
        // plans the same block twice, which leaves one copy.
        write_instructions(work, &p.instructions, profile, opts, false, ID)?;
        if profile.settings.contains_key(ID) {
            work.info(
                ID,
                "kitbag doesn't manage Antigravity settings; settings.antigravity was not applied",
            );
        }
        Ok(())
    }
}
