//! Cline (the VS Code and JetBrains extension, the CLI and the SDK share one
//! configuration): MCP servers and instructions.
//!
//! * MCP: `cline_mcp_settings.json` in `~/.cline/data/settings/`, under
//!   `mcpServers`. `CLINE_DIR` replaces `~/.cline`, `CLINE_DATA_DIR` replaces
//!   `~/.cline/data`, and `CLINE_MCP_SETTINGS_PATH` names the file itself.
//!   Local servers take `command`, `args` and `env`; remote servers take `url`
//!   and `headers` with `type: "streamableHttp"` or `"sse"` (Cline reads a URL
//!   with no `type` as SSE, so kitbag always writes it). `disabled: true` turns a
//!   server off; `autoApprove`, `timeout` and anything else on an entry are kept.
//!   Cline documents no env-reference syntax, so references are resolved at apply
//!   time (or kept as placeholders with `--secrets never`), as for Claude
//!   Desktop and Zed. Cline's docs describe no project MCP file, so there is none
//!   at project scope. Nothing else in the data directory is read: it holds the
//!   provider settings, which contain API keys.
//! * Instructions: a managed block in `rules/kitbag.md` of the config directory
//!   (`~/.cline/rules/`, one of the global rules directories Cline reads), and
//!   `AGENTS.md` at the project root, which Cline reads and which is shared with
//!   Codex, Cursor, Antigravity, opencode, Grok Build and Windsurf. Rules are
//!   folded into the same block as labelled sections. Rule files of the
//!   user's own (`~/Documents/Cline/Rules/`, `.clinerules/`, `.cline/rules/`,
//!   with their `paths` frontmatter) are not read; capture says so.
//! * Commands (workflows), skills, hooks and agents are not carried.
//!
//! Docs: <https://docs.cline.bot/mcp/mcp-overview>,
//! <https://docs.cline.bot/getting-started/config>,
//! <https://docs.cline.bot/customization/cline-rules>.

use std::path::PathBuf;

use anyhow::Result;
use serde_json::{Map, Value, json};

use super::common::{
    Resolver, capture_instructions, import_server, merge_json_servers, note_unread_rule_files,
    server_entries, server_from_json, servers_for, write_instructions,
};
use super::{Adapter, ApplyOptions, Captured, Env, Feature, Scope, Support, Work};
use crate::edit::json::parse_value;
use crate::model::{Profile, Transport};

const ID: &str = "cline";
/// Server keys kitbag writes; anything else (`autoApprove`, `timeout` ...) is kept.
const MANAGED_KEYS: &[&str] = &[
    "command", "args", "env", "url", "headers", "type", "disabled",
];
/// The file kitbag writes in the global rules directory.
const RULES_FILE: &str = "kitbag.md";

pub struct Cline;

/// `~/.cline`, or `CLINE_DIR`.
fn config_root(env: &Env) -> PathBuf {
    env.location_var("CLINE_DIR")
        .unwrap_or_else(|| env.home.join(".cline"))
}

/// `~/.cline/data`, or `CLINE_DATA_DIR`; with only `CLINE_DIR` set, `data`
/// inside it.
fn data_dir(env: &Env) -> PathBuf {
    env.location_var("CLINE_DATA_DIR")
        .unwrap_or_else(|| config_root(env).join("data"))
}

/// The MCP settings file, or `None` at project scope.
fn mcp_file(env: &Env) -> Option<PathBuf> {
    (env.scope == Scope::User).then(|| {
        env.location_var("CLINE_MCP_SETTINGS_PATH")
            .unwrap_or_else(|| {
                data_dir(env)
                    .join("settings")
                    .join("cline_mcp_settings.json")
            })
    })
}

fn instructions_file(env: &Env) -> PathBuf {
    match env.scope {
        Scope::User => config_root(env).join("rules").join(RULES_FILE),
        Scope::Project => env.project.join("AGENTS.md"),
    }
}

/// Rule directories Cline reads at this scope, for the capture note.
fn rule_dirs(env: &Env) -> Vec<PathBuf> {
    match env.scope {
        Scope::User => vec![
            config_root(env).join("rules"),
            env.home.join("Documents").join("Cline").join("Rules"),
        ],
        Scope::Project => vec![
            env.project.join(".clinerules"),
            env.project.join(".cline").join("rules"),
        ],
    }
}

impl Adapter for Cline {
    fn id(&self) -> &'static str {
        ID
    }
    fn name(&self) -> &'static str {
        "Cline"
    }
    fn docs(&self) -> &'static str {
        "https://docs.cline.bot/mcp/mcp-overview"
    }
    fn scopes(&self) -> &'static [Scope] {
        &[Scope::User, Scope::Project]
    }
    fn support(&self, scope: Scope) -> [(Feature, Support); 7] {
        Feature::ALL.map(|f| {
            let s = match f {
                Feature::Mcp if scope == Scope::User => Support::Native,
                Feature::Instructions => Support::Native,
                Feature::Rules => Support::Folded,
                _ => Support::None,
            };
            (f, s)
        })
    }

    fn locations(&self, env: &Env) -> Vec<(&'static str, PathBuf)> {
        let mut out = Vec::new();
        if let Some(mcp) = mcp_file(env) {
            out.push(("mcp", mcp));
        }
        out.push(("instructions", instructions_file(env)));
        out
    }

    fn detect(&self, env: &Env) -> bool {
        match env.scope {
            Scope::User => config_root(env).is_dir() || data_dir(env).is_dir(),
            // AGENTS.md is shared with other tools and says nothing about Cline.
            Scope::Project => {
                env.project.join(".clinerules").exists() || env.project.join(".cline").is_dir()
            }
        }
    }

    fn capture(&self, env: &Env) -> Result<Captured> {
        let mut cap = Captured::for_env(env);

        if let Some(file) = mcp_file(env)
            && let Some(text) = cap.read_config(&file, ID)?
        {
            cap.sources.push(file.clone());
            let doc = parse_value(Some(&text), &file)?;
            for (name, def) in
                server_entries(&mut cap, ID, &file, "mcpServers", doc.get("mcpServers"))
            {
                let mut s = server_from_json(def);
                match def.get("type").and_then(Value::as_str) {
                    Some("streamableHttp") => s.transport = Some(Transport::Http),
                    // Cline's docs: a URL with no `type` is SSE.
                    Some("sse") | None if s.url.is_some() => s.transport = Some(Transport::Sse),
                    Some("stdio") | None => {}
                    Some(other) => {
                        cap.warn(
                            ID,
                            format!(
                                "skipped MCP server '{name}': unknown type '{other}' (Cline's docs list streamableHttp and sse)"
                            ),
                        );
                        continue;
                    }
                }
                s.disabled = def.get("disabled") == Some(&Value::Bool(true));
                import_server(&mut cap, ID, name, s, false);
            }
        }

        let file = instructions_file(env);
        capture_instructions(&mut cap, &file, ID)?;
        for dir in rule_dirs(env) {
            note_unread_rule_files(&mut cap, ID, &dir, Some(&file), &env.home);
        }
        if env.scope == Scope::Project && env.project.join(".clinerules").is_file() {
            cap.info(
                ID,
                "a .clinerules file was not read: Cline's docs describe .clinerules as a directory of rule files",
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
        if let Some(file) = mcp_file(env) {
            let servers = mcp_servers(profile, env, opts, work);
            merge_json_servers(
                work,
                &file,
                &["mcpServers"],
                &servers,
                MANAGED_KEYS,
                opts,
                ID,
            )?;
        } else if !profile.mcp_servers.is_empty() {
            work.info(
                ID,
                format!(
                    "Cline documents no project-level MCP config; {} MCP server(s) not written (apply at user scope for {})",
                    profile.mcp_servers.len(),
                    "~/.cline/data/settings/cline_mcp_settings.json"
                ),
            );
        }

        // At project scope this is AGENTS.md, with the same block id as Codex,
        // Cursor, Antigravity, opencode, Windsurf and Grok Build: applying
        // several plans the same block more than once, which leaves one copy.
        write_instructions(work, &instructions_file(env), profile, opts, true, ID)?;
        if profile.settings.contains_key(ID) {
            work.info(
                ID,
                "kitbag doesn't manage Cline settings; settings.cline was not applied",
            );
        }
        Ok(())
    }
}

/// The profile's servers as Cline entries.
fn mcp_servers(
    profile: &Profile,
    env: &Env,
    opts: &ApplyOptions,
    work: &mut Work,
) -> Vec<(String, Value)> {
    let mut servers = Vec::new();
    for (name, s) in servers_for(profile, ID, work, true) {
        if s.cwd.is_some() {
            work.warn(
                ID,
                format!("MCP server '{name}': 'cwd' is not documented by Cline and was dropped"),
            );
        }
        let mut res = Resolver::new(env, opts);
        let mut def = Map::new();
        if s.transport() == Transport::Stdio {
            def.insert(
                "command".into(),
                json!(res.value(s.command.as_deref().unwrap_or_default(), "command", work)),
            );
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
                    .map(|(k, v)| (k.clone(), json!(res.value(v, &format!("env.{k}"), work))))
                    .collect();
                def.insert("env".into(), Value::Object(e));
            }
        } else {
            def.insert(
                "url".into(),
                json!(res.value(s.url.as_deref().unwrap_or_default(), "url", work)),
            );
            let kind = if s.transport() == Transport::Sse {
                "sse"
            } else {
                "streamableHttp"
            };
            def.insert("type".into(), json!(kind));
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
        if s.disabled {
            def.insert("disabled".into(), json!(true));
        }
        res.finish(work, ID, "Cline", name, "cline_mcp_settings.json");
        servers.push((name.clone(), Value::Object(def)));
    }
    servers
}
