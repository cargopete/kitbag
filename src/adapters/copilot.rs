//! GitHub Copilot (Copilot CLI, and the Copilot agent in VS Code): MCP servers,
//! instructions and path-specific instructions (rules).
//!
//! * MCP: Copilot's native files use a top-level `mcpServers` object:
//!   `$COPILOT_HOME/mcp-config.json` (default `~/.copilot/mcp-config.json`) for
//!   the user, and `.mcp.json` at the workspace root for a project. Copilot CLI
//!   reads both; VS Code reads the user file and, with the experimental
//!   `chat.mcp.workspaceRootConfig.enabled` setting, the workspace `.mcp.json`.
//!   Local servers take `type` (`stdio`; Copilot CLI's examples use `local`),
//!   `command`, `args`, `env` and `cwd`;
//!   remote servers `type` (`http` or `sse`), `url` and `headers`; `tools`
//!   selects tools (`["*"]` for all). References are written as `${VAR}` and
//!   `${VAR:-default}`, which Copilot expands itself, so no secret reaches disk.
//!   The project `.mcp.json` is the same file Claude Code reads, so kitbag writes
//!   it with Claude Code's server shape (no `tools`, no `cwd`), and neither
//!   target removes a server there that the other still gets.
//!   Capture also reads `.github/mcp.json` (Copilot CLI), the bare top-level
//!   format Copilot CLI accepts in project files, and VS Code's deprecated
//!   editor-managed `.vscode/mcp.json` (`servers`, `${env:VAR}`), which Copilot
//!   CLI doesn't read; servers in `disabledMcpServers` (`~/.copilot/settings.json`
//!   or `.github/copilot/settings.json`) are captured as disabled.
//! * Instructions: `$COPILOT_HOME/copilot-instructions.md` and
//!   `.github/copilot-instructions.md`, in a managed block.
//! * Rules: `*.instructions.md` files with `applyTo` globs, in
//!   `$COPILOT_HOME/instructions/` and `.github/instructions/`. A rule that is
//!   always on gets `applyTo: "**"`; an on-demand rule gets no `applyTo`.
//! * Prompt files, custom agents, chat modes and skills are not supported yet.
//!
//! Docs: <https://code.visualstudio.com/docs/agents/reference/mcp-configuration>,
//! <https://docs.github.com/en/copilot/how-tos/copilot-cli/customize-copilot/add-mcp-servers>,
//! <https://docs.github.com/en/copilot/how-tos/copilot-cli/customize-copilot/add-custom-instructions>,
//! <https://docs.github.com/en/copilot/reference/copilot-cli-reference/cli-config-dir-reference>.

use std::path::{Path, PathBuf};

use anyhow::Result;
use serde_json::{Map, Value, json};

use super::claude_code;
use super::common::{
    add_rule, capture_instructions, claim_name, import_server, keep_shared_servers,
    merge_json_servers, name_from_rel, read_text_item, render_server, same_server, server_entries,
    server_from_json, servers_for, string_list, write_instructions, write_rule,
};
use super::{Adapter, ApplyOptions, Captured, Env, Feature, Scope, Support, Work};
use crate::edit::json::parse_value;
use crate::frontmatter::{self, opt};
use crate::model::{Profile, Rule, RuleMode, Transport, is_valid_name};
use crate::plan_fs::{join_rel, walk_files};
use crate::profile_io::rule_from_markdown;
use crate::secrets::{RefStyle, has_ref};

const ID: &str = "copilot";
/// Server keys kitbag writes in the user file. `tools` is not one of them: a
/// tool list you chose is kept, and `["*"]` is only added when there is none.
const MANAGED_KEYS: &[&str] = &["type", "command", "args", "env", "cwd", "url", "headers"];
const RULE_EXT: &str = ".instructions.md";

pub struct Copilot;

struct Paths {
    mcp: PathBuf,
    instructions: PathBuf,
    rules: PathBuf,
    /// Copilot CLI settings, for `disabledMcpServers`.
    settings: PathBuf,
}

fn copilot_home(env: &Env) -> PathBuf {
    env.location_var("COPILOT_HOME")
        .unwrap_or_else(|| env.home.join(".copilot"))
}

fn paths(env: &Env) -> Paths {
    match env.scope {
        Scope::User => {
            let d = copilot_home(env);
            Paths {
                mcp: d.join("mcp-config.json"),
                instructions: d.join("copilot-instructions.md"),
                rules: d.join("instructions"),
                settings: d.join("settings.json"),
            }
        }
        Scope::Project => {
            let g = env.project.join(".github");
            Paths {
                mcp: env.project.join(".mcp.json"),
                instructions: g.join("copilot-instructions.md"),
                rules: g.join("instructions"),
                settings: g.join("copilot").join("settings.json"),
            }
        }
    }
}

/// A rule as a Copilot `.instructions.md` file. `applyTo` takes one string of
/// comma-separated globs; `"**"` matches every file.
pub fn copilot_rule(r: &Rule) -> String {
    let apply_to = match r.mode() {
        RuleMode::Globs => Value::String(r.globs.join(",")),
        RuleMode::Always => Value::String("**".into()),
        RuleMode::OnDemand => Value::Null,
    };
    frontmatter::render(
        &[("description", opt(&r.description)), ("applyTo", apply_to)],
        &r.body,
    )
}

/// A Copilot `.instructions.md` file -> profile rule. `applyTo: "**"` is
/// always on; a file without `applyTo` is only used when attached, which is
/// an on-demand rule.
pub fn rule_from_instructions(name: &str, text: &str) -> Rule {
    let mut r = rule_from_markdown(name, text);
    if !r.globs.is_empty() && r.globs.iter().all(|g| g == "**" || g == "**/*") {
        r.globs.clear();
        r.always_apply = None;
    } else if r.globs.is_empty() && r.always_apply.is_none() {
        r.always_apply = Some(false);
    }
    r
}

/// The servers of a project MCP file: its `mcpServers` object, or, in Copilot
/// CLI's bare format, the top level itself.
fn project_servers(doc: &Value) -> Option<&Value> {
    if let Some(s) = doc.get("mcpServers") {
        return Some(s);
    }
    is_bare(doc).then_some(doc)
}

/// Is `doc` a non-empty object whose every value looks like a server entry?
fn is_bare(doc: &Value) -> bool {
    let Value::Object(m) = doc else { return false };
    !m.is_empty()
        && m.values().all(|v| {
            v.as_object()
                .is_some_and(|d| d.contains_key("command") || d.contains_key("url"))
        })
}

/// Import one server entry from a Copilot MCP file.
fn import(
    cap: &mut Captured,
    name: &str,
    def: &Map<String, Value>,
    file: &Path,
    bare_dollar: bool,
) {
    let mut s = server_from_json(def);
    match def.get("type").and_then(Value::as_str) {
        None | Some("local" | "stdio" | "http" | "streamable-http") => {}
        Some("sse") => s.transport = Some(Transport::Sse),
        Some(other) => {
            cap.warn(
                ID,
                format!(
                    "skipped MCP server '{name}' in {}: unknown type '{other}'",
                    file.display()
                ),
            );
            return;
        }
    }
    let tools = string_list(def.get("tools"));
    let mut left: Vec<&str> = Vec::new();
    if def.contains_key("tools") && tools != ["*"] {
        left.push("tools");
    }
    left.extend(
        [
            "oauthClientId",
            "oauthScopes",
            "oauthPublicClient",
            "oauthGrantType",
            "oidc",
        ]
        .into_iter()
        .filter(|k| def.contains_key(*k)),
    );
    if !left.is_empty() {
        cap.info(
            ID,
            format!(
                "MCP server '{name}': {} not captured (Copilot-specific)",
                left.join(", ")
            ),
        );
    }
    import_server(cap, ID, name, s, bare_dollar);
}

/// Import one server from VS Code's older editor-managed `.vscode/mcp.json`.
fn import_vscode(cap: &mut Captured, name: &str, def: &Map<String, Value>, file: &Path) {
    let blob = Value::Object(def.clone()).to_string();
    if blob.contains("${input:") || blob.contains("${command:") {
        cap.warn(
            ID,
            format!(
                "skipped MCP server '{name}' in {}: it uses VS Code ${{input:...}} or ${{command:...}} variables, which only VS Code can fill in (Copilot doesn't take such servers either); use env references instead",
                file.display()
            ),
        );
        return;
    }
    if def.contains_key("envFile") {
        cap.warn(
            ID,
            format!("MCP server '{name}': envFile is VS Code-specific and was not captured; list the variables in 'env' instead"),
        );
    }
    if blob.contains("${workspaceFolder") || blob.contains("${userHome}") {
        cap.warn(
            ID,
            format!("MCP server '{name}' uses VS Code variables (${{workspaceFolder}}, ${{userHome}}); other tools won't expand them"),
        );
    }
    // VS Code treats a bare `$NAME` as literal text.
    import(cap, name, def, file, false);
}

/// Copilot features kitbag doesn't carry yet, so the user knows what a
/// captured profile leaves behind.
fn note_not_carried(cap: &mut Captured, env: &Env) {
    let dirs: Vec<(PathBuf, &str)> = match env.scope {
        Scope::User => {
            let d = copilot_home(env);
            vec![
                (d.join("agents"), "custom agents"),
                (d.join("skills"), "skills"),
            ]
        }
        Scope::Project => {
            let g = env.project.join(".github");
            vec![
                (g.join("prompts"), "prompt files"),
                (g.join("agents"), "custom agents"),
                (g.join("chatmodes"), "chat modes"),
                (g.join("skills"), "skills"),
            ]
        }
    };
    let found: Vec<String> = dirs
        .iter()
        .filter_map(|(dir, what)| {
            let n = walk_files(dir, 10_000).map_or(0, |f| f.len());
            (n > 0).then(|| format!("{what} ({})", crate::ui::tildify(dir, &env.home)))
        })
        .collect();
    if !found.is_empty() {
        cap.info(
            ID,
            format!(
                "not carried by kitbag for Copilot yet: {}",
                found.join(", ")
            ),
        );
    }
}

/// Mark the captured servers that Copilot CLI's `disabledMcpServers` lists.
fn capture_disabled(cap: &mut Captured, path: &Path) -> Result<()> {
    let Some(text) = cap.read_config(path, ID)? else {
        return Ok(());
    };
    let doc = parse_value(Some(&text), path)?;
    let names = string_list(doc.get("disabledMcpServers"));
    if names.is_empty() {
        return Ok(());
    }
    cap.sources.push(path.to_path_buf());
    for n in names {
        if let Some(s) = cap.profile.mcp_servers.get_mut(&n) {
            s.disabled = true;
        }
    }
    Ok(())
}

impl Adapter for Copilot {
    fn id(&self) -> &'static str {
        ID
    }
    fn name(&self) -> &'static str {
        "GitHub Copilot"
    }
    fn docs(&self) -> &'static str {
        "https://code.visualstudio.com/docs/agents/reference/mcp-configuration"
    }
    fn scopes(&self) -> &'static [Scope] {
        &[Scope::User, Scope::Project]
    }
    fn support(&self, _scope: Scope) -> [(Feature, Support); 6] {
        Feature::ALL.map(|f| {
            (
                f,
                if matches!(f, Feature::Mcp | Feature::Instructions | Feature::Rules) {
                    Support::Native
                } else {
                    Support::None
                },
            )
        })
    }

    fn locations(&self, env: &Env) -> Vec<(&'static str, PathBuf)> {
        let p = paths(env);
        vec![
            ("mcp", p.mcp),
            ("instructions", p.instructions),
            ("rules", p.rules),
        ]
    }

    fn detect(&self, env: &Env) -> bool {
        // `.mcp.json` alone belongs to Claude Code.
        match env.scope {
            Scope::User => copilot_home(env).exists(),
            Scope::Project => {
                let g = env.project.join(".github");
                g.join("copilot-instructions.md").exists()
                    || g.join("instructions").exists()
                    || g.join("mcp.json").exists()
                    || g.join("copilot").exists()
                    || env.project.join(".vscode").join("mcp.json").exists()
            }
        }
    }

    fn capture(&self, env: &Env) -> Result<Captured> {
        let mut cap = Captured::for_env(env);
        let p = paths(env);

        match env.scope {
            Scope::User => {
                if let Some(text) = cap.read_config(&p.mcp, ID)? {
                    cap.sources.push(p.mcp.clone());
                    let doc = parse_value(Some(&text), &p.mcp)?;
                    for (name, def) in
                        server_entries(&mut cap, ID, &p.mcp, "mcpServers", doc.get("mcpServers"))
                    {
                        // The docs only promise `${VAR}` here (VS Code keeps a
                        // bare `$VAR` as text), but a bare `$VAR` is still taken
                        // as a reference so its value is never captured.
                        import(&mut cap, name, def, &p.mcp, true);
                    }
                }
            }
            Scope::Project => {
                // Closest first: `.mcp.json` wins over `.github/mcp.json`, and
                // both over the older VS Code file.
                let github = env.project.join(".github").join("mcp.json");
                for file in [&p.mcp, &github] {
                    if let Some(text) = cap.read_config(file, ID)? {
                        cap.sources.push(file.clone());
                        let doc = parse_value(Some(&text), file)?;
                        for (name, def) in
                            server_entries(&mut cap, ID, file, "mcpServers", project_servers(&doc))
                        {
                            import(&mut cap, name, def, file, false);
                        }
                    }
                }
                let vscode = env.project.join(".vscode").join("mcp.json");
                if let Some(text) = cap.read_config(&vscode, ID)? {
                    cap.sources.push(vscode.clone());
                    let doc = parse_value(Some(&text), &vscode)?;
                    let entries =
                        server_entries(&mut cap, ID, &vscode, "servers", doc.get("servers"));
                    if !entries.is_empty() {
                        cap.info(
                            ID,
                            "read .vscode/mcp.json, VS Code's older editor-managed format; kitbag writes Copilot servers to .mcp.json",
                        );
                    }
                    for (name, def) in entries {
                        import_vscode(&mut cap, name, def, &vscode);
                    }
                }
            }
        }
        capture_disabled(&mut cap, &p.settings)?;

        capture_instructions(&mut cap, &p.instructions, ID)?;

        if !cap.escapes(&p.rules, ID) {
            let mut seen = std::collections::BTreeMap::new();
            for rel in walk_files(&p.rules, 500)? {
                if !rel.ends_with(RULE_EXT) {
                    continue;
                }
                let path = join_rel(&p.rules, &rel);
                let Some(text) = read_text_item(&mut cap, &path, ID) else {
                    continue;
                };
                let name = name_from_rel(&rel, RULE_EXT);
                claim_name(&mut seen, &p.rules, &rel, &name)?;
                if !is_valid_name(&name) {
                    cap.warn(
                        ID,
                        format!("skipped {}: unsupported file name", path.display()),
                    );
                    continue;
                }
                cap.sources.push(path);
                add_rule(&mut cap, rule_from_instructions(&name, &text), ID);
            }
        }

        note_not_carried(&mut cap, env);
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
        match env.scope {
            Scope::User => apply_user_servers(profile, env, opts, work, &p.mcp)?,
            Scope::Project => apply_project_servers(profile, env, opts, work, &p.mcp)?,
        }

        write_instructions(work, &p.instructions, profile, opts, false, ID)?;

        for r in &profile.rules {
            if r.mode() == RuleMode::Globs && r.globs.iter().any(|g| g.contains(',')) {
                work.warn(
                    ID,
                    format!(
                        "rule '{}': Copilot splits applyTo at commas, so a glob with a comma in it ({}) won't match as written",
                        r.name,
                        r.globs.join(" ")
                    ),
                );
            }
            if r.mode() == RuleMode::OnDemand {
                work.info(
                    ID,
                    format!(
                        "rule '{}' is on-demand in the profile; it is written without applyTo, so Copilot only uses it when it is attached",
                        r.name
                    ),
                );
            }
            let path = p.rules.join(format!("{}{RULE_EXT}", r.name));
            write_rule(work, &path, copilot_rule(r).as_bytes(), opts, ID, &r.name)?;
        }

        if profile.settings.contains_key(ID) {
            work.info(
                ID,
                "kitbag doesn't manage Copilot settings; settings.copilot was not applied",
            );
        }
        Ok(())
    }
}

/// Warn about references where VS Code's Copilot docs don't list them.
fn note_unlisted_refs(work: &mut Work, name: &str, fields: &[(&str, Option<&str>)]) {
    let found: Vec<&str> = fields
        .iter()
        .filter(|(_, v)| v.is_some_and(has_ref))
        .map(|(k, _)| *k)
        .collect();
    if !found.is_empty() {
        work.warn(
            ID,
            format!(
                "MCP server '{name}': env reference in {}; Copilot CLI expands it, but VS Code's Copilot docs only list command, args, env and headers",
                found.join(", ")
            ),
        );
    }
}

/// `$COPILOT_HOME/mcp-config.json`: Copilot's own shape, with `cwd` and `tools`.
fn apply_user_servers(
    profile: &Profile,
    env: &Env,
    opts: &ApplyOptions,
    work: &mut Work,
    path: &Path,
) -> Result<()> {
    let existing: Map<String, Value> = match work.fs.read(path)? {
        Some(text) => parse_value(Some(&text), path)?
            .get("mcpServers")
            .and_then(Value::as_object)
            .cloned()
            .unwrap_or_default(),
        None => Map::new(),
    };
    let mut servers = Vec::new();
    for (name, s) in servers_for(profile, ID, work, false) {
        note_unlisted_refs(
            work,
            name,
            &[("url", s.url.as_deref()), ("cwd", s.cwd.as_deref())],
        );
        let s = render_server(s, RefStyle::Dollar, env, work);
        let mut def = Map::new();
        match s.transport() {
            Transport::Stdio => {
                def.insert("type".into(), json!("stdio"));
                def.insert("command".into(), json!(s.command));
                def.insert("args".into(), json!(s.args));
                if !s.env.is_empty() {
                    def.insert("env".into(), json!(s.env));
                }
                if let Some(cwd) = &s.cwd {
                    def.insert("cwd".into(), json!(cwd));
                }
            }
            t => {
                def.insert("type".into(), json!(t.as_str()));
                def.insert("url".into(), json!(s.url));
                if !s.headers.is_empty() {
                    def.insert("headers".into(), json!(s.headers));
                }
            }
        }
        // Copilot CLI requires `tools`. Keep the list of an entry for the same
        // server; anything else gets all tools.
        let keeps_tools = existing
            .get(name.as_str())
            .and_then(Value::as_object)
            .is_some_and(|cur| cur.contains_key("tools") && same_server(cur, &def));
        if !keeps_tools {
            def.insert("tools".into(), json!(["*"]));
        }
        servers.push((name.clone(), Value::Object(def)));
    }
    merge_json_servers(
        work,
        path,
        &["mcpServers"],
        &servers,
        MANAGED_KEYS,
        opts,
        ID,
    )
}

/// The project `.mcp.json`, shared with Claude Code: written exactly as Claude
/// Code writes it, so applying both targets plans each entry once and neither
/// changes what the other wrote.
fn apply_project_servers(
    profile: &Profile,
    env: &Env,
    opts: &ApplyOptions,
    work: &mut Work,
    path: &Path,
) -> Result<()> {
    if let Some(text) = work.fs.read(path)? {
        let doc = parse_value(Some(&text), path)?;
        if doc.get("mcpServers").is_none() && is_bare(&doc) {
            if !profile.mcp_servers.is_empty() {
                work.warn(
                    ID,
                    format!(
                        "{} lists its servers at the top level, without mcpServers; kitbag left it alone. Move them under \"mcpServers\" (Claude Code reads only that) and apply again",
                        crate::ui::tildify(path, &work.home)
                    ),
                );
            }
            return Ok(());
        }
    }
    let mut servers = Vec::new();
    for (name, s) in servers_for(profile, ID, work, false) {
        if s.cwd.is_some() {
            work.warn(
                ID,
                format!(
                    "MCP server '{name}': 'cwd' was dropped from .mcp.json (Claude Code reads the same file and has no cwd, and VS Code's workspace form has none); set it in your user config instead"
                ),
            );
        }
        note_unlisted_refs(work, name, &[("url", s.url.as_deref())]);
        if !s.applies_to(claude_code::ID) {
            work.warn(
                ID,
                format!(
                    "MCP server '{name}' is kept off claude-code in the profile, but Claude Code reads .mcp.json too, so it will see it"
                ),
            );
        }
        let s = render_server(s, RefStyle::Dollar, env, work);
        servers.push((name.clone(), claude_code::server_json(&s)));
    }
    keep_shared_servers(
        work,
        profile,
        opts,
        ID,
        claude_code::ID,
        "Claude Code",
        path,
    );
    merge_json_servers(
        work,
        path,
        &["mcpServers"],
        &servers,
        claude_code::MANAGED_KEYS,
        opts,
        ID,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rule(globs: &[&str], always: Option<bool>, description: Option<&str>) -> Rule {
        Rule {
            name: "r".into(),
            description: description.map(str::to_string),
            globs: globs.iter().map(|g| g.to_string()).collect(),
            always_apply: always,
            body: "Body.".into(),
        }
    }

    #[test]
    fn rule_format_round_trips_every_mode() {
        let globs = rule(&["src/**/*.ts", "test/**"], None, Some("TypeScript"));
        assert_eq!(
            copilot_rule(&globs),
            "---\ndescription: TypeScript\napplyTo: \"src/**/*.ts,test/**\"\n---\n\nBody.\n"
        );
        let always = rule(&[], None, None);
        assert_eq!(
            copilot_rule(&always),
            "---\napplyTo: \"**\"\n---\n\nBody.\n"
        );
        let on_demand = rule(&[], Some(false), Some("Use for SQL"));
        assert_eq!(
            copilot_rule(&on_demand),
            "---\ndescription: Use for SQL\n---\n\nBody.\n"
        );
        for r in [globs, always, on_demand] {
            assert_eq!(rule_from_instructions("r", &copilot_rule(&r)), r);
        }
    }

    #[test]
    fn hand_written_instruction_files_are_read() {
        let r = rule_from_instructions("r", "---\napplyTo: '**/*.py'\n---\nUse hints.\n");
        assert_eq!(r.globs, vec!["**/*.py"]);
        assert_eq!(r.mode(), RuleMode::Globs);
        let r = rule_from_instructions("r", "---\napplyTo: \"**/*\"\n---\nAll.\n");
        assert_eq!(r.mode(), RuleMode::Always);
        // No frontmatter at all: only used when attached.
        let r = rule_from_instructions("r", "Just text.\n");
        assert_eq!(r.mode(), RuleMode::OnDemand);
    }

    #[test]
    fn bare_project_files_are_recognised() {
        assert!(is_bare(&json!({"pw": {"command": "npx"}})));
        assert!(is_bare(
            &json!({"ctx": {"type": "http", "url": "https://x.example"}})
        ));
        assert!(!is_bare(&json!({})));
        assert!(!is_bare(&json!({"mcpServers": {}})));
        assert!(!is_bare(&json!({"pw": {"command": "npx"}, "inputs": []})));
    }
}
