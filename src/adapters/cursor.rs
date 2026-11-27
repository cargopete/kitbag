//! Cursor (editor and CLI).
//!
//! * MCP: `~/.cursor/mcp.json` / `.cursor/mcp.json`. Cursor interpolates
//!   `${env:VAR}` in command, args, env, url and headers, so references are
//!   rewritten into that syntax and secrets never touch disk.
//! * Rules: `.cursor/rules/*.mdc` (project). User rules live in Cursor's
//!   settings UI, not in files, so they cannot be written at user scope.
//! * Instructions: `AGENTS.md` at the project root.
//! * Commands: `.cursor/commands/*.md` (plain markdown). Skills: `.cursor/skills/`.
//! * Subagents: `~/.cursor/agents/<name>.md` and `.cursor/agents/<name>.md`,
//!   markdown with YAML frontmatter (`name`, `description`, `model`,
//!   `readonly`, `is_background`) and the prompt as the body. Cursor also reads
//!   `.claude/agents/` and `~/.claude/agents/`, which are Claude Code's, so
//!   they are not captured or written here.
//!   Docs: <https://cursor.com/docs/subagents>.
//! * Settings, at user scope only, and only the Agent CLI's file: the keys of
//!   `crate::policy::allowed_keys("cursor")` in `~/.cursor/cli-config.json`
//!   (`$CURSOR_CONFIG_DIR/cli-config.json`, or `$XDG_CONFIG_HOME/cursor/cli-config.json`
//!   on Linux, as the CLI docs say; the project's `.cursor/cli.json` takes only
//!   permissions and is not written). A file kitbag creates starts with
//!   `"version": 1`; the docs say the CLI repairs missing fields. Docs:
//!   <https://cursor.com/docs/cli/reference/configuration> and
//!   <https://cursor.com/docs/cli/reference/permissions> (checked 2026-11-27).
//!   Cursor's docs name no `settings.json` path or agent/model keys for the
//!   editor, and the editor keeps some settings in a `state.vscdb` database with
//!   no documented layout; kitbag never reads or writes either. The CLI's
//!   `model` object (no documented shape), `sandbox`, `network` and `channel`
//!   are left alone.

use std::path::PathBuf;
use std::sync::LazyLock;

use anyhow::Result;
use regex::Regex;
use serde_json::{Map, Value, json};

use super::common::{
    AgentName, add_command, add_instructions, add_rule, capture_agent_dir, capture_instructions,
    capture_skills_dir, claim_name, documented_agent, drop_redacted_settings, import_server,
    merge_json_servers, merge_json_settings, name_from_rel, note_agent_keys, pick_settings,
    read_text_item, render_server, safe_settings, server_entries, server_from_json, servers_for,
    write_agent, write_instructions, write_skill,
};
use super::{Adapter, ApplyOptions, Captured, Env, Feature, Os, Scope, Support, Work};
use crate::edit::block;
use crate::edit::json::parse_value;
use crate::frontmatter;
use crate::model::{Agent, Command, Profile, Rule, RuleMode, Transport, is_valid_name};
use crate::plan_fs::{join_rel, walk_files};
use crate::profile_io::rule_from_markdown;
use crate::secrets::RefStyle;

const ID: &str = "cursor";
/// Server keys kitbag writes; anything else in an existing entry is preserved.
const MANAGED_KEYS: &[&str] = &["type", "command", "args", "env", "url", "headers"];

pub struct Cursor;

struct Paths {
    mcp: PathBuf,
    instructions: Option<PathBuf>,
    rules: Option<PathBuf>,
    commands: PathBuf,
    skills: PathBuf,
    agents: PathBuf,
}

fn paths(env: &Env) -> Paths {
    match env.scope {
        Scope::User => {
            let d = env.home.join(".cursor");
            Paths {
                mcp: d.join("mcp.json"),
                instructions: None,
                rules: None,
                commands: d.join("commands"),
                skills: d.join("skills"),
                agents: d.join("agents"),
            }
        }
        Scope::Project => {
            let d = env.project.join(".cursor");
            Paths {
                mcp: d.join("mcp.json"),
                instructions: Some(env.project.join("AGENTS.md")),
                rules: Some(d.join("rules")),
                commands: d.join("commands"),
                skills: d.join("skills"),
                agents: d.join("agents"),
            }
        }
    }
}

/// The Agent CLI's global config file, as its docs give the location:
/// `$CURSOR_CONFIG_DIR`, else `$XDG_CONFIG_HOME/cursor` on Linux, else `~/.cursor`.
fn cli_config(env: &Env) -> PathBuf {
    let dir = env
        .location_var("CURSOR_CONFIG_DIR")
        .or_else(|| {
            (env.os == Os::Linux)
                .then(|| env.location_var("XDG_CONFIG_HOME"))
                .flatten()
                .map(|d| d.join("cursor"))
        })
        .unwrap_or_else(|| env.home.join(".cursor"));
    dir.join("cli-config.json")
}

/// Cursor's frontmatter reader is not a YAML parser: it expects unquoted,
/// comma-separated globs (`globs: src/**/*.ts,test/**`), so `.mdc` is written by hand.
pub fn cursor_mdc(r: &Rule) -> String {
    let mode = r.mode();
    let globs = if mode == RuleMode::Globs {
        r.globs.join(",")
    } else {
        String::new()
    };
    let description = r
        .description
        .as_deref()
        .unwrap_or("")
        .replace(['\r', '\n'], " ");
    let lines = [
        "---".to_string(),
        format!("description: {description}").trim_end().to_string(),
        format!("globs: {globs}").trim_end().to_string(),
        format!("alwaysApply: {}", mode == RuleMode::Always),
        "---".to_string(),
        String::new(),
        r.body.trim().to_string(),
        String::new(),
    ];
    lines.join("\n")
}

static CURSOR_VARS: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"\$\{(userHome|workspaceFolder|workspaceFolderBasename|pathSeparator|/)\}")
        .expect("valid regex")
});

fn import(cap: &mut Captured, name: &str, def: &Map<String, Value>) {
    if def.contains_key("envFile") {
        cap.warn(
            ID,
            format!("MCP server '{name}': envFile is Cursor-specific and was not captured; list the variables in 'env' instead"),
        );
    }
    let blob = Value::Object(def.clone()).to_string();
    let mut vars: Vec<&str> = CURSOR_VARS
        .captures_iter(&blob)
        .filter_map(|c| c.get(1))
        .map(|m| m.as_str())
        .collect();
    vars.dedup();
    if !vars.is_empty() {
        cap.warn(
            ID,
            format!("MCP server '{name}' uses Cursor-only variables ({}); other tools won't expand them", vars.join(", ")),
        );
    }
    let mut s = server_from_json(def);
    s.oauth = crate::oauth::read(ID, def);
    if def.get("type").and_then(Value::as_str) == Some("sse") {
        s.transport = Some(Transport::Sse);
    }
    import_server(cap, ID, name, s, false);
}

/// A Cursor rule file -> profile rule, normalising Cursor's always-present `alwaysApply`.
pub fn rule_from_mdc(name: &str, text: &str) -> Rule {
    let mut r = rule_from_markdown(name, text);
    match (r.globs.is_empty(), r.always_apply) {
        (false, Some(false)) | (true, Some(true)) => r.always_apply = None,
        _ => {}
    }
    r
}

/// Frontmatter keys Cursor's subagent docs list besides `name` and `description`.
const AGENT_KEYS: &[&str] = &["model", "readonly", "is_background"];

/// A subagent as a Cursor file: the documented keys only, with `model` a
/// string and the two switches booleans, as documented.
pub fn cursor_agent(a: &Agent) -> (String, Vec<String>) {
    documented_agent(a, AgentName::Optional, AGENT_KEYS, |k, v| match k {
        "model" => v.is_string(),
        _ => v.is_boolean(),
    })
}

fn apply_agent(
    work: &mut Work,
    dir: &std::path::Path,
    a: &Agent,
    opts: &ApplyOptions,
) -> Result<()> {
    let (text, dropped) = cursor_agent(a);
    note_agent_keys(work, ID, "Cursor", a, &dropped);
    let path = dir.join(format!("{}.md", a.name));
    write_agent(work, &path, text.as_bytes(), opts, ID, &a.name)
}

impl Adapter for Cursor {
    fn id(&self) -> &'static str {
        ID
    }
    fn name(&self) -> &'static str {
        "Cursor"
    }
    fn docs(&self) -> &'static str {
        "https://cursor.com/docs/context/mcp"
    }
    fn scopes(&self) -> &'static [Scope] {
        &[Scope::User, Scope::Project]
    }
    fn support(&self, scope: Scope) -> [(Feature, Support); 8] {
        let file_rules = if scope == Scope::Project {
            Support::Native
        } else {
            Support::None
        };
        [
            (Feature::Mcp, Support::Native),
            (Feature::Instructions, file_rules),
            (Feature::Rules, file_rules),
            (Feature::Commands, Support::Native),
            (Feature::Skills, Support::Native),
            (Feature::Agents, Support::Native),
            (Feature::OutputStyles, Support::None),
            (
                Feature::Settings,
                if scope == Scope::User {
                    Support::Native
                } else {
                    Support::None
                },
            ),
        ]
    }

    fn locations(&self, env: &Env) -> Vec<(&'static str, PathBuf)> {
        let p = paths(env);
        let mut out = vec![("mcp", p.mcp)];
        if let Some(i) = p.instructions {
            out.push(("instructions", i));
        }
        if let Some(r) = p.rules {
            out.push(("rules", r));
        }
        out.push(("commands", p.commands));
        out.push(("skills", p.skills));
        out.push(("agents", p.agents));
        if env.scope == Scope::User {
            out.push(("settings", cli_config(env)));
        }
        out
    }

    fn detect(&self, env: &Env) -> bool {
        match env.scope {
            Scope::User => env.home.join(".cursor").exists(),
            Scope::Project => {
                env.project.join(".cursor").exists() || env.project.join(".cursorrules").exists()
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

        if env.scope == Scope::User {
            let file = cli_config(env);
            if let Some(text) = cap.read_config(&file, ID)? {
                cap.sources.push(file.clone());
                if let Value::Object(doc) = parse_value(Some(&text), &file)? {
                    pick_settings(&mut cap, ID, &doc);
                }
            }
        }

        if let Some(i) = &p.instructions {
            capture_instructions(&mut cap, i, ID)?;
            let legacy = env.project.join(".cursorrules");
            if let Some(text) = read_text_item(&mut cap, &legacy, ID)
                && !text.trim().is_empty()
            {
                cap.sources.push(legacy.clone());
                add_instructions(&mut cap, ID, &text, &legacy);
                cap.info(ID, "imported legacy .cursorrules into instructions");
            }
        } else {
            cap.info(ID, "Cursor user rules live in Settings > Rules (not in files) and can't be captured; copy them into instructions.md");
        }

        if let Some(dir) = &p.rules {
            let mut seen = std::collections::BTreeMap::new();
            for rel in walk_files(dir, 500)? {
                if !rel.ends_with(".mdc") {
                    continue;
                }
                let path = join_rel(dir, &rel);
                let Some(text) = read_text_item(&mut cap, &path, ID) else {
                    continue;
                };
                let name = name_from_rel(&rel, ".mdc");
                claim_name(&mut seen, dir, &rel, &name)?;
                if !is_valid_name(&name) {
                    cap.warn(
                        ID,
                        format!("skipped {}: unsupported file name", path.display()),
                    );
                    continue;
                }
                cap.sources.push(path.clone());
                // A rule file holding kitbag blocks carries folded rules from another apply.
                let split = block::split(&frontmatter::parse(&text).body);
                if !split.blocks.is_empty() {
                    for (_, inner) in split.blocks {
                        let (rest, rules) = block::unfold_rules(&inner);
                        add_instructions(&mut cap, ID, &rest, dir);
                        for r in rules {
                            add_rule(&mut cap, r, ID);
                        }
                    }
                    if split.outside.is_empty() {
                        continue;
                    }
                }
                cap.rule_files.push((name.clone(), path.clone()));
                add_rule(&mut cap, rule_from_mdc(&name, &text), ID);
            }
        }

        let mut seen = std::collections::BTreeMap::new();
        for rel in walk_files(&p.commands, 500)? {
            if !rel.ends_with(".md") {
                continue;
            }
            let path = join_rel(&p.commands, &rel);
            let Some(text) = read_text_item(&mut cap, &path, ID) else {
                continue;
            };
            let name = name_from_rel(&rel, ".md");
            claim_name(&mut seen, &p.commands, &rel, &name)?;
            if !is_valid_name(&name) {
                continue;
            }
            cap.sources.push(path);
            let doc = frontmatter::parse(&text);
            let cmd = Command {
                name,
                description: frontmatter::string(doc.data.get("description")),
                body: doc.body.trim().to_string(),
                ..Default::default()
            };
            add_command(&mut cap, cmd, ID);
        }

        capture_skills_dir(&mut cap, &p.skills, ID)?;
        capture_agent_dir(&mut cap, &p.agents, None, ID, "a subagent")?;
        super::hook_files::capture_for(&mut cap, env, &super::hook_files::CURSOR)?;
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
                        "MCP server '{name}': 'cwd' is not supported by Cursor and was dropped"
                    ),
                );
            }
            let original = s;
            let s = render_server(s, RefStyle::EnvColon, env, work);
            let mut def = Map::new();
            if s.transport() == Transport::Stdio {
                def.insert("type".into(), json!("stdio"));
                def.insert("command".into(), json!(s.command));
                def.insert("args".into(), json!(s.args));
                if !s.env.is_empty() {
                    def.insert("env".into(), json!(s.env));
                }
            } else {
                def.insert("url".into(), json!(s.url));
                if !s.headers.is_empty() {
                    def.insert("headers".into(), json!(s.headers));
                }
            }
            super::common::add_oauth(work, ID, name, (original, &s), &mut def);
            servers.push((name.clone(), Value::Object(def)));
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

        match (&p.instructions, &p.rules) {
            (Some(instr), Some(rules_dir)) => {
                write_instructions(work, instr, profile, opts, false, ID)?;
                for r in &profile.rules {
                    let path = rules_dir.join(format!("{}.mdc", r.name));
                    super::common::write_rule(
                        work,
                        &path,
                        cursor_mdc(r).as_bytes(),
                        opts,
                        ID,
                        &r.name,
                    )?;
                }
            }
            _ => {
                if profile.instructions.is_some() || !profile.rules.is_empty() {
                    work.info(
                        ID,
                        "Cursor keeps user rules in Settings > Rules, not in files; paste instructions there, or apply with --project to write .cursor/rules and AGENTS.md",
                    );
                }
            }
        }

        for c in &profile.commands {
            // Cursor commands are plain markdown: the body is the prompt.
            let path = p.commands.join(format!("{}.md", c.name));
            let body = format!("{}\n", c.body.trim());
            super::common::write_command(work, &path, body.as_bytes(), opts, ID, &c.name)?;
        }
        for s in &profile.skills {
            let s = super::common::skill_with_required_frontmatter(s);
            write_skill(work, &p.skills.join(&s.name), &s, opts, ID)?;
        }
        for a in &profile.agents {
            apply_agent(work, &p.agents, a, opts)?;
        }
        if env.scope == Scope::User {
            let file = cli_config(env);
            let mut settings = safe_settings(profile, ID, work);
            drop_redacted_settings(&mut settings, ID, work);
            if !settings.is_empty() {
                if work.fs.read(&file)?.is_none() {
                    settings.insert("version".into(), json!(1));
                }
                merge_json_settings(work, &file, &settings, opts, ID)?;
                work.info(
                    ID,
                    format!(
                        "settings.cursor went to {}, the Cursor Agent CLI's file; the editor's own settings are not documented and were not touched",
                        crate::ui::tildify(&file, &work.home)
                    ),
                );
            }
        } else if profile.settings.contains_key(ID) {
            work.info(
                ID,
                "settings.cursor was not applied to the project: .cursor/cli.json takes only permissions and kitbag writes the user's cli-config.json; apply it at user scope",
            );
        }
        super::hook_files::apply_for(profile, env, opts, work, &super::hook_files::CURSOR)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mdc_format() {
        let r = Rule {
            name: "py".into(),
            description: Some("Python style".into()),
            globs: vec!["*.py".into(), "scripts/**/*.py".into()],
            always_apply: None,
            body: "Use hints.".into(),
            ..Default::default()
        };
        assert_eq!(
            cursor_mdc(&r),
            "---\ndescription: Python style\nglobs: *.py,scripts/**/*.py\nalwaysApply: false\n---\n\nUse hints.\n"
        );
        let back = rule_from_mdc("py", &cursor_mdc(&r));
        assert_eq!(back, r);
        let always = Rule {
            name: "a".into(),
            body: "x".into(),
            ..Default::default()
        };
        assert_eq!(rule_from_mdc("a", &cursor_mdc(&always)), always);
        let on_demand = Rule {
            always_apply: Some(false),
            ..always.clone()
        };
        assert_eq!(rule_from_mdc("a", &cursor_mdc(&on_demand)), on_demand);
    }
}
