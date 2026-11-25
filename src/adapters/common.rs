//! Building blocks shared by adapters.

use std::collections::BTreeMap;
use std::path::Path;

use anyhow::Result;
use serde_json::{Map, Value};

use super::{ApplyOptions, Captured, Env, SecretsMode, Work};
use crate::edit::block;
use crate::edit::json::JsonDoc;
use crate::frontmatter;
use crate::model::{Command, McpServer, Profile, Rule, Skill, SkillFile, Transport, is_valid_name};
use crate::plan_fs::{decode_text, join_rel, read_opt, subdirs, walk_confined, walk_files};
use crate::policy;
use crate::profile_io::{
    MAX_FILE_BYTES, MAX_SKILL_FILES, command_from_markdown, max_file_size, rule_from_markdown,
    skill_description,
};
use crate::secrets::{
    RefStyle, has_redaction_marker, has_ref, is_redacted_form_of, normalize_refs, redact_server,
    redact_text, render_refs,
};

/// Suffix of the managed block holding folded rules.
pub const RULES_BLOCK_SUFFIX: &str = ":rules";

// ---------------------------------------------------------------------------
// Applying

/// Write a whole file kitbag owns (one rule / command / skill file).
pub fn write_owned(
    work: &mut Work,
    path: &Path,
    content: &[u8],
    opts: &ApplyOptions,
    target: &str,
    label: &str,
) -> Result<()> {
    let cur = work.fs.read_bytes(path)?;
    if cur.as_deref() == Some(content) || cur.as_deref().is_some_and(|c| same_markdown(c, content))
    {
        return Ok(());
    }
    if cur.is_some() && !opts.overwrite {
        work.warn(
            target,
            format!(
                "kept existing {label} ({}) because --no-overwrite was given",
                path.display()
            ),
        );
        return Ok(());
    }
    if !check_redacted(work, path, cur.as_deref(), content, target) {
        return Ok(());
    }
    work.fs.write(path, content.to_vec())
}

/// Markdown files that differ only in frontmatter formatting (quoting, key
/// order) or surrounding whitespace are left alone, so re-applying a profile
/// to the tool it was captured from doesn't churn files.
fn same_markdown(a: &[u8], b: &[u8]) -> bool {
    markdown_matches(a, b, |x, y| x == y)
}

/// `cur` (on disk) and `want` (from the profile) compared with `eq` on the
/// trimmed text, or on frontmatter data plus trimmed body.
fn markdown_matches(cur: &[u8], want: &[u8], eq: impl Fn(&str, &str) -> bool) -> bool {
    let (Ok(a), Ok(b)) = (decode_text(cur.to_vec()), std::str::from_utf8(want)) else {
        return false;
    };
    let a = a.as_str();
    if eq(b.trim(), a.trim()) {
        return true;
    }
    if !a.starts_with("---") && !b.starts_with("---") {
        return false;
    }
    let (da, db) = (frontmatter::parse(a), frontmatter::parse(b));
    let clean = |m: &Map<String, Value>| -> Map<String, Value> {
        m.iter()
            .filter(|(_, v)| !v.is_null())
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect()
    };
    clean(&da.data) == clean(&db.data) && eq(db.body.trim(), da.body.trim())
}

/// Before writing profile content that holds redaction markers over `cur`:
/// if the profile copy is just the redacted form of what is on disk (a profile
/// applied back to the tool it was captured from), keep the file and say so;
/// otherwise warn that the markers will be written literally. Returns whether
/// to go ahead with the write.
fn check_redacted(
    work: &mut Work,
    path: &Path,
    cur: Option<&[u8]>,
    want: &[u8],
    target: &str,
) -> bool {
    if !std::str::from_utf8(want).is_ok_and(has_redaction_marker) {
        return true;
    }
    match cur {
        Some(c) if markdown_matches(c, want, is_redacted_form_of) => {
            work.info(
                target,
                format!(
                    "kept {}: the profile holds a redacted copy of it ([REDACTED] where this file has the real values)",
                    path.display()
                ),
            );
            false
        }
        Some(_) => {
            work.warn(
                target,
                format!(
                    "{} will be replaced with profile content that contains [REDACTED] markers where capture removed secrets; restore the real values after applying (or keep your file with --no-overwrite)",
                    path.display()
                ),
            );
            true
        }
        None => {
            work.warn(
                target,
                format!(
                    "{} will contain [REDACTED] markers where capture removed secrets; fill in the real values after applying",
                    path.display()
                ),
            );
            true
        }
    }
}

/// SKILL.md with the `name` / `description` frontmatter some tools require.
pub fn skill_with_required_frontmatter(skill: &Skill) -> Skill {
    let mut s = skill.clone();
    for f in &mut s.files {
        if f.path != "SKILL.md" {
            continue;
        }
        let text = String::from_utf8_lossy(&f.content).into_owned();
        let doc = frontmatter::parse(&text);
        if doc.data.contains_key("name") && doc.data.contains_key("description") {
            continue;
        }
        let mut data = doc.data.clone();
        data.entry("name")
            .or_insert_with(|| Value::String(skill.name.clone()));
        data.entry("description").or_insert_with(|| {
            Value::String(
                skill
                    .description
                    .clone()
                    .unwrap_or_else(|| format!("{} skill", skill.name)),
            )
        });
        // name and description first, everything else after.
        let mut entries: Vec<(&str, Value)> = vec![
            ("name", data["name"].clone()),
            ("description", data["description"].clone()),
        ];
        entries.extend(
            data.iter()
                .filter(|(k, _)| *k != "name" && *k != "description")
                .map(|(k, v)| (k.as_str(), v.clone())),
        );
        f.content = frontmatter::render(&entries, &doc.body).into_bytes();
    }
    s
}

pub fn write_skill(
    work: &mut Work,
    dir: &Path,
    skill: &Skill,
    opts: &ApplyOptions,
    target: &str,
) -> Result<()> {
    let main = dir.join("SKILL.md");
    let want = skill.main_file().map(|f| f.content.as_slice());
    let cur = work.fs.read_bytes(&main)?;
    let differs = match (cur.as_deref(), want) {
        (Some(c), Some(w)) => c != w && !markdown_matches(c, w, is_redacted_form_of),
        (c, w) => c != w,
    };
    if cur.is_some() && differs && !opts.overwrite {
        work.warn(
            target,
            format!(
                "kept existing skill '{}' ({}) because --no-overwrite was given",
                skill.name,
                dir.display()
            ),
        );
        return Ok(());
    }
    for f in &skill.files {
        let path = join_rel(dir, &f.path);
        let cur = work.fs.read_bytes(&path)?;
        if cur.as_deref() != Some(f.content.as_slice())
            && check_redacted(work, &path, cur.as_deref(), &f.content, target)
        {
            work.fs.write(&path, f.content.clone())?;
        }
    }
    Ok(())
}

/// Insert, replace or (with `None`) remove a managed block in a shared file.
pub fn upsert_block_file(
    work: &mut Work,
    path: &Path,
    id: &str,
    content: Option<&str>,
    target: &str,
) -> Result<()> {
    let cur = work.fs.read(path)?;
    if content.is_none() && cur.is_none() {
        return Ok(());
    }
    if let (Some(text), Some(c)) = (cur.as_deref(), content)
        && block::holds_original_of(text, id, c)
    {
        work.info(
            target,
            format!(
                "kept kitbag block '{id}' in {}: the profile holds a redacted copy of it",
                path.display()
            ),
        );
        return Ok(());
    }
    let next = block::upsert(cur.as_deref(), id, content);
    if Some(next.as_str()) != cur.as_deref() {
        if let Some(c) = content.filter(|c| has_redaction_marker(c)) {
            let _ = check_redacted(work, path, None, c.as_bytes(), target);
        }
        work.fs.write(path, next)?;
    }
    Ok(())
}

/// Write instructions (and optionally folded rules) into a shared markdown file.
pub fn write_instructions(
    work: &mut Work,
    path: &Path,
    profile: &Profile,
    opts: &ApplyOptions,
    fold_rules: bool,
    target: &str,
) -> Result<()> {
    let outside = work
        .fs
        .read(path)?
        .map(|t| block::split(&t).outside)
        .unwrap_or_default();
    let content = profile
        .instructions
        .as_deref()
        .map(|i| block::without_paragraphs_in(i, &outside))
        .filter(|c| !c.trim().is_empty());
    upsert_block_file(work, path, &opts.block_id, content.as_deref(), target)?;
    if fold_rules {
        let folded = block::fold_rules(&profile.rules);
        let id = format!("{}{RULES_BLOCK_SUFFIX}", opts.block_id);
        upsert_block_file(work, path, &id, folded.as_deref(), target)?;
    }
    Ok(())
}

/// Copy of `s` with every string rendered in `style`.
pub fn render_server(s: &McpServer, style: RefStyle, env: &Env, work: &mut Work) -> McpServer {
    s.map_strings(|v| render_refs(v, style, &env.vars, &mut work.render))
}

/// Resolves `${VAR}` references for tools that cannot expand them, and reports precisely.
pub struct Resolver<'a> {
    env: &'a Env,
    mode: SecretsMode,
    resolved: Vec<String>,
    missing: Vec<String>,
    kept: Vec<String>,
}

impl<'a> Resolver<'a> {
    pub fn new(env: &'a Env, opts: &ApplyOptions) -> Self {
        Resolver {
            env,
            mode: opts.secrets,
            resolved: Vec::new(),
            missing: Vec::new(),
            kept: Vec::new(),
        }
    }

    pub fn value(&mut self, v: &str, location: &str, work: &mut Work) -> String {
        if !has_ref(v) {
            return v.to_string();
        }
        if self.mode == SecretsMode::Never {
            self.kept.push(location.to_string());
            return v.to_string();
        }
        let mut probe = crate::secrets::RenderState::default();
        let out = render_refs(v, RefStyle::Resolve, &self.env.vars, &mut probe);
        work.render.resolved.extend(probe.resolved);
        work.render.missing.extend(probe.missing.iter().cloned());
        if probe.missing.is_empty() {
            self.resolved.push(location.to_string());
        } else {
            self.missing.push(location.to_string());
        }
        out
    }

    pub fn finish(self, work: &mut Work, target: &str, tool: &str, server: &str, file: &str) {
        if !self.resolved.is_empty() {
            work.warn(
                target,
                format!(
                    "MCP server '{server}': {} resolved from your environment and written to {file} ({tool} can't expand env references there)",
                    self.resolved.join(", ")
                ),
            );
        }
        if !self.missing.is_empty() {
            work.warn(
                target,
                format!(
                    "MCP server '{server}': {} reference unset env vars; written as placeholders. Set them and re-apply",
                    self.missing.join(", ")
                ),
            );
        }
        if !self.kept.is_empty() {
            work.warn(
                target,
                format!(
                    "MCP server '{server}': {} left as ${{VAR}} placeholders (--secrets never); {tool} won't expand them",
                    self.kept.join(", ")
                ),
            );
        }
    }
}

/// Combine an existing server definition with the one kitbag wants: keys kitbag
/// manages come from `want`; anything else the user set (timeouts, trust flags,
/// tool filters...) is preserved. Returns `None` when nothing would change.
pub fn merge_server_def(
    existing: Option<&Map<String, Value>>,
    want: &Map<String, Value>,
    managed: &[&str],
) -> Option<Map<String, Value>> {
    let Some(cur) = existing else {
        return Some(want.clone());
    };
    let mut out: Map<String, Value> = cur
        .iter()
        .filter(|(k, _)| !managed.contains(&k.as_str()))
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect();
    for (k, v) in want {
        out.insert(k.clone(), v.clone());
    }
    let same = out.len() == cur.len() && out.iter().all(|(k, v)| cur.get(k) == Some(v));
    (!same).then_some(out)
}

/// Merge server definitions into the object at `key_path` of a JSON file,
/// leaving every other key, and other servers, untouched.
pub fn merge_json_servers(
    work: &mut Work,
    path: &Path,
    key_path: &[&str],
    servers: &[(String, Value)],
    managed: &[&str],
    opts: &ApplyOptions,
    target: &str,
) -> Result<()> {
    if servers.is_empty() {
        return Ok(());
    }
    let text = work.fs.read(path)?;
    let mut doc = JsonDoc::parse(text.as_deref(), path)?;
    let existing = doc.object(key_path);
    let mut changed = false;
    for (name, def) in servers {
        let Value::Object(want) = def else { continue };
        let cur = existing.get(name).and_then(Value::as_object);
        let Some(merged) = merge_server_def(cur, want, managed) else {
            continue;
        };
        if cur.is_some() && !opts.overwrite {
            work.warn(
                target,
                format!(
                    "kept existing MCP server '{name}' in {} because --no-overwrite was given",
                    path.display()
                ),
            );
            continue;
        }
        let mut full: Vec<&str> = key_path.to_vec();
        full.push(name);
        doc.set(&full, &Value::Object(merged));
        changed = true;
    }
    if changed {
        work.fs.write(path, doc.render())?;
    }
    Ok(())
}

/// Deep merge: objects merge, arrays union, scalars from `b` win.
pub fn merge_settings(a: &Value, b: &Value) -> Value {
    match (a, b) {
        (Value::Array(x), Value::Array(y)) => {
            let mut out = x.clone();
            for v in y {
                if !out.contains(v) {
                    out.push(v.clone());
                }
            }
            Value::Array(out)
        }
        (Value::Object(x), Value::Object(y)) => {
            let mut out = x.clone();
            for (k, v) in y {
                let merged = match x.get(k) {
                    Some(cur) => merge_settings(cur, v),
                    None => v.clone(),
                };
                out.insert(k.clone(), merged);
            }
            Value::Object(out)
        }
        (_, b) => b.clone(),
    }
}

/// Like [`merge_settings`], but existing scalar values win at every depth.
/// Sets `kept` when a differing value was kept.
pub fn merge_settings_keep(a: &Value, b: &Value, kept: &mut bool) -> Value {
    match (a, b) {
        (Value::Object(x), Value::Object(y)) => {
            let mut out = x.clone();
            for (k, v) in y {
                let merged = match x.get(k) {
                    Some(cur) => merge_settings_keep(cur, v, kept),
                    None => v.clone(),
                };
                out.insert(k.clone(), merged);
            }
            Value::Object(out)
        }
        (Value::Array(_), Value::Array(_)) => merge_settings(a, b),
        (a, b) => {
            if a != b {
                *kept = true;
            }
            a.clone()
        }
    }
}

/// Merge allow-listed settings into the top level of a JSON settings file.
pub fn merge_json_settings(
    work: &mut Work,
    path: &Path,
    settings: &Map<String, Value>,
    opts: &ApplyOptions,
    target: &str,
) -> Result<()> {
    if settings.is_empty() {
        return Ok(());
    }
    let text = work.fs.read(path)?;
    let mut doc = JsonDoc::parse(text.as_deref(), path)?;
    let current = doc.value();
    let mut changed = false;
    for (k, v) in settings {
        let cur = current.get(k);
        let next = match cur {
            None => v.clone(),
            Some(c) if opts.overwrite => merge_settings(c, v),
            Some(c) => {
                let mut kept = false;
                let merged = merge_settings_keep(c, v, &mut kept);
                if kept {
                    work.warn(
                        target,
                        format!("kept existing value(s) of setting '{k}' in {} because --no-overwrite was given", path.display()),
                    );
                }
                merged
            }
        };
        if cur == Some(&next) {
            continue;
        }
        doc.set(&[k], &next);
        changed = true;
    }
    if changed {
        work.fs.write(path, doc.render())?;
    }
    Ok(())
}

/// The profile's settings for `target`, restricted to the allow-list (dropped keys are noted).
pub fn safe_settings(profile: &Profile, target: &str, work: &mut Work) -> Map<String, Value> {
    let (allowed, dropped) = policy::filter(target, profile.settings.get(target));
    if !dropped.is_empty() {
        work.warn(
            target,
            format!(
                "settings not applied (not on kitbag's allow-list of safe keys): {}",
                dropped.join(", ")
            ),
        );
    }
    allowed
}

/// Servers from the profile that apply to `target`, noting disabled ones the tool can't represent.
pub fn servers_for<'p>(
    profile: &'p Profile,
    target: &str,
    work: &mut Work,
    disabled_supported: bool,
) -> Vec<(&'p String, &'p McpServer)> {
    let mut out = Vec::new();
    for (name, s) in &profile.mcp_servers {
        if !s.applies_to(target) {
            continue;
        }
        if s.disabled && !disabled_supported {
            work.info(
                target,
                format!("MCP server '{name}' is disabled in the profile; not written"),
            );
            continue;
        }
        out.push((name, s));
    }
    out
}

// ---------------------------------------------------------------------------
// Capturing

pub fn str_of(v: Option<&Value>) -> Option<String> {
    match v {
        Some(Value::String(s)) if !s.is_empty() => Some(s.clone()),
        _ => None,
    }
}

fn scalar_string(v: &Value) -> Option<String> {
    match v {
        Value::String(s) => Some(s.clone()),
        Value::Number(n) => Some(n.to_string()),
        Value::Bool(b) => Some(b.to_string()),
        _ => None,
    }
}

pub fn string_list(v: Option<&Value>) -> Vec<String> {
    match v {
        Some(Value::Array(a)) => a.iter().filter_map(scalar_string).collect(),
        _ => Vec::new(),
    }
}

pub fn string_map(v: Option<&Value>) -> BTreeMap<String, String> {
    match v {
        Some(Value::Object(m)) => m
            .iter()
            .filter_map(|(k, v)| scalar_string(v).map(|s| (k.clone(), s)))
            .collect(),
        _ => BTreeMap::new(),
    }
}

/// The common `{command, args, env, cwd, url, headers}` shape used by JSON configs.
pub fn server_from_json(def: &Map<String, Value>) -> McpServer {
    McpServer {
        command: str_of(def.get("command")),
        args: string_list(def.get("args")),
        env: string_map(def.get("env")),
        cwd: str_of(def.get("cwd")),
        url: str_of(def.get("url")),
        headers: string_map(def.get("headers")),
        ..Default::default()
    }
}

/// The server definitions under `key` in a config file, warning about anything
/// malformed (`"mcpServers": []`, `"a": "not an object"`) instead of ignoring it.
pub fn server_entries<'v>(
    cap: &mut Captured,
    target: &str,
    file: &Path,
    key: &str,
    servers: Option<&'v Value>,
) -> Vec<(&'v String, &'v Map<String, Value>)> {
    let map = match servers {
        None | Some(Value::Null) => return Vec::new(),
        Some(Value::Object(m)) => m,
        Some(other) => {
            cap.warn(
                target,
                format!(
                    "ignored '{key}' in {}: expected an object of servers, found {}",
                    file.display(),
                    json_kind(other)
                ),
            );
            return Vec::new();
        }
    };
    let mut out = Vec::new();
    for (name, def) in map {
        match def {
            Value::Object(d) => out.push((name, d)),
            other => cap.warn(
                target,
                format!(
                    "skipped MCP server '{name}' in {}: expected an object, found {}",
                    file.display(),
                    json_kind(other)
                ),
            ),
        }
    }
    out
}

fn json_kind(v: &Value) -> &'static str {
    match v {
        Value::Null => "null",
        Value::Bool(_) => "a boolean",
        Value::Number(_) => "a number",
        Value::String(_) => "a string",
        Value::Array(_) => "an array",
        Value::Object(_) => "an object",
    }
}

/// Normalise one captured server (tool syntax -> canonical), redact secrets, and add it.
pub fn import_server(
    cap: &mut Captured,
    target: &str,
    name: &str,
    server: McpServer,
    bare_dollar: bool,
) {
    if !is_valid_name(name) {
        cap.warn(
            target,
            format!("skipped MCP server '{name}': unsupported name"),
        );
        return;
    }
    let mut s = server.map_strings(|v| normalize_refs(v, bare_dollar));
    if s.command.is_none() && s.url.is_none() {
        cap.warn(
            target,
            format!("skipped MCP server '{name}': no command or url"),
        );
        return;
    }
    // An explicit transport is only needed when inference would get it wrong.
    if s.transport == Some(Transport::Http)
        || (s.transport == Some(Transport::Stdio) && s.url.is_none())
    {
        s.transport = None;
    }
    let (redacted, mut redactions) = redact_server(name, &s);
    if cap.profile.mcp_servers.contains_key(name) {
        return;
    }
    let originals: BTreeMap<String, String> = s
        .strings()
        .into_iter()
        .map(|(loc, v)| (format!("mcp_servers.{name}.{loc}"), v.to_string()))
        .collect();
    for r in &mut redactions {
        r.fingerprint = originals
            .get(&r.location)
            .map(|v| crate::txn::sha256(Some(v.as_bytes())).unwrap_or_default());
    }
    let s = redacted;
    cap.redactions.extend(redactions);
    cap.profile.mcp_servers.insert(name.to_string(), s);
}

fn append_instructions(cap: &mut Captured, text: &str) {
    let text = text.trim();
    if text.is_empty() {
        return;
    }
    cap.profile.instructions = Some(match cap.profile.instructions.take() {
        Some(cur) if cur.contains(text) => cur,
        Some(cur) => format!("{cur}\n\n{text}"),
        None => text.to_string(),
    });
}

/// Add free text to the captured instructions after redaction.
pub fn add_instructions(cap: &mut Captured, target: &str, text: &str, from: &Path) {
    let (red, kinds) = redact_text(text);
    if !kinds.is_empty() {
        cap.warn(
            target,
            format!("redacted {} from {}", kinds.join(", "), from.display()),
        );
    }
    append_instructions(cap, &red);
}

/// Read one text item (instructions, rule, command) for capture: UTF-8, or
/// UTF-16 with a byte-order mark. Files that are too large, can't be read or
/// can't be decoded are skipped with a warning instead of failing the capture.
pub fn read_text_item(cap: &mut Captured, path: &Path, target: &str) -> Option<String> {
    let skip = |cap: &mut Captured, why: String| {
        cap.warn(target, format!("skipped {}: {why}", path.display()));
        None
    };
    match std::fs::metadata(path) {
        Ok(m) if m.len() > MAX_FILE_BYTES => {
            return skip(
                cap,
                format!(
                    "{} KiB is over the {} limit for profile files",
                    m.len() / 1024,
                    max_file_size()
                ),
            );
        }
        _ => {}
    }
    match read_opt(path) {
        Ok(None) => None,
        Ok(Some(bytes)) => match decode_text(bytes) {
            Ok(text) => Some(text),
            Err(_) => skip(
                cap,
                "not valid UTF-8 (or UTF-16 with a byte-order mark)".into(),
            ),
        },
        Err(e) => skip(cap, format!("{e:#}")),
    }
}

/// Read an instructions file: user content plus kitbag-managed blocks, with folded rules recovered.
pub fn capture_instructions(cap: &mut Captured, path: &Path, target: &str) -> Result<()> {
    let Some(text) = read_text_item(cap, path, target) else {
        return Ok(());
    };
    if text.trim().is_empty() {
        return Ok(());
    }
    cap.sources.push(path.to_path_buf());
    let split = block::split(&text);
    let mut pieces = Vec::new();
    if !split.outside.is_empty() {
        pieces.push(split.outside);
    }
    for (_, inner) in split.blocks {
        let (rest, rules) = block::unfold_rules(&inner);
        if !rest.is_empty() {
            pieces.push(rest);
        }
        for r in rules {
            add_rule(cap, r, target);
        }
    }
    let combined = pieces.join("\n\n");
    add_instructions(cap, target, &combined, path);
    Ok(())
}

pub fn add_rule(cap: &mut Captured, mut r: Rule, target: &str) {
    let (body, kinds) = redact_text(&r.body);
    if !kinds.is_empty() {
        cap.warn(
            target,
            format!("redacted {} from rule '{}'", kinds.join(", "), r.name),
        );
    }
    r.body = body;
    if !cap.profile.rules.iter().any(|x| x.name == r.name) {
        cap.profile.rules.push(r);
    }
}

pub fn add_command(cap: &mut Captured, mut c: Command, target: &str) {
    let (body, kinds) = redact_text(&c.body);
    if !kinds.is_empty() {
        cap.warn(
            target,
            format!("redacted {} from command '{}'", kinds.join(", "), c.name),
        );
    }
    c.body = body;
    if !cap.profile.commands.iter().any(|x| x.name == c.name) {
        cap.profile.commands.push(c);
    }
}

/// `frontend/component.md` -> `frontend-component`.
pub fn name_from_rel(rel: &str, ext: &str) -> String {
    rel.strip_suffix(ext)
        .unwrap_or(rel)
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '.' || c == '_' || c == '-' {
                c
            } else {
                '-'
            }
        })
        .collect()
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum MdKind {
    Rule,
    Command,
}

/// Capture `<dir>/**/*.<ext>` markdown files as rules or commands.
pub fn capture_markdown_dir(
    cap: &mut Captured,
    dir: &Path,
    ext: &str,
    kind: MdKind,
    target: &str,
) -> Result<()> {
    for rel in walk_files(dir, 500)? {
        if !rel.ends_with(ext) {
            continue;
        }
        let path = join_rel(dir, &rel);
        let Some(text) = read_text_item(cap, &path, target) else {
            continue;
        };
        let name = name_from_rel(&rel, ext);
        if !is_valid_name(&name) {
            cap.warn(
                target,
                format!("skipped {}: unsupported file name", path.display()),
            );
            continue;
        }
        cap.sources.push(path);
        match kind {
            MdKind::Rule => add_rule(cap, rule_from_markdown(&name, &text), target),
            MdKind::Command => add_command(cap, command_from_markdown(&name, &text), target),
        }
    }
    Ok(())
}

/// Capture every `<dir>/<name>/SKILL.md` skill with its supporting files.
pub fn capture_skills_dir(cap: &mut Captured, dir: &Path, target: &str) -> Result<()> {
    for name in subdirs(dir)? {
        if !is_valid_name(&name) || cap.profile.skills.iter().any(|s| s.name == name) {
            continue;
        }
        let skill_dir = dir.join(&name);
        let walk = match walk_confined(&skill_dir, MAX_SKILL_FILES) {
            Ok(w) => w,
            Err(e) => {
                cap.warn(target, format!("skipped skill '{name}': {e:#}"));
                continue;
            }
        };
        if !walk.files.iter().any(|r| r == "SKILL.md") {
            continue;
        }
        for why in &walk.skipped {
            cap.warn(target, format!("skipped skill file '{name}/{why}'"));
        }
        if walk.truncated {
            cap.warn(
                target,
                format!("skill '{name}' has more than {MAX_SKILL_FILES} files; only the first {MAX_SKILL_FILES} were captured"),
            );
        }
        let mut files = Vec::new();
        let mut broken = false;
        for rel in walk.files {
            let path = join_rel(&skill_dir, &rel);
            let skip = |cap: &mut Captured, why: String| {
                cap.warn(target, format!("skipped skill file '{name}/{rel}': {why}"));
            };
            match std::fs::metadata(&path) {
                Ok(m) if m.len() > MAX_FILE_BYTES => {
                    skip(
                        cap,
                        format!(
                            "{} KiB is over the {} limit for profile files",
                            m.len() / 1024,
                            max_file_size()
                        ),
                    );
                    broken |= rel == "SKILL.md";
                    continue;
                }
                _ => {}
            }
            let mut bytes = match read_opt(&path) {
                Ok(Some(b)) => b,
                Ok(None) => continue,
                Err(e) => {
                    skip(cap, format!("{e:#}"));
                    broken |= rel == "SKILL.md";
                    continue;
                }
            };
            if rel == "SKILL.md" {
                match decode_text(bytes) {
                    Ok(text) => bytes = text.into_bytes(),
                    Err(_) => {
                        skip(
                            cap,
                            "not valid UTF-8 (or UTF-16 with a byte-order mark)".into(),
                        );
                        broken = true;
                        continue;
                    }
                }
            }
            if let Some(why) = crate::secrets::sensitive_file(&rel, &bytes) {
                cap.warn(target, format!("skipped skill file '{name}/{rel}': {why}; keep it out of the profile and provide it locally"));
                continue;
            }
            let content = match String::from_utf8(bytes) {
                Ok(text) => {
                    let (red, kinds) = redact_text(&text);
                    if !kinds.is_empty() {
                        cap.warn(
                            target,
                            format!("redacted {} from skill '{name}/{rel}'", kinds.join(", ")),
                        );
                    }
                    red.into_bytes()
                }
                Err(e) => e.into_bytes(),
            };
            files.push(SkillFile { path: rel, content });
        }
        if broken {
            cap.warn(
                target,
                format!("skipped skill '{name}': its SKILL.md couldn't be captured"),
            );
            continue;
        }
        cap.sources.push(skill_dir);
        cap.profile.skills.push(Skill {
            description: skill_description(&files),
            name,
            files,
        });
    }
    Ok(())
}

/// Allow-listed keys of a settings object, with notes about notable skipped keys.
pub fn pick_settings(cap: &mut Captured, target: &str, doc: &Map<String, Value>) {
    let keys = policy::allowed_keys(target);
    let picked: Map<String, Value> = doc
        .iter()
        .filter(|(k, v)| keys.contains(&k.as_str()) && !v.is_null())
        .map(|(k, v)| (k.clone(), strip_nulls(v)))
        .collect();
    if !picked.is_empty() {
        cap.profile.settings.insert(target.to_string(), picked);
    }
}

fn strip_nulls(v: &Value) -> Value {
    match v {
        Value::Object(m) => Value::Object(
            m.iter()
                .filter(|(_, v)| !v.is_null())
                .map(|(k, v)| (k.clone(), strip_nulls(v)))
                .collect(),
        ),
        Value::Array(a) => {
            Value::Array(a.iter().filter(|v| !v.is_null()).map(strip_nulls).collect())
        }
        other => other.clone(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn settings_merge() {
        let a = json!({"permissions": {"allow": ["A"], "deny": ["X"]}, "model": "a"});
        let b = json!({"permissions": {"allow": ["A", "B"]}, "model": "b"});
        assert_eq!(
            merge_settings(&a, &b),
            json!({"permissions": {"allow": ["A", "B"], "deny": ["X"]}, "model": "b"})
        );
    }

    #[test]
    fn required_skill_frontmatter() {
        let s = Skill {
            name: "x".into(),
            description: None,
            files: vec![SkillFile {
                path: "SKILL.md".into(),
                content: b"---\nlicense: MIT\n---\nBody\n".to_vec(),
            }],
        };
        let out = skill_with_required_frontmatter(&s);
        let text = String::from_utf8(out.files[0].content.clone()).unwrap();
        assert_eq!(
            text,
            "---\nname: x\ndescription: x skill\nlicense: MIT\n---\n\nBody\n"
        );
    }

    #[test]
    fn rel_names() {
        assert_eq!(
            name_from_rel("frontend/component.md", ".md"),
            "frontend-component"
        );
        assert_eq!(name_from_rel("a b.mdc", ".mdc"), "a-b");
    }
}
