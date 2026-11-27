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
//! translated to another tool's. Claude Code, Copilot CLI, Codex, opencode,
//! Antigravity, Zed and Grok Build are carried ([`TARGETS`]); the other systems
//! are documented skips, see below.
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
//! Antigravity, from its docs (checked 2026-11-27): the `plugins.json`
//! discovery manifest, in `~/.gemini/config/` (user) or `.agents/` (workspace),
//! lists folders to load: `{"entries": [{"path": .., "include_only": [..],
//! "exclude": [..]}]}`. Antigravity doesn't expand `~`, resolves a relative
//! path from `~/.gemini/config/` in the personal manifest and from the project
//! in a workspace one, and (changelog) loads a workspace entry only from inside
//! the workspace or repository. kitbag carries those entries, with the folder's
//! path as the id (`~/...` for the home directory), and writes only `entries`.
//! It never copies, installs or enables a plugin; Antigravity keeps the enabled
//! state itself (the changelog says in `config.json`, whose keys are not
//! documented, so not written). The folders `agy plugin install` makes in
//! `plugins/` have no recorded source and are the plugin code, so they are
//! counted and left out.
//! <https://antigravity.google/docs/customization-discovery/>,
//! <https://antigravity.google/docs/plugins>
//!
//! Skipped, with the docs read on 2026-11-27 (all would need plugin code
//! copied, or a layout the docs don't give):
//!
//! * Gemini CLI: extensions are copied into `~/.gemini/extensions/<name>/` by
//!   `gemini extensions install`; the docs name no file that records an
//!   install source or enablement (UNVERIFIED: `extension-enablement.json`) and
//!   no `settings.json` key. The only by-reference route is the `gemini
//!   extensions link <path>` command (a symlink to a directory on that machine),
//!   which is a command and not a layout.
//!   <https://geminicli.com/docs/extensions/reference/>
//! * Claude Desktop: the only filesystem layout its docs name for plugins you
//!   could set up is the organization plugins directory
//!   (`/Library/Application Support/Claude/org-plugins/` on macOS,
//!   `C:\Program Files\Claude\org-plugins\` on Windows, none on Linux),
//!   which an administrator owns (root or administrators only) and which holds
//!   the plugins themselves. User plugins are added in the app and live under
//!   `local-agent-mode-sessions/.../cowork_plugins/` in its data directory (the
//!   docs elide the middle of the path), as plugin code; no file lists one by
//!   reference, and marketplaces are admin policy or set in the app.
//!   <https://claude.com/docs/third-party/claude-desktop/extensions>,
//!   <https://claude.com/docs/third-party/claude-desktop/data-storage>
//! * Amp: plugins are files or folders in `plugins/` directories (or hosted
//!   repositories); no manifest lists them. Cline: SDK code, loaded by
//!   `extensions` / `pluginPaths` or `cline plugin install`; no settings file.
//!
//! * Cursor: plugins are installed from the app's Customize page and team
//!   marketplaces are set in the dashboard; `~/.cursor/plugins/local` holds the
//!   plugin code. No file or key lists one by reference.
//!   <https://cursor.com/docs/plugins>, <https://cursor.com/docs/reference/plugins>
//! * Hermes Agent: `hermes plugins install` clones into `~/.hermes/plugins/`
//!   and records the source in `.install-metadata.json` inside the install;
//!   `plugins.enabled` lists names of installed plugins and `hermes-pack.yaml` is
//!   read only by `hermes plugins pack install`. No layout without the install.
//!   <https://hermes-agent.nousresearch.com/docs/user-guide/features/plugins>
//! * Windsurf (MCP "plugins" are MCP servers, the JetBrains plugin is an editor
//!   integration), Roo Code (its marketplace installs MCP servers and modes) and
//!   Continue (hub blocks are configuration): no plugin system to carry.
//!
//! The same pages, read on 2026-11-27, for the systems that are carried:
//!
//! * Copilot CLI: `enabledPlugins` (`name@marketplace`: bool) and
//!   `extraKnownMarketplaces` (`source` of `github` (`repo`, `ref`, `path`), `git`
//!   (`url`, `ref`, `path`) or `directory` (`path`), and `autoUpdate`, honoured
//!   only in user settings) in `~/.copilot/settings.json` (`$COPILOT_HOME`) and
//!   the repository's `.github/copilot/settings.json`. The same shapes as Claude
//!   Code's, so [`capture`] and [`apply`] serve both. Built-in marketplace:
//!   `awesome-copilot`. VS Code reads the repository file too (workspace plugin
//!   recommendations and enablement). Not carried yet (#12): VS Code's own user
//!   settings `chat.plugins.marketplaces` and `chat.pluginLocations`
//!   (Experimental; <https://code.visualstudio.com/docs/agents/reference/agent-plugin-reference>).
//!   <https://docs.github.com/en/copilot/reference/copilot-cli-reference/cli-config-dir-reference>,
//!   <https://docs.github.com/en/copilot/reference/enterprise-managed-settings-reference>
//! * Codex: `[marketplaces.<name>]` (`source_type` `git` | `local`, `source`,
//!   `ref`, `sparse_paths`) and `[plugins."<name>@<marketplace>"]` (`enabled`) in
//!   `config.toml`, in user, system, cloud-managed or trusted-project config. The
//!   profile keeps Codex's own keys in the marketplace's `source` table. When
//!   Codex fetches a configured marketplace by itself is UNVERIFIED (its docs
//!   say it can install or refresh configured plugins "during marketplace
//!   refresh").
//!   <https://learn.chatgpt.com/docs/config-file/config-reference>,
//!   <https://developers.openai.com/plugins/build/plugins>
//! * Zed: `auto_install_extensions` (`{"id": true | false}`) in `settings.json`;
//!   written at user scope only (a project's `.zed/settings.json` is not
//!   documented for it: UNVERIFIED).
//!   <https://zed.dev/docs/reference/all-settings>,
//!   <https://zed.dev/docs/extensions/installing-extensions>
//! * Grok Build: extra plugin folders under `[plugins] paths` of
//!   `~/.grok/config.toml`. Whether `~` is expanded, what a relative path is
//!   relative to, and the shape of `[[marketplace.sources]]` are not documented
//!   (UNVERIFIED), so kitbag writes absolute folders at user scope only and no
//!   marketplace sources. Grok Build reads Claude Code's marketplaces and plugins
//!   itself.
//!   <https://docs.x.ai/build/features/skills-plugins-marketplaces>,
//!   <https://docs.x.ai/build/settings/reference>
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
use toml_edit::Item;

use crate::adapters::{ApplyOptions, Captured, Env, Scope, Work};
use crate::edit::json::JsonDoc;
use crate::model::{Profile, is_valid_name, targeting_issues, targeting_skip_reason};

/// The targets that have a plugin section.
pub const TARGETS: &[&str] = &[
    "claude-code",
    "copilot",
    "codex",
    "opencode",
    "antigravity",
    "zed",
    "grok-build",
];

/// Targets whose plugins are `name@marketplace` ids with marketplaces.
fn has_marketplaces(target: &str) -> bool {
    matches!(target, "claude-code" | "copilot" | "codex")
}

/// Targets whose plugin ids are folders on this machine.
pub fn lists_folders(target: &str) -> bool {
    matches!(target, "antigravity" | "grok-build")
}

/// The marketplace source kinds a `name@marketplace` target's settings take.
fn source_kinds(target: &str) -> &'static [&'static str] {
    match target {
        "copilot" => &["github", "git", "directory"],
        _ => &["github", "git", "url", "file", "directory"],
    }
}

/// The marketplace each tool ships built in, which a settings file needn't declare.
fn builtin_marketplace(target: &str) -> &'static str {
    match target {
        "copilot" => "awesome-copilot",
        "codex" => "openai-curated",
        _ => "claude-plugins-official",
    }
}

/// How to register a marketplace by hand, for the plan.
fn add_marketplace_hint(target: &str) -> &'static str {
    match target {
        "copilot" => "copilot plugin marketplace add",
        "codex" => "codex plugin marketplace add",
        _ => "claude plugin marketplace add",
    }
}

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
    /// Antigravity only: the `include_only` of the `plugins.json` entry.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub include_only: Vec<String>,
    /// Antigravity only: the `exclude` of the `plugins.json` entry.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub exclude: Vec<String>,
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
    // Codex's own shape: `source_type = "git" | "local"` and `source`.
    if let Some(kind) = get("source_type") {
        let mut out = format!("{kind} {}", get("source").unwrap_or("?"));
        if let Some(r) = get("ref") {
            out.push_str(&format!(", ref {r}"));
        }
        return out;
    }
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

/// What a plan, a withheld-list or `inspect` calls a plugin of `target`: for
/// Antigravity the id is the path of a plugin folder.
pub fn describe(target: &str, id: &str, p: &Plugin) -> String {
    if lists_folders(target) {
        format!("plugin folder '{}'", id.escape_default())
    } else if target == "zed" {
        format!("Zed extension '{}'", id.escape_default())
    } else {
        describe_plugin(id, p)
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

/// What is wrong with a marketplace source in a `target` section: the shapes
/// differ (Claude Code's and Copilot CLI's `source` objects, Codex's
/// `source_type` / `source`).
fn marketplace_issues(target: &str, src: &Map<String, Value>) -> Vec<String> {
    if target == "codex" {
        return codex_source_issues(src);
    }
    let mut out = source_issues(src);
    let kind = src.get("source").and_then(Value::as_str).unwrap_or("");
    if out.is_empty() && !source_kinds(target).contains(&kind) {
        out.push(format!(
            "a '{kind}' source isn't one {target} reads in its settings; they are {}",
            source_kinds(target).join(", ")
        ));
    }
    out
}

/// What is wrong with a Codex `[marketplaces.<name>]` source (`source_type`,
/// `source`, `ref`, `sparse_paths`).
fn codex_source_issues(src: &Map<String, Value>) -> Vec<String> {
    let mut out = Vec::new();
    let kind = src.get("source_type").and_then(Value::as_str).unwrap_or("");
    if !matches!(kind, "git" | "local") {
        out.push("source_type must be git or local".to_string());
        return out;
    }
    let plain = |v: &Value| {
        v.as_str()
            .is_some_and(|s| !s.trim().is_empty() && !has_control(s) && s.len() <= 1024)
    };
    for (k, v) in src {
        match k.as_str() {
            "source_type" => {}
            "source" | "ref" => {
                if !plain(v) {
                    out.push(format!("{k} must be a non-empty string"));
                }
            }
            "sparse_paths" if kind == "git" => {
                if !v
                    .as_array()
                    .is_some_and(|a| !a.is_empty() && a.iter().all(plain))
                {
                    out.push("sparse_paths must be a list of paths".to_string());
                }
            }
            other => out.push(format!(
                "{other} is not a key of a '{kind}' marketplace; they are source_type, source{}",
                if kind == "git" {
                    ", ref and sparse_paths"
                } else {
                    ""
                }
            )),
        }
    }
    match src.get("source").and_then(Value::as_str) {
        None => out.push("a marketplace needs a source".to_string()),
        Some(v) if kind == "local" && !(is_absolute_path(v) || v.starts_with("~/")) => out.push(
            "a local source must be an absolute path, or ~/... for a folder in your home directory"
                .to_string(),
        ),
        Some(v) if kind == "git" && crate::source::strip_url_credentials(v) != v => out.push(
            "source has credentials in it; kitbag doesn't store secrets, so use a URL without them and let git's credential helper sign in"
                .to_string(),
        ),
        Some(_) => {}
    }
    out
}

/// What is wrong with a Grok Build plugin folder: it has to be a path that
/// means the same wherever it is read, so absolute or `~/...`.
fn grok_path_issue(path: &str) -> Option<String> {
    if path.trim().is_empty() || has_control(path) || path.len() > 1024 {
        return Some("a plugin folder path must be a short, plain, non-empty path".into());
    }
    if !(is_absolute_path(path) || path.starts_with("~/")) {
        return Some(format!(
            "'{}' must be an absolute path, or ~/... for a folder in your home directory (Grok Build's docs don't say what a relative path is read from)",
            path.escape_default()
        ));
    }
    None
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
            "{what} '{name}' is reserved; it isn't a marketplace"
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
        if !has_marketplaces(&target) && !set.marketplaces.is_empty() {
            issues.push(format!(
                "{at}: {}",
                match target.as_str() {
                    "opencode" =>
                        "opencode has no plugin marketplaces; list its npm packages under [plugins.opencode.enabled]",
                    "antigravity" =>
                        "Antigravity has no marketplace setting kitbag can write; list local plugin folders under [plugins.antigravity.enabled]",
                    "zed" =>
                        "Zed installs extensions from its gallery; list them under [plugins.zed.enabled]",
                    _ =>
                        "Grok Build's marketplace sources have no documented layout kitbag can write; list local plugin folders under [plugins.grok-build.enabled]",
                }
            ));
        }
        for (name, m) in &mut set.marketplaces {
            let label = format!("{at}.marketplaces.{}", name.escape_default());
            issues.extend(name_issue("marketplace", name).map(|i| format!("{at}: {i}")));
            issues.extend(
                marketplace_issues(&target, &m.source)
                    .into_iter()
                    .map(|i| format!("{label}: {i}")),
            );
            if m.auto_update.is_some() && !matches!(target.as_str(), "claude-code" | "copilot") {
                issues.push(format!(
                    "{label}: autoUpdate is a Claude Code and Copilot CLI setting, not used for {target}"
                ));
            }
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
            if target != "antigravity" && (!p.include_only.is_empty() || !p.exclude.is_empty()) {
                issues.push(format!(
                    "{label}: include_only and exclude are Antigravity plugins.json fields, not used for {target}"
                ));
            }
            if target != "claude-code" && p.version.is_some() {
                issues.push(format!(
                    "{label}: version is not used for {target}{}",
                    match target.as_str() {
                        "opencode" => "; pin the package in its id (name@1.2.0)",
                        "antigravity" | "grok-build" => "; a plugin folder has no version",
                        "codex" | "copilot" => "; pin the marketplace with its ref instead",
                        _ => "",
                    }
                ));
            }
            match target.as_str() {
                "antigravity" => {
                    issues.extend(antigravity_path_issue(id).map(|i| format!("{label}: {i}")));
                    for (key, list) in [("include_only", &p.include_only), ("exclude", &p.exclude)]
                    {
                        if list
                            .iter()
                            .any(|v| v.trim().is_empty() || has_control(v) || v.len() > 256)
                        {
                            issues.push(format!(
                                "{label}: {key} must be short plain names (a folder name or a relative path)"
                            ));
                        }
                    }
                }
                "grok-build" => {
                    issues.extend(grok_path_issue(id).map(|i| format!("{label}: {i}")));
                }
                "opencode" => {
                    issues.extend(opencode_spec_issue(id).map(|i| format!("{label}: {i}")));
                }
                "zed" => {
                    if !is_valid_name(id) || id.len() > 128 {
                        issues.push(format!(
                            "{label}: a Zed extension id may only contain letters, digits, '.', '_' and '-'"
                        ));
                    }
                }
                _ => {
                    match split_id(id) {
                        Some((name, market)) => {
                            if !is_valid_name(name) {
                                issues.push(format!("{label}: the plugin name '{}' may only contain letters, digits, '.', '_' and '-'", name.escape_default()));
                            }
                            issues.extend(
                                name_issue("marketplace", market).map(|i| format!("{label}: {i}")),
                            );
                        }
                        None => issues.push(format!(
                            "{label}: a plugin id is <name>@<marketplace>, as {target} writes it"
                        )),
                    }
                    if let Some(v) = &p.version
                        && target == "claude-code"
                        && (v.trim().is_empty() || has_control(v) || v.len() > 128)
                    {
                        issues.push(format!("{label}: version must be a short plain string"));
                    }
                }
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

/// What is wrong with the path of an Antigravity plugin folder, as a profile
/// holds it: absolute, `~/...` (expanded on the machine you apply to) or
/// relative (to the project, in a project profile).
fn antigravity_path_issue(path: &str) -> Option<String> {
    if path.trim().is_empty() || has_control(path) || path.len() > 1024 {
        return Some("a plugin folder path must be a short, plain, non-empty path".into());
    }
    if path.starts_with('~') && path != "~" && !path.starts_with("~/") {
        return Some(format!(
            "'{}' is not a path Antigravity or kitbag reads; write ~/... for a folder in your home directory",
            path.escape_default()
        ));
    }
    None
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
            match marketplace_from_settings(target, name, entry, cap) {
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
        if !set.marketplaces.contains_key(market) && market != builtin_marketplace(target) {
            cap.info(
                target,
                format!(
                    "plugin '{id}': its marketplace '{market}' is not declared in this settings file, so only the plugin is carried; add the marketplace on the other machine with {}",
                    add_marketplace_hint(target)
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
    target: &str,
    name: &str,
    entry: &Value,
    cap: &Captured,
) -> std::result::Result<Marketplace, String> {
    let Some(src) = entry.get("source").and_then(Value::as_object) else {
        return Err("it has no source".into());
    };
    let kind = src.get("source").and_then(Value::as_str).unwrap_or("");
    if !source_kinds(target).contains(&kind) {
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
    let issues = marketplace_issues(target, &src);
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

/// The marketplaces and plugins of a set that apply to a target.
type Applicable<'a> = (
    Vec<(&'a String, &'a Marketplace)>,
    Vec<(&'a String, &'a Plugin)>,
);

/// The marketplaces and plugins of `set` that apply to `target`; the others
/// are listed in the plan as disabled in the profile.
fn applicable<'a>(set: &'a PluginSet, target: &str, work: &mut Work) -> Applicable<'a> {
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
                    describe(target, id, p)
                ),
            ),
        }
    }
    (markets, plugins)
}

/// Add the profile's marketplaces and plugins for `target` to the Claude Code
/// or Copilot CLI settings file at `path`: `extraKnownMarketplaces` entries and
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
    let (markets, plugins) = applicable(set, target, work);
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
    let who = if target == "copilot" {
        "Copilot CLI"
    } else {
        "Claude Code"
    };
    match (target, env.scope) {
        ("copilot", Scope::User) => work.warn(
            target,
            "kitbag doesn't download plugins. Copilot CLI installs the enabledPlugins entries above itself (declarative plugin auto-install) and fetches the marketplaces; plugins can bundle hooks, MCP servers and agents. To install now, run `copilot plugin marketplace add` and `copilot plugin install <name>@<marketplace>`",
        ),
        ("copilot", Scope::Project) => work.warn(
            target,
            "kitbag doesn't download plugins. .github/copilot/settings.json is a repository default: Copilot CLI and Copilot in the cloud enable these plugins for everyone who works in the repository, and a person can turn one off with `false` in .github/copilot/settings.local.json. VS Code reads the file for its workspace plugin recommendations. An autoUpdate on a marketplace there is ignored; only user settings turn auto-update on",
        ),
        (_, Scope::User) => work.warn(
            target,
            format!("kitbag doesn't download plugins. {who} fetches the marketplaces and plugins above itself the next time it starts (plugins can bundle hooks, MCP servers and commands); to install now, run `claude plugin marketplace add` and `claude plugin install <name>@<marketplace>`"),
        ),
        (_, Scope::Project) => work.warn(
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
// Antigravity

/// Antigravity's discovery manifest for plugins: `plugins.json` in
/// `~/.gemini/config/` (user) or `.agents/` (workspace), `{"entries": [{"path":
/// ..., "include_only": [..], "exclude": [..]}]}`.
pub const ANTIGRAVITY_MANIFEST: &str = "plugins.json";

fn is_absolute_path(p: &str) -> bool {
    let b = p.as_bytes();
    p.starts_with('/')
        || p.starts_with('\\')
        || (b.len() >= 3
            && b[0].is_ascii_alphabetic()
            && b[1] == b':'
            && matches!(b[2], b'/' | b'\\'))
}

/// `p` with `\` as `/`, `.` and `..` segments folded and no trailing `/`.
fn fold_path(p: &str) -> String {
    let p = p.replace('\\', "/");
    let rooted = p.starts_with('/');
    let mut out: Vec<&str> = Vec::new();
    for seg in p.split('/') {
        match seg {
            "" | "." => {}
            ".." if out.last().is_some_and(|l| *l != "..") => {
                out.pop();
            }
            s => out.push(s),
        }
    }
    let joined = out.join("/");
    if p.starts_with("//") {
        format!("//{joined}")
    } else if rooted {
        format!("/{joined}")
    } else {
        joined
    }
}

/// `abs`, a folded path with `/` separators, with this machine's home
/// directory written as `~`.
fn home_relative(abs: &str, home: &Path, os: crate::adapters::Os) -> String {
    let h = fold_path(&home.display().to_string());
    if h.is_empty() || h == "/" {
        return abs.to_string();
    }
    let same = |a: &str, b: &str| {
        if os == crate::adapters::Os::Linux {
            a == b
        } else {
            a.eq_ignore_ascii_case(b)
        }
    };
    match abs.get(..h.len()) {
        Some(head)
            if same(head, &h) && (abs.len() == h.len() || abs[h.len()..].starts_with('/')) =>
        {
            format!("~{}", &abs[h.len()..])
        }
        _ => abs.to_string(),
    }
}

/// Where an `entries` path points: absolute as it is, relative to `base`.
fn resolve_entry(path: &str, base: &Path, os: crate::adapters::Os) -> String {
    let full = if is_absolute_path(path) {
        path.to_string()
    } else {
        format!("{}/{path}", base.display())
    };
    let folded = fold_path(&full);
    if os == crate::adapters::Os::Linux {
        folded
    } else {
        folded.to_lowercase()
    }
}

fn plain_strings(v: Option<&Value>) -> Option<Vec<String>> {
    match v {
        None | Some(Value::Null) => Some(Vec::new()),
        Some(Value::Array(a)) => a
            .iter()
            .map(|x| {
                x.as_str()
                    .filter(|s| !s.trim().is_empty() && !has_control(s) && s.len() <= 256)
                    .map(str::to_string)
            })
            .collect(),
        Some(_) => None,
    }
}

/// How many folders sit directly in `dir` (the plugins `agy plugin install`
/// unpacked, or that were put there by hand). Names only; nothing is read.
fn count_dirs(dir: &Path) -> usize {
    std::fs::read_dir(dir)
        .map(|rd| {
            rd.filter_map(std::result::Result::ok)
                .filter(|e| e.file_type().is_ok_and(|t| t.is_dir()))
                .count()
        })
        .unwrap_or(0)
}

/// Read the plugin folders an Antigravity `plugins.json` lists into the
/// profile, by reference. `file` is how the plan names the manifest; `folder`
/// is the plugins directory Antigravity unpacks its installs into.
pub fn capture_antigravity(
    cap: &mut Captured,
    env: &Env,
    doc: Option<&Value>,
    file: &str,
    folder: &Path,
) {
    let target = "antigravity";
    let base = match env.scope {
        Scope::User => env.home.join(".gemini").join("config"),
        Scope::Project => env.project.clone(),
    };
    let mut found: BTreeMap<String, Plugin> = BTreeMap::new();
    let (mut other, mut outside, mut tilde) = (Vec::new(), 0, Vec::new());
    if let Some(doc) = doc {
        match doc.get("entries") {
            None | Some(Value::Null) => {}
            Some(Value::Array(list)) => {
                for entry in list {
                    let Some(path) = entry.get("path").and_then(Value::as_str) else {
                        other.push(entry.to_string());
                        continue;
                    };
                    let (Some(include_only), Some(exclude)) = (
                        plain_strings(entry.get("include_only")),
                        plain_strings(entry.get("exclude")),
                    ) else {
                        other.push(path.to_string());
                        continue;
                    };
                    if path.starts_with('~') {
                        tilde.push(path.to_string());
                        continue;
                    }
                    let key = match env.scope {
                        Scope::User => {
                            let abs = if is_absolute_path(path) {
                                fold_path(path)
                            } else {
                                fold_path(&format!("{}/{path}", base.display()))
                            };
                            home_relative(&abs, &env.home, env.os)
                        }
                        Scope::Project => {
                            let rel = fold_path(path);
                            if is_absolute_path(path) || rel.starts_with("..") || rel.is_empty() {
                                outside += 1;
                                continue;
                            }
                            rel
                        }
                    };
                    if antigravity_path_issue(&key).is_some() {
                        other.push(path.to_string());
                        continue;
                    }
                    found.entry(key).or_insert_with(|| Plugin {
                        include_only,
                        exclude,
                        ..Plugin::default()
                    });
                }
            }
            Some(_) => cap.info(
                target,
                format!("'entries' in {file} is not a list, so no plugins were captured from it"),
            ),
        }
        let inherits = doc
            .get("inherits")
            .and_then(Value::as_array)
            .map_or(0, Vec::len);
        if inherits > 0 {
            cap.info(
                target,
                format!("{inherits} 'inherits' manifest(s) in {file} are not carried; they point at other manifests on this machine"),
            );
        }
        if doc
            .get("exclude")
            .and_then(Value::as_array)
            .is_some_and(|a| !a.is_empty())
        {
            cap.info(
                target,
                format!("the top-level 'exclude' list in {file} is not carried"),
            );
        }
    }
    let shown = |l: &[String]| {
        l.iter()
            .take(5)
            .map(|s| s.escape_default().to_string())
            .collect::<Vec<_>>()
            .join(", ")
    };
    if !other.is_empty() {
        cap.info(
            target,
            format!(
                "{} plugin entr{} in {file} not carried (no path, or include_only/exclude that isn't a list of names): {}",
                other.len(),
                if other.len() == 1 { "y" } else { "ies" },
                shown(&other)
            ),
        );
    }
    if !tilde.is_empty() {
        cap.info(
            target,
            format!(
                "{} plugin entr{} in {file} not carried: Antigravity doesn't expand '~', so it never loads {}",
                tilde.len(),
                if tilde.len() == 1 { "y" } else { "ies" },
                shown(&tilde)
            ),
        );
    }
    if outside > 0 {
        cap.info(
            target,
            format!(
                "{outside} plugin entr{} in {file} not carried: a project profile keeps only paths inside the project (Antigravity's changelog says it skips a workspace entry that points elsewhere)",
                if outside == 1 { "y" } else { "ies" }
            ),
        );
    }
    let installed = count_dirs(folder);
    if installed > 0 {
        cap.info(
            target,
            format!(
                "{installed} plugin folder(s) in {} were installed by `agy plugin install` (or put there by hand) and are not carried: Antigravity records no source for them, and kitbag never copies plugin files; reinstall them with `agy plugin install` on the other machine",
                folder.display()
            ),
        );
    }
    if found.is_empty() {
        return;
    }
    let n = found.len();
    cap.profile
        .plugins
        .entry(target.to_string())
        .or_default()
        .enabled
        .extend(found);
    cap.info(
        target,
        format!("{n} plugin folder(s) captured by reference from {file}; the plugins themselves are not copied"),
    );
}

/// Add the profile's plugin folders to the `entries` of the Antigravity
/// `plugins.json` at `path`, nothing else. A folder already listed (by the same
/// resolved path) is left alone, unless the profile gives it different
/// `include_only` / `exclude` lists (replaced, but not with `--no-overwrite`).
pub fn apply_antigravity(
    profile: &Profile,
    env: &Env,
    opts: &ApplyOptions,
    work: &mut Work,
    path: &Path,
) -> Result<()> {
    let target = "antigravity";
    let Some(set) = profile.plugins.get(target) else {
        return Ok(());
    };
    let (base, user) = match env.scope {
        Scope::User => (env.home.join(".gemini").join("config"), true),
        Scope::Project => (env.project.clone(), false),
    };
    let mut wanted = Vec::new();
    for (key, p) in &set.enabled {
        if let Some(why) = targeting_skip_reason(&p.targets, &p.disabled_on, target) {
            work.info(
                target,
                format!(
                    "{} skipped: disabled in profile ({why})",
                    describe(target, key, p)
                ),
            );
            continue;
        }
        // A relative path means something else in each scope, so a profile's
        // path is written only where it means what it says. A workspace
        // manifest is committed, so it holds nothing machine-specific.
        let abs = is_absolute_path(key);
        let outside = fold_path(key).starts_with("..") || fold_path(key).is_empty();
        if (user && !abs) || (!user && (abs || outside)) {
            work.warn(
                target,
                format!(
                    "{} not written: {}",
                    describe(target, key, p),
                    if user {
                        "a relative path would be read from ~/.gemini/config/ here and from the project elsewhere; write it as ~/... or an absolute path"
                    } else {
                        "a project's plugins.json is committed, and Antigravity's changelog says it skips workspace entries that point outside the workspace; use a path inside the project"
                    }
                ),
            );
            continue;
        }
        wanted.push((key, p));
    }
    if wanted.is_empty() {
        return Ok(());
    }
    let text = work.fs.read(path)?;
    let mut doc = JsonDoc::parse(text.as_deref(), path)?;
    doc.check_unique_keys(&[], &["entries"])?;
    let current = doc.value();
    if current.get("entries").is_some_and(|v| !v.is_array()) {
        bail!(
            "entries in {} is not a list; fix it by hand, kitbag won't overwrite it",
            path.display()
        );
    }
    let mut entries: Vec<Value> = current
        .get("entries")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let file = path
        .file_name()
        .map_or_else(String::new, |f| f.to_string_lossy().into_owned());
    let mut added = Vec::new();
    let mut changed = false;
    for (key, p) in wanted {
        let want = resolve_entry(key, &base, env.os);
        let at = entries.iter().position(|e| {
            e.get("path")
                .and_then(Value::as_str)
                .is_some_and(|have| resolve_entry(have, &base, env.os) == want)
        });
        let strings = |e: &Value, k: &str| plain_strings(e.get(k)).unwrap_or_default();
        match at {
            None => {
                let mut e = Map::new();
                e.insert("path".into(), json!(key));
                if !p.include_only.is_empty() {
                    e.insert("include_only".into(), json!(p.include_only));
                }
                if !p.exclude.is_empty() {
                    e.insert("exclude".into(), json!(p.exclude));
                }
                entries.push(Value::Object(e));
                changed = true;
                added.push(describe(target, key, p));
                let there = if is_absolute_path(key) {
                    Path::new(key).to_path_buf()
                } else {
                    base.join(key)
                };
                if !there.exists() {
                    work.warn(
                        target,
                        format!(
                            "{} doesn't exist on this machine: Antigravity has nothing to load from it until the plugin is there (kitbag doesn't copy or fetch plugins)",
                            describe(target, key, p)
                        ),
                    );
                }
            }
            Some(i) => {
                let differs = (!p.include_only.is_empty()
                    && strings(&entries[i], "include_only") != p.include_only)
                    || (!p.exclude.is_empty() && strings(&entries[i], "exclude") != p.exclude);
                if !differs {
                    continue;
                }
                if !opts.overwrite {
                    work.warn(
                        target,
                        format!(
                            "kept the existing {file} entry for '{}' because --no-overwrite was given; the profile has a different include_only / exclude",
                            key.escape_default()
                        ),
                    );
                    continue;
                }
                if let Value::Object(o) = &mut entries[i] {
                    if !p.include_only.is_empty() {
                        o.insert("include_only".into(), json!(p.include_only));
                    }
                    if !p.exclude.is_empty() {
                        o.insert("exclude".into(), json!(p.exclude));
                    }
                }
                changed = true;
                added.push(format!(
                    "{} (include_only / exclude)",
                    describe(target, key, p)
                ));
            }
        }
    }
    if !changed {
        return Ok(());
    }
    doc.set(&["entries"], &Value::Array(entries));
    work.fs.write(path, doc.render())?;
    for d in &added {
        work.info(target, format!("{d} added to {file}"));
    }
    work.warn(
        target,
        "kitbag doesn't install or enable plugins. Antigravity loads the folders above itself and keeps its own enabled / disabled state (`agy plugin enable|disable`); a plugin can bundle hooks, MCP servers and skills that run on this machine",
    );
    Ok(())
}

// ---------------------------------------------------------------------------
// Codex

/// A Codex `[marketplaces.<name>]` table as a profile marketplace: the keys
/// Codex stores, `source_type`, `source`, `ref` and `sparse_paths`, as they are.
fn codex_marketplace(
    name: &str,
    entry: &Value,
    cap: &Captured,
) -> std::result::Result<Marketplace, String> {
    let Some(table) = entry.as_object() else {
        return Err("it is not a table".into());
    };
    let mut src = Map::new();
    for key in ["source_type", "source", "ref", "sparse_paths"] {
        if let Some(v) = table.get(key) {
            src.insert(key.to_string(), v.clone());
        }
    }
    if let Some((home, os)) = &cap.home
        && src.get("source_type").and_then(Value::as_str) == Some("local")
        && let Some(Value::String(p)) = src.get_mut("source")
    {
        *p = crate::portable::to_home_relative(p, home, *os);
    }
    let issues = codex_source_issues(&src);
    if !issues.is_empty() {
        return Err(issues.join("; "));
    }
    if let Some(i) = name_issue("marketplace", name) {
        return Err(i);
    }
    Ok(Marketplace {
        source: src,
        auto_update: None,
        targets: None,
        disabled_on: Vec::new(),
    })
}

/// Read the `[marketplaces.*]` and `[plugins.*]` tables of a Codex
/// `config.toml` (as JSON) into the profile, by reference. `file` is how the
/// plan names the file.
pub fn capture_codex(cap: &mut Captured, doc: &Value, file: &str) {
    let target = "codex";
    let mut set = PluginSet::default();
    if let Some(Value::Object(declared)) = doc.get("marketplaces") {
        for (name, entry) in declared {
            match codex_marketplace(name, entry, cap) {
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
    let (mut off, mut no_switch, mut with_servers) = (0, 0, 0);
    if let Some(Value::Object(configured)) = doc.get("plugins") {
        for (id, v) in configured {
            // `[plugins._default]` is the default for every plugin, not one.
            if id == "_default" {
                continue;
            }
            let ok_id = split_id(id).is_some_and(|(n, m)| {
                is_valid_name(n) && is_valid_name(m) && name_issue("marketplace", m).is_none()
            });
            if !ok_id {
                cap.info(
                    target,
                    format!(
                        "plugin '{}' not captured: not a <name>@<marketplace> id kitbag writes to a profile",
                        id.escape_default()
                    ),
                );
                continue;
            }
            if v.get("mcp_servers").is_some() {
                with_servers += 1;
            }
            match v.get("enabled").and_then(Value::as_bool) {
                Some(true) => {
                    set.enabled.insert(id.clone(), Plugin::default());
                }
                Some(false) => off += 1,
                None => no_switch += 1,
            }
        }
    }
    if off > 0 {
        cap.info(
            target,
            format!("{off} plugin(s) set to enabled = false in {file} are not carried"),
        );
    }
    if no_switch > 0 {
        cap.info(
            target,
            format!("{no_switch} [plugins.*] table(s) in {file} have no enabled key, so they are not carried"),
        );
    }
    if with_servers > 0 {
        cap.info(
            target,
            format!("the per-plugin MCP server settings (approval modes, tool lists) of {with_servers} plugin(s) in {file} are not carried"),
        );
    }
    for id in set.enabled.keys() {
        let market = split_id(id).map_or("", |(_, m)| m);
        if !set.marketplaces.contains_key(market) && market != builtin_marketplace(target) {
            cap.info(
                target,
                format!(
                    "plugin '{id}': its marketplace '{market}' is not declared in {file}, so only the plugin is carried; add the marketplace on the other machine with {}",
                    add_marketplace_hint(target)
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
            "{} marketplace(s) and {} plugin(s) captured by reference from {file}; the plugin cache (~/.codex/plugins/cache/) is not copied",
            set.marketplaces.len(),
            set.enabled.len()
        ),
    );
    cap.profile.plugins.insert(target.to_string(), set);
}

/// Add the profile's marketplaces and plugins to a Codex `config.toml` that is
/// being edited: `[marketplaces.<name>]` tables and `enabled = true` under
/// `[plugins."<name>@<marketplace>"]`, nothing else. A marketplace that is
/// already there with the same source is left alone; a different one is
/// updated key by key (not with `--no-overwrite`). Returns whether the
/// document changed.
pub fn apply_codex(
    profile: &Profile,
    env: &Env,
    opts: &ApplyOptions,
    work: &mut Work,
    doc: &mut toml_edit::DocumentMut,
    path: &Path,
) -> Result<bool> {
    let target = "codex";
    let Some(set) = profile.plugins.get(target) else {
        return Ok(false);
    };
    let (markets, plugins) = applicable(set, target, work);
    if markets.is_empty() && plugins.is_empty() {
        return Ok(false);
    }
    let file = path
        .file_name()
        .map_or_else(String::new, |f| f.to_string_lossy().into_owned());
    let mut added = Vec::new();
    if !markets.is_empty() {
        let table = crate::edit::toml::table_mut(doc, &["marketplaces"])?;
        for (name, m) in markets {
            let have = table.get(name).map(crate::edit::toml::item_to_json);
            // Only what the profile sets is compared; a `ref` it leaves out
            // is not an instruction to unpin.
            let same = have
                .as_ref()
                .and_then(Value::as_object)
                .is_some_and(|h| m.source.iter().all(|(k, v)| h.get(k) == Some(v)));
            if same {
                continue;
            }
            if have.is_some() && !opts.overwrite {
                work.warn(
                    target,
                    format!(
                        "kept the existing [marketplaces.{name}] in {} because --no-overwrite was given; the profile has {}",
                        path.display(),
                        describe_source(&m.source)
                    ),
                );
                continue;
            }
            match table.get_mut(name).and_then(Item::as_table_like_mut) {
                Some(t) => {
                    for (k, v) in &m.source {
                        if let Some(val) = crate::edit::toml::json_to_value(v) {
                            t.insert(k, Item::Value(val));
                        }
                    }
                }
                None => {
                    table.insert(
                        name,
                        Item::Table(crate::edit::toml::json_to_table(&m.source)),
                    );
                }
            }
            added.push(describe_marketplace(name, m));
        }
    }
    if !plugins.is_empty() {
        let table = crate::edit::toml::table_mut(doc, &["plugins"])?;
        for (id, p) in plugins {
            let have = table.get(id);
            let on = have.and_then(|i| i.get("enabled")).and_then(Item::as_bool);
            if on == Some(true) {
                continue;
            }
            if have.is_some() && !have.is_some_and(Item::is_table_like) {
                work.warn(
                    target,
                    format!(
                        "kept plugins.'{id}' in {} as it is: it is not a table",
                        path.display()
                    ),
                );
                continue;
            }
            if on.is_some() && !opts.overwrite {
                work.warn(
                    target,
                    format!(
                        "kept plugins.'{id}' as it is in {} because --no-overwrite was given",
                        path.display()
                    ),
                );
                continue;
            }
            match table.get_mut(id).and_then(Item::as_table_like_mut) {
                Some(t) => {
                    t.insert("enabled", toml_edit::value(true));
                }
                None => {
                    let mut t = toml_edit::Table::new();
                    t.insert("enabled", toml_edit::value(true));
                    table.insert(id, Item::Table(t));
                }
            }
            added.push(describe_plugin(id, p));
        }
    }
    if added.is_empty() {
        return Ok(false);
    }
    for d in &added {
        work.info(target, format!("{d} added to {file}"));
    }
    work.warn(
        target,
        "kitbag doesn't install plugins. Codex fetches a configured marketplace and installs or refreshes the plugins configured for it itself (`codex plugin marketplace upgrade` does it now); a plugin can bundle hooks, MCP servers and skills that run on this machine",
    );
    if env.scope == Scope::Project {
        work.info(
            target,
            "Codex only loads .codex/config.toml in projects you have marked as trusted",
        );
    }
    Ok(true)
}

// ---------------------------------------------------------------------------
// Zed

const ZED_KEY: &str = "auto_install_extensions";

/// Read the extensions Zed's `auto_install_extensions` setting installs into
/// the profile, by reference (the extension ids).
pub fn capture_zed(cap: &mut Captured, settings: &Value, file: &str) {
    let target = "zed";
    let Some(Value::Object(listed)) = settings.get(ZED_KEY) else {
        return;
    };
    let (mut never, mut other) = (0, Vec::new());
    let mut found = Vec::new();
    for (id, v) in listed {
        match v {
            Value::Bool(true) if is_valid_name(id) && id.len() <= 128 => found.push(id.clone()),
            Value::Bool(false) => never += 1,
            _ => other.push(id.escape_default().to_string()),
        }
    }
    if never > 0 {
        cap.info(
            target,
            format!("{never} extension(s) set to false (never install) in {file} are not carried"),
        );
    }
    if !other.is_empty() {
        cap.info(
            target,
            format!(
                "{} entr{} of {ZED_KEY} in {file} not carried (not a plain id set to true): {}",
                other.len(),
                if other.len() == 1 { "y" } else { "ies" },
                other.iter().take(5).cloned().collect::<Vec<_>>().join(", ")
            ),
        );
    }
    if found.is_empty() {
        return;
    }
    let n = found.len();
    let set = cap.profile.plugins.entry(target.to_string()).or_default();
    for id in found {
        set.enabled.entry(id).or_default();
    }
    cap.info(
        target,
        format!(
            "{n} extension(s) captured by reference from {file}; extension files are not copied"
        ),
    );
}

/// Add the profile's extension ids to `auto_install_extensions` in the Zed
/// settings file at `path`, as `true`, nothing else. Zed installs them from
/// its gallery itself. User scope only: Zed's docs name no project-level
/// `auto_install_extensions` (UNVERIFIED for `.zed/settings.json`).
pub fn apply_zed(
    profile: &Profile,
    env: &Env,
    opts: &ApplyOptions,
    work: &mut Work,
    path: &Path,
) -> Result<()> {
    let target = "zed";
    let Some(set) = profile.plugins.get(target) else {
        return Ok(());
    };
    let (_, wanted) = applicable(set, target, work);
    if wanted.is_empty() {
        return Ok(());
    }
    if env.scope == Scope::Project {
        work.info(
            target,
            format!(
                "{} Zed extension(s) not written: Zed's docs describe {ZED_KEY} as a setting of the user's settings.json and say nothing of a project's .zed/settings.json (UNVERIFIED), so kitbag writes it at user scope only",
                wanted.len()
            ),
        );
        return Ok(());
    }
    let text = work.fs.read(path)?;
    let mut doc = JsonDoc::parse(text.as_deref(), path)?;
    doc.check_unique_keys(&[], &[ZED_KEY])?;
    let current = doc.value();
    if current.get(ZED_KEY).is_some_and(|v| !v.is_object()) {
        bail!(
            "{ZED_KEY} in {} is not an object; fix it by hand, kitbag won't overwrite it",
            path.display()
        );
    }
    let mut added = Vec::new();
    for (id, p) in wanted {
        let have = current.get(ZED_KEY).and_then(|v| v.get(id.as_str()));
        if have == Some(&json!(true)) {
            continue;
        }
        if have.is_some() && !opts.overwrite {
            work.warn(
                target,
                format!(
                    "kept {ZED_KEY} '{id}' as it is in {} because --no-overwrite was given",
                    path.display()
                ),
            );
            continue;
        }
        doc.set(&[ZED_KEY, id], &json!(true));
        added.push(describe(target, id, p));
    }
    if added.is_empty() {
        return Ok(());
    }
    work.fs.write(path, doc.render())?;
    let file = path
        .file_name()
        .map_or_else(String::new, |f| f.to_string_lossy().into_owned());
    for d in &added {
        work.info(target, format!("{d} added to {file}"));
    }
    work.warn(
        target,
        "kitbag doesn't install extensions. Zed downloads the extensions above from its gallery itself the next time it starts (an extension can bundle language servers and MCP servers that run on this machine)",
    );
    Ok(())
}

// ---------------------------------------------------------------------------
// Grok Build

/// Read the plugin folders of `[plugins] paths` in Grok Build's user
/// `config.toml` (as JSON) into the profile, by reference.
pub fn capture_grok(cap: &mut Captured, env: &Env, doc: &Value, file: &str) {
    let target = "grok-build";
    let plugins = doc.get("plugins");
    let listed = plugins
        .and_then(|p| p.get("paths"))
        .and_then(Value::as_array);
    let names = |k: &str| {
        plugins
            .and_then(|p| p.get(k))
            .and_then(Value::as_array)
            .map_or(0, Vec::len)
    };
    let by_name = names("enabled") + names("disabled");
    if by_name > 0 {
        cap.info(
            target,
            format!("{by_name} plugin name(s) in [plugins] enabled / disabled in {file} are not carried: they name plugins that have to be installed first"),
        );
    }
    let Some(listed) = listed else {
        return;
    };
    if env.scope == Scope::Project {
        cap.info(
            target,
            format!("[plugins] paths in {file} are not carried at project scope: Grok Build's docs don't say what a relative path there is read from, and an absolute one is specific to this machine"),
        );
        return;
    }
    let mut found = Vec::new();
    let mut other = Vec::new();
    for v in listed {
        let Some(path) = v.as_str() else {
            other.push(v.to_string());
            continue;
        };
        let key = if is_absolute_path(path) {
            home_relative(&fold_path(path), &env.home, env.os)
        } else {
            path.to_string()
        };
        if grok_path_issue(&key).is_some() {
            other.push(path.to_string());
        } else {
            found.push(key);
        }
    }
    if !other.is_empty() {
        cap.info(
            target,
            format!(
                "{} [plugins] paths entr{} in {file} not carried (relative paths are not carried, since Grok Build's docs don't say what they are read from): {}",
                other.len(),
                if other.len() == 1 { "y" } else { "ies" },
                other
                    .iter()
                    .take(5)
                    .map(|s| s.escape_default().to_string())
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
        );
    }
    if found.is_empty() {
        return;
    }
    let n = found.len();
    let set = cap.profile.plugins.entry(target.to_string()).or_default();
    for key in found {
        set.enabled.entry(key).or_default();
    }
    cap.info(
        target,
        format!("{n} plugin folder(s) captured by reference from {file}; the plugins themselves are not copied"),
    );
}

/// Add the profile's plugin folders to `[plugins] paths` in Grok Build's user
/// `config.toml` that is being edited, nothing else. A folder already listed
/// (by the same resolved path) is left alone. Returns whether the document
/// changed. User scope only.
pub fn apply_grok(
    profile: &Profile,
    env: &Env,
    work: &mut Work,
    doc: &mut toml_edit::DocumentMut,
    path: &Path,
) -> Result<bool> {
    let target = "grok-build";
    let Some(set) = profile.plugins.get(target) else {
        return Ok(false);
    };
    let (_, wanted) = applicable(set, target, work);
    if wanted.is_empty() {
        return Ok(false);
    }
    if env.scope == Scope::Project {
        work.info(
            target,
            format!(
                "{} plugin folder(s) not written to .grok/config.toml: Grok Build's docs don't say what a relative path in [plugins] paths is read from, and an absolute one is specific to this machine; kitbag writes them to the user config",
                wanted.len()
            ),
        );
        return Ok(false);
    }
    let file = path
        .file_name()
        .map_or_else(String::new, |f| f.to_string_lossy().into_owned());
    let mut todo = Vec::new();
    for (key, p) in wanted {
        if let Some(why) = grok_path_issue(key) {
            work.warn(
                target,
                format!("{} not written: {why}", describe(target, key, p)),
            );
            continue;
        }
        todo.push((key, p));
    }
    if todo.is_empty() {
        return Ok(false);
    }
    let table = crate::edit::toml::table_mut(doc, &["plugins"])?;
    if table.get("paths").is_some_and(|i| !i.is_array()) {
        bail!(
            "[plugins] paths in {} is not a list; fix it by hand, kitbag won't overwrite it",
            path.display()
        );
    }
    if table.get("paths").is_none() {
        table.insert("paths", toml_edit::value(toml_edit::Array::new()));
    }
    let Some(paths) = table.get_mut("paths").and_then(Item::as_array_mut) else {
        return Ok(false);
    };
    let mut added = Vec::new();
    for (key, p) in todo {
        let want = resolve_entry(key, &env.home, env.os);
        let listed = paths.iter().filter_map(|v| v.as_str()).any(|have| {
            resolve_entry(
                &crate::portable::expand_home(have, &env.home, env.os),
                &env.home,
                env.os,
            ) == want
        });
        if listed {
            continue;
        }
        paths.push(key.as_str());
        added.push(describe(target, key, p));
        if !Path::new(key).exists() {
            work.warn(
                target,
                format!(
                    "{} doesn't exist on this machine: Grok Build has nothing to load from it until the plugin is there (kitbag doesn't copy or fetch plugins)",
                    describe(target, key, p)
                ),
            );
        }
    }
    if added.is_empty() {
        return Ok(false);
    }
    for d in &added {
        work.info(target, format!("{d} added to {file}"));
    }
    work.warn(
        target,
        "kitbag doesn't install or enable plugins. Grok Build loads the folders above itself; a plugin can bundle hooks, MCP servers and skills that run on this machine (a project plugin may need to be enabled in [plugins] enabled)",
    );
    Ok(true)
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
            out.push((target.clone(), describe(&target, id, p)));
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
            out.push(format!("{target}: {}", describe(target, id, p)));
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
            let what = match target.as_str() {
                "opencode" => {
                    "an npm package opencode installs itself and runs inside opencode, with its permissions"
                }
                "antigravity" => {
                    "a folder on this machine Antigravity loads as a plugin, with whatever it bundles (hooks, MCP servers, skills) once it is enabled"
                }
                "grok-build" => {
                    "a folder on this machine Grok Build loads as a plugin, with whatever it bundles (hooks, MCP servers, skills)"
                }
                "zed" => {
                    "an extension Zed downloads from its gallery itself and runs, with whatever it bundles (language servers, MCP servers)"
                }
                _ => {
                    "runs whatever the plugin bundles (hooks, MCP servers, commands) once the tool installs it"
                }
            };
            out.push(format!("{} for {target}: {what}", describe(target, id, p)));
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

/// Expand `~` in local marketplace paths and in the plugin folders of
/// Antigravity (whose `plugins.json` doesn't expand it) for this machine.
pub fn expand_paths(
    plugins: &mut BTreeMap<String, PluginSet>,
    home: &Path,
    os: crate::adapters::Os,
) {
    for target in ["antigravity", "grok-build"] {
        if let Some(set) = plugins.get_mut(target) {
            set.enabled = std::mem::take(&mut set.enabled)
                .into_iter()
                .map(|(k, v)| (crate::portable::expand_home(&k, home, os), v))
                .collect();
        }
    }
    for m in plugins
        .values_mut()
        .flat_map(|s| s.marketplaces.values_mut())
    {
        let local = if m.source.get("source_type").and_then(Value::as_str) == Some("local") {
            Some("source")
        } else if matches!(
            m.source.get("source").and_then(Value::as_str),
            Some("file" | "directory")
        ) {
            Some("path")
        } else {
            None
        };
        if let Some(key) = local
            && let Some(Value::String(p)) = m.source.get_mut(key)
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
    fn antigravity_paths_fold_and_resolve_the_same_on_every_os() {
        assert_eq!(fold_path("a/./b/../c/"), "a/c");
        assert_eq!(fold_path("../x"), "../x");
        assert_eq!(fold_path(r"C:\Users\me\plugins\"), "C:/Users/me/plugins");
        assert_eq!(fold_path(r"\\server\share\p"), "//server/share/p");
        assert!(is_absolute_path("/a") && is_absolute_path(r"C:\a") && is_absolute_path("c:/a"));
        assert!(!is_absolute_path("a/b") && !is_absolute_path("~/a") && !is_absolute_path("C:a"));
        let base = Path::new("/home/me/.gemini/config");
        let linux = crate::adapters::Os::Linux;
        assert_eq!(resolve_entry("../../x/", base, linux), "/home/me/x");
        assert_eq!(resolve_entry("/abs/y", base, linux), "/abs/y");
        let win = crate::adapters::Os::Windows;
        assert_eq!(resolve_entry(r"C:\Users\Me\P", base, win), "c:/users/me/p");
    }

    #[test]
    fn antigravity_paths_in_the_home_directory_become_tilde_paths() {
        let linux = crate::adapters::Os::Linux;
        let home = Path::new("/home/me");
        assert_eq!(home_relative("/home/me/team/x", home, linux), "~/team/x");
        assert_eq!(home_relative("/home/me", home, linux), "~");
        assert_eq!(home_relative("/home/meow/x", home, linux), "/home/meow/x");
        assert_eq!(home_relative("/Home/me/x", home, linux), "/Home/me/x");
        let win = crate::adapters::Os::Windows;
        let home = Path::new(r"C:\Users\Me");
        assert_eq!(home_relative("c:/users/me/team", home, win), "~/team");
        assert_eq!(home_relative("D:/other", home, win), "D:/other");
    }

    #[test]
    fn antigravity_plugin_paths_are_checked() {
        assert!(antigravity_path_issue("~/team/x").is_none());
        assert!(antigravity_path_issue("tools/x").is_none());
        assert!(antigravity_path_issue(r"C:\Users\me\x").is_none());
        assert!(antigravity_path_issue("").is_some());
        assert!(antigravity_path_issue("~other/x").is_some());
        assert!(antigravity_path_issue("a\nb").is_some());
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
