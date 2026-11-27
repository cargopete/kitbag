//! Roo Code (the VS Code extension `RooVeterinaryInc.roo-cline`): MCP servers,
//! instructions, rules, commands and skills.
//!
//! * MCP, user: `mcp_settings.json` in the extension's `settings/` folder of
//!   the editor's global storage (`<user data>/User/globalStorage/
//!   rooveterinaryinc.roo-cline/settings/`). Roo's docs don't give the path;
//!   the file name and the `settings` folder are from its source
//!   (`GlobalFileNames.mcpSettings`, `getSettingsDirectoryPath`). Roo is an
//!   extension, so the folder lives under the host editor's user data
//!   directory: `~/.config/Code` (`$XDG_CONFIG_HOME/Code`) on Linux,
//!   `~/Library/Application Support/Code` on macOS, `%APPDATA%\Code` on Windows.
//!   kitbag uses the first of VS Code, VS Code Insiders, VSCodium and Cursor
//!   whose folder for the extension exists, and VS Code when none does. It
//!   can't see the `roo-cline.customStoragePath` setting.
//! * MCP, project: `.roo/mcp.json`, which wins over the user file for a server
//!   of the same name. Both hold `mcpServers`. Local servers take `command`,
//!   `args`, `env` and `cwd` (`type` is optional); remote servers take `url`
//!   and `headers` and need `type: "streamable-http"` or `"sse"`. `disabled`,
//!   `timeout`, `alwaysAllow`, `watchPaths` and `disabledTools` are Roo's own
//!   keys on an entry and are kept. Roo expands `${env:VAR}`, so references are
//!   written that way and no secret reaches the file.
//! * Instructions: a managed block in `~/.roo/rules/kitbag.md` (Roo reads every
//!   file in `~/.roo/rules/`, alphabetically), and `AGENTS.md` at the project
//!   root, which Roo reads and which is shared with Codex, Cursor,
//!   Antigravity, opencode, Grok Build and Windsurf. Instructions only:
//!   profile rules are files of their own (below), and `kitbag` is not a
//!   usable rule name at user scope.
//! * Rules: `<name>.md` in `~/.roo/rules/` and a project's `.roo/rules/`
//!   (Roo reads those folders recursively; capture takes every `*.md`
//!   under them). The docs describe no frontmatter for them, so none is
//!   written: a rule with globs, or an on-demand one, loads every time, with
//!   a first line saying when it applies. Mode-specific rules
//!   (`rules-<mode>/`, `.roorules-<mode>`) and `.roorules` are not captured
//!   and not written; capture lists them. Roo falls back to `.roorules` only
//!   while `.roo/rules/` is empty, so kitbag creates no empty folder, and
//!   warns when its first rule file there would hide a `.roorules`.
//! * Commands: `<name>.md` in `~/.roo/commands/` and `.roo/commands/`, with
//!   `description`, `argument-hint` and `mode` frontmatter (Roo's docs list
//!   those three; the file name is the command name).
//! * Skills: `<name>/SKILL.md` in `~/.roo/skills/` and `.roo/skills/`, where
//!   kitbag writes them; capture also reads the cross-agent `~/.agents/skills/`
//!   and `.agents/skills/`, which kitbag never writes to. Mode-specific
//!   `skills-<mode>/` folders are listed by capture, not read. Roo needs the
//!   name to be 1-64 lowercase letters, digits and hyphens (no leading,
//!   trailing or doubled hyphen) and to match the folder.
//! * Custom modes, hooks and settings are not carried (Roo's docs describe no hooks,
//!   checked 2026-11-27).
//!
//! Docs: <https://roocodeinc.github.io/Roo-Code/features/mcp/using-mcp-in-roo>,
//! <https://roocodeinc.github.io/Roo-Code/features/custom-instructions>,
//! <https://roocodeinc.github.io/Roo-Code/features/skills>,
//! <https://roocodeinc.github.io/Roo-Code/features/slash-commands>.

use std::path::PathBuf;

use anyhow::Result;
use serde_json::{Map, Value, json};

use super::common::{
    add_command, add_rule, capture_instructions, capture_md_files, capture_skills_dir,
    import_server, merge_json_servers_off, note_uncaptured, render_server, rule_from_lead_line,
    rule_lead_line, server_entries, server_from_json, servers_for, set_disabled_true,
    skill_name_mismatch, skill_with_required_frontmatter, write_command, write_instructions,
    write_rule, write_skill,
};
use super::{Adapter, ApplyOptions, Captured, Env, Feature, Os, Scope, Support, Work};
use crate::edit::json::parse_value;
use crate::frontmatter;
use crate::model::{Command, Profile, Rule, RuleMode, Transport};
use crate::profile_io::command_from_markdown;
use crate::secrets::RefStyle;

const ID: &str = "roo";
/// Server keys kitbag writes; anything else (`alwaysAllow`, `timeout`,
/// `watchPaths`, `disabledTools` ...) is kept.
const MANAGED_KEYS: &[&str] = &[
    "command", "args", "env", "cwd", "url", "headers", "type", "disabled",
];
/// The file kitbag writes in the global rules directory.
const RULES_FILE: &str = "kitbag.md";
/// The extension's folder in a VS Code user data directory (lower-cased id).
const EXTENSION_DIR: &str = "rooveterinaryinc.roo-cline";
/// Editors Roo runs in, as the name of their user data directory.
const HOSTS: &[&str] = &["Code", "Code - Insiders", "VSCodium", "Cursor"];

pub struct Roo;

/// An editor's `User` directory.
fn user_data_dir(env: &Env, host: &str) -> PathBuf {
    match env.os {
        Os::Windows => env
            .location_var("APPDATA")
            .unwrap_or_else(|| env.home.join("AppData").join("Roaming"))
            .join(host)
            .join("User"),
        Os::MacOs => env
            .home
            .join("Library")
            .join("Application Support")
            .join(host)
            .join("User"),
        Os::Linux => env
            .location_var("XDG_CONFIG_HOME")
            .unwrap_or_else(|| env.home.join(".config"))
            .join(host)
            .join("User"),
    }
}

fn storage_dir(env: &Env, host: &str) -> PathBuf {
    user_data_dir(env, host)
        .join("globalStorage")
        .join(EXTENSION_DIR)
}

/// The editors that have Roo's global storage on this machine.
fn hosts_with_roo(env: &Env) -> Vec<&'static str> {
    HOSTS
        .iter()
        .copied()
        .filter(|h| storage_dir(env, h).is_dir())
        .collect()
}

/// The user-level MCP file: in the first editor that has Roo, else VS Code's.
fn user_mcp_file(env: &Env) -> PathBuf {
    let host = hosts_with_roo(env).first().copied().unwrap_or(HOSTS[0]);
    storage_dir(env, host)
        .join("settings")
        .join("mcp_settings.json")
}

fn mcp_file(env: &Env) -> PathBuf {
    match env.scope {
        Scope::User => user_mcp_file(env),
        Scope::Project => env.project.join(".roo").join("mcp.json"),
    }
}

fn instructions_file(env: &Env) -> PathBuf {
    match env.scope {
        Scope::User => env.home.join(".roo").join("rules").join(RULES_FILE),
        Scope::Project => env.project.join("AGENTS.md"),
    }
}

/// `.roo/` at this scope.
fn roo_dir(env: &Env) -> PathBuf {
    match env.scope {
        Scope::User => env.home.join(".roo"),
        Scope::Project => env.project.join(".roo"),
    }
}

fn rules_dir(env: &Env) -> PathBuf {
    roo_dir(env).join("rules")
}

fn commands_dir(env: &Env) -> PathBuf {
    roo_dir(env).join("commands")
}

/// The skills folders Roo reads at this scope, kitbag's own first, then the
/// cross-agent `.agents/skills/`.
fn skills_dirs(env: &Env) -> Vec<PathBuf> {
    let agents = match env.scope {
        Scope::User => env.home.join(".agents"),
        Scope::Project => env.project.join(".agents"),
    };
    vec![roo_dir(env).join("skills"), agents.join("skills")]
}

/// Folders next to `base/<prefix>` named `<prefix>-<mode>`.
fn mode_dirs(base: &std::path::Path, prefix: &str) -> Vec<PathBuf> {
    let lead = format!("{prefix}-");
    crate::plan_fs::subdirs(base)
        .unwrap_or_default()
        .into_iter()
        .filter(|n| n.starts_with(&lead))
        .map(|n| base.join(n))
        .collect()
}

/// A profile rule as a Roo rule file: plain markdown, with a first line saying
/// when the rule applies if it isn't always on.
fn roo_rule(r: &Rule) -> String {
    let body = r.body.trim();
    match rule_lead_line(r) {
        Some(lead) => format!("{lead}\n\n{body}\n"),
        None => format!("{body}\n"),
    }
}

/// A profile command as a Roo command file.
fn roo_command(c: &Command) -> String {
    let mode = c.extra.get("mode").and_then(Value::as_str);
    frontmatter::render(
        &[
            ("description", frontmatter::opt(&c.description)),
            ("argument-hint", frontmatter::opt(&c.argument_hint)),
            ("mode", mode.map_or(Value::Null, |m| json!(m))),
        ],
        &c.body,
    )
}

/// Roo's rule for a skill name.
fn valid_skill_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 64
        && name
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
        && !name.starts_with('-')
        && !name.ends_with('-')
        && !name.contains("--")
}

impl Adapter for Roo {
    fn id(&self) -> &'static str {
        ID
    }
    fn name(&self) -> &'static str {
        "Roo Code"
    }
    fn docs(&self) -> &'static str {
        "https://roocodeinc.github.io/Roo-Code/features/mcp/using-mcp-in-roo"
    }
    fn scopes(&self) -> &'static [Scope] {
        &[Scope::User, Scope::Project]
    }
    fn support(&self, _scope: Scope) -> [(Feature, Support); 8] {
        Feature::ALL.map(|f| {
            let s = match f {
                Feature::Mcp
                | Feature::Instructions
                | Feature::Rules
                | Feature::Commands
                | Feature::Skills => Support::Native,
                _ => Support::None,
            };
            (f, s)
        })
    }

    fn locations(&self, env: &Env) -> Vec<(&'static str, PathBuf)> {
        vec![
            ("mcp", mcp_file(env)),
            ("instructions", instructions_file(env)),
            ("rules", rules_dir(env)),
            ("commands", commands_dir(env)),
            ("skills", skills_dirs(env)[0].clone()),
        ]
    }

    fn detect(&self, env: &Env) -> bool {
        match env.scope {
            Scope::User => env.home.join(".roo").is_dir() || !hosts_with_roo(env).is_empty(),
            // AGENTS.md is shared with other tools and says nothing about Roo.
            Scope::Project => {
                env.project.join(".roo").is_dir() || env.project.join(".roorules").exists()
            }
        }
    }

    fn capture(&self, env: &Env) -> Result<Captured> {
        let mut cap = Captured::for_env(env);
        let file = mcp_file(env);

        if let Some(text) = cap.read_config(&file, ID)? {
            cap.sources.push(file.clone());
            let doc = parse_value(Some(&text), &file)?;
            for (name, def) in
                server_entries(&mut cap, ID, &file, "mcpServers", doc.get("mcpServers"))
            {
                let mut s = server_from_json(def);
                match def.get("type").and_then(Value::as_str) {
                    Some("streamable-http") => s.transport = Some(Transport::Http),
                    Some("sse") => s.transport = Some(Transport::Sse),
                    Some("stdio") => {}
                    None if s.url.is_some() => {
                        cap.warn(
                            ID,
                            format!(
                                "skipped MCP server '{name}': it has a url but no type, which Roo rejects (it needs streamable-http or sse)"
                            ),
                        );
                        continue;
                    }
                    None => {}
                    Some(other) => {
                        cap.warn(
                            ID,
                            format!(
                                "skipped MCP server '{name}': unknown type '{other}' (Roo's docs list stdio, streamable-http and sse)"
                            ),
                        );
                        continue;
                    }
                }
                s.disabled = def.get("disabled") == Some(&Value::Bool(true));
                import_server(&mut cap, ID, name, s, false);
            }
        }
        if env.scope == Scope::User {
            let hosts = hosts_with_roo(env);
            if hosts.len() > 1 {
                cap.info(
                    ID,
                    format!(
                        "Roo Code is installed in more than one editor ({}); only {}'s MCP settings were read",
                        hosts.join(", "),
                        hosts[0]
                    ),
                );
            }
        }

        let own = instructions_file(env);
        let base = roo_dir(env);

        // Rule files first: one of them beats a stale copy of the same rule
        // folded into the instructions file by an older kitbag.
        let rules = rules_dir(env);
        capture_md_files(
            &mut cap,
            &rules,
            ".md",
            false,
            Some(&own),
            ID,
            &mut |cap, name, text, path| {
                cap.sources.push(path.to_path_buf());
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
                add_rule(cap, rule_from_lead_line(name, text), ID);
            },
        )?;
        note_uncaptured(
            &mut cap,
            ID,
            &rules,
            &env.home,
            "kitbag reads only *.md rule files",
            &|rel| rel.ends_with(".md") || crate::plan_fs::join_rel(&rules, rel) == own,
        );
        for dir in mode_dirs(&base, "rules") {
            note_uncaptured(
                &mut cap,
                ID,
                &dir,
                &env.home,
                "mode-specific rules, which kitbag has no mapping for",
                &|_| false,
            );
        }
        if env.scope == Scope::Project {
            for name in std::fs::read_dir(&env.project)
                .into_iter()
                .flatten()
                .flatten()
                .map(|e| e.file_name().to_string_lossy().into_owned())
                .filter(|n| n.starts_with(".roorules-"))
            {
                cap.info(
                    ID,
                    format!("{name} was not captured: kitbag has no mapping for Roo's modes"),
                );
            }
            if env.project.join(".roorules").is_file() {
                cap.info(
                    ID,
                    "a .roorules file was not read: kitbag carries the files in .roo/rules/, and Roo reads .roorules only while that folder is empty",
                );
            }
        }

        let commands = commands_dir(env);
        capture_md_files(
            &mut cap,
            &commands,
            ".md",
            false,
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
            &commands,
            &env.home,
            "kitbag reads only *.md command files",
            &|rel| rel.ends_with(".md"),
        );

        let skills = skills_dirs(env);
        for dir in &skills {
            capture_skills_dir(&mut cap, dir, ID)?;
        }
        let agents = skills[1].parent().map(std::path::Path::to_path_buf);
        for base in [Some(base.clone()), agents].into_iter().flatten() {
            for dir in mode_dirs(&base, "skills") {
                note_uncaptured(
                    &mut cap,
                    ID,
                    &dir,
                    &env.home,
                    "mode-specific skills, which kitbag has no mapping for",
                    &|_| false,
                );
            }
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
        let file = mcp_file(env);
        let mut servers = Vec::new();
        for (name, s) in servers_for(profile, ID, work, true) {
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
                if let Some(cwd) = &s.cwd {
                    def.insert("cwd".into(), json!(cwd));
                }
            } else {
                def.insert("url".into(), json!(s.url));
                let kind = if s.transport() == Transport::Sse {
                    "sse"
                } else {
                    "streamable-http"
                };
                def.insert("type".into(), json!(kind));
                if !s.headers.is_empty() {
                    def.insert("headers".into(), json!(s.headers));
                }
                if s.cwd.is_some() {
                    work.warn(
                        ID,
                        format!("MCP server '{name}': 'cwd' only applies to local servers in Roo and was dropped"),
                    );
                }
            }
            if s.disabled {
                def.insert("disabled".into(), json!(true));
            }
            servers.push((name.clone(), Value::Object(def)));
        }
        merge_json_servers_off(
            work,
            (&file, &["mcpServers"]),
            &servers,
            MANAGED_KEYS,
            opts,
            ID,
            Some((&set_disabled_true, "disabled: true")),
        )?;
        if env.scope == Scope::User && !servers.is_empty() {
            let hosts = hosts_with_roo(env);
            if hosts.is_empty() {
                work.warn(
                    ID,
                    format!(
                        "Roo Code's storage was not found in VS Code, VS Code Insiders, VSCodium or Cursor; the servers go to {}. If Roo is in another editor, or you set roo-cline.customStoragePath, copy that file to where Roo reads it",
                        crate::ui::tildify(&file, &env.home)
                    ),
                );
            } else {
                work.info(
                    ID,
                    format!(
                        "Roo Code's global MCP settings are in {}'s extension storage; kitbag can't see a roo-cline.customStoragePath setting",
                        hosts[0]
                    ),
                );
            }
        }
        if env.scope == Scope::Project && !servers.is_empty() {
            work.info(
                ID,
                "kitbag writes no alwaysAllow, so Roo's own tool approval applies to these servers; a project's .roo/mcp.json wins over the user file for a server of the same name",
            );
        }

        // At project scope this is AGENTS.md, with the same block id as Codex,
        // Cursor, Antigravity, opencode, Windsurf and Grok Build. Rules are
        // files of their own, so at user scope (a file only kitbag writes) the
        // copy an older kitbag folded into the instructions is removed; in
        // AGENTS.md it is left to the other targets that fold.
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

        let rules = rules_dir(env);
        let mut hides_roorules = false;
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
            match r.mode() {
                RuleMode::Globs => work.info(
                    ID,
                    format!("rule '{}' is file-scoped in the profile; Roo Code's docs describe no file-scoped rules, so it always loads, with a line saying which files it is for", r.name),
                ),
                RuleMode::OnDemand => work.info(
                    ID,
                    format!("rule '{}' is on-demand in the profile; Roo Code has no on-demand rules, so it always loads", r.name),
                ),
                RuleMode::Always => {}
            }
            let path = rules.join(&file);
            // Roo reads `.roorules` only while `.roo/rules/` has no files.
            if !user
                && work
                    .fs
                    .read_bytes(&env.project.join(".roorules"))?
                    .is_some()
                && crate::plan_fs::walk_files(&rules, 1)?.is_empty()
            {
                hides_roorules = true;
            }
            write_rule(work, &path, roo_rule(r).as_bytes(), opts, ID, &r.name)?;
        }
        if hides_roorules {
            work.warn(
                ID,
                "this project has a .roorules file, which Roo Code reads only while .roo/rules/ is empty; the rule files kitbag writes there make Roo ignore it (move its content into a rule file, or apply without the rules)",
            );
        }
        if !user && !profile.rules.is_empty() && work.targets.iter().any(|t| t == "codex") {
            work.info(
                ID,
                "Codex folds the rules into the shared AGENTS.md, and Roo Code reads that file as well as .roo/rules/, so in this project Roo sees the rules twice",
            );
        }

        for c in &profile.commands {
            if let Some(m) = c.extra.get("mode").and_then(Value::as_str) {
                work.info(
                    ID,
                    format!("command '{}': its mode '{m}' is written as Roo Code's `mode`, which switches Roo to that mode before the command runs; check it names a Roo mode", c.name),
                );
            }
            let path = commands_dir(env).join(format!("{}.md", c.name));
            write_command(work, &path, roo_command(c).as_bytes(), opts, ID, &c.name)?;
        }

        let skills = skills_dirs(env);
        for s in &profile.skills {
            if !valid_skill_name(&s.name) {
                work.warn(
                    ID,
                    format!("skill '{}' skipped: Roo Code needs a name of 1-64 lowercase letters, digits and single hyphens, not starting or ending with one", s.name),
                );
                continue;
            }
            let s = skill_with_required_frontmatter(s);
            if let Some(w) = skill_name_mismatch(&s) {
                work.warn(ID, w);
            }
            // `.roo/skills` wins over `.agents/skills` for a name in both, so
            // another tool's copy there is not touched, but it is shadowed.
            if let Some(other) = skills.get(1)
                && work
                    .fs
                    .read_bytes(&other.join(&s.name).join("SKILL.md"))?
                    .is_some()
            {
                work.info(
                    ID,
                    format!(
                        "skill '{}' also exists in {}; Roo Code prefers the copy in .roo/skills/, which is the one kitbag writes",
                        s.name,
                        crate::ui::tildify(&other.join(&s.name), &env.home)
                    ),
                );
            }
            write_skill(work, &skills[0].join(&s.name), &s, opts, ID)?;
        }
        if profile.settings.contains_key(ID) {
            work.info(
                ID,
                "kitbag doesn't manage Roo Code settings; settings.roo was not applied",
            );
        }
        Ok(())
    }
}
