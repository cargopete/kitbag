//! Google Antigravity (app, IDE and CLI): MCP servers, instructions, rules,
//! skills and subagents.
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
//! * Rules: `~/.gemini/config/rules/*.md` (user) and `.agents/rules/*.md`
//!   (workspace), directly in that directory. Each file needs YAML frontmatter
//!   with a `trigger` (`always_on`, `glob`, `model_decision` or `manual`), or
//!   Antigravity silently discards it; `glob` takes `globs`, one string of
//!   comma-separated patterns, and `model_decision` needs a `description`. A
//!   file over 24,000 bytes is truncated. Gemini CLI folds its rules into
//!   `~/.gemini/GEMINI.md`, which Antigravity reads too, so with both targets
//!   at user scope a rule is seen twice (always on, from the block, and
//!   by its own trigger, from the file); kitbag says so in the plan.
//! * Skills: `~/.gemini/config/skills/<name>/SKILL.md` (user) and
//!   `.agents/skills/<name>/SKILL.md` (workspace, shared with Codex, which
//!   writes the same files). `name` defaults to the folder name and
//!   `description` is required. The CLI also reads `~/.gemini/skills/`, which
//!   is Gemini CLI's directory, so with both targets the CLI sees a skill
//!   from each; the app reads only `~/.gemini/config/skills/`.
//! * Commands: workflows (`.agents/workflows/<name>.md`) are deprecated by
//!   Google in favour of skills, and its docs give two global directories
//!   (`~/.gemini/config/global_workflows/` and `.../workflows/`), so kitbag
//!   writes no workflows. Its migration guide (workflows-to-skills) turns a
//!   workflow into `skills/<name>/SKILL.md` with `name` and `description`
//!   and the body kept, invoked as `/<name>` as before, and kitbag writes a
//!   command the same way (as Amp does). Hooks are carried by `hook_files`;
//!   plugins aren't.
//! * Subagents: `~/.gemini/config/agents/<name>.md` (user) and
//!   `.agents/agents/<name>.md` (workspace); Antigravity also reads
//!   `<name>/agent.md` in those directories, which capture reads too, and
//!   kitbag writes `<name>.md`. Markdown with YAML frontmatter: `name` and
//!   `description` required, then `tools` (a list of exact tool names; the
//!   docs warn that an unknown name may make the subagent hang), `mainAgent`,
//!   `subagent`, `model` (`inherit`, `flash` or `pro`),
//!   `commandExecutionPolicy` (`off`, `auto`, `eager` or `sandbox`),
//!   `mcpServers`, `skills` and `plugins`; the body is the system prompt.
//!   Docs: <https://antigravity.google/docs/subagents>.
//!
//! Docs: <https://antigravity.google/docs/mcp>, <https://antigravity.google/docs/rules>,
//! <https://antigravity.google/docs/skills>,
//! <https://antigravity.google/docs/migration/workflows-to-skills/>.

use std::collections::BTreeMap;
use std::path::PathBuf;

use anyhow::Result;
use serde_json::{Map, Value, json};

use super::common::{
    AgentName, Resolver, add_rule, capture_agent_dir, capture_instructions, capture_skills_dir,
    claim_name, commands_as_skills, documented_agent, import_server, merge_json_servers_off,
    name_from_rel, note_agent_keys, read_text_item, server_entries, server_from_json, servers_for,
    set_disabled_true, skill_with_required_frontmatter, str_of, write_agent, write_instructions,
    write_rule, write_skill,
};
use super::{Adapter, ApplyOptions, Captured, Env, Feature, Scope, Support, Work};
use crate::edit::json::parse_value;
use crate::frontmatter::{self, glob_list, opt};
use crate::model::{Agent, Profile, Rule, RuleMode, Transport, is_valid_name};
use crate::plan_fs::{join_rel, walk_files};
use crate::profile_io::rule_from_markdown;

const ID: &str = "antigravity";
const TOOL: &str = "Antigravity";
/// Antigravity truncates a rule file over this many bytes.
const RULE_MAX_BYTES: usize = 24_000;
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
    rules: PathBuf,
    skills: PathBuf,
    agents: PathBuf,
}

/// Frontmatter keys Antigravity's subagent docs list besides `name` and `description`.
const AGENT_KEYS: &[&str] = &[
    "tools",
    "mainAgent",
    "subagent",
    "model",
    "commandExecutionPolicy",
    "mcpServers",
    "skills",
    "plugins",
];

fn string_array(v: &Value) -> bool {
    v.as_array().is_some_and(|a| a.iter().all(Value::is_string))
}

/// A subagent as an Antigravity file: the documented keys only, each with
/// the type or the values the docs give it (`model` is a tier, not a model id).
pub fn antigravity_agent(a: &Agent) -> (String, Vec<String>) {
    documented_agent(a, AgentName::Required, AGENT_KEYS, |k, v| match k {
        "tools" | "skills" | "plugins" => string_array(v),
        "mainAgent" | "subagent" => v.is_boolean(),
        "model" => matches!(v.as_str(), Some("inherit" | "flash" | "pro")),
        "commandExecutionPolicy" => {
            matches!(v.as_str(), Some("off" | "auto" | "eager" | "sandbox"))
        }
        _ => v.is_array(),
    })
}

fn apply_agent(
    work: &mut Work,
    dir: &std::path::Path,
    a: &Agent,
    opts: &ApplyOptions,
) -> Result<()> {
    if a.description.is_none() {
        work.warn(
            ID,
            format!(
                "agent '{}' skipped: Antigravity requires a description, and kitbag does not make one up",
                a.name
            ),
        );
        return Ok(());
    }
    let (text, dropped) = antigravity_agent(a);
    note_agent_keys(work, ID, TOOL, a, &dropped);
    if a.extra.contains_key("tools") && !dropped.iter().any(|d| d == "tools") {
        work.warn(
            ID,
            format!(
                "agent '{}': Antigravity warns that a tool name it doesn't know may make the subagent hang; check the tools list against its tool names",
                a.name
            ),
        );
    }
    let path = dir.join(format!("{}.md", a.name));
    write_agent(work, &path, text.as_bytes(), opts, ID, &a.name)
}

/// A rule as an Antigravity rule file. `globs` is one string of
/// comma-separated patterns; `model_decision` needs a description, so an
/// on-demand rule without one becomes `manual` (loaded by @-mention only).
pub fn antigravity_rule(r: &Rule) -> String {
    let (trigger, globs) = match r.mode() {
        RuleMode::Always => ("always_on", Value::Null),
        RuleMode::Globs => ("glob", Value::String(r.globs.join(", "))),
        RuleMode::OnDemand if r.description.is_some() => ("model_decision", Value::Null),
        RuleMode::OnDemand => ("manual", Value::Null),
    };
    frontmatter::render(
        &[
            ("trigger", json!(trigger)),
            ("description", opt(&r.description)),
            ("globs", globs),
        ],
        &r.body,
    )
}

/// An Antigravity rule file -> profile rule, or why it isn't one. A file
/// without a valid `trigger` is discarded by Antigravity, and so by capture.
pub fn rule_from_antigravity(name: &str, text: &str) -> Result<Rule, String> {
    let doc = frontmatter::parse(text);
    let d = &doc.data;
    let mut r = rule_from_markdown(name, text);
    r.globs = Vec::new();
    r.always_apply = None;
    match d.get("trigger").and_then(Value::as_str) {
        Some("always_on") => {}
        Some("glob") => {
            r.globs = ["globs", "glob"]
                .iter()
                .map(|k| glob_list(d.get(*k)))
                .find(|g| !g.is_empty())
                .unwrap_or_default();
            if r.globs.is_empty() {
                return Err("trigger: glob without globs, which Antigravity discards".into());
            }
        }
        Some("model_decision" | "manual") => r.always_apply = Some(false),
        Some(other) => {
            return Err(format!(
                "unknown trigger '{other}', which Antigravity discards"
            ));
        }
        None => return Err("no trigger in its frontmatter, so Antigravity discards it".into()),
    }
    Ok(r)
}

fn paths(env: &Env) -> Paths {
    match env.scope {
        Scope::User => {
            let g = env.home.join(".gemini");
            let c = g.join("config");
            Paths {
                mcp: c.join("mcp_config.json"),
                rules: c.join("rules"),
                skills: c.join("skills"),
                agents: c.join("agents"),
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
                rules: a.join("rules"),
                skills: a.join("skills"),
                agents: a.join("agents"),
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
    fn support(&self, _scope: Scope) -> [(Feature, Support); 8] {
        Feature::ALL.map(|f| {
            (
                f,
                if matches!(
                    f,
                    Feature::Mcp
                        | Feature::Instructions
                        | Feature::Rules
                        | Feature::Skills
                        | Feature::Agents
                ) {
                    Support::Native
                } else if f == Feature::Commands {
                    // Workflows are deprecated; Google's migration guide turns
                    // each into a skill, so that is what is written.
                    Support::Folded
                } else {
                    Support::None
                },
            )
        })
    }

    fn locations(&self, env: &Env) -> Vec<(&'static str, PathBuf)> {
        let p = paths(env);
        vec![
            ("mcp", p.mcp),
            ("instructions", p.instructions),
            ("rules", p.rules),
            ("skills", p.skills),
            ("agents", p.agents),
        ]
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
        // Antigravity's own rule files first, so a rule that is also folded
        // into a shared instructions file keeps its file's trigger.
        if !cap.escapes(&p.rules, ID) {
            let mut seen = BTreeMap::new();
            for rel in walk_files(&p.rules, 500)? {
                // Only `rules/*.md` directly in the directory is documented.
                if rel.contains('/') || !rel.ends_with(".md") {
                    continue;
                }
                let path = join_rel(&p.rules, &rel);
                let name = name_from_rel(&rel, ".md");
                claim_name(&mut seen, &p.rules, &rel, &name)?;
                let Some(text) = read_text_item(&mut cap, &path, ID) else {
                    continue;
                };
                if !is_valid_name(&name) {
                    cap.warn(
                        ID,
                        format!("skipped {}: unsupported file name", path.display()),
                    );
                    continue;
                }
                match rule_from_antigravity(&name, &text) {
                    Ok(r) => {
                        cap.sources.push(path.clone());
                        cap.rule_files.push((name, path));
                        add_rule(&mut cap, r, ID);
                    }
                    Err(why) => cap.warn(ID, format!("skipped {}: {why}", path.display())),
                }
            }
        }
        let own = cap.profile.rules.len();
        for path in &p.read {
            capture_instructions(&mut cap, path, ID)?;
        }
        // Rules folded into a shared file by another target (Gemini CLI's
        // GEMINI.md, Codex's AGENTS.md) come back out of capture_instructions.
        // One that is also a file here was kept above, as the file's version.
        if cap.profile.rules.len() > own {
            let names: Vec<String> = cap
                .profile
                .rules
                .split_off(own)
                .into_iter()
                .map(|r| r.name)
                .collect();
            cap.info(
                ID,
                format!(
                    "rules folded into Antigravity's instructions files by another tool were not captured ({}); capture that tool for them",
                    names.join(", ")
                ),
            );
        }
        capture_skills_dir(&mut cap, &p.skills, ID)?;
        capture_agent_dir(&mut cap, &p.agents, Some("agent.md"), ID, "a subagent")?;
        super::hook_files::capture_for(&mut cap, env, &super::hook_files::ANTIGRAVITY)?;
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
        merge_json_servers_off(
            work,
            (&p.mcp, &["mcpServers"]),
            &servers,
            MANAGED_KEYS,
            opts,
            ID,
            Some((&set_disabled_true, "disabled: true")),
        )?;
        // Same file and block id as Gemini CLI at user scope: applying both
        // plans the same block twice, which leaves one copy.
        write_instructions(work, &p.instructions, profile, opts, false, ID)?;
        for r in &profile.rules {
            let text = antigravity_rule(r);
            let path = p.rules.join(format!("{}.md", r.name));
            if r.mode() == RuleMode::OnDemand && r.description.is_none() {
                work.info(
                    ID,
                    format!(
                        "rule '{}' is on-demand with no description; Antigravity can only load it when you @-mention it (and not in the CLI), so it is written with trigger: manual",
                        r.name
                    ),
                );
            }
            if r.mode() == RuleMode::Globs && r.globs.iter().any(|g| g.contains(',')) {
                work.warn(
                    ID,
                    format!(
                        "rule '{}': Antigravity splits globs at commas, so a pattern with a comma in it ({}) won't match as written",
                        r.name,
                        r.globs.join(" ")
                    ),
                );
            }
            if text.len() > RULE_MAX_BYTES {
                work.warn(
                    ID,
                    format!(
                        "rule '{}' is {} bytes; Antigravity reads only the first {RULE_MAX_BYTES} bytes of a rule file",
                        r.name,
                        text.len()
                    ),
                );
            }
            write_rule(work, &path, text.as_bytes(), opts, ID, &r.name)?;
        }
        if env.scope == Scope::User
            && !profile.rules.is_empty()
            && work.targets.iter().any(|t| t == "gemini-cli")
        {
            work.info(
                ID,
                "Antigravity reads Gemini CLI's rules block in ~/.gemini/GEMINI.md as well as its own rules/ files, so in this setup it sees each rule twice: always on from the block, and with its own trigger from the file",
            );
        }
        let skills = commands_as_skills(
            work,
            ID,
            "Antigravity",
            &profile.skills,
            &profile.commands,
            false,
        );
        if !profile.commands.is_empty() {
            work.info(
                ID,
                format!(
                    "Antigravity deprecated workflows in favour of skills, so commands are written as skills in {} (as its workflows-to-skills migration guide does); no workflows folder is written",
                    crate::ui::tildify(&p.skills, &work.home)
                ),
            );
        }
        for s in &skills {
            let s = skill_with_required_frontmatter(s);
            write_skill(work, &p.skills.join(&s.name), &s, opts, ID)?;
        }
        for a in &profile.agents {
            apply_agent(work, &p.agents, a, opts)?;
        }
        if env.scope == Scope::User
            && !profile.skills.is_empty()
            && work.targets.iter().any(|t| t == "gemini-cli")
        {
            work.info(
                ID,
                "skills go to ~/.gemini/config/skills/ for Antigravity and ~/.gemini/skills/ for Gemini CLI; the Antigravity CLI reads both directories, the app and IDE only the first",
            );
        }
        if profile.settings.contains_key(ID) {
            work.info(
                ID,
                "kitbag doesn't manage Antigravity settings; settings.antigravity was not applied",
            );
        }
        super::hook_files::apply_for(profile, env, opts, work, &super::hook_files::ANTIGRAVITY)?;
        Ok(())
    }
}
