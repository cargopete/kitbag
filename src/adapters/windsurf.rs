//! Windsurf (the editor Codeium made, now also shipped as Devin Desktop): MCP
//! servers and instructions.
//!
//! * MCP: `~/.codeium/windsurf/mcp_config.json`, under `mcpServers`. Local
//!   servers take `command`, `args` and `env`; remote servers take `serverUrl`
//!   (`url` is also read) and `headers`. Windsurf interpolates `${env:VAR}` in
//!   `command`, `args`, `env`, `serverUrl`, `url` and `headers`, so references
//!   are rewritten into that syntax and secrets never touch disk. Its docs
//!   describe no project-level MCP file, so there is none at project scope.
//! * Instructions: `~/.codeium/windsurf/memories/global_rules.md` at user scope
//!   (Windsurf's always-on global rules). In a project, `AGENTS.md` at the root,
//!   shared with Codex, Cursor, Antigravity, opencode and Grok Build; capture
//!   also reads the legacy `.windsurfrules`.
//! * Rules (`.windsurf/rules/`), workflows (`global_workflows/`,
//!   `.windsurf/workflows/`), skills and agents are not supported yet.
//!
//! Docs: <https://docs.devin.ai/windsurf/plugins/cascade/mcp>,
//! <https://docs.devin.ai/desktop/cascade/memories>,
//! <https://docs.devin.ai/desktop/cascade/agents-md>.

use std::path::PathBuf;

use anyhow::Result;
use serde_json::{Map, Value, json};

use super::common::{
    add_instructions, capture_instructions, import_server, merge_json_servers, read_text_item,
    render_server, server_entries, server_from_json, servers_for, str_of, write_instructions,
};
use super::{Adapter, ApplyOptions, Captured, Env, Feature, Scope, Support, Work};
use crate::edit::json::parse_value;
use crate::model::{Profile, Transport};
use crate::secrets::RefStyle;

const ID: &str = "windsurf";
/// Server keys kitbag writes; anything else (`disabled`, `disabledTools` ...)
/// is preserved. `url` is listed so an old `url` entry gives way to
/// `serverUrl` instead of sitting next to it.
const MANAGED_KEYS: &[&str] = &["command", "args", "env", "serverUrl", "url", "headers"];
/// Windsurf's documentation has limited `global_rules.md` to this many characters.
const GLOBAL_RULES_MAX_CHARS: usize = 6_000;

pub struct Windsurf;

struct Paths {
    /// `None` at project scope: Windsurf documents no project MCP file.
    mcp: Option<PathBuf>,
    instructions: PathBuf,
    /// A legacy instructions file Windsurf still reads, for capture only.
    legacy: Option<PathBuf>,
}

fn paths(env: &Env) -> Paths {
    match env.scope {
        Scope::User => {
            let d = env.home.join(".codeium").join("windsurf");
            Paths {
                mcp: Some(d.join("mcp_config.json")),
                instructions: d.join("memories").join("global_rules.md"),
                legacy: None,
            }
        }
        Scope::Project => Paths {
            mcp: None,
            instructions: env.project.join("AGENTS.md"),
            legacy: Some(env.project.join(".windsurfrules")),
        },
    }
}

impl Adapter for Windsurf {
    fn id(&self) -> &'static str {
        ID
    }
    fn name(&self) -> &'static str {
        "Windsurf"
    }
    fn docs(&self) -> &'static str {
        "https://docs.devin.ai/windsurf/plugins/cascade/mcp"
    }
    fn scopes(&self) -> &'static [Scope] {
        &[Scope::User, Scope::Project]
    }
    fn support(&self, scope: Scope) -> [(Feature, Support); 7] {
        Feature::ALL.map(|f| {
            let s = match f {
                Feature::Mcp if scope == Scope::User => Support::Native,
                Feature::Instructions => Support::Native,
                _ => Support::None,
            };
            (f, s)
        })
    }

    fn locations(&self, env: &Env) -> Vec<(&'static str, PathBuf)> {
        let p = paths(env);
        let mut out = Vec::new();
        if let Some(mcp) = p.mcp {
            out.push(("mcp", mcp));
        }
        out.push(("instructions", p.instructions));
        out
    }

    fn detect(&self, env: &Env) -> bool {
        match env.scope {
            Scope::User => env.home.join(".codeium").join("windsurf").exists(),
            // AGENTS.md is shared with other tools and says nothing about Windsurf.
            Scope::Project => {
                env.project.join(".windsurf").exists()
                    || env.project.join(".windsurfrules").exists()
            }
        }
    }

    fn capture(&self, env: &Env) -> Result<Captured> {
        let mut cap = Captured::for_env(env);
        let p = paths(env);

        if let Some(mcp) = &p.mcp
            && let Some(text) = cap.read_config(mcp, ID)?
        {
            cap.sources.push(mcp.clone());
            let doc = parse_value(Some(&text), mcp)?;
            for (name, def) in
                server_entries(&mut cap, ID, mcp, "mcpServers", doc.get("mcpServers"))
            {
                let mut s = server_from_json(def);
                if let Some(url) = str_of(def.get("serverUrl")) {
                    s.url = Some(url);
                }
                s.disabled = def.get("disabled") == Some(&Value::Bool(true));
                import_server(&mut cap, ID, name, s, false);
            }
        }

        capture_instructions(&mut cap, &p.instructions, ID)?;
        if let Some(legacy) = &p.legacy
            && let Some(text) = read_text_item(&mut cap, legacy, ID)
            && !text.trim().is_empty()
        {
            cap.sources.push(legacy.clone());
            add_instructions(&mut cap, ID, &text, legacy);
            cap.info(ID, "imported legacy .windsurfrules into instructions");
        }
        // Rules folded into a shared file by another target (Codex's AGENTS.md)
        // come back out of capture_instructions.
        if !cap.profile.rules.is_empty() {
            let names: Vec<String> = cap.profile.rules.drain(..).map(|r| r.name).collect();
            cap.info(
                ID,
                format!(
                    "rules folded into Windsurf's instructions file by another tool were not captured ({}); capture that tool for them",
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

        if let Some(mcp) = &p.mcp {
            let mut servers = Vec::new();
            let mut sse = Vec::new();
            // `disabled` is not in Windsurf's docs, so a disabled server isn't written.
            for (name, s) in servers_for(profile, ID, work, false) {
                if s.transport() == Transport::Sse {
                    sse.push(name.as_str());
                    continue;
                }
                if s.cwd.is_some() {
                    work.warn(
                        ID,
                        format!(
                            "MCP server '{name}': 'cwd' is not supported by Windsurf and was dropped"
                        ),
                    );
                }
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
                } else {
                    def.insert("serverUrl".into(), json!(s.url));
                    if !s.headers.is_empty() {
                        def.insert("headers".into(), json!(s.headers));
                    }
                }
                servers.push((name.clone(), Value::Object(def)));
            }
            if !sse.is_empty() {
                work.warn(
                    ID,
                    format!(
                        "MCP server(s) {} skipped: Windsurf's docs list SSE as supported but don't say how mcp_config.json marks an SSE server",
                        sse.join(", ")
                    ),
                );
            }
            merge_json_servers(work, mcp, &["mcpServers"], &servers, MANAGED_KEYS, opts, ID)?;
        } else if !profile.mcp_servers.is_empty() {
            work.info(
                ID,
                format!(
                    "Windsurf documents no project-level MCP config; {} MCP server(s) not written (apply at user scope for ~/.codeium/windsurf/mcp_config.json)",
                    profile.mcp_servers.len()
                ),
            );
        }

        // At project scope this is AGENTS.md, with the same block id as Codex,
        // Cursor, Antigravity, opencode and Grok Build: applying several plans
        // the same block more than once, which leaves one copy.
        write_instructions(work, &p.instructions, profile, opts, false, ID)?;
        if env.scope == Scope::User
            && let Some(i) = &profile.instructions
            && i.chars().count() > GLOBAL_RULES_MAX_CHARS
        {
            work.warn(
                ID,
                format!(
                    "the instructions are {} characters; Windsurf's docs have limited global_rules.md to {GLOBAL_RULES_MAX_CHARS}, so the end may be ignored",
                    i.chars().count()
                ),
            );
        }
        if profile.settings.contains_key(ID) {
            work.info(
                ID,
                "kitbag doesn't manage Windsurf settings; settings.windsurf was not applied",
            );
        }
        Ok(())
    }
}
