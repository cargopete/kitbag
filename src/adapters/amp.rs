//! Amp (Sourcegraph's coding agent): MCP servers, instructions and skills.
//!
//! * MCP: `amp.mcpServers` (a flat, dotted key) in Amp's settings file: user
//!   settings `~/.config/amp/settings.json` or `settings.jsonc` (the same path
//!   under `%USERPROFILE%` on Windows), and the workspace's `.amp/settings.json`
//!   or `settings.jsonc`. Local servers take `command`, `args` and `env`;
//!   remote servers take `url` and `headers`; Amp picks the transport from
//!   which of the two is there. Amp expands `${VAR}` in these values, so
//!   references are written as they are and no secret touches disk. A
//!   workspace server has to be approved by the user before it runs
//!   (`amp mcp approve <name>`); kitbag never approves one, and writes no
//!   `amp.mcpTrustedServers` or `amp.mcpPermissions`. When a server is defined
//!   in both, the workspace one wins over the user one (then skills' own).
//! * `${amp:id-token}` is an Amp-only reference that lets an orb send its Amp
//!   ID token to a server; a server that uses it is not captured and not written.
//! * Instructions: `~/.config/amp/AGENTS.md` (Amp also reads
//!   `~/.config/AGENTS.md`, which kitbag leaves alone). In a project, Amp reads
//!   `AGENTS.md`, or `AGENT.md` / `CLAUDE.md` when the directory has no
//!   `AGENTS.md`; kitbag writes to the one Amp reads. `AGENTS.md` is shared with
//!   Codex, Cursor, Antigravity, opencode, Grok Build, Windsurf and Hermes.
//! * Skills: `<name>/SKILL.md` (with `name` and `description` frontmatter) and
//!   its bundled files. Amp reads, first match by name winning:
//!   `~/.config/agents/skills`, `~/.agents/skills`, `~/.config/amp/skills`,
//!   the project's `.agents/skills` (and its parents'), the Claude-compatible
//!   `.claude/skills` and `~/.claude/skills`, then `amp.skills.path`. kitbag
//!   captures the first three and `~/.claude/skills` (user), the project's
//!   `.agents/skills` and `.claude/skills` (project), and writes only to
//!   `~/.config/agents/skills` (where `amp skill add --global` installs) and
//!   `.agents/skills`, never to Claude Code's folders.
//! * Commands: Amp removed custom commands in favour of skills, and its own
//!   migration turns `.agents/commands/<n>.md` into `.agents/skills/<n>/SKILL.md`
//!   and `~/.config/amp/commands` into `~/.config/agents/skills`. kitbag does the
//!   same when it applies a profile's commands, and never writes a commands folder.
//! * Not carried: MCP servers defined on ampcode.com (`amp mcp remote`), servers
//!   bundled in a skill (`mcp.json` or an `mcpServers` frontmatter key: Amp
//!   starts them when it finds the skill and its docs describe no approval for
//!   them), `includeTools`, rules (Amp has none; granular guidance is
//!   `@`-mentions in `AGENTS.md`), permissions, hooks and the other `amp.*`
//!   settings.
//!
//! Docs: <https://ampcode.com/docs/customize/mcp>,
//! <https://ampcode.com/docs/customize/skills>,
//! <https://ampcode.com/news/slashing-custom-commands>,
//! <https://ampcode.com/docs/cli/settings>,
//! <https://ampcode.com/docs/customize/agents-md>.

use std::path::{Path, PathBuf};

use anyhow::Result;
use serde_json::{Map, Value, json};

use super::common::{
    capture_instructions, capture_skills_dir, import_server, merge_json_servers, render_server,
    server_entries, server_from_json, servers_for, skill_with_required_frontmatter,
    write_instructions, write_skill,
};
use super::{Adapter, ApplyOptions, Captured, Env, Feature, Scope, Support, Work};
use crate::edit::json::parse_value;
use crate::frontmatter;
use crate::model::{McpServer, Profile, Skill, SkillFile, Transport};
use crate::secrets::RefStyle;

const ID: &str = "amp";
/// The settings key Amp keeps MCP servers under (one key, dots and all).
const SERVERS_KEY: &str = "amp.mcpServers";
/// Server keys kitbag writes; anything else (`includeTools` ...) is preserved.
const MANAGED_KEYS: &[&str] = &["command", "args", "env", "url", "headers"];
/// Server keys kitbag reads; any other key on an entry is not captured.
const CAPTURED_KEYS: &[&str] = &["command", "args", "env", "url", "headers"];
/// Where Amp's docs put the Amp ID token reference.
const ID_TOKEN_REF: &str = "${amp:";
/// Project instruction files in the order Amp falls back through them: it reads
/// the first one the directory has.
const PROJECT_FILES: &[&str] = &["AGENTS.md", "AGENT.md", "CLAUDE.md"];

/// A skill's own MCP servers file; kitbag never writes it.
const SKILL_MCP_FILE: &str = "mcp.json";
/// Frontmatter key that declares a skill's MCP servers inline.
const SKILL_MCP_KEY: &str = "mcpServers";

pub struct Amp;

/// Amp's user config directory: `~/.config/amp`, on every OS (Windows:
/// `%USERPROFILE%\.config\amp`). The docs name no environment variable for it.
pub fn config_dir(env: &Env) -> PathBuf {
    env.home.join(".config").join("amp")
}

/// The directory holding `settings.json[c]` in this scope.
fn settings_dir(env: &Env) -> PathBuf {
    match env.scope {
        Scope::User => config_dir(env),
        Scope::Project => env.project.join(".amp"),
    }
}

/// Both spellings of a settings file in `dir`, in the order Amp's docs give them.
fn settings_candidates(dir: &Path) -> [PathBuf; 2] {
    [dir.join("settings.json"), dir.join("settings.jsonc")]
}

/// The settings file of a directory: `settings.json`, else an existing
/// `settings.jsonc`, else `settings.json` to create.
fn settings_in(dir: &Path) -> PathBuf {
    let [json, jsonc] = settings_candidates(dir);
    if !json.is_file() && jsonc.is_file() {
        jsonc
    } else {
        json
    }
}

fn settings_file(env: &Env) -> PathBuf {
    settings_in(&settings_dir(env))
}

/// Say so when both spellings exist: the docs don't say which one wins.
fn note_both_spellings(dir: &Path, report: &mut dyn FnMut(String)) {
    let [json, jsonc] = settings_candidates(dir);
    if json.is_file() && jsonc.is_file() {
        report(format!(
            "both {} and {} exist and Amp's docs don't say which one wins; kitbag uses settings.json",
            json.display(),
            jsonc.display()
        ));
    }
}

/// The skills folder kitbag writes: `~/.config/agents/skills` (where
/// `amp skill add --global` installs) or the project's `.agents/skills`.
fn skills_dir(env: &Env) -> PathBuf {
    match env.scope {
        Scope::User => env.home.join(".config").join("agents").join("skills"),
        Scope::Project => env.project.join(".agents").join("skills"),
    }
}

/// The local skills folders of a scope that Amp reads and kitbag captures, in
/// Amp's order (first match by name wins), with the settings flag that
/// switches each group off.
fn skill_roots(env: &Env, no_global_agents: bool, no_claude: bool) -> Vec<PathBuf> {
    let mut roots = Vec::new();
    match env.scope {
        Scope::User => {
            if !no_global_agents {
                roots.push(env.home.join(".config").join("agents").join("skills"));
                roots.push(env.home.join(".agents").join("skills"));
            }
            roots.push(config_dir(env).join("skills"));
            if !no_claude {
                roots.push(env.home.join(".claude").join("skills"));
            }
        }
        Scope::Project => {
            roots.push(env.project.join(".agents").join("skills"));
            if !no_claude {
                roots.push(env.project.join(".claude").join("skills"));
            }
        }
    }
    roots
}

/// Names of the skill folders in `dir` that hold a `SKILL.md`.
fn skill_names(dir: &Path) -> Vec<String> {
    let Ok(rd) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut names: Vec<String> = rd
        .flatten()
        .filter(|e| e.path().join("SKILL.md").is_file())
        .filter_map(|e| e.file_name().into_string().ok())
        .collect();
    names.sort();
    names
}

/// A command as the skill Amp's migration turns it into: a folder named for
/// the command with a `SKILL.md` that holds its prompt.
fn command_as_skill(c: &crate::model::Command) -> Skill {
    let description = c
        .description
        .clone()
        .unwrap_or_else(|| format!("Instructions from the {} command", c.name));
    let text = frontmatter::render(
        &[
            ("name", Value::String(c.name.clone())),
            ("description", Value::String(description.clone())),
        ],
        &c.body,
    );
    Skill {
        name: c.name.clone(),
        description: Some(description),
        files: vec![SkillFile {
            path: "SKILL.md".into(),
            content: text.into_bytes(),
            executable: false,
        }],
        ..Default::default()
    }
}

/// `skill` without the MCP servers it declares itself: `mcp.json` and the
/// `mcpServers` frontmatter key. Says which it dropped.
fn without_skill_mcp(skill: &Skill) -> (Skill, Vec<&'static str>) {
    let mut s = skill.clone();
    let mut dropped = Vec::new();
    s.files.retain(|f| {
        let keep = f.path != SKILL_MCP_FILE;
        if !keep {
            dropped.push(SKILL_MCP_FILE);
        }
        keep
    });
    for f in &mut s.files {
        if f.path != "SKILL.md" {
            continue;
        }
        let text = String::from_utf8_lossy(&f.content).into_owned();
        if let Some(next) = frontmatter::remove_keys(&text, &[SKILL_MCP_KEY]) {
            f.content = next.into_bytes();
            dropped.push(SKILL_MCP_KEY);
        }
    }
    (s, dropped)
}

fn project_file(env: &Env, name: &str) -> PathBuf {
    env.project.join(name)
}

/// The instructions file Amp reads in this scope; in a project, the first of
/// `AGENTS.md`, `AGENT.md`, `CLAUDE.md` for which `exists` is true, else `AGENTS.md`.
fn instructions_file(env: &Env, exists: impl Fn(&Path) -> bool) -> PathBuf {
    match env.scope {
        Scope::User => config_dir(env).join("AGENTS.md"),
        Scope::Project => {
            let name = PROJECT_FILES
                .iter()
                .find(|n| exists(&project_file(env, n)))
                .unwrap_or(&PROJECT_FILES[0]);
            project_file(env, name)
        }
    }
}

/// Does the server use Amp's ID-token reference anywhere?
fn uses_id_token(s: &McpServer) -> bool {
    let mut hit = false;
    let _ = s.map_strings(|v| {
        hit |= v.contains(ID_TOKEN_REF);
        v.to_string()
    });
    hit
}

/// Names of the servers under `amp.mcpServers` in the settings file of `dir`,
/// read leniently: a file that isn't there or doesn't parse has none.
fn server_names(dir: &Path) -> Vec<String> {
    let file = settings_in(dir);
    let Ok(Some(text)) = crate::plan_fs::read_text_opt(&file) else {
        return Vec::new();
    };
    match parse_value(Some(&text), &file) {
        Ok(doc) => doc
            .get(SERVERS_KEY)
            .and_then(Value::as_object)
            .map(|m| m.keys().cloned().collect())
            .unwrap_or_default(),
        Err(_) => Vec::new(),
    }
}

fn flag(settings: &Value, key: &str) -> bool {
    settings.get(key).and_then(Value::as_bool).unwrap_or(false)
}

/// Capture skills from the local folders Amp reads, first in Amp's order
/// winning a name; a skill of the same name further down is named and left.
fn capture_amp_skills(cap: &mut Captured, env: &Env, settings: &Value) {
    let roots = skill_roots(
        env,
        flag(settings, "amp.skills.disableGlobalAgentsSkills"),
        flag(settings, "amp.skills.disableClaudeCodeSkills"),
    );
    // Which folder each captured skill came from.
    let mut from: Vec<(String, PathBuf)> = Vec::new();
    for root in &roots {
        for name in skill_names(root) {
            if let Some((_, first)) = from.iter().find(|(n, _)| *n == name) {
                cap.info(
                    ID,
                    format!(
                        "skill '{name}' in {} skipped: Amp uses the one in {} (first in its order)",
                        crate::ui::tildify(root, &env.home),
                        crate::ui::tildify(first, &env.home)
                    ),
                );
            }
        }
        let before = cap.profile.skills.len();
        if let Err(e) = capture_skills_dir(cap, root, ID) {
            cap.warn(ID, format!("skills in {}: {e:#}", root.display()));
        }
        for i in before..cap.profile.skills.len() {
            let name = cap.profile.skills[i].name.clone();
            from.push((name.clone(), root.clone()));
            // A skill's own MCP servers stay behind (see the module docs).
            let (clean, dropped) = without_skill_mcp(&cap.profile.skills[i]);
            if dropped.contains(&SKILL_MCP_FILE) {
                cap.profile.skills[i] = clean;
                cap.warn(
                    ID,
                    format!(
                        "skill '{name}': {SKILL_MCP_FILE} not captured; its MCP servers are not carried (add them to the profile's mcp_servers instead)"
                    ),
                );
            }
            if dropped.contains(&SKILL_MCP_KEY) {
                cap.info(
                    ID,
                    format!(
                        "skill '{name}' declares MCP servers in its frontmatter ({SKILL_MCP_KEY}); the line stays in the captured SKILL.md but kitbag leaves it out when it applies the skill to Amp"
                    ),
                );
            }
        }
    }
    if settings.get("amp.skills.path").is_some() {
        cap.info(
            ID,
            "amp.skills.path folders are not captured; kitbag reads Amp's standard skills folders",
        );
    }
    if env.scope == Scope::User {
        let cache = env.home.join(".claude").join("plugins").join("cache");
        if cache.is_dir() {
            cap.info(
                ID,
                "~/.claude/plugins/cache is read by Amp but holds Claude Code plugins, so kitbag doesn't capture it",
            );
        }
    }
}

/// Write the profile's skills, and its commands as skills (Amp's own
/// migration for the custom commands it removed).
fn apply_skills(profile: &Profile, env: &Env, opts: &ApplyOptions, work: &mut Work) -> Result<()> {
    let dir = skills_dir(env);
    let mut skills: Vec<Skill> = profile.skills.clone();
    for c in &profile.commands {
        if skills.iter().any(|s| s.name == c.name) {
            work.warn(
                ID,
                format!(
                    "command '{}' skipped: the profile already has a skill of that name, and Amp has no commands to keep them apart",
                    c.name
                ),
            );
            continue;
        }
        let mut lost = Vec::new();
        if c.argument_hint.is_some() {
            lost.push("argument-hint");
        }
        if !c.extra.is_empty() {
            lost.push("its other frontmatter");
        }
        if c.body.contains("$ARGUMENTS") || (1..=9).any(|n| c.body.contains(&format!("${n}"))) {
            lost.push("$ARGUMENTS and $1..$9 substitution");
        }
        if !lost.is_empty() {
            work.info(
                ID,
                format!(
                    "command '{}' becomes a skill and loses {} (skills take no arguments template)",
                    c.name,
                    lost.join(", ")
                ),
            );
        }
        skills.push(command_as_skill(c));
    }
    if !profile.commands.is_empty() {
        work.info(
            ID,
            format!(
                "Amp removed custom commands in favour of skills, so commands are written as skills in {} (as Amp's own migration does); no commands folder is written",
                crate::ui::tildify(&dir, &work.home)
            ),
        );
    }

    let user_lower: Vec<PathBuf> = vec![
        env.home.join(".agents").join("skills"),
        config_dir(env).join("skills"),
        env.home.join(".claude").join("skills"),
    ];
    let user_first = env.home.join(".config").join("agents").join("skills");
    for s in &skills {
        let (s, dropped) = without_skill_mcp(s);
        if dropped.contains(&SKILL_MCP_FILE) || dropped.contains(&SKILL_MCP_KEY) {
            work.warn(
                ID,
                format!(
                    "skill '{}': {} not written; Amp starts a skill's MCP servers when it finds the skill and its docs describe no approval step for them (add the servers to the profile's mcp_servers to have kitbag write them to settings)",
                    s.name,
                    dropped.join(" and ")
                ),
            );
        }
        let s = skill_with_required_frontmatter(&s);
        let here = dir.join(&s.name).join("SKILL.md");
        match env.scope {
            Scope::User => {
                let lower: Vec<String> = user_lower
                    .iter()
                    .filter(|d| d.join(&s.name).join("SKILL.md").is_file())
                    .map(|d| crate::ui::tildify(d, &work.home))
                    .collect();
                if !lower.is_empty() {
                    work.info(
                        ID,
                        format!(
                            "skill '{}' also exists in {}; Amp uses the one in ~/.config/agents/skills, which kitbag writes",
                            s.name,
                            lower.join(", ")
                        ),
                    );
                }
            }
            Scope::Project => {
                let user = [user_first.clone()]
                    .into_iter()
                    .chain(user_lower.iter().take(2).cloned())
                    .find(|d| d.join(&s.name).join("SKILL.md").is_file());
                if let Some(d) = user {
                    work.info(
                        ID,
                        format!(
                            "skill '{}' also exists in {}; Amp lists user skills before this project's ({}), so that one is used",
                            s.name,
                            crate::ui::tildify(&d, &work.home),
                            here.strip_prefix(&env.project).unwrap_or(&here).display()
                        ),
                    );
                }
            }
        }
        write_skill(work, &dir.join(&s.name), &s, opts, ID)?;
    }
    Ok(())
}

impl Adapter for Amp {
    fn id(&self) -> &'static str {
        ID
    }
    fn name(&self) -> &'static str {
        "Amp"
    }
    fn docs(&self) -> &'static str {
        "https://ampcode.com/docs/customize/mcp"
    }
    fn scopes(&self) -> &'static [Scope] {
        &[Scope::User, Scope::Project]
    }
    fn support(&self, _scope: Scope) -> [(Feature, Support); 7] {
        Feature::ALL.map(|f| {
            let s = match f {
                Feature::Mcp | Feature::Instructions | Feature::Skills => Support::Native,
                // Amp removed custom commands; they are written as skills.
                Feature::Commands => Support::Folded,
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
        // AGENTS.md is shared with other tools and says nothing about Amp.
        settings_dir(env).is_dir()
    }

    fn capture(&self, env: &Env) -> Result<Captured> {
        let mut cap = Captured::for_env(env);
        let dir = settings_dir(env);
        let file = settings_file(env);
        let mut both = Vec::new();
        note_both_spellings(&dir, &mut |m| both.push(m));
        for m in both {
            cap.warn(ID, m);
        }

        let mut settings = Value::Null;
        if let Some(text) = cap.read_config(&file, ID)? {
            cap.sources.push(file.clone());
            let doc = parse_value(Some(&text), &file)?;
            settings = doc.clone();
            for (name, def) in
                server_entries(&mut cap, ID, &file, SERVERS_KEY, doc.get(SERVERS_KEY))
            {
                let s = server_from_json(def);
                if uses_id_token(&s) {
                    cap.warn(
                        ID,
                        format!(
                            "skipped MCP server '{name}': it uses ${{amp:id-token}}, which only works in an Amp orb and is never carried"
                        ),
                    );
                    continue;
                }
                let other: Vec<&str> = def
                    .keys()
                    .map(String::as_str)
                    .filter(|k| !CAPTURED_KEYS.contains(k))
                    .collect();
                if !other.is_empty() {
                    cap.info(
                        ID,
                        format!(
                            "MCP server '{name}': {} not captured (kitbag carries command, args, env, url and headers)",
                            other.join(", ")
                        ),
                    );
                }
                import_server(&mut cap, ID, name, s, false);
            }
        }
        if env.scope == Scope::User || file.is_file() {
            cap.info(
                ID,
                "MCP servers defined on ampcode.com (amp mcp remote) or bundled in a skill's mcp.json are not captured",
            );
        }

        capture_amp_skills(&mut cap, env, &settings);

        let instructions = instructions_file(env, |p| p.is_file());
        capture_instructions(&mut cap, &instructions, ID)?;
        // Rules folded into a shared file by another target (Codex's AGENTS.md)
        // come back out of capture_instructions.
        if !cap.profile.rules.is_empty() {
            let names: Vec<String> = cap.profile.rules.drain(..).map(|r| r.name).collect();
            cap.info(
                ID,
                format!(
                    "rules folded into Amp's instructions file by another tool were not captured ({}); capture that tool for them",
                    names.join(", ")
                ),
            );
        }
        match env.scope {
            Scope::User => {
                let shared = env.home.join(".config").join("AGENTS.md");
                if shared.is_file() {
                    cap.info(
                        ID,
                        format!(
                            "{} is also read by Amp and was not captured; kitbag reads only {}",
                            shared.display(),
                            instructions.display()
                        ),
                    );
                }
            }
            Scope::Project => {
                // Amp reads one file per directory; say which of the others it ignores.
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
                            "{} not read: Amp uses AGENTS.md, else AGENT.md, else CLAUDE.md, and found {}",
                            ignored.join(", "),
                            instructions
                                .strip_prefix(&env.project)
                                .unwrap_or(&instructions)
                                .display()
                        ),
                    );
                }
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
        let dir = settings_dir(env);
        let file = settings_file(env);
        let mut both = Vec::new();
        note_both_spellings(&dir, &mut |m| both.push(m));
        for m in both {
            work.warn(ID, m);
        }

        let mut servers = Vec::new();
        // Amp documents no way to switch a server off, so a disabled one isn't written.
        for (name, s) in servers_for(profile, ID, work, false) {
            if uses_id_token(s) {
                work.warn(
                    ID,
                    format!(
                        "MCP server '{name}' skipped: it uses ${{amp:id-token}}, which kitbag doesn't write"
                    ),
                );
                continue;
            }
            if s.cwd.is_some() {
                work.warn(
                    ID,
                    format!(
                        "MCP server '{name}': 'cwd' is not supported by Amp (servers start in the thread's working directory) and was dropped"
                    ),
                );
            }
            if s.transport() == Transport::Sse {
                work.warn(
                    ID,
                    format!(
                        "MCP server '{name}': Amp documents no transport setting, so it is written as a plain url (its own example is an /sse URL) and Amp decides how to connect"
                    ),
                );
            }
            // Amp expands ${VAR} itself; `${VAR:-default}` isn't documented, so
            // defaults are resolved now.
            let s = render_server(s, RefStyle::DollarPlain, env, work);
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
                def.insert("url".into(), json!(s.url));
                if !s.headers.is_empty() {
                    def.insert("headers".into(), json!(s.headers));
                }
            }
            servers.push((name.clone(), Value::Object(def)));
        }
        merge_json_servers(
            work,
            &file,
            &[SERVERS_KEY],
            &servers,
            MANAGED_KEYS,
            opts,
            ID,
        )?;

        if !servers.is_empty() {
            let names: Vec<&str> = servers.iter().map(|(n, _)| n.as_str()).collect();
            match env.scope {
                Scope::Project => {
                    work.info(
                        ID,
                        "Amp asks you to approve the MCP servers of a workspace's .amp/settings.json before it runs them (`amp mcp approve <name>`; `amp mcp doctor` lists those awaiting approval); kitbag leaves that to you and writes no approval or trust setting",
                    );
                    let user = server_names(&config_dir(env));
                    let both: Vec<&str> = names
                        .iter()
                        .copied()
                        .filter(|n| user.iter().any(|u| u == n))
                        .collect();
                    if !both.is_empty() {
                        work.info(
                            ID,
                            format!(
                                "in this workspace, the entry for {} overrides the server of the same name in your user settings (Amp: workspace over user)",
                                both.join(", ")
                            ),
                        );
                    }
                }
                Scope::User => {
                    let ws = server_names(&env.project.join(".amp"));
                    let both: Vec<&str> = names
                        .iter()
                        .copied()
                        .filter(|n| ws.iter().any(|w| w == n))
                        .collect();
                    if !both.is_empty() {
                        work.info(
                            ID,
                            format!(
                                "{} is also defined in {}, which overrides these user settings in that workspace (Amp: workspace over user)",
                                both.join(", "),
                                settings_in(&env.project.join(".amp")).display()
                            ),
                        );
                    }
                }
            }
        }

        // Amp reads one project file, so write where it will look: the first of
        // AGENTS.md, AGENT.md, CLAUDE.md that exists, counting what this plan
        // creates (Claude Code writes CLAUDE.md, Codex AGENTS.md).
        let exists = |p: &Path| p.is_file() || work.fs.read(p).ok().flatten().is_some();
        let path = instructions_file(env, exists);
        if env.scope == Scope::Project
            && path != project_file(env, PROJECT_FILES[0])
            && profile.instructions.is_some()
        {
            work.info(
                ID,
                format!(
                    "Amp reads only {} here (it uses AGENT.md or CLAUDE.md when there is no AGENTS.md), so the instructions go there; creating an AGENTS.md would hide it from Amp",
                    path.strip_prefix(&env.project).unwrap_or(&path).display()
                ),
            );
        }
        // At project scope this is AGENTS.md or one of its fallbacks, with the
        // same block id as the other tools sharing the file: applying several
        // plans the same block more than once, which leaves one copy.
        write_instructions(work, &path, profile, opts, false, ID)?;
        if env.scope == Scope::User
            && profile.instructions.is_some()
            && env.home.join(".config").join("AGENTS.md").is_file()
        {
            work.info(
                ID,
                "Amp also reads ~/.config/AGENTS.md, which kitbag leaves alone; the same text there would reach Amp twice",
            );
        }
        apply_skills(profile, env, opts, work)?;
        if profile.settings.contains_key(ID) {
            work.info(
                ID,
                "kitbag doesn't manage Amp settings; settings.amp was not applied",
            );
        }
        Ok(())
    }
}
