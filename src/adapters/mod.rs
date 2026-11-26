//! Target adapters: one per AI tool.
//!
//! An adapter knows where a tool keeps its configuration and how to translate
//! between that and a [`Profile`]. Adapters never touch disk when applying:
//! they plan writes into a [`PlanFs`] and report anything noteworthy as
//! [`Note`]s. See `README.md` ("Adding an adapter") for a walkthrough.

use std::collections::HashMap;
use std::fmt;
use std::path::{Path, PathBuf};

use anyhow::Result;

use crate::model::Profile;
use crate::plan_fs::PlanFs;
use crate::secrets::{Redaction, RenderState};

pub mod claude_code;
pub mod claude_desktop;
pub mod codex;
pub mod common;
pub mod cursor;
pub mod gemini;

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
    Settings,
}

impl Feature {
    pub const ALL: [Feature; 6] = [
        Feature::Mcp,
        Feature::Instructions,
        Feature::Rules,
        Feature::Commands,
        Feature::Skills,
        Feature::Settings,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            Feature::Mcp => "mcp",
            Feature::Instructions => "instructions",
            Feature::Rules => "rules",
            Feature::Commands => "commands",
            Feature::Skills => "skills",
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
    /// (target, server name) of profile servers not applied to that target
    /// because of `targets` / `disabled_on`.
    pub skipped_servers: Vec<(String, String)>,
    /// (target, server name) of MCP server entries the plan removes.
    pub removed_servers: Vec<(String, String)>,
    /// MCP server entries the plan writes or removes, for the state records.
    pub server_writes: Vec<ServerWrite>,
    /// Instruction blocks and rule files kitbag manages, written or not, for
    /// drift detection (`kitbag status`).
    pub tracked: Vec<Tracked>,
    /// (target, `command <name>` / `skill <name>`) of every command and skill
    /// the plan writes.
    pub changed_items: Vec<(String, String)>,
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
    fn support(&self, scope: Scope) -> [(Feature, Support); 6];
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
    ]
}

pub fn get(id: &str) -> Option<&'static dyn Adapter> {
    all().iter().copied().find(|a| a.id() == id)
}

pub fn ids() -> Vec<&'static str> {
    all().iter().map(|a| a.id()).collect()
}
