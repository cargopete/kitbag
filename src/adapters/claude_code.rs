//! Claude Code (Anthropic's CLI agent).
//!
//! * MCP: `~/.claude.json` (`mcpServers`, user scope) / `.mcp.json` (project).
//!   Claude Code expands `${VAR}` and `${VAR:-default}` itself, so references
//!   are written as-is and secrets never touch disk.
//! * Instructions: `~/.claude/CLAUDE.md` / `./CLAUDE.md` (or `./.claude/CLAUDE.md`).
//! * Rules: `.claude/rules/*.md` with `paths:` frontmatter for file scoping.
//! * Commands: `.claude/commands/*.md`. Skills: `.claude/skills/<name>/SKILL.md`.
//! * Settings: `.claude/settings.json` (allow-listed keys only).

use std::path::PathBuf;

use anyhow::Result;
use serde_json::{Map, Value, json};

use super::common::{
    MdKind, capture_instructions, capture_markdown_dir, capture_skills_dir, import_server,
    merge_json_servers, merge_json_settings, pick_settings, render_server, safe_settings,
    server_from_json, servers_for, write_instructions, write_owned, write_skill,
};
use super::{Adapter, ApplyOptions, Captured, Env, Feature, Scope, Support, Work};
use crate::edit::json::parse_value;
use crate::frontmatter::{self, opt};
use crate::model::{McpServer, Profile, Rule, RuleMode, Transport};
use crate::plan_fs::read_text_opt;
use crate::profile_io::command_to_markdown;
use crate::secrets::RefStyle;

const ID: &str = "claude-code";
/// Server keys kitbag writes; anything else in an existing entry is preserved.
const MANAGED_KEYS: &[&str] = &["type", "command", "args", "env", "url", "headers"];

pub struct ClaudeCode;

struct Paths {
    mcp: PathBuf,
    instructions: PathBuf,
    rules: PathBuf,
    commands: PathBuf,
    skills: PathBuf,
    settings: PathBuf,
}

fn config_dir(env: &Env) -> PathBuf {
    env.location_var("CLAUDE_CONFIG_DIR")
        .unwrap_or_else(|| env.home.join(".claude"))
}

fn claude_json(env: &Env) -> PathBuf {
    match env.location_var("CLAUDE_CONFIG_DIR") {
        Some(d) => d.join(".claude.json"),
        None => env.home.join(".claude.json"),
    }
}

fn paths(env: &Env) -> Paths {
    match env.scope {
        Scope::User => {
            let d = config_dir(env);
            Paths {
                mcp: claude_json(env),
                instructions: d.join("CLAUDE.md"),
                rules: d.join("rules"),
                commands: d.join("commands"),
                skills: d.join("skills"),
                settings: d.join("settings.json"),
            }
        }
        Scope::Project => {
            let p = &env.project;
            let root_md = p.join("CLAUDE.md");
            let alt_md = p.join(".claude").join("CLAUDE.md");
            Paths {
                mcp: p.join(".mcp.json"),
                instructions: if !root_md.exists() && alt_md.exists() {
                    alt_md
                } else {
                    root_md
                },
                rules: p.join(".claude").join("rules"),
                commands: p.join(".claude").join("commands"),
                skills: p.join(".claude").join("skills"),
                settings: p.join(".claude").join("settings.json"),
            }
        }
    }
}

/// Rule file in Claude Code's format: `paths` scopes it, otherwise always loaded.
pub fn claude_rule(r: &Rule) -> String {
    let paths = match r.mode() {
        RuleMode::Globs => Value::from(r.globs.clone()),
        _ => Value::Null,
    };
    frontmatter::render(
        &[("description", opt(&r.description)), ("paths", paths)],
        &r.body,
    )
}

fn server_json(s: &McpServer) -> Value {
    let mut def = Map::new();
    match s.transport() {
        Transport::Stdio => {
            def.insert("type".into(), json!("stdio"));
            def.insert("command".into(), json!(s.command));
            def.insert("args".into(), json!(s.args));
            if !s.env.is_empty() {
                def.insert("env".into(), json!(s.env));
            }
        }
        t => {
            def.insert("type".into(), json!(t.as_str()));
            def.insert("url".into(), json!(s.url));
            if !s.headers.is_empty() {
                def.insert("headers".into(), json!(s.headers));
            }
        }
    }
    Value::Object(def)
}

fn import(cap: &mut Captured, name: &str, def: &Map<String, Value>) {
    let ty = def.get("type").and_then(Value::as_str);
    if ty == Some("ws") {
        cap.warn(
            ID,
            format!("skipped MCP server '{name}': WebSocket transport isn't portable"),
        );
        return;
    }
    if def.contains_key("headersHelper") {
        cap.warn(
            ID,
            format!("MCP server '{name}': headersHelper is Claude-specific and was not captured"),
        );
    }
    let mut s = server_from_json(def);
    if ty == Some("sse") {
        s.transport = Some(Transport::Sse);
    }
    import_server(cap, ID, name, s, false);
}

impl Adapter for ClaudeCode {
    fn id(&self) -> &'static str {
        ID
    }
    fn name(&self) -> &'static str {
        "Claude Code"
    }
    fn docs(&self) -> &'static str {
        "https://code.claude.com/docs/en/settings"
    }
    fn scopes(&self) -> &'static [Scope] {
        &[Scope::User, Scope::Project]
    }
    fn support(&self, _scope: Scope) -> [(Feature, Support); 6] {
        Feature::ALL.map(|f| (f, Support::Native))
    }

    fn locations(&self, env: &Env) -> Vec<(&'static str, PathBuf)> {
        let p = paths(env);
        vec![
            ("mcp", p.mcp),
            ("instructions", p.instructions),
            ("rules", p.rules),
            ("commands", p.commands),
            ("skills", p.skills),
            ("settings", p.settings),
        ]
    }

    fn detect(&self, env: &Env) -> bool {
        match env.scope {
            Scope::User => config_dir(env).exists() || claude_json(env).exists(),
            Scope::Project => {
                let p = paths(env);
                p.mcp.exists() || p.instructions.exists() || env.project.join(".claude").exists()
            }
        }
    }

    fn capture(&self, env: &Env) -> Result<Captured> {
        let mut cap = Captured::default();
        let p = paths(env);

        if let Some(text) = read_text_opt(&p.mcp)? {
            cap.sources.push(p.mcp.clone());
            let doc = parse_value(Some(&text), &p.mcp)?;
            if let Some(Value::Object(servers)) = doc.get("mcpServers") {
                for (name, def) in servers {
                    if let Value::Object(def) = def {
                        import(&mut cap, name, def);
                    }
                }
            }
        }
        if env.scope == Scope::Project {
            // "local"-scope servers live in ~/.claude.json, keyed by project path.
            let cj = claude_json(env);
            if let Some(text) = read_text_opt(&cj)? {
                let doc = parse_value(Some(&text), &cj)?;
                let key = env.project.to_string_lossy();
                if let Some(Value::Object(servers)) = doc
                    .get("projects")
                    .and_then(|p| p.get(key.as_ref()))
                    .and_then(|p| p.get("mcpServers"))
                    && !servers.is_empty()
                {
                    cap.sources.push(cj.clone());
                    for (name, def) in servers {
                        if let Value::Object(def) = def {
                            import(&mut cap, name, def);
                        }
                    }
                    cap.info(
                        ID,
                        "included local-scope servers for this project from ~/.claude.json",
                    );
                }
            }
        }

        capture_instructions(&mut cap, &p.instructions, ID)?;
        capture_markdown_dir(&mut cap, &p.rules, ".md", MdKind::Rule, ID)?;
        capture_markdown_dir(&mut cap, &p.commands, ".md", MdKind::Command, ID)?;
        capture_skills_dir(&mut cap, &p.skills, ID)?;

        if let Some(text) = read_text_opt(&p.settings)? {
            cap.sources.push(p.settings.clone());
            if let Value::Object(doc) = parse_value(Some(&text), &p.settings)? {
                pick_settings(&mut cap, ID, &doc);
                let skipped: Vec<&str> = [
                    "env",
                    "hooks",
                    "apiKeyHelper",
                    "awsAuthRefresh",
                    "statusLine",
                ]
                .into_iter()
                .filter(|k| doc.contains_key(*k))
                .collect();
                if !skipped.is_empty() {
                    cap.info(
                        ID,
                        format!(
                            "not captured from settings.json (may hold secrets or machine-specific commands): {}",
                            skipped.join(", ")
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
        let p = paths(env);

        let mut servers = Vec::new();
        for (name, s) in servers_for(profile, ID, work, false) {
            if s.cwd.is_some() {
                work.warn(
                    ID,
                    format!(
                        "MCP server '{name}': 'cwd' is not supported by Claude Code and was dropped"
                    ),
                );
            }
            let s = render_server(s, RefStyle::Dollar, env, work);
            servers.push((name.clone(), server_json(&s)));
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

        write_instructions(work, &p.instructions, profile, opts, false, ID)?;

        for r in &profile.rules {
            if r.mode() == RuleMode::OnDemand {
                work.info(
                    ID,
                    format!("rule '{}' is on-demand in the profile; Claude Code has no on-demand rules, so it is always loaded", r.name),
                );
            }
            let path = p.rules.join(format!("{}.md", r.name));
            write_owned(
                work,
                &path,
                claude_rule(r).as_bytes(),
                opts,
                ID,
                &format!("rule '{}'", r.name),
            )?;
        }
        for c in &profile.commands {
            let path = p.commands.join(format!("{}.md", c.name));
            let body = command_to_markdown(c, true);
            write_owned(
                work,
                &path,
                body.as_bytes(),
                opts,
                ID,
                &format!("command '{}'", c.name),
            )?;
        }
        for s in &profile.skills {
            write_skill(work, &p.skills.join(&s.name), s, opts, ID)?;
        }

        let settings = safe_settings(profile, ID, work);
        merge_json_settings(work, &p.settings, &settings, opts, ID)?;
        Ok(())
    }
}
