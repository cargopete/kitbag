//! Plugins, carried by reference.
//!
//! A plugin is somebody else's code (it can bundle hooks, MCP servers,
//! commands, agents and skills), so kitbag never copies one. A profile holds
//! only what the tool itself stores to find and enable it, in a `[plugins.<target>]`
//! section of `kitbag.toml`:
//!
//! ```toml
//! [plugins.claude-code.marketplaces.acme-tools]
//! source = { source = "github", repo = "acme/tools", ref = "v1.2.0" }
//! autoUpdate = false
//!
//! [plugins.claude-code.enabled."formatter@acme-tools"]
//! version = "1.2.0"      # what was installed when it was captured; not enforced
//! # targets / disabled_on work as for an MCP server
//! ```
//!
//! The ids are the tool's own (`name@marketplace`, as Claude Code keys
//! `enabledPlugins`; an npm package spec for opencode) and are never
//! translated to another tool's. Claude Code and opencode are carried
//! ([`TARGETS`]); the other systems are documented skips, see below.
//!
//! Claude Code, from its docs (checked 2026-11-27):
//!
//! * `enabledPlugins` (`"name@marketplace": true`) is written to
//!   `~/.claude/settings.json` (user scope), `.claude/settings.json` (project
//!   scope, committed) or `.claude/settings.local.json` (local scope).
//! * `extraKnownMarketplaces` (`"<name>": {"source": {...}, "autoUpdate": bool}`)
//!   declares marketplaces; `claude plugin marketplace add` writes the entry to
//!   the user settings. A `source` is `github` (`repo`, `ref`, `path`), `git`
//!   (`url`, `ref`, `path`), `url` (`url`, `headers`), `file` or `directory`
//!   (`path`) or an inline `settings` marketplace.
//! * Claude Code itself fetches a declared marketplace and the enabled plugins
//!   that aren't cached yet, when it next starts, if the user settings (or an
//!   untracked local file) enable them. A plugin enabled only in the project's
//!   `.claude/settings.json` is not installed: each person runs
//!   `claude plugin install <name>@<marketplace> --scope project`, and the
//!   project's `extraKnownMarketplaces` apply only after the folder is trusted.
//!
//! kitbag only writes those two settings keys. It never downloads or runs a
//! plugin; the plan says what Claude Code will fetch and what is left to do.
//!
//! opencode, from its docs (checked 2026-11-27): the `plugins` array of
//! `opencode.json(c)` (`plugin` in opencode 1) lists npm packages, which
//! opencode installs itself at startup (`opencode-x`, `opencode-x@1.2.0`,
//! `@scope/x`). kitbag carries those specs as `[plugins.opencode.enabled."<spec>"]`
//! and appends missing ones to the array of the config file it writes MCP
//! servers to. Not carried: local paths and `file:` entries (code on this
//! machine), `-name` entries (a plugin turned off), the `options` of the
//! object/tuple forms (they can hold keys) and anything that isn't an npm spec
//! (a git spec's syntax is not documented: UNVERIFIED). opencode has no
//! marketplaces, and `plugins/` folders are the plugin code itself, so they are
//! never copied.
//!
//! Skipped, with the docs read on 2026-11-27 (all would need plugin code
//! copied, or a layout the docs don't give):
//!
//! * Antigravity: plugins are folders under `~/.gemini/config/plugins/<name>/`
//!   (or `.agents/plugins/`), unpacked by `agy plugin install`, and enabled
//!   and disabled with `agy plugin enable|disable`. The docs name no
//!   `plugins.json`, no setting that lists plugins or marketplaces, and no
//!   file holding enablement, so there is nothing to write by reference.
//!   <https://antigravity.google/docs/plugins>, <https://antigravity.google/docs/build-plugins/>
//! * Gemini CLI: extensions are copied into `~/.gemini/extensions/<name>/` by
//!   `gemini extensions install`; the docs name no file that records an
//!   install source or enablement (UNVERIFIED: `extension-enablement.json`).
//!   <https://geminicli.com/docs/extensions/reference/>
//! * Claude Desktop: the only filesystem layout its docs name is the
//!   organization plugins directory (`/Library/Application Support/Claude/org-plugins/`
//!   on macOS, `C:\Program Files\Claude\org-plugins\` on Windows, none on Linux),
//!   which an administrator owns (root or administrators only) and which
//!   holds the plugins themselves. User plugins are added in the app and live
//!   in its local data directory, whose path the docs don't give.
//!   <https://claude.com/docs/third-party/claude-desktop/extensions>
//!
//! * <https://code.claude.com/docs/en/plugins/install>
//! * <https://code.claude.com/docs/en/plugins/loading>
//! * <https://code.claude.com/docs/en/plugins/org>
//! * <https://opencode.ai/docs/plugins/>, <https://opencode.ai/docs/migrate-v1/>

use std::collections::BTreeMap;
use std::path::Path;

use anyhow::{Result, bail};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};

use crate::adapters::{ApplyOptions, Captured, Env, Scope, Work};
use crate::edit::json::JsonDoc;
use crate::model::{Profile, is_valid_name, targeting_issues, targeting_skip_reason};

/// The targets that have a plugin section.
pub const TARGETS: &[&str] = &["claude-code", "opencode"];

/// Origins in a plugin id (`name@origin`) that are not marketplaces: a plugin
/// loaded for one session, from a skills directory or synced from claude.ai.
const RESERVED_ORIGINS: &[&str] = &["inline", "skills-dir", "synced"];

/// The settings keys kitbag writes.
const PLUGINS_KEY: &str = "enabledPlugins";
const MARKETPLACES_KEY: &str = "extraKnownMarketplaces";

/// The plugins and marketplaces of one target.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PluginSet {
    /// By the name Claude Code registers them under.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub marketplaces: BTreeMap<String, Marketplace>,
    /// By id (`name@marketplace`).
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub enabled: BTreeMap<String, Plugin>,
}

impl PluginSet {
    pub fn is_empty(&self) -> bool {
        self.marketplaces.is_empty() && self.enabled.is_empty()
    }
}

/// A marketplace: Claude Code's own `extraKnownMarketplaces` entry.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Marketplace {
    /// Claude Code's `source` object, as it stores it (see the module docs).
    pub source: Map<String, Value>,
    #[serde(
        rename = "autoUpdate",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub auto_update: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub targets: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub disabled_on: Vec<String>,
}

/// An enabled plugin.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Plugin {
    /// The version `installed_plugins.json` recorded when it was captured.
    /// Claude Code has no setting to pin a plugin to one, so this is shown in
    /// the plan and not enforced (pin the marketplace with `ref` instead).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub targets: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub disabled_on: Vec<String>,
}

fn applies(target: &str, targets: &Option<Vec<String>>, disabled_on: &[String]) -> bool {
    targeting_skip_reason(targets, disabled_on, target).is_none()
}

impl Marketplace {
    pub fn applies_to(&self, target: &str) -> bool {
        applies(target, &self.targets, &self.disabled_on)
    }

    /// The `extraKnownMarketplaces` entry Claude Code reads.
    fn entry(&self) -> Value {
        let mut m = Map::new();
        m.insert("source".into(), Value::Object(self.source.clone()));
        if let Some(a) = self.auto_update {
            m.insert("autoUpdate".into(), Value::Bool(a));
        }
        Value::Object(m)
    }
}

impl Plugin {
    pub fn applies_to(&self, target: &str) -> bool {
        applies(target, &self.targets, &self.disabled_on)
    }
}

/// `github acme/tools, ref v1.2.0`: where a marketplace comes from, for plans.
pub fn describe_source(src: &Map<String, Value>) -> String {
    let get = |k: &str| src.get(k).and_then(Value::as_str);
    let kind = get("source").unwrap_or("?");
    let place = get("repo")
        .or_else(|| get("url"))
        .or_else(|| get("path"))
        .unwrap_or("?");
    let mut out = format!("{kind} {place}");
    if let Some(r) = get("ref") {
        out.push_str(&format!(", ref {r}"));
    }
    if matches!(kind, "github" | "git")
        && let Some(p) = get("path")
    {
        out.push_str(&format!(", path {p}"));
    }
    out
}

/// What a plan or a withheld-list calls a marketplace.
pub fn describe_marketplace(name: &str, m: &Marketplace) -> String {
    format!(
        "plugin marketplace '{name}' ({})",
        describe_source(&m.source)
    )
}

/// What a plan or a withheld-list calls a plugin.
pub fn describe_plugin(id: &str, p: &Plugin) -> String {
    match &p.version {
        Some(v) => format!("plugin '{id}' (captured at version {v})"),
        None => format!("plugin '{id}'"),
    }
}

fn has_control(s: &str) -> bool {
    s.chars().any(char::is_control)
}

/// What is wrong with a marketplace `source` object.
fn source_issues(src: &Map<String, Value>) -> Vec<String> {
    let mut out = Vec::new();
    let kind = src.get("source").and_then(Value::as_str).unwrap_or("");
    let (allowed, required): (&[&str], &str) = match kind {
        "github" => (&["repo", "ref", "path"], "repo"),
        "git" => (&["url", "ref", "path"], "url"),
        "url" => (&["url"], "url"),
        "file" | "directory" => (&["path"], "path"),
        _ => {
            out.push(format!(
                "source.source must be one of github, git, url, file or directory, got {}",
                if kind.is_empty() {
                    "nothing".to_string()
                } else {
                    format!("'{}'", kind.escape_default())
                }
            ));
            return out;
        }
    };
    for (k, v) in src {
        if k == "source" {
            continue;
        }
        if !allowed.contains(&k.as_str()) {
            let why = if k == "headers" {
                " (request headers can hold credentials, which kitbag doesn't store)"
            } else {
                ""
            };
            out.push(format!(
                "source.{k} is not a key of a '{kind}' source{why}; they are {}",
                allowed.join(", ")
            ));
            continue;
        }
        match v.as_str() {
            Some(s) if !s.trim().is_empty() && !has_control(s) => {}
            _ => out.push(format!("source.{k} must be a non-empty string")),
        }
    }
    match src.get(required).and_then(Value::as_str) {
        None => out.push(format!("a '{kind}' source needs {required}")),
        Some(v) if kind == "github" && !is_repo(v) => {
            out.push(format!(
                "source.repo '{}' must be owner/repo",
                v.escape_default()
            ));
        }
        Some(v)
            if matches!(kind, "git" | "url") && crate::source::strip_url_credentials(v) != v =>
        {
            out.push("source.url has credentials in it; kitbag doesn't store secrets, so use a URL without them and let git's credential helper sign in".to_string());
        }
        Some(_) => {}
    }
    out
}

fn is_repo(v: &str) -> bool {
    let ok = |s: &str| {
        !s.is_empty()
            && s.chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'))
    };
    v.split_once('/').is_some_and(|(o, r)| ok(o) && ok(r))
}

fn name_issue(what: &str, name: &str) -> Option<String> {
    if !is_valid_name(name) {
        return Some(format!(
            "{what} '{}': names may only contain letters, digits, '.', '_' and '-'",
            name.escape_default()
        ));
    }
    if RESERVED_ORIGINS.contains(&name) {
        return Some(format!(
            "{what} '{name}' is reserved by Claude Code; it isn't a marketplace"
        ));
    }
    None
}

/// Split `name@marketplace`.
pub fn split_id(id: &str) -> Option<(&str, &str)> {
    let (name, market) = id.split_once('@')?;
    (!name.is_empty() && !market.is_empty() && !market.contains('@')).then_some((name, market))
}

/// Check the `[plugins]` section of a manifest and normalise its target ids.
/// Returns (errors, warnings).
pub fn validate(plugins: &mut BTreeMap<String, PluginSet>) -> (Vec<String>, Vec<String>) {
    let (mut issues, mut warnings) = (Vec::new(), Vec::new());
    let valid = crate::adapters::ids();
    let mut by_id: BTreeMap<String, PluginSet> = BTreeMap::new();
    for (key, mut set) in std::mem::take(plugins) {
        let target = crate::adapters::canonical_id(&key).to_string();
        if !valid.contains(&target.as_str()) {
            issues.push(format!(
                "plugins.{}: unknown target; valid targets: {}",
                key.escape_default(),
                valid.join(", ")
            ));
            continue;
        }
        if by_id.contains_key(&target) {
            issues.push(format!(
                "both [plugins.{target}] and its alias are set; keep one"
            ));
            continue;
        }
        if !TARGETS.contains(&target.as_str()) {
            warnings.push(format!(
                "plugins.{target}: kitbag carries plugins for {} only so far; this section is ignored",
                TARGETS.join(", ")
            ));
        }
        let at = format!("plugins.{target}");
        if target == "opencode" && !set.marketplaces.is_empty() {
            issues.push(format!(
                "{at}: opencode has no plugin marketplaces; list its npm packages under [{at}.enabled]"
            ));
        }
        for (name, m) in &mut set.marketplaces {
            let label = format!("{at}.marketplaces.{}", name.escape_default());
            issues.extend(name_issue("marketplace", name).map(|i| format!("{at}: {i}")));
            issues.extend(
                source_issues(&m.source)
                    .into_iter()
                    .map(|i| format!("{label}: {i}")),
            );
            targeting(
                &label,
                &mut m.targets,
                &mut m.disabled_on,
                "marketplace",
                &valid,
                &mut issues,
                &mut warnings,
            );
        }
        for (id, p) in &mut set.enabled {
            let label = format!("{at}.enabled.{}", id.escape_default());
            if target == "opencode" {
                issues.extend(opencode_spec_issue(id).map(|i| format!("{label}: {i}")));
                if p.version.is_some() {
                    issues.push(format!(
                        "{label}: version is not used for opencode; pin the package in its id (name@1.2.0)"
                    ));
                }
                targeting(
                    &label,
                    &mut p.targets,
                    &mut p.disabled_on,
                    "plugin",
                    &valid,
                    &mut issues,
                    &mut warnings,
                );
                continue;
            }
            match split_id(id) {
                Some((name, market)) => {
                    if !is_valid_name(name) {
                        issues.push(format!("{label}: the plugin name '{}' may only contain letters, digits, '.', '_' and '-'", name.escape_default()));
                    }
                    issues.extend(name_issue("marketplace", market).map(|i| format!("{label}: {i}")));
                }
                None => issues.push(format!(
                    "{label}: a plugin id is <name>@<marketplace>, as Claude Code writes it in enabledPlugins"
                )),
            }
            if let Some(v) = &p.version
                && (v.trim().is_empty() || has_control(v) || v.len() > 128)
            {
                issues.push(format!("{label}: version must be a short plain string"));
            }
            targeting(
                &label,
                &mut p.targets,
                &mut p.disabled_on,
                "plugin",
                &valid,
                &mut issues,
                &mut warnings,
            );
        }
        by_id.insert(target, set);
    }
    *plugins = by_id;
    (issues, warnings)
}

fn targeting(
    label: &str,
    targets: &mut Option<Vec<String>>,
    disabled_on: &mut Vec<String>,
    noun: &str,
    valid: &[&str],
    issues: &mut Vec<String>,
    warnings: &mut Vec<String>,
) {
    for ids in targets.iter_mut().chain(std::iter::once(&mut *disabled_on)) {
        let mut seen = std::collections::BTreeSet::new();
        ids.retain_mut(|id| {
            *id = crate::adapters::canonical_id(id).to_string();
            seen.insert(id.clone())
        });
    }
    let (e, w) = targeting_issues(targets, disabled_on, valid, noun);
    issues.extend(e.into_iter().map(|i| format!("{label}: {i}")));
    warnings.extend(w.into_iter().map(|i| format!("{label}: {i}")));
}

// ---------------------------------------------------------------------------
// Capture

/// The version `installed_plugins.json` records for `id` at user scope. The
/// file's layout isn't spelled out in the docs beyond "each install with its
/// `scope`, `installPath` and `version`", so this reads an entry that is an
/// object, or a list of them, under `plugins.<id>`.
fn installed_version(installed: Option<&Value>, id: &str) -> Option<String> {
    let entry = installed?.get("plugins")?.get(id)?;
    let list: Vec<&Value> = match entry {
        Value::Array(a) => a.iter().collect(),
        Value::Object(_) => vec![entry],
        _ => return None,
    };
    list.into_iter()
        .find(|e| is_user_install(e))
        .and_then(|e| e.get("version"))
        .and_then(Value::as_str)
        .filter(|v| !v.trim().is_empty() && !has_control(v) && v.len() <= 128)
        .map(str::to_string)
}

/// The ids `installed_plugins.json` lists with a user-scope install.
fn installed_user_ids(installed: Option<&Value>) -> Vec<String> {
    let Some(Value::Object(plugins)) = installed.and_then(|v| v.get("plugins")) else {
        return Vec::new();
    };
    plugins
        .iter()
        .filter(|(_, entry)| match entry {
            Value::Array(a) => a.iter().any(is_user_install),
            other => is_user_install(other),
        })
        .map(|(id, _)| id.clone())
        .collect()
}

fn is_user_install(v: &Value) -> bool {
    v.get("scope").and_then(Value::as_str) == Some("user")
}

/// Read Claude Code's declared marketplaces and enabled plugins from a
/// settings document into the profile, by reference.
pub fn capture(
    cap: &mut Captured,
    target: &str,
    env: &Env,
    settings: &Map<String, Value>,
    installed: Option<&Value>,
) {
    let mut set = PluginSet::default();
    if let Some(Value::Object(declared)) = settings.get(MARKETPLACES_KEY) {
        for (name, entry) in declared {
            match marketplace_from_settings(name, entry, cap) {
                Ok(m) => {
                    set.marketplaces.insert(name.clone(), m);
                }
                Err(why) => cap.info(
                    target,
                    format!(
                        "plugin marketplace '{}' not captured: {why}",
                        name.escape_default()
                    ),
                ),
            }
        }
    }
    let mut off = 0;
    let mut other_origin = Vec::new();
    if let Some(Value::Object(enabled)) = settings.get(PLUGINS_KEY) {
        for (id, v) in enabled {
            let Some((name, market)) = split_id(id) else {
                cap.info(
                    target,
                    format!(
                        "plugin '{}' not captured: not a <name>@<marketplace> id",
                        id.escape_default()
                    ),
                );
                continue;
            };
            if RESERVED_ORIGINS.contains(&market) {
                other_origin.push(id.clone());
                continue;
            }
            if !is_valid_name(name) || !is_valid_name(market) {
                cap.info(
                    target,
                    format!(
                        "plugin '{}' not captured: its name has characters kitbag doesn't write to a profile",
                        id.escape_default()
                    ),
                );
                continue;
            }
            match v {
                Value::Bool(true) => {
                    let version = if env.scope == Scope::User {
                        installed_version(installed, id)
                    } else {
                        None
                    };
                    set.enabled.insert(
                        id.clone(),
                        Plugin {
                            version,
                            ..Plugin::default()
                        },
                    );
                }
                Value::Bool(false) => off += 1,
                _ => cap.info(
                    target,
                    format!(
                        "plugin '{}' not captured: its enabledPlugins value is not true or false",
                        id.escape_default()
                    ),
                ),
            }
        }
    }
    if !other_origin.is_empty() {
        cap.info(
            target,
            format!(
                "not carried, since they don't come from a marketplace: {}",
                other_origin
                    .iter()
                    .map(|i| i.escape_default().to_string())
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
        );
    }
    if off > 0 {
        cap.info(
            target,
            format!("{off} plugin(s) set to false in enabledPlugins are not carried"),
        );
    }
    for id in set.enabled.keys() {
        let market = split_id(id).map_or("", |(_, m)| m);
        if !set.marketplaces.contains_key(market) && market != "claude-plugins-official" {
            cap.info(
                target,
                format!(
                    "plugin '{id}': its marketplace '{market}' is not declared in this settings file, so only the plugin is carried; add the marketplace on the other machine with claude plugin marketplace add"
                ),
            );
        }
    }
    // Installs the settings file doesn't enable: the docs have the install
    // write `enabledPlugins`, so these were turned off or came from elsewhere.
    if env.scope == Scope::User {
        let missing = installed_user_ids(installed)
            .into_iter()
            .filter(|id| !set.enabled.contains_key(id) && !other_origin.contains(id))
            .filter(|id| settings.get(PLUGINS_KEY).and_then(|e| e.get(id)).is_none())
            .count();
        if missing > 0 {
            cap.info(
                target,
                format!(
                    "{missing} plugin(s) in plugins/installed_plugins.json are not enabled in settings.json, so they are not carried"
                ),
            );
        }
    }
    if set.is_empty() {
        return;
    }
    cap.info(
        target,
        format!(
            "{} marketplace(s) and {} plugin(s) captured by reference (settings.json); plugin files in plugins/ are not copied",
            set.marketplaces.len(),
            set.enabled.len()
        ),
    );
    cap.profile.plugins.insert(target.to_string(), set);
}

fn marketplace_from_settings(
    name: &str,
    entry: &Value,
    cap: &Captured,
) -> std::result::Result<Marketplace, String> {
    let Some(src) = entry.get("source").and_then(Value::as_object) else {
        return Err("it has no source".into());
    };
    let kind = src.get("source").and_then(Value::as_str).unwrap_or("");
    if !matches!(kind, "github" | "git" | "url" | "file" | "directory") {
        return Err(format!(
            "a '{}' source isn't one kitbag carries",
            kind.escape_default()
        ));
    }
    let mut src = src.clone();
    if src.contains_key("headers") {
        return Err(
            "it sends request headers, which can hold credentials and aren't carried".into(),
        );
    }
    if let Some((home, os)) = &cap.home
        && matches!(kind, "file" | "directory")
        && let Some(Value::String(p)) = src.get_mut("path")
    {
        *p = crate::portable::to_home_relative(p, home, *os);
    }
    let issues = source_issues(&src);
    if !issues.is_empty() {
        return Err(issues.join("; "));
    }
    if let Some(i) = name_issue("marketplace", name) {
        return Err(i);
    }
    Ok(Marketplace {
        source: src,
        auto_update: entry.get("autoUpdate").and_then(Value::as_bool),
        targets: None,
        disabled_on: Vec::new(),
    })
}

// ---------------------------------------------------------------------------
// Apply

/// Add the profile's marketplaces and plugins for `target` to the Claude Code
/// settings file at `path`: `extraKnownMarketplaces` entries and
/// `enabledPlugins` keys, nothing else. An entry that is already there is left
/// alone; a differing one is replaced (not with `--no-overwrite`).
pub fn apply(
    profile: &Profile,
    env: &Env,
    opts: &ApplyOptions,
    work: &mut Work,
    path: &Path,
    target: &str,
) -> Result<()> {
    let Some(set) = profile.plugins.get(target) else {
        return Ok(());
    };
    let mut markets = Vec::new();
    let mut plugins = Vec::new();
    for (name, m) in &set.marketplaces {
        match targeting_skip_reason(&m.targets, &m.disabled_on, target) {
            None => markets.push((name, m)),
            Some(why) => work.info(
                target,
                format!(
                    "{} skipped: disabled in profile ({why})",
                    describe_marketplace(name, m)
                ),
            ),
        }
    }
    for (id, p) in &set.enabled {
        match targeting_skip_reason(&p.targets, &p.disabled_on, target) {
            None => plugins.push((id, p)),
            Some(why) => work.info(
                target,
                format!(
                    "{} skipped: disabled in profile ({why})",
                    describe_plugin(id, p)
                ),
            ),
        }
    }
    if markets.is_empty() && plugins.is_empty() {
        return Ok(());
    }
    let text = work.fs.read(path)?;
    let mut doc = JsonDoc::parse(text.as_deref(), path)?;
    doc.check_unique_keys(&[], &[PLUGINS_KEY, MARKETPLACES_KEY])?;
    let current = doc.value();
    for key in [PLUGINS_KEY, MARKETPLACES_KEY] {
        if current.get(key).is_some_and(|v| !v.is_object()) {
            bail!(
                "{key} in {} is not an object; fix it by hand, kitbag won't overwrite it",
                path.display()
            );
        }
    }
    let mut changed = false;
    let mut added_markets = Vec::new();
    let mut added_plugins = Vec::new();
    for (name, m) in markets {
        let want = m.entry();
        let have = current
            .get(MARKETPLACES_KEY)
            .and_then(|v| v.get(name.as_str()));
        let same = have.is_some_and(|h| {
            h.get("source") == want.get("source")
                && (m.auto_update.is_none() || h.get("autoUpdate") == want.get("autoUpdate"))
        });
        if same {
            continue;
        }
        if have.is_some() && !opts.overwrite {
            work.warn(
                target,
                format!(
                    "kept the existing {MARKETPLACES_KEY} entry '{name}' in {} because --no-overwrite was given; the profile has {}",
                    path.display(),
                    describe_source(&m.source)
                ),
            );
            continue;
        }
        match have {
            // Replace the source whole (its fields don't merge); other keys
            // of the entry stay.
            Some(_) => {
                doc.set(
                    &[MARKETPLACES_KEY, name, "source"],
                    &Value::Object(m.source.clone()),
                );
                if let Some(a) = m.auto_update {
                    doc.set(&[MARKETPLACES_KEY, name, "autoUpdate"], &Value::Bool(a));
                }
            }
            None => doc.set(&[MARKETPLACES_KEY, name], &want),
        }
        changed = true;
        added_markets.push(describe_marketplace(name, m));
    }
    for (id, p) in plugins {
        let have = current.get(PLUGINS_KEY).and_then(|v| v.get(id.as_str()));
        if have == Some(&json!(true)) {
            continue;
        }
        if have.is_some() && !opts.overwrite {
            work.warn(
                target,
                format!(
                    "kept {PLUGINS_KEY} '{id}' as it is in {} because --no-overwrite was given",
                    path.display()
                ),
            );
            continue;
        }
        doc.set(&[PLUGINS_KEY, id], &json!(true));
        changed = true;
        added_plugins.push(describe_plugin(id, p));
    }
    if !changed {
        return Ok(());
    }
    work.fs.write(path, doc.render())?;
    let file = path
        .file_name()
        .map_or_else(String::new, |f| f.to_string_lossy().into_owned());
    for d in added_markets.iter().chain(&added_plugins) {
        work.info(target, format!("{d} added to {file}"));
    }
    match env.scope {
        Scope::User => work.warn(
            target,
            "kitbag doesn't download plugins. Claude Code fetches the marketplaces and plugins above itself the next time it starts (plugins can bundle hooks, MCP servers and commands); to install now, run `claude plugin marketplace add` and `claude plugin install <name>@<marketplace>`",
        ),
        Scope::Project => work.warn(
            target,
            "still to do by hand: a plugin enabled in .claude/settings.json isn't installed by that. Each person runs `claude plugin install <name>@<marketplace> --scope project` once per clone, and the project's marketplaces are registered only after the folder is trusted",
        ),
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// opencode

/// The config keys that list plugins: opencode 2's and opencode 1's.
const OPENCODE_KEYS: [&str; 2] = ["plugins", "plugin"];

fn npm_name_ok(name: &str) -> bool {
    let seg = |s: &str| {
        !s.is_empty()
            && !s.starts_with(['.', '_', '-'])
            && s.chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'))
    };
    match name.strip_prefix('@') {
        Some(scoped) => scoped
            .split_once('/')
            .is_some_and(|(scope, n)| seg(scope) && seg(n)),
        None => seg(name),
    }
}

/// An npm package spec (`name`, `name@1.2.0`, `@scope/name`, `@scope/name@^1`)
/// as (package name, version).
fn npm_parts(spec: &str) -> Option<(&str, Option<&str>)> {
    if spec.len() > 300 {
        return None;
    }
    let from = usize::from(spec.starts_with('@'));
    let (name, version) = match spec[from..].find('@') {
        Some(i) => (&spec[..from + i], Some(&spec[from + i + 1..])),
        None => (spec, None),
    };
    if !npm_name_ok(name) {
        return None;
    }
    if let Some(v) = version
        && (v.is_empty()
            || !v.chars().all(|c| {
                c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-' | '+' | '^' | '~' | '*')
            }))
    {
        return None;
    }
    Some((name, version))
}

/// What is wrong with an opencode plugin id, which is an npm package spec.
pub fn opencode_spec_issue(spec: &str) -> Option<String> {
    if npm_parts(spec).is_some() {
        return None;
    }
    Some(format!(
        "'{}' is not an npm package spec (name, name@1.2.0 or @scope/name); local paths and git specs aren't carried",
        spec.escape_default()
    ))
}

/// The spec an entry of an opencode plugin array names, whatever its form: a
/// string, opencode 2's `{"package": ..}` or opencode 1's `[spec, options]`.
/// The `bool` says it carries options.
fn opencode_entry(v: &Value) -> Option<(&str, bool)> {
    match v {
        Value::String(s) => Some((s, false)),
        Value::Object(o) => o
            .get("package")
            .and_then(Value::as_str)
            .map(|s| (s, o.get("options").is_some_and(|x| !x.is_null()))),
        Value::Array(a) => a.first().and_then(Value::as_str).map(|s| (s, a.len() > 1)),
        _ => None,
    }
}

/// Read the npm plugins of one opencode config document into the profile, by
/// reference. `file` is how the plan names the config file.
pub fn capture_opencode(cap: &mut Captured, doc: &Value, file: &str) {
    let target = "opencode";
    let mut found = Vec::new();
    let mut other = Vec::new();
    let (mut off, mut with_options) = (0, 0);
    for key in OPENCODE_KEYS {
        match doc.get(key) {
            None | Some(Value::Null) => continue,
            Some(Value::Array(list)) => {
                for entry in list {
                    let Some((spec, has_options)) = opencode_entry(entry) else {
                        other.push(entry.to_string());
                        continue;
                    };
                    if spec.starts_with('-') {
                        off += 1;
                    } else if npm_parts(spec).is_some() {
                        if has_options {
                            with_options += 1;
                        }
                        found.push(spec.to_string());
                    } else {
                        other.push(spec.to_string());
                    }
                }
            }
            Some(_) => cap.info(
                target,
                format!("'{key}' in {file} is not a list, so no plugins were captured from it"),
            ),
        }
    }
    if !other.is_empty() {
        let shown: Vec<String> = other
            .iter()
            .take(5)
            .map(|s| s.escape_default().to_string())
            .collect();
        cap.info(
            target,
            format!(
                "{} plugin entr{} in {file} not carried (local paths and other specs are code on this machine, not a package to install): {}{}",
                other.len(),
                if other.len() == 1 { "y" } else { "ies" },
                shown.join(", "),
                if other.len() > shown.len() { ", ..." } else { "" }
            ),
        );
    }
    if off > 0 {
        cap.info(
            target,
            format!("{off} plugin(s) turned off with a '-' prefix in {file} are not carried"),
        );
    }
    if with_options > 0 {
        cap.info(
            target,
            format!("the options of {with_options} plugin(s) in {file} are not carried (they can hold keys); set them on the other machine"),
        );
    }
    if found.is_empty() {
        return;
    }
    let n = found.len();
    let set = cap.profile.plugins.entry(target.to_string()).or_default();
    for spec in found {
        set.enabled.entry(spec).or_default();
    }
    cap.info(
        target,
        format!(
            "{n} plugin(s) captured by reference from {file}; plugin files (plugins/) are not copied"
        ),
    );
}

/// Add the profile's npm plugins to the plugin array of the opencode config at
/// `path`. The array is the one the file already has (`plugins`, or opencode
/// 1's `plugin`); a new one is `plugins`, or `plugin` when `legacy` says the
/// file is opencode 1's. A package that is already listed in any form is left
/// alone, unless the profile pins a different version of it (replaced, but not
/// with `--no-overwrite`, and not when it carries options).
pub fn apply_opencode(
    profile: &Profile,
    opts: &ApplyOptions,
    work: &mut Work,
    path: &Path,
    legacy: bool,
) -> Result<()> {
    let target = "opencode";
    let Some(set) = profile.plugins.get(target) else {
        return Ok(());
    };
    let mut wanted = Vec::new();
    for (spec, p) in &set.enabled {
        match targeting_skip_reason(&p.targets, &p.disabled_on, target) {
            None => wanted.push((spec, p)),
            Some(why) => work.info(
                target,
                format!(
                    "{} skipped: disabled in profile ({why})",
                    describe_plugin(spec, p)
                ),
            ),
        }
    }
    if wanted.is_empty() {
        return Ok(());
    }
    let text = work.fs.read(path)?;
    let mut doc = JsonDoc::parse(text.as_deref(), path)?;
    doc.check_unique_keys(&[], &OPENCODE_KEYS)?;
    let current = doc.value();
    for key in OPENCODE_KEYS {
        if current.get(key).is_some_and(|v| !v.is_array()) {
            bail!(
                "{key} in {} is not a list; fix it by hand, kitbag won't overwrite it",
                path.display()
            );
        }
    }
    let list = |key: &str| -> Vec<Value> {
        current
            .get(key)
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default()
    };
    let mut lists: Vec<(&str, Vec<Value>)> = OPENCODE_KEYS.iter().map(|k| (*k, list(k))).collect();
    let new_key = if current.get("plugins").is_some() {
        "plugins"
    } else if current.get("plugin").is_some() || legacy {
        "plugin"
    } else {
        "plugins"
    };
    let file = path
        .file_name()
        .map_or_else(String::new, |f| f.to_string_lossy().into_owned());
    let mut dirty: Vec<&str> = Vec::new();
    let mut added = Vec::new();
    for (spec, p) in wanted {
        let Some((name, version)) = npm_parts(spec) else {
            continue;
        };
        // Where this package is listed already: (list, position, spec, options).
        let mut have = None;
        let mut turned_off = false;
        for (key, entries) in &lists {
            for (i, e) in entries.iter().enumerate() {
                let Some((s, opts_)) = opencode_entry(e) else {
                    continue;
                };
                if s.strip_prefix('-').and_then(npm_parts).map(|(n, _)| n) == Some(name) {
                    turned_off = true;
                }
                if npm_parts(s).map(|(n, _)| n) == Some(name) && have.is_none() {
                    have = Some((*key, i, s.to_string(), opts_, e.is_string()));
                }
            }
        }
        if turned_off {
            work.warn(
                target,
                format!(
                    "{} not added to {file}: it is turned off there with a '-' entry; remove that entry to enable it",
                    describe_plugin(spec, p)
                ),
            );
            continue;
        }
        match have {
            None => {
                if let Some((_, entries)) = lists.iter_mut().find(|(k, _)| *k == new_key) {
                    entries.push(json!(spec));
                }
                if !dirty.contains(&new_key) {
                    dirty.push(new_key);
                }
                added.push(describe_plugin(spec, p));
            }
            Some((key, i, have_spec, has_options, is_string)) => {
                let same = version.is_none() || have_spec == *spec;
                if same {
                    continue;
                }
                if !opts.overwrite || !is_string || has_options {
                    let why = if !opts.overwrite {
                        "because --no-overwrite was given".to_string()
                    } else {
                        "it is written with options, which kitbag leaves to you".to_string()
                    };
                    work.warn(
                        target,
                        format!(
                            "kept '{}' in {file}, {why}; the profile has '{spec}'",
                            have_spec.escape_default()
                        ),
                    );
                    continue;
                }
                if let Some((_, entries)) = lists.iter_mut().find(|(k, _)| *k == key) {
                    entries[i] = json!(spec);
                }
                if !dirty.contains(&key) {
                    dirty.push(key);
                }
                added.push(describe_plugin(spec, p));
            }
        }
    }
    if dirty.is_empty() {
        return Ok(());
    }
    for key in dirty {
        if let Some((_, entries)) = lists.iter().find(|(k, _)| *k == key) {
            doc.set(&[key], &Value::Array(entries.clone()));
        }
    }
    work.fs.write(path, doc.render())?;
    for d in &added {
        work.info(target, format!("{d} added to {file}"));
    }
    work.warn(
        target,
        "kitbag doesn't install plugins. opencode installs the npm packages above itself the next time it starts, and they run inside opencode with its permissions",
    );
    Ok(())
}

// ---------------------------------------------------------------------------
// Policy

/// Take the plugins and marketplaces out of a profile from a git source that
/// isn't trusted: plugins can bundle hooks, MCP servers and commands, and
/// Claude Code fetches them itself. Returns each as the plan names it, with
/// the target it was for.
pub fn drop_untrusted(profile: &mut Profile) -> Vec<(String, String)> {
    let mut out = Vec::new();
    for (target, set) in std::mem::take(&mut profile.plugins) {
        if !TARGETS.contains(&target.as_str()) {
            continue;
        }
        for (name, m) in &set.marketplaces {
            out.push((target.clone(), describe_marketplace(name, m)));
        }
        for (id, p) in &set.enabled {
            out.push((target.clone(), describe_plugin(id, p)));
        }
    }
    out
}

/// Every marketplace and plugin the profile holds, as `target: description`.
pub fn list(profile: &Profile) -> Vec<String> {
    let mut out = Vec::new();
    for (target, set) in &profile.plugins {
        for (name, m) in &set.marketplaces {
            out.push(format!("{target}: {}", describe_marketplace(name, m)));
        }
        for (id, p) in &set.enabled {
            out.push(format!("{target}: {}", describe_plugin(id, p)));
        }
    }
    out
}

/// What `inspect` lists for review.
pub fn review_lines(profile: &Profile) -> Vec<String> {
    let mut out = Vec::new();
    for (target, set) in &profile.plugins {
        if !TARGETS.contains(&target.as_str()) {
            continue;
        }
        for (name, m) in &set.marketplaces {
            out.push(format!(
                "{} for {target}: {} is cloned or fetched by the tool itself",
                describe_marketplace(name, m),
                "the marketplace",
            ));
        }
        for (id, p) in &set.enabled {
            let what = if target == "opencode" {
                "an npm package opencode installs itself and runs inside opencode, with its permissions"
            } else {
                "runs whatever the plugin bundles (hooks, MCP servers, commands) once the tool installs it"
            };
            out.push(format!("{} for {target}: {what}", describe_plugin(id, p)));
        }
    }
    out
}

/// Carry the targeting of `old`'s marketplaces and plugins over to the
/// same-named ones of a fresh capture. Returns them, as `plugin <id>`.
pub fn keep_targeting(new: &mut Profile, old: &Profile) -> Vec<String> {
    let mut kept = Vec::new();
    for (target, set) in &mut new.plugins {
        let Some(o) = old.plugins.get(target) else {
            continue;
        };
        for (name, m) in &mut set.marketplaces {
            if let Some(om) = o.marketplaces.get(name)
                && (om.targets.is_some() || !om.disabled_on.is_empty())
            {
                m.targets.clone_from(&om.targets);
                m.disabled_on.clone_from(&om.disabled_on);
                kept.push(format!("plugin marketplace {name}"));
            }
        }
        for (id, p) in &mut set.enabled {
            if let Some(op) = o.enabled.get(id)
                && (op.targets.is_some() || !op.disabled_on.is_empty())
            {
                p.targets.clone_from(&op.targets);
                p.disabled_on.clone_from(&op.disabled_on);
                kept.push(format!("plugin {id}"));
            }
        }
    }
    kept
}

/// Expand `~` in local marketplace paths for this machine.
pub fn expand_paths(
    plugins: &mut BTreeMap<String, PluginSet>,
    home: &Path,
    os: crate::adapters::Os,
) {
    for m in plugins
        .values_mut()
        .flat_map(|s| s.marketplaces.values_mut())
    {
        if matches!(
            m.source.get("source").and_then(Value::as_str),
            Some("file" | "directory")
        ) && let Some(Value::String(p)) = m.source.get_mut("path")
        {
            *p = crate::portable::expand_home(p, home, os);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn src(v: Value) -> Map<String, Value> {
        v.as_object().unwrap().clone()
    }

    #[test]
    fn ids_are_name_at_marketplace() {
        assert_eq!(split_id("fmt@acme"), Some(("fmt", "acme")));
        assert_eq!(split_id("fmt"), None);
        assert_eq!(split_id("@acme"), None);
        assert_eq!(split_id("fmt@"), None);
        assert_eq!(split_id("a@b@c"), None);
    }

    #[test]
    fn sources_are_checked_by_type() {
        let ok = |v: Value| source_issues(&src(v)).is_empty();
        assert!(ok(
            json!({"source": "github", "repo": "acme/tools", "ref": "v1"})
        ));
        assert!(ok(
            json!({"source": "git", "url": "https://git.example/x.git"})
        ));
        assert!(ok(
            json!({"source": "url", "url": "https://example.com/m.json"})
        ));
        assert!(ok(json!({"source": "directory", "path": "~/m"})));
        assert!(!ok(json!({"source": "github"})));
        assert!(!ok(json!({"source": "github", "repo": "nope"})));
        assert!(!ok(
            json!({"source": "url", "url": "https://a/b", "headers": {"X": "y"}})
        ));
        assert!(!ok(
            json!({"source": "git", "url": "https://u:tok@git.example/x.git"})
        ));
        assert!(!ok(json!({"source": "npm", "package": "x"})));
        assert!(!ok(json!({"source": "directory", "path": "a\nb"})));
    }

    #[test]
    fn opencode_specs_are_npm_packages_only() {
        for ok in [
            "opencode-x",
            "opencode-x@1.2.0",
            "@acme/opencode-plugin",
            "@acme/p@^1.2.0",
            "x@latest",
        ] {
            assert!(npm_parts(ok).is_some(), "{ok}");
        }
        assert_eq!(npm_parts("@acme/p@1.0"), Some(("@acme/p", Some("1.0"))));
        for bad in [
            "",
            "./plugins/local",
            "../x",
            "/abs/x",
            "~/x",
            "file:///x",
            "C:\\x",
            "-x",
            "@acme",
            "@acme/",
            "a/b",
            "x@",
            "x@github:u/r",
            "git+https://h/r.git",
            "x y",
            ".hidden",
        ] {
            assert!(npm_parts(bad).is_none(), "{bad}");
        }
    }

    #[test]
    fn opencode_entries_come_in_three_forms() {
        assert_eq!(opencode_entry(&json!("a@1")), Some(("a@1", false)));
        assert_eq!(
            opencode_entry(&json!({"package": "a", "options": {"k": 1}})),
            Some(("a", true))
        );
        assert_eq!(opencode_entry(&json!({"package": "a"})), Some(("a", false)));
        assert_eq!(opencode_entry(&json!(["a", {"k": 1}])), Some(("a", true)));
        assert_eq!(opencode_entry(&json!(3)), None);
    }

    #[test]
    fn describing_a_source_names_the_place_and_pin() {
        let s = src(json!({"source": "github", "repo": "acme/tools", "ref": "v1.2.0"}));
        assert_eq!(describe_source(&s), "github acme/tools, ref v1.2.0");
    }

    #[test]
    fn version_comes_from_the_user_scope_install() {
        let v = json!({"plugins": {
            "a@m": [{"scope": "project", "version": "9"}, {"scope": "user", "version": "1.0.0"}],
            "b@m": {"scope": "user", "version": "2.0.0"},
            "c@m": [{"scope": "project", "version": "3"}],
        }});
        assert_eq!(installed_version(Some(&v), "a@m").as_deref(), Some("1.0.0"));
        assert_eq!(installed_version(Some(&v), "b@m").as_deref(), Some("2.0.0"));
        assert_eq!(installed_version(Some(&v), "c@m"), None);
        assert_eq!(installed_version(None, "a@m"), None);
    }
}
