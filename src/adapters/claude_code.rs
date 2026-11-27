//! Claude Code (Anthropic's CLI agent).
//!
//! * MCP: `~/.claude.json` (`mcpServers`, user scope) / `.mcp.json` (project).
//!   Claude Code expands `${VAR}` and `${VAR:-default}` itself, so references
//!   are written as-is and secrets never touch disk.
//! * Instructions: `~/.claude/CLAUDE.md` / `./CLAUDE.md` (or `./.claude/CLAUDE.md`).
//! * Rules: `.claude/rules/*.md` with `paths:` frontmatter for file scoping.
//! * Commands: `.claude/commands/*.md`. Skills: `.claude/skills/<name>/SKILL.md`.
//! * Subagents: `~/.claude/agents/*.md` and `.claude/agents/*.md` (scanned
//!   recursively), markdown with `name` and `description` frontmatter, the body
//!   being the system prompt. Frontmatter keys other than `description` are kept
//!   as they are in the profile's agent.
//!   Docs: <https://code.claude.com/docs/en/sub-agents>.
//! * Settings: `.claude/settings.json` (allow-listed keys only).

use std::path::{Path, PathBuf};

use anyhow::Result;
use serde_json::{Map, Value, json};

use super::common::{
    MdKind, add_agent, capture_instructions, capture_markdown_dir, capture_skills_dir, claim_name,
    import_server, keep_shared_servers, merge_json_servers, merge_json_settings, name_from_rel,
    pick_settings, read_text_item, render_server, safe_settings, server_entries, server_from_json,
    servers_for, write_agent, write_instructions, write_skill,
};
use super::{Adapter, ApplyOptions, Captured, Env, Feature, Scope, Support, Work};
use crate::edit::json::parse_value;
use crate::frontmatter::{self, opt};
use crate::model::{Agent, McpServer, Profile, Rule, RuleMode, Transport, is_valid_name};
use crate::plan_fs::{join_rel, read_text_opt, walk_files};
use crate::profile_io::{agent_from_markdown, command_to_markdown};
use crate::secrets::RefStyle;

pub(crate) const ID: &str = "claude-code";
/// Server keys kitbag writes; anything else in an existing entry is preserved.
pub(crate) const MANAGED_KEYS: &[&str] = &["type", "command", "args", "env", "url", "headers"];

pub struct ClaudeCode;

struct Paths {
    mcp: PathBuf,
    instructions: PathBuf,
    rules: PathBuf,
    commands: PathBuf,
    skills: PathBuf,
    agents: PathBuf,
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
                agents: d.join("agents"),
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
                agents: p.join(".claude").join("agents"),
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

/// Frontmatter keys the Claude Code subagent docs list. Any other key in an
/// agent is kept and written as it is, with a note, since Claude Code ignores
/// unrecognised fields.
const AGENT_KEYS: &[&str] = &[
    "name",
    "description",
    "tools",
    "disallowedTools",
    "model",
    "permissionMode",
    "maxTurns",
    "autoCompactWindow",
    "skills",
    "mcpServers",
    "hooks",
    "memory",
    "background",
    "omitClaudeMd",
    "effort",
    "isolation",
    "color",
    "initialPrompt",
    "experimental",
];

/// Subagent keys with no counterpart in Copilot's custom agent format that
/// kitbag knows of; apply to Copilot says so.
pub(crate) const CLAUDE_ONLY_AGENT_KEYS: &[&str] = &[
    "disallowedTools",
    "permissionMode",
    "maxTurns",
    "autoCompactWindow",
    "mcpServers",
    "memory",
    "background",
    "omitClaudeMd",
    "isolation",
    "initialPrompt",
    "experimental",
];

/// A subagent as a Claude Code file: `name` (required by Claude Code; the
/// profile's agent name unless the agent carries one) and `description`, then
/// the other keys as they were.
pub fn claude_agent(a: &Agent) -> String {
    let name = a
        .extra
        .get("name")
        .cloned()
        .unwrap_or_else(|| json!(a.name));
    let mut entries: Vec<(&str, Value)> =
        vec![("name", name), ("description", opt(&a.description))];
    entries.extend(
        a.extra
            .iter()
            .filter(|(k, _)| k.as_str() != "name")
            .map(|(k, v)| (k.as_str(), v.clone())),
    );
    frontmatter::render(&entries, &a.body)
}

/// Capture `<dir>/**/*.md` as subagents. The file stem is the agent's name
/// (Claude Code's own identity is the `name` key, which is kept with the
/// other frontmatter), and two files that flatten to the same name are an
/// error, as for commands and rules.
fn capture_agents(cap: &mut Captured, dir: &Path) -> Result<()> {
    if cap.escapes(dir, ID) {
        return Ok(());
    }
    let mut seen = std::collections::BTreeMap::new();
    for rel in walk_files(dir, 500)? {
        if !rel.ends_with(".md") {
            continue;
        }
        let path = join_rel(dir, &rel);
        if rel
            .rsplit('/')
            .next()
            .is_some_and(|f| f.eq_ignore_ascii_case("readme.md"))
        {
            cap.warn(
                ID,
                format!("skipped {}: a README, not a subagent", path.display()),
            );
            continue;
        }
        let name = name_from_rel(&rel, ".md");
        claim_name(&mut seen, dir, &rel, &name)?;
        if !is_valid_name(&name) {
            cap.warn(
                ID,
                format!("skipped {}: unsupported file name", path.display()),
            );
            continue;
        }
        let Some(text) = read_text_item(cap, &path, ID) else {
            continue;
        };
        if add_agent(cap, agent_from_markdown(&name, &text), ID) {
            cap.sources.push(path);
        }
    }
    Ok(())
}

/// One subagent as `<dir>/<name>.md`.
fn apply_agent(work: &mut Work, dir: &Path, a: &Agent, opts: &ApplyOptions) -> Result<()> {
    if a.description.is_none() {
        work.info(
            ID,
            format!(
                "agent '{}' has no description; Claude Code requires one, since it decides from it when to use the agent",
                a.name
            ),
        );
    }
    let other: Vec<&str> = a
        .extra
        .keys()
        .map(String::as_str)
        .filter(|k| !AGENT_KEYS.contains(k))
        .collect();
    if !other.is_empty() {
        work.info(
            ID,
            format!(
                "agent '{}': {} {} not documented for Claude Code subagents; written as they are, Claude Code ignores unrecognised fields",
                a.name,
                other.join(", "),
                if other.len() == 1 { "is" } else { "are" }
            ),
        );
    }
    let path = dir.join(format!("{}.md", a.name));
    write_agent(work, &path, claude_agent(a).as_bytes(), opts, ID, &a.name)
}

pub(crate) fn server_json(s: &McpServer) -> Value {
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

/// Output styles that ship with Claude Code; any other name is a file in `output-styles/`.
const BUILTIN_OUTPUT_STYLES: &[&str] = &["default", "explanatory", "learning"];

/// `outputStyle` naming a custom style would point at a file the profile doesn't carry.
fn drop_custom_output_style(cap: &mut Captured) {
    let Some(settings) = cap.profile.settings.get_mut(ID) else {
        return;
    };
    let Some(Value::String(style)) = settings.get("outputStyle") else {
        return;
    };
    if BUILTIN_OUTPUT_STYLES.contains(&style.to_ascii_lowercase().as_str()) {
        return;
    }
    let style = style.clone();
    settings.remove("outputStyle");
    if settings.is_empty() {
        cap.profile.settings.remove(ID);
    }
    cap.warn(
        ID,
        format!(
            "dropped setting outputStyle = \"{style}\": it names a custom output style, and kitbag doesn't carry output styles"
        ),
    );
}

/// Claude Code features kitbag has no portable form for yet, so the user
/// knows what a captured profile leaves behind.
fn note_not_carried(cap: &mut Captured, env: &Env) {
    let dir = match env.scope {
        Scope::User => config_dir(env),
        Scope::Project => env.project.join(".claude"),
    };
    let count_md = |sub: &str| {
        walk_files(&dir.join(sub), 10_000)
            .map(|f| f.iter().filter(|r| r.ends_with(".md")).count())
            .unwrap_or(0)
    };
    let plugins = read_text_opt(&dir.join("plugins").join("installed_plugins.json"))
        .ok()
        .flatten()
        .and_then(|t| serde_json::from_str::<Value>(&t).ok())
        .map_or(0, |v| match v.get("plugins") {
            Some(Value::Object(m)) => m.len(),
            Some(Value::Array(a)) => a.len(),
            _ => 0,
        });
    let plural =
        |n: usize, one: &str, many: &str| format!("{n} {}", if n == 1 { one } else { many });
    let mut parts = Vec::new();
    let styles = count_md("output-styles");
    if styles > 0 {
        parts.push(format!(
            "{} (output-styles/)",
            plural(styles, "output style", "output styles")
        ));
    }
    if plugins > 0 {
        parts.push(format!(
            "{} (plugins/installed_plugins.json)",
            plural(plugins, "plugin", "plugins")
        ));
    }
    if !parts.is_empty() {
        cap.info(
            ID,
            format!(
                "not carried by kitbag (no portable form yet): {}",
                parts.join(", ")
            ),
        );
    }
}

/// Permission rules and directories the settings file already has in another
/// spelling of the same home path (`Read(~/x)` and `Read(//home/me/x)`) are
/// not added again: lists are merged as a union.
fn drop_equivalent_paths(settings: &mut Map<String, Value>, existing: &Value, env: &Env) {
    let Some(Value::Object(perms)) = settings.get_mut("permissions") else {
        return;
    };
    let norm = |list: &str, v: &str| match list {
        "additionalDirectories" => crate::portable::to_home_relative(v, &env.home, env.os),
        _ => crate::portable::rule_to_home_relative(v, &env.home, env.os),
    };
    for list in ["allow", "ask", "deny", "additionalDirectories"] {
        let have: Vec<String> = existing
            .get("permissions")
            .and_then(|p| p.get(list))
            .and_then(Value::as_array)
            .map(|a| {
                a.iter()
                    .filter_map(Value::as_str)
                    .map(|v| norm(list, v))
                    .collect()
            })
            .unwrap_or_default();
        if let Some(Value::Array(items)) = perms.get_mut(list) {
            items.retain(|r| {
                r.as_str().is_none_or(|v| {
                    let n = norm(list, v);
                    // Keep exact matches (a no-op in the merge); drop other spellings.
                    !have.contains(&n) || existing_has_exact(existing, list, v)
                })
            });
        }
    }
}

fn existing_has_exact(existing: &Value, list: &str, v: &str) -> bool {
    existing
        .get("permissions")
        .and_then(|p| p.get(list))
        .and_then(Value::as_array)
        .is_some_and(|a| a.iter().any(|x| x.as_str() == Some(v)))
}

/// A permission rule capture redacted (`Bash(curl -H 'X-Api-Key: [REDACTED]' *)`)
/// no longer matches what it used to, so it isn't added.
fn drop_redacted_rules(settings: &mut Map<String, Value>, work: &mut Work) {
    let Some(Value::Object(perms)) = settings.get_mut("permissions") else {
        return;
    };
    let mut dropped = Vec::new();
    for list in ["allow", "ask", "deny"] {
        if let Some(Value::Array(rules)) = perms.get_mut(list) {
            rules.retain(|r| {
                let redacted = r.as_str().is_some_and(crate::secrets::has_redaction_marker);
                if redacted {
                    dropped.push(format!("permissions.{list} {r}"));
                }
                !redacted
            });
            if rules.is_empty() {
                perms.remove(list);
            }
        }
    }
    if perms.is_empty() {
        settings.remove("permissions");
    }
    if !dropped.is_empty() {
        work.warn(
            ID,
            format!(
                "permission rules with [REDACTED] markers not applied (capture removed a secret from them; add them back by hand): {}",
                dropped.join(", ")
            ),
        );
    }
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
    fn support(&self, _scope: Scope) -> [(Feature, Support); 7] {
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
            ("agents", p.agents),
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
        let mut cap = Captured::for_env(env);
        let p = paths(env);

        if let Some(text) = cap.read_config(&p.mcp, ID)? {
            cap.sources.push(p.mcp.clone());
            let doc = parse_value(Some(&text), &p.mcp)?;
            for (name, def) in
                server_entries(&mut cap, ID, &p.mcp, "mcpServers", doc.get("mcpServers"))
            {
                import(&mut cap, name, def);
            }
        }
        if env.scope == Scope::Project {
            // "local"-scope servers live in ~/.claude.json, keyed by project path.
            let cj = claude_json(env);
            if let Some(text) = read_text_opt(&cj)? {
                let doc = parse_value(Some(&text), &cj)?;
                let key = env.project.to_string_lossy();
                let servers = doc
                    .get("projects")
                    .and_then(|p| p.get(key.as_ref()))
                    .and_then(|p| p.get("mcpServers"));
                let entries = server_entries(&mut cap, ID, &cj, "mcpServers", servers);
                if !entries.is_empty() {
                    cap.sources.push(cj.clone());
                    for (name, def) in entries {
                        import(&mut cap, name, def);
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
        capture_agents(&mut cap, &p.agents)?;

        if let Some(text) = cap.read_config(&p.settings, ID)? {
            cap.sources.push(p.settings.clone());
            if let Value::Object(doc) = parse_value(Some(&text), &p.settings)? {
                pick_settings(&mut cap, ID, &doc);
                let skipped: Vec<&str> = [
                    "env",
                    "hooks",
                    "apiKeyHelper",
                    "awsAuthRefresh",
                    "statusLine",
                    "enabledPlugins",
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
            drop_custom_output_style(&mut cap);
        }
        note_not_carried(&mut cap, env);
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
        if env.scope == Scope::Project {
            // GitHub Copilot reads the project .mcp.json too.
            keep_shared_servers(work, profile, opts, ID, "copilot", "GitHub Copilot", &p.mcp);
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
            super::common::write_rule(work, &path, claude_rule(r).as_bytes(), opts, ID, &r.name)?;
        }
        for c in &profile.commands {
            let path = p.commands.join(format!("{}.md", c.name));
            let body = command_to_markdown(c, true);
            super::common::write_command(work, &path, body.as_bytes(), opts, ID, &c.name)?;
        }
        for s in &profile.skills {
            write_skill(work, &p.skills.join(&s.name), s, opts, ID)?;
        }
        for a in &profile.agents {
            apply_agent(work, &p.agents, a, opts)?;
        }

        let mut settings = safe_settings(profile, ID, work);
        drop_redacted_rules(&mut settings, work);
        let existing = work.fs.read(&p.settings)?;
        if let Some(text) = existing {
            let doc = parse_value(Some(&text), &p.settings)?;
            drop_equivalent_paths(&mut settings, &doc, env);
        }
        merge_json_settings(work, &p.settings, &settings, opts, ID)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::profile_io::agent_from_markdown;

    #[test]
    fn a_subagent_file_keeps_every_documented_key() {
        let text = "---\nname: reviewer\ndescription: Reviews\ntools: Read, Grep\nmodel: inherit\nmcpServers:\n  - github\n---\n\nBody.\n";
        let a = agent_from_markdown("rev", text);
        assert_eq!(a.extra["name"], "reviewer");
        let out = claude_agent(&a);
        assert_eq!(agent_from_markdown("rev", &out), a);
        assert!(out.starts_with("---\nname: reviewer\ndescription: Reviews\n"));
    }

    #[test]
    fn name_defaults_to_the_agent_name() {
        let a = agent_from_markdown("helper", "---\ndescription: Helps\n---\nGo.\n");
        assert!(claude_agent(&a).starts_with("---\nname: helper\ndescription: Helps\n"));
    }
}
