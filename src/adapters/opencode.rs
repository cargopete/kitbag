//! opencode: MCP servers, instructions, rules, commands, skills and agents.
//!
//! * Config: `~/.config/opencode/opencode.json(c)` (`$XDG_CONFIG_HOME/opencode`
//!   when that is set) at user scope; `opencode.json(c)` or
//!   `.opencode/opencode.json(c)` in a project. Capture reads every one that
//!   exists; apply writes the first that exists, or creates `opencode.json`.
//!   The files are JSONC, so comments and trailing commas are kept.
//! * MCP: opencode 2 keeps servers under `mcp.servers`; opencode 1 kept them
//!   directly under `mcp`, and opencode 2 still reads that. Local servers are
//!   `type: local` with `command` (an array: the program and its arguments),
//!   `environment` and `cwd`; remote servers are `type: remote` with `url`
//!   (Streamable HTTP) and `headers`. opencode 2 turns a server off with
//!   `disabled: true`, opencode 1 with `enabled: false`. Apply writes the
//!   layout the file already uses (`mcp.servers` for a new file), so a file
//!   opencode 1 still reads stays readable. opencode expands `{env:VAR}` in
//!   config strings, so references are written that way and secrets never
//!   touch disk. SSE servers are skipped.
//! * Instructions: `~/.config/opencode/AGENTS.md` (user) and `AGENTS.md`
//!   (project, shared with Codex, Cursor and Antigravity). opencode reads
//!   `~/.claude/CLAUDE.md` / a project's `CLAUDE.md` only while there is no
//!   `AGENTS.md`; those belong to Claude Code and are not captured here.
//! * Rules: opencode has no rule files (its rules are `AGENTS.md`, plus the
//!   `instructions` list in its config), so profile rules are folded into
//!   `AGENTS.md` as labelled sections, as Codex does; capture reads them back.
//!   The same block in a file shared with Codex is written once.
//! * Commands: `commands/*.md` in the config dir (`~/.config/opencode`) or in
//!   `.opencode/`; opencode 1's singular `command/` is read too, and written
//!   to when it is the one that exists. Frontmatter: `description`, `agent`,
//!   `model` (`provider/model`) and `subagent` (`subtask` in opencode 1); the
//!   body is the template (`$ARGUMENTS`, `$1`.., `` !`cmd` ``).
//! * Skills: `skills/<name>/SKILL.md` (`skill/` in opencode 1) in the same
//!   places. opencode also reads `.claude/skills` and `.agents/skills`.
//! * Agents: `agents/<name>.md` (`agent/` in opencode 1). The body is the
//!   system prompt. Only frontmatter keys opencode documents are written;
//!   keys from other tools (`tools` lists, `permissionMode`, `hooks` ...) are
//!   left out with a note rather than translated. `mode/` and `modes/` (opencode
//!   1 primary agents) and agents and commands defined inside `opencode.json`
//!   are not captured.
//! * Plugins, by reference: the npm packages in the `plugins` array of the
//!   config (`plugin` in opencode 1), which opencode installs itself (see
//!   `plugins.rs`). Local paths, `-name` entries and plugin options are left
//!   out, and the `plugins/` folders, which are the plugin code, are never
//!   copied. Apply appends to the array the file already has, in the config
//!   file the MCP servers go to.
//!
//! * Settings: `model` (`provider/model`), `default_agent` and the V2
//!   `permissions` array (`{action, resource, effect}`), from
//!   opencode.ai/docs/config and /docs/permissions (checked 2026-11-27), in the
//!   config file the MCP servers go to. The last matching permission rule
//!   wins, so rules from a profile are put before the ones the file has: the
//!   file's own rules keep the last word. opencode 1's `permission` object is a
//!   different shape and is neither read nor written, and `permissions` is
//!   left out of a file that still uses the opencode 1 layout. Theme and
//!   terminal settings live in opencode 2's `cli.json`, which is
//!   carried separately at user scope (see `apply_cli_settings`).
//!
//! Docs: <https://opencode.ai/docs/config/>, <https://opencode.ai/docs/mcp-servers/>,
//! <https://opencode.ai/docs/rules/>, <https://opencode.ai/docs/commands/>,
//! <https://opencode.ai/docs/skills/>, <https://opencode.ai/docs/agents/>,
//! <https://opencode.ai/docs/migrate-v1/>.

use std::path::{Path, PathBuf};

use anyhow::Result;
use serde_json::{Map, Value, json};

use super::common::{
    MdKind, add_agent, capture_instructions, capture_markdown_dir, capture_skills_dir, claim_name,
    drop_redacted_settings, import_server, merge_json_servers_off, merge_json_settings,
    name_from_rel, pick_settings, read_text_item, render_server, safe_settings, server_entries,
    servers_for, set_disabled_true, set_enabled_false, str_of, string_list, string_map,
    write_agent, write_command, write_instructions, write_skill,
};
use super::{Adapter, ApplyOptions, Captured, Env, Feature, Scope, Support, Work};
use crate::edit::json::{JsonDoc, parse_value};
use crate::frontmatter;
use crate::model::{Agent, Command, McpServer, Profile, Transport, is_valid_name};
use crate::plan_fs::{join_rel, walk_files};
use crate::policy;
use crate::profile_io::agent_from_markdown;
use crate::schema::SchemaSpec;
use crate::secrets::{RefStyle, normalize_brace_refs};
use crate::ui::tildify;

const ID: &str = "opencode";
/// Server keys kitbag writes; anything else (`timeout`, `oauth`, `codemode`,
/// `protocol` ...) is preserved. Both on/off keys are listed so turning a
/// server back on removes whichever one turned it off.
const MANAGED_KEYS: &[&str] = &[
    "type",
    "command",
    "environment",
    "cwd",
    "url",
    "headers",
    "disabled",
    "enabled",
];

/// Command frontmatter keys opencode documents besides `description`
/// (`subtask` is opencode 1's name for `subagent`, `variant` its separate
/// model variant). Others, such as Claude Code's `allowed-tools`, mean
/// nothing to opencode and are left out with a note.
const COMMAND_KEYS: &[&str] = &["agent", "model", "subagent", "subtask", "variant"];
/// Agent frontmatter keys opencode documents besides `description`: opencode
/// 2's (`permissions`, `steps`, `disabled`, `request` ...) and opencode 1's
/// (`permission`, `maxSteps`, `disable`, `temperature`, `top_p`, `options`,
/// and `tools`, which is a map there). `prompt` and `system` are JSON-only;
/// in a file the body is the prompt.
const AGENT_KEYS: &[&str] = &[
    "mode",
    "model",
    "variant",
    "permissions",
    "permission",
    "steps",
    "maxSteps",
    "hidden",
    "color",
    "disabled",
    "disable",
    "request",
    "temperature",
    "top_p",
    "options",
    "tools",
];

pub struct OpenCode;

struct Paths {
    /// Config files opencode reads at this scope, in the order kitbag picks
    /// one to write.
    configs: Vec<PathBuf>,
    instructions: PathBuf,
    /// What opencode reads instead of `instructions` while that doesn't exist.
    fallback: PathBuf,
    /// Where opencode keeps commands, skills and agents.
    base: PathBuf,
}

/// A directory of one kind of item: the current plural name, and opencode 1's
/// singular one, which opencode 2 still reads.
struct ItemDir {
    plural: PathBuf,
    legacy: PathBuf,
}

impl ItemDir {
    fn new(base: &Path, plural: &str, legacy: &str) -> Self {
        ItemDir {
            plural: base.join(plural),
            legacy: base.join(legacy),
        }
    }
    /// Where new files go: the plural directory, unless only the singular one
    /// exists.
    fn target(&self) -> &Path {
        if !self.plural.exists() && self.legacy.is_dir() {
            &self.legacy
        } else {
            &self.plural
        }
    }
}

impl Paths {
    fn commands(&self) -> ItemDir {
        ItemDir::new(&self.base, "commands", "command")
    }
    fn skills(&self) -> ItemDir {
        ItemDir::new(&self.base, "skills", "skill")
    }
    fn agents(&self) -> ItemDir {
        ItemDir::new(&self.base, "agents", "agent")
    }
}

fn config_dir(env: &Env) -> PathBuf {
    env.location_var("XDG_CONFIG_HOME")
        .unwrap_or_else(|| env.home.join(".config"))
        .join("opencode")
}

fn paths(env: &Env) -> Paths {
    match env.scope {
        Scope::User => {
            let d = config_dir(env);
            Paths {
                configs: vec![d.join("opencode.json"), d.join("opencode.jsonc")],
                instructions: d.join("AGENTS.md"),
                fallback: env.home.join(".claude").join("CLAUDE.md"),
                base: d,
            }
        }
        Scope::Project => {
            let p = &env.project;
            let o = p.join(".opencode");
            Paths {
                configs: vec![
                    p.join("opencode.json"),
                    p.join("opencode.jsonc"),
                    o.join("opencode.json"),
                    o.join("opencode.jsonc"),
                ],
                instructions: p.join("AGENTS.md"),
                fallback: p.join("CLAUDE.md"),
                base: o,
            }
        }
    }
}

/// The two layouts of the `mcp` section, from opencode's migration guide
/// (<https://opencode.ai/docs/migrate-v1/>, read 2026-11-27): V2 "groups
/// servers under `mcp.servers`, replaces `enabled` with the inverse
/// `disabled`", and "normalizes supported V1 and native V2 fields in memory
/// without rewriting the source file". Both are read and written: a server
/// is the same in a profile either way, and apply writes the layout the file
/// already uses.
const SCHEMAS: &[SchemaSpec] = &[
    SchemaSpec {
        id: "v1",
        summary: "mcp.<name> with `enabled` (opencode 1; opencode 2 still reads it)",
        read: true,
        write: true,
    },
    SchemaSpec {
        id: "v2",
        summary: "mcp.servers.<name> with `disabled` (opencode 2)",
        read: true,
        write: true,
    },
];

/// Is `file`, a path in the project, really inside it (not a symbolic link out)?
fn stays_in(file: &Path, project: &Path) -> bool {
    match (file.canonicalize(), project.canonicalize()) {
        (Ok(f), Ok(p)) => f.starts_with(p),
        _ => false,
    }
}

/// Where servers live in a config file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Layout {
    /// `mcp.servers.<name>`, with `disabled` (opencode 2).
    Servers,
    /// `mcp.<name>`, with `enabled` (opencode 1, still read by opencode 2).
    Legacy,
}

/// Keys opencode itself uses directly under `mcp`, so they can't be server
/// names in the opencode 1 layout.
const RESERVED_MCP_KEYS: &[&str] = &["servers", "timeout"];

/// An opencode 1 server entry directly under `mcp`: an object with a `type`.
fn is_legacy_entry(name: &str, def: &Value) -> bool {
    !RESERVED_MCP_KEYS.contains(&name) && def.get("type").is_some()
}

/// The layout a config file already uses; `mcp.servers` unless it only has
/// opencode 1 entries.
fn layout(doc: &Value) -> Layout {
    let Some(Value::Object(mcp)) = doc.get("mcp") else {
        return Layout::Servers;
    };
    if mcp.get("servers").is_some_and(Value::is_object) {
        return Layout::Servers;
    }
    if mcp.iter().any(|(k, v)| is_legacy_entry(k, v)) {
        Layout::Legacy
    } else {
        Layout::Servers
    }
}

fn import(cap: &mut Captured, file: &Path, name: &str, def: &Map<String, Value>) {
    let mut s = McpServer::default();
    match def.get("type").and_then(Value::as_str) {
        Some("local") => {
            let cmd = match def.get("command") {
                Some(Value::String(c)) => vec![c.clone()],
                v => string_list(v),
            };
            if let Some((first, rest)) = cmd.split_first() {
                s.command = Some(first.clone());
                s.args = rest.to_vec();
            }
            s.env = string_map(def.get("environment"));
            s.cwd = str_of(def.get("cwd"));
        }
        Some("remote") => {
            s.url = str_of(def.get("url"));
            s.headers = string_map(def.get("headers"));
            s.oauth = crate::oauth::read(ID, def);
        }
        other => {
            let found = other.map_or("no type".to_string(), |t| format!("type '{t}'"));
            cap.warn(
                ID,
                format!(
                    "skipped MCP server '{name}' in {}: expected type 'local' or 'remote', found {found}",
                    file.display()
                ),
            );
            return;
        }
    }
    s.disabled = def.get("disabled") == Some(&Value::Bool(true))
        || def.get("enabled") == Some(&Value::Bool(false));
    if Value::Object(def.clone()).to_string().contains("{file:") {
        cap.warn(
            ID,
            format!("MCP server '{name}' uses opencode's {{file:...}} references; other tools won't expand them"),
        );
    }
    let s = s.map_strings(normalize_brace_refs);
    import_server(cap, ID, name, s, false);
}

impl Adapter for OpenCode {
    fn id(&self) -> &'static str {
        ID
    }
    fn name(&self) -> &'static str {
        "opencode"
    }
    fn docs(&self) -> &'static str {
        "https://opencode.ai/docs/mcp-servers/"
    }
    fn scopes(&self) -> &'static [Scope] {
        &[Scope::User, Scope::Project]
    }
    fn support(&self, _scope: Scope) -> [(Feature, Support); 8] {
        Feature::ALL.map(|f| {
            (
                f,
                match f {
                    Feature::Mcp
                    | Feature::Instructions
                    | Feature::Commands
                    | Feature::Skills
                    | Feature::Agents
                    | Feature::Settings => Support::Native,
                    // No rule files: rules are folded into AGENTS.md.
                    Feature::Rules => Support::Folded,
                    _ => Support::None,
                },
            )
        })
    }

    fn locations(&self, env: &Env) -> Vec<(&'static str, PathBuf)> {
        let p = paths(env);
        let config = p
            .configs
            .iter()
            .find(|c| c.exists())
            .unwrap_or(&p.configs[0])
            .clone();
        let mut out = vec![
            ("mcp", config),
            ("instructions", p.instructions.clone()),
            ("commands", p.commands().target().to_path_buf()),
            ("skills", p.skills().target().to_path_buf()),
            ("agents", p.agents().target().to_path_buf()),
        ];
        if env.scope == Scope::User {
            out.push(("settings", cli_config(env)));
        }
        out
    }

    fn schemas(&self) -> &'static [SchemaSpec] {
        SCHEMAS
    }

    /// The layout the config file apply would write to uses, when it has
    /// servers to tell by: an empty or server-less file says nothing (apply
    /// writes `mcp.servers` there, but the tool has not chosen it).
    fn detect_schema(&self, env: &Env) -> Option<String> {
        let p = paths(env);
        let file = p.configs.iter().find(|c| c.exists())?;
        if env.scope == Scope::Project && !stays_in(file, &env.project) {
            return None;
        }
        let text = crate::plan_fs::read_text_opt(file).ok()??;
        let doc = parse_value(Some(&text), file).ok()?;
        let Value::Object(mcp) = doc.get("mcp")? else {
            return None;
        };
        if mcp.get("servers").is_some_and(Value::is_object) {
            Some("v2".into())
        } else if mcp.iter().any(|(k, v)| is_legacy_entry(k, v)) {
            Some("v1".into())
        } else {
            None
        }
    }

    fn detect(&self, env: &Env) -> bool {
        match env.scope {
            Scope::User => config_dir(env).exists(),
            Scope::Project => {
                env.project.join(".opencode").exists()
                    || paths(env).configs.iter().any(|c| c.exists())
            }
        }
    }

    fn capture(&self, env: &Env) -> Result<Captured> {
        let mut cap = Captured::for_env(env);
        let p = paths(env);
        let mut settings_read = false;
        if env.scope == Scope::User {
            let file = cli_config(env);
            if let Some(text) = cap.read_config(&file, ID)? {
                cap.sources.push(file.clone());
                if let Value::Object(doc) = parse_value(Some(&text), &file)? {
                    let mut cli = doc;
                    cli.retain(|k, _| policy::OPENCODE_CLI_KEYS.contains(&k.as_str()));
                    pick_settings(&mut cap, ID, &cli);
                }
            }
        }
        for file in &p.configs {
            let Some(text) = cap.read_config(file, ID)? else {
                continue;
            };
            cap.sources.push(file.clone());
            let doc = parse_value(Some(&text), file)?;
            // The settings come from the first config file, the one apply writes to.
            if !settings_read && let Value::Object(map) = &doc {
                settings_read = true;
                let mut own = map.clone();
                own.retain(|k, _| !policy::OPENCODE_CLI_KEYS.contains(&k.as_str()));
                pick_settings(&mut cap, ID, &own);
            }
            crate::plugins::capture_opencode(&mut cap, &doc, &tildify(file, &env.home));
            match doc.get("mcp") {
                None | Some(Value::Null) => {}
                Some(Value::Object(mcp)) => {
                    for (name, def) in
                        server_entries(&mut cap, ID, file, "mcp.servers", mcp.get("servers"))
                    {
                        import(&mut cap, file, name, def);
                    }
                    for (name, def) in mcp {
                        if let (true, Value::Object(d)) = (is_legacy_entry(name, def), def) {
                            import(&mut cap, file, name, d);
                        }
                    }
                }
                Some(_) => cap.warn(
                    ID,
                    format!("ignored 'mcp' in {}: expected an object", file.display()),
                ),
            }
            for (keys, what) in [
                (["commands", "command"], "commands"),
                (["agents", "agent"], "agents"),
            ] {
                let n: usize = keys
                    .iter()
                    .filter_map(|k| doc.get(*k).and_then(Value::as_object))
                    .map(Map::len)
                    .sum();
                if n > 0 {
                    cap.info(
                        ID,
                        format!(
                            "{n} {what} defined in {} not captured; only {what} kept as markdown files are",
                            tildify(file, &env.home)
                        ),
                    );
                }
            }
            if doc
                .get("instructions")
                .and_then(Value::as_array)
                .is_some_and(|a| !a.is_empty())
            {
                cap.info(
                    ID,
                    format!(
                        "{} lists more instruction files under 'instructions'; only {} was captured",
                        tildify(file, &env.home),
                        tildify(&p.instructions, &env.home)
                    ),
                );
            }
        }
        // Rules folded into AGENTS.md (by kitbag, here or for Codex) come back
        // as rules; opencode has no rule files of its own.
        capture_instructions(&mut cap, &p.instructions, ID)?;
        let commands = p.commands();
        for dir in [&commands.plural, &commands.legacy] {
            capture_markdown_dir(&mut cap, dir, ".md", MdKind::Command, ID)?;
        }
        let skills = p.skills();
        for dir in [&skills.plural, &skills.legacy] {
            capture_skills_dir(&mut cap, dir, ID)?;
        }
        let agents = p.agents();
        for dir in [&agents.plural, &agents.legacy] {
            capture_agents(&mut cap, dir)?;
        }
        for name in ["mode", "modes", "plugin", "plugins"] {
            let dir = p.base.join(name);
            if dir.is_dir() {
                let why = if name.starts_with("mode") {
                    "opencode 1 primary agents) not captured; kitbag doesn't carry them"
                } else {
                    "plugin code) not copied; only npm packages listed in the config are carried, by reference"
                };
                cap.info(ID, format!("{} ({why}", tildify(&dir, &env.home)));
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
        let mut existing = Vec::new();
        for c in &p.configs {
            if work.fs.read(c)?.is_some() {
                existing.push(c.clone());
            }
        }
        let file = existing.first().unwrap_or(&p.configs[0]).clone();
        let layout = layout(&parse_value(work.fs.read(&file)?.as_deref(), &file)?);

        let mut servers = Vec::new();
        let mut sse = Vec::new();
        let mut reserved = Vec::new();
        for (name, s) in servers_for(profile, ID, work, true) {
            // In the opencode 1 layout a server named `servers` or `timeout`
            // would overwrite opencode's own key under `mcp`.
            if layout == Layout::Legacy && RESERVED_MCP_KEYS.contains(&name.as_str()) {
                reserved.push(name.as_str());
                continue;
            }
            let original = s;
            let s = render_server(s, RefStyle::EnvBrace, env, work);
            let mut def = Map::new();
            match s.transport() {
                Transport::Stdio => {
                    def.insert("type".into(), json!("local"));
                    let mut cmd = vec![s.command.clone().unwrap_or_default()];
                    cmd.extend(s.args.iter().cloned());
                    def.insert("command".into(), json!(cmd));
                    if !s.env.is_empty() {
                        def.insert("environment".into(), json!(s.env));
                    }
                    if let Some(cwd) = &s.cwd {
                        def.insert("cwd".into(), json!(cwd));
                    }
                }
                Transport::Http => {
                    def.insert("type".into(), json!("remote"));
                    def.insert("url".into(), json!(s.url));
                    if !s.headers.is_empty() {
                        def.insert("headers".into(), json!(s.headers));
                    }
                    super::common::add_oauth(work, ID, name, (original, &s), &mut def);
                }
                Transport::Sse => {
                    sse.push(name.as_str());
                    continue;
                }
            }
            if s.disabled {
                match layout {
                    Layout::Servers => def.insert("disabled".into(), json!(true)),
                    Layout::Legacy => def.insert("enabled".into(), json!(false)),
                };
            }
            servers.push((name.clone(), Value::Object(def)));
        }
        if !sse.is_empty() {
            work.warn(
                ID,
                format!(
                    "MCP server(s) {} skipped: opencode connects to remote servers over Streamable HTTP, not SSE",
                    sse.join(", ")
                ),
            );
        }
        if !reserved.is_empty() {
            work.warn(
                ID,
                format!(
                    "MCP server(s) {} skipped: {} keeps servers directly under 'mcp', where {} are opencode's own keys; rename the server(s) in the profile",
                    reserved.join(", "),
                    tildify(&file, &work.home),
                    RESERVED_MCP_KEYS.join(" and ")
                ),
            );
        }
        if !servers.is_empty() && existing.len() > 1 {
            let others: Vec<String> = existing[1..]
                .iter()
                .map(|c| tildify(c, &work.home))
                .collect();
            work.info(
                ID,
                format!(
                    "opencode also reads {}; MCP servers were written to {} only",
                    others.join(", "),
                    tildify(&file, &work.home)
                ),
            );
        }
        let key_path: &[&str] = match layout {
            Layout::Servers => &["mcp", "servers"],
            Layout::Legacy => &["mcp"],
        };
        let (set_off, flag): (fn(&mut Map<String, Value>), &str) = match layout {
            Layout::Servers => (set_disabled_true, "disabled: true"),
            Layout::Legacy => (set_enabled_false, "enabled: false"),
        };
        merge_json_servers_off(
            work,
            (&file, key_path),
            &servers,
            MANAGED_KEYS,
            opts,
            ID,
            Some((&set_off, flag)),
        )?;

        crate::plugins::apply_opencode(profile, opts, work, &file, layout == Layout::Legacy)?;

        let had_instructions = work.fs.read(&p.instructions)?.is_some();
        write_instructions(work, &p.instructions, profile, opts, true, ID)?;
        if !profile.rules.is_empty() {
            work.info(
                ID,
                "opencode has no rule files (its rules are AGENTS.md); rules were folded into AGENTS.md as labelled sections, with their globs in the label",
            );
        }
        if !had_instructions
            && work.fs.read(&p.instructions)?.is_some()
            && work.fs.read(&p.fallback)?.is_some()
        {
            work.info(
                ID,
                format!(
                    "opencode reads {} only while {} doesn't exist; with the new file it reads that instead",
                    tildify(&p.fallback, &work.home),
                    tildify(&p.instructions, &work.home)
                ),
            );
        }
        let commands = p.commands();
        for c in &profile.commands {
            apply_command(work, commands.target(), c, opts)?;
        }
        let skills = p.skills();
        for s in &profile.skills {
            if s.description.is_none() {
                work.info(
                    ID,
                    format!(
                        "skill '{}' has no description; opencode doesn't advertise a skill to the model without one",
                        s.name
                    ),
                );
            }
            write_skill(work, &skills.target().join(&s.name), s, opts, ID)?;
        }
        if !profile.skills.is_empty() && work.targets.iter().any(|t| t == "claude-code") {
            work.info(
                ID,
                "opencode also reads Claude Code's skills folder (.claude/skills), so these skills are visible to it from there as well",
            );
        }
        let agents = p.agents();
        for a in &profile.agents {
            apply_agent(work, agents.target(), a, opts)?;
        }
        let mut settings = safe_settings(profile, ID, work);
        drop_redacted_settings(&mut settings, ID, work);
        // `cli.json` keys go to that file, never to `opencode.json`.
        let mut cli = settings.clone();
        cli.retain(|k, _| policy::OPENCODE_CLI_KEYS.contains(&k.as_str()));
        settings.retain(|k, _| !policy::OPENCODE_CLI_KEYS.contains(&k.as_str()));
        let rules = settings.remove("permissions");
        merge_json_settings(work, &file, &settings, opts, ID)?;
        apply_cli_settings(work, env, cli, opts)?;
        if let Some(rules) = rules {
            apply_permissions(work, &file, layout, &rules)?;
        }
        Ok(())
    }
}

/// Where opencode 2 keeps its terminal UI settings: one global file, with no
/// project-level counterpart.
fn cli_config(env: &Env) -> PathBuf {
    config_dir(env).join("cli.json")
}

const CLI_SCHEMA: &str = "https://opencode.ai/v2/cli.json";

/// Write the allow-listed `cli.json` settings, at user scope only.
fn apply_cli_settings(
    work: &mut Work,
    env: &Env,
    mut cli: Map<String, Value>,
    opts: &ApplyOptions,
) -> Result<()> {
    if cli.is_empty() {
        return Ok(());
    }
    if env.scope != Scope::User {
        work.info(
            ID,
            "opencode's cli.json settings were not applied to the project: opencode has one global cli.json and no project file; apply them at user scope",
        );
        return Ok(());
    }
    let file = cli_config(env);
    if work.fs.read(&file)?.is_none() {
        let mut with_schema = Map::new();
        with_schema.insert("$schema".into(), Value::String(CLI_SCHEMA.into()));
        with_schema.append(&mut cli);
        cli = with_schema;
    }
    merge_json_settings(work, &file, &cli, opts, ID)?;
    work.info(
        ID,
        format!(
            "terminal UI settings went to {}, opencode's cli.json; opencode.json was not used for them",
            crate::ui::tildify(&file, &work.home)
        ),
    );
    Ok(())
}

/// Add the profile's permission rules to the `permissions` array of `file`,
/// before the rules it has: opencode uses the last rule that matches, so what
/// the file already says keeps the last word. Rules it has are not repeated.
fn apply_permissions(work: &mut Work, file: &Path, layout: Layout, rules: &Value) -> Result<()> {
    let Value::Array(rules) = rules else {
        work.warn(
            ID,
            "settings.opencode.permissions is not a list of rules; not applied",
        );
        return Ok(());
    };
    let text = work.fs.read(file)?;
    let mut doc = JsonDoc::parse(text.as_deref(), file)?;
    let current = doc.value();
    if layout == Layout::Legacy || current.get("permission").is_some() {
        work.warn(
            ID,
            format!(
                "{} uses opencode 1's layout (a 'permission' object, or MCP servers directly under 'mcp'); permissions were not applied, since the 'permissions' list is opencode 2's",
                file.display()
            ),
        );
        return Ok(());
    }
    let have: Vec<Value> = match current.get("permissions") {
        None | Some(Value::Null) => Vec::new(),
        Some(Value::Array(a)) => a.clone(),
        Some(_) => {
            work.warn(
                ID,
                format!(
                    "'permissions' in {} is not a list; left as it is",
                    file.display()
                ),
            );
            return Ok(());
        }
    };
    let mut merged: Vec<Value> = Vec::new();
    for r in rules {
        if !have.contains(r) && !merged.contains(r) {
            merged.push(r.clone());
        }
    }
    if merged.is_empty() {
        return Ok(());
    }
    merged.extend(have);
    doc.set(&["permissions"], &Value::Array(merged));
    work.fs.write(file, doc.render())
}

/// Is `model` in opencode's `provider/model[#variant]` form?
fn opencode_model(model: &Value) -> bool {
    model.as_str().is_some_and(|m| {
        m.split_once('/')
            .is_some_and(|(p, r)| !p.is_empty() && !r.is_empty())
    })
}

/// A command as an opencode command file: `description`, then the keys
/// opencode documents; and the keys that were left out. A `model` that isn't
/// `provider/model` is another tool's alias (`sonnet`), which opencode can't
/// resolve, so it is left out rather than guessed at.
fn opencode_command(c: &Command) -> (String, Vec<String>) {
    let mut entries: Vec<(&str, Value)> = vec![(
        "description",
        c.description.clone().map_or(Value::Null, Value::String),
    )];
    let mut dropped = Vec::new();
    if c.argument_hint.is_some() {
        dropped.push("argument-hint".to_string());
    }
    for (k, v) in &c.extra {
        if !COMMAND_KEYS.contains(&k.as_str()) {
            dropped.push(k.clone());
        } else if k == "model" && !opencode_model(v) {
            dropped.push(format!("{k} (not provider/model)"));
        } else {
            entries.push((k.as_str(), v.clone()));
        }
    }
    (frontmatter::render(&entries, &c.body), dropped)
}

fn apply_command(work: &mut Work, dir: &Path, c: &Command, opts: &ApplyOptions) -> Result<()> {
    let (text, dropped) = opencode_command(c);
    if !dropped.is_empty() {
        work.info(
            ID,
            format!(
                "command '{}': {} left out of the command file (not an opencode command key, or a value opencode can't use)",
                c.name,
                dropped.join(", ")
            ),
        );
    }
    let path = dir.join(format!("{}.md", c.name));
    write_command(work, &path, text.as_bytes(), opts, ID, &c.name)
}

/// An agent as an opencode agent file, and the keys left out: those opencode
/// doesn't document (another tool's `tools` list, `permissionMode`, `hooks`,
/// `name` ...), a `tools` that isn't opencode 1's map, and a `model` that
/// isn't `provider/model`. Nothing is translated.
fn opencode_agent(a: &Agent) -> (String, Vec<String>) {
    let mut entries: Vec<(&str, Value)> = vec![(
        "description",
        a.description.clone().map_or(Value::Null, Value::String),
    )];
    let mut dropped = Vec::new();
    for (k, v) in &a.extra {
        let ok = AGENT_KEYS.contains(&k.as_str())
            && match k.as_str() {
                "tools" => v.is_object(),
                "model" => opencode_model(v),
                _ => true,
            };
        if ok {
            entries.push((k.as_str(), v.clone()));
        } else {
            dropped.push(k.clone());
        }
    }
    (frontmatter::render(&entries, &a.body), dropped)
}

fn apply_agent(work: &mut Work, dir: &Path, a: &Agent, opts: &ApplyOptions) -> Result<()> {
    let (text, mut dropped) = opencode_agent(a);
    // opencode takes the agent's name from the file name, so a `name` is not lost.
    dropped.retain(|k| k != "name");
    if !dropped.is_empty() {
        work.warn(
            ID,
            format!(
                "agent '{}': {} left out of the agent file; opencode's agent format has no counterpart that kitbag could carry over (tool lists, permissions and models are not translated between tools)",
                a.name,
                dropped.join(", ")
            ),
        );
    }
    if a.description.is_none() {
        work.info(
            ID,
            format!(
                "agent '{}' has no description; opencode asks for one on subagents",
                a.name
            ),
        );
    }
    if !a.extra.contains_key("mode") {
        work.info(
            ID,
            format!(
                "agent '{}' has no mode; opencode treats a custom agent without one as a primary agent (add `mode: subagent` for a subagent)",
                a.name
            ),
        );
    }
    let path = dir.join(format!("{}.md", a.name));
    write_agent(work, &path, text.as_bytes(), opts, ID, &a.name)
}

/// Capture `<dir>/**/*.md` as agents named after the file (folders become part
/// of the name, as for commands); a README is skipped.
fn capture_agents(cap: &mut Captured, dir: &Path) -> Result<()> {
    if cap.escapes(dir, ID) {
        return Ok(());
    }
    let mut seen = std::collections::BTreeMap::new();
    for rel in walk_files(dir, 500)? {
        if !rel.ends_with(".md") {
            continue;
        }
        let path = join_rel(dir, &rel);
        if rel
            .rsplit('/')
            .next()
            .is_some_and(|f| f.eq_ignore_ascii_case("readme.md"))
        {
            cap.warn(
                ID,
                format!("skipped {}: a README, not an agent", path.display()),
            );
            continue;
        }
        let name = name_from_rel(&rel, ".md");
        claim_name(&mut seen, dir, &rel, &name)?;
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
        if add_agent(cap, agent_from_markdown(&name, &text), ID) {
            cap.sources.push(path);
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn layout_follows_the_file() {
        assert_eq!(layout(&json!({})), Layout::Servers);
        assert_eq!(
            layout(&json!({"mcp": {"timeout": {"catalog": 1000}}})),
            Layout::Servers
        );
        assert_eq!(
            layout(&json!({"mcp": {"a": {"type": "local", "command": ["x"]}}})),
            Layout::Legacy
        );
        assert_eq!(
            layout(&json!({"mcp": {"servers": {}, "a": {"type": "local"}}})),
            Layout::Servers
        );
    }
}
