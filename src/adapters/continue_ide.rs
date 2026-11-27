//! Continue (the open-source coding assistant for VS Code and JetBrains): MCP
//! servers, instructions and (in a project) rules.
//!
//! * MCP, user scope: `mcpServers` in `~/.continue/config.yaml` (Windows:
//!   `%USERPROFILE%\.continue\config.yaml`), a list of `- name: ...` entries.
//!   A local server takes `command`, `args`, `env` and `cwd`; a remote one
//!   `type: sse` or `type: streamable-http` with `url`, and headers under
//!   `requestOptions.headers` (the docs say a server's `requestOptions` have
//!   the format of a model's). Continue has no way to switch a server off in
//!   a file, so a server the profile disables is not written.
//! * MCP, project scope: standalone block files in `.continue/mcpServers/`
//!   (`name`, `version`, `schema: v1` and an `mcpServers` list). A server
//!   already defined in one of those YAML files is edited there, and new ones
//!   go to `kitbag.yaml` next to them. Servers that live in a JSON file
//!   there (the Claude Desktop / Cursor / Cline format Continue also reads)
//!   are captured, and left alone on apply.
//! * Secrets: Continue documents `${{ secrets.NAME }}` for `args` and `env`,
//!   which it fills from its own locally stored secrets, so `${VAR}`
//!   references are written that way and no value reaches the file. Its docs
//!   show no such syntax for `command`, `cwd`, `url` or headers, so a
//!   reference there is resolved from the environment (the file is then
//!   written 0600, and a project file is warned about), or kept with
//!   `--secrets never`.
//! * Instructions, user scope: a `rules` entry named `kitbag-instructions` in
//!   `config.yaml`, next to the plain-text rules already there, which are
//!   captured as instructions too. In a project: a managed block in
//!   `.continue/rules/kitbag-instructions.md`. Continue's docs don't mention
//!   `AGENTS.md`, so it is not used.
//! * Rules, project scope: `.continue/rules/<name>.md` with `name`,
//!   `description`, `globs` and `alwaysApply` frontmatter. Continue's docs
//!   don't describe a rules folder in `~/.continue`, so at user scope rules
//!   are not written.
//! * `config.json` (deprecated) is not read or written: Continue loads
//!   `config.yaml` instead of it when both exist, so kitbag never creates a
//!   `config.yaml` next to a `config.json` that has no `config.yaml`.
//!
//! Docs: <https://docs.continue.dev/customize/deep-dives/mcp>,
//! <https://docs.continue.dev/reference>,
//! <https://docs.continue.dev/customize/deep-dives/rules>,
//! <https://docs.continue.dev/customize/deep-dives/configuration>,
//! <https://docs.continue.dev/reference/yaml-migration>.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use anyhow::Result;
use serde_json::{Map, Value, json};

use super::common::{
    Resolver, add_instructions, add_rule, capture_instructions, import_server, merge_server_def,
    note_dropped_server_keys, read_text_item, record_server_write, remove_skipped_server,
    server_from_json, servers_for, skipped_servers, str_of, write_instructions, write_rule,
};
use super::{Adapter, ApplyOptions, Captured, Env, Feature, Scope, Support, Work};
use crate::edit::block;
use crate::edit::json::parse_value;
use crate::edit::yaml::{YamlDoc, parse_map};
use crate::frontmatter;
use crate::model::{McpServer, Profile, Rule, RuleMode, Transport, is_valid_name};
use crate::plan_fs::{join_rel, walk_files};
use crate::profile_io::rule_from_markdown;
use crate::secrets::{
    RefStyle, has_other_continue_template, normalize_continue_secrets, render_refs,
};

const ID: &str = "continue";
const SECTION: &str = "mcpServers";
const RULES: &str = "rules";
/// Name of the `rules` entry of `config.yaml` kitbag writes instructions to.
const INSTRUCTIONS_ENTRY: &str = "kitbag-instructions";
/// File of the project's `.continue/rules/` the instructions block goes to.
const INSTRUCTIONS_FILE: &str = "kitbag-instructions.md";
/// Block file kitbag creates in `.continue/mcpServers/` for servers that are
/// not defined in a file there yet.
const MCP_FILE: &str = "kitbag.yaml";
/// Header of a `config.yaml` kitbag creates (`name`, `version` and `schema`
/// are required).
const CONFIG_HEADER: &str = "name: Local Config\nversion: 1.0.0\nschema: v1\n";
/// Header of a block file kitbag creates.
const BLOCK_HEADER: &str = "name: kitbag\nversion: 0.0.1\nschema: v1\n";
/// Server keys kitbag writes; anything else (`connectionTimeout` ...) is kept.
const MANAGED_KEYS: &[&str] = &[
    "name",
    "type",
    "command",
    "args",
    "env",
    "cwd",
    "url",
    "requestOptions",
];
/// Keys of a server entry that are read when capturing.
const KNOWN_KEYS: &[&str] = &[
    "name",
    "type",
    "command",
    "args",
    "env",
    "cwd",
    "url",
    "requestOptions",
    "headers",
];
const MAX_FILES: usize = 500;

pub struct ContinueIde;

struct Paths {
    /// User scope: `config.yaml`.
    config: Option<PathBuf>,
    /// Project scope: `.continue/mcpServers`.
    mcp_dir: Option<PathBuf>,
    /// Project scope: `.continue/rules`.
    rules_dir: Option<PathBuf>,
    /// Project scope: the file instructions are written to.
    instructions: Option<PathBuf>,
}

fn continue_dir(env: &Env) -> PathBuf {
    match env.scope {
        Scope::User => env.home.join(".continue"),
        Scope::Project => env.project.join(".continue"),
    }
}

fn paths(env: &Env) -> Paths {
    let d = continue_dir(env);
    match env.scope {
        Scope::User => Paths {
            config: Some(d.join("config.yaml")),
            mcp_dir: None,
            rules_dir: None,
            instructions: None,
        },
        Scope::Project => Paths {
            config: None,
            mcp_dir: Some(d.join("mcpServers")),
            instructions: Some(d.join("rules").join(INSTRUCTIONS_FILE)),
            rules_dir: Some(d.join("rules")),
        },
    }
}

/// `name` as a kitbag name: Continue names are free text (`Browser search`).
fn kitbag_name(name: &str) -> String {
    if is_valid_name(name) {
        return name.to_string();
    }
    let mut out = String::new();
    for c in name.chars() {
        if c.is_ascii_alphanumeric() || c == '_' {
            out.push(c.to_ascii_lowercase());
        } else if (c == '.' || c == '-') && !out.is_empty() {
            out.push(c);
        } else if !out.ends_with('-') && !out.is_empty() {
            out.push('-');
        }
    }
    out.trim_end_matches(['-', '.']).to_string()
}

/// The entry of `items` that is server `name`: named so, or whose Continue
/// name becomes `name` when captured.
fn find_server<'a>(items: &'a [Value], name: &str) -> Option<&'a Map<String, Value>> {
    let named = |v: &&'a Value| v.get("name").and_then(Value::as_str);
    items
        .iter()
        .filter(|v| v.is_object())
        .find(|v| named(v) == Some(name))
        .or_else(|| {
            items
                .iter()
                .filter(|v| v.is_object())
                .find(|v| named(v).is_some_and(|n| kitbag_name(n) == name))
        })
        .and_then(Value::as_object)
}

/// Convert every string in `v` from Continue's `${{ secrets.X }}` to `${X}`.
fn normalize_value(v: &Value) -> Value {
    match v {
        Value::String(s) => Value::String(normalize_continue_secrets(s)),
        Value::Array(a) => Value::Array(a.iter().map(normalize_value).collect()),
        Value::Object(m) => Value::Object(
            m.iter()
                .map(|(k, v)| (k.clone(), normalize_value(v)))
                .collect(),
        ),
        other => other.clone(),
    }
}

/// Is this file in `.continue/mcpServers/` a YAML or a JSON one?
fn is_yaml(rel: &str) -> bool {
    rel.ends_with(".yaml") || rel.ends_with(".yml")
}

/// The other things Continue keeps under a project's `.continue/` or in
/// `config.yaml` that kitbag doesn't carry.
const OTHER_KEYS: &[&str] = &[
    "models",
    "context",
    "prompts",
    "docs",
    "data",
    "assistants",
    "blocks",
];

impl Adapter for ContinueIde {
    fn id(&self) -> &'static str {
        ID
    }
    fn name(&self) -> &'static str {
        "Continue"
    }
    fn docs(&self) -> &'static str {
        "https://docs.continue.dev/customize/deep-dives/mcp"
    }
    fn scopes(&self) -> &'static [Scope] {
        &[Scope::User, Scope::Project]
    }
    fn support(&self, scope: Scope) -> [(Feature, Support); 7] {
        Feature::ALL.map(|f| {
            let s = match f {
                Feature::Mcp | Feature::Instructions => Support::Native,
                Feature::Rules if scope == Scope::Project => Support::Native,
                _ => Support::None,
            };
            (f, s)
        })
    }

    fn locations(&self, env: &Env) -> Vec<(&'static str, PathBuf)> {
        let p = paths(env);
        let mut out = Vec::new();
        if let Some(c) = p.config {
            out.push(("mcp", c.clone()));
            out.push(("instructions", c));
        }
        if let Some(m) = p.mcp_dir {
            out.push(("mcp", m));
        }
        if let Some(i) = p.instructions {
            out.push(("instructions", i));
        }
        if let Some(r) = p.rules_dir {
            out.push(("rules", r));
        }
        out
    }

    fn detect(&self, env: &Env) -> bool {
        continue_dir(env).is_dir()
    }

    fn capture(&self, env: &Env) -> Result<Captured> {
        let mut cap = Captured::for_env(env);
        let p = paths(env);

        if let Some(config) = &p.config {
            capture_config(&mut cap, env, config)?;
        }
        if let Some(dir) = &p.mcp_dir {
            capture_mcp_dir(&mut cap, dir)?;
        }
        if let Some(dir) = &p.rules_dir {
            capture_rules_dir(&mut cap, dir)?;
        }
        if let Some(i) = &p.instructions {
            capture_instructions(&mut cap, i, ID)?;
            // Rules folded into the file by another tool come back out of it
            // as rules; Continue's rules are the files next to it.
        }
        if env.scope == Scope::Project {
            let d = continue_dir(env);
            let other: Vec<String> = std::fs::read_dir(&d)
                .into_iter()
                .flatten()
                .flatten()
                .filter(|e| e.path().is_dir())
                .map(|e| e.file_name().to_string_lossy().into_owned())
                .filter(|n| n != "mcpServers" && n != "rules")
                .collect();
            if !other.is_empty() {
                let mut other = other;
                other.sort();
                cap.info(
                    ID,
                    format!(
                        "Continue {} in .continue/ not captured: kitbag doesn't carry them yet",
                        other.join(", ")
                    ),
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
        if let Some(config) = &p.config {
            apply_user(profile, env, opts, work, config)?;
        } else {
            if let Some(dir) = &p.mcp_dir {
                apply_project_servers(profile, env, opts, work, dir)?;
            }
            if let (Some(file), Some(rules)) = (&p.instructions, &p.rules_dir) {
                write_instructions(work, file, profile, opts, false, ID)?;
                for r in &profile.rules {
                    if format!("{}.md", r.name) == INSTRUCTIONS_FILE {
                        work.warn(
                            ID,
                            format!(
                                "rule '{}' skipped: its file is where kitbag writes the instructions",
                                r.name
                            ),
                        );
                        continue;
                    }
                    let path = rules.join(format!("{}.md", r.name));
                    // Keep the display name a rule file already has.
                    let shown = work
                        .fs
                        .read(&path)?
                        .and_then(|t| frontmatter::string(frontmatter::parse(&t).data.get("name")));
                    write_rule(
                        work,
                        &path,
                        continue_rule(r, shown.as_deref()).as_bytes(),
                        opts,
                        ID,
                        &r.name,
                    )?;
                }
            }
        }
        if env.scope == Scope::User && !profile.rules.is_empty() {
            work.info(
                ID,
                "Continue's docs describe rule files only in a project's .continue/rules; apply with --project to write the profile's rules (instructions are written to config.yaml)",
            );
        }
        if profile.settings.contains_key(ID) {
            work.info(
                ID,
                "kitbag doesn't manage Continue settings; settings.continue was not applied",
            );
        }
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Capture

fn capture_config(cap: &mut Captured, env: &Env, config: &Path) -> Result<()> {
    let Some(text) = cap.read_config(config, ID)? else {
        let json = config.with_file_name("config.json");
        if json.is_file() {
            cap.info(
                ID,
                format!(
                    "{} is Continue's deprecated JSON config, which kitbag doesn't read; Continue loads config.yaml instead of it when there is one",
                    crate::ui::tildify(&json, &env.home)
                ),
            );
        }
        return Ok(());
    };
    cap.sources.push(config.to_path_buf());
    let doc = Value::Object(parse_map(Some(&text), config)?);
    capture_server_list(cap, config, doc.get(SECTION));

    let mut strings = 0;
    let mut other_rules = 0;
    let mut uses = 0;
    let mut pieces: Vec<String> = Vec::new();
    let mut ours: Option<String> = None;
    match doc.get(RULES) {
        None | Some(Value::Null) => {}
        Some(Value::Array(items)) => {
            for item in items {
                match item {
                    Value::String(s) => {
                        strings += 1;
                        pieces.push(s.clone());
                    }
                    Value::Object(m) if m.contains_key("uses") => uses += 1,
                    Value::Object(m)
                        if m.get("name").and_then(Value::as_str) == Some(INSTRUCTIONS_ENTRY) =>
                    {
                        ours = str_of(m.get("rule"));
                    }
                    Value::Object(_) => other_rules += 1,
                    _ => {}
                }
            }
        }
        Some(_) => cap.warn(
            ID,
            format!(
                "ignored 'rules' in {}: expected a list",
                crate::ui::tildify(config, &env.home)
            ),
        ),
    }
    if strings > 0 || ours.is_some() {
        for s in pieces.iter().chain(ours.iter()) {
            add_instructions(cap, ID, s, config);
        }
    }
    if other_rules > 0 {
        cap.info(
            ID,
            format!(
                "{other_rules} named rule(s) in config.yaml were not captured as rules: Continue's docs describe rule files in a project's .continue/rules, which capture --project reads"
            ),
        );
    }
    if uses > 0 {
        cap.info(
            ID,
            format!(
                "{uses} rule(s) in config.yaml that use a Hub block or a file (`uses:`) were not captured"
            ),
        );
    }
    let present: Vec<&str> = OTHER_KEYS
        .iter()
        .copied()
        .filter(|k| doc.get(*k).is_some_and(|v| !v.is_null()))
        .collect();
    if !present.is_empty() {
        cap.info(
            ID,
            format!(
                "Continue {} in config.yaml not captured: kitbag doesn't carry them yet",
                present.join(", ")
            ),
        );
    }
    Ok(())
}

/// Capture the servers of a list (`mcpServers` of a YAML file).
fn capture_server_list(cap: &mut Captured, file: &Path, list: Option<&Value>) {
    let items = match list {
        None | Some(Value::Null) => return,
        Some(Value::Array(a)) => a,
        Some(_) => {
            cap.warn(
                ID,
                format!(
                    "ignored '{SECTION}' in {}: expected a list of servers",
                    file.display()
                ),
            );
            return;
        }
    };
    for item in items {
        let Some(def) = item.as_object() else {
            cap.warn(
                ID,
                format!(
                    "skipped an entry of '{SECTION}' in {}: expected a mapping",
                    file.display()
                ),
            );
            continue;
        };
        let Some(name) = def.get("name").and_then(Value::as_str) else {
            if def.contains_key("uses") {
                cap.info(
                    ID,
                    format!(
                        "an MCP server of {} comes from a Hub block (`uses:`) and was not captured",
                        file.display()
                    ),
                );
            } else {
                cap.warn(
                    ID,
                    format!("skipped an MCP server of {} with no name", file.display()),
                );
            }
            continue;
        };
        capture_server(cap, name, def);
    }
}

fn capture_server(cap: &mut Captured, display: &str, def: &Map<String, Value>) {
    let name = kitbag_name(display);
    if name != display && is_valid_name(&name) {
        cap.info(
            ID,
            format!("MCP server '{display}' is captured as '{name}' (kitbag names have no spaces)"),
        );
    }
    let def = match normalize_value(&Value::Object(def.clone())) {
        Value::Object(m) => m,
        _ => return,
    };
    if has_other_continue_template(&Value::Object(def.clone()).to_string()) {
        cap.warn(
            ID,
            format!(
                "MCP server '{display}' uses a ${{{{ ... }}}} template other than `secrets.NAME`, which other tools can't expand; it was captured as written"
            ),
        );
    }
    let mut s = server_from_json(&def);
    let ro = def.get("requestOptions").and_then(Value::as_object);
    if let Some(h) = ro.and_then(|r| r.get("headers")) {
        s.headers.extend(super::common::string_map(Some(h)));
    }
    if let Some(ro) = ro {
        let extra: Vec<&str> = ro
            .keys()
            .map(String::as_str)
            .filter(|k| *k != "headers")
            .collect();
        if !extra.is_empty() {
            cap.info(
                ID,
                format!(
                    "MCP server '{display}': requestOptions not captured: {}",
                    extra.join(", ")
                ),
            );
        }
    }
    match str_of(def.get("type")).as_deref() {
        Some("sse") => s.transport = Some(Transport::Sse),
        Some("streamable-http") => s.transport = Some(Transport::Http),
        Some("stdio") | None => {}
        Some(other) => cap.warn(
            ID,
            format!("MCP server '{display}' has the unknown type '{other}'"),
        ),
    }
    let other: Vec<&str> = def
        .keys()
        .map(String::as_str)
        .filter(|k| !KNOWN_KEYS.contains(k))
        .collect();
    if !other.is_empty() {
        cap.info(
            ID,
            format!(
                "MCP server '{display}': Continue-specific keys not captured: {}",
                other.join(", ")
            ),
        );
    }
    import_server(cap, ID, &name, s, false);
}

fn capture_mcp_dir(cap: &mut Captured, dir: &Path) -> Result<()> {
    for rel in walk_files(dir, MAX_FILES)? {
        let yaml = is_yaml(&rel);
        if !yaml && !rel.ends_with(".json") {
            continue;
        }
        let path = join_rel(dir, &rel);
        let Some(text) = read_text_item(cap, &path, ID) else {
            continue;
        };
        cap.sources.push(path.clone());
        if yaml {
            let doc = Value::Object(parse_map(Some(&text), &path)?);
            capture_server_list(cap, &path, doc.get(SECTION));
        } else {
            let doc = parse_value(Some(&text), &path)?;
            for (name, def) in
                super::common::server_entries(cap, ID, &path, "mcpServers", doc.get("mcpServers"))
            {
                capture_server(cap, name, def);
            }
        }
    }
    Ok(())
}

fn capture_rules_dir(cap: &mut Captured, dir: &Path) -> Result<()> {
    let mut seen = BTreeMap::new();
    let mut yaml_rules = Vec::new();
    for rel in walk_files(dir, MAX_FILES)? {
        if is_yaml(&rel) {
            yaml_rules.push(rel);
            continue;
        }
        let Some(stem) = rel.strip_suffix(".md") else {
            continue;
        };
        if rel == INSTRUCTIONS_FILE {
            continue;
        }
        let path = join_rel(dir, &rel);
        let Some(text) = read_text_item(cap, &path, ID) else {
            continue;
        };
        let name = stem.replace('/', "-");
        if !is_valid_name(&name) {
            cap.warn(
                ID,
                format!("skipped {}: unsupported file name", path.display()),
            );
            continue;
        }
        if seen.insert(name.clone(), rel.clone()).is_some() {
            cap.warn(
                ID,
                format!(
                    "skipped {}: another rule file has the name '{name}'",
                    path.display()
                ),
            );
            continue;
        }
        cap.sources.push(path.clone());
        let doc = frontmatter::parse(&text);
        if doc.data.contains_key("regex") {
            cap.info(
                ID,
                format!("rule '{name}': `regex` is Continue-specific and was not captured"),
            );
        }
        cap.rule_files.push((name.clone(), path));
        add_rule(cap, rule_from_continue(&name, &text), ID);
    }
    if !yaml_rules.is_empty() {
        cap.info(
            ID,
            format!(
                "YAML rule file(s) in .continue/rules not captured ({}): kitbag reads Markdown rules",
                yaml_rules.join(", ")
            ),
        );
    }
    Ok(())
}

/// A Continue rule file as a profile rule. Continue's `alwaysApply` is read
/// as the profile does, with the values that only restate what globs mean dropped.
fn rule_from_continue(name: &str, text: &str) -> Rule {
    let mut r = rule_from_markdown(name, text);
    match (r.globs.is_empty(), r.always_apply) {
        (false, Some(false)) | (true, Some(true)) => r.always_apply = None,
        _ => {}
    }
    r
}

/// A profile rule as a Continue rule file.
fn continue_rule(r: &Rule, shown: Option<&str>) -> String {
    let always = match r.mode() {
        RuleMode::Always => Some(true),
        RuleMode::OnDemand => Some(false),
        RuleMode::Globs => None,
    };
    frontmatter::render(
        &[
            ("name", Value::String(shown.unwrap_or(&r.name).to_string())),
            (
                "description",
                r.description.clone().map_or(Value::Null, Value::String),
            ),
            ("globs", Value::from(r.globs.clone())),
            ("alwaysApply", always.map_or(Value::Null, Value::Bool)),
        ],
        &r.body,
    )
}

// ---------------------------------------------------------------------------
// Apply

/// A value of the entry being replaced that already holds `want` in
/// Continue's `${{ secrets.X }}` syntax: it is kept, so a field written by
/// hand that works is not turned into a placeholder or a resolved secret.
fn kept_value(cur: Option<&Value>, want: &str) -> Option<String> {
    let c = cur?.as_str()?;
    (c != want && normalize_continue_secrets(c) == want).then(|| c.to_string())
}

/// The `mcpServers` entry for `s`. `cur` is the entry it replaces, whose
/// display name and unmanaged `requestOptions` are kept.
fn entry_for(
    name: &str,
    s: &McpServer,
    cur: Option<&Map<String, Value>>,
    env: &Env,
    opts: &ApplyOptions,
    work: &mut Work,
    file: &str,
) -> Map<String, Value> {
    let mut def = Map::new();
    let display = cur
        .and_then(|c| c.get("name"))
        .and_then(Value::as_str)
        .unwrap_or(name);
    def.insert("name".into(), json!(display));
    let before = work.render.resolved.len();
    let mut res = Resolver::new(env, opts);
    // `${VAR}` in `args` and `env` becomes Continue's `${{ secrets.VAR }}`.
    let mut secrets: Vec<String> = Vec::new();
    let mut secret = |v: &str, work: &mut Work| -> String {
        for r in crate::secrets::refs_in(v) {
            if !secrets.contains(&r) {
                secrets.push(r);
            }
        }
        render_refs(v, RefStyle::ContinueSecret, &env.vars, &mut work.render)
    };
    match s.transport() {
        Transport::Stdio => {
            if cur
                .and_then(|c| c.get("type"))
                .and_then(Value::as_str)
                .is_some_and(|t| t == "stdio")
            {
                def.insert("type".into(), json!("stdio"));
            }
            let command = s.command.as_deref().unwrap_or_default();
            def.insert(
                "command".into(),
                json!(
                    kept_value(cur.and_then(|c| c.get("command")), command)
                        .unwrap_or_else(|| res.value(command, "command", work))
                ),
            );
            if !s.args.is_empty() {
                let args: Vec<String> = s.args.iter().map(|a| secret(a, work)).collect();
                def.insert("args".into(), json!(args));
            }
            if !s.env.is_empty() {
                let e: Map<String, Value> = s
                    .env
                    .iter()
                    .map(|(k, v)| (k.clone(), json!(secret(v, work))))
                    .collect();
                def.insert("env".into(), Value::Object(e));
            }
            if let Some(cwd) = &s.cwd {
                def.insert(
                    "cwd".into(),
                    json!(
                        kept_value(cur.and_then(|c| c.get("cwd")), cwd)
                            .unwrap_or_else(|| res.value(cwd, "cwd", work))
                    ),
                );
            }
        }
        t => {
            let ty = if t == Transport::Sse {
                "sse"
            } else {
                "streamable-http"
            };
            def.insert("type".into(), json!(ty));
            let url = s.url.as_deref().unwrap_or_default();
            def.insert(
                "url".into(),
                json!(
                    kept_value(cur.and_then(|c| c.get("url")), url)
                        .unwrap_or_else(|| res.value(url, "url", work))
                ),
            );
            let cur_headers = cur
                .and_then(|c| c.get("requestOptions"))
                .and_then(|r| r.get("headers"))
                .and_then(Value::as_object);
            let mut ro = cur
                .and_then(|c| c.get("requestOptions"))
                .and_then(Value::as_object)
                .cloned()
                .unwrap_or_default();
            ro.remove("headers");
            if !s.headers.is_empty() {
                let h: Map<String, Value> = s
                    .headers
                    .iter()
                    .map(|(k, v)| {
                        let kept = kept_value(cur_headers.and_then(|h| h.get(k)), v);
                        let v = kept.unwrap_or_else(|| res.value(v, &format!("headers.{k}"), work));
                        (k.clone(), json!(v))
                    })
                    .collect();
                ro.insert("headers".into(), Value::Object(h));
            }
            if !ro.is_empty() {
                def.insert("requestOptions".into(), Value::Object(ro));
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
    res.finish(work, ID, "Continue", name, file);
    if !secrets.is_empty() {
        work.info(
            ID,
            format!(
                "MCP server '{name}' uses ${{{{ secrets.{} }}}}; Continue fills these from its own locally stored secrets, which kitbag never reads or writes, so set them in Continue",
                secrets.join(" }}, ${{ secrets.")
            ),
        );
    }
    if env.scope == Scope::Project && work.render.resolved.len() > before {
        work.warn(
            ID,
            format!(
                "MCP server '{name}': a secret was written to {file} in the project, which is usually committed; keep it out of version control, or use --secrets never"
            ),
        );
    }
    def
}

/// Write `servers` into `mcpServers` of `doc` (`path`), and remove the
/// `skipped` ones kitbag put there. Leaves other entries and keys alone.
fn merge_servers(
    work: &mut Work,
    doc: &mut YamlDoc,
    path: &Path,
    servers: &[(&String, &McpServer)],
    skipped: &[String],
    env: &Env,
    opts: &ApplyOptions,
) -> Result<()> {
    let file = crate::ui::tildify(path, &work.home);
    let items = doc.list(SECTION);
    for name in skipped {
        let cur = find_server(&items, name).map(|m| Value::Object(m.clone()));
        if remove_skipped_server(work, opts, ID, path, name, cur.as_ref())
            && let Some(display) = cur
                .as_ref()
                .and_then(|c| c.get("name"))
                .and_then(Value::as_str)
        {
            doc.remove_item(SECTION, "name", display)?;
        }
    }
    for (name, s) in servers {
        let cur = find_server(&items, name);
        let want = entry_for(name, s, cur, env, opts, work, &file);
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
        let display = def
            .get("name")
            .and_then(Value::as_str)
            .unwrap_or(name)
            .to_string();
        doc.set_item(SECTION, "name", &display, &def)?;
        work.changed_servers.push((ID.to_string(), (*name).clone()));
        record_server_write(work, ID, path, name, Some(&def));
    }
    Ok(())
}

/// Open `path` for editing: a missing or empty file starts from `header`.
fn open_doc(work: &mut Work, path: &Path, header: &str) -> Result<YamlDoc> {
    let text = work.fs.read(path)?;
    let text = match text {
        Some(t) if !t.trim().is_empty() => t,
        _ => header.to_string(),
    };
    YamlDoc::parse(Some(&text), path)
}

fn apply_user(
    profile: &Profile,
    env: &Env,
    opts: &ApplyOptions,
    work: &mut Work,
    config: &Path,
) -> Result<()> {
    let servers = servers_for(profile, ID, work, false);
    let skipped = skipped_servers(work, ID);
    let has_instructions = profile
        .instructions
        .as_deref()
        .is_some_and(|i| !i.trim().is_empty());
    let file = crate::ui::tildify(config, &work.home);
    let existing = work.fs.read(config)?;
    if servers.is_empty()
        && skipped.is_empty()
        && !has_instructions
        && existing
            .as_deref()
            .is_none_or(|t| !t.contains(INSTRUCTIONS_ENTRY))
    {
        return Ok(());
    }
    let json = config.with_file_name("config.json");
    if existing.as_deref().is_none_or(|t| t.trim().is_empty()) && json.is_file() {
        work.warn(
            ID,
            format!(
                "{file} was not created: Continue loads config.yaml instead of its deprecated config.json when there is one, which would leave out your config.json (models and all); migrate to config.yaml first (https://docs.continue.dev/reference/yaml-migration), then apply again"
            ),
        );
        return Ok(());
    }
    let created = existing.as_deref().is_none_or(|t| t.trim().is_empty());
    let mut doc = open_doc(work, config, CONFIG_HEADER)?;
    let need_servers = !servers.is_empty() || !skipped.is_empty();
    let need_rules = has_instructions
        || existing
            .as_deref()
            .is_some_and(|t| t.contains(INSTRUCTIONS_ENTRY));
    for (section, needed, what) in [
        (SECTION, need_servers, "MCP servers"),
        (RULES, need_rules, "instructions"),
    ] {
        if !needed {
            continue;
        }
        if let Err(why) = doc.check_editable_list(section) {
            work.warn(
                ID,
                format!(
                    "nothing written to {file}: {why}, which kitbag can't edit in place; add the {what} by hand"
                ),
            );
            return Ok(());
        }
    }
    merge_servers(work, &mut doc, config, &servers, &skipped, env, opts)?;
    apply_user_instructions(profile, opts, work, &mut doc, &file)?;
    if doc.changed() {
        if created {
            work.info(
                ID,
                format!(
                    "{file} is created with the name, version and schema Continue requires; it holds no models, so add some in Continue"
                ),
            );
        }
        work.fs.write(config, doc.render()?)?;
    }
    Ok(())
}

/// The `kitbag-instructions` entry of `rules` in `config.yaml`.
fn apply_user_instructions(
    profile: &Profile,
    opts: &ApplyOptions,
    work: &mut Work,
    doc: &mut YamlDoc,
    file: &str,
) -> Result<()> {
    let items = doc.list(RULES);
    let ours = items
        .iter()
        .find(|v| v.get("name").and_then(Value::as_str) == Some(INSTRUCTIONS_ENTRY));
    // Plain-text rules already in the file are captured as instructions, so
    // they are not written a second time.
    let outside = items
        .iter()
        .filter_map(Value::as_str)
        .collect::<Vec<_>>()
        .join("\n\n");
    let want = profile
        .instructions
        .as_deref()
        .map(|i| block::without_paragraphs_in(i, &outside))
        .filter(|c| !c.trim().is_empty());
    match (want, ours) {
        (None, None) => {}
        (None, Some(_)) => {
            if !opts.overwrite {
                work.warn(
                    ID,
                    format!(
                        "kept the '{INSTRUCTIONS_ENTRY}' rule in {file} because --no-overwrite was given"
                    ),
                );
            } else {
                doc.remove_item(RULES, "name", INSTRUCTIONS_ENTRY)?;
            }
        }
        (Some(text), cur) => {
            let def = json!({
                "name": INSTRUCTIONS_ENTRY,
                "rule": text,
                "alwaysApply": true,
            });
            let same = cur.is_some_and(|c| {
                c.get("rule").and_then(Value::as_str).map(str::trim) == Some(text.trim())
                    && c.get("alwaysApply") != Some(&Value::Bool(false))
            });
            if same {
                return Ok(());
            }
            if cur.is_some() && !opts.overwrite {
                work.warn(
                    ID,
                    format!(
                        "kept the existing '{INSTRUCTIONS_ENTRY}' rule in {file} because --no-overwrite was given"
                    ),
                );
                return Ok(());
            }
            if crate::secrets::has_redaction_marker(&text) {
                work.warn(
                    ID,
                    format!(
                        "the instructions hold a [REDACTED] marker from capture; written to {file} as they are"
                    ),
                );
            }
            doc.set_item(RULES, "name", INSTRUCTIONS_ENTRY, &def)?;
        }
    }
    Ok(())
}

/// What goes into one block file: servers to write, names to remove.
type FileEdit<'p> = (Vec<(&'p String, &'p McpServer)>, Vec<String>);

/// Project scope: servers go to the block file that defines them, or to
/// `.continue/mcpServers/kitbag.yaml`.
fn apply_project_servers(
    profile: &Profile,
    env: &Env,
    opts: &ApplyOptions,
    work: &mut Work,
    dir: &Path,
) -> Result<()> {
    let servers = servers_for(profile, ID, work, false);
    let skipped = skipped_servers(work, ID);
    if servers.is_empty() && skipped.is_empty() {
        return Ok(());
    }
    let default = dir.join(MCP_FILE);
    // Which file defines which server now.
    let mut yaml_files: Vec<PathBuf> = Vec::new();
    let mut json_defs: BTreeMap<String, (PathBuf, Value)> = BTreeMap::new();
    for rel in walk_files(dir, MAX_FILES).unwrap_or_default() {
        let path = join_rel(dir, &rel);
        if is_yaml(&rel) {
            yaml_files.push(path);
        } else if rel.ends_with(".json")
            && let Ok(Some(text)) = work.fs.read(&path)
            && let Ok(doc) = parse_value(Some(&text), &path)
            && let Some(Value::Object(m)) = doc.get("mcpServers")
        {
            for (name, def) in m {
                json_defs
                    .entry(name.clone())
                    .or_insert_with(|| (path.clone(), def.clone()));
            }
        }
    }
    let mut items: BTreeMap<PathBuf, Vec<Value>> = BTreeMap::new();
    for path in &yaml_files {
        if let Ok(Some(text)) = work.fs.read(path)
            && let Ok(map) = parse_map(Some(&text), path)
            && let Some(Value::Array(a)) = map.get(SECTION)
        {
            items.insert(path.clone(), a.clone());
        }
    }
    let holder = |name: &str| -> Option<PathBuf> {
        items
            .iter()
            .find(|(_, v)| find_server(v, name).is_some())
            .map(|(p, _)| p.clone())
    };
    let mut put: BTreeMap<PathBuf, FileEdit<'_>> = BTreeMap::new();
    for (name, s) in servers {
        if let Some((j, def)) = json_defs
            .get(name.as_str())
            .filter(|_| holder(name).is_none())
        {
            let runs = |d: &Value| {
                let m = d.as_object().cloned().unwrap_or_default();
                let s = server_from_json(&m);
                (s.command, s.args, s.url)
            };
            let wanted = (s.command.clone(), s.args.clone(), s.url.clone());
            if runs(def) == wanted {
                work.info(
                    ID,
                    format!(
                        "MCP server '{name}' is already defined in {}; left as it is",
                        crate::ui::tildify(j, &work.home)
                    ),
                );
                continue;
            }
            work.warn(
                ID,
                format!(
                    "MCP server '{name}' is defined in {}, a JSON file kitbag leaves alone; it was not written. Remove it there to have kitbag manage it, or edit it by hand",
                    crate::ui::tildify(j, &work.home)
                ),
            );
            continue;
        }
        let at = holder(name).unwrap_or_else(|| default.clone());
        put.entry(at).or_default().0.push((name, s));
    }
    for name in skipped {
        if let Some(at) = holder(&name) {
            put.entry(at).or_default().1.push(name);
        }
    }
    for (path, (servers, skipped)) in put {
        let mut doc = open_doc(work, &path, BLOCK_HEADER)?;
        if let Err(why) = doc.check_editable_list(SECTION) {
            work.warn(
                ID,
                format!(
                    "MCP servers not written to {}: {why}, which kitbag can't edit in place; add them by hand",
                    crate::ui::tildify(&path, &work.home)
                ),
            );
            continue;
        }
        merge_servers(work, &mut doc, &path, &servers, &skipped, env, opts)?;
        if doc.changed() {
            work.fs.write(&path, doc.render()?)?;
        }
    }
    Ok(())
}
