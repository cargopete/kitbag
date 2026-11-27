//! Target adapters: one per AI tool.
//!
//! An adapter knows where a tool keeps its configuration and how to translate
//! between that and a [`Profile`]. Adapters never touch disk when applying:
//! they plan writes into a [`PlanFs`] and report anything noteworthy as
//! [`Note`]s. See `README.md` ("Adding an adapter") for a walkthrough.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::fmt;
use std::path::{Path, PathBuf};

use anyhow::Result;

use crate::model::Profile;
use crate::plan_fs::PlanFs;
use crate::secrets::{Redaction, RenderState};

pub mod antigravity;
pub mod claude_code;
pub mod claude_desktop;
mod claude_hooks;
pub mod codex;
pub mod common;
pub mod copilot;
pub mod cursor;
pub mod gemini;
pub mod grok_build;
pub mod opencode;
pub mod windsurf;
pub mod zed;

/// Global (per-user) configuration or one project's configuration.
#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
pub enum Scope {
    User,
    Project,
}

impl fmt::Display for Scope {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Scope::User => "user",
            Scope::Project => "project",
        })
    }
}

/// Operating system, for platform-specific config locations.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Os {
    Linux,
    MacOs,
    Windows,
}

impl Os {
    pub fn current() -> Os {
        if cfg!(target_os = "macos") {
            Os::MacOs
        } else if cfg!(windows) {
            Os::Windows
        } else {
            Os::Linux
        }
    }
}

/// Where and for whom kitbag is operating.
#[derive(Debug, Clone)]
pub struct Env {
    pub scope: Scope,
    /// The user's home directory (overridable for sandboxes and tests).
    pub home: PathBuf,
    /// Absolute project root (meaningful when `scope` is `Project`).
    pub project: PathBuf,
    /// Environment variables used to resolve `${VAR}` references.
    pub vars: HashMap<String, String>,
    /// Variables that relocate tool config (`CODEX_HOME`, `CLAUDE_CONFIG_DIR`, ...).
    /// Kept separate so `--home` sandboxes can ignore the real ones.
    pub locations: HashMap<String, String>,
    pub os: Os,
    /// Capture hooks too (`capture --include hooks`). Without it they are
    /// only counted, because they run commands automatically.
    pub include_hooks: bool,
}

impl Env {
    pub fn location_var(&self, name: &str) -> Option<PathBuf> {
        self.locations
            .get(name)
            .filter(|v| !v.is_empty())
            .map(PathBuf::from)
    }
}

/// Environment variables that move tool configuration around.
pub const LOCATION_VARS: &[&str] = &[
    "CLAUDE_CONFIG_DIR",
    "CODEX_HOME",
    "COPILOT_HOME",
    "XDG_CONFIG_HOME",
    "APPDATA",
];

/// A configurable aspect of an agent setup.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Feature {
    Mcp,
    Instructions,
    Rules,
    Commands,
    Skills,
    Agents,
    Settings,
}

impl Feature {
    pub const ALL: [Feature; 7] = [
        Feature::Mcp,
        Feature::Instructions,
        Feature::Rules,
        Feature::Commands,
        Feature::Skills,
        Feature::Agents,
        Feature::Settings,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            Feature::Mcp => "mcp",
            Feature::Instructions => "instructions",
            Feature::Rules => "rules",
            Feature::Commands => "commands",
            Feature::Skills => "skills",
            Feature::Agents => "agents",
            Feature::Settings => "settings",
        }
    }
}

/// How well a target supports a feature.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Support {
    /// The tool has a first-class home for it.
    Native,
    /// Emulated, e.g. scoped rules folded into AGENTS.md as sections.
    Folded,
    /// Not supported; skipped with a note.
    None,
}

impl Support {
    pub fn as_str(self) -> &'static str {
        match self {
            Support::Native => "native",
            Support::Folded => "folded",
            Support::None => "-",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Level {
    Info,
    Warn,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Note {
    pub level: Level,
    pub target: String,
    pub message: String,
}

/// Options that change how a profile is applied.
#[derive(Debug, Clone)]
pub struct ApplyOptions {
    /// Replace same-named entries (servers, rule files...) that differ. Default true.
    pub overwrite: bool,
    /// For tools without env interpolation: resolve `${VAR}` now, or keep placeholders.
    pub secrets: SecretsMode,
    /// Id of managed blocks in shared files (CLAUDE.md, AGENTS.md ...).
    pub block_id: String,
    /// MCP server entries earlier applies wrote that still stand (see
    /// [`crate::sync::applied_servers`]). A server the profile disables for a
    /// tool is only removed from it when one of these says kitbag put it there.
    pub applied_servers: Vec<crate::sync::ServerRecord>,
    /// Where the profile comes from: its local directory, or its git URL
    /// (without credentials). Recorded with the MCP server entries it writes,
    /// and only that same source may remove them later.
    pub source: Option<String>,
    /// For a profile from a git source, the servers it may remove from a tool
    /// (the same opt-in as adding them: `--allow-git-mcp` / `--trust-source`).
    /// `None` for a local profile, which may remove any of its own.
    pub git_removals: Option<crate::engine::GitMcp>,
    /// Managed items to leave as they are in the tool, as (real file, block
    /// id; `None` for a whole rule file): `sync` holds back the ones edited
    /// by hand. Empty for `apply`.
    pub hold: BTreeSet<(PathBuf, Option<String>)>,
    /// Write the profile's hooks (`apply --apply-hooks`). Hooks run commands
    /// automatically, so without this they are only listed in the plan.
    pub apply_hooks: bool,
    /// Rule, command, skill and agent files earlier applies wrote that still
    /// stand (see [`crate::sync::applied_files`]). One the profile disables for
    /// a tool is only removed from it when one of these says kitbag put it
    /// there and it is unchanged since.
    pub applied_files: Vec<crate::sync::FileRecord>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
pub enum SecretsMode {
    /// Resolve references from the environment for tools that can't (warned, masked in diffs).
    Resolve,
    /// Never write secret values; leave `${VAR}` placeholders in place.
    Never,
}

/// Mutable state while planning an apply.
#[derive(Debug, Default)]
pub struct Work {
    pub fs: PlanFs,
    pub render: RenderState,
    pub notes: Vec<Note>,
    /// (target, server name) of every MCP server entry the plan adds or changes.
    pub changed_servers: Vec<(String, String)>,
    /// Name of the profile being applied.
    pub profile: String,
    /// The home directory, for `~/...` paths in notes.
    pub home: PathBuf,
    /// (target, server name) of profile servers not applied to that target
    /// because of `targets` / `disabled_on`.
    pub skipped_servers: Vec<(String, String)>,
    /// (target, server name) of MCP server entries the plan removes.
    pub removed_servers: Vec<(String, String)>,
    /// (target, server name) of MCP server entries a git-sourced profile
    /// would remove, held back for lack of `--allow-git-mcp`.
    pub withheld_removals: Vec<(String, String)>,
    /// MCP server entries the plan writes or removes, for the state records.
    pub server_writes: Vec<ServerWrite>,
    /// Hook entries the plan writes, for the state records.
    pub hook_writes: Vec<HookWrite>,
    /// Instruction blocks and rule files kitbag manages, written or not, for
    /// drift detection (`kitbag status`).
    pub tracked: Vec<Tracked>,
    /// (target, `command <name>` / `skill <name>`) of every command and skill
    /// the plan writes.
    pub changed_items: Vec<(String, String)>,
    /// Ids of the targets this plan applies, so a target sharing a file with
    /// another knows whether that one is applied too.
    pub targets: Vec<String>,
    /// Rule, command, skill and agent files the plan writes or removes, for
    /// the state records.
    pub file_writes: Vec<FileWrite>,
    /// (target, `rule <name>` ...) of items the plan removes from a tool,
    /// because the profile now disables them there.
    pub removed_items: Vec<(String, String)>,
    /// Skill directories the plan empties; removed when empty after the write.
    pub prune_dirs: Vec<PathBuf>,
}

/// One rule, command, skill or agent a plan writes (`hash` is its
/// [`crate::sync::hash`]; for a skill, [`crate::sync::files_hash`] of `files`)
/// or removes (`hash` is `None`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileWrite {
    pub target: String,
    /// The file, or a skill's directory, as the adapter spelled it until the
    /// plan resolves it.
    pub path: PathBuf,
    /// `rule <name>`, `command <name>`, `skill <name>` or `agent <name>`.
    pub item: String,
    pub hash: Option<String>,
    /// For a skill: each file's path under the directory and the hash of its content.
    pub files: BTreeMap<String, String>,
}

/// One MCP server entry a plan writes (`hash` is its [`crate::sync::entry_hash`])
/// or removes (`hash` is `None`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServerWrite {
    pub target: String,
    /// The config file, as the adapter spelled it until the plan resolves it.
    pub path: PathBuf,
    pub name: String,
    pub hash: Option<String>,
}

/// One hook entry a plan writes, with the scripts that go with it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HookWrite {
    pub target: String,
    /// The settings file, as the adapter spelled it until the plan resolves it.
    pub path: PathBuf,
    /// The tool's own event name.
    pub event: String,
    pub matcher: Option<String>,
    pub command: String,
    /// [`crate::sync::entry_hash`] of the entry as written.
    pub hash: String,
    /// The scripts it runs that the plan writes: path (as the adapter spelled
    /// it) and SHA-256 of the content.
    pub scripts: Vec<(PathBuf, String)>,
}

/// One instructions item kitbag manages in a tool: a managed block in a
/// shared file, or a whole rule file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Tracked {
    pub target: String,
    /// As the adapter spelled it; the plan resolves it to the real file.
    pub path: PathBuf,
    /// Managed block id, or `None` for a whole file.
    pub block: Option<String>,
    /// `instructions`, `rules` (folded) or `rule <name>`.
    pub item: String,
}

impl Work {
    pub fn track(&mut self, target: &str, path: &Path, block: Option<&str>, item: String) {
        self.tracked.push(Tracked {
            target: target.to_string(),
            path: path.to_path_buf(),
            block: block.map(str::to_string),
            item,
        });
    }
}

impl Work {
    pub fn note(&mut self, level: Level, target: &str, message: impl Into<String>) {
        self.notes.push(Note {
            level,
            target: target.to_string(),
            message: message.into(),
        });
    }
    pub fn info(&mut self, target: &str, message: impl Into<String>) {
        self.note(Level::Info, target, message);
    }
    pub fn warn(&mut self, target: &str, message: impl Into<String>) {
        self.note(Level::Warn, target, message);
    }
}

/// The result of capturing one target.
#[derive(Debug, Default)]
pub struct Captured {
    pub profile: Profile,
    pub notes: Vec<Note>,
    pub redactions: Vec<Redaction>,
    /// Files and directories that were read.
    pub sources: Vec<PathBuf>,
    /// Rules read from a file of their own (not folded into an instructions
    /// file), by name: lets `status` find rule files that exist only in the tool.
    pub rule_files: Vec<(String, PathBuf)>,
    /// At project scope: the project root as given and canonicalised. Files
    /// under it are only read if their real location is inside it too.
    pub project: Option<(PathBuf, PathBuf)>,
    /// This machine's home directory and OS: paths under it are captured as
    /// `~/...` (see [`crate::portable`]).
    pub home: Option<(PathBuf, Os)>,
}

impl Captured {
    /// An empty capture for `env`; at project scope reads are confined to the project.
    pub fn for_env(env: &Env) -> Self {
        let project = (env.scope == Scope::Project).then(|| {
            let real = env
                .project
                .canonicalize()
                .unwrap_or_else(|_| env.project.clone());
            (env.project.clone(), real)
        });
        Captured {
            project,
            home: Some((env.home.clone(), env.os)),
            ..Self::default()
        }
    }

    /// Does `path`, a project file or directory, really live outside the
    /// project (through a symbolic link)? If so, warn that it was skipped.
    /// Paths outside the project (the home directory) are not checked.
    pub fn escapes(&mut self, path: &Path, target: &str) -> bool {
        let Some((given, root)) = &self.project else {
            return false;
        };
        if !path.starts_with(given) && !path.starts_with(root) {
            return false;
        }
        match path.canonicalize() {
            Ok(real) if !real.starts_with(root) => {
                let msg = format!(
                    "skipped {}: symbolic link to {}, outside the project",
                    path.display(),
                    real.display()
                );
                self.warn(target, msg);
                true
            }
            _ => false,
        }
    }

    /// Read a config file for capture, skipping one that escapes the project.
    pub fn read_config(&mut self, path: &Path, target: &str) -> Result<Option<String>> {
        if self.escapes(path, target) {
            return Ok(None);
        }
        crate::plan_fs::read_text_opt(path)
    }

    pub fn info(&mut self, target: &str, message: impl Into<String>) {
        self.notes.push(Note {
            level: Level::Info,
            target: target.to_string(),
            message: message.into(),
        });
    }
    pub fn warn(&mut self, target: &str, message: impl Into<String>) {
        self.notes.push(Note {
            level: Level::Warn,
            target: target.to_string(),
            message: message.into(),
        });
    }
}

/// One AI tool kitbag can capture from and apply to.
pub trait Adapter: Sync {
    /// Stable identifier used on the command line, e.g. `claude-code`.
    fn id(&self) -> &'static str;
    /// Display name, e.g. `Claude Code`.
    fn name(&self) -> &'static str;
    /// Documentation link for the tool's configuration.
    fn docs(&self) -> &'static str;
    fn scopes(&self) -> &'static [Scope];
    fn support(&self, scope: Scope) -> [(Feature, Support); 7];
    /// Key config locations for this scope (shown by `doctor` and `list-targets`).
    fn locations(&self, env: &Env) -> Vec<(&'static str, PathBuf)>;
    /// Is the tool installed or configured (in this project)?
    fn detect(&self, env: &Env) -> bool;
    /// Read the tool's current setup into a (redacted) profile.
    fn capture(&self, env: &Env) -> Result<Captured>;
    /// Plan the writes that make the tool match `profile`. Never touches disk.
    fn apply(
        &self,
        profile: &Profile,
        env: &Env,
        opts: &ApplyOptions,
        work: &mut Work,
    ) -> Result<()>;

    fn supports(&self, scope: Scope, feature: Feature) -> Support {
        if !self.scopes().contains(&scope) {
            return Support::None;
        }
        self.support(scope)
            .into_iter()
            .find(|(f, _)| *f == feature)
            .map_or(Support::None, |(_, s)| s)
    }
}

/// All built-in adapters, in display order.
pub fn all() -> &'static [&'static dyn Adapter] {
    &[
        &claude_code::ClaudeCode,
        &cursor::Cursor,
        &codex::Codex,
        &gemini::GeminiCli,
        &claude_desktop::ClaudeDesktop,
        &antigravity::Antigravity,
        &opencode::OpenCode,
        &copilot::Copilot,
        &grok_build::GrokBuild,
        &windsurf::Windsurf,
        &zed::Zed,
    ]
}

/// Other names accepted for a target id, as (alias, canonical id). Only ever
/// used to read user input; output, state files and profiles use the
/// canonical id.
pub const ALIASES: &[(&str, &str)] = &[("claude", "claude-code"), ("grok", "grok-build")];

/// The canonical id for `id`: an alias becomes the id it stands for, anything
/// else is returned unchanged. Case-sensitive, like the ids themselves.
pub fn canonical_id(id: &str) -> &str {
    ALIASES
        .iter()
        .find(|(alias, _)| *alias == id)
        .map_or(id, |(_, canonical)| canonical)
}

pub fn get(id: &str) -> Option<&'static dyn Adapter> {
    let id = canonical_id(id);
    all().iter().copied().find(|a| a.id() == id)
}

pub fn ids() -> Vec<&'static str> {
    all().iter().map(|a| a.id()).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn claude_is_an_alias_for_claude_code_only() {
        assert_eq!(canonical_id("claude"), "claude-code");
        assert_eq!(canonical_id("claude-code"), "claude-code");
        assert_eq!(canonical_id("claude-desktop"), "claude-desktop");
        assert_eq!(canonical_id("Claude"), "Claude");
        assert_eq!(get("claude").map(|a| a.id()), Some("claude-code"));
        assert!(get("Claude").is_none());
        assert_eq!(canonical_id("grok"), "grok-build");
        assert_eq!(get("grok").map(|a| a.id()), Some("grok-build"));
        // Aliases never shadow a real id and always point at one.
        for (alias, id) in ALIASES {
            assert!(!ids().contains(alias));
            assert!(ids().contains(id));
        }
    }
}
