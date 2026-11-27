//! OpenAI Codex CLI.
//!
//! * MCP + settings: `$CODEX_HOME/config.toml` (default `~/.codex`) or
//!   `.codex/config.toml` in a trusted project, as `[mcp_servers.<name>]` tables.
//!   Codex does not interpolate env references in config, but it can forward
//!   variables (`env_vars`), read bearer tokens (`bearer_token_env_var`) and
//!   headers (`env_http_headers`) from the environment. kitbag uses those so
//!   secrets stay out of the file; anything else is resolved (or kept) per `--secrets`.
//! * Instructions: `AGENTS.md` (`$CODEX_HOME/AGENTS.md` or the project root).
//!   Codex has no file-scoped rules, so rules are folded into AGENTS.md.
//! * Commands: custom prompts in `$CODEX_HOME/prompts/` (user scope only).
//! * Skills: `~/.agents/skills/` (user) or `.agents/skills/` (project).
//! * Custom agents: `$CODEX_HOME/agents/<name>.toml` (user) and
//!   `.codex/agents/<name>.toml` (project), one TOML file per agent with
//!   `name`, `description` and `developer_instructions` (the profile agent's
//!   body), and optionally `model`, `model_reasoning_effort`,
//!   `model_reasoning_summary`, `model_verbosity`, `features` and `skills`.
//!   Codex says `sandbox_mode`, `approval_policy`, `model_provider` and
//!   `mcp_servers` in an agent file don't override the parent's, so they are
//!   not written. Docs: <https://developers.openai.com/codex/subagents>.

use std::path::PathBuf;
use std::sync::LazyLock;

use anyhow::Result;
use regex::Regex;
use serde_json::{Map, Value, json};
use toml_edit::Item;

use super::common::{
    MdKind, Resolver, add_agent, agent_files, capture_instructions, capture_markdown_dir,
    capture_skills_dir, import_server, merge_server_def, note_agent_keys, off_skipped_server,
    record_server_write, safe_settings, server_entries, servers_for, set_enabled_false,
    skill_with_required_frontmatter, skipped_servers, str_of, string_list, string_map, write_agent,
    write_instructions, write_skill,
};
use super::{Adapter, ApplyOptions, Captured, Env, Feature, Scope, Support, Work};
use crate::edit::keep_line_endings;
use crate::edit::toml::{
    item_to_json, json_to_table, json_to_value, parse, set_keep_decor, table_mut,
};
use crate::model::{Agent, McpServer, Profile, Transport};
use crate::policy;
use crate::profile_io::command_to_markdown;
use crate::secrets::{reference, sole_ref};

const ID: &str = "codex";
/// Server keys kitbag writes; anything else (timeouts, tool filters, oauth) is preserved.
const MANAGED_KEYS: &[&str] = &[
    "command",
    "args",
    "env",
    "env_vars",
    "cwd",
    "url",
    "bearer_token_env_var",
    "http_headers",
    "env_http_headers",
    "enabled",
];
/// Keys Codex ignores in a project-scoped `.codex/config.toml`.
const USER_ONLY_KEYS: &[&str] = &[
    "model_provider",
    "model_providers",
    "notify",
    "profile",
    "profiles",
    "openai_base_url",
    "chatgpt_base_url",
    "otel",
];
/// Codex reads at most this many bytes of AGENTS.md by default (`project_doc_max_bytes`).
const AGENTS_MD_LIMIT: usize = 32 * 1024;

pub struct Codex;

struct Paths {
    config: PathBuf,
    instructions: PathBuf,
    prompts: Option<PathBuf>,
    skills: PathBuf,
    agents: PathBuf,
}

fn codex_home(env: &Env) -> PathBuf {
    env.location_var("CODEX_HOME")
        .unwrap_or_else(|| env.home.join(".codex"))
}

fn paths(env: &Env) -> Paths {
    match env.scope {
        Scope::User => {
            let d = codex_home(env);
            Paths {
                config: d.join("config.toml"),
                instructions: d.join("AGENTS.md"),
                prompts: Some(d.join("prompts")),
                skills: env.home.join(".agents").join("skills"),
                agents: d.join("agents"),
            }
        }
        Scope::Project => Paths {
            config: env.project.join(".codex").join("config.toml"),
            instructions: env.project.join("AGENTS.md"),
            prompts: None,
            skills: env.project.join(".agents").join("skills"),
            agents: env.project.join(".codex").join("agents"),
        },
    }
}

static BEARER: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)^Bearer\s+(.+)$").expect("valid regex"));

/// Render one profile server as a Codex `[mcp_servers.<name>]` table.
fn codex_server(
    name: &str,
    s: &McpServer,
    env: &Env,
    opts: &ApplyOptions,
    work: &mut Work,
) -> Option<Map<String, Value>> {
    let mut res = Resolver::new(env, opts);
    let mut t = Map::new();
    match s.transport() {
        Transport::Stdio => {
            let command = res.value(s.command.as_deref().unwrap_or_default(), "command", work);
            t.insert("command".into(), json!(command));
            if !s.args.is_empty() {
                let args: Vec<String> = s
                    .args
                    .iter()
                    .enumerate()
                    .map(|(i, a)| res.value(a, &format!("args[{i}]"), work))
                    .collect();
                t.insert("args".into(), json!(args));
            }
            let mut literal = Map::new();
            let mut forward = Vec::new();
            for (k, v) in &s.env {
                match sole_ref(v) {
                    // `KEY = ${KEY}` is exactly what env_vars does: forward it from Codex's environment.
                    Some(r) if r.name == *k && r.fallback.is_none() => forward.push(k.clone()),
                    _ => {
                        literal.insert(k.clone(), json!(res.value(v, &format!("env.{k}"), work)));
                    }
                }
            }
            if !literal.is_empty() {
                t.insert("env".into(), Value::Object(literal));
            }
            if !forward.is_empty() {
                t.insert("env_vars".into(), json!(forward));
            }
            if let Some(cwd) = &s.cwd {
                t.insert("cwd".into(), json!(res.value(cwd, "cwd", work)));
            }
        }
        Transport::Http => {
            let url = res.value(s.url.as_deref().unwrap_or_default(), "url", work);
            t.insert("url".into(), json!(url));
            let mut literal = Map::new();
            let mut from_env = Map::new();
            for (k, v) in &s.headers {
                let bearer_ref = BEARER.captures(v).and_then(|c| sole_ref(&c[1]));
                if k.eq_ignore_ascii_case("authorization")
                    && let Some(r) = bearer_ref.filter(|r| r.fallback.is_none())
                {
                    t.insert("bearer_token_env_var".into(), json!(r.name));
                    continue;
                }
                match sole_ref(v) {
                    Some(r) if r.fallback.is_none() => {
                        from_env.insert(k.clone(), json!(r.name));
                    }
                    _ => {
                        literal.insert(
                            k.clone(),
                            json!(res.value(v, &format!("headers.{k}"), work)),
                        );
                    }
                }
            }
            if !literal.is_empty() {
                t.insert("http_headers".into(), Value::Object(literal));
            }
            if !from_env.is_empty() {
                t.insert("env_http_headers".into(), Value::Object(from_env));
            }
        }
        Transport::Sse => {
            work.warn(ID, format!("MCP server '{name}': Codex supports stdio and streamable HTTP only; SSE server skipped"));
            return None;
        }
    }
    if s.disabled {
        t.insert("enabled".into(), json!(false));
    }
    res.finish(work, ID, "Codex", name, "config.toml");
    Some(t)
}

fn import(cap: &mut Captured, name: &str, def: &Map<String, Value>) {
    let mut env = string_map(def.get("env"));
    if let Some(Value::Array(vars)) = def.get("env_vars") {
        for v in vars {
            let n = v.as_str().or_else(|| v.get("name").and_then(Value::as_str));
            if let Some(n) = n {
                env.insert(n.to_string(), reference(n));
            }
        }
    }
    let mut headers = string_map(def.get("http_headers"));
    for (k, v) in string_map(def.get("env_http_headers")) {
        headers.insert(k, reference(&v));
    }
    if let Some(var) = str_of(def.get("bearer_token_env_var")) {
        headers.insert(
            "Authorization".into(),
            format!("Bearer {}", reference(&var)),
        );
    }
    let dropped: Vec<&str> = def
        .keys()
        .map(String::as_str)
        .filter(|k| !MANAGED_KEYS.contains(k))
        .collect();
    if !dropped.is_empty() {
        cap.info(
            ID,
            format!(
                "MCP server '{name}': Codex-specific keys not captured: {}",
                dropped.join(", ")
            ),
        );
    }
    let s = McpServer {
        command: str_of(def.get("command")),
        args: string_list(def.get("args")),
        env,
        cwd: str_of(def.get("cwd")),
        url: str_of(def.get("url")),
        headers,
        disabled: def.get("enabled") == Some(&Value::Bool(false)),
        ..Default::default()
    };
    import_server(cap, ID, name, s, false);
}

/// Keys of a Codex agent file kitbag carries besides `name`, `description`
/// and `developer_instructions`: the model settings (strings) and the
/// `features` and `skills` tables, as Codex's docs list them.
const AGENT_KEYS: &[&str] = &[
    "model",
    "model_reasoning_effort",
    "model_reasoning_summary",
    "model_verbosity",
    "features",
    "skills",
];

/// A Codex agent file as a profile agent. Codex takes the agent's name from
/// the `name` key, not the file name, so a `name` that differs from the file
/// name is kept in the agent's extra keys, and `developer_instructions`
/// becomes the body.
fn agent_from_toml(name: &str, text: &str, path: &std::path::Path) -> Result<Agent> {
    let doc = parse(Some(text), path)?;
    let mut a = Agent {
        name: name.to_string(),
        ..Default::default()
    };
    for (k, item) in doc.iter() {
        let v = item_to_json(item);
        match (k, v) {
            ("description", Value::String(s)) => a.description = Some(s),
            ("developer_instructions", Value::String(s)) => a.body = s.trim().to_string(),
            ("name", Value::String(s)) if s == name => {}
            (_, Value::Null) => {}
            (k, v) => {
                a.extra.insert(k.to_string(), v);
            }
        }
    }
    Ok(a)
}

fn capture_agents(cap: &mut Captured, dir: &std::path::Path) -> Result<()> {
    for f in agent_files(cap, dir, ".toml", None, ID, "a custom agent")? {
        match agent_from_toml(&f.name, &f.text, &f.path) {
            Ok(a) => {
                if add_agent(cap, a, ID) {
                    cap.sources.push(f.path);
                }
            }
            Err(e) => cap.warn(ID, format!("skipped {}: {e:#}", f.path.display())),
        }
    }
    Ok(())
}

/// A profile agent as a Codex agent file, and the keys left out: those
/// Codex's docs don't list for an agent file, or with a value of another type.
pub fn codex_agent(a: &Agent) -> (String, Vec<String>) {
    let mut doc = toml_edit::DocumentMut::new();
    let t = doc.as_table_mut();
    let name = a
        .extra
        .get("name")
        .and_then(Value::as_str)
        .unwrap_or(&a.name);
    t.insert("name", toml_edit::value(name));
    t.insert(
        "description",
        toml_edit::value(a.description.as_deref().unwrap_or_default()),
    );
    let mut dropped = Vec::new();
    let mut tables = Vec::new();
    for (k, v) in &a.extra {
        if k == "name" {
            continue;
        }
        let ok = AGENT_KEYS.contains(&k.as_str())
            && match k.as_str() {
                "features" | "skills" => v.is_object(),
                _ => v.is_string(),
            };
        match (ok, v) {
            (true, Value::Object(m)) => tables.push((k.clone(), m)),
            (true, v) => {
                if let Some(val) = json_to_value(v) {
                    t.insert(k, Item::Value(val));
                }
            }
            (false, _) => dropped.push(k.clone()),
        }
    }
    t.insert(
        "developer_instructions",
        toml_edit::value(format!("{}\n", a.body.trim())),
    );
    for (k, m) in tables {
        t.insert(&k, Item::Table(json_to_table(m)));
    }
    (doc.to_string(), dropped)
}

fn apply_agent(
    work: &mut Work,
    dir: &std::path::Path,
    a: &Agent,
    opts: &ApplyOptions,
) -> Result<()> {
    let missing = match (&a.description, a.body.trim().is_empty()) {
        (None, true) => Some("a description and instructions"),
        (None, false) => Some("a description"),
        (Some(_), true) => Some("instructions (the agent's body)"),
        _ => None,
    };
    if let Some(what) = missing {
        work.warn(
            ID,
            format!(
                "agent '{}' skipped: Codex requires {what}, and kitbag does not make them up",
                a.name
            ),
        );
        return Ok(());
    }
    let (text, dropped) = codex_agent(a);
    note_agent_keys(work, ID, "Codex", a, &dropped);
    let path = dir.join(format!("{}.toml", a.name));
    write_agent(work, &path, text.as_bytes(), opts, ID, &a.name)
}

impl Adapter for Codex {
    fn id(&self) -> &'static str {
        ID
    }
    fn name(&self) -> &'static str {
        "OpenAI Codex CLI"
    }
    fn docs(&self) -> &'static str {
        "https://developers.openai.com/codex/config-reference"
    }
    fn scopes(&self) -> &'static [Scope] {
        &[Scope::User, Scope::Project]
    }
    fn support(&self, scope: Scope) -> [(Feature, Support); 8] {
        [
            (Feature::Mcp, Support::Native),
            (Feature::Instructions, Support::Native),
            (Feature::Rules, Support::Folded),
            (
                Feature::Commands,
                if scope == Scope::User {
                    Support::Native
                } else {
                    Support::None
                },
            ),
            (Feature::Skills, Support::Native),
            (Feature::Agents, Support::Native),
            (Feature::OutputStyles, Support::None),
            (Feature::Settings, Support::Native),
        ]
    }

    fn locations(&self, env: &Env) -> Vec<(&'static str, PathBuf)> {
        let p = paths(env);
        let mut out = vec![
            ("mcp", p.config.clone()),
            ("instructions", p.instructions.clone()),
            ("rules", p.instructions),
        ];
        if let Some(pr) = p.prompts {
            out.push(("commands", pr));
        }
        out.push(("skills", p.skills));
        out.push(("agents", p.agents));
        out.push(("settings", p.config));
        out
    }

    fn detect(&self, env: &Env) -> bool {
        match env.scope {
            Scope::User => codex_home(env).exists(),
            Scope::Project => env.project.join(".codex").exists(),
        }
    }

    fn capture(&self, env: &Env) -> Result<Captured> {
        let mut cap = Captured::for_env(env);
        let p = paths(env);
        if let Some(text) = cap.read_config(&p.config, ID)? {
            cap.sources.push(p.config.clone());
            let doc = parse(Some(&text), &p.config)?;
            let servers = doc.get("mcp_servers").map(item_to_json);
            for (name, def) in
                server_entries(&mut cap, ID, &p.config, "mcp_servers", servers.as_ref())
            {
                import(&mut cap, name, def);
            }
            let plugin_tables = Value::Object(
                ["marketplaces", "plugins"]
                    .iter()
                    .filter_map(|k| doc.get(k).map(|i| ((*k).to_string(), item_to_json(i))))
                    .collect(),
            );
            crate::plugins::capture_codex(&mut cap, &plugin_tables, "config.toml");
            let mut settings = Map::new();
            for k in policy::allowed_keys(ID) {
                if let Some(item) = doc.get(k) {
                    settings.insert((*k).to_string(), item_to_json(item));
                }
            }
            if !settings.is_empty() {
                cap.profile.settings.insert(ID.to_string(), settings);
            }
        }
        if env.scope == Scope::User && codex_home(env).join("AGENTS.override.md").exists() {
            cap.warn(ID, "AGENTS.override.md exists in CODEX_HOME and takes precedence over AGENTS.md; captured AGENTS.md only");
        }
        capture_instructions(&mut cap, &p.instructions, ID)?;
        if let Some(prompts) = &p.prompts {
            capture_markdown_dir(&mut cap, prompts, ".md", MdKind::Command, ID)?;
        }
        capture_skills_dir(&mut cap, &p.skills, ID)?;
        if env.scope == Scope::User {
            // Older Codex releases read skills from $CODEX_HOME/skills.
            capture_skills_dir(&mut cap, &codex_home(env).join("skills"), ID)?;
        }
        capture_agents(&mut cap, &p.agents)?;
        super::hook_files::capture_for(&mut cap, env, &super::hook_files::CODEX)?;
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

        let original = work.fs.read(&p.config)?;
        let mut doc = parse(original.as_deref(), &p.config)?;
        let mut changed = false;

        let wanted: Vec<(String, Map<String, Value>)> = servers_for(profile, ID, work, true)
            .into_iter()
            .filter_map(|(name, s)| {
                codex_server(name, s, env, opts, work).map(|t| (name.clone(), t))
            })
            .collect();
        // Servers the profile disables here, turned off if kitbag added them.
        for name in skipped_servers(work, ID) {
            let cur = doc
                .get("mcp_servers")
                .and_then(|t| t.get(&name))
                .map(item_to_json);
            if off_skipped_server(
                work,
                opts,
                ID,
                &p.config,
                &name,
                cur.as_ref(),
                (&set_enabled_false, "enabled = false"),
            )
            .is_some()
                && let Some(t) = doc
                    .get_mut("mcp_servers")
                    .and_then(|t| t.get_mut(&name))
                    .and_then(Item::as_table_like_mut)
            {
                t.insert("enabled", toml_edit::value(false));
                changed = true;
            }
        }
        if !wanted.is_empty() {
            let servers = table_mut(&mut doc, &["mcp_servers"])?;
            for (name, want) in wanted {
                let cur = servers.get(&name).map(item_to_json);
                let cur_obj = cur.as_ref().and_then(Value::as_object);
                let Some(merged) = merge_server_def(cur_obj, &want, MANAGED_KEYS) else {
                    continue;
                };
                if cur.is_some() && !opts.overwrite {
                    work.warn(ID, format!("kept existing MCP server '{name}' in {} because --no-overwrite was given", p.config.display()));
                    continue;
                }
                super::common::note_dropped_server_keys(
                    work,
                    ID,
                    &name,
                    &p.config,
                    &merged.dropped,
                );
                match servers.get_mut(&name).and_then(Item::as_table_mut) {
                    // Update in place so the table keeps its position and comments.
                    Some(t) => {
                        for k in MANAGED_KEYS
                            .iter()
                            .copied()
                            .chain(merged.dropped.iter().map(String::as_str))
                        {
                            if !want.contains_key(k) {
                                t.remove(k);
                            }
                        }
                        for (k, v) in &want {
                            if let Some(val) = json_to_value(v)
                                && t.get(k).map(item_to_json).as_ref() != Some(v)
                            {
                                set_keep_decor(t, k, val);
                            }
                        }
                    }
                    None => {
                        servers.insert(&name, Item::Table(json_to_table(&merged.def)));
                    }
                }
                let written = servers.get(&name).map(item_to_json);
                record_server_write(work, ID, &p.config, &name, written.as_ref());
                work.changed_servers.push((ID.to_string(), name));
                changed = true;
            }
        }

        for (k, v) in safe_settings(profile, ID, work) {
            if env.scope == Scope::Project && USER_ONLY_KEYS.contains(&k.as_str()) {
                work.warn(
                    ID,
                    format!("setting '{k}' is ignored by Codex in project config; skipped"),
                );
                continue;
            }
            let cur = doc.get(&k).map(item_to_json);
            if cur.as_ref() == Some(&v) {
                continue;
            }
            if cur.is_some() && !opts.overwrite {
                work.warn(
                    ID,
                    format!("kept existing setting '{k}' because --no-overwrite was given"),
                );
                continue;
            }
            if let Some(val) = json_to_value(&v) {
                set_keep_decor(doc.as_table_mut(), &k, val);
                changed = true;
            }
        }
        changed |= crate::plugins::apply_codex(profile, env, opts, work, &mut doc, &p.config)?;
        if changed {
            work.fs.write(
                &p.config,
                keep_line_endings(original.as_deref(), doc.to_string()),
            )?;
        }
        if env.scope == Scope::Project && !profile.mcp_servers.is_empty() {
            work.info(
                ID,
                "Codex only loads .codex/config.toml in projects you have marked as trusted",
            );
        }

        write_instructions(work, &p.instructions, profile, opts, true, ID)?;
        if profile.rules.iter().any(|r| !r.globs.is_empty()) {
            work.info(ID, "Codex has no file-scoped rules; rules were folded into AGENTS.md as labelled sections");
        }
        if let Some(text) = work.fs.read(&p.instructions)?
            && text.len() > AGENTS_MD_LIMIT
        {
            work.warn(
                ID,
                format!(
                    "{} is {} KiB; Codex reads only the first 32 KiB by default (raise project_doc_max_bytes)",
                    p.instructions.display(),
                    text.len() / 1024
                ),
            );
        }
        if env.scope == Scope::User && codex_home(env).join("AGENTS.override.md").exists() {
            work.warn(
                ID,
                "AGENTS.override.md exists in CODEX_HOME and takes precedence over AGENTS.md",
            );
        }

        if !profile.commands.is_empty() {
            match &p.prompts {
                Some(dir) => {
                    for c in &profile.commands {
                        let path = dir.join(format!("{}.md", c.name));
                        let body = command_to_markdown(c, false);
                        super::common::write_command(work, &path, body.as_bytes(), opts, ID, &c.name)?;
                    }
                    work.info(ID, "commands were written as Codex custom prompts (/prompts:<name>); Codex now recommends skills for reusable workflows");
                }
                None => work.info(
                    ID,
                    format!(
                        "Codex has no project-scoped custom prompts; {} command(s) skipped (apply with --scope user)",
                        profile.commands.len()
                    ),
                ),
            }
        }
        for s in &profile.skills {
            let s = skill_with_required_frontmatter(s);
            write_skill(work, &p.skills.join(&s.name), &s, opts, ID)?;
        }
        for a in &profile.agents {
            apply_agent(work, &p.agents, a, opts)?;
        }
        if env.scope == Scope::Project && !profile.agents.is_empty() {
            work.info(
                ID,
                "Codex's docs don't say whether .codex/agents/ is read only in a trusted project, as .codex/config.toml is",
            );
        }
        super::hook_files::apply_for(profile, env, opts, work, &super::hook_files::CODEX)?;
        Ok(())
    }
}
