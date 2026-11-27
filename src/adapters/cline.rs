//! Cline (the VS Code and JetBrains extension, the CLI and the SDK share one
//! configuration): MCP servers, instructions, rules, commands (workflows) and
//! skills.
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
//!   Codex, Cursor, Antigravity, opencode, Grok Build and Windsurf. Instructions
//!   only: profile rules are files of their own (below), and `kitbag` is not a
//!   usable rule name at user scope.
//! * Rules: `<name>.md` in `~/.cline/rules/` (read from there and from
//!   `~/Documents/Cline/Rules/` and `~/Cline/Rules/` too) and in a project's
//!   `.cline/rules/` (and the older `.clinerules/` directory). A rule with
//!   globs gets a `paths` list in YAML frontmatter, the one conditional Cline
//!   documents; every other rule is plain markdown. Cline has no on-demand
//!   rules, so one is written always-on with a line saying when to use it.
//!   `.cursorrules`, `.windsurfrules` and `AGENTS.md`, which Cline also reads,
//!   belong to other targets and are not captured as Cline rules. Only the
//!   `*.md` files directly in a rules directory are read; capture lists the rest.
//! * Commands (Cline calls them workflows): `<name>.md` in `~/.cline/workflows/`
//!   (also read from `~/Documents/Cline/Workflows/`) and a project's
//!   `.cline/workflows/` (and `.clinerules/workflows/`), plain markdown with the
//!   prompt as the body. Cline's current docs name these folders but describe no
//!   file format, so kitbag writes no frontmatter.
//! * Skills: `<name>/SKILL.md` in `~/.cline/skills/` and a project's
//!   `.cline/skills/`, where kitbag writes them; capture also reads
//!   `.clinerules/skills/` and `.agents/skills/` (`~/.agents/skills/` at user
//!   scope), which kitbag never writes to.
//! * Hooks and agents are not carried (issue #14 for agents). Cline's hooks page now only
//!   points to its TypeScript SDK plugins (#12), not a hook file (checked 2026-11-27).
//!
//! Docs: <https://docs.cline.bot/mcp/mcp-overview>,
//! <https://docs.cline.bot/getting-started/config>,
//! <https://docs.cline.bot/customization/cline-rules>,
//! <https://docs.cline.bot/customization/skills>.

use std::path::PathBuf;

use anyhow::Result;
use serde_json::{Map, Value, json};

use super::common::{
    Resolver, add_command, add_rule, capture_instructions, capture_md_files, capture_skills_dir,
    dir_holding, import_server, merge_json_servers_off, note_shadowed_skill, note_uncaptured,
    rule_from_lead_line, rule_lead_line, server_entries, server_from_json, servers_for,
    set_disabled_true, skill_name_mismatch, skill_with_required_frontmatter, write_command,
    write_instructions, write_rule, write_skill,
};
use super::{Adapter, ApplyOptions, Captured, Env, Feature, Scope, Support, Work};
use crate::edit::json::parse_value;
use crate::frontmatter;
use crate::model::{Profile, Rule, RuleMode, Transport};
use crate::profile_io::command_from_markdown;

const ID: &str = "cline";
/// Server keys kitbag writes; anything else (`autoApprove`, `timeout` ...) is kept.
const MANAGED_KEYS: &[&str] = &[
    "command", "args", "env", "url", "headers", "type", "disabled",
];
/// The file kitbag writes instructions to in the global rules directory.
const RULES_FILE: &str = "kitbag.md";
/// Folders of `.clinerules/` that hold workflows and skills (and hooks), not rules.
const CLINERULES_SUBDIRS: &[&str] = &["workflows/", "skills/"];

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

/// The folders Cline reads at one scope, the first being where kitbag writes.
struct Dirs {
    rules: Vec<PathBuf>,
    workflows: Vec<PathBuf>,
    skills: Vec<PathBuf>,
}

fn dirs(env: &Env) -> Dirs {
    match env.scope {
        Scope::User => {
            let root = config_root(env);
            let docs = env.home.join("Documents").join("Cline");
            Dirs {
                rules: vec![
                    root.join("rules"),
                    docs.join("Rules"),
                    env.home.join("Cline").join("Rules"),
                ],
                workflows: vec![root.join("workflows"), docs.join("Workflows")],
                skills: vec![root.join("skills"), env.home.join(".agents").join("skills")],
            }
        }
        Scope::Project => {
            let cline = env.project.join(".cline");
            let legacy = env.project.join(".clinerules");
            Dirs {
                rules: vec![cline.join("rules"), legacy.clone()],
                workflows: vec![cline.join("workflows"), legacy.join("workflows")],
                skills: vec![
                    cline.join("skills"),
                    legacy.join("skills"),
                    env.project.join(".agents").join("skills"),
                ],
            }
        }
    }
}

/// A rule file as a rule. `paths: []` means Cline never applies it, which
/// is not a rule that is always on, so it is left out.
fn rule_from_cline(name: &str, text: &str) -> Option<Rule> {
    let paths = frontmatter::parse(text).data.get("paths").cloned();
    if matches!(&paths, Some(Value::Array(a)) if a.is_empty()) {
        return None;
    }
    Some(rule_from_lead_line(name, text))
}

/// A profile rule as a Cline rule file.
fn cline_rule(r: &Rule) -> String {
    let body = r.body.trim();
    match r.mode() {
        RuleMode::Globs => frontmatter::render(&[("paths", json!(r.globs))], body),
        _ => match rule_lead_line(r) {
            Some(lead) => format!("{lead}\n\n{body}\n"),
            None => format!("{body}\n"),
        },
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
                Feature::Rules | Feature::Commands | Feature::Skills => Support::Native,
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
        let d = dirs(env);
        out.push(("rules", d.rules[0].clone()));
        out.push(("commands", d.workflows[0].clone()));
        out.push(("skills", d.skills[0].clone()));
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

        let own = instructions_file(env);
        let d = dirs(env);

        // Rule files first: one of them beats a stale copy of the same rule
        // folded into the instructions file by an older kitbag.
        for dir in &d.rules {
            capture_md_files(
                &mut cap,
                dir,
                ".md",
                true,
                Some(&own),
                ID,
                &mut |cap, name, text, path| {
                    cap.sources.push(path.to_path_buf());
                    let Some(rule) = rule_from_cline(name, text) else {
                        cap.info(
                        ID,
                        format!(
                            "rule '{name}' has `paths: []`, so Cline never applies it; it was not captured"
                        ),
                    );
                        return;
                    };
                    if cap.profile.rules.iter().any(|r| r.name == name) {
                        cap.info(
                            ID,
                            format!(
                                "{} has the name of a rule already captured and was skipped",
                                path.display()
                            ),
                        );
                        return;
                    }
                    cap.rule_files.push((name.to_string(), path.to_path_buf()));
                    add_rule(cap, rule, ID);
                },
            )?;
            let legacy = dir.file_name().is_some_and(|n| n == ".clinerules");
            note_uncaptured(
                &mut cap,
                ID,
                dir,
                &env.home,
                "kitbag reads only the *.md rule files directly in a rules folder",
                &|rel| {
                    (rel.ends_with(".md") && !rel.contains('/'))
                        || crate::plan_fs::join_rel(dir, rel) == own
                        || (legacy && CLINERULES_SUBDIRS.iter().any(|p| rel.starts_with(p)))
                },
            );
        }
        if env.scope == Scope::Project && env.project.join(".clinerules").is_file() {
            cap.info(
                ID,
                "a .clinerules file was not read: Cline's docs describe .clinerules as a directory of rule files",
            );
        }

        for dir in &d.workflows {
            capture_md_files(
                &mut cap,
                dir,
                ".md",
                true,
                None,
                ID,
                &mut |cap, name, text, path| {
                    cap.sources.push(path.to_path_buf());
                    add_command(cap, command_from_markdown(name, text), ID);
                },
            )?;
            note_uncaptured(
                &mut cap,
                ID,
                dir,
                &env.home,
                "kitbag reads only the *.md workflow files directly in a workflows folder",
                &|rel| rel.ends_with(".md") && !rel.contains('/'),
            );
        }
        for dir in &d.skills {
            capture_skills_dir(&mut cap, dir, ID)?;
        }

        capture_instructions(&mut cap, &own, ID)?;
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
            merge_json_servers_off(
                work,
                (&file, &["mcpServers"]),
                &servers,
                MANAGED_KEYS,
                opts,
                ID,
                Some((&set_disabled_true, "disabled: true")),
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
        // Rules are files of their own, so at user scope (a file only kitbag
        // writes) the copy an older kitbag folded into the instructions is
        // removed; in AGENTS.md it is left to the other targets that fold.
        let own = instructions_file(env);
        let user = env.scope == Scope::User;
        let without_rules;
        let (instr_profile, fold) = if user {
            without_rules = Profile {
                rules: Vec::new(),
                ..profile.clone()
            };
            (&without_rules, true)
        } else {
            (profile, false)
        };
        write_instructions(work, &own, instr_profile, opts, fold, ID)?;

        let d = dirs(env);
        for r in &profile.rules {
            let file = format!("{}.md", r.name);
            if user && file == RULES_FILE {
                work.warn(
                    ID,
                    format!(
                        "rule '{}' skipped: its file is where kitbag writes the instructions",
                        r.name
                    ),
                );
                continue;
            }
            if r.mode() == RuleMode::OnDemand {
                work.info(
                    ID,
                    format!("rule '{}' is on-demand in the profile; Cline has no on-demand rules, so it always loads, with a line saying when to use it", r.name),
                );
            }
            let dir = dir_holding(work, &d.rules, &file)?;
            write_rule(
                work,
                &dir.join(&file),
                cline_rule(r).as_bytes(),
                opts,
                ID,
                &r.name,
            )?;
        }
        if !user && !profile.rules.is_empty() && work.targets.iter().any(|t| t == "codex") {
            work.info(
                ID,
                "Codex folds the rules into the shared AGENTS.md, and Cline reads that file as well as .cline/rules/, so in this project Cline sees the rules twice",
            );
        }
        for c in &profile.commands {
            // Plain markdown: the body is the prompt. Cline's docs describe no
            // workflow frontmatter, so none is written.
            let file = format!("{}.md", c.name);
            let dir = dir_holding(work, &d.workflows, &file)?;
            let body = format!("{}\n", c.body.trim());
            write_command(work, &dir.join(&file), body.as_bytes(), opts, ID, &c.name)?;
        }
        for s in &profile.skills {
            let s = skill_with_required_frontmatter(s);
            if let Some(w) = skill_name_mismatch(&s) {
                work.warn(ID, w);
            }
            note_shadowed_skill(work, ID, &d.skills, &s.name);
            write_skill(work, &d.skills[0].join(&s.name), &s, opts, ID)?;
        }
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
