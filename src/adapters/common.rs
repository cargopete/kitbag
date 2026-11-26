//! Building blocks shared by adapters.

use std::collections::BTreeMap;
use std::path::Path;

use anyhow::{Result, bail};
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
/// Returns whether the plan writes it.
pub fn write_owned(
    work: &mut Work,
    path: &Path,
    content: &[u8],
    opts: &ApplyOptions,
    target: &str,
    label: &str,
) -> Result<bool> {
    let cur = work.fs.read_bytes(path)?;
    if cur.as_deref() == Some(content) || cur.as_deref().is_some_and(|c| same_markdown(c, content))
    {
        return Ok(false);
    }
    if cur.is_some() && !opts.overwrite {
        work.warn(
            target,
            format!(
                "kept existing {label} ({}) because --no-overwrite was given",
                path.display()
            ),
        );
        return Ok(false);
    }
    if !check_redacted(work, path, cur.as_deref(), content, target) {
        return Ok(false);
    }
    work.fs.write(path, content.to_vec())?;
    Ok(true)
}

/// Write one command (or Codex prompt) file, noting it for `kitbag status`.
pub fn write_command(
    work: &mut Work,
    path: &Path,
    content: &[u8],
    opts: &ApplyOptions,
    target: &str,
    name: &str,
) -> Result<()> {
    if write_owned(
        work,
        path,
        content,
        opts,
        target,
        &format!("command '{name}'"),
    )? {
        work.changed_items
            .push((target.to_string(), format!("command {name}")));
    }
    Ok(())
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
    values_match(
        &Value::Object(clean(&da.data)),
        &Value::Object(clean(&db.data)),
        &eq,
    ) && eq(db.body.trim(), da.body.trim())
}

/// `cur` and `want` have the same shape, and strings compare with `eq(want, cur)`.
fn values_match(cur: &Value, want: &Value, eq: &impl Fn(&str, &str) -> bool) -> bool {
    match (cur, want) {
        (Value::String(c), Value::String(w)) => eq(w, c),
        (Value::Array(c), Value::Array(w)) => {
            c.len() == w.len() && c.iter().zip(w).all(|(c, w)| values_match(c, w, eq))
        }
        (Value::Object(c), Value::Object(w)) => {
            c.len() == w.len()
                && c.iter()
                    .all(|(k, cv)| w.get(k).is_some_and(|wv| values_match(cv, wv, eq)))
        }
        (c, w) => c == w,
    }
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

/// Whether a file has an executable bit set (always false off Unix).
pub fn skill_file_executable(path: &Path) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::metadata(path).is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
    }
    #[cfg(not(unix))]
    {
        let _ = path;
        false
    }
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
    let mut wrote = false;
    for f in &skill.files {
        let path = join_rel(dir, &f.path);
        let cur = work.fs.read_bytes(&path)?;
        if f.path != "SKILL.md"
            && cur.is_some()
            && cur.as_deref() != Some(f.content.as_slice())
            && !opts.overwrite
        {
            work.warn(
                target,
                format!(
                    "kept existing skill file {} because --no-overwrite was given",
                    path.display()
                ),
            );
            continue;
        }
        if cur.as_deref() != Some(f.content.as_slice())
            && check_redacted(work, &path, cur.as_deref(), &f.content, target)
        {
            work.fs.write(&path, f.content.clone())?;
            if f.executable {
                work.fs.mark_executable(&path)?;
            }
            wrote = true;
        }
    }
    if wrote {
        work.changed_items
            .push((target.to_string(), format!("skill {}", skill.name)));
    }
    Ok(())
}

/// Insert, replace or (with `None`) remove a managed block in a shared file.
pub fn upsert_block_file(
    work: &mut Work,
    path: &Path,
    id: &str,
    content: Option<&str>,
    overwrite: bool,
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
        let has_block = cur
            .as_deref()
            .is_some_and(|t| block::split(t).blocks.iter().any(|(b, _)| b == id));
        if has_block && content.is_some() && !overwrite {
            work.warn(
                target,
                format!(
                    "kept existing kitbag block '{id}' in {} because --no-overwrite was given",
                    path.display()
                ),
            );
            return Ok(());
        }
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
    work.track(target, path, Some(&opts.block_id), "instructions".into());
    upsert_block_file(
        work,
        path,
        &opts.block_id,
        content.as_deref(),
        opts.overwrite,
        target,
    )?;
    if fold_rules {
        let folded = block::fold_rules(&profile.rules);
        let id = format!("{}{RULES_BLOCK_SUFFIX}", opts.block_id);
        work.track(target, path, Some(&id), "rules".into());
        upsert_block_file(work, path, &id, folded.as_deref(), opts.overwrite, target)?;
    }
    Ok(())
}

/// Write one rule file kitbag owns, tracked for drift detection.
pub fn write_rule(
    work: &mut Work,
    path: &Path,
    content: &[u8],
    opts: &ApplyOptions,
    target: &str,
    name: &str,
) -> Result<()> {
    work.track(target, path, None, format!("rule {name}"));
    write_owned(work, path, content, opts, target, &format!("rule '{name}'"))?;
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
    /// Locations written with a `${VAR:-fallback}` because VAR was unset.
    defaulted: Vec<String>,
    missing: Vec<String>,
    kept: Vec<String>,
}

impl<'a> Resolver<'a> {
    pub fn new(env: &'a Env, opts: &ApplyOptions) -> Self {
        Resolver {
            env,
            mode: opts.secrets,
            resolved: Vec::new(),
            defaulted: Vec::new(),
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
            work.render.kept.extend(crate::secrets::refs_in(v));
            return v.to_string();
        }
        let mut probe = crate::secrets::RenderState::default();
        let out = render_refs(v, RefStyle::Resolve, &self.env.vars, &mut probe);
        work.render.resolved.extend(probe.resolved);
        work.render.missing.extend(probe.missing.iter().cloned());
        let from_env = crate::secrets::refs_in(v)
            .iter()
            .any(|r| self.env.vars.contains_key(r));
        if !probe.missing.is_empty() {
            self.missing.push(location.to_string());
        } else if from_env {
            self.resolved.push(location.to_string());
        }
        if !probe.fallbacks.is_empty() {
            self.defaulted.push(location.to_string());
        }
        work.render.fallbacks.extend(probe.fallbacks);
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
        if !self.defaulted.is_empty() {
            work.warn(
                target,
                format!(
                    "MCP server '{server}': {} written to {file} with the ${{VAR:-…}} fallback, because the variable is unset ({tool} can't expand env references there)",
                    self.defaulted.join(", ")
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
                    "MCP server '{server}': {} left as ${{VAR}} placeholders (secrets not resolved); {tool} won't expand them",
                    self.kept.join(", ")
                ),
            );
        }
    }
}

/// What a server entry runs or connects to: its `command`, or its URL
/// (`url`, Gemini CLI's `httpUrl` or Antigravity's `serverUrl`).
fn server_identity(def: &Map<String, Value>) -> (Option<&Value>, Option<&Value>) {
    (
        def.get("command"),
        def.get("url")
            .or_else(|| def.get("httpUrl"))
            .or_else(|| def.get("serverUrl")),
    )
}

/// Do two server entries run the same thing? Same command (or URL) and the
/// same args, so `npx -y good-pkg` and `npx -y evil-pkg` differ. Env
/// references in args (`${A}`, `${env:A}`, `$A`) don't count: `--key=${A}`
/// matches `--key=${B}`, and an arg that is only a reference matches anything
/// (it may have been resolved).
fn same_server(a: &Map<String, Value>, b: &Map<String, Value>) -> bool {
    use std::sync::LazyLock;
    static REF: LazyLock<regex::Regex> = LazyLock::new(|| {
        regex::Regex::new(r"\$\{[^}]*\}|\$[A-Za-z_][A-Za-z0-9_]*").expect("valid regex")
    });
    let args = |d: &Map<String, Value>| -> Vec<Value> {
        match d.get("args") {
            Some(Value::Array(a)) => a.clone(),
            Some(v) => vec![v.clone()],
            None => Vec::new(),
        }
    };
    let same_arg = |x: &Value, y: &Value| match (x, y) {
        (Value::String(x), Value::String(y)) => {
            let norm = |v: &str| REF.replace_all(v, regex::NoExpand("${}")).into_owned();
            let (x, y) = (norm(x), norm(y));
            x == y || x == "${}" || y == "${}"
        }
        _ => x == y,
    };
    let (aa, ba) = (args(a), args(b));
    server_identity(a) == server_identity(b)
        && aa.len() == ba.len()
        && aa.iter().zip(&ba).all(|(x, y)| same_arg(x, y))
}

/// The result of [`merge_server_def`].
#[derive(Debug, Clone, PartialEq)]
pub struct MergedServer {
    pub def: Map<String, Value>,
    /// Keys of the existing entry that were not carried over, because the
    /// server now runs a different command or args, or connects to a different URL.
    pub dropped: Vec<String>,
}

/// Combine an existing server definition with the one kitbag wants: keys kitbag
/// manages come from `want`; anything else the user set (timeouts, trust flags,
/// tool filters...) is preserved, as long as the entry still runs the same
/// command with the same args (see `same_server`) or connects to the same
/// URL. If it doesn't, it is a different server,
/// and keys such as `trust`, `headersHelper` or `oauth` granted to the old one
/// must not carry over: the entry is replaced wholesale. Returns `None` when
/// nothing would change.
pub fn merge_server_def(
    existing: Option<&Map<String, Value>>,
    want: &Map<String, Value>,
    managed: &[&str],
) -> Option<MergedServer> {
    let Some(cur) = existing else {
        return Some(MergedServer {
            def: want.clone(),
            dropped: Vec::new(),
        });
    };
    let extra = cur.iter().filter(|(k, _)| !managed.contains(&k.as_str()));
    let mut dropped = Vec::new();
    let mut out: Map<String, Value> = if same_server(cur, want) {
        extra.map(|(k, v)| (k.clone(), v.clone())).collect()
    } else {
        dropped = extra.map(|(k, _)| k.clone()).collect();
        Map::new()
    };
    for (k, v) in want {
        out.insert(k.clone(), v.clone());
    }
    let same = out.len() == cur.len() && out.iter().all(|(k, v)| cur.get(k) == Some(v));
    (!same).then_some(MergedServer { def: out, dropped })
}

/// Note the keys a replaced server entry lost.
pub fn note_dropped_server_keys(
    work: &mut Work,
    target: &str,
    name: &str,
    path: &Path,
    dropped: &[String],
) {
    if !dropped.is_empty() {
        work.warn(
            target,
            format!(
                "MCP server '{name}' in {} now runs a different command, arguments or URL, so settings of the old entry were not kept: {}",
                path.display(),
                dropped.join(", ")
            ),
        );
    }
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
    let skipped = skipped_servers(work, target);
    if servers.is_empty() && skipped.is_empty() {
        return Ok(());
    }
    let text = work.fs.read(path)?;
    if servers.is_empty() && text.is_none() {
        return Ok(());
    }
    let mut doc = JsonDoc::parse(text.as_deref(), path)?;
    let existing = doc.object(key_path);
    let mut changed = false;
    for name in &skipped {
        if remove_skipped_server(work, opts, target, path, name, existing.get(name)) {
            let mut full: Vec<&str> = key_path.to_vec();
            full.push(name);
            doc.remove(&full);
            changed = true;
        }
    }
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
        note_dropped_server_keys(work, target, name, path, &merged.dropped);
        let mut full: Vec<&str> = key_path.to_vec();
        full.push(name);
        let def = Value::Object(merged.def);
        doc.set(&full, &def);
        work.changed_servers
            .push((target.to_string(), name.clone()));
        record_server_write(work, target, path, name, Some(&def));
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

/// Names of the profile's servers that [`servers_for`] left out for `target`.
pub fn skipped_servers(work: &Work, target: &str) -> Vec<String> {
    work.skipped_servers
        .iter()
        .filter(|(t, _)| t == target)
        .map(|(_, n)| n.clone())
        .collect()
}

/// Note that the plan writes server `name` to `path` as `def`, or removes it (`None`).
pub fn record_server_write(
    work: &mut Work,
    target: &str,
    path: &Path,
    name: &str,
    def: Option<&Value>,
) {
    work.server_writes.push(super::ServerWrite {
        target: target.to_string(),
        path: path.to_path_buf(),
        name: name.to_string(),
        hash: def.map(crate::sync::entry_hash),
    });
    if def.is_none() {
        work.removed_servers
            .push((target.to_string(), name.to_string()));
    }
}

/// Should server `name`, which the profile disables for `target`, be removed
/// from `path`, where its entry is `current`? Only if an earlier apply of the
/// same profile (same name and source) put it there and it is unchanged
/// since; anything else (added by hand, by another profile, or edited) stays,
/// and the plan says why. A profile from a git source also needs the opt-in
/// for adding servers; without it the removal is held back. A removal is
/// noted and recorded.
pub fn remove_skipped_server(
    work: &mut Work,
    opts: &ApplyOptions,
    target: &str,
    path: &Path,
    name: &str,
    current: Option<&Value>,
) -> bool {
    let Some(current) = current else {
        return false;
    };
    let real = work
        .fs
        .real_path(path)
        .unwrap_or_else(|_| path.to_path_buf());
    let rec = opts
        .applied_servers
        .iter()
        .find(|r| r.target == target && r.path == real && r.name == name)
        .filter(|r| r.hash.is_some());
    let remote = opts.git_removals.is_some();
    let file = crate::ui::tildify(path, &work.home);
    let disabled = format!("MCP server '{name}' is disabled for {target} in the profile");
    let keep = match rec {
        None => Some(format!(
            "{disabled}, but kitbag has no record of adding it to {file}, so it was left in place; remove it by hand if you don't want it"
        )),
        Some(r) if r.profile != work.profile => Some(format!(
            "{disabled}, but profile '{}' added it to {file}, so it was left in place",
            r.profile
        )),
        Some(r) if r.source.is_none() && remote => Some(format!(
            "{disabled}, but kitbag's record of adding it to {file} doesn't say where the profile came from, and this one is from a git source, so it was left in place; remove it by hand if you don't want it"
        )),
        Some(r) if r.source.is_some() && r.source != opts.source => Some(format!(
            "{disabled}, but a profile '{}' from {} added it to {file}, so it was left in place",
            r.profile,
            r.source.as_deref().unwrap_or_default()
        )),
        Some(r) if r.hash.as_deref() != Some(crate::sync::entry_hash(current).as_str()) => {
            Some(format!(
                "{disabled}, but it was edited in {file} since kitbag added it, so it was left in place; remove it by hand if you don't want it"
            ))
        }
        Some(_) if !opts.overwrite => Some(format!(
            "kept MCP server '{name}' in {file}, disabled for {target} in the profile, because --no-overwrite was given"
        )),
        Some(_) => None,
    };
    if let Some(msg) = keep {
        work.warn(target, msg);
        return false;
    }
    if opts.git_removals.as_ref().is_some_and(|a| !a.allows(name)) {
        work.warn(
            target,
            format!(
                "{disabled}, but the profile comes from a git source, so it was not removed from {file} (pass --allow-git-mcp {name} to remove it)"
            ),
        );
        work.withheld_removals
            .push((target.to_string(), name.to_string()));
        return false;
    }
    work.info(
        target,
        format!(
            "MCP server '{name}' removed from {file}: disabled for {target} in the profile (kitbag added it there before)"
        ),
    );
    record_server_write(work, target, path, name, None);
    true
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
        if let Some(why) = s.skip_reason(target) {
            work.info(
                target,
                format!("MCP server '{name}' skipped on {target}: disabled in profile ({why})"),
            );
            work.skipped_servers
                .push((target.to_string(), name.clone()));
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
    if let Some((home, os)) = &cap.home {
        s = s.map_strings(|v| crate::portable::to_home_relative(v, home, *os));
    }
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
    let (redacted, redactions) = redact_server(name, &s);
    if cap.profile.mcp_servers.contains_key(name) {
        return;
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
    if cap.escapes(path, target) {
        return None;
    }
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

/// Redact an optional frontmatter string in place, collecting the kinds found.
fn redact_opt(v: &mut Option<String>, kinds: &mut Vec<&'static str>) {
    if let Some(s) = v {
        let (red, k) = redact_text(s);
        if !k.is_empty() {
            *s = red;
            kinds.extend(k);
        }
    }
}

pub fn add_rule(cap: &mut Captured, mut r: Rule, target: &str) {
    let (body, mut kinds) = redact_text(&r.body);
    redact_opt(&mut r.description, &mut kinds);
    kinds.dedup();
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
    let (body, mut kinds) = redact_text(&c.body);
    redact_opt(&mut c.description, &mut kinds);
    redact_opt(&mut c.argument_hint, &mut kinds);
    for v in c.extra.values_mut() {
        kinds.extend(crate::secrets::redact_json_strings(v));
    }
    kinds.dedup();
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

/// Remember which file a flattened name came from; two different files that
/// flatten to the same name (`team-build.md`, `team/build.md`) are an error
/// rather than one silently replacing the other.
pub fn claim_name(
    seen: &mut std::collections::BTreeMap<String, String>,
    dir: &Path,
    rel: &str,
    name: &str,
) -> Result<()> {
    if let Some(prev) = seen.insert(name.to_string(), rel.to_string())
        && prev != rel
    {
        bail!(
            "{} and {} both flatten to the name '{name}'; rename one of them",
            join_rel(dir, &prev).display(),
            join_rel(dir, rel).display()
        );
    }
    Ok(())
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
    if cap.escapes(dir, target) {
        return Ok(());
    }
    let mut seen = std::collections::BTreeMap::new();
    for rel in walk_files(dir, 500)? {
        if !rel.ends_with(ext) {
            continue;
        }
        let path = join_rel(dir, &rel);
        let Some(text) = read_text_item(cap, &path, target) else {
            continue;
        };
        let name = name_from_rel(&rel, ext);
        claim_name(&mut seen, dir, &rel, &name)?;
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
    if cap.escapes(dir, target) {
        return Ok(());
    }
    for name in subdirs(dir)? {
        if !is_valid_name(&name) || cap.profile.skills.iter().any(|s| s.name == name) {
            continue;
        }
        let skill_dir = dir.join(&name);
        if cap.escapes(&skill_dir, target) {
            continue;
        }
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
            files.push(SkillFile {
                path: rel,
                content,
                executable: skill_file_executable(&path),
            });
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
    let mut picked: Map<String, Value> = doc
        .iter()
        .filter(|(k, v)| keys.contains(&k.as_str()) && !v.is_null())
        .map(|(k, v)| (k.clone(), strip_nulls(v)))
        .collect();
    if let Some((home, os)) = &cap.home {
        crate::portable::capture_settings(target, &mut picked, home, *os);
    }
    // Permission rules such as `Bash(curl -H 'X-Api-Key: ...' *)` can embed secrets.
    let mut kinds = Vec::new();
    for v in picked.values_mut() {
        kinds.extend(crate::secrets::redact_json_strings(v));
    }
    kinds.dedup();
    if !kinds.is_empty() {
        cap.warn(
            target,
            format!("redacted {} from settings", kinds.join(", ")),
        );
    }
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
                executable: false,
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

    #[test]
    fn server_url_is_part_of_a_servers_identity() {
        let obj = |v: Value| v.as_object().unwrap().clone();
        let managed = &["serverUrl", "url", "headers"];
        // A different Antigravity serverUrl is a different server: its oauth goes.
        let cur = obj(json!({"serverUrl": "https://good.example/mcp", "oauth": {"clientId": "x"}}));
        let want = obj(json!({"serverUrl": "https://other.example/mcp"}));
        let m = merge_server_def(Some(&cur), &want, managed).unwrap();
        assert_eq!(m.dropped, vec!["oauth"]);
        assert_eq!(
            Value::Object(m.def),
            json!({"serverUrl": "https://other.example/mcp"})
        );
        // The same URL under the older `url` key is the same server.
        let cur = obj(json!({"url": "https://good.example/mcp", "timeoutSeconds": 30}));
        let want = obj(json!({"serverUrl": "https://good.example/mcp"}));
        let m = merge_server_def(Some(&cur), &want, managed).unwrap();
        assert!(m.dropped.is_empty());
        assert_eq!(
            Value::Object(m.def),
            json!({"timeoutSeconds": 30, "serverUrl": "https://good.example/mcp"})
        );
    }
}
