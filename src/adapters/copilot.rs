//! GitHub Copilot (Copilot CLI, and the Copilot agent in VS Code): MCP servers,
//! instructions, path-specific instructions (rules), prompt files (commands)
//! and skills.
//!
//! * MCP: Copilot's native files use a top-level `mcpServers` object:
//!   `$COPILOT_HOME/mcp-config.json` (default `~/.copilot/mcp-config.json`) for
//!   the user, and `.mcp.json` at the workspace root for a project. Copilot CLI
//!   reads both; VS Code reads the user file and an existing workspace
//!   `.mcp.json` (the experimental `chat.mcp.workspaceRootConfig.enabled`
//!   setting only controls whether Add Server offers that file).
//!   Local servers take `type` (`local` or `stdio`, default `local`),
//!   `command`, `args`, `env` and `cwd`;
//!   remote servers `type` (`http` or `sse`), `url` and `headers`. Copilot
//!   CLI's reference marks `tools` required (`["*"]` for all), so the user file
//!   always gets one. References are written as `${VAR}` and `${VAR:-default}`,
//!   which Copilot expands itself, so no secret reaches disk. The CLI reference
//!   documents `$VAR`, `${VAR}` and `${VAR:-default}` in `env` and expansion in
//!   `headers`; VS Code's lists `command`, `args`, `env` and `headers`. Neither
//!   says references expand in `url` or `cwd`, so kitbag warns there.
//!   The project `.mcp.json` is the same file Claude Code reads, so kitbag writes
//!   it with Claude Code's server shape (no `tools`, no `cwd`), and neither
//!   target removes a server there that the other still gets.
//!   Capture also reads `.github/mcp.json` (Copilot CLI), the bare top-level
//!   format Copilot CLI accepts in project files, and VS Code's deprecated
//!   editor-managed `.vscode/mcp.json` (`servers`, `${env:VAR}`), which Copilot
//!   CLI doesn't read; servers in `disabledMcpServers` (`~/.copilot/settings.json`
//!   or `.github/copilot/settings.json`) are captured as disabled.
//! * Instructions: `$COPILOT_HOME/copilot-instructions.md` and
//!   `.github/copilot-instructions.md`, in a managed block.
//! * Rules: `*.instructions.md` files with `applyTo` globs, in
//!   `$COPILOT_HOME/instructions/` and `.github/instructions/`. A rule that is
//!   always on gets `applyTo: "**"`; an on-demand rule gets no `applyTo`.
//! * Commands: VS Code prompt files, `.github/prompts/<name>.prompt.md`, at
//!   project scope. VS Code keeps user prompt files in its profile storage,
//!   whose path its docs don't give, and Copilot CLI's docs don't list prompt
//!   files among what it reads; VS Code's migration guide (migrate-
//!   customizations) turns prompt files into skills (`name`, `description`,
//!   `argument-hint` and `disable-model-invocation: true`, dropping `agent`,
//!   `model` and `tools`), so at user scope a command is written that way, as
//!   `$COPILOT_HOME/skills/<name>/SKILL.md`.
//! * Skills: `$COPILOT_HOME/skills/<name>/SKILL.md` and
//!   `.github/skills/<name>/SKILL.md`, the locations both Copilot CLI and
//!   VS Code list for Copilot.
//! * Custom agents: `$COPILOT_HOME/agents/<name>.agent.md` (default
//!   `~/.copilot/agents/`) and `.github/agents/<name>.agent.md`, where both
//!   Copilot CLI and VS Code look; both also take a plain `<name>.md`, which
//!   capture reads and apply keeps writing to if it is already there. Chat
//!   modes are the old name of custom agents: capture reads
//!   `.github/chatmodes/*.chatmode.md` (and a `*.chatmode.md` left among the
//!   agents) as agents, and apply writes them as `.agent.md` files, as VS Code
//!   says to rename them. Frontmatter is kept as it is.
//!
//! Docs: <https://code.visualstudio.com/docs/agents/reference/mcp-configuration>,
//! <https://docs.github.com/en/copilot/reference/copilot-cli-reference/cli-command-reference>,
//! <https://docs.github.com/en/copilot/how-tos/copilot-cli/customize-copilot/add-mcp-servers>,
//! <https://docs.github.com/en/copilot/how-tos/copilot-cli/customize-copilot/add-custom-instructions>,
//! <https://docs.github.com/en/copilot/reference/copilot-cli-reference/cli-config-dir-reference>,
//! <https://code.visualstudio.com/docs/agent-customization/prompt-files>,
//! <https://code.visualstudio.com/docs/agent-customization/agent-skills>,
//! <https://code.visualstudio.com/docs/agents/reference/custom-agent-file-reference>,
//! <https://docs.github.com/en/copilot/how-tos/copilot-cli/customize-copilot/create-custom-agents-for-cli>.

use std::path::{Path, PathBuf};

use anyhow::Result;
use serde_json::{Map, Value, json};

use super::claude_code;
use super::common::{
    MdKind, add_agent, add_rule, capture_instructions, capture_markdown_dir, capture_skills_dir,
    claim_name, commands_as_skills, import_server, keep_shared_servers, merge_json_servers,
    name_from_rel, read_text_item, render_server, same_server, server_entries, server_from_json,
    servers_for, skill_with_required_frontmatter, string_list, write_agent, write_command,
    write_instructions, write_rule, write_skill,
};
use super::{Adapter, ApplyOptions, Captured, Env, Feature, Scope, Support, Work};
use crate::edit::json::parse_value;
use crate::frontmatter::{self, opt};
use crate::model::{Agent, Command, Profile, Rule, RuleMode, Skill, Transport, is_valid_name};
use crate::plan_fs::{join_rel, walk_files};
use crate::profile_io::{agent_from_markdown, agent_to_markdown, rule_from_markdown};
use crate::secrets::{RefStyle, has_ref};

const ID: &str = "copilot";
/// Server keys kitbag writes in the user file. `tools` is not one of them: a
/// tool list you chose is kept, and `["*"]` is only added when there is none.
const MANAGED_KEYS: &[&str] = &["type", "command", "args", "env", "cwd", "url", "headers"];
const RULE_EXT: &str = ".instructions.md";
const PROMPT_EXT: &str = ".prompt.md";
const AGENT_EXT: &str = ".agent.md";
/// The old name and extension of custom agents.
const CHATMODE_EXT: &str = ".chatmode.md";
/// Prompt-file frontmatter keys VS Code lists besides `description` and
/// `argument-hint`; other keys a command carries (Claude Code's
/// `allowed-tools` ...) mean nothing in a prompt file and are left out.
const PROMPT_KEYS: &[&str] = &["name", "agent", "model", "tools"];

pub struct Copilot;

struct Paths {
    mcp: PathBuf,
    instructions: PathBuf,
    rules: PathBuf,
    skills: PathBuf,
    agents: PathBuf,
    /// VS Code prompt files; project scope only.
    prompts: Option<PathBuf>,
    /// Copilot CLI settings, for `disabledMcpServers`.
    settings: PathBuf,
}

fn copilot_home(env: &Env) -> PathBuf {
    env.location_var("COPILOT_HOME")
        .unwrap_or_else(|| env.home.join(".copilot"))
}

fn paths(env: &Env) -> Paths {
    match env.scope {
        Scope::User => {
            let d = copilot_home(env);
            Paths {
                mcp: d.join("mcp-config.json"),
                instructions: d.join("copilot-instructions.md"),
                rules: d.join("instructions"),
                skills: d.join("skills"),
                agents: d.join("agents"),
                prompts: None,
                settings: d.join("settings.json"),
            }
        }
        Scope::Project => {
            let g = env.project.join(".github");
            Paths {
                mcp: env.project.join(".mcp.json"),
                instructions: g.join("copilot-instructions.md"),
                rules: g.join("instructions"),
                skills: g.join("skills"),
                agents: g.join("agents"),
                prompts: Some(g.join("prompts")),
                settings: g.join("copilot").join("settings.json"),
            }
        }
    }
}

/// A rule as a Copilot `.instructions.md` file. `applyTo` takes one string of
/// comma-separated globs; `"**"` matches every file.
pub fn copilot_rule(r: &Rule) -> String {
    let apply_to = match r.mode() {
        RuleMode::Globs => Value::String(r.globs.join(",")),
        RuleMode::Always => Value::String("**".into()),
        RuleMode::OnDemand => Value::Null,
    };
    frontmatter::render(
        &[("description", opt(&r.description)), ("applyTo", apply_to)],
        &r.body,
    )
}

/// A Copilot `.instructions.md` file -> profile rule. `applyTo: "**"` is
/// always on; a file without `applyTo` is only used when attached, which is
/// an on-demand rule.
pub fn rule_from_instructions(name: &str, text: &str) -> Rule {
    let mut r = rule_from_markdown(name, text);
    if !r.globs.is_empty() && r.globs.iter().all(|g| g == "**" || g == "**/*") {
        r.globs.clear();
        r.always_apply = None;
    } else if r.globs.is_empty() && r.always_apply.is_none() {
        r.always_apply = Some(false);
    }
    r
}

/// A command as a VS Code prompt file: `description`, `argument-hint`, and
/// the prompt-file keys among the command's own (`name`, `agent`, `model`,
/// `tools`). Returns the file and the keys left out.
pub fn prompt_file(c: &Command) -> (String, Vec<String>) {
    let mut entries: Vec<(&str, Value)> = vec![
        ("description", opt(&c.description)),
        ("argument-hint", opt(&c.argument_hint)),
    ];
    let mut dropped = Vec::new();
    for (k, v) in &c.extra {
        if PROMPT_KEYS.contains(&k.as_str()) {
            entries.push((k.as_str(), v.clone()));
        } else {
            dropped.push(k.clone());
        }
    }
    (frontmatter::render(&entries, &c.body), dropped)
}

/// Why VS Code would skip a skill: its docs want a `name` of lowercase
/// letters, digits and hyphens (at most 64), the same as its directory.
fn skill_name_issue(s: &Skill) -> Option<String> {
    let ok = |n: &str| {
        !n.is_empty()
            && n.len() <= 64
            && n.chars()
                .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
    };
    if !ok(&s.name) {
        return Some(format!(
            "skill '{}': VS Code only loads skills named with lowercase letters, digits and hyphens (at most 64), so it may skip this one; Copilot CLI accepts it",
            s.name
        ));
    }
    let main = s.main_file()?;
    let doc = frontmatter::parse(&String::from_utf8_lossy(&main.content));
    match doc.data.get("name").and_then(Value::as_str) {
        Some(n) if n != s.name => Some(format!(
            "skill '{}': SKILL.md says name '{n}', but VS Code wants the name to match the directory, so it may skip it",
            s.name
        )),
        _ => None,
    }
}

/// The servers of a project MCP file: its `mcpServers` object, or, in Copilot
/// CLI's bare format, the top level itself.
fn project_servers(doc: &Value) -> Option<&Value> {
    if let Some(s) = doc.get("mcpServers") {
        return Some(s);
    }
    is_bare(doc).then_some(doc)
}

/// Is `doc` a non-empty object whose every value looks like a server entry?
fn is_bare(doc: &Value) -> bool {
    let Value::Object(m) = doc else { return false };
    !m.is_empty()
        && m.values().all(|v| {
            v.as_object()
                .is_some_and(|d| d.contains_key("command") || d.contains_key("url"))
        })
}

/// Import one server entry from a Copilot MCP file.
fn import(
    cap: &mut Captured,
    name: &str,
    def: &Map<String, Value>,
    file: &Path,
    bare_dollar: bool,
) {
    let mut s = server_from_json(def);
    match def.get("type").and_then(Value::as_str) {
        None | Some("local" | "stdio" | "http" | "streamable-http") => {}
        Some("sse") => s.transport = Some(Transport::Sse),
        Some(other) => {
            cap.warn(
                ID,
                format!(
                    "skipped MCP server '{name}' in {}: unknown type '{other}'",
                    file.display()
                ),
            );
            return;
        }
    }
    let tools = string_list(def.get("tools"));
    let mut left: Vec<&str> = Vec::new();
    if def.contains_key("tools") && tools != ["*"] {
        left.push("tools");
    }
    left.extend(
        [
            "oauthClientId",
            "oauthScopes",
            "oauthPublicClient",
            "oauthGrantType",
            "oidc",
        ]
        .into_iter()
        .filter(|k| def.contains_key(*k)),
    );
    if !left.is_empty() {
        cap.info(
            ID,
            format!(
                "MCP server '{name}': {} not captured (Copilot-specific)",
                left.join(", ")
            ),
        );
    }
    import_server(cap, ID, name, s, bare_dollar);
}

/// Import one server from VS Code's older editor-managed `.vscode/mcp.json`.
fn import_vscode(cap: &mut Captured, name: &str, def: &Map<String, Value>, file: &Path) {
    let blob = Value::Object(def.clone()).to_string();
    if blob.contains("${input:") || blob.contains("${command:") {
        cap.warn(
            ID,
            format!(
                "skipped MCP server '{name}' in {}: it uses VS Code ${{input:...}} or ${{command:...}} variables, which only VS Code can fill in (Copilot doesn't take such servers either); use env references instead",
                file.display()
            ),
        );
        return;
    }
    if def.contains_key("envFile") {
        cap.warn(
            ID,
            format!("MCP server '{name}': envFile is VS Code-specific and was not captured; list the variables in 'env' instead"),
        );
    }
    if blob.contains("${workspaceFolder") || blob.contains("${userHome}") {
        cap.warn(
            ID,
            format!("MCP server '{name}' uses VS Code variables (${{workspaceFolder}}, ${{userHome}}); other tools won't expand them"),
        );
    }
    // VS Code treats a bare `$NAME` as literal text.
    import(cap, name, def, file, false);
}

/// The agent name of a file in an agents or chat-modes directory, the path
/// without its extension, and whether it is a legacy chat mode:
/// `x.agent.md`, `x.chatmode.md`, `x.md`.
fn agent_name(rel: &str) -> Option<(String, String, bool)> {
    let (ext, mode) = if rel.ends_with(AGENT_EXT) {
        (AGENT_EXT, false)
    } else if rel.ends_with(CHATMODE_EXT) {
        (CHATMODE_EXT, true)
    } else if rel.ends_with(".md") {
        (".md", false)
    } else {
        return None;
    };
    let stem = rel.strip_suffix(ext).unwrap_or(rel).to_string();
    Some((name_from_rel(rel, ext), stem, mode))
}

/// Markdown files in an agents directory that aren't agents: other Copilot
/// file types, and a README.
fn not_an_agent(rel: &str) -> Option<&'static str> {
    let file = rel.rsplit('/').next().unwrap_or(rel);
    if file.ends_with(".prompt.md") {
        Some("a prompt file, not an agent")
    } else if file.ends_with(".instructions.md") {
        Some("an instructions file, not an agent")
    } else if file.eq_ignore_ascii_case("readme.md") {
        Some("a README, not an agent")
    } else {
        None
    }
}

/// Capture the custom agents in `dir` (`.agent.md` before `.md` and
/// `.chatmode.md` of the same name); `legacy` is the old chat-modes
/// directory, where only `.chatmode.md` files count. Two different files that
/// flatten to the same name (`team/x.agent.md`, `team-x.agent.md`) are an
/// error, as for prompts and rules. Returns the legacy chat modes it captured.
fn capture_agents(cap: &mut Captured, dir: &Path, legacy: bool) -> Result<Vec<String>> {
    let mut modes = Vec::new();
    if cap.escapes(dir, ID) {
        return Ok(modes);
    }
    let mut files: Vec<(String, String, String, bool)> = Vec::new();
    for rel in walk_files(dir, 500)? {
        let Some((name, stem, mode)) = agent_name(&rel) else {
            continue;
        };
        if legacy && !mode {
            continue;
        }
        if let Some(why) = not_an_agent(&rel) {
            cap.warn(
                ID,
                format!("skipped {}: {why}", join_rel(dir, &rel).display()),
            );
            continue;
        }
        files.push((rel, name, stem, mode));
    }
    // `.agent.md` first, then plain `.md`, then chat modes.
    files.sort_by_key(|(rel, _, _, mode)| (*mode, !rel.ends_with(AGENT_EXT), rel.clone()));
    // Name -> the file claiming it, and the path without its extension, so
    // `x.agent.md` and `x.md` are one agent and `a/x.md`, `a-x.md` a clash.
    let mut seen = std::collections::BTreeMap::new();
    let mut stems = std::collections::BTreeMap::new();
    for (rel, name, stem, mode) in files {
        let path = join_rel(dir, &rel);
        if !is_valid_name(&name) {
            cap.warn(
                ID,
                format!("skipped {}: unsupported file name", path.display()),
            );
            continue;
        }
        if stems.get(&name) != Some(&stem) {
            claim_name(&mut seen, dir, &rel, &name)?;
            stems.insert(name.clone(), stem);
        }
        if cap.profile.agents.iter().any(|a| a.name == name) {
            cap.info(
                ID,
                format!(
                    "skipped {}: an agent named '{name}' was already captured",
                    path.display()
                ),
            );
            continue;
        }
        let Some(text) = read_text_item(cap, &path, ID) else {
            continue;
        };
        if add_agent(cap, agent_from_markdown(&name, &text), ID) {
            cap.sources.push(path);
            if mode {
                modes.push(name);
            }
        }
    }
    Ok(modes)
}

/// Mark the captured servers that Copilot CLI's `disabledMcpServers` lists.
fn capture_disabled(cap: &mut Captured, path: &Path) -> Result<()> {
    let Some(text) = cap.read_config(path, ID)? else {
        return Ok(());
    };
    let doc = parse_value(Some(&text), path)?;
    let names = string_list(doc.get("disabledMcpServers"));
    if names.is_empty() {
        return Ok(());
    }
    cap.sources.push(path.to_path_buf());
    for n in names {
        if let Some(s) = cap.profile.mcp_servers.get_mut(&n) {
            s.disabled = true;
        }
    }
    Ok(())
}

impl Adapter for Copilot {
    fn id(&self) -> &'static str {
        ID
    }
    fn name(&self) -> &'static str {
        "GitHub Copilot"
    }
    fn docs(&self) -> &'static str {
        "https://code.visualstudio.com/docs/agents/reference/mcp-configuration"
    }
    fn scopes(&self) -> &'static [Scope] {
        &[Scope::User, Scope::Project]
    }
    fn support(&self, scope: Scope) -> [(Feature, Support); 8] {
        Feature::ALL.map(|f| {
            let native = match f {
                Feature::Mcp
                | Feature::Instructions
                | Feature::Rules
                | Feature::Skills
                | Feature::Agents => true,
                // VS Code prompt files; user ones live in VS Code's profile storage.
                Feature::Commands => scope == Scope::Project,
                Feature::OutputStyles | Feature::Settings => false,
            };
            (
                f,
                if native {
                    Support::Native
                } else if f == Feature::Commands {
                    // User scope: skills, as VS Code's prompt-file migration does.
                    Support::Folded
                } else {
                    Support::None
                },
            )
        })
    }

    fn locations(&self, env: &Env) -> Vec<(&'static str, PathBuf)> {
        let p = paths(env);
        let mut out = vec![
            ("mcp", p.mcp),
            ("instructions", p.instructions),
            ("rules", p.rules),
        ];
        if let Some(prompts) = p.prompts {
            out.push(("commands", prompts));
        }
        out.push(("skills", p.skills));
        out.push(("agents", p.agents));
        out
    }

    fn detect(&self, env: &Env) -> bool {
        // `.mcp.json` alone belongs to Claude Code.
        match env.scope {
            Scope::User => copilot_home(env).exists(),
            Scope::Project => {
                let g = env.project.join(".github");
                g.join("copilot-instructions.md").exists()
                    || g.join("instructions").exists()
                    || g.join("mcp.json").exists()
                    || g.join("copilot").exists()
                    || g.join("prompts").exists()
                    || g.join("skills").exists()
                    || g.join("agents").exists()
                    || g.join("chatmodes").exists()
                    || env.project.join(".vscode").join("mcp.json").exists()
            }
        }
    }

    fn capture(&self, env: &Env) -> Result<Captured> {
        let mut cap = Captured::for_env(env);
        let p = paths(env);

        match env.scope {
            Scope::User => {
                if let Some(text) = cap.read_config(&p.mcp, ID)? {
                    cap.sources.push(p.mcp.clone());
                    let doc = parse_value(Some(&text), &p.mcp)?;
                    for (name, def) in
                        server_entries(&mut cap, ID, &p.mcp, "mcpServers", doc.get("mcpServers"))
                    {
                        // Copilot CLI expands a bare `$VAR` in `env` (VS Code
                        // keeps it as text), so it is taken as a reference and
                        // its value never captured.
                        import(&mut cap, name, def, &p.mcp, true);
                    }
                }
            }
            Scope::Project => {
                // Closest first: `.mcp.json` wins over `.github/mcp.json`, and
                // both over the older VS Code file.
                let github = env.project.join(".github").join("mcp.json");
                for file in [&p.mcp, &github] {
                    if let Some(text) = cap.read_config(file, ID)? {
                        cap.sources.push(file.clone());
                        let doc = parse_value(Some(&text), file)?;
                        for (name, def) in
                            server_entries(&mut cap, ID, file, "mcpServers", project_servers(&doc))
                        {
                            // Copilot CLI expands a bare `$VAR` in `env`, so it is
                            // taken as a reference and its value never captured.
                            import(&mut cap, name, def, file, true);
                        }
                    }
                }
                let vscode = env.project.join(".vscode").join("mcp.json");
                if let Some(text) = cap.read_config(&vscode, ID)? {
                    cap.sources.push(vscode.clone());
                    let doc = parse_value(Some(&text), &vscode)?;
                    let entries =
                        server_entries(&mut cap, ID, &vscode, "servers", doc.get("servers"));
                    if !entries.is_empty() {
                        cap.info(
                            ID,
                            "read .vscode/mcp.json, VS Code's older editor-managed format; kitbag writes Copilot servers to .mcp.json",
                        );
                    }
                    for (name, def) in entries {
                        import_vscode(&mut cap, name, def, &vscode);
                    }
                }
            }
        }
        capture_disabled(&mut cap, &p.settings)?;

        capture_instructions(&mut cap, &p.instructions, ID)?;

        if !cap.escapes(&p.rules, ID) {
            let mut seen = std::collections::BTreeMap::new();
            for rel in walk_files(&p.rules, 500)? {
                if !rel.ends_with(RULE_EXT) {
                    continue;
                }
                let path = join_rel(&p.rules, &rel);
                let Some(text) = read_text_item(&mut cap, &path, ID) else {
                    continue;
                };
                let name = name_from_rel(&rel, RULE_EXT);
                claim_name(&mut seen, &p.rules, &rel, &name)?;
                if !is_valid_name(&name) {
                    cap.warn(
                        ID,
                        format!("skipped {}: unsupported file name", path.display()),
                    );
                    continue;
                }
                cap.sources.push(path.clone());
                cap.rule_files.push((name.clone(), path.clone()));
                add_rule(&mut cap, rule_from_instructions(&name, &text), ID);
            }
        }

        if let Some(prompts) = &p.prompts {
            capture_markdown_dir(&mut cap, prompts, PROMPT_EXT, MdKind::Command, ID)?;
        }
        capture_skills_dir(&mut cap, &p.skills, ID)?;

        let mut modes = capture_agents(&mut cap, &p.agents, false)?;
        if env.scope == Scope::Project {
            let legacy = env.project.join(".github").join("chatmodes");
            modes.extend(capture_agents(&mut cap, &legacy, true)?);
        }
        if !modes.is_empty() {
            cap.info(
                ID,
                format!(
                    "captured chat modes as agents ({}): chat modes are the old name of custom agents, and kitbag applies them as .agent.md files in {}",
                    modes.join(", "),
                    crate::ui::tildify(&p.agents, &env.home)
                ),
            );
        }
        super::hook_files::capture_for(&mut cap, env, &super::hook_files::COPILOT)?;
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
        match env.scope {
            Scope::User => apply_user_servers(profile, env, opts, work, &p.mcp, &p.settings)?,
            Scope::Project => apply_project_servers(profile, env, opts, work, &p.mcp)?,
        }

        write_instructions(work, &p.instructions, profile, opts, false, ID)?;

        for r in &profile.rules {
            if r.mode() == RuleMode::Globs && r.globs.iter().any(|g| g.contains(',')) {
                work.warn(
                    ID,
                    format!(
                        "rule '{}': Copilot splits applyTo at commas, so a glob with a comma in it ({}) won't match as written",
                        r.name,
                        r.globs.join(" ")
                    ),
                );
            }
            if r.mode() == RuleMode::OnDemand {
                work.info(
                    ID,
                    format!(
                        "rule '{}' is on-demand in the profile; it is written without applyTo, so Copilot only uses it when it is attached",
                        r.name
                    ),
                );
            }
            let path = p.rules.join(format!("{}{RULE_EXT}", r.name));
            write_rule(work, &path, copilot_rule(r).as_bytes(), opts, ID, &r.name)?;
        }

        if let Some(prompts) = &p.prompts {
            for c in &profile.commands {
                apply_prompt(work, prompts, c, opts)?;
            }
        }

        // At user scope there is no prompt-file folder, so commands become
        // skills that only a slash command invokes (VS Code's own migration).
        let skills = if p.prompts.is_none() {
            let skills = commands_as_skills(
                work,
                ID,
                "Copilot",
                &profile.skills,
                &profile.commands,
                true,
            );
            if !profile.commands.is_empty() {
                work.info(
                    ID,
                    format!(
                        "Copilot keeps user prompt files in VS Code's profile storage, which its docs give no path for, so commands are written as skills in {} (VS Code's prompt-file migration: name, description, argument-hint and disable-model-invocation; agent, model and tools are not carried); project scope writes prompt files",
                        crate::ui::tildify(&p.skills, &work.home)
                    ),
                );
            }
            skills
        } else {
            profile.skills.clone()
        };
        for s in &skills {
            if let Some(why) = skill_name_issue(s) {
                work.warn(ID, why);
            }
            let s = skill_with_required_frontmatter(s);
            write_skill(work, &p.skills.join(&s.name), &s, opts, ID)?;
        }

        for a in &profile.agents {
            apply_agent(work, env, &p.agents, a, opts)?;
        }

        if profile.settings.contains_key(ID) {
            work.info(
                ID,
                "kitbag doesn't manage Copilot settings; settings.copilot was not applied",
            );
        }
        super::hook_files::apply_for(profile, env, opts, work, &super::hook_files::COPILOT)?;
        Ok(())
    }
}

/// One command as `.github/prompts/<name>.prompt.md`.
fn apply_prompt(work: &mut Work, dir: &Path, c: &Command, opts: &ApplyOptions) -> Result<()> {
    let (text, dropped) = prompt_file(c);
    if !dropped.is_empty() {
        work.info(
            ID,
            format!(
                "command '{}': {} left out of the prompt file (not a VS Code prompt-file key)",
                c.name,
                dropped.join(", ")
            ),
        );
    }
    if c.body.contains("$ARGUMENTS") || (1..=9).any(|n| c.body.contains(&format!("${n}"))) {
        work.warn(
            ID,
            format!(
                "command '{}' uses $ARGUMENTS or $N placeholders, which VS Code's prompt-file docs don't list; they are written as is (VS Code documents ${{input:NAME}} instead)",
                c.name
            ),
        );
    }
    let path = dir.join(format!("{}{PROMPT_EXT}", c.name));
    write_command(work, &path, text.as_bytes(), opts, ID, &c.name)
}

/// One custom agent as `<dir>/<name>.agent.md`, or as `<name>.md` when that
/// file is there and the `.agent.md` isn't, so an agent captured from a plain
/// `.md` file is written back to it rather than twice.
fn apply_agent(
    work: &mut Work,
    env: &Env,
    dir: &Path,
    a: &Agent,
    opts: &ApplyOptions,
) -> Result<()> {
    if a.description.is_none() {
        work.info(
            ID,
            format!(
                "agent '{}' has no description; Copilot's custom agent schema asks for one",
                a.name
            ),
        );
    }
    let claude_only: Vec<&str> = claude_code::CLAUDE_ONLY_AGENT_KEYS
        .iter()
        .copied()
        .filter(|k| a.extra.contains_key(*k))
        .collect();
    if !claude_only.is_empty() {
        work.info(
            ID,
            format!(
                "agent '{}': {} are Claude Code subagent keys with no Copilot counterpart that kitbag knows of; written as they are, not translated",
                a.name,
                claude_only.join(", ")
            ),
        );
    }
    let agent_md = dir.join(format!("{}{AGENT_EXT}", a.name));
    let plain = dir.join(format!("{}.md", a.name));
    let path = if work.fs.read_bytes(&agent_md)?.is_none() && work.fs.read_bytes(&plain)?.is_some()
    {
        plain
    } else {
        agent_md
    };
    if env.scope == Scope::Project {
        let mode = env
            .project
            .join(".github")
            .join("chatmodes")
            .join(format!("{}{CHATMODE_EXT}", a.name));
        if work.fs.read_bytes(&mode)?.is_some() {
            work.info(
                ID,
                format!(
                    "agent '{}': the old chat mode {} was left in place; remove it once the agent in {} works",
                    a.name,
                    crate::ui::tildify(&mode, &env.home),
                    crate::ui::tildify(dir, &env.home)
                ),
            );
        }
    }
    write_agent(
        work,
        &path,
        agent_to_markdown(a).as_bytes(),
        opts,
        ID,
        &a.name,
    )
}

/// Warn about references where neither Copilot CLI's nor VS Code's docs
/// say they expand.
fn note_unlisted_refs(work: &mut Work, name: &str, fields: &[(&str, Option<&str>)]) {
    let found: Vec<&str> = fields
        .iter()
        .filter(|(_, v)| v.is_some_and(has_ref))
        .map(|(k, _)| *k)
        .collect();
    if !found.is_empty() {
        work.warn(
            ID,
            format!(
                "MCP server '{name}': env reference in {}; Copilot's docs only say references expand in env and headers (Copilot CLI) and command, args, env and headers (VS Code), so check that the server still works",
                found.join(", ")
            ),
        );
    }
}

/// `$COPILOT_HOME/mcp-config.json`: Copilot's own shape, with `cwd` and `tools`.
fn apply_user_servers(
    profile: &Profile,
    env: &Env,
    opts: &ApplyOptions,
    work: &mut Work,
    path: &Path,
    settings: &Path,
) -> Result<()> {
    let existing: Map<String, Value> = match work.fs.read(path)? {
        Some(text) => parse_value(Some(&text), path)?
            .get("mcpServers")
            .and_then(Value::as_object)
            .cloned()
            .unwrap_or_default(),
        None => Map::new(),
    };
    let mut servers = Vec::new();
    for (name, s) in servers_for(profile, ID, work, false) {
        note_unlisted_refs(
            work,
            name,
            &[("url", s.url.as_deref()), ("cwd", s.cwd.as_deref())],
        );
        let s = render_server(s, RefStyle::Dollar, env, work);
        let mut def = Map::new();
        match s.transport() {
            Transport::Stdio => {
                def.insert("type".into(), json!("stdio"));
                def.insert("command".into(), json!(s.command));
                def.insert("args".into(), json!(s.args));
                if !s.env.is_empty() {
                    def.insert("env".into(), json!(s.env));
                }
                if let Some(cwd) = &s.cwd {
                    def.insert("cwd".into(), json!(cwd));
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
        // Copilot CLI's reference marks `tools` required. Keep the list of an
        // entry for the same server; anything else gets all tools.
        let keeps_tools = existing
            .get(name.as_str())
            .and_then(Value::as_object)
            .is_some_and(|cur| cur.contains_key("tools") && same_server(cur, &def));
        if !keeps_tools {
            def.insert("tools".into(), json!(["*"]));
        }
        servers.push((name.clone(), Value::Object(def)));
    }
    // Copilot CLI's `disabledMcpServers` (in its settings.json) lists servers
    // that are "configured but not started": a server the profile disables
    // here goes in it and its entry stays. (A project has no such switch
    // kitbag can write without touching a file shared with the team, so there
    // the entry is removed.)
    let skipped = super::common::take_skipped_servers(work, ID);
    merge_json_servers(
        work,
        path,
        &["mcpServers"],
        &servers,
        MANAGED_KEYS,
        opts,
        ID,
    )?;
    let enabled: Vec<String> = servers.iter().map(|(n, _)| n.clone()).collect();
    super::common::sync_off_list(
        work,
        opts,
        ID,
        (path, &["mcpServers"]),
        &super::common::OffList {
            path: settings,
            key: &["disabledMcpServers"],
            label: "disabledMcpServers",
        },
        &skipped,
        &enabled,
    )
}

/// The project `.mcp.json`, shared with Claude Code: written exactly as Claude
/// Code writes it, so applying both targets plans each entry once and neither
/// changes what the other wrote.
fn apply_project_servers(
    profile: &Profile,
    env: &Env,
    opts: &ApplyOptions,
    work: &mut Work,
    path: &Path,
) -> Result<()> {
    if let Some(text) = work.fs.read(path)? {
        let doc = parse_value(Some(&text), path)?;
        if doc.get("mcpServers").is_none() && is_bare(&doc) {
            if !profile.mcp_servers.is_empty() {
                work.warn(
                    ID,
                    format!(
                        "{} lists its servers at the top level, without mcpServers; kitbag left it alone. Move them under \"mcpServers\" (Claude Code reads only that) and apply again",
                        crate::ui::tildify(path, &work.home)
                    ),
                );
            }
            return Ok(());
        }
    }
    let mut servers = Vec::new();
    for (name, s) in servers_for(profile, ID, work, false) {
        if s.cwd.is_some() {
            work.warn(
                ID,
                format!(
                    "MCP server '{name}': 'cwd' was dropped from .mcp.json (Claude Code reads the same file and has no cwd, and VS Code's workspace form has none); set it in your user config instead"
                ),
            );
        }
        note_unlisted_refs(work, name, &[("url", s.url.as_deref())]);
        if !s.applies_to(claude_code::ID) {
            work.warn(
                ID,
                format!(
                    "MCP server '{name}' is kept off claude-code in the profile, but Claude Code reads .mcp.json too, so it will see it"
                ),
            );
        }
        let s = render_server(s, RefStyle::Dollar, env, work);
        servers.push((name.clone(), claude_code::server_json(&s)));
    }
    keep_shared_servers(
        work,
        profile,
        opts,
        ID,
        claude_code::ID,
        "Claude Code",
        path,
    );
    merge_json_servers(
        work,
        path,
        &["mcpServers"],
        &servers,
        claude_code::MANAGED_KEYS,
        opts,
        ID,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rule(globs: &[&str], always: Option<bool>, description: Option<&str>) -> Rule {
        Rule {
            name: "r".into(),
            description: description.map(str::to_string),
            globs: globs.iter().map(|g| g.to_string()).collect(),
            always_apply: always,
            body: "Body.".into(),
            ..Default::default()
        }
    }

    #[test]
    fn rule_format_round_trips_every_mode() {
        let globs = rule(&["src/**/*.ts", "test/**"], None, Some("TypeScript"));
        assert_eq!(
            copilot_rule(&globs),
            "---\ndescription: TypeScript\napplyTo: \"src/**/*.ts,test/**\"\n---\n\nBody.\n"
        );
        let always = rule(&[], None, None);
        assert_eq!(
            copilot_rule(&always),
            "---\napplyTo: \"**\"\n---\n\nBody.\n"
        );
        let on_demand = rule(&[], Some(false), Some("Use for SQL"));
        assert_eq!(
            copilot_rule(&on_demand),
            "---\ndescription: Use for SQL\n---\n\nBody.\n"
        );
        for r in [globs, always, on_demand] {
            assert_eq!(rule_from_instructions("r", &copilot_rule(&r)), r);
        }
    }

    #[test]
    fn hand_written_instruction_files_are_read() {
        let r = rule_from_instructions("r", "---\napplyTo: '**/*.py'\n---\nUse hints.\n");
        assert_eq!(r.globs, vec!["**/*.py"]);
        assert_eq!(r.mode(), RuleMode::Globs);
        let r = rule_from_instructions("r", "---\napplyTo: \"**/*\"\n---\nAll.\n");
        assert_eq!(r.mode(), RuleMode::Always);
        // No frontmatter at all: only used when attached.
        let r = rule_from_instructions("r", "Just text.\n");
        assert_eq!(r.mode(), RuleMode::OnDemand);
    }

    #[test]
    fn bare_project_files_are_recognised() {
        assert!(is_bare(&json!({"pw": {"command": "npx"}})));
        assert!(is_bare(
            &json!({"ctx": {"type": "http", "url": "https://x.example"}})
        ));
        assert!(!is_bare(&json!({})));
        assert!(!is_bare(&json!({"mcpServers": {}})));
        assert!(!is_bare(&json!({"pw": {"command": "npx"}, "inputs": []})));
    }
}
