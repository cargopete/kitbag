//! xAI Grok Build CLI (`grok`).
//!
//! Sources: <https://docs.x.ai/build/overview> and the user guide and source in
//! <https://github.com/xai-org/grok-build> (`crates/codegen/xai-grok-pager/docs/user-guide/`,
//! `xai-grok-agent/src/prompt/agents_md.rs`, `xai-grok-tools/src/implementations/skills/`,
//! `xai-grok-config/src/mcp_server_config.rs`).
//!
//! * Home: `$GROK_HOME`, default `~/.grok`.
//! * MCP: `[mcp_servers.<name>]` tables in `$GROK_HOME/config.toml` or a
//!   project's `.grok/config.toml` (a project server replaces a user one of the
//!   same name). `command`/`args`/`env`/`cwd` for stdio, `url`/`headers` for
//!   streamable HTTP, plus `type = "sse"` (or a URL ending in `/sse`) for SSE;
//!   `bearer_token_env_var` names a variable holding a bearer token.
//!   `enabled = false`, or the name in the user config's
//!   `disabled_mcp_servers`, turns one off. Grok expands `${VAR}` and
//!   `${VAR:-default}` itself, so references are written as they are.
//! * Instructions: `$GROK_HOME/AGENTS.md` and `AGENTS.md` in each directory
//!   from the repo root down (shared with Codex, Cursor, Antigravity and
//!   opencode). Grok also loads `CLAUDE.md` next to it.
//! * Rules: `*.md` directly in `$GROK_HOME/rules/` or `.grok/rules/`, always
//!   loaded, frontmatter stripped. There are no file-scoped rules, so a rule
//!   with globs is written with an "applies when" line and loads every time.
//! * Commands: flat `*.md` files in `$GROK_HOME/commands/` or `.grok/commands/`
//!   (Claude Code's legacy layout); `$ARGUMENTS` is expanded, positionals are
//!   `$0`, `$1`, ... counted from zero.
//! * Skills: `$GROK_HOME/skills/<name>/SKILL.md` or `.grok/skills/<name>/`.
//! * Project files load only in folders the user has trusted.

use std::path::{Path, PathBuf};
use std::sync::LazyLock;

use anyhow::Result;
use regex::Regex;
use serde_json::{Map, Value, json};
use toml_edit::Item;

use super::common::{
    MdKind, add_rule, capture_instructions, capture_markdown_dir, capture_skills_dir,
    import_server, merge_server_def, off_skipped_server, read_text_item, record_server_write,
    render_server, server_entries, servers_for, set_enabled_false, skipped_servers, str_of,
    string_list, string_map, write_instructions, write_skill,
};
use super::{Adapter, ApplyOptions, Captured, Env, Feature, Scope, Support, Work};
use crate::edit::keep_line_endings;
use crate::edit::toml::{
    item_to_json, json_to_table, json_to_value, parse, set_keep_decor, table_mut,
};
use crate::model::{McpServer, Profile, Rule, RuleMode, Transport, is_valid_name};
use crate::profile_io::{command_to_markdown, rule_from_markdown, rule_to_markdown};
use crate::secrets::{RefStyle, reference, sole_ref};

const ID: &str = "grok-build";
/// Server keys kitbag writes; anything else (timeouts, oauth, setup) is preserved.
const MANAGED_KEYS: &[&str] = &[
    "command",
    "args",
    "env",
    "cwd",
    "url",
    "headers",
    "bearer_token_env_var",
    "type",
    "enabled",
];
static BEARER: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)^Bearer\s+(.+)$").expect("valid regex"));
/// Lines kitbag puts at the top of a rule body to say when it applies; Grok
/// strips frontmatter from rule files, so this is what the model sees.
const APPLIES_PREFIX: &str = "_Applies when working with files matching: ";
const ON_DEMAND_LINE: &str = "_Use when relevant to the task._";

pub struct GrokBuild;

struct Paths {
    config: PathBuf,
    instructions: PathBuf,
    rules: PathBuf,
    commands: PathBuf,
    skills: PathBuf,
}

pub(crate) fn grok_home(env: &Env) -> PathBuf {
    env.location_var("GROK_HOME")
        .unwrap_or_else(|| env.home.join(".grok"))
}

fn paths(env: &Env) -> Paths {
    match env.scope {
        Scope::User => {
            let d = grok_home(env);
            Paths {
                config: d.join("config.toml"),
                instructions: d.join("AGENTS.md"),
                rules: d.join("rules"),
                commands: d.join("commands"),
                skills: d.join("skills"),
            }
        }
        Scope::Project => {
            let d = env.project.join(".grok");
            Paths {
                config: d.join("config.toml"),
                instructions: env.project.join("AGENTS.md"),
                rules: d.join("rules"),
                commands: d.join("commands"),
                skills: d.join("skills"),
            }
        }
    }
}

/// A profile rule as a Grok rule file: profile frontmatter (kept for capture;
/// Grok strips it) and, for rules that aren't always on, a first line saying
/// when the rule applies.
fn grok_rule(r: &Rule) -> String {
    let lead = match r.mode() {
        RuleMode::Globs => Some(format!(
            "{APPLIES_PREFIX}{}_",
            r.globs
                .iter()
                .map(|g| format!("`{g}`"))
                .collect::<Vec<_>>()
                .join(", ")
        )),
        RuleMode::OnDemand => Some(ON_DEMAND_LINE.to_string()),
        RuleMode::Always => None,
    };
    let body = match lead {
        Some(l) => format!("{l}\n\n{}", r.body.trim()),
        None => r.body.trim().to_string(),
    };
    rule_to_markdown(&Rule { body, ..r.clone() })
}

/// Parse a Grok rule file back, dropping the "applies when" line kitbag added.
fn rule_from_grok(name: &str, text: &str) -> Rule {
    let mut r = rule_from_markdown(name, text);
    let first = r.body.lines().next().unwrap_or_default();
    let ours = (first.starts_with(APPLIES_PREFIX) && !r.globs.is_empty())
        || (first == ON_DEMAND_LINE && r.mode() == RuleMode::OnDemand);
    if ours {
        r.body = r.body[first.len()..].trim().to_string();
    }
    r
}

/// `*.md` files directly in `dir` (Grok doesn't look in subdirectories), sorted.
fn rule_files(dir: &Path) -> Vec<PathBuf> {
    let Ok(rd) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut out: Vec<PathBuf> = rd
        .flatten()
        .map(|e| e.path())
        .filter(|p| {
            p.is_file()
                && p.extension()
                    .and_then(|e| e.to_str())
                    .is_some_and(|e| e.eq_ignore_ascii_case("md"))
        })
        .collect();
    out.sort();
    out
}

fn capture_rules(cap: &mut Captured, dir: &Path) {
    if cap.escapes(dir, ID) {
        return;
    }
    for path in rule_files(dir) {
        let Some(name) = path
            .file_stem()
            .and_then(|s| s.to_str())
            .map(str::to_string)
        else {
            continue;
        };
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
        cap.sources.push(path.clone());
        cap.rule_files.push((name.clone(), path.clone()));
        add_rule(cap, rule_from_grok(&name, &text), ID);
    }
}

/// Render one profile server as a Grok `[mcp_servers.<name>]` table.
fn grok_server(s: &McpServer) -> Map<String, Value> {
    let mut t = Map::new();
    match s.transport() {
        Transport::Stdio => {
            t.insert(
                "command".into(),
                json!(s.command.clone().unwrap_or_default()),
            );
            if !s.args.is_empty() {
                t.insert("args".into(), json!(s.args));
            }
            if !s.env.is_empty() {
                t.insert("env".into(), json!(s.env));
            }
            if let Some(cwd) = &s.cwd {
                t.insert("cwd".into(), json!(cwd));
            }
        }
        Transport::Http | Transport::Sse => {
            t.insert("url".into(), json!(s.url.clone().unwrap_or_default()));
            if s.transport() == Transport::Sse {
                t.insert("type".into(), json!("sse"));
            }
            let mut headers = Map::new();
            for (k, v) in &s.headers {
                // `Authorization: Bearer ${VAR}` is Grok's own bearer_token_env_var,
                // which is what capture reads back.
                let bearer = BEARER
                    .captures(v)
                    .and_then(|c| sole_ref(&c[1]))
                    .filter(|r| r.fallback.is_none());
                match bearer {
                    Some(r) if k.eq_ignore_ascii_case("authorization") => {
                        t.insert("bearer_token_env_var".into(), json!(r.name));
                    }
                    _ => {
                        headers.insert(k.clone(), json!(v));
                    }
                }
            }
            if !headers.is_empty() {
                t.insert("headers".into(), Value::Object(headers));
            }
        }
    }
    if s.disabled {
        t.insert("enabled".into(), json!(false));
    }
    t
}

fn import(cap: &mut Captured, name: &str, def: &Map<String, Value>, listed_off: bool) {
    let mut headers = string_map(def.get("headers"));
    if let Some(var) = str_of(def.get("bearer_token_env_var")) {
        headers
            .entry("Authorization".into())
            .or_insert_with(|| format!("Bearer {}", reference(&var)));
    }
    let dropped: Vec<&str> = def
        .keys()
        .map(String::as_str)
        .filter(|k| !MANAGED_KEYS.contains(k) && !["urlTemplate", "url_template"].contains(k))
        .collect();
    if !dropped.is_empty() {
        cap.info(
            ID,
            format!(
                "MCP server '{name}': Grok Build-specific keys not captured: {}",
                dropped.join(", ")
            ),
        );
    }
    let url = str_of(def.get("url"))
        .or_else(|| str_of(def.get("urlTemplate")))
        .or_else(|| str_of(def.get("url_template")));
    // Grok's own rule: `type = "sse"`, or a URL ending in /sse, is SSE.
    let sse = def.get("command").is_none()
        && (str_of(def.get("type")).is_some_and(|t| t.eq_ignore_ascii_case("sse"))
            || url.as_deref().is_some_and(|u| u.ends_with("/sse")));
    let s = McpServer {
        transport: sse.then_some(Transport::Sse),
        command: str_of(def.get("command")),
        args: string_list(def.get("args")),
        env: string_map(def.get("env")),
        cwd: str_of(def.get("cwd")),
        url,
        headers,
        disabled: listed_off || def.get("enabled") == Some(&Value::Bool(false)),
        ..Default::default()
    };
    import_server(cap, ID, name, s, false);
}

/// Names in config.toml's top-level `disabled_mcp_servers` (what `grok mcp
/// disable` writes): Grok keeps those servers off whatever their entry says.
fn disabled_list(doc: &toml_edit::DocumentMut) -> Vec<String> {
    doc.get("disabled_mcp_servers")
        .and_then(Item::as_array)
        .map(|a| {
            a.iter()
                .filter_map(|v| v.as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default()
}

/// Does `path` (as planned so far) hold this profile's managed instructions block?
fn has_block(work: &Work, path: &Path, block_id: &str) -> bool {
    work.fs
        .read(path)
        .ok()
        .flatten()
        .is_some_and(|t| t.contains(&format!("<!-- kitbag:begin {block_id} -->")))
}

impl Adapter for GrokBuild {
    fn id(&self) -> &'static str {
        ID
    }
    fn name(&self) -> &'static str {
        "Grok Build"
    }
    fn docs(&self) -> &'static str {
        "https://docs.x.ai/build/overview"
    }
    fn scopes(&self) -> &'static [Scope] {
        &[Scope::User, Scope::Project]
    }
    fn support(&self, _scope: Scope) -> [(Feature, Support); 8] {
        [
            (Feature::Mcp, Support::Native),
            (Feature::Instructions, Support::Native),
            // Rules have a home of their own; scoped ones load for every file (noted).
            (Feature::Rules, Support::Native),
            (Feature::Commands, Support::Native),
            (Feature::Skills, Support::Native),
            (Feature::Agents, Support::None),
            (Feature::OutputStyles, Support::None),
            (Feature::Settings, Support::None),
        ]
    }

    fn locations(&self, env: &Env) -> Vec<(&'static str, PathBuf)> {
        let p = paths(env);
        vec![
            ("mcp", p.config),
            ("instructions", p.instructions),
            ("rules", p.rules),
            ("commands", p.commands),
            ("skills", p.skills),
        ]
    }

    fn detect(&self, env: &Env) -> bool {
        match env.scope {
            Scope::User => grok_home(env).exists(),
            Scope::Project => env.project.join(".grok").exists(),
        }
    }

    fn capture(&self, env: &Env) -> Result<Captured> {
        let mut cap = Captured::for_env(env);
        let p = paths(env);
        if let Some(text) = cap.read_config(&p.config, ID)? {
            cap.sources.push(p.config.clone());
            let doc = parse(Some(&text), &p.config)?;
            let servers = doc.get("mcp_servers").map(item_to_json);
            let off = disabled_list(&doc);
            for (name, def) in
                server_entries(&mut cap, ID, &p.config, "mcp_servers", servers.as_ref())
            {
                let listed_off = off.iter().any(|n| n == name);
                import(&mut cap, name, def, listed_off);
            }
        }
        capture_instructions(&mut cap, &p.instructions, ID)?;
        // Rules another tool folded into a shared AGENTS.md (Codex) come back
        // out of capture_instructions; Grok's own rules are the files in rules/.
        if !cap.profile.rules.is_empty() {
            let names: Vec<String> = cap.profile.rules.drain(..).map(|r| r.name).collect();
            cap.info(
                ID,
                format!(
                    "rules folded into AGENTS.md by another tool were not captured ({}); capture that tool for them",
                    names.join(", ")
                ),
            );
        }
        capture_rules(&mut cap, &p.rules);
        capture_markdown_dir(&mut cap, &p.commands, ".md", MdKind::Command, ID)?;
        capture_skills_dir(&mut cap, &p.skills, ID)?;
        let agents = match env.scope {
            Scope::User => grok_home(env).join("agents"),
            Scope::Project => env.project.join(".grok").join("agents"),
        };
        if agents.is_dir() {
            cap.info(
                ID,
                format!(
                    "{} (Grok Build agent definitions) not captured; docs.x.ai documents no file format for them",
                    agents.display()
                ),
            );
        }
        super::hook_files::capture_for(&mut cap, env, &super::hook_files::GROK)?;
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

        // MCP servers.
        let original = work.fs.read(&p.config)?;
        let mut doc = parse(original.as_deref(), &p.config)?;
        let mut changed = false;
        let mut wanted: Vec<(String, Map<String, Value>)> = Vec::new();
        for (name, s) in servers_for(profile, ID, work, true) {
            let s = render_server(s, RefStyle::Dollar, env, work);
            wanted.push((name.clone(), grok_server(&s)));
        }
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
        let off = disabled_list(&doc);
        for (name, want) in &wanted {
            if want.get("enabled") != Some(&json!(false)) && off.contains(name) {
                work.warn(
                    ID,
                    format!("MCP server '{name}' is listed in disabled_mcp_servers in {}, so Grok Build keeps it off; `grok mcp enable {name}` turns it back on", p.config.display()),
                );
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
        if changed {
            work.fs.write(
                &p.config,
                keep_line_endings(original.as_deref(), doc.to_string()),
            )?;
        }
        if env.scope == Scope::Project && !profile.mcp_servers.is_empty() {
            work.info(ID, "a server in .grok/config.toml replaces a user server of the same name entirely (no merging)");
        }

        // Instructions, then rules in their own files.
        write_instructions(work, &p.instructions, profile, opts, false, ID)?;
        for r in &profile.rules {
            match r.mode() {
                RuleMode::Globs => work.info(
                    ID,
                    format!("rule '{}' is file-scoped in the profile; Grok Build has no file-scoped rules, so it always loads, with a line saying which files it is for", r.name),
                ),
                RuleMode::OnDemand => work.info(
                    ID,
                    format!("rule '{}' is on-demand in the profile; Grok Build has no on-demand rules, so it always loads", r.name),
                ),
                RuleMode::Always => {}
            }
            let path = p.rules.join(format!("{}.md", r.name));
            super::common::write_rule(work, &path, grok_rule(r).as_bytes(), opts, ID, &r.name)?;
        }

        if env.scope == Scope::Project
            && !profile.rules.is_empty()
            && work.targets.iter().any(|t| t == "codex")
        {
            work.info(
                ID,
                "Codex folds the rules into the shared AGENTS.md, and Grok Build reads that file as well as .grok/rules/, so in this project Grok sees the rules twice",
            );
        }

        // Grok reads CLAUDE.md files too: the same block there loads twice.
        if profile.instructions.is_some() {
            let claude = match env.scope {
                Scope::User => vec![env.home.join(".claude").join("CLAUDE.md")],
                Scope::Project => vec![
                    env.project.join("CLAUDE.md"),
                    env.project.join(".claude").join("CLAUDE.md"),
                ],
            };
            if let Some(c) = claude.iter().find(|c| has_block(work, c, &opts.block_id)) {
                work.info(
                    ID,
                    format!(
                        "Grok Build also loads {}, which has the same instructions, so they load twice; set `agents = false` under [compat.claude] in ~/.grok/config.toml to stop it reading Claude's files{}",
                        crate::ui::tildify(c, &env.home),
                        if env.scope == Scope::Project { " (a top-level CLAUDE.md is still read)" } else { "" }
                    ),
                );
            }
        }

        for c in &profile.commands {
            if (0..=9).any(|n| {
                let pat = format!("${n}");
                c.body.match_indices(&pat).any(|(i, _)| {
                    !c.body[i + pat.len()..].starts_with(|ch: char| ch.is_ascii_digit())
                })
            }) {
                work.warn(
                    ID,
                    format!("command '{}' uses positional $N placeholders; Grok Build counts them from $0 (the first argument), so check the numbering", c.name),
                );
            }
            let path = p.commands.join(format!("{}.md", c.name));
            let body = command_to_markdown(c, true);
            super::common::write_command(work, &path, body.as_bytes(), opts, ID, &c.name)?;
        }
        for s in &profile.skills {
            write_skill(work, &p.skills.join(&s.name), s, opts, ID)?;
        }
        if env.scope == Scope::Project
            && (profile.instructions.is_some()
                || !profile.rules.is_empty()
                || !profile.commands.is_empty()
                || !profile.skills.is_empty())
        {
            work.info(
                ID,
                "Grok Build reads a project's AGENTS.md, rules, commands and skills only once the folder is trusted (`grok --trust`, or accept the prompt)",
            );
        }
        if !profile.agents.is_empty() {
            work.info(
                ID,
                format!(
                    "{} custom agent(s) not written: docs.x.ai names `.grok/agents/` but documents no agent file format",
                    profile.agents.len()
                ),
            );
        }
        super::hook_files::apply_for(profile, env, opts, work, &super::hook_files::GROK)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rule(globs: &[&str], always: Option<bool>) -> Rule {
        Rule {
            name: "ts".into(),
            description: Some("TypeScript".into()),
            globs: globs.iter().map(|g| g.to_string()).collect(),
            always_apply: always,
            body: "Strict mode.\n\nNo any.".into(),
            ..Default::default()
        }
    }

    #[test]
    fn rule_round_trips_through_a_grok_rule_file() {
        for r in [
            rule(&["src/**/*.ts", "*.tsx"], None),
            rule(&[], Some(false)),
            rule(&[], None),
            rule(&["x/*"], Some(true)),
        ] {
            let text = grok_rule(&r);
            assert_eq!(rule_from_grok("ts", &text), r, "{text}");
        }
    }

    #[test]
    fn scoped_rule_says_which_files_it_is_for() {
        let text = grok_rule(&rule(&["src/**/*.ts"], None));
        let body = crate::frontmatter::parse(&text).body;
        assert!(
            body.trim_start()
                .starts_with("_Applies when working with files matching: `src/**/*.ts`_"),
            "{text}"
        );
    }

    #[test]
    fn a_hand_written_rule_keeps_its_first_line() {
        let r = rule_from_grok(
            "x",
            "_Applies when working with files matching: `a`_\n\nBody",
        );
        assert!(
            r.body.starts_with("_Applies"),
            "no globs, so the line is the user's"
        );
    }
}
