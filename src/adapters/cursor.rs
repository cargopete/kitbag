//! Cursor (editor and CLI).
//!
//! * MCP: `~/.cursor/mcp.json` / `.cursor/mcp.json`. Cursor interpolates
//!   `${env:VAR}` in command, args, env, url and headers, so references are
//!   rewritten into that syntax and secrets never touch disk.
//! * Rules: `.cursor/rules/*.mdc` (project). User rules live in Cursor's
//!   settings UI, not in files, so they cannot be written at user scope.
//! * Instructions: `AGENTS.md` at the project root.
//! * Commands: `.cursor/commands/*.md` (plain markdown). Skills: `.cursor/skills/`.

use std::path::PathBuf;
use std::sync::LazyLock;

use anyhow::Result;
use regex::Regex;
use serde_json::{Map, Value, json};

use super::common::{
    add_command, add_instructions, add_rule, capture_instructions, capture_skills_dir,
    import_server, merge_json_servers, name_from_rel, read_text_item, render_server,
    server_entries, server_from_json, servers_for, write_instructions, write_owned, write_skill,
};
use super::{Adapter, ApplyOptions, Captured, Env, Feature, Scope, Support, Work};
use crate::edit::block;
use crate::edit::json::parse_value;
use crate::frontmatter;
use crate::model::{Command, Profile, Rule, RuleMode, Transport, is_valid_name};
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
            }
        }
    }
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
    fn support(&self, scope: Scope) -> [(Feature, Support); 6] {
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
            (Feature::Settings, Support::None),
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
            for rel in walk_files(dir, 500)? {
                if !rel.ends_with(".mdc") {
                    continue;
                }
                let path = join_rel(dir, &rel);
                let Some(text) = read_text_item(&mut cap, &path, ID) else {
                    continue;
                };
                let name = name_from_rel(&rel, ".mdc");
                if !is_valid_name(&name) {
                    cap.warn(
                        ID,
                        format!("skipped {}: unsupported file name", path.display()),
                    );
                    continue;
                }
                cap.sources.push(path);
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
                add_rule(&mut cap, rule_from_mdc(&name, &text), ID);
            }
        }

        for rel in walk_files(&p.commands, 500)? {
            if !rel.ends_with(".md") {
                continue;
            }
            let path = join_rel(&p.commands, &rel);
            let Some(text) = read_text_item(&mut cap, &path, ID) else {
                continue;
            };
            let name = name_from_rel(&rel, ".md");
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
            let s = super::common::skill_with_required_frontmatter(s);
            write_skill(work, &p.skills.join(&s.name), &s, opts, ID)?;
        }
        if profile.settings.contains_key(ID) {
            work.info(
                ID,
                "Cursor settings are managed in its UI; settings.cursor was not applied",
            );
        }
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
