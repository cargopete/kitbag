//! Claude Desktop (local MCP servers only).
//!
//! `claude_desktop_config.json` lives in `~/Library/Application Support/Claude`
//! (macOS), `%APPDATA%\Claude` (Windows) or `~/.config/Claude` (Linux builds).
//! Claude Desktop does not expand env references, so they are resolved at apply
//! time (or kept as placeholders with `--secrets never`). Remote servers are
//! added through Settings > Connectors in the app, not this file.

use std::path::PathBuf;

use anyhow::Result;
use serde_json::{Map, Value, json};

use super::common::{
    Resolver, import_server, merge_json_servers, server_entries, server_from_json, servers_for,
};
use super::{Adapter, ApplyOptions, Captured, Env, Feature, Os, Scope, Support, Work};
use crate::edit::json::parse_value;
use crate::model::{Profile, Transport};
use crate::plan_fs::read_text_opt;

const ID: &str = "claude-desktop";
const MANAGED_KEYS: &[&str] = &["command", "args", "env"];

pub struct ClaudeDesktop;

pub fn config_path(env: &Env) -> PathBuf {
    let file = "claude_desktop_config.json";
    match env.os {
        Os::MacOs => env
            .home
            .join("Library/Application Support/Claude")
            .join(file),
        Os::Windows => env
            .location_var("APPDATA")
            .unwrap_or_else(|| env.home.join("AppData").join("Roaming"))
            .join("Claude")
            .join(file),
        Os::Linux => env
            .location_var("XDG_CONFIG_HOME")
            .unwrap_or_else(|| env.home.join(".config"))
            .join("Claude")
            .join(file),
    }
}

impl Adapter for ClaudeDesktop {
    fn id(&self) -> &'static str {
        ID
    }
    fn name(&self) -> &'static str {
        "Claude Desktop"
    }
    fn docs(&self) -> &'static str {
        "https://modelcontextprotocol.io/docs/develop/connect-local-servers"
    }
    fn scopes(&self) -> &'static [Scope] {
        &[Scope::User]
    }
    fn support(&self, _scope: Scope) -> [(Feature, Support); 6] {
        Feature::ALL.map(|f| {
            (
                f,
                if f == Feature::Mcp {
                    Support::Native
                } else {
                    Support::None
                },
            )
        })
    }
    fn locations(&self, env: &Env) -> Vec<(&'static str, PathBuf)> {
        vec![("mcp", config_path(env))]
    }
    fn detect(&self, env: &Env) -> bool {
        env.scope == Scope::User && config_path(env).parent().is_some_and(|d| d.exists())
    }

    fn capture(&self, env: &Env) -> Result<Captured> {
        let mut cap = Captured::for_env(env);
        let file = config_path(env);
        let Some(text) = read_text_opt(&file)? else {
            return Ok(cap);
        };
        cap.sources.push(file.clone());
        let doc = parse_value(Some(&text), &file)?;
        for (name, def) in server_entries(&mut cap, ID, &file, "mcpServers", doc.get("mcpServers"))
        {
            import_server(&mut cap, ID, name, server_from_json(def), false);
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
        let mut servers = Vec::new();
        let mut remote = Vec::new();
        for (name, s) in servers_for(profile, ID, work, false) {
            if s.transport() != Transport::Stdio {
                remote.push(name.as_str());
                continue;
            }
            if s.cwd.is_some() {
                work.warn(ID, format!("MCP server '{name}': 'cwd' is not supported by Claude Desktop and was dropped"));
            }
            let mut res = Resolver::new(env, opts);
            let mut def = Map::new();
            def.insert(
                "command".into(),
                json!(res.value(s.command.as_deref().unwrap_or_default(), "command", work)),
            );
            let args: Vec<String> = s
                .args
                .iter()
                .enumerate()
                .map(|(i, a)| res.value(a, &format!("args[{i}]"), work))
                .collect();
            def.insert("args".into(), json!(args));
            if !s.env.is_empty() {
                let e: Map<String, Value> = s
                    .env
                    .iter()
                    .map(|(k, v)| (k.clone(), json!(res.value(v, &format!("env.{k}"), work))))
                    .collect();
                def.insert("env".into(), Value::Object(e));
            }
            res.finish(
                work,
                ID,
                "Claude Desktop",
                name,
                "claude_desktop_config.json",
            );
            servers.push((name.clone(), Value::Object(def)));
        }
        if !remote.is_empty() {
            work.info(
                ID,
                format!(
                    "remote MCP server(s) {} skipped: add them in Claude Desktop under Settings > Connectors (the config file holds local servers only)",
                    remote.join(", ")
                ),
            );
        }
        merge_json_servers(
            work,
            &config_path(env),
            &["mcpServers"],
            &servers,
            MANAGED_KEYS,
            opts,
            ID,
        )?;
        Ok(())
    }
}
