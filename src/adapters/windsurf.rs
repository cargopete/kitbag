//! Windsurf (the editor Codeium made, now also shipped as Devin Desktop): MCP
//! servers, instructions, rules, commands (workflows) and skills.
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
//! * Rules, project scope only: `<name>.md` in `.windsurf/rules/`, with a
//!   `trigger` in the frontmatter (`always_on`, `glob` with `globs`,
//!   `model_decision` with a `description`; `manual` has no profile
//!   equivalent and is left behind by capture, as is a file with no
//!   `trigger`, whose default the docs don't give). The docs name no
//!   user-level rules folder under `~/.codeium/windsurf`, only Devin Desktop's
//!   `~/.devin/rules/`, which kitbag doesn't write, so at user scope rules
//!   are not carried. Several globs are written as one comma-separated
//!   string, since the docs show only a single pattern.
//! * Commands (Windsurf's workflows): `<name>.md` in
//!   `~/.codeium/windsurf/global_workflows/` and a project's
//!   `.windsurf/workflows/` (Devin Desktop's docs call it the fallback to
//!   `.devin/workflows/`), invoked as `/<name>`. The docs call for a title,
//!   description and steps without giving frontmatter syntax; kitbag writes the
//!   prompt as the body, with the command's description as `description` in
//!   frontmatter when it has one. A workflow file is limited to 12,000 characters.
//! * Skills, project scope only: `<name>/SKILL.md` and its files in
//!   `.windsurf/skills/` (the docs call it the legacy workspace folder, still
//!   read). The docs name no user-level folder of Windsurf's own, only
//!   `~/.config/devin/skills/` and `~/.agents/skills/`, so at user scope skills
//!   are not carried.
//! * Hooks (#13) and agents (#14) are not carried.
//!
//! Docs: <https://docs.devin.ai/windsurf/plugins/cascade/mcp>,
//! <https://docs.devin.ai/desktop/cascade/memories>,
//! <https://docs.devin.ai/desktop/cascade/agents-md>,
//! <https://docs.devin.ai/desktop/cascade/workflows>,
//! <https://docs.devin.ai/desktop/cascade/skills>.

use std::path::PathBuf;

use anyhow::Result;
use serde_json::{Map, Value, json};

use super::common::{
    add_command, add_instructions, add_rule, capture_instructions, capture_md_files,
    capture_skills_dir, import_server, merge_json_servers, note_shadowed_skill, note_uncaptured,
    read_text_item, render_server, server_entries, server_from_json, servers_for,
    skill_name_mismatch, skill_with_required_frontmatter, str_of, write_command,
    write_instructions, write_rule, write_skill,
};
use super::{Adapter, ApplyOptions, Captured, Env, Feature, Scope, Support, Work};
use crate::edit::json::parse_value;
use crate::frontmatter;
use crate::model::{Command, Profile, Rule, RuleMode, Transport};
use crate::profile_io::{command_from_markdown, rule_from_markdown};
use crate::secrets::RefStyle;

const ID: &str = "windsurf";
/// Server keys kitbag writes; anything else (`disabled`, `disabledTools` ...)
/// is preserved. `url` is listed so an old `url` entry gives way to
/// `serverUrl` instead of sitting next to it.
const MANAGED_KEYS: &[&str] = &["command", "args", "env", "serverUrl", "url", "headers"];
/// Windsurf's documentation has limited `global_rules.md` to this many characters.
const GLOBAL_RULES_MAX_CHARS: usize = 6_000;
/// Windsurf's documentation limits each workflow file to this many characters.
const WORKFLOW_MAX_CHARS: usize = 12_000;

pub struct Windsurf;

struct Paths {
    /// `None` at project scope: Windsurf documents no project MCP file.
    mcp: Option<PathBuf>,
    instructions: PathBuf,
    /// A legacy instructions file Windsurf still reads, for capture only.
    legacy: Option<PathBuf>,
    /// `None` at user scope: no user-level rules folder is documented.
    rules: Option<PathBuf>,
    workflows: PathBuf,
    /// `None` at user scope: no user-level skills folder of Windsurf's is documented.
    skills: Option<PathBuf>,
}

fn paths(env: &Env) -> Paths {
    match env.scope {
        Scope::User => {
            let d = env.home.join(".codeium").join("windsurf");
            Paths {
                mcp: Some(d.join("mcp_config.json")),
                instructions: d.join("memories").join("global_rules.md"),
                legacy: None,
                rules: None,
                workflows: d.join("global_workflows"),
                skills: None,
            }
        }
        Scope::Project => {
            let d = env.project.join(".windsurf");
            Paths {
                mcp: None,
                instructions: env.project.join("AGENTS.md"),
                legacy: Some(env.project.join(".windsurfrules")),
                rules: Some(d.join("rules")),
                workflows: d.join("workflows"),
                skills: Some(d.join("skills")),
            }
        }
    }
}

/// A rule file as a rule, or why it is left behind. Windsurf's `trigger`
/// decides when a rule applies; the docs give no default for a file without one.
fn rule_from_windsurf(name: &str, text: &str) -> Result<Rule, String> {
    let doc = frontmatter::parse(text);
    let mut r = rule_from_markdown(name, text);
    r.globs.clear();
    r.always_apply = None;
    match doc.data.get("trigger").and_then(Value::as_str) {
        Some("always_on") => Ok(r),
        Some("glob") => {
            r.globs = frontmatter::glob_list(doc.data.get("globs"));
            if r.globs.is_empty() {
                Err("its `glob` trigger has no `globs`, so it never applies".into())
            } else {
                Ok(r)
            }
        }
        Some("model_decision") => {
            r.always_apply = Some(false);
            Ok(r)
        }
        Some("manual") => Err(
            "its trigger is `manual` (applies only when mentioned), which the profile has no rule mode for".into(),
        ),
        Some(other) => Err(format!("its trigger '{other}' is not one kitbag knows")),
        None => Err("it has no `trigger`, and Windsurf's docs don't say when such a rule applies".into()),
    }
}

/// A profile rule as a Windsurf rule file.
fn windsurf_rule(r: &Rule) -> String {
    let (trigger, globs) = match r.mode() {
        RuleMode::Always => ("always_on", Value::Null),
        RuleMode::Globs => ("glob", Value::String(r.globs.join(", "))),
        RuleMode::OnDemand => ("model_decision", Value::Null),
    };
    frontmatter::render(
        &[
            ("trigger", json!(trigger)),
            (
                "description",
                r.description.clone().map_or(Value::Null, Value::String),
            ),
            ("globs", globs),
        ],
        &r.body,
    )
}

/// A profile command as a workflow file: the prompt, with the description in
/// frontmatter when there is one.
fn windsurf_workflow(c: &Command) -> String {
    frontmatter::render(
        &[(
            "description",
            c.description.clone().map_or(Value::Null, Value::String),
        )],
        &c.body,
    )
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
                Feature::Instructions | Feature::Commands => Support::Native,
                Feature::Rules | Feature::Skills if scope == Scope::Project => Support::Native,
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
        if let Some(rules) = p.rules {
            out.push(("rules", rules));
        }
        out.push(("commands", p.workflows));
        if let Some(skills) = p.skills {
            out.push(("skills", skills));
        }
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

        if let Some(dir) = &p.rules {
            capture_md_files(
                &mut cap,
                dir,
                ".md",
                true,
                None,
                ID,
                &mut |cap, name, text, path| {
                    cap.sources.push(path.to_path_buf());
                    match rule_from_windsurf(name, text) {
                        Ok(rule) => {
                            cap.rule_files.push((name.to_string(), path.to_path_buf()));
                            add_rule(cap, rule, ID);
                        }
                        Err(why) => cap.info(ID, format!("rule '{name}' was not captured: {why}")),
                    }
                },
            )?;
        }
        capture_md_files(
            &mut cap,
            &p.workflows,
            ".md",
            true,
            None,
            ID,
            &mut |cap, name, text, path| {
                cap.sources.push(path.to_path_buf());
                add_command(cap, command_from_markdown(name, text), ID);
            },
        )?;
        if let Some(dir) = &p.skills {
            capture_skills_dir(&mut cap, dir, ID)?;
        }

        // Say what stays behind. Under `~/.codeium/windsurf` that is the
        // memories Cascade writes by itself and any other workflow files;
        // the rest of the folder is Windsurf's own state and is not listed.
        match env.scope {
            Scope::User => {
                note_uncaptured(
                    &mut cap,
                    ID,
                    &p.workflows,
                    &env.home,
                    "kitbag reads only the *.md workflow files directly in a workflows folder",
                    &|rel| rel.ends_with(".md") && !rel.contains('/'),
                );
                if let Some(dir) = p.instructions.parent() {
                    let own = p
                        .instructions
                        .file_name()
                        .map(|n| n.to_string_lossy().into_owned());
                    note_uncaptured(
                        &mut cap,
                        ID,
                        dir,
                        &env.home,
                        "kitbag carries only global_rules.md from Windsurf's memories folder",
                        &|rel| Some(rel) == own.as_deref(),
                    );
                }
            }
            Scope::Project => {
                note_uncaptured(
                    &mut cap,
                    ID,
                    &env.project.join(".windsurf"),
                    &env.home,
                    "kitbag reads only the *.md files directly in .windsurf/rules and .windsurf/workflows, and the skills",
                    &|rel| {
                        ["rules/", "workflows/"].iter().any(|d| {
                            rel.strip_prefix(d)
                                .is_some_and(|f| f.ends_with(".md") && !f.contains('/'))
                        }) || rel.starts_with("skills/")
                    },
                );
                note_uncaptured(
                    &mut cap,
                    ID,
                    &env.project.join(".devin"),
                    &env.home,
                    "kitbag carries Windsurf's own .windsurf folders, not Devin Desktop's .devin",
                    &|_| false,
                );
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
        self.apply_files(profile, env, &p, opts, work)?;
        if profile.settings.contains_key(ID) {
            work.info(
                ID,
                "kitbag doesn't manage Windsurf settings; settings.windsurf was not applied",
            );
        }
        Ok(())
    }
}

impl Windsurf {
    /// Rules, workflows and skills: files of their own.
    fn apply_files(
        &self,
        profile: &Profile,
        env: &Env,
        p: &Paths,
        opts: &ApplyOptions,
        work: &mut Work,
    ) -> Result<()> {
        if let Some(dir) = &p.rules {
            for r in &profile.rules {
                if r.mode() == RuleMode::Globs && r.globs.len() > 1 {
                    work.info(
                        ID,
                        format!(
                            "rule '{}': Windsurf's docs show a single `globs` pattern; the {} patterns are written as one comma-separated string",
                            r.name,
                            r.globs.len()
                        ),
                    );
                }
                if r.mode() == RuleMode::Globs && r.globs.iter().any(|g| g.contains(',')) {
                    work.warn(
                        ID,
                        format!(
                            "rule '{}': a glob with a comma may be read as two patterns by Windsurf",
                            r.name
                        ),
                    );
                }
                if r.mode() == RuleMode::OnDemand && r.description.is_none() {
                    work.info(
                        ID,
                        format!(
                            "rule '{}' is on-demand but has no description, which Windsurf's model_decision trigger goes by",
                            r.name
                        ),
                    );
                }
                write_rule(
                    work,
                    &dir.join(format!("{}.md", r.name)),
                    windsurf_rule(r).as_bytes(),
                    opts,
                    ID,
                    &r.name,
                )?;
            }
            if !profile.rules.is_empty() && work.targets.iter().any(|t| t == "codex") {
                work.info(
                    ID,
                    "Codex folds the rules into the shared AGENTS.md, and Windsurf reads that file as well as .windsurf/rules/, so in this project Windsurf sees the rules twice",
                );
            }
        }

        for c in &profile.commands {
            let text = windsurf_workflow(c);
            if text.chars().count() > WORKFLOW_MAX_CHARS {
                work.warn(
                    ID,
                    format!(
                        "command '{}' is {} characters; Windsurf's docs limit a workflow file to {WORKFLOW_MAX_CHARS}, so it may not load",
                        c.name,
                        text.chars().count()
                    ),
                );
            }
            if c.argument_hint.is_some() {
                work.info(
                    ID,
                    format!(
                        "command '{}': Windsurf workflows take no argument hint; it was dropped",
                        c.name
                    ),
                );
            }
            write_command(
                work,
                &p.workflows.join(format!("{}.md", c.name)),
                text.as_bytes(),
                opts,
                ID,
                &c.name,
            )?;
        }

        if let Some(dir) = &p.skills {
            // Devin Desktop's docs say `.devin/skills/` and `.agents/skills/` are read too.
            let read = [
                dir.clone(),
                env.project.join(".devin").join("skills"),
                env.project.join(".agents").join("skills"),
            ];
            for s in &profile.skills {
                let s = skill_with_required_frontmatter(s);
                if let Some(w) = skill_name_mismatch(&s) {
                    work.warn(ID, w);
                }
                note_shadowed_skill(work, ID, &read, &s.name);
                write_skill(work, &dir.join(&s.name), &s, opts, ID)?;
            }
        }
        Ok(())
    }
}
