//! Hermes Agent (Nous Research): MCP servers and instructions.
//!
//! * MCP: `mcp_servers` in `$HERMES_HOME/config.yaml` (default `~/.hermes`),
//!   user scope only: the docs describe no project-level MCP file. A local
//!   server takes `command`, `args`, `env` and `cwd`; a remote one `url` and
//!   `headers`, with `transport: sse` for the legacy SSE transport. `enabled:
//!   false` switches a server off. Hermes expands `${VAR}` (and `${env:VAR}`)
//!   in config.yaml from its environment and `~/.hermes/.env`, so references
//!   are written as `${VAR}` and no secret reaches the file. Keys kitbag
//!   doesn't manage (`timeout`, `tools`, `auth`, `sampling` ...) are kept.
//! * Instructions: `$HERMES_HOME/SOUL.md` at user scope, the only file Hermes
//!   reads from its home that you write (its identity file; there is no global
//!   AGENTS.md). Apply writes a managed block next to what is in it; capture
//!   reads only that block, never the persona text around it, which is not
//!   instructions for other tools. In a project Hermes loads one context file, the first of
//!   `.hermes.md` / `HERMES.md`, `AGENTS.override.md`, `AGENTS.md`, `CLAUDE.md`,
//!   `.cursorrules`; kitbag writes `.hermes.md` (or `HERMES.md`) when the
//!   project has one, since it would shadow `AGENTS.md`, and `AGENTS.md`
//!   (shared with Codex, Cursor, Antigravity, opencode, Windsurf and Grok
//!   Build) otherwise.
//! * Hooks: the shell hooks of the `hooks:` block in `config.yaml` (user scope
//!   only), through `hook_files`: `capture --include hooks` reads them,
//!   `apply --apply-hooks` writes them, scripts go to `agent-hooks/kitbag/`.
//!   Hermes asks for approval of each new hook (`shell-hooks-allowlist.json`)
//!   and kitbag never writes that file. Its other hook systems are not carried:
//!   gateway hooks (`hooks/<name>/HOOK.yaml` + `handler.py`), plugin hooks
//!   (`ctx.register_hook()`) and outbound webhooks (`hooks.outbound`, left as
//!   it is when `hooks:` is edited).
//! * Skills: `<name>/SKILL.md` (`name` and `description` frontmatter, plus
//!   optional `references/`, `templates/`, `scripts/`, `assets/`) in
//!   `$HERMES_HOME/skills/` (the "primary directory and source of truth") and,
//!   in a project, `.hermes/skills/`, Hermes's own project folder. Hermes also
//!   reads a project's `.agents/skills/` (shared with Amp and Zed) and any
//!   `skills.external_dirs`, but kitbag writes only to the Hermes-owned folders.
//!   Hermes loads project skills only once you run `hermes skills trust` in the
//!   repository. It finds skills at any depth (the docs group them in category
//!   folders, `mlops/axolotl/SKILL.md`), so capture looks in category folders
//!   too and apply updates a skill where it already is, else writes it flat.
//!   The skills Hermes put there itself, named in `skills/.bundled_manifest`
//!   (bundled) or `skills/.hub/lock.json` (installed from the hub), are neither
//!   captured nor overwritten; Hermes keeps those up to date.
//! * Rules and commands: Hermes's docs name no rules folder (its context-file
//!   list only reads Cursor's `.cursorrules` and `.cursor/rules/*.mdc`, which
//!   Cursor owns) and no commands folder (`quick_commands` in `config.yaml` are
//!   shell or alias entries, and the docs say to put reusable prompts in a
//!   skill; every skill is already a slash command), so neither is carried.
//! * Never read or written: `.env`, `auth.json`, `mcp-tokens/` (credentials),
//!   and Hermes' own stores (`memories/`, sessions, `profiles/`,
//!   `skill-bundles/`). Run kitbag with `HERMES_HOME` set to a profile's
//!   directory to use that profile.
//!
//! Docs: <https://hermes-agent.nousresearch.com/docs/user-guide/features/mcp>,
//! <https://hermes-agent.nousresearch.com/docs/user-guide/features/skills>,
//! <https://hermes-agent.nousresearch.com/docs/user-guide/features/context-files>,
//! <https://hermes-agent.nousresearch.com/docs/user-guide/features/hooks>,
//! <https://hermes-agent.nousresearch.com/docs/user-guide/configuration/>.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use anyhow::Result;
use serde_json::{Map, Value, json};

use super::common::{
    capture_instructions, capture_managed_instructions, capture_skill, import_server,
    merge_server_def, note_dropped_server_keys, off_skipped_server, record_server_write,
    render_server, server_entries, server_from_json, servers_for, set_enabled_false,
    skill_with_required_frontmatter, skipped_servers, str_of, write_instructions, write_skill,
};
use super::{Adapter, ApplyOptions, Captured, Env, Feature, Scope, Support, Work};
use crate::edit::yaml::{YamlDoc, parse_map};
use crate::model::{Profile, Transport};
use crate::secrets::RefStyle;

const ID: &str = "hermes";
const SECTION: &str = "mcp_servers";
/// Server keys kitbag writes; anything else is preserved.
const MANAGED_KEYS: &[&str] = &[
    "command",
    "args",
    "env",
    "cwd",
    "url",
    "headers",
    "transport",
    "enabled",
];
/// Credential files and directories in the Hermes home that are never read.
const CREDENTIALS: &[&str] = &[".env", "auth.json", "mcp-tokens"];
/// Hermes-only stores kitbag does not carry (yet).
const OTHER_STORES: &[&str] = &["memories", "sessions", "profiles", "skill-bundles"];
/// Folders Hermes never looks for skills in (besides hidden ones).
const EXCLUDED_SKILL_DIRS: &[&str] = &["node_modules"];
/// How many category folders deep capture looks for skills.
const MAX_SKILL_DEPTH: usize = 3;

pub struct Hermes;

struct Paths {
    /// `None` at project scope: Hermes documents no project MCP file.
    config: Option<PathBuf>,
    instructions: PathBuf,
}

pub(super) fn hermes_home(env: &Env) -> PathBuf {
    env.location_var("HERMES_HOME")
        .unwrap_or_else(|| env.home.join(".hermes"))
}

/// Hermes's own skills folder: `$HERMES_HOME/skills`, or a project's
/// `.hermes/skills` (its native project location; `.agents/skills` is shared
/// with other tools).
fn skills_dir(env: &Env) -> PathBuf {
    match env.scope {
        Scope::User => hermes_home(env).join("skills"),
        Scope::Project => env.project.join(".hermes").join("skills"),
    }
}

fn paths(env: &Env) -> Paths {
    match env.scope {
        Scope::User => {
            let d = hermes_home(env);
            Paths {
                config: Some(d.join("config.yaml")),
                instructions: d.join("SOUL.md"),
            }
        }
        Scope::Project => Paths {
            config: None,
            instructions: project_context_file(env),
        },
    }
}

/// The project file Hermes reads first among those kitbag can write: a
/// `.hermes.md` or `HERMES.md` the project already has (it shadows
/// `AGENTS.md`), otherwise `AGENTS.md`.
fn project_context_file(env: &Env) -> PathBuf {
    [".hermes.md", "HERMES.md"]
        .iter()
        .map(|n| env.project.join(n))
        .find(|p| p.is_file())
        .unwrap_or_else(|| env.project.join("AGENTS.md"))
}

impl Adapter for Hermes {
    fn id(&self) -> &'static str {
        ID
    }
    fn name(&self) -> &'static str {
        "Hermes Agent"
    }
    fn docs(&self) -> &'static str {
        "https://hermes-agent.nousresearch.com/docs/user-guide/features/mcp"
    }
    fn scopes(&self) -> &'static [Scope] {
        &[Scope::User, Scope::Project]
    }
    fn support(&self, scope: Scope) -> [(Feature, Support); 7] {
        Feature::ALL.map(|f| {
            let s = match f {
                Feature::Mcp if scope == Scope::User => Support::Native,
                Feature::Instructions | Feature::Skills => Support::Native,
                _ => Support::None,
            };
            (f, s)
        })
    }

    fn locations(&self, env: &Env) -> Vec<(&'static str, PathBuf)> {
        let p = paths(env);
        let mut out = Vec::new();
        if let Some(c) = p.config {
            out.push(("mcp", c));
        }
        out.push(("instructions", p.instructions));
        out.push(("skills", skills_dir(env)));
        out
    }

    fn detect(&self, env: &Env) -> bool {
        match env.scope {
            Scope::User => hermes_home(env).exists(),
            // AGENTS.md is shared with other tools and says nothing about Hermes.
            Scope::Project => {
                [".hermes.md", "HERMES.md"]
                    .iter()
                    .any(|n| env.project.join(n).is_file())
                    || skills_dir(env).is_dir()
            }
        }
    }

    fn capture(&self, env: &Env) -> Result<Captured> {
        let mut cap = Captured::for_env(env);
        let p = paths(env);

        if let Some(config) = &p.config {
            if let Some(text) = cap.read_config(config, ID)? {
                cap.sources.push(config.clone());
                let doc = Value::Object(parse_map(Some(&text), config)?);
                for (name, def) in server_entries(&mut cap, ID, config, SECTION, doc.get(SECTION)) {
                    capture_server(&mut cap, name, def);
                }
                let external = doc
                    .pointer("/skills/external_dirs")
                    .and_then(Value::as_array)
                    .is_some_and(|a| !a.is_empty());
                if external {
                    cap.info(
                        ID,
                        "skills.external_dirs in config.yaml is not captured: those folders belong to whatever tool owns them, and Hermes only reads them",
                    );
                }
            }
            let home = hermes_home(env);
            let present = |names: &[&str]| -> Vec<String> {
                names
                    .iter()
                    .filter(|n| home.join(n).exists())
                    .map(|n| n.to_string())
                    .collect()
            };
            let creds = present(CREDENTIALS);
            if !creds.is_empty() {
                cap.warn(
                    ID,
                    format!(
                        "found credentials ({}) in {}; kitbag does not read or capture them",
                        creds.join(", "),
                        crate::ui::tildify(&home, &env.home)
                    ),
                );
            }
            let stores = present(OTHER_STORES);
            if !stores.is_empty() {
                cap.info(
                    ID,
                    format!(
                        "Hermes {} not captured: kitbag doesn't carry them yet",
                        stores.join(", ")
                    ),
                );
            }
        }

        if env.scope == Scope::User {
            super::hook_files::capture_for(&mut cap, env, &super::hook_files::HERMES)?;
            other_hook_systems(&mut cap, &hermes_home(env), &env.home);
        }

        capture_skills(&mut cap, env)?;

        if env.scope == Scope::User {
            // SOUL.md is Hermes's persona (a fresh install seeds one); its own
            // text is not instructions for other tools, so only a block kitbag
            // wrote there is captured.
            if capture_managed_instructions(&mut cap, &p.instructions, ID)? {
                cap.info(
                    ID,
                    "SOUL.md is Hermes's persona file; only a kitbag-managed block in it is captured as instructions, the rest was left out",
                );
            }
        } else {
            capture_instructions(&mut cap, &p.instructions, ID)?;
        }
        // Rules folded into a shared file by another target (Codex's AGENTS.md)
        // come back out of capture_instructions.
        if !cap.profile.rules.is_empty() {
            let names: Vec<String> = cap.profile.rules.drain(..).map(|r| r.name).collect();
            cap.info(
                ID,
                format!(
                    "rules folded into Hermes's instructions file by another tool were not captured ({}); capture that tool for them",
                    names.join(", ")
                ),
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
        let p = paths(env);

        if let Some(config) = &p.config {
            apply_servers(profile, env, opts, work, config)?;
        } else if !profile.mcp_servers.is_empty() {
            work.info(
                ID,
                format!(
                    "Hermes documents no project-level MCP config; {} MCP server(s) not written (apply at user scope for ~/.hermes/config.yaml)",
                    profile.mcp_servers.len()
                ),
            );
        }

        if profile.instructions.is_some() {
            match env.scope {
                Scope::User => work.info(
                    ID,
                    "Hermes has no global instructions file other than SOUL.md, its identity file; the instructions go there, in a block of their own",
                ),
                Scope::Project => project_notes(env, &p, work),
            }
        }
        // At project scope this may be AGENTS.md, with the same block id as
        // Codex, Cursor, Antigravity, opencode, Windsurf and Grok Build:
        // applying several plans the same block more than once, which leaves one.
        write_instructions(work, &p.instructions, profile, opts, false, ID)?;
        apply_skills(profile, env, opts, work)?;
        super::hook_files::apply_for(profile, env, opts, work, &super::hook_files::HERMES)?;
        if profile.settings.contains_key(ID) {
            work.info(
                ID,
                "kitbag doesn't manage Hermes settings; settings.hermes was not applied",
            );
        }
        Ok(())
    }
}

/// Every skill folder under `root`, as (its parent, its name): a folder with a
/// `SKILL.md`, in `root` or in category folders below it. Hidden folders
/// (`.hub`, `.git`, `.archive`, `.venv`) are not looked into, and neither is a
/// skill's own folder.
fn find_skills(root: &Path) -> Vec<(PathBuf, String)> {
    fn walk(dir: &Path, depth: usize, out: &mut Vec<(PathBuf, String)>) {
        for name in crate::plan_fs::subdirs(dir).unwrap_or_default() {
            if name.starts_with('.') || EXCLUDED_SKILL_DIRS.contains(&name.as_str()) {
                continue;
            }
            let path = dir.join(&name);
            if path.join("SKILL.md").is_file() {
                out.push((dir.to_path_buf(), name));
            } else if depth < MAX_SKILL_DEPTH {
                walk(&path, depth + 1, out);
            }
        }
    }
    let mut out = Vec::new();
    walk(root, 1, &mut out);
    out
}

/// Names of the skills Hermes manages itself in a skills folder: the bundled
/// ones listed in `.bundled_manifest` (`name:hash` lines, or bare names in
/// the older format) and those installed from the hub (the keys of `installed`
/// in `.hub/lock.json`).
fn hermes_managed_skills(skills: &Path) -> BTreeSet<String> {
    let read = |p: PathBuf| {
        std::fs::metadata(&p)
            .ok()
            .filter(|m| m.is_file() && m.len() <= crate::profile_io::MAX_FILE_BYTES)
            .and_then(|_| std::fs::read_to_string(p).ok())
    };
    let mut names = BTreeSet::new();
    if let Some(text) = read(skills.join(".bundled_manifest")) {
        for line in text.lines() {
            let name = line.split(':').next().unwrap_or_default().trim();
            if !name.is_empty() {
                names.insert(name.to_string());
            }
        }
    }
    if let Some(text) = read(skills.join(".hub").join("lock.json"))
        && let Ok(doc) = serde_json::from_str::<Value>(&text)
        && let Some(installed) = doc.get("installed").and_then(Value::as_object)
    {
        names.extend(installed.keys().cloned());
    }
    names
}

fn capture_skills(cap: &mut Captured, env: &Env) -> Result<()> {
    let dir = skills_dir(env);
    // Only the user's folder is Hermes's to fill: a project's .hermes/skills
    // holds what the project put there.
    let managed = if env.scope == Scope::User {
        hermes_managed_skills(&dir)
    } else {
        BTreeSet::new()
    };
    let mut seen = BTreeSet::new();
    let mut left = Vec::new();
    for (parent, name) in find_skills(&dir) {
        if managed.contains(&name) {
            left.push(name);
            continue;
        }
        if !seen.insert(name.clone()) {
            cap.warn(
                ID,
                format!(
                    "skill '{name}' is in more than one folder of {}; only the first was captured",
                    crate::ui::tildify(&dir, &env.home)
                ),
            );
            continue;
        }
        capture_skill(cap, &parent, name, ID)?;
    }
    left.sort();
    if !left.is_empty() {
        cap.info(
            ID,
            format!(
                "{} skill(s) that Hermes bundled or installed from its hub were not captured (Hermes keeps those up to date): {}",
                left.len(),
                left.join(", ")
            ),
        );
    }
    Ok(())
}

/// Write the profile's skills to Hermes's skills folder.
fn apply_skills(profile: &Profile, env: &Env, opts: &ApplyOptions, work: &mut Work) -> Result<()> {
    let dir = skills_dir(env);
    let managed = if env.scope == Scope::User {
        hermes_managed_skills(&dir)
    } else {
        BTreeSet::new()
    };
    let existing = find_skills(&dir);
    let mut wrote_any = false;
    for s in &profile.skills {
        if managed.contains(&s.name) {
            work.warn(
                ID,
                format!(
                    "skill '{}' skipped: Hermes bundled it or installed it from its hub, and keeps it up to date itself; rename the skill to carry your own",
                    s.name
                ),
            );
            continue;
        }
        let s = skill_with_required_frontmatter(s);
        // Where it already is (Hermes groups skills in category folders), else flat.
        let target = existing
            .iter()
            .find(|(_, n)| *n == s.name)
            .map(|(parent, n)| parent.join(n))
            .unwrap_or_else(|| dir.join(&s.name));
        if env.scope == Scope::Project {
            // Two skills of one name in one tier make Hermes refuse both.
            let shared = env.project.join(".agents").join("skills");
            super::common::note_shadowed_skill(work, ID, &[dir.clone(), shared], &s.name);
        }
        write_skill(work, &target, &s, opts, ID)?;
        wrote_any = true;
    }
    if wrote_any && env.scope == Scope::Project {
        work.info(
            ID,
            "Hermes loads the skills of a project's .hermes/skills only once you run `hermes skills trust` in the repository (its root is the nearest folder with a .git); kitbag leaves that choice to you",
        );
    }
    Ok(())
}

/// Hermes's gateway hooks (`hooks/<name>/HOOK.yaml`) and plugin hooks
/// (`ctx.register_hook()` in a plugin) are other systems than the shell hooks
/// kitbag carries: say so when there are some, instead of leaving them out silently.
fn other_hook_systems(cap: &mut Captured, home: &std::path::Path, user_home: &std::path::Path) {
    let subdirs = |dir: &std::path::Path| -> Vec<PathBuf> {
        let mut v: Vec<PathBuf> = std::fs::read_dir(dir)
            .into_iter()
            .flatten()
            .flatten()
            .map(|e| e.path())
            .filter(|p| p.is_dir())
            .collect();
        v.sort();
        v
    };
    let name = |p: &PathBuf| {
        p.file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default()
    };
    let gateway: Vec<String> = subdirs(&home.join("hooks"))
        .iter()
        .filter(|d| d.join("HOOK.yaml").is_file())
        .map(name)
        .collect();
    if !gateway.is_empty() {
        cap.warn(
            ID,
            format!(
                "gateway hooks ({}) in {} (HOOK.yaml + handler.py) were not captured: kitbag carries Hermes's shell hooks only",
                gateway.join(", "),
                crate::ui::tildify(&home.join("hooks"), user_home)
            ),
        );
    }
    let plugins: Vec<String> = subdirs(&home.join("plugins"))
        .iter()
        .filter(|d| {
            std::fs::read_dir(d)
                .into_iter()
                .flatten()
                .flatten()
                .map(|e| e.path())
                .filter(|p| p.extension().is_some_and(|e| e == "py") && p.is_file())
                .any(|p| {
                    std::fs::metadata(&p)
                        .is_ok_and(|m| m.len() <= crate::profile_io::MAX_FILE_BYTES)
                        && std::fs::read_to_string(&p).is_ok_and(|t| t.contains("register_hook("))
                })
        })
        .map(name)
        .collect();
    if !plugins.is_empty() {
        cap.warn(
            ID,
            format!(
                "plugin hooks (ctx.register_hook() in plugin {}) were not captured: kitbag carries Hermes's shell hooks only",
                plugins.join(", ")
            ),
        );
    }
}

/// Notes about which project file Hermes will actually read.
fn project_notes(env: &Env, p: &Paths, work: &mut Work) {
    let name = p
        .instructions
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    if name != "AGENTS.md" {
        work.info(
            ID,
            format!(
                "the project has a {name}, which Hermes loads before AGENTS.md; the instructions go there"
            ),
        );
    } else if env.project.join("AGENTS.override.md").is_file() {
        work.warn(
            ID,
            "the project has an AGENTS.override.md, which Hermes loads instead of AGENTS.md, so it won't see the instructions written to AGENTS.md",
        );
    }
}

fn capture_server(cap: &mut Captured, name: &str, def: &Map<String, Value>) {
    let mut s = server_from_json(def);
    s.disabled = def.get("enabled") == Some(&Value::Bool(false));
    if def.get("command").is_none()
        && str_of(def.get("transport")).is_some_and(|t| t.eq_ignore_ascii_case("sse"))
    {
        s.transport = Some(Transport::Sse);
    }
    let other: Vec<&str> = def
        .keys()
        .map(String::as_str)
        .filter(|k| !MANAGED_KEYS.contains(k))
        .collect();
    if !other.is_empty() {
        cap.info(
            ID,
            format!(
                "MCP server '{name}': Hermes-specific keys not captured: {}",
                other.join(", ")
            ),
        );
    }
    if str_of(def.get("auth")).is_some_and(|a| a.eq_ignore_ascii_case("oauth")) {
        cap.warn(
            ID,
            format!(
                "MCP server '{name}' signs in with OAuth; kitbag doesn't carry the sign-in or its tokens, so other tools will ask you to sign in"
            ),
        );
    }
    import_server(cap, ID, name, s, false);
}

/// The `mcp_servers` entry for `s`, in Hermes's keys.
fn entry_for(name: &str, s: &crate::model::McpServer, work: &mut Work) -> Map<String, Value> {
    let mut def = Map::new();
    match s.transport() {
        Transport::Stdio => {
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
        }
        t => {
            def.insert("url".into(), json!(s.url));
            if t == Transport::Sse {
                def.insert("transport".into(), json!("sse"));
            }
            if !s.headers.is_empty() {
                def.insert("headers".into(), json!(s.headers));
            }
            if s.cwd.is_some() {
                work.warn(
                    ID,
                    format!(
                        "MCP server '{name}': 'cwd' only applies to local servers and was dropped"
                    ),
                );
            }
        }
    }
    if s.disabled {
        def.insert("enabled".into(), json!(false));
    }
    def
}

fn apply_servers(
    profile: &Profile,
    env: &Env,
    opts: &ApplyOptions,
    work: &mut Work,
    path: &std::path::Path,
) -> Result<()> {
    let mut servers: Vec<(String, Value)> = Vec::new();
    let mut with_refs = false;
    for (name, s) in servers_for(profile, ID, work, true) {
        let s = render_server(s, RefStyle::DollarPlain, env, work);
        with_refs |= crate::secrets::has_ref(&format!(
            "{:?}{:?}{:?}{:?}",
            s.command, s.args, s.env, s.headers
        )) || s.url.as_deref().is_some_and(crate::secrets::has_ref);
        let def = entry_for(name, &s, work);
        servers.push((name.clone(), Value::Object(def)));
    }
    if with_refs {
        work.info(
            ID,
            "MCP servers reference ${VAR}s; Hermes expands them from its environment and ~/.hermes/.env, which kitbag never reads or writes, so make sure they are set there",
        );
    }
    merge_yaml_servers(work, path, &servers, opts)
}

/// Merge server definitions into `mcp_servers` of a YAML file, leaving every
/// other key, and other servers, as they are.
fn merge_yaml_servers(
    work: &mut Work,
    path: &std::path::Path,
    servers: &[(String, Value)],
    opts: &ApplyOptions,
) -> Result<()> {
    let skipped = skipped_servers(work, ID);
    if servers.is_empty() && skipped.is_empty() {
        return Ok(());
    }
    let text = work.fs.read(path)?;
    if servers.is_empty() && text.is_none() {
        return Ok(());
    }
    let mut doc = YamlDoc::parse(text.as_deref(), path)?;
    if let Err(why) = doc.check_editable(SECTION) {
        work.warn(
            ID,
            format!(
                "MCP servers not written to {}: {why}, which kitbag can't edit in place; add them by hand",
                crate::ui::tildify(path, &work.home)
            ),
        );
        return Ok(());
    }
    let existing = doc.entries(SECTION);
    for name in &skipped {
        if let Some(def) = off_skipped_server(
            work,
            opts,
            ID,
            path,
            name,
            existing.get(name),
            (&set_enabled_false, "enabled: false"),
        ) {
            doc.set(SECTION, name, &def)?;
        }
    }
    for (name, def) in servers {
        let Value::Object(want) = def else { continue };
        let cur = existing.get(name).and_then(Value::as_object);
        let mut want = want.clone();
        // Keep a `transport` kitbag doesn't know: only `sse` is documented.
        if let Some(t) = cur.and_then(|c| c.get("transport"))
            && !want.contains_key("transport")
            && !t.as_str().is_some_and(|t| t.eq_ignore_ascii_case("sse"))
        {
            want.insert("transport".into(), t.clone());
        }
        let Some(merged) = merge_server_def(cur, &want, MANAGED_KEYS) else {
            continue;
        };
        if cur.is_some() && !opts.overwrite {
            work.warn(
                ID,
                format!(
                    "kept existing MCP server '{name}' in {} because --no-overwrite was given",
                    path.display()
                ),
            );
            continue;
        }
        note_dropped_server_keys(work, ID, name, path, &merged.dropped);
        let def = Value::Object(merged.def);
        doc.set(SECTION, name, &def)?;
        work.changed_servers.push((ID.to_string(), name.clone()));
        record_server_write(work, ID, path, name, Some(&def));
    }
    if doc.changed() {
        work.fs.write(path, doc.render()?)?;
    }
    Ok(())
}
