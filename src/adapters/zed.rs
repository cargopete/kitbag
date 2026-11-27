//! Zed: MCP servers (Zed calls them context servers), instructions and skills.
//!
//! * MCP: `context_servers` in `settings.json` (JSONC) of Zed's config
//!   directory (`~/.config/zed` on macOS, `$XDG_CONFIG_HOME/zed` or
//!   `~/.config/zed` on Linux, `%APPDATA%\Zed` on Windows), and in
//!   `.zed/settings.json` in a project. Local servers take `command` (a string),
//!   `args` and `env`; remote servers take `url` and `headers`; either may carry
//!   `enabled: false`. Zed documents no env-reference syntax, so references are
//!   resolved at apply time (or kept as placeholders with `--secrets never`),
//!   as for Claude Desktop. Project servers only start in a trusted worktree;
//!   that is Zed's own check and is not touched.
//! * Instructions: Zed's personal `AGENTS.md` in the config directory, and in a
//!   project the first existing file of `.rules`, `.cursorrules`,
//!   `.windsurfrules`, `.clinerules`, `.github/copilot-instructions.md`,
//!   `AGENT.md`, `AGENTS.md` (Zed reads only that one), else `AGENTS.md`,
//!   which is shared with Codex, Cursor, Antigravity, opencode, Grok Build and
//!   Windsurf.
//! * Skills: `<name>/SKILL.md` (`name` and `description` frontmatter, optional
//!   `disable-model-invocation`) in `~/.agents/skills/` and a project's
//!   `.agents/skills/`, the only folders Zed's docs name; Zed reads no other
//!   tool's skill folder and loads project skills only from a trusted worktree.
//!   The folder is also read by other tools (Amp writes its project skills to
//!   the same one), but Zed names no folder of its own, so kitbag writes there.
//! * Rules and commands: Zed's docs describe no rules file or commands folder
//!   (the Rules Library is a database inside Zed that is being replaced by
//!   skills and the personal `AGENTS.md`, and kitbag never reads or writes it),
//!   so neither is carried. Agents (#14), hooks (#13) and settings are not
//!   carried either.
//!
//! Docs: <https://zed.dev/docs/ai/mcp>, <https://zed.dev/docs/ai/instructions>,
//! <https://zed.dev/docs/ai/skills>, <https://zed.dev/docs/worktree-trust>. The user and project `context_servers`
//! keys, `enabled`, and the config directory are from Zed's source
//! (`crates/settings_content/src/project.rs`, `crates/paths/src/paths.rs`).

use std::path::PathBuf;

use anyhow::Result;
use serde_json::{Map, Value, json};

use super::common::{
    Resolver, capture_instructions, capture_skills_dir, import_server, merge_json_servers_off,
    server_entries, server_from_json, servers_for, set_enabled_false, skill_name_mismatch,
    skill_with_required_frontmatter, write_instructions, write_skill,
};
use super::{Adapter, ApplyOptions, Captured, Env, Feature, Os, Scope, Support, Work};
use crate::edit::json::parse_value;
use crate::frontmatter;
use crate::model::{Profile, Transport};

const ID: &str = "zed";
/// Server keys kitbag writes; anything else (`timeout`, `oauth`, `source` ...)
/// is preserved.
const MANAGED_KEYS: &[&str] = &["command", "args", "env", "url", "headers", "enabled"];
/// Project instruction files in the order Zed looks for them; it uses the
/// first one that exists. The last one is where kitbag writes by default.
const PROJECT_FILES: &[&str] = &[
    ".rules",
    ".cursorrules",
    ".windsurfrules",
    ".clinerules",
    ".github/copilot-instructions.md",
    "AGENT.md",
    "AGENTS.md",
];
const DEFAULT_PROJECT_FILE: &str = "AGENTS.md";
/// Longest skill description Zed loads without a warning.
const MAX_DESCRIPTION: usize = 1024;

pub struct Zed;

/// Zed's config directory.
pub fn config_dir(env: &Env) -> PathBuf {
    match env.os {
        Os::Windows => env
            .location_var("APPDATA")
            .unwrap_or_else(|| env.home.join("AppData").join("Roaming"))
            .join("Zed"),
        Os::Linux => env
            .location_var("XDG_CONFIG_HOME")
            .unwrap_or_else(|| env.home.join(".config"))
            .join("zed"),
        Os::MacOs => env.home.join(".config").join("zed"),
    }
}

fn settings_file(env: &Env) -> PathBuf {
    match env.scope {
        Scope::User => config_dir(env).join("settings.json"),
        Scope::Project => env.project.join(".zed").join("settings.json"),
    }
}

fn project_file(env: &Env, name: &str) -> PathBuf {
    name.split('/')
        .fold(env.project.clone(), |p, part| p.join(part))
}

/// Zed's skills folder: `~/.agents/skills` or the project's `.agents/skills`.
fn skills_dir(env: &Env) -> PathBuf {
    let base = match env.scope {
        Scope::User => &env.home,
        Scope::Project => &env.project,
    };
    base.join(".agents").join("skills")
}

/// Zed loads a skill only when its name is 1 to 64 lowercase letters, digits
/// and hyphens, with no leading, trailing or doubled hyphen.
fn zed_skill_name_ok(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 64
        && name
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
        && !name.starts_with('-')
        && !name.ends_with('-')
        && !name.contains("--")
}

/// The instructions file Zed reads in this scope; in a project, the first
/// candidate for which `exists` is true, else `AGENTS.md`.
fn instructions_file(env: &Env, exists: impl Fn(&std::path::Path) -> bool) -> PathBuf {
    match env.scope {
        Scope::User => config_dir(env).join("AGENTS.md"),
        Scope::Project => {
            let name = PROJECT_FILES
                .iter()
                .find(|n| exists(&project_file(env, n)))
                .unwrap_or(&DEFAULT_PROJECT_FILE);
            project_file(env, name)
        }
    }
}

impl Adapter for Zed {
    fn id(&self) -> &'static str {
        ID
    }
    fn name(&self) -> &'static str {
        "Zed"
    }
    fn docs(&self) -> &'static str {
        "https://zed.dev/docs/ai/mcp"
    }
    fn scopes(&self) -> &'static [Scope] {
        &[Scope::User, Scope::Project]
    }
    fn support(&self, _scope: Scope) -> [(Feature, Support); 7] {
        Feature::ALL.map(|f| {
            let s = match f {
                Feature::Mcp | Feature::Instructions | Feature::Skills => Support::Native,
                _ => Support::None,
            };
            (f, s)
        })
    }

    fn locations(&self, env: &Env) -> Vec<(&'static str, PathBuf)> {
        vec![
            ("mcp", settings_file(env)),
            ("instructions", instructions_file(env, |p| p.is_file())),
            ("skills", skills_dir(env)),
        ]
    }

    fn detect(&self, env: &Env) -> bool {
        match env.scope {
            Scope::User => config_dir(env).is_dir(),
            // AGENTS.md is shared with other tools and says nothing about Zed.
            Scope::Project => {
                env.project.join(".zed").is_dir() || env.project.join(".rules").is_file()
            }
        }
    }

    fn capture(&self, env: &Env) -> Result<Captured> {
        let mut cap = Captured::for_env(env);
        let file = settings_file(env);

        if let Some(text) = cap.read_config(&file, ID)? {
            cap.sources.push(file.clone());
            let doc = parse_value(Some(&text), &file)?;
            for (name, def) in server_entries(
                &mut cap,
                ID,
                &file,
                "context_servers",
                doc.get("context_servers"),
            ) {
                if def.get("command").is_some_and(Value::is_object) {
                    cap.warn(
                        ID,
                        format!(
                            "skipped MCP server '{name}': its 'command' is an object (an older Zed layout); kitbag reads the current one, where 'command' is the program and 'args' and 'env' sit beside it"
                        ),
                    );
                    continue;
                }
                if !def.contains_key("command") && !def.contains_key("url") {
                    cap.info(
                        ID,
                        format!(
                            "MCP server '{name}' has no command or url (a server provided by a Zed extension); not captured"
                        ),
                    );
                    continue;
                }
                let mut s = server_from_json(def);
                s.disabled = def.get("enabled") == Some(&Value::Bool(false));
                import_server(&mut cap, ID, name, s, false);
            }
        }

        let instructions = instructions_file(env, |p| p.is_file());
        capture_instructions(&mut cap, &instructions, ID)?;
        // Rules folded into a shared file by another target (Codex's AGENTS.md)
        // come back out of capture_instructions.
        if !cap.profile.rules.is_empty() {
            let names: Vec<String> = cap.profile.rules.drain(..).map(|r| r.name).collect();
            cap.info(
                ID,
                format!(
                    "rules folded into Zed's instructions file by another tool were not captured ({}); capture that tool for them",
                    names.join(", ")
                ),
            );
        }
        if env.scope == Scope::Project {
            // Zed loads one project file; say which of the others it ignores.
            let ignored: Vec<&str> = PROJECT_FILES
                .iter()
                .copied()
                .filter(|n| {
                    let p = project_file(env, n);
                    p != instructions && p.is_file()
                })
                .collect();
            if !ignored.is_empty() {
                cap.info(
                    ID,
                    format!(
                        "{} not read: Zed uses only the first project instructions file it finds ({})",
                        ignored.join(", "),
                        instructions
                            .strip_prefix(&env.project)
                            .unwrap_or(&instructions)
                            .display()
                    ),
                );
            }
        }
        capture_skills_dir(&mut cap, &skills_dir(env), ID)?;
        if env.scope == Scope::User {
            cap.info(
                ID,
                "Zed's Rules Library lives in Zed's own database, which kitbag never reads, so those rules were not captured; Zed is replacing it with skills (~/.agents/skills) and the personal AGENTS.md, which are captured",
            );
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
        let file = settings_file(env);
        let mut servers = Vec::new();
        let mut sse = Vec::new();
        for (name, s) in servers_for(profile, ID, work, true) {
            if s.transport() == Transport::Sse {
                sse.push(name.as_str());
                continue;
            }
            if s.cwd.is_some() {
                work.warn(
                    ID,
                    format!("MCP server '{name}': 'cwd' is not supported by Zed and was dropped"),
                );
            }
            let before = work.render.resolved.len();
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
                def.insert("enabled".into(), json!(false));
            }
            res.finish(work, ID, "Zed", name, "settings.json");
            if env.scope == Scope::Project && work.render.resolved.len() > before {
                work.warn(
                    ID,
                    format!(
                        "MCP server '{name}': a secret was written to .zed/settings.json in the project, which is usually committed; keep it out of version control, or use --secrets never"
                    ),
                );
            }
            servers.push((name.clone(), Value::Object(def)));
        }
        if !sse.is_empty() {
            work.warn(
                ID,
                format!(
                    "MCP server(s) {} skipped: Zed's docs describe command and URL (HTTP) servers only, not SSE",
                    sse.join(", ")
                ),
            );
        }
        merge_json_servers_off(
            work,
            (&file, &["context_servers"]),
            &servers,
            MANAGED_KEYS,
            opts,
            ID,
            Some((&set_enabled_false, "enabled: false")),
        )?;
        if env.scope == Scope::Project && !servers.is_empty() {
            work.info(
                ID,
                "Zed starts the MCP servers of a project's .zed/settings.json only in a trusted worktree; kitbag leaves that choice to Zed",
            );
        }

        // Zed reads one project file, so write where it will look: the first
        // of its files that exists, counting what this plan creates (Copilot
        // writes .github/copilot-instructions.md when it is a target).
        let copilot = work.targets.iter().any(|t| t == "copilot");
        let exists = |p: &std::path::Path| {
            p.is_file()
                || work.fs.read(p).ok().flatten().is_some()
                || (copilot && p == project_file(env, ".github/copilot-instructions.md"))
        };
        let path = instructions_file(env, exists);
        if env.scope == Scope::Project
            && path != project_file(env, DEFAULT_PROJECT_FILE)
            && profile.instructions.is_some()
        {
            work.info(
                ID,
                format!(
                    "Zed uses only the first project instructions file it finds, so the instructions go to {} rather than AGENTS.md",
                    path.strip_prefix(&env.project).unwrap_or(&path).display()
                ),
            );
        }
        write_instructions(work, &path, profile, opts, false, ID)?;
        apply_skills(profile, env, opts, work)?;
        if profile.settings.contains_key(ID) {
            work.info(
                ID,
                "kitbag doesn't manage Zed settings; settings.zed was not applied",
            );
        }
        Ok(())
    }
}

/// Write the profile's skills to Zed's skills folder.
fn apply_skills(profile: &Profile, env: &Env, opts: &ApplyOptions, work: &mut Work) -> Result<()> {
    let dir = skills_dir(env);
    let amp = env.scope == Scope::Project && work.targets.iter().any(|t| t == "amp");
    let mut wrote_any = false;
    for s in &profile.skills {
        if !zed_skill_name_ok(&s.name) {
            work.warn(
                ID,
                format!(
                    "skill '{}' skipped: Zed loads only names of lowercase letters, digits and single hyphens (1 to 64, none at either end); rename the skill to use it in Zed",
                    s.name
                ),
            );
            continue;
        }
        let s = skill_with_required_frontmatter(s);
        if let Some(w) = skill_name_mismatch(&s) {
            work.warn(ID, w);
        }
        if let Some(main) = s.main_file() {
            let text = String::from_utf8_lossy(&main.content).into_owned();
            let long = frontmatter::string(frontmatter::parse(&text).data.get("description"))
                .is_some_and(|d| d.chars().count() > MAX_DESCRIPTION);
            if long {
                work.warn(
                    ID,
                    format!(
                        "skill '{}': its description is over {MAX_DESCRIPTION} characters; Zed still loads it but shows a warning",
                        s.name
                    ),
                );
            }
        }
        if amp && !super::amp::without_skill_mcp(&s).1.is_empty() {
            work.warn(
                ID,
                format!(
                    "skill '{}' declares MCP servers of its own, which kitbag leaves out for Amp; Amp and Zed share .agents/skills in a project, so the two copies differ and each apply rewrites it; apply them to one of the two",
                    s.name
                ),
            );
        }
        write_skill(work, &dir.join(&s.name), &s, opts, ID)?;
        wrote_any = true;
    }
    if wrote_any && env.scope == Scope::Project {
        work.info(
            ID,
            "Zed loads the skills of a project's .agents/skills only in a trusted worktree; kitbag leaves that choice to Zed",
        );
    }
    Ok(())
}
