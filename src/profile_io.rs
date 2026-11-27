//! Reading and writing profile directories.

use std::collections::BTreeMap;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::Deserialize;
use serde_json::{Map, Value};
use toml_edit::{DocumentMut, Item, Table, value};

use crate::edit::toml::{json_to_table, json_to_value};
use crate::frontmatter::{self, glob_list, opt, string};
use crate::model::{
    Agent, Command, FORMAT_VERSION, Hook, McpServer, Profile, Rule, Skill, SkillFile,
    TARGETING_KEYS, Targeting, is_valid_name, portable_name_issues,
};
use crate::secrets::{
    redact_server, refs_in, sensitive_file, strong_secret_kind, text_secret_kind,
};

pub const MANIFEST: &str = "kitbag.toml";
/// The directory at a plugin's root that holds its rules, one `*.md` per rule.
pub const PLUGIN_RULES: &str = "rules";
/// Where a plugin may keep a manifest with its `name`, first match wins.
const PLUGIN_MANIFESTS: &[&str] = &[".claude-plugin/plugin.json", "plugin.json"];
/// Most files a skill may have. Capture and loading use the same limit.
pub const MAX_SKILL_FILES: usize = 200;
/// Largest file a profile may contain. Capture skips anything bigger, so it
/// never writes a profile that can't be loaded.
pub const MAX_FILE_BYTES: u64 = 2 * 1024 * 1024;

/// Most bytes all files of a profile may hold together, so loading a
/// profile (perhaps someone else's repository) has bounded memory use.
pub const MAX_PROFILE_BYTES: u64 = 64 * 1024 * 1024;

/// What is left of [`MAX_PROFILE_BYTES`] while one profile is read.
struct ReadBudget<'a> {
    dir: &'a Path,
    left: u64,
}

impl ReadBudget<'_> {
    fn take(&mut self, bytes: u64) -> Result<()> {
        if bytes > self.left {
            anyhow::bail!(
                "{} is larger than {} in total, the most a profile may hold; remove large files from it",
                self.dir.display(),
                max_size(MAX_PROFILE_BYTES)
            );
        }
        self.left -= bytes;
        Ok(())
    }
}

fn max_size(bytes: u64) -> String {
    format!("{} MiB", bytes / (1024 * 1024))
}

/// `MAX_FILE_BYTES` for messages.
pub fn max_file_size() -> String {
    max_size(MAX_FILE_BYTES)
}

/// A profile that failed validation, with every problem found.
#[derive(Debug, thiserror::Error)]
#[error("{message}{}", issues.iter().map(|i| format!("\n  - {i}")).collect::<String>())]
pub struct ProfileError {
    pub message: String,
    pub issues: Vec<String>,
}

impl ProfileError {
    fn new(message: impl Into<String>, issues: Vec<String>) -> Self {
        ProfileError {
            message: message.into(),
            issues,
        }
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Manifest {
    kitbag: u32,
    name: String,
    #[serde(default)]
    description: Option<String>,
    #[serde(default)]
    mcp_servers: BTreeMap<String, McpServer>,
    #[serde(default)]
    settings: BTreeMap<String, Map<String, Value>>,
    #[serde(default)]
    hooks: Vec<Hook>,
}

/// Parse and validate a `kitbag.toml` manifest into a profile without file-based parts.
pub fn parse_manifest(text: &str, file: &Path) -> Result<Profile, ProfileError> {
    let mut m: Manifest = toml::from_str(text).map_err(|e| {
        ProfileError::new(
            format!("{} is not a valid kitbag manifest", file.display()),
            vec![crate::edit::safe_parse_error(
                text,
                e.span().map(|s| s.start),
                e.message(),
            )],
        )
    })?;
    let mut issues = Vec::new();
    if m.kitbag != FORMAT_VERSION {
        issues.push(format!(
            "'kitbag' must be {FORMAT_VERSION} (the profile format version), got {}; upgrade kitbag to read newer profiles",
            m.kitbag
        ));
    }
    if let Err(why) = check_profile_name(&m.name) {
        issues.push(format!("'name' {why}"));
    }
    let mut warnings = Vec::new();
    for (name, s) in &mut m.mcp_servers {
        if !is_valid_name(name) {
            issues.push(format!(
                "mcp_servers.{name}: names may only contain letters, digits, '.', '_' and '-'"
            ));
        }
        for i in s.validate() {
            issues.push(format!("mcp_servers.{name}: {i}"));
        }
        let (legacy_errors, legacy_warnings) = s.take_legacy_targeting();
        // Aliases (`claude`) become canonical ids here, so nothing later sees them.
        // A target named twice (`["claude", "claude-code"]`) is kept once, at
        // its first place.
        normalize_target_ids(&mut s.targets, &mut s.disabled_on);
        let (errors, unknown) = s.targeting_issues(&crate::adapters::ids());
        for i in legacy_errors.into_iter().chain(errors) {
            issues.push(format!("mcp_servers.{name}: {i}"));
        }
        for w in legacy_warnings.into_iter().chain(unknown) {
            warnings.push(format!("mcp_servers.{name}: {w}"));
        }
    }
    // `[settings.claude]` is read as `[settings.claude-code]`, like the ids in
    // `targets`; a table under both names is an error.
    let mut settings = BTreeMap::new();
    let mut spelled: BTreeMap<String, String> = BTreeMap::new();
    for (key, table) in m.settings {
        let id = crate::adapters::canonical_id(&key).to_string();
        if let Some(other) = spelled.get(&id) {
            let alias = if *other == id { &key } else { other };
            issues.push(format!(
                "both [settings.{other}] and [settings.{key}] are set; '{alias}' is another name for '{id}', keep one"
            ));
            continue;
        }
        spelled.insert(id.clone(), key);
        settings.insert(id, table);
    }
    issues.extend(crate::hooks::validate(&m.hooks, None));
    if !issues.is_empty() {
        return Err(ProfileError::new(
            format!("{} is not a valid kitbag profile", file.display()),
            issues,
        ));
    }
    Ok(Profile {
        name: m.name.trim().to_string(),
        description: m.description.filter(|d| !d.trim().is_empty()),
        mcp_servers: m.mcp_servers,
        settings,
        hooks: m.hooks,
        warnings,
        ..Profile::default()
    })
}

/// Turn aliases in `targets` / `disabled_on` (`claude`) into canonical ids,
/// and keep a target named twice (`["claude", "claude-code"]`) once, at its
/// first place.
fn normalize_target_ids(targets: &mut Option<Vec<String>>, disabled_on: &mut Vec<String>) {
    for ids in targets.iter_mut().chain(std::iter::once(disabled_on)) {
        let mut seen = std::collections::BTreeSet::new();
        ids.retain_mut(|id| {
            *id = crate::adapters::canonical_id(id).to_string();
            seen.insert(id.clone())
        });
    }
}

/// Read (and take out of `data`) the targeting of an item loaded from `label`,
/// adding what is wrong with it to `issues` and `warnings`.
fn load_targeting(
    data: &mut Map<String, Value>,
    noun: &str,
    label: &str,
    issues: &mut Vec<String>,
    warnings: &mut Vec<String>,
) -> Targeting {
    let (t, errors, warns) = Targeting::take_from(
        data,
        &crate::adapters::ids(),
        |id| crate::adapters::canonical_id(id).to_string(),
        noun,
    );
    issues.extend(errors.into_iter().map(|e| format!("{label}: {e}")));
    warnings.extend(warns.into_iter().map(|w| format!("{label}: {w}")));
    t
}

/// Does `text` mention a targeting key at all? A cheap check before parsing.
fn mentions_targeting(text: &str) -> bool {
    TARGETING_KEYS.iter().any(|k| text.contains(k))
}

/// Why a profile `name` (as written, before trimming) is unusable, if it is:
/// empty, or holding control characters (a newline, tab, escape ...) or
/// invisible formatting characters (text-direction overrides, zero-width
/// spaces) that would end up in the apply records and in what kitbag prints.
pub fn check_profile_name(name: &str) -> std::result::Result<(), &'static str> {
    if name.trim().is_empty() {
        Err("must not be empty")
    } else if name.chars().any(char::is_control) {
        Err("must not contain control characters")
    } else if name.chars().any(is_hidden_format) {
        Err("must not contain text-direction or zero-width characters")
    } else {
        Ok(())
    }
}

/// Text-direction embeddings, overrides and isolates, zero-width spaces and
/// joiners, and the byte order mark: characters that change how a name looks
/// without being visible themselves.
fn is_hidden_format(c: char) -> bool {
    matches!(
        c,
        '\u{202A}'..='\u{202E}' | '\u{2066}'..='\u{2069}' | '\u{200B}'..='\u{200D}' | '\u{FEFF}'
    )
}

/// Read a file from an (untrusted) profile directory: no symlinks, bounded
/// size, and counted against the profile's total.
fn read_profile_file(path: &Path, budget: &mut ReadBudget) -> Result<Option<Vec<u8>>> {
    let meta = match fs::symlink_metadata(path) {
        Ok(m) => m,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e).with_context(|| format!("reading {}", path.display())),
    };
    if meta.file_type().is_symlink() {
        // A profile may come from someone else's repository; following a link
        // could copy arbitrary local files (keys, tokens) into a tool's config.
        anyhow::bail!(
            "symbolic links are not allowed in a profile: {}",
            path.display()
        );
    }
    if meta.len() > MAX_FILE_BYTES {
        anyhow::bail!("{} is larger than {}", path.display(), max_file_size());
    }
    budget.take(meta.len())?;
    let data = fs::read(path).with_context(|| format!("reading {}", path.display()))?;
    // The file may have grown since it was measured.
    budget.take((data.len() as u64).saturating_sub(meta.len()))?;
    Ok(Some(data))
}

fn read_profile_text(path: &Path, budget: &mut ReadBudget) -> Result<Option<String>> {
    match read_profile_file(path, budget)? {
        None => Ok(None),
        Some(b) => {
            Ok(Some(String::from_utf8(b).with_context(|| {
                format!("{} is not valid UTF-8", path.display())
            })?))
        }
    }
}

/// Directory entries `(name, is_dir)`, sorted; rejects symlinks. Missing dir -> empty.
fn list_profile_dir(dir: &Path) -> Result<Vec<(String, bool)>> {
    if fs::symlink_metadata(dir).is_ok_and(|m| m.file_type().is_symlink()) {
        anyhow::bail!(
            "symbolic links are not allowed in a profile: {}",
            dir.display()
        );
    }
    let rd = match fs::read_dir(dir) {
        Ok(rd) => rd,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(e).with_context(|| format!("listing {}", dir.display())),
    };
    let mut out = Vec::new();
    for e in rd {
        let e = e?;
        let ft = e.file_type()?;
        if ft.is_symlink() {
            anyhow::bail!(
                "symbolic links are not allowed in a profile: {}",
                e.path().display()
            );
        }
        out.push((e.file_name().to_string_lossy().into_owned(), ft.is_dir()));
    }
    out.sort();
    Ok(out)
}

/// Parse a rule markdown file (profile format; also accepts `paths` / `applyTo` aliases).
pub fn rule_from_markdown(name: &str, text: &str) -> Rule {
    let doc = frontmatter::parse(text);
    let d = &doc.data;
    let globs = ["globs", "paths", "applyTo"]
        .iter()
        .map(|k| glob_list(d.get(*k)))
        .find(|g| !g.is_empty())
        .unwrap_or_default();
    Rule {
        name: name.to_string(),
        description: string(d.get("description")),
        globs,
        always_apply: d.get("alwaysApply").and_then(Value::as_bool),
        body: doc.body.trim().to_string(),
        ..Default::default()
    }
}

pub fn rule_to_markdown(r: &Rule) -> String {
    frontmatter::render(
        &[
            ("description", opt(&r.description)),
            ("globs", Value::from(r.globs.clone())),
            (
                "alwaysApply",
                r.always_apply.map_or(Value::Null, Value::Bool),
            ),
        ],
        &r.body,
    )
}

const COMMAND_KNOWN: &[&str] = &["description", "argument-hint", "argumentHint"];

pub fn command_from_markdown(name: &str, text: &str) -> Command {
    let doc = frontmatter::parse(text);
    let d = &doc.data;
    Command {
        name: name.to_string(),
        description: string(d.get("description")),
        argument_hint: string(d.get("argument-hint")).or_else(|| string(d.get("argumentHint"))),
        extra: d
            .iter()
            .filter(|(k, v)| !COMMAND_KNOWN.contains(&k.as_str()) && !v.is_null())
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect(),
        body: doc.body.trim().to_string(),
        ..Default::default()
    }
}

/// Command markdown; `with_extra` includes tool-specific frontmatter keys.
pub fn command_to_markdown(c: &Command, with_extra: bool) -> String {
    let mut entries: Vec<(&str, Value)> = vec![
        ("description", opt(&c.description)),
        ("argument-hint", opt(&c.argument_hint)),
    ];
    if with_extra {
        entries.extend(c.extra.iter().map(|(k, v)| (k.as_str(), v.clone())));
    }
    frontmatter::render(&entries, &c.body)
}

pub fn agent_from_markdown(name: &str, text: &str) -> Agent {
    let doc = frontmatter::parse(text);
    Agent {
        name: name.to_string(),
        description: string(doc.data.get("description")),
        extra: doc
            .data
            .iter()
            .filter(|(k, v)| k.as_str() != "description" && !v.is_null())
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect(),
        body: doc.body.trim().to_string(),
        ..Default::default()
    }
}

/// Agent markdown: `description` first, then the other keys as they were.
pub fn agent_to_markdown(a: &Agent) -> String {
    let mut entries: Vec<(&str, Value)> = vec![("description", opt(&a.description))];
    entries.extend(a.extra.iter().map(|(k, v)| (k.as_str(), v.clone())));
    frontmatter::render(&entries, &a.body)
}

pub fn skill_description(files: &[SkillFile]) -> Option<String> {
    let main = files.iter().find(|f| f.path == "SKILL.md")?;
    let text = String::from_utf8_lossy(&main.content);
    string(frontmatter::parse(&text).data.get("description"))
}

/// Read a skill's targeting from the frontmatter of its `SKILL.md` and take
/// the keys out of the file's content, so tools never see them (they are put
/// back when the profile is written, see [`profile_files`]).
fn take_skill_targeting(
    name: &str,
    files: &mut [SkillFile],
    issues: &mut Vec<String>,
    warnings: &mut Vec<String>,
) -> Targeting {
    let Some(main) = files.iter_mut().find(|f| f.path == "SKILL.md") else {
        return Targeting::default();
    };
    let Ok(text) = std::str::from_utf8(&main.content) else {
        return Targeting::default();
    };
    if !mentions_targeting(text) {
        return Targeting::default();
    }
    let mut data = frontmatter::parse(text).data;
    let label = format!("skills/{name}/SKILL.md");
    let t = load_targeting(&mut data, "skill", &label, issues, warnings);
    if let Some(stripped) = frontmatter::remove_keys(text, &TARGETING_KEYS) {
        main.content = stripped.into_bytes();
    }
    t
}

fn read_skill_dir(
    dir: &Path,
    prefix: &str,
    out: &mut Vec<SkillFile>,
    budget: &mut ReadBudget,
) -> Result<()> {
    for (name, is_dir) in list_profile_dir(dir)? {
        if name == ".git" || name == ".DS_Store" || name == "node_modules" {
            continue;
        }
        if out.len() >= MAX_SKILL_FILES {
            anyhow::bail!("{} has more than {MAX_SKILL_FILES} files", dir.display());
        }
        let rel = if prefix.is_empty() {
            name.clone()
        } else {
            format!("{prefix}/{name}")
        };
        let abs = dir.join(&name);
        if is_dir {
            read_skill_dir(&abs, &rel, out, budget)?;
        } else if let Some(content) = read_profile_file(&abs, budget)? {
            out.push(SkillFile {
                path: rel,
                content,
                executable: crate::adapters::common::skill_file_executable(&abs),
            });
        }
    }
    Ok(())
}

/// Load a profile directory, or a plugin directory (see [`load_named`]) named
/// after the directory.
pub fn load(dir: &Path) -> Result<Profile> {
    load_with_limit(dir, MAX_PROFILE_BYTES, None)
}

/// Load a profile directory. A directory without a `kitbag.toml` but with a
/// `rules/` directory at its root is loaded as a plugin: its rules, commands
/// and skills, no instructions, MCP servers or settings, under the `name` in
/// its `.claude-plugin/plugin.json` or `plugin.json`, else `name_hint` (or
/// the directory's name) as a slug. Nothing is written into the directory.
pub fn load_named(dir: &Path, name_hint: &str) -> Result<Profile> {
    load_with_limit(dir, MAX_PROFILE_BYTES, Some(name_hint))
}

/// Is `dir` a plugin rather than a profile: no `kitbag.toml`, but `rules/`?
pub fn is_plugin(dir: &Path) -> bool {
    !dir.join(MANIFEST).exists() && dir.join(PLUGIN_RULES).is_dir()
}

/// The `name` and `description` in a plugin's manifest, if it has one.
fn plugin_manifest(
    dir: &Path,
    budget: &mut ReadBudget,
    warnings: &mut Vec<String>,
) -> Result<Option<(Option<String>, Option<String>)>> {
    for rel in PLUGIN_MANIFESTS {
        let path = dir.join(rel);
        // `read_profile_file` only checks the file itself; a symlinked
        // `.claude-plugin/` would read a manifest from outside the plugin.
        if let Some(parent) = Path::new(rel)
            .parent()
            .filter(|p| !p.as_os_str().is_empty())
            && fs::symlink_metadata(dir.join(parent)).is_ok_and(|m| m.file_type().is_symlink())
        {
            anyhow::bail!(
                "symbolic links are not allowed in a profile: {}",
                dir.join(parent).display()
            );
        }
        let Some(text) = read_profile_text(&path, budget)? else {
            continue;
        };
        let Ok(Value::Object(m)) = serde_json::from_str::<Value>(&text) else {
            warnings.push(format!(
                "{rel} is not a JSON object; the plugin is named after its directory"
            ));
            return Ok(Some((None, None)));
        };
        let name = string(m.get("name")).filter(|n| check_profile_name(n).is_ok());
        if name.is_none() && m.contains_key("name") {
            warnings.push(format!(
                "{rel}: 'name' is not a usable name; the plugin is named after its directory"
            ));
        }
        return Ok(Some((name, string(m.get("description")))));
    }
    Ok(None)
}

/// The profile for a plugin directory, without its rules, commands and skills.
fn plugin_profile(dir: &Path, name_hint: Option<&str>, budget: &mut ReadBudget) -> Result<Profile> {
    let rules = dir.join(PLUGIN_RULES);
    // A symlinked `rules` is refused by `list_profile_dir`, a file is no plugin.
    let rules_dir =
        fs::symlink_metadata(&rules).is_ok_and(|m| m.is_dir() || m.file_type().is_symlink());
    let has_rules = rules_dir
        && list_profile_dir(&rules)?
            .iter()
            .any(|(f, is_dir)| !is_dir && (f.ends_with(".md") || f.ends_with(".mdc")));
    let mut warnings = Vec::new();
    let manifest = if rules_dir {
        plugin_manifest(dir, budget, &mut warnings)?
    } else {
        None
    };
    if !has_rules && manifest.is_none() {
        return Err(ProfileError::new(
            format!(
                "no {MANIFEST} found in {}, and no rules in a {PLUGIN_RULES}/ directory at its root: \
                 create a profile with 'kitbag init', or put a plugin's rules in {PLUGIN_RULES}/<name>.md",
                dir.display()
            ),
            vec![],
        )
        .into());
    }
    if fs::symlink_metadata(dir.join("instructions.md")).is_ok() {
        warnings.push(
            "instructions.md is not read from a plugin; only rules/, commands/ and skills/ are"
                .to_string(),
        );
    }
    let (name, description) = manifest.unwrap_or_default();
    let name = name.unwrap_or_else(|| {
        let fallback = match name_hint {
            Some(h) => h.to_string(),
            None => std::path::absolute(dir)
                .ok()
                .and_then(|d| d.file_name().map(|n| n.to_string_lossy().into_owned()))
                .unwrap_or_default(),
        };
        crate::model::slugify(&fallback)
    });
    Ok(Profile {
        name: name.trim().to_string(),
        description: description.filter(|d| !d.trim().is_empty()),
        warnings,
        ..Profile::default()
    })
}

fn load_with_limit(dir: &Path, limit: u64, name_hint: Option<&str>) -> Result<Profile> {
    let budget = &mut ReadBudget { dir, left: limit };
    let manifest = dir.join(MANIFEST);
    let mut plugin = false;
    let mut p = match read_profile_text(&manifest, budget)? {
        Some(text) => {
            let mut p = parse_manifest(&text, &manifest)?;
            if let Some(i) = read_profile_text(&dir.join("instructions.md"), budget)?
                && !i.trim().is_empty()
            {
                p.instructions = Some(i.trim().to_string());
            }
            p
        }
        None => {
            plugin = true;
            plugin_profile(dir, name_hint, budget)?
        }
    };
    let mut issues = Vec::new();
    for (file, is_dir) in list_profile_dir(&dir.join(PLUGIN_RULES))? {
        let Some(stem) = file
            .strip_suffix(".md")
            .or_else(|| file.strip_suffix(".mdc"))
        else {
            continue;
        };
        if is_dir {
            continue;
        }
        if !is_valid_name(stem) {
            issues.push(format!("rules/{file}: invalid file name"));
            continue;
        }
        let text =
            read_profile_text(&dir.join(PLUGIN_RULES).join(&file), budget)?.unwrap_or_default();
        let mut rule = rule_from_markdown(stem, &text);
        if mentions_targeting(&text) {
            rule.targeting = load_targeting(
                &mut frontmatter::parse(&text).data,
                "rule",
                &format!("rules/{file}"),
                &mut issues,
                &mut p.warnings,
            );
        }
        p.rules.push(rule);
    }
    for (file, is_dir) in list_profile_dir(&dir.join("commands"))? {
        let Some(stem) = file.strip_suffix(".md") else {
            continue;
        };
        if is_dir {
            continue;
        }
        if !is_valid_name(stem) {
            issues.push(format!("commands/{file}: invalid file name"));
            continue;
        }
        let text =
            read_profile_text(&dir.join("commands").join(&file), budget)?.unwrap_or_default();
        let mut command = command_from_markdown(stem, &text);
        command.targeting = load_targeting(
            &mut command.extra,
            "command",
            &format!("commands/{file}"),
            &mut issues,
            &mut p.warnings,
        );
        p.commands.push(command);
    }
    // A plugin's `agents/` holds Claude Code subagents, another format.
    let agents = if plugin {
        Vec::new()
    } else {
        list_profile_dir(&dir.join("agents"))?
    };
    for (file, is_dir) in agents {
        let Some(stem) = file.strip_suffix(".md") else {
            continue;
        };
        if is_dir {
            continue;
        }
        if !is_valid_name(stem) {
            issues.push(format!("agents/{file}: invalid file name"));
            continue;
        }
        let text = read_profile_text(&dir.join("agents").join(&file), budget)?.unwrap_or_default();
        let mut agent = agent_from_markdown(stem, &text);
        agent.targeting = load_targeting(
            &mut agent.extra,
            "agent",
            &format!("agents/{file}"),
            &mut issues,
            &mut p.warnings,
        );
        p.agents.push(agent);
    }
    for (name, is_dir) in list_profile_dir(&dir.join("skills"))? {
        if !is_dir {
            continue;
        }
        if !is_valid_name(&name) {
            issues.push(format!("skills/{name}: invalid directory name"));
            continue;
        }
        let mut files = Vec::new();
        read_skill_dir(&dir.join("skills").join(&name), "", &mut files, budget)?;
        if !files.iter().any(|f| f.path == "SKILL.md") {
            issues.push(format!("skills/{name}: missing SKILL.md"));
            continue;
        }
        let targeting = take_skill_targeting(&name, &mut files, &mut issues, &mut p.warnings);
        p.skills.push(Skill {
            description: skill_description(&files),
            name,
            files,
            targeting,
        });
    }
    // A plugin's `hooks/` is Claude Code's own format, not a profile's.
    if !plugin {
        read_skill_dir(&dir.join("hooks"), "", &mut p.hook_files, budget)?;
        if !p.hooks.is_empty() {
            issues.extend(crate::hooks::validate(&p.hooks, Some(&p.hook_files)));
        }
    }
    issues.extend(portable_name_issues(&p));
    if !issues.is_empty() {
        return Err(ProfileError::new(
            format!("{} is not a valid kitbag profile", dir.display()),
            issues,
        )
        .into());
    }
    p.sort();
    Ok(p)
}

/// Render the manifest for `p`.
pub fn manifest_toml(p: &Profile) -> String {
    let mut doc = DocumentMut::new();
    doc.decor_mut().set_prefix(
        "# kitbag profile: https://github.com/cargopete/kitbag\n\
         # Secrets are never stored here. Use ${ENV_VAR} references; they are\n\
         # resolved from your environment (or by the tool itself) when you apply.\n",
    );
    doc["kitbag"] = value(i64::from(FORMAT_VERSION));
    doc["name"] = value(p.name.as_str());
    if let Some(d) = &p.description {
        doc["description"] = value(d.as_str());
    }
    if !p.mcp_servers.is_empty() {
        let mut servers = Table::new();
        servers.set_implicit(true);
        for (name, s) in &p.mcp_servers {
            let v = serde_json::to_value(s).expect("serializable");
            servers.insert(
                name,
                Item::Table(json_to_table(v.as_object().expect("object"))),
            );
        }
        doc["mcp_servers"] = Item::Table(servers);
    }
    if !p.settings.is_empty() {
        let mut settings = Table::new();
        settings.set_implicit(true);
        for (target, m) in &p.settings {
            let mut t = Table::new();
            for (k, v) in m {
                if let Some(val) = json_to_value(v) {
                    t.insert(k, Item::Value(val));
                }
            }
            settings.insert(target, Item::Table(t));
        }
        doc["settings"] = Item::Table(settings);
    }
    if !p.hooks.is_empty() {
        let mut hooks = toml_edit::ArrayOfTables::new();
        for h in &p.hooks {
            let v = serde_json::to_value(h).expect("serializable");
            hooks.push(json_to_table(v.as_object().expect("object")));
        }
        doc["hooks"] = Item::ArrayOfTables(hooks);
    }
    doc.to_string()
}

/// Every file making up the profile on disk, as relative path -> content.
pub fn profile_files(p: &Profile) -> BTreeMap<String, Vec<u8>> {
    let mut out = BTreeMap::new();
    out.insert(MANIFEST.to_string(), manifest_toml(p).into_bytes());
    if let Some(i) = &p.instructions {
        out.insert(
            "instructions.md".into(),
            format!("{}\n", i.trim()).into_bytes(),
        );
    }
    // Targeting goes at the end of an item's frontmatter, as `targets` /
    // `disabled_on`; items without it are written as they always were.
    let with = |markdown: String, t: &Targeting| frontmatter::add_entries(&markdown, &t.entries());
    for r in &p.rules {
        out.insert(
            format!("rules/{}.md", r.name),
            with(rule_to_markdown(r), &r.targeting).into_bytes(),
        );
    }
    for c in &p.commands {
        out.insert(
            format!("commands/{}.md", c.name),
            with(command_to_markdown(c, true), &c.targeting).into_bytes(),
        );
    }
    for a in &p.agents {
        out.insert(
            format!("agents/{}.md", a.name),
            with(agent_to_markdown(a), &a.targeting).into_bytes(),
        );
    }
    for s in &p.skills {
        for f in &s.files {
            let content = match std::str::from_utf8(&f.content) {
                Ok(text) if f.path == "SKILL.md" && !s.targeting.is_empty() => {
                    with(text.to_string(), &s.targeting).into_bytes()
                }
                _ => f.content.clone(),
            };
            out.insert(format!("skills/{}/{}", s.name, f.path), content);
        }
    }
    for f in &p.hook_files {
        out.insert(format!("hooks/{}", f.path), f.content.clone());
    }
    out
}

/// Carry the per-tool targeting (`targets` / `disabled_on`) of `old`'s MCP
/// servers over to the same-named servers of a fresh capture, which can't
/// know it. Returns the servers it was kept for.
pub fn keep_targeting(new: &mut Profile, old: &Profile) -> Vec<String> {
    let mut kept = Vec::new();
    for (name, s) in &mut new.mcp_servers {
        let Some(o) = old.mcp_servers.get(name) else {
            continue;
        };
        if o.targets.is_none() && o.disabled_on.is_empty() {
            continue;
        }
        s.targets.clone_from(&o.targets);
        s.disabled_on.clone_from(&o.disabled_on);
        // A server kitbag turned off in one tool reads back as disabled when
        // that tool alone is captured; the targeting already says so.
        if s.disabled && !o.disabled {
            s.disabled = false;
        }
        kept.push(name.clone());
    }
    kept
}

/// Copy over `old`'s hooks and their scripts, for a capture that didn't look
/// for hooks, so re-capturing doesn't silently drop them. Returns how many.
pub fn keep_hooks(new: &mut Profile, old: &Profile) -> usize {
    if !new.hooks.is_empty() || old.hooks.is_empty() {
        return 0;
    }
    new.hooks.clone_from(&old.hooks);
    new.hook_files.clone_from(&old.hook_files);
    new.hooks.len()
}

/// Carry the per-tool targeting of `old`'s rules, commands, skills and agents
/// over to the same-named items of a fresh capture. Returns them as
/// `rule <name>` ...
pub fn keep_item_targeting(new: &mut Profile, old: &Profile) -> Vec<String> {
    let mut kept = Vec::new();
    macro_rules! keep {
        ($field:ident, $kind:literal) => {
            for item in &mut new.$field {
                if let Some(o) = old.$field.iter().find(|o| o.name == item.name)
                    && !o.targeting.is_empty()
                {
                    item.targeting = o.targeting.clone();
                    kept.push(format!(concat!($kind, " {}"), item.name));
                }
            }
        };
    }
    keep!(rules, "rule");
    keep!(commands, "command");
    keep!(skills, "skill");
    keep!(agents, "agent");
    kept
}

/// Copy over `old`'s rules, commands, skills and agents that have targeting
/// and that a fresh capture didn't find in any tool (set up only for a tool
/// not installed here), so a re-capture doesn't drop them. Returns them as
/// `rule <name>` ...
pub fn keep_uncaptured_items(new: &mut Profile, old: &Profile) -> Vec<String> {
    let mut kept = Vec::new();
    macro_rules! keep {
        ($field:ident, $kind:literal) => {
            for item in &old.$field {
                if !item.targeting.is_empty() && !new.$field.iter().any(|n| n.name == item.name) {
                    new.$field.push(item.clone());
                    kept.push(format!(concat!($kind, " {}"), item.name));
                }
            }
        };
    }
    keep!(rules, "rule");
    keep!(commands, "command");
    keep!(skills, "skill");
    keep!(agents, "agent");
    new.sort();
    kept
}

/// Copy over `old`'s MCP servers that a fresh capture didn't find in any
/// tool (disabled everywhere, or set up for a tool not installed here), with
/// their targeting, so a re-capture doesn't silently drop them. Returns the
/// servers it kept.
pub fn keep_uncaptured_servers(new: &mut Profile, old: &Profile) -> Vec<String> {
    let mut kept = Vec::new();
    for (name, s) in &old.mcp_servers {
        if !new.mcp_servers.contains_key(name) {
            new.mcp_servers.insert(name.clone(), s.clone());
            kept.push(name.clone());
        }
    }
    kept
}

/// Write a profile directory. Refuses to replace an existing profile unless `force`,
/// and refuses outright if anything still looks like a credential.
pub fn save(p: &Profile, dir: &Path, force: bool) -> Result<Vec<PathBuf>> {
    let manifest = dir.join(MANIFEST);
    let leaks = scan_for_secrets(p);
    if !leaks.is_empty() {
        return Err(ProfileError::new(
            "refusing to write a profile that appears to contain secrets",
            leaks,
        )
        .into());
    }
    let names = portable_name_issues(p);
    if !names.is_empty() {
        return Err(ProfileError::new(
            "refusing to write a profile that couldn't be used on every system",
            names,
        )
        .into());
    }
    let new_files = profile_files(p);
    let oversize: Vec<String> = new_files
        .iter()
        .filter(|(_, c)| c.len() as u64 > MAX_FILE_BYTES)
        .map(|(rel, c)| format!("{rel}: {} KiB", c.len() / 1024))
        .collect();
    if !oversize.is_empty() {
        return Err(ProfileError::new(
            format!(
                "refusing to write a profile that couldn't be loaded: files are limited to {}",
                max_file_size()
            ),
            oversize,
        )
        .into());
    }
    let total: u64 = new_files.values().map(|c| c.len() as u64).sum();
    if total > MAX_PROFILE_BYTES {
        return Err(ProfileError::new(
            format!(
                "refusing to write a profile that couldn't be loaded: it would hold {} MiB, more than the {} a profile may hold in total",
                total.div_ceil(1024 * 1024),
                max_size(MAX_PROFILE_BYTES)
            ),
            vec![],
        )
        .into());
    }
    if manifest.exists() {
        if !force {
            return Err(ProfileError::new(
                format!(
                    "{} already contains a profile; pass --force to replace it",
                    dir.display()
                ),
                vec![],
            )
            .into());
        }
        // Remove only files that belonged to the previous profile and are no
        // longer part of it; anything else in the directory (README, .git) stays.
        let old = load(dir).with_context(|| {
            format!("the existing profile in {} can't be read, so it won't be replaced; fix or remove it first", dir.display())
        })?;
        // `profile_files` names every rule `<name>.md`, but a profile may hold it
        // as `<name>.mdc`: look for that twin too, so it is neither missed when
        // removing nor left behind next to a rewritten `.md`.
        let mut stale: Vec<String> = Vec::new();
        for rel in profile_files(&old).keys() {
            let is_rule = rel.starts_with("rules/") && rel.ends_with(".md");
            if !new_files.contains_key(rel) {
                stale.push(rel.clone());
            }
            if is_rule {
                stale.push(format!("{rel}c"));
            }
        }
        for rel in &stale {
            let abs = crate::plan_fs::join_rel(dir, rel);
            if abs.symlink_metadata().is_err() {
                continue;
            }
            fs::remove_file(&abs).with_context(|| format!("removing {}", abs.display()))?;
            // Tidy now-empty directories up to the profile root.
            let mut d = abs.parent().map(Path::to_path_buf);
            while let Some(cur) = d {
                if cur == dir || fs::remove_dir(&cur).is_err() {
                    break;
                }
                d = cur.parent().map(Path::to_path_buf);
            }
        }
    } else if fs::read_dir(dir).is_ok_and(|rd| {
        // A fresh repository (.git, README, LICENSE) is fine to capture into.
        rd.flatten().any(|e| {
            let n = e.file_name().to_string_lossy().to_ascii_lowercase();
            !(n.starts_with('.') || n.starts_with("readme") || n.starts_with("license"))
        })
    }) {
        return Err(ProfileError::new(
            format!(
                "{} is not empty and is not a kitbag profile; choose a new or empty directory",
                dir.display()
            ),
            vec![],
        )
        .into());
    }
    // Skill scripts that were executable stay so (Unix).
    #[cfg(unix)]
    let exec: std::collections::BTreeSet<String> = p
        .skills
        .iter()
        .flat_map(|s| {
            s.files
                .iter()
                .filter(|f| f.executable)
                .map(|f| format!("skills/{}/{}", s.name, f.path))
        })
        .chain(
            p.hook_files
                .iter()
                .filter(|f| f.executable)
                .map(|f| format!("hooks/{}", f.path)),
        )
        .collect();
    let mut written = Vec::new();
    fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
    for (rel, content) in new_files {
        // Links in the profile are replaced, never written through.
        crate::plan_fs::write_owned_file(dir, &rel, &content)?;
        #[cfg(unix)]
        if exec.contains(&rel) {
            use std::os::unix::fs::PermissionsExt;
            let path = crate::plan_fs::join_rel(dir, &rel);
            let mode = fs::metadata(&path)?.permissions().mode();
            fs::set_permissions(&path, fs::Permissions::from_mode(mode | 0o111))?;
        }
        written.push(crate::plan_fs::join_rel(dir, &rel));
    }
    Ok(written)
}

/// Last line of defence: anything in the profile that still looks like a credential.
pub fn scan_for_secrets(p: &Profile) -> Vec<String> {
    let mut out = Vec::new();
    let mut out_extra = Vec::new();
    let mut check = |where_: String, v: &str, free_text: bool| {
        let kind = if free_text {
            text_secret_kind(v)
        } else {
            strong_secret_kind(v)
        };
        if let Some(k) = kind {
            out.push(format!("{where_}: looks like a secret ({k})"));
        }
    };
    for (n, s) in &p.mcp_servers {
        for (loc, v) in s.strings() {
            check(format!("mcp_servers.{n}.{loc}"), v, false);
        }
        for (loc, v) in s.strings() {
            for (var, _) in crate::secrets::secret_fallbacks(v) {
                out_extra.push(format!(
                    "mcp_servers.{n}.{loc}: the ${{{var}:-…}} fallback looks like a secret"
                ));
            }
        }
        // Anything capture-time redaction would still replace is a secret too.
        // (Secret fallbacks are reported above, with a clearer message.)
        for r in redact_server(n, s).1 {
            if r.reason != "secret-fallback" {
                out_extra.push(format!(
                    "{}: looks like a secret ({})",
                    r.location, r.reason
                ));
            }
        }
    }
    for (t, m) in &p.settings {
        let all = Value::Object(m.clone());
        check(format!("settings.{t}"), &all.to_string(), false);
        for s in crate::secrets::json_strings(&all) {
            check(format!("settings.{t}"), s, true);
        }
        // Nested values under secret-sounding names, as in structured files.
        for at in crate::secrets::json_secret_fields(&all) {
            out_extra.push(format!(
                "settings.{t}.{at}: looks like a secret (secret-named-field)"
            ));
        }
    }
    if let Some(i) = &p.instructions {
        check("instructions.md".into(), i, true);
    }
    for r in &p.rules {
        let at = format!("rules/{}.md", r.name);
        check(at.clone(), &r.body, true);
        if let Some(d) = &r.description {
            check(at, d, true);
        }
    }
    for c in &p.commands {
        let at = format!("commands/{}.md", c.name);
        check(at.clone(), &c.body, true);
        for v in [&c.description, &c.argument_hint].into_iter().flatten() {
            check(at.clone(), v, true);
        }
        for v in c.extra.values() {
            for s in crate::secrets::json_strings(v) {
                check(at.clone(), s, true);
            }
        }
    }
    for a in &p.agents {
        let at = format!("agents/{}.md", a.name);
        check(at.clone(), &a.body, true);
        if let Some(d) = &a.description {
            check(at.clone(), d, true);
        }
        for v in a.extra.values() {
            for s in crate::secrets::json_strings(v) {
                check(at.clone(), s, true);
            }
        }
    }
    for (i, h) in p.hooks.iter().enumerate() {
        let at = format!("hooks[{i}]");
        check(at.clone(), &h.command, true);
        for v in h.extra.values() {
            for s in crate::secrets::json_strings(v) {
                check(at.clone(), s, true);
            }
        }
    }
    for f in &p.hook_files {
        if let Ok(t) = std::str::from_utf8(&f.content)
            && let Some(k) = crate::secrets::support_file_secret_kind(&f.path, t)
        {
            out.push(format!("hooks/{}: looks like a secret ({k})", f.path));
        }
        if let Some(why) = sensitive_file(&f.path, &f.content) {
            out_extra.push(format!("hooks/{}: {why}", f.path));
        }
    }
    for s in &p.skills {
        for f in &s.files {
            if let Ok(t) = std::str::from_utf8(&f.content)
                && let Some(k) = crate::secrets::support_file_secret_kind(&f.path, t)
            {
                out.push(format!(
                    "skills/{}/{}: looks like a secret ({k})",
                    s.name, f.path
                ));
            }
            if let Some(why) = sensitive_file(&f.path, &f.content) {
                out_extra.push(format!("skills/{}/{}: {why}", s.name, f.path));
            }
        }
    }
    out.extend(out_extra);
    out.dedup();
    out
}

/// Every env var the profile's MCP servers reference, with where it is used.
pub fn required_env(p: &Profile) -> BTreeMap<String, Vec<String>> {
    let mut out: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for (n, s) in &p.mcp_servers {
        for (loc, v) in s.strings() {
            for name in refs_in(v) {
                out.entry(name)
                    .or_default()
                    .push(format!("mcp_servers.{n}.{loc}"));
            }
        }
    }
    for (i, h) in p.hooks.iter().enumerate() {
        for name in refs_in(&h.command) {
            // The tool's and the shell's own variables are not ones to set.
            if name.starts_with("CLAUDE_")
                || matches!(name.as_str(), "HOME" | "PATH" | "PWD" | "USER")
            {
                continue;
            }
            out.entry(name).or_default().push(format!("hooks[{i}]"));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn agents_load_and_save_with_their_frontmatter() {
        let dir = tempfile::tempdir().unwrap();
        write(dir.path(), "kitbag.toml", "kitbag = 1\nname = \"x\"\n");
        let text = "---\ndescription: Reviews\ntools:\n  - read\nmodel: GPT-5\nuser-invocable: false\n---\n\nReview.\n";
        write(dir.path(), "agents/reviewer.md", text);
        write(dir.path(), "agents/notes.txt", "not an agent");
        let p = load(dir.path()).unwrap();
        assert_eq!(p.agents.len(), 1);
        let a = &p.agents[0];
        assert_eq!(a.name, "reviewer");
        assert_eq!(a.description.as_deref(), Some("Reviews"));
        assert_eq!(a.extra["user-invocable"], Value::Bool(false));
        assert_eq!(a.body, "Review.");
        assert_eq!(profile_files(&p)["agents/reviewer.md"], text.as_bytes());
        assert!(!p.is_empty());
    }

    #[test]
    fn a_plugin_agents_dir_is_not_read() {
        // A plugin's agents/ holds Claude Code subagents, another format.
        let dir = tempfile::tempdir().unwrap();
        write(dir.path(), "rules/style.md", "Use tabs.\n");
        write(
            dir.path(),
            "agents/helper.md",
            "---\nname: helper\n---\nHelp.\n",
        );
        let p = load(dir.path()).unwrap();
        assert!(p.agents.is_empty());
        assert_eq!(p.rules.len(), 1);
    }

    #[test]
    fn rule_glob_aliases_read_the_same() {
        let want = vec!["src/**/*.ts".to_string(), "test/**/*.ts".to_string()];
        for text in [
            "---\nglobs: src/**/*.ts, test/**/*.ts\n---\nx\n",
            "---\nglobs:\n  - src/**/*.ts\n  - test/**/*.ts\n---\nx\n",
            "---\npaths:\n  - \"src/**/*.ts\"\n  - \"test/**/*.ts\"\n---\nx\n",
            "---\napplyTo: \"src/**/*.ts,test/**/*.ts\"\n---\nx\n",
        ] {
            let r = rule_from_markdown("r", text);
            assert_eq!(r.globs, want, "{text}");
            assert_eq!(r.always_apply, None);
        }
    }

    fn write(dir: &Path, rel: &str, text: &str) {
        let p = dir.join(rel);
        fs::create_dir_all(p.parent().unwrap()).unwrap();
        fs::write(p, text).unwrap();
    }

    #[test]
    fn plugins_load_without_a_manifest() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("My Plugin");
        write(&dir, "rules/plain.md", "Plain.\n");
        write(&dir, "commands/go.md", "Go.\n");
        write(&dir, "skills/s/SKILL.md", "---\ndescription: S\n---\nS.\n");
        write(&dir, "instructions.md", "Not part of a plugin.\n");
        let p = load(&dir).unwrap();
        assert_eq!(p.name, "my-plugin");
        assert!(
            p.warnings[0].contains("instructions.md is not read"),
            "{:?}",
            p.warnings
        );
        assert_eq!(p.rules.len(), 1);
        assert_eq!(p.commands.len(), 1);
        assert_eq!(p.skills.len(), 1);
        assert!(p.instructions.is_none() && p.mcp_servers.is_empty() && p.settings.is_empty());
        assert!(!dir.join(MANIFEST).exists());
        assert!(is_plugin(&dir));
        assert_eq!(load_named(&dir, "Repo.Name").unwrap().name, "repo.name");
        fs::remove_file(dir.join("instructions.md")).unwrap();

        // A plugin manifest's name wins; .claude-plugin/ before the root file.
        write(&dir, "plugin.json", r#"{"name": "root-name"}"#);
        assert_eq!(load(&dir).unwrap().name, "root-name");
        write(
            &dir,
            ".claude-plugin/plugin.json",
            r#"{"name": "claude-name", "description": "D"}"#,
        );
        let p = load_named(&dir, "hint").unwrap();
        assert_eq!(p.name, "claude-name");
        assert_eq!(p.description.as_deref(), Some("D"));
        assert!(p.warnings.is_empty());

        // A manifest that can't be used falls back to the directory, with a warning.
        write(&dir, ".claude-plugin/plugin.json", "not json");
        let p = load(&dir).unwrap();
        assert_eq!(p.name, "my-plugin");
        assert_eq!(p.warnings.len(), 1);
        write(&dir, ".claude-plugin/plugin.json", r#"{"name": " "}"#);
        let p = load(&dir).unwrap();
        assert_eq!(p.name, "my-plugin");
        assert!(p.warnings[0].contains("'name'"), "{:?}", p.warnings);
    }

    #[test]
    fn neither_profile_nor_plugin_says_both() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path();
        for setup in [
            None,
            Some(("README.md", "x")),
            Some(("rules/notes.txt", "x")),
            Some(("rules", "a file, not a directory")),
        ] {
            let _ = fs::remove_dir_all(dir.join("rules"));
            let _ = fs::remove_file(dir.join("rules"));
            if let Some((rel, text)) = setup {
                write(dir, rel, text);
            }
            let err = load(dir).unwrap_err().to_string();
            assert!(
                err.contains("kitbag init") && err.contains("rules/"),
                "{setup:?}: {err}"
            );
        }
        // An empty rules/ is still a plugin if its manifest names it.
        fs::remove_file(dir.join("rules")).unwrap();
        fs::create_dir(dir.join("rules")).unwrap();
        write(dir, "plugin.json", r#"{"name": "empty"}"#);
        assert_eq!(load(dir).unwrap().name, "empty");
    }

    #[test]
    fn profiles_with_a_manifest_load_as_before() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path();
        write(dir, MANIFEST, "kitbag = 1\nname = \"prof\"\n");
        write(dir, "instructions.md", "Be brief.\n");
        write(dir, "rules/r.md", "R.\n");
        write(dir, "plugin.json", r#"{"name": "ignored"}"#);
        let p = load_named(dir, "hint").unwrap();
        assert_eq!(p.name, "prof");
        assert_eq!(p.instructions.as_deref(), Some("Be brief."));
        assert!(!is_plugin(dir));
    }

    #[test]
    fn total_profile_size_is_capped() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join(MANIFEST), "kitbag = 1\nname = \"me\"\n").unwrap();
        fs::create_dir_all(dir.path().join("skills/big")).unwrap();
        fs::write(dir.path().join("skills/big/SKILL.md"), "Big.\n").unwrap();
        for i in 0..4 {
            fs::write(dir.path().join(format!("skills/big/{i}.bin")), [b'x'; 1000]).unwrap();
        }
        assert!(load_with_limit(dir.path(), 10_000, None).is_ok());
        let err = load_with_limit(dir.path(), 3_000, None)
            .unwrap_err()
            .to_string();
        assert!(
            err.contains("in total, the most a profile may hold"),
            "{err}"
        );
        const { assert!(MAX_PROFILE_BYTES >= 16 * MAX_FILE_BYTES) };
    }

    fn sample() -> Profile {
        let mut p = Profile::new("demo");
        p.description = Some("Demo profile".into());
        p.mcp_servers.insert(
            "github".into(),
            McpServer {
                command: Some("npx".into()),
                args: vec!["-y".into(), "@modelcontextprotocol/server-github".into()],
                env: [("GITHUB_TOKEN".to_string(), "${GITHUB_TOKEN}".to_string())]
                    .into_iter()
                    .collect(),
                ..Default::default()
            },
        );
        p.settings.insert(
            "claude-code".into(),
            serde_json::json!({"model": "opus", "permissions": {"allow": ["Bash(npm test:*)"]}})
                .as_object()
                .cloned()
                .unwrap(),
        );
        p.instructions = Some("Be concise.".into());
        p.rules.push(Rule {
            name: "ts".into(),
            globs: vec!["src/**/*.ts".into()],
            body: "Strict.".into(),
            ..Default::default()
        });
        p.commands.push(Command {
            name: "review".into(),
            description: Some("Review".into()),
            argument_hint: Some("[focus]".into()),
            extra: serde_json::json!({"allowed-tools": "Bash(git diff:*)"})
                .as_object()
                .cloned()
                .unwrap(),
            body: "Review $ARGUMENTS".into(),
            ..Default::default()
        });
        p.skills.push(Skill {
            name: "pdf".into(),
            description: Some("PDFs".into()),
            files: vec![
                SkillFile {
                    path: "SKILL.md".into(),
                    content: b"---\nname: pdf\ndescription: PDFs\n---\nUse it.\n".to_vec(),
                    executable: false,
                },
                SkillFile {
                    path: "bin/blob.bin".into(),
                    content: vec![0, 159, 146, 150],
                    executable: false,
                },
            ],
            ..Default::default()
        });
        p
    }

    #[test]
    fn save_and_load_round_trip() {
        let dir = tempfile::tempdir().unwrap();
        let p = sample();
        save(&p, dir.path(), false).unwrap();
        let back = load(dir.path()).unwrap();
        assert_eq!(back, p);
        // Refuses to clobber without force.
        assert!(save(&p, dir.path(), false).is_err());
        save(&p, dir.path(), true).unwrap();
    }

    #[test]
    fn force_only_replaces_profile_files() {
        let dir = tempfile::tempdir().unwrap();
        let mut p = sample();
        save(&p, dir.path(), false).unwrap();
        fs::write(dir.path().join("README.md"), "mine").unwrap();
        fs::write(dir.path().join("commands/notes.txt"), "mine too").unwrap();
        p.commands.clear();
        p.rules.clear();
        save(&p, dir.path(), true).unwrap();
        assert!(!dir.path().join("commands/review.md").exists());
        assert!(
            !dir.path().join("rules").exists(),
            "emptied directories are removed"
        );
        assert_eq!(
            fs::read_to_string(dir.path().join("README.md")).unwrap(),
            "mine"
        );
        assert!(dir.path().join("commands/notes.txt").exists());
        assert_eq!(load(dir.path()).unwrap(), p);
    }

    #[test]
    fn captures_into_a_fresh_repository() {
        let dir = tempfile::tempdir().unwrap();
        fs::create_dir_all(dir.path().join(".git")).unwrap();
        fs::write(dir.path().join("README.md"), "# dotagents").unwrap();
        save(&sample(), dir.path(), false).unwrap();
        assert_eq!(
            fs::read_to_string(dir.path().join("README.md")).unwrap(),
            "# dotagents"
        );
    }

    #[test]
    fn refuses_non_empty_non_profile_dirs() {
        let dir = tempfile::tempdir().unwrap();
        fs::create_dir_all(dir.path().join("commands")).unwrap();
        fs::write(dir.path().join("commands/deploy.sh"), "echo").unwrap();
        for force in [false, true] {
            let e = save(&sample(), dir.path(), force).unwrap_err();
            assert!(e.to_string().contains("not a kitbag profile"), "{e}");
        }
        assert!(dir.path().join("commands/deploy.sh").exists());
    }

    #[test]
    fn manifest_is_readable_toml() {
        let text = manifest_toml(&sample());
        assert!(text.contains("[mcp_servers.github]\ncommand = \"npx\""));
        assert!(text.contains("[settings.claude-code]\nmodel = \"opus\""));
    }

    #[test]
    fn settings_tables_take_the_alias() {
        let text = "kitbag = 1\nname = \"x\"\n[settings.claude]\nmodel = \"opus\"\n[settings.vim]\nx = 1\n";
        let p = parse_manifest(text, Path::new("k")).unwrap();
        assert_eq!(
            p.settings.keys().collect::<Vec<_>>(),
            ["claude-code", "vim"]
        );
        assert_eq!(p.settings["claude-code"]["model"], "opus");
        assert!(manifest_toml(&p).contains("[settings.claude-code]\nmodel = \"opus\""));

        let both = "kitbag = 1\nname = \"x\"\n[settings.claude-code]\nmodel = \"opus\"\n[settings.claude]\nmodel = \"sonnet\"\n";
        let e = parse_manifest(both, Path::new("k")).unwrap_err();
        assert_eq!(
            e.issues,
            [
                "both [settings.claude] and [settings.claude-code] are set; 'claude' is another name for 'claude-code', keep one"
            ],
            "{e}"
        );
    }

    #[test]
    fn validation_reports_every_issue() {
        let text = "kitbag = 1\nname = \"x\"\n[mcp_servers.bad]\nargs = [\"x\"]\n[mcp_servers.remote]\nurl = \"https://x\"\ncommand = \"y\"\ntransport = \"http\"\n";
        let e = parse_manifest(text, Path::new("kitbag.toml")).unwrap_err();
        assert_eq!(e.issues.len(), 2, "{e}");
        let e =
            parse_manifest("kitbag = 1\nname = \"x\"\nsurprise = 1\n", Path::new("k")).unwrap_err();
        assert!(e.to_string().contains("surprise"));
        let e = parse_manifest("kitbag = 9\nname = \"x\"\n", Path::new("k")).unwrap_err();
        assert!(e.to_string().contains("upgrade kitbag"));
    }

    #[test]
    fn refuses_to_save_secrets() {
        let mut p = Profile::new("x");
        p.instructions = Some("token ghp_FAKE0123456789abcdefghijklmnopqrstuv".into());
        let dir = tempfile::tempdir().unwrap();
        let e = save(&p, dir.path(), false).unwrap_err();
        assert!(
            e.to_string()
                .contains("instructions.md: looks like a secret (github-token)")
        );
        assert!(!dir.path().join(MANIFEST).exists());
    }

    #[test]
    fn secret_messages_read_as_english() {
        // Not "looks like a openai-key" / "a aws-access-key": the kind goes in
        // brackets after a fixed phrase, so the article is always right.
        let mut p = Profile::new("x");
        p.instructions = Some("key sk-FAKEFAKEFAKEFAKEFAKEFAKE0123 in here".into());
        let leaks = scan_for_secrets(&p);
        assert!(
            leaks.contains(&"instructions.md: looks like a secret (openai-key)".to_string()),
            "{leaks:?}"
        );
        p.instructions = Some("aws AKIAFAKEFAKEFAKEFAKE".into());
        let leaks = scan_for_secrets(&p);
        assert!(
            leaks.contains(&"instructions.md: looks like a secret (aws-access-key)".to_string()),
            "{leaks:?}"
        );
    }

    #[cfg(unix)]
    #[test]
    fn rejects_symlinks() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join(MANIFEST), "kitbag = 1\nname = \"x\"\n").unwrap();
        fs::create_dir_all(dir.path().join("skills/evil")).unwrap();
        fs::write(
            dir.path().join("skills/evil/SKILL.md"),
            "---\nname: evil\n---\n",
        )
        .unwrap();
        let outside = tempfile::tempdir().unwrap();
        fs::write(outside.path().join("id_rsa"), "PRIVATE").unwrap();
        std::os::unix::fs::symlink(
            outside.path().join("id_rsa"),
            dir.path().join("skills/evil/key"),
        )
        .unwrap();
        let e = load(dir.path()).unwrap_err();
        assert!(
            e.to_string().contains("symbolic links are not allowed"),
            "{e}"
        );
    }

    #[cfg(unix)]
    #[test]
    fn plugin_manifests_behind_symlinks_are_refused() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("plug");
        write(&dir, "rules/r.md", "R.\n");
        let outside = tempfile::tempdir().unwrap();
        write(outside.path(), "plugin.json", r#"{"name": "outside"}"#);
        // A symlinked .claude-plugin/ directory is not followed.
        std::os::unix::fs::symlink(outside.path(), dir.join(".claude-plugin")).unwrap();
        let e = load(&dir).unwrap_err().to_string();
        assert!(e.contains("symbolic links are not allowed"), "{e}");
        assert!(e.contains(".claude-plugin"), "{e}");
        // Nor is a symlinked root plugin.json.
        fs::remove_file(dir.join(".claude-plugin")).unwrap();
        std::os::unix::fs::symlink(outside.path().join("plugin.json"), dir.join("plugin.json"))
            .unwrap();
        let e = load(&dir).unwrap_err().to_string();
        assert!(e.contains("symbolic links are not allowed"), "{e}");
        fs::remove_file(dir.join("plugin.json")).unwrap();
        assert_eq!(load(&dir).unwrap().name, "plug");
    }

    #[test]
    fn item_targeting_loads_saves_and_is_taken_out_of_the_files_tools_get() {
        let dir = tempfile::tempdir().unwrap();
        let w = |rel: &str, text: &str| {
            let p = dir.path().join(rel);
            fs::create_dir_all(p.parent().unwrap()).unwrap();
            fs::write(p, text).unwrap();
        };
        w("kitbag.toml", "kitbag = 1\nname = \"x\"\n");
        w(
            "rules/r.md",
            "---\ndescription: R\ntargets: [claude, claude-code, cursor]\n---\nRule\n",
        );
        w(
            "commands/c.md",
            "---\ndescription: C\nmodel: m\ndisabled_on:\n  - codex\n---\nCmd\n",
        );
        w(
            "agents/a.md",
            "---\ndescription: A\nonly: cursor, codex\n---\nAgent\n",
        );
        w(
            "skills/s/SKILL.md",
            "---\nname: s\ntargets:\n  - codex\ndescription: S\n---\nSkill\n",
        );
        w(
            "skills/plain/SKILL.md",
            "---\nname: plain\ndescription: P\n---\nSkill\n",
        );
        let p = load(dir.path()).unwrap();
        assert!(p.warnings.is_empty(), "{:?}", p.warnings);
        assert_eq!(
            p.rules[0].targeting.targets,
            Some(vec!["claude-code".to_string(), "cursor".to_string()])
        );
        assert_eq!(
            p.commands[0].targeting.disabled_on,
            vec!["codex".to_string()]
        );
        // The keys are the profile's, so they are not passed on to tools.
        assert!(!p.commands[0].extra.contains_key("disabled_on"));
        assert_eq!(p.commands[0].extra["model"], "m");
        assert_eq!(
            p.agents[0].targeting.targets,
            Some(vec!["cursor".to_string(), "codex".to_string()])
        );
        assert!(!p.agents[0].extra.contains_key("only"));
        let skill = p.skills.iter().find(|s| s.name == "s").unwrap();
        assert_eq!(skill.targeting.targets, Some(vec!["codex".to_string()]));
        let main = String::from_utf8(skill.main_file().unwrap().content.clone()).unwrap();
        assert_eq!(main, "---\nname: s\ndescription: S\n---\nSkill\n");
        assert!(
            p.skills
                .iter()
                .find(|s| s.name == "plain")
                .unwrap()
                .targeting
                .is_empty()
        );
        assert!(!p.rules[0].targeting.applies_to("codex"));

        // Saved with the new names, where the loader finds them again.
        let out = tempfile::tempdir().unwrap();
        save(&p, out.path(), false).unwrap();
        let rule = fs::read_to_string(out.path().join("rules/r.md")).unwrap();
        assert!(
            rule.contains("targets:\n  - claude-code\n  - cursor\n"),
            "{rule}"
        );
        let agent = fs::read_to_string(out.path().join("agents/a.md")).unwrap();
        assert!(agent.contains("targets:\n  - cursor\n  - codex\n") && !agent.contains("only"));
        let again = load(out.path()).unwrap();
        assert_eq!(again.rules, p.rules);
        assert_eq!(again.commands, p.commands);
        assert_eq!(again.agents, p.agents);
        assert_eq!(again.skills, p.skills);
    }

    #[test]
    fn items_without_targeting_are_written_as_before() {
        let mut p = sample();
        p.rules[0].targeting = Targeting::default();
        let files = profile_files(&p);
        for r in &p.rules {
            assert_eq!(
                files[&format!("rules/{}.md", r.name)],
                rule_to_markdown(r).into_bytes()
            );
        }
        for c in &p.commands {
            assert_eq!(
                files[&format!("commands/{}.md", c.name)],
                command_to_markdown(c, true).into_bytes()
            );
        }
        for s in &p.skills {
            for f in &s.files {
                assert_eq!(files[&format!("skills/{}/{}", s.name, f.path)], f.content);
            }
        }
    }

    #[test]
    fn item_targeting_errors_and_warnings() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("kitbag.toml"), "kitbag = 1\nname = \"x\"\n").unwrap();
        fs::create_dir_all(dir.path().join("rules")).unwrap();
        let rule = |text: &str| fs::write(dir.path().join("rules/r.md"), text).unwrap();
        rule("---\ntargets: [cursor]\ndisabled_on: [codex]\n---\nB\n");
        let e = load(dir.path()).unwrap_err().to_string();
        assert!(e.contains("rules/r.md: set either 'targets'"), "{e}");
        rule("---\nonly: [cursor]\ntargets: [cursor]\n---\nB\n");
        let e = load(dir.path()).unwrap_err().to_string();
        assert!(
            e.contains("rules/r.md: both 'only' and 'targets' are set"),
            "{e}"
        );
        rule("---\ntargets: []\n---\nB\n");
        let e = load(dir.path()).unwrap_err().to_string();
        assert!(
            e.contains("'targets' is empty, so the rule would apply nowhere"),
            "{e}"
        );
        rule("---\ntargets: 3\n---\nB\n");
        let e = load(dir.path()).unwrap_err().to_string();
        assert!(e.contains("'targets' must be a list of tool names"), "{e}");
        rule("---\ndisabled_on: [vscode]\nonly: []\n---\nB\n");
        let p = load(dir.path()).unwrap();
        assert_eq!(p.warnings.len(), 2, "{:?}", p.warnings);
        assert!(p.warnings[0].starts_with("rules/r.md: 'only' is empty"));
        assert!(p.warnings[1].contains("unknown target 'vscode' in 'disabled_on' is ignored"));
        assert!(p.rules[0].targeting.applies_to("cursor"));
        // Another tool's `only` that isn't a list is left alone.
        fs::create_dir_all(dir.path().join("commands")).unwrap();
        fs::write(
            dir.path().join("commands/c.md"),
            "---\nonly: true\n---\nB\n",
        )
        .unwrap();
        let p = load(dir.path()).unwrap();
        assert_eq!(p.commands[0].extra["only"], true);
        assert!(p.commands[0].targeting.is_empty());
    }

    #[test]
    fn keeps_item_targeting_on_recapture() {
        let mut old = Profile::new("x");
        old.rules.push(Rule {
            name: "r".into(),
            targeting: Targeting {
                targets: Some(vec!["cursor".into()]),
                ..Default::default()
            },
            ..Default::default()
        });
        old.agents.push(Agent {
            name: "gone".into(),
            targeting: Targeting {
                disabled_on: vec!["codex".into()],
                ..Default::default()
            },
            ..Default::default()
        });
        old.agents.push(Agent {
            name: "plain".into(),
            ..Default::default()
        });
        let mut new = Profile::new("x");
        new.rules.push(Rule {
            name: "r".into(),
            ..Default::default()
        });
        assert_eq!(keep_item_targeting(&mut new, &old), vec!["rule r"]);
        assert_eq!(
            new.rules[0].targeting.targets,
            Some(vec!["cursor".to_string()])
        );
        // Only an item with targeting is worth keeping when no tool has it.
        assert_eq!(keep_uncaptured_items(&mut new, &old), vec!["agent gone"]);
        assert_eq!(new.agents.len(), 1);
    }

    #[test]
    fn keeps_targeting_on_recapture() {
        let mut old = sample();
        let gh = old.mcp_servers.get_mut("github").unwrap();
        gh.targets = Some(vec!["claude-code".into()]);
        let mut new = sample();
        new.mcp_servers.insert(
            "other".into(),
            McpServer {
                command: Some("x".into()),
                ..Default::default()
            },
        );
        assert_eq!(keep_targeting(&mut new, &old), vec!["github"]);
        assert_eq!(
            new.mcp_servers["github"].targets,
            Some(vec!["claude-code".to_string()])
        );
        assert_eq!(new.mcp_servers["other"].targets, None);
        // A profile without targeting renders exactly as before.
        assert!(!manifest_toml(&sample()).contains("targets"));
    }

    #[test]
    fn keeps_servers_a_recapture_did_not_find() {
        let mut old = sample();
        old.mcp_servers.insert(
            "gone".into(),
            McpServer {
                command: Some("gone-mcp".into()),
                disabled_on: vec!["cursor".into()],
                ..Default::default()
            },
        );
        let mut new = sample();
        let before = new.mcp_servers["github"].clone();
        assert_eq!(keep_uncaptured_servers(&mut new, &old), vec!["gone"]);
        assert_eq!(new.mcp_servers["gone"], old.mcp_servers["gone"]);
        // Captured servers are left as captured.
        assert_eq!(new.mcp_servers["github"], before);
        assert!(keep_uncaptured_servers(&mut new, &old).is_empty());
    }

    #[test]
    fn targeting_fields_load_and_save() {
        let text = "kitbag = 1\nname = \"x\"\n[mcp_servers.a]\ncommand = \"a\"\ntargets = [\"claude-code\"]\n[mcp_servers.b]\ncommand = \"b\"\ndisabled_on = [\"cursor\"]\n[mcp_servers.c]\ncommand = \"c\"\nonly = [\"codex\"]\n[mcp_servers.d]\ncommand = \"d\"\nexcept = [\"gemini-cli\"]\n";
        let p = parse_manifest(text, Path::new("k")).unwrap();
        assert_eq!(p.mcp_servers["a"].targets, Some(vec!["claude-code".into()]));
        assert_eq!(p.mcp_servers["b"].disabled_on, vec!["cursor".to_string()]);
        // The older names still load, and are written with the new ones.
        assert_eq!(p.mcp_servers["c"].targets, Some(vec!["codex".into()]));
        assert_eq!(
            p.mcp_servers["d"].disabled_on,
            vec!["gemini-cli".to_string()]
        );
        let out = manifest_toml(&p);
        assert!(
            out.contains("[mcp_servers.a]\ncommand = \"a\"\ntargets = [\"claude-code\"]"),
            "{out}"
        );
        assert!(out.contains("disabled_on = [\"cursor\"]"), "{out}");
        assert_eq!(parse_manifest(&out, Path::new("k")).unwrap(), p);
    }

    #[test]
    fn targeting_errors() {
        let both = "kitbag = 1\nname = \"x\"\n[mcp_servers.a]\ncommand = \"a\"\ntargets = [\"codex\"]\ndisabled_on = [\"cursor\"]\n";
        let e = parse_manifest(both, Path::new("k")).unwrap_err();
        assert!(
            e.to_string()
                .contains("mcp_servers.a: set either 'targets'"),
            "{e}"
        );
        let both_names = "kitbag = 1\nname = \"x\"\n[mcp_servers.a]\ncommand = \"a\"\nonly = [\"codex\"]\ntargets = [\"codex\"]\n";
        let e = parse_manifest(both_names, Path::new("k")).unwrap_err();
        assert!(
            e.to_string().contains(
                "mcp_servers.a: both 'only' and 'targets' are set; 'only' is the older name of 'targets'"
            ),
            "{e}"
        );
        let empty = "kitbag = 1\nname = \"x\"\n[mcp_servers.a]\ncommand = \"a\"\ntargets = []\n";
        let e = parse_manifest(empty, Path::new("k")).unwrap_err();
        assert!(e.to_string().contains("'targets' is empty"), "{e}");
    }

    #[test]
    fn targeting_warnings() {
        // A target from a newer kitbag loads, with a warning, and matches nothing.
        let unknown = "kitbag = 1\nname = \"x\"\n[mcp_servers.a]\ncommand = \"a\"\ntargets = [\"vim\", \"codex\"]\n[mcp_servers.b]\ncommand = \"b\"\nexcept = [\"vscode\"]\n";
        let p = parse_manifest(unknown, Path::new("k")).unwrap();
        assert_eq!(p.warnings.len(), 2, "{:?}", p.warnings);
        assert!(
            p.warnings[0]
                .starts_with("mcp_servers.a: unknown target 'vim' in 'targets' is ignored"),
            "{:?}",
            p.warnings
        );
        assert!(p.warnings[0].contains(
            "valid targets: claude-code, cursor, codex, gemini-cli, claude-desktop, antigravity, opencode, copilot, grok-build, windsurf, zed, hermes, amp, cline, roo, continue"
        ));
        assert!(
            p.warnings[1].starts_with("mcp_servers.b: unknown target 'vscode' in 'disabled_on'")
        );
        assert!(p.mcp_servers["a"].applies_to("codex"));
        assert!(!p.mcp_servers["a"].applies_to("cursor"));
        assert!(p.mcp_servers["b"].applies_to("cursor"));
        // The name is kept, so re-saving the profile doesn't lose it.
        assert!(manifest_toml(&p).contains("targets = [\"vim\", \"codex\"]"));

        // The `claude` alias is no unknown target: it is read as `claude-code`.
        let alias = "kitbag = 1\nname = \"x\"\n[mcp_servers.a]\ncommand = \"a\"\ntargets = [\"claude\"]\n[mcp_servers.b]\ncommand = \"b\"\nexcept = [\"claude\"]\n";
        let p = parse_manifest(alias, Path::new("k")).unwrap();
        assert!(p.warnings.is_empty(), "{:?}", p.warnings);
        assert_eq!(
            p.mcp_servers["a"].targets,
            Some(vec!["claude-code".to_string()])
        );
        assert_eq!(p.mcp_servers["b"].disabled_on, vec!["claude-code"]);
        assert!(!p.mcp_servers["b"].applies_to("claude-code"));
        assert!(p.mcp_servers["b"].applies_to("claude-desktop"));
        assert!(manifest_toml(&p).contains("targets = [\"claude-code\"]"));

        // A target named under both spellings is kept once, at its first place.
        let twice = "kitbag = 1\nname = \"x\"\n[mcp_servers.a]\ncommand = \"a\"\ntargets = [\"codex\", \"claude\", \"cursor\", \"claude-code\", \"codex\"]\n[mcp_servers.b]\ncommand = \"b\"\ndisabled_on = [\"claude-code\", \"claude\"]\n";
        let p = parse_manifest(twice, Path::new("k")).unwrap();
        assert_eq!(
            p.mcp_servers["a"].targets,
            Some(vec!["codex".into(), "claude-code".into(), "cursor".into()])
        );
        assert_eq!(p.mcp_servers["b"].disabled_on, vec!["claude-code"]);
        assert!(manifest_toml(&p).contains("targets = [\"codex\", \"claude-code\", \"cursor\"]"));

        // An empty 'only' keeps its old meaning: no restriction.
        let empty_only = "kitbag = 1\nname = \"x\"\n[mcp_servers.a]\ncommand = \"a\"\nonly = []\n";
        let p = parse_manifest(empty_only, Path::new("k")).unwrap();
        assert_eq!(p.mcp_servers["a"].targets, None);
        assert!(p.mcp_servers["a"].applies_to("cursor"));
        assert!(
            p.warnings[0].contains("'only' is empty"),
            "{:?}",
            p.warnings
        );
        assert!(!manifest_toml(&p).contains("only"));
    }

    #[test]
    fn lists_required_env() {
        let req = required_env(&sample());
        assert_eq!(
            req["GITHUB_TOKEN"],
            vec!["mcp_servers.github.env.GITHUB_TOKEN"]
        );
    }
}
