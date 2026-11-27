//! `kitbag doctor --why DIR`: which instruction files each tool loads when it
//! is started in a directory, in what order, which files it skips and why,
//! and known traps.
//!
//! Every rule here follows the tool's own documentation, source or issue
//! tracker (see the links next to each rule). Where those leave a question
//! open, the report says so with an `unverified` line instead of guessing.
//! Nothing is started and nothing is written: this only looks at files.

use std::fs;
use std::path::{Component, Path, PathBuf};

use serde_json::Value;

use crate::adapters::{Env, Os};
use crate::frontmatter;
use crate::plan_fs::read_text_opt;

/// What a tool does with one file, or a remark about its setup.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Status {
    /// Read when the tool starts (numbered in load order).
    Loaded,
    /// Read later, when the agent touches matching files.
    Later,
    /// Exists, but the tool doesn't read it.
    Skipped,
    /// A known way this setup goes wrong.
    Trap,
    /// Something worth knowing that is not a problem.
    Note,
    /// A question the tool's documentation doesn't answer.
    Unverified,
}

impl Status {
    pub fn label(self) -> &'static str {
        match self {
            Status::Loaded => "load",
            Status::Later => "later",
            Status::Skipped => "skip",
            Status::Trap => "trap",
            Status::Note => "note",
            Status::Unverified => "unverified",
        }
    }
}

#[derive(Debug, Clone)]
pub struct Entry {
    pub status: Status,
    pub path: Option<PathBuf>,
    pub why: String,
}

/// One tool's answer.
#[derive(Debug, Clone)]
pub struct Report {
    pub id: &'static str,
    pub name: &'static str,
    pub entries: Vec<Entry>,
}

impl Report {
    fn new(id: &'static str, name: &'static str) -> Self {
        Report {
            id,
            name,
            entries: Vec::new(),
        }
    }
    fn push(&mut self, status: Status, path: Option<&Path>, why: impl Into<String>) {
        self.entries.push(Entry {
            status,
            path: path.map(Path::to_path_buf),
            why: why.into(),
        });
    }
    fn load(&mut self, path: &Path, why: impl Into<String>) {
        self.push(Status::Loaded, Some(path), why);
    }
    fn later(&mut self, path: &Path, why: impl Into<String>) {
        self.push(Status::Later, Some(path), why);
    }
    fn skip(&mut self, path: &Path, why: impl Into<String>) {
        self.push(Status::Skipped, Some(path), why);
    }
    fn trap(&mut self, path: Option<&Path>, why: impl Into<String>) {
        self.push(Status::Trap, path, why);
    }
    fn note(&mut self, why: impl Into<String>) {
        self.push(Status::Note, None, why);
    }
    fn unverified(&mut self, why: impl Into<String>) {
        self.push(Status::Unverified, None, why);
    }
    /// Files loaded at start, in order.
    pub fn loaded(&self) -> impl Iterator<Item = &Path> {
        self.entries
            .iter()
            .filter(|e| e.status == Status::Loaded)
            .filter_map(|e| e.path.as_deref())
    }
}

/// Where to look.
#[derive(Debug, Clone)]
pub struct Place<'a> {
    pub env: &'a Env,
    /// The directory the tool would be started in (absolute, no `..`).
    pub dir: PathBuf,
    /// `--home` is in use: system-wide files (managed policy) are not looked at.
    pub sandboxed: bool,
}

/// Tools that read instruction files, by target id.
pub const TOOLS: &[&str] = &[
    "claude-code",
    "cursor",
    "codex",
    "gemini-cli",
    "antigravity",
    "opencode",
    "grok-build",
];

/// The report for target `id`.
pub fn explain(id: &str, place: &Place) -> Report {
    match id {
        "claude-code" => claude_code(place),
        "opencode" => opencode(place),
        "codex" => codex(place),
        "cursor" => cursor(place),
        "gemini-cli" => gemini(place),
        "antigravity" => antigravity(place),
        "grok-build" => grok_build(place),
        _ => {
            let (id, name) = crate::adapters::get(id).map_or(("", ""), |a| (a.id(), a.name()));
            let mut r = Report::new(id, name);
            r.note("reads no instruction files");
            r
        }
    }
}

/// `path` made absolute with `.` and `..` resolved by name, the way a shell
/// shows a working directory (symbolic links are kept).
pub fn normalize(path: &Path) -> std::io::Result<PathBuf> {
    let abs = std::path::absolute(path)?;
    let mut out = PathBuf::new();
    for c in abs.components() {
        match c {
            Component::CurDir => {}
            Component::ParentDir => {
                // Never above the root (or a Windows drive).
                if out.parent().is_some() {
                    out.pop();
                }
            }
            other => out.push(other.as_os_str()),
        }
    }
    Ok(out)
}

/// `dir` and the directories above it, root first.
fn root_down(dir: &Path) -> Vec<PathBuf> {
    let mut v: Vec<PathBuf> = dir.ancestors().map(Path::to_path_buf).collect();
    v.reverse();
    v
}

/// A regular file (or a link to one).
fn file(path: &Path) -> bool {
    path.is_file()
}

fn size(path: &Path) -> u64 {
    fs::metadata(path).map(|m| m.len()).unwrap_or(0)
}

fn same_file(a: &Path, b: &Path) -> bool {
    match (fs::canonicalize(a), fs::canonicalize(b)) {
        (Ok(a), Ok(b)) => a == b,
        _ => false,
    }
}

fn kib(n: u64) -> String {
    if n >= 1024 * 1024 {
        format!("{:.1} MiB", n as f64 / (1024.0 * 1024.0))
    } else {
        format!("{:.1} KiB", n as f64 / 1024.0)
    }
}

/// `*.md` files under `dir`, sorted, at most `max`.
fn markdown_under(dir: &Path, exts: &[&str], max: usize) -> Vec<PathBuf> {
    if !dir.is_dir() {
        return Vec::new();
    }
    let Ok(files) = crate::plan_fs::walk_files(dir, max) else {
        return Vec::new();
    };
    files
        .iter()
        .filter(|f| {
            Path::new(f)
                .extension()
                .and_then(|e| e.to_str())
                .is_some_and(|e| exts.contains(&e))
        })
        .map(|f| crate::plan_fs::join_rel(dir, f))
        .collect()
}

/// A file with entries a tool reads by name, but in a different case: on a
/// case-sensitive file system the tool doesn't see it.
fn wrong_case(dir: &Path, name: &str) -> Option<PathBuf> {
    let rd = fs::read_dir(dir).ok()?;
    for e in rd.flatten() {
        let n = e.file_name();
        let n = n.to_string_lossy();
        if n != name && n.eq_ignore_ascii_case(name) {
            return Some(e.path());
        }
    }
    None
}

/// Report files named like `wanted` in any case but not exactly. Only on
/// systems that are usually case-sensitive (on macOS and Windows the tool
/// finds them anyway).
fn flag_wrong_case(r: &mut Report, place: &Place, dir: &Path, wanted: &str) {
    if place.env.os != Os::Linux {
        return;
    }
    if file(&dir.join(wanted)) {
        return;
    }
    if let Some(p) = wrong_case(dir, wanted) {
        r.skip(&p, format!("wrong name: the file must be called {wanted}"));
    }
}

fn read_json(path: &Path) -> Option<Value> {
    let text = read_text_opt(path).ok()??;
    crate::edit::json::parse_value(Some(&text), path).ok()
}

/// `path` for a message, `~/...` under the home directory.
fn show(env: &Env, path: &Path) -> String {
    crate::ui::tildify(path, &env.home)
}

fn var_set(env: &Env, name: &str) -> bool {
    env.vars
        .get(name)
        .is_some_and(|v| !v.is_empty() && v != "0" && !v.eq_ignore_ascii_case("false"))
}

// ---------------------------------------------------------------------------
// Claude Code
// https://code.claude.com/docs/en/memory

/// Claude Code skips a file it loads at start when it is bigger than this
/// ("Claude Code skips a file over 4 MiB").
const CLAUDE_MAX_BYTES: u64 = 4 * 1024 * 1024;

fn claude_config_dir(env: &Env) -> PathBuf {
    env.location_var("CLAUDE_CONFIG_DIR")
        .unwrap_or_else(|| env.home.join(".claude"))
}

fn claude_managed(os: Os) -> PathBuf {
    match os {
        Os::MacOs => PathBuf::from("/Library/Application Support/ClaudeCode/CLAUDE.md"),
        Os::Linux => PathBuf::from("/etc/claude-code/CLAUDE.md"),
        Os::Windows => PathBuf::from(r"C:\Program Files\ClaudeCode\CLAUDE.md"),
    }
}

/// How Claude Code picks between CLAUDE.md and AGENTS.md (the
/// `instructionFiles` option of its built-in AGENTS.md plugin).
#[derive(Debug, Clone, PartialEq, Eq)]
enum AgentsMode {
    /// Default: AGENTS.md only without any CLAUDE.md in or above the directory.
    Fallback,
    /// CLAUDE.md and AGENTS.md; in each directory the CLAUDE.md files first.
    Both,
    ClaudeOnly,
    /// Only the managed CLAUDE.md (and auto memory).
    ManagedOnly,
    Other(String),
}

fn claude_agents_mode(settings: Option<&Value>) -> AgentsMode {
    let configs = settings.and_then(|s| s.get("pluginConfigs"));
    // The plugin's id before v2.1.285 was `agents-md@builtin`; both are read.
    let value = ["cc-plugin-agents-md@builtin", "agents-md@builtin"]
        .iter()
        .find_map(|id| {
            configs
                .and_then(|c| c.get(id))
                .and_then(|p| p.get("options"))
                .and_then(|o| o.get("instructionFiles"))
                .and_then(Value::as_str)
        });
    match value {
        None | Some("claude-md-or-agents-md") => AgentsMode::Fallback,
        Some("claude-md-and-agents-md") => AgentsMode::Both,
        Some("claude-md") => AgentsMode::ClaudeOnly,
        Some("managed-only") => AgentsMode::ManagedOnly,
        Some(other) => AgentsMode::Other(other.to_string()),
    }
}

fn claude_check_size(r: &mut Report, path: &Path, why: &str) {
    let n = size(path);
    if n > CLAUDE_MAX_BYTES {
        r.skip(
            path,
            format!("too large: {} (Claude Code skips files over 4 MiB)", kib(n)),
        );
    } else {
        r.load(path, why);
    }
}

fn claude_code(place: &Place) -> Report {
    let mut r = Report::new("claude-code", "Claude Code");
    let env = place.env;
    let config = claude_config_dir(env);
    let user_md = config.join("CLAUDE.md");
    let settings = read_json(&config.join("settings.json"));
    let mode = claude_agents_mode(settings.as_ref());

    // 1. Managed policy, 2. user, 3. the directories from the root down.
    if place.sandboxed {
        r.note(format!(
            "with --home, the managed policy file ({}) is not checked",
            claude_managed(env.os).display()
        ));
    } else if file(&claude_managed(env.os)) {
        claude_check_size(&mut r, &claude_managed(env.os), "managed policy");
    }
    if mode == AgentsMode::ManagedOnly {
        claude_managed_only(&mut r, place, &config);
        return r;
    }
    if file(&user_md) {
        claude_check_size(&mut r, &user_md, "user instructions");
    }
    claude_rules(&mut r, &config.join("rules"), "user rule");

    // CLAUDE.md, .claude/CLAUDE.md and CLAUDE.local.md from the file system
    // root down to the directory. The ancestor walk is documented for
    // CLAUDE.md and CLAUDE.local.md only; `.claude/CLAUDE.md` only for the
    // directory itself, but a `.claude/CLAUDE.md` above still blocks AGENTS.md.
    // With "claude-md-and-agents-md", each directory's AGENTS.md comes right
    // after its CLAUDE.md files (https://code.claude.com/docs/en/memory).
    let mut blockers: Vec<PathBuf> = Vec::new();
    let mut agents: Vec<PathBuf> = Vec::new();
    let mut alt_above = false;
    for d in root_down(&place.dir) {
        let here = d == place.dir;
        let label = if here { "project" } else { "parent directory" };
        let md = d.join("CLAUDE.md");
        let alt = d.join(".claude").join("CLAUDE.md");
        let local = d.join("CLAUDE.local.md");
        if file(&md) && md != user_md {
            claude_check_size(&mut r, &md, label);
            blockers.push(md.clone());
        }
        // The user file seen as a parent's `.claude/CLAUDE.md` is the user
        // file: by the docs it doesn't block AGENTS.md (but see #103458 below).
        if file(&alt) && alt != user_md {
            if here {
                claude_check_size(&mut r, &alt, label);
                if file(&md) {
                    r.unverified(
                        "the docs don't say whether Claude Code reads both CLAUDE.md and .claude/CLAUDE.md when a directory has both",
                    );
                }
            } else {
                alt_above = true;
            }
            blockers.push(alt.clone());
        }
        if file(&local) {
            claude_check_size(&mut r, &local, &format!("{label}, local"));
            blockers.push(local.clone());
        }
        flag_wrong_case(&mut r, place, &d, "CLAUDE.md");
        // AGENTS.md, through the built-in AGENTS.md plugin (v2.1.277 and later).
        for f in [d.join("AGENTS.md"), d.join(".claude").join("AGENTS.md")] {
            if file(&f) {
                if mode == AgentsMode::Both {
                    claude_agents_md(&mut r, env, &f, &user_md);
                }
                agents.push(f);
            }
        }
    }
    if alt_above {
        r.unverified(
            "the docs don't say whether a .claude/CLAUDE.md in a parent directory is loaded (it does stop AGENTS.md from loading)",
        );
    }
    claude_rules(
        &mut r,
        &place.dir.join(".claude").join("rules"),
        "project rule",
    );

    for f in ["AGENTS.local.md", "AGENTS.override.md"] {
        let p = place.dir.join(f);
        if file(&p) {
            r.skip(&p, "Claude Code never reads this file name");
        }
    }
    match &mode {
        AgentsMode::Both | AgentsMode::ManagedOnly => {}
        AgentsMode::ClaudeOnly => {
            for a in &agents {
                r.skip(
                    a,
                    "not read: instructionFiles is \"claude-md\" in your Claude Code settings",
                );
            }
        }
        AgentsMode::Other(v) => {
            r.unverified(format!(
                "instructionFiles is \"{v}\" in your Claude Code settings; kitbag doesn't know which AGENTS.md files that loads"
            ));
        }
        AgentsMode::Fallback if !blockers.is_empty() => {
            let loaded_before: Vec<PathBuf> = r.loaded().map(Path::to_path_buf).collect();
            for a in &agents {
                if let Some(link) = loaded_before.iter().find(|l| same_file(l, a)) {
                    r.skip(
                        a,
                        format!("read once, through {} (a link to it)", show(env, link)),
                    );
                    continue;
                }
                r.skip(
                    a,
                    format!(
                        "shadowed: Claude Code reads AGENTS.md only when there is no CLAUDE.md in or above the directory, and {} exists",
                        show(env, &blockers[0])
                    ),
                );
            }
            if !agents.is_empty() {
                r.note(format!(
                    "set instructionFiles to \"claude-md-and-agents-md\" in {} to read both, or @import AGENTS.md from CLAUDE.md",
                    show(env, &config.join("settings.json"))
                ));
            }
        }
        AgentsMode::Fallback => {
            for a in &agents {
                claude_agents_md(&mut r, env, a, &user_md);
            }
            if !agents.is_empty() {
                r.unverified(
                    "the docs don't say where AGENTS.md files come in the load order when no CLAUDE.md is found; they are listed after the user and rule files",
                );
            }
            // #103458: under the home directory the check finds
            // ~/.claude/CLAUDE.md as a parent's .claude/CLAUDE.md.
            if !agents.is_empty()
                && file(&user_md)
                && config == env.home.join(".claude")
                && place.dir.starts_with(&env.home)
            {
                r.trap(
                    Some(&user_md),
                    "Claude Code 2.1.302 treats ~/.claude/CLAUDE.md as a .claude/CLAUDE.md above every project under your home directory, so it may not load AGENTS.md here at all (anthropics/claude-code#103458, open)",
                );
            }
        }
    }
    if r.loaded().next().is_none() && agents.is_empty() {
        r.note("no instruction files found");
    }
    r.note("CLAUDE.md files in subdirectories load later, when Claude reads a file there; @path imports are followed up to four levels deep");
    r
}

/// One AGENTS.md that Claude Code reads, unless a file loaded before it is
/// the same file.
fn claude_agents_md(r: &mut Report, env: &Env, a: &Path, user_md: &Path) {
    let loaded: Vec<PathBuf> = r.loaded().map(Path::to_path_buf).collect();
    if !loaded.iter().any(|l| same_file(l, a)) {
        claude_check_size(r, a, "AGENTS.md");
    } else if loaded.iter().any(|l| l == user_md && same_file(l, a)) {
        // The docs: "Claude Code skips an AGENTS.md it has already loaded" -
        // but only for the project's own files (#102369).
        r.trap(
            Some(a),
            format!(
                "loaded twice: {} is a link to this file, and Claude Code reads it once as user instructions and again as project instructions (anthropics/claude-code#102369)",
                show(env, user_md)
            ),
        );
        r.load(a, "AGENTS.md, again");
    } else {
        r.skip(a, "already loaded through a CLAUDE.md that links to it");
    }
}

/// `instructionFiles = "managed-only"`: only the managed CLAUDE.md and auto
/// memory load at launch; user, project and local CLAUDE.md files, rules and
/// every AGENTS.md are excluded (https://code.claude.com/docs/en/memory).
fn claude_managed_only(r: &mut Report, place: &Place, config: &Path) {
    let why = "not read: instructionFiles is \"managed-only\" in your Claude Code settings";
    let mut files = vec![config.join("CLAUDE.md")];
    files.extend(markdown_under(&config.join("rules"), &["md"], 1000));
    for d in root_down(&place.dir) {
        for name in ["CLAUDE.md", "CLAUDE.local.md", "AGENTS.md"] {
            files.push(d.join(name));
        }
        files.push(d.join(".claude").join("CLAUDE.md"));
        files.push(d.join(".claude").join("AGENTS.md"));
    }
    files.extend(markdown_under(
        &place.dir.join(".claude").join("rules"),
        &["md"],
        1000,
    ));
    // The managed file is read even if the walk passes its directory.
    let mut seen: Vec<PathBuf> = vec![claude_managed(place.env.os)];
    for f in files {
        if file(&f) && !seen.contains(&f) {
            r.skip(&f, why);
            seen.push(f);
        }
    }
    r.note("with instructionFiles \"managed-only\", only the managed CLAUDE.md and auto memory load (auto memory is not listed here)");
}

/// Rules under `dir` (recursively): loaded at start, or later when `paths`
/// frontmatter scopes them.
fn claude_rules(r: &mut Report, dir: &Path, label: &str) {
    for f in markdown_under(dir, &["md"], 1000) {
        let scoped = read_text_opt(&f)
            .ok()
            .flatten()
            .map(|t| frontmatter::parse(&t))
            .is_some_and(|d| !frontmatter::glob_list(d.data.get("paths")).is_empty());
        if scoped {
            r.later(
                &f,
                format!("{label} with paths: loads when Claude reads a matching file"),
            );
        } else {
            claude_check_size(r, &f, label);
        }
    }
}

// ---------------------------------------------------------------------------
// opencode
// https://opencode.ai/docs/rules/ and
// https://github.com/anomalyco/opencode/blob/dev/packages/core/src/config/plugin/instruction.ts

fn opencode(place: &Place) -> Report {
    let mut r = Report::new("opencode", "opencode");
    let env = place.env;
    let config = env
        .location_var("XDG_CONFIG_HOME")
        .unwrap_or_else(|| env.home.join(".config"))
        .join("opencode");
    let no_claude = var_set(env, "OPENCODE_DISABLE_CLAUDE_CODE")
        || var_set(env, "OPENCODE_DISABLE_CLAUDE_CODE_PROMPT");
    let claude_user = env.home.join(".claude").join("CLAUDE.md");

    // Global: opencode's own AGENTS.md, else ~/.claude/CLAUDE.md.
    let global = config.join("AGENTS.md");
    if file(&global) {
        r.load(&global, "global");
        if file(&claude_user) {
            r.skip(&claude_user, format!("shadowed by {}", show(env, &global)));
        }
    } else if file(&claude_user) {
        if no_claude {
            r.skip(&claude_user, "OPENCODE_DISABLE_CLAUDE_CODE(_PROMPT) is set");
        } else {
            r.load(
                &claude_user,
                "global (Claude Code's, as there is no opencode AGENTS.md)",
            );
        }
    }

    // Project: from the directory up to the home directory (included) when
    // inside it; outside it, up to the project root.
    if var_set(env, "OPENCODE_DISABLE_PROJECT_CONFIG") {
        r.note("OPENCODE_DISABLE_PROJECT_CONFIG is set: no project AGENTS.md is read");
    } else {
        let dirs: Vec<PathBuf> = if place.dir.starts_with(&env.home) {
            place
                .dir
                .ancestors()
                .take_while(|d| d.starts_with(&env.home))
                .map(Path::to_path_buf)
                .collect()
        } else {
            r.unverified("outside the home directory opencode walks up to the project root; which directory that is isn't documented, so only this directory is shown");
            vec![place.dir.clone()]
        };
        for d in &dirs {
            let agents = d.join("AGENTS.md");
            let claude = d.join("CLAUDE.md");
            let label = if *d == place.dir {
                "project".to_string()
            } else {
                "parent directory".to_string()
            };
            if file(&agents) && agents != global {
                r.load(&agents, &label);
                if file(&claude) {
                    if same_file(&claude, &agents) {
                        r.skip(&claude, "a link to AGENTS.md, read once");
                    } else {
                        r.skip(&claude, "shadowed by AGENTS.md in the same directory");
                    }
                }
            } else if file(&claude) && claude != claude_user {
                if no_claude {
                    r.skip(&claude, "OPENCODE_DISABLE_CLAUDE_CODE(_PROMPT) is set");
                } else {
                    r.load(
                        &claude,
                        format!("{label} (CLAUDE.md, as there is no AGENTS.md)"),
                    );
                }
            }
            flag_wrong_case(&mut r, place, d, "AGENTS.md");
        }
        if dirs.len() > 1 {
            r.unverified(
                "opencode reads the project files concurrently; the order among them isn't fixed",
            );
        }
    }

    // `instructions` in the config files.
    let mut configured = Vec::new();
    for f in [
        config.join("opencode.json"),
        config.join("opencode.jsonc"),
        place.dir.join("opencode.json"),
        place.dir.join("opencode.jsonc"),
        place.dir.join(".opencode").join("opencode.json"),
        place.dir.join(".opencode").join("opencode.jsonc"),
    ] {
        if let Some(list) = read_json(&f)
            .and_then(|v| v.get("instructions").cloned())
            .and_then(|v| v.as_array().cloned())
        {
            for i in list.iter().filter_map(Value::as_str) {
                configured.push(format!("{i} (from {})", show(env, &f)));
            }
        }
    }
    if !configured.is_empty() {
        r.note(format!(
            "also loads, after the files above, what `instructions` lists: {}",
            configured.join(", ")
        ));
    }
    if r.loaded().next().is_none() {
        r.note("no instruction files found");
    }
    r.note("AGENTS.md files in subdirectories load later, when opencode reads a file there");
    r
}

// ---------------------------------------------------------------------------
// Codex CLI
// https://learn.chatgpt.com/docs/agent-configuration/agents-md and
// https://github.com/openai/codex/blob/main/codex-rs/core/src/agents_md.rs

/// `project_doc_max_bytes` by default: the project files' total budget.
const CODEX_DEFAULT_MAX: u64 = 32 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Trust {
    Trusted,
    Untrusted,
    Unknown,
}

struct CodexConfig {
    max_bytes: u64,
    fallbacks: Vec<String>,
    markers: Vec<String>,
}

fn read_toml(path: &Path) -> Option<toml_edit::DocumentMut> {
    let text = read_text_opt(path).ok()??;
    crate::edit::toml::parse(Some(&text), path).ok()
}

/// Apply the settings `doc` sets on top of `cfg`.
fn codex_settings(cfg: &mut CodexConfig, doc: &toml_edit::DocumentMut) {
    if let Some(n) = doc
        .get("project_doc_max_bytes")
        .and_then(|i| i.as_integer())
    {
        cfg.max_bytes = u64::try_from(n).unwrap_or(0);
    }
    let strings = |key: &str| {
        doc.get(key).and_then(|i| i.as_array()).map(|a| {
            a.iter()
                .filter_map(|v| v.as_str().map(str::to_string))
                .collect::<Vec<_>>()
        })
    };
    if let Some(v) = strings("project_doc_fallback_filenames") {
        cfg.fallbacks = v;
    }
    if let Some(v) = strings("project_root_markers") {
        cfg.markers = v;
    }
}

/// The trust recorded for `dir` or, failing that, for `root`
/// (`[projects."<path>"] trust_level`).
fn codex_trust(doc: Option<&toml_edit::DocumentMut>, dir: &Path, root: &Path) -> Trust {
    let Some(projects) = doc
        .and_then(|d| d.get("projects"))
        .and_then(|p| p.as_table_like())
    else {
        return Trust::Unknown;
    };
    let level = |want: &Path| {
        projects.iter().find_map(|(k, v)| {
            let same = normalize(Path::new(k)).is_ok_and(|p| p == want);
            same.then(|| {
                v.get("trust_level")
                    .and_then(|t| t.as_str())
                    .map(str::to_string)
            })
            .flatten()
        })
    };
    match level(dir).or_else(|| level(root)).as_deref() {
        Some("trusted") => Trust::Trusted,
        Some("untrusted") => Trust::Untrusted,
        _ => Trust::Unknown,
    }
}

fn codex(place: &Place) -> Report {
    let mut r = Report::new("codex", "Codex CLI");
    let env = place.env;
    let home = env
        .location_var("CODEX_HOME")
        .unwrap_or_else(|| env.home.join(".codex"));

    // Global: the first non-empty of AGENTS.override.md and AGENTS.md. It
    // doesn't count against project_doc_max_bytes.
    let mut global_done: Option<PathBuf> = None;
    for name in ["AGENTS.override.md", "AGENTS.md"] {
        let p = home.join(name);
        if !file(&p) {
            continue;
        }
        if let Some(g) = &global_done {
            r.skip(&p, format!("shadowed by {}", show(env, g)));
        } else if size(&p) == 0
            || read_text_opt(&p)
                .ok()
                .flatten()
                .is_some_and(|t| t.trim().is_empty())
        {
            r.skip(&p, "empty");
        } else {
            r.load(&p, "global (not counted against project_doc_max_bytes)");
            global_done = Some(p);
        }
    }

    let user_config = read_toml(&home.join("config.toml"));
    let mut cfg = CodexConfig {
        max_bytes: CODEX_DEFAULT_MAX,
        fallbacks: Vec::new(),
        markers: vec![".git".to_string()],
    };
    if let Some(doc) = &user_config {
        codex_settings(&mut cfg, doc);
    }

    // The project root: the nearest directory with a marker (`.git`), else
    // only the directory itself.
    let root = place
        .dir
        .ancestors()
        .find(|d| cfg.markers.iter().any(|m| d.join(m).exists()))
        .unwrap_or(&place.dir)
        .to_path_buf();
    let dirs: Vec<PathBuf> = root_down(&place.dir)
        .into_iter()
        .filter(|d| d.starts_with(&root))
        .collect();

    // Project config is read only in trusted projects; an explicitly
    // untrusted one gets no project AGENTS.md either.
    let trust = codex_trust(user_config.as_ref(), &place.dir, &root);
    for d in &dirs {
        let c = d.join(".codex").join("config.toml");
        if !file(&c) {
            continue;
        }
        if trust == Trust::Trusted {
            r.note(format!("reads {} (trusted project)", show(env, &c)));
            if let Some(doc) = read_toml(&c) {
                codex_settings(&mut cfg, &doc);
            }
        } else {
            r.trap(
                Some(&c),
                format!(
                    "ignored: Codex reads a project's .codex/config.toml only when the project is trusted, and {} is {} in {}",
                    show(env, &root),
                    if trust == Trust::Untrusted { "marked untrusted" } else { "not marked trusted" },
                    show(env, &home.join("config.toml"))
                ),
            );
        }
    }

    let mut names: Vec<String> = vec!["AGENTS.override.md".into(), "AGENTS.md".into()];
    names.extend(cfg.fallbacks.iter().cloned());
    let mut budget = cfg.max_bytes;
    let mut used_by: Vec<PathBuf> = Vec::new();
    for d in &dirs {
        // One file per directory: the first that exists, even an empty one.
        let found: Vec<PathBuf> = names
            .iter()
            .map(|n| d.join(n))
            .filter(|p| file(p))
            .collect();
        let Some(first) = found.first() else {
            flag_wrong_case(&mut r, place, d, "AGENTS.md");
            continue;
        };
        let label = if *d == place.dir {
            "project"
        } else {
            "project root or parent"
        };
        let text = read_text_opt(first).ok().flatten().unwrap_or_default();
        // The cap counts bytes on disk.
        let n = size(first);
        if trust == Trust::Untrusted {
            r.skip(
                first,
                "not read: the project is marked untrusted in Codex's config.toml",
            );
        } else if cfg.max_bytes == 0 {
            r.skip(first, "not read: project_doc_max_bytes is 0");
        } else if text.trim().is_empty() {
            if found.len() > 1 {
                r.trap(
                    Some(first),
                    format!(
                        "empty, so nothing is read from this directory: Codex takes the first file it finds and then skips it as empty, so {} is not read",
                        show(env, &found[1])
                    ),
                );
            } else {
                r.skip(first, "empty");
            }
        } else if budget == 0 {
            r.skip(
                first,
                format!(
                    "over the cap: {} already used all {} of project_doc_max_bytes ({} by default)",
                    used_by
                        .iter()
                        .map(|p| show(env, p))
                        .collect::<Vec<_>>()
                        .join(", "),
                    kib(cfg.max_bytes),
                    kib(CODEX_DEFAULT_MAX)
                ),
            );
        } else if n > budget {
            r.load(first, label);
            r.trap(
                Some(first),
                format!(
                    "truncated: Codex reads only the first {} of its {} (project_doc_max_bytes is {} in total for all project files; raise it in config.toml)",
                    kib(budget),
                    kib(n),
                    kib(cfg.max_bytes)
                ),
            );
            budget = 0;
            used_by.push(first.clone());
        } else {
            r.load(first, label);
            budget -= n;
            used_by.push(first.clone());
        }
        for other in found.iter().skip(1) {
            if !(text.trim().is_empty() && found.len() > 1 && other == &found[1]) {
                r.skip(
                    other,
                    format!(
                        "shadowed by {} (Codex reads one file per directory)",
                        show(env, first)
                    ),
                );
            }
        }
    }
    if trust == Trust::Unknown
        && dirs
            .iter()
            .any(|d| d.join(".codex").join("config.toml").is_file())
    {
        r.note("mark the project trusted in Codex (it asks on first run) to load its .codex/config.toml");
    }
    if root != place.dir {
        r.note(format!(
            "project files are read from {} down to this directory",
            show(env, &root)
        ));
    }
    if r.loaded().next().is_none() {
        r.note("no instruction files found");
    }
    r
}

// ---------------------------------------------------------------------------
// Cursor
// https://cursor.com/docs/context/rules

fn cursor(place: &Place) -> Report {
    let mut r = Report::new("cursor", "Cursor");
    r.trap(
        None,
        "User Rules live in Cursor's settings (Customize > Rules) in app storage, not in files on disk; kitbag can't list them",
    );
    let rules = place.dir.join(".cursor").join("rules");
    for f in markdown_under(&rules, &["mdc", "md"], 1000) {
        if f.extension().and_then(|e| e.to_str()) != Some("mdc") {
            r.skip(&f, "wrong name: project rules must use the .mdc extension");
            continue;
        }
        let doc = read_text_opt(&f)
            .ok()
            .flatten()
            .map(|t| frontmatter::parse(&t))
            .unwrap_or_default();
        let always = doc.data.get("alwaysApply").is_some_and(|v| {
            v.as_bool() == Some(true) || v.as_str().is_some_and(|s| s.eq_ignore_ascii_case("true"))
        });
        let globs = frontmatter::glob_list(doc.data.get("globs"));
        let description =
            frontmatter::string(doc.data.get("description")).filter(|d| !d.trim().is_empty());
        if always {
            r.load(&f, "project rule, always applied");
        } else if !globs.is_empty() {
            r.later(
                &f,
                format!("project rule for files matching {}", globs.join(", ")),
            );
        } else if description.is_some() {
            r.later(
                &f,
                "project rule the agent applies when its description fits",
            );
        } else {
            r.later(&f, "project rule applied only when @-mentioned");
        }
    }
    let agents = place.dir.join("AGENTS.md");
    if file(&agents) {
        if r.loaded().next().is_some() {
            r.unverified(
                "the docs don't say whether AGENTS.md comes before or after the project rules",
            );
        }
        r.load(&agents, "project");
    } else {
        flag_wrong_case(&mut r, place, &place.dir, "AGENTS.md");
    }
    let legacy = place.dir.join(".cursorrules");
    if file(&legacy) {
        r.push(
            Status::Unverified,
            Some(&legacy),
            "legacy .cursorrules: the current docs don't mention it, so whether this Cursor version still reads it is unknown; move it to .cursor/rules/*.mdc",
        );
    }
    let claude = place.dir.join("CLAUDE.md");
    if file(&claude) {
        r.push(
            Status::Unverified,
            Some(&claude),
            "the docs don't say whether Cursor reads CLAUDE.md",
        );
    }
    r.note("this directory is taken as the folder opened in Cursor; AGENTS.md files in subdirectories are combined with it, the nearer one taking precedence");
    r
}

// ---------------------------------------------------------------------------
// Gemini CLI
// https://github.com/google-gemini/gemini-cli/blob/main/docs/cli/gemini-md.md and
// https://github.com/google-gemini/gemini-cli/blob/main/packages/core/src/utils/memoryDiscovery.ts

fn gemini_names(settings: Option<&Value>) -> Option<Vec<String>> {
    let v = settings?.get("context")?.get("fileName")?;
    match v {
        Value::String(s) => Some(vec![s.clone()]),
        Value::Array(a) => Some(
            a.iter()
                .filter_map(|x| x.as_str().map(str::to_string))
                .collect(),
        ),
        _ => None,
    }
}

fn gemini_markers(settings: Option<&Value>) -> Option<Vec<String>> {
    let a = settings?
        .get("context")?
        .get("memoryBoundaryMarkers")?
        .as_array()?;
    Some(
        a.iter()
            .filter_map(|x| x.as_str().map(str::to_string))
            .collect(),
    )
}

fn gemini(place: &Place) -> Report {
    let mut r = Report::new("gemini-cli", "Gemini CLI");
    let env = place.env;
    let base = env.home.join(".gemini");
    let settings = read_json(&base.join("settings.json"));
    let names = gemini_names(settings.as_ref()).unwrap_or_else(|| vec!["GEMINI.md".to_string()]);
    let markers = gemini_markers(settings.as_ref()).unwrap_or_else(|| vec![".git".to_string()]);
    let project_settings = read_json(&place.dir.join(".gemini").join("settings.json"));
    if gemini_names(project_settings.as_ref()).is_some_and(|n| n != names) {
        r.unverified("this project's .gemini/settings.json sets other context.fileName names; kitbag shows the names from ~/.gemini/settings.json");
    }

    for n in &names {
        let p = base.join(n);
        if file(&p) {
            r.load(&p, "global");
        }
    }
    // Up from the directory to the git root (root first); without one, only
    // the directory. Never into ~/.gemini itself.
    let root = place
        .dir
        .ancestors()
        .find(|d| markers.iter().any(|m| d.join(m).exists()));
    if root.is_none() {
        r.unverified("no .git above this directory; kitbag shows only this directory's files (the docs don't say where Gemini CLI stops then)");
    }
    let root = root.unwrap_or(&place.dir).to_path_buf();
    for d in root_down(&place.dir)
        .into_iter()
        .filter(|d| d.starts_with(&root))
    {
        if d == base {
            continue;
        }
        let label = if d == place.dir {
            "project"
        } else {
            "project root or parent"
        };
        for n in &names {
            let p = d.join(n);
            if file(&p) {
                r.load(&p, label);
            } else {
                flag_wrong_case(&mut r, place, &d, n);
            }
        }
        for other in ["GEMINI.md", "AGENTS.md"] {
            let p = d.join(other);
            if !names.iter().any(|n| n == other) && file(&p) {
                r.skip(
                    &p,
                    format!(
                        "not read: Gemini CLI reads {} (context.fileName in ~/.gemini/settings.json)",
                        names.join(", ")
                    ),
                );
            }
        }
    }
    if r.loaded().next().is_none() {
        r.note("no instruction files found");
    }
    r.note("files with these names in subdirectories load later, when a tool reads a file there (older versions read up to 200 subdirectories at start)");
    shared_gemini_md(&mut r, env, "Google Antigravity");
    r
}

/// Gemini CLI and Antigravity both read `~/.gemini/GEMINI.md`
/// (google-gemini/gemini-cli#16058).
fn shared_gemini_md(r: &mut Report, env: &Env, other: &str) {
    let p = env.home.join(".gemini").join("GEMINI.md");
    if file(&p) {
        r.push(
            Status::Note,
            Some(&p),
            format!("shared with {other}, which reads the same file as its global instructions"),
        );
    }
}

// ---------------------------------------------------------------------------
// Google Antigravity
// https://antigravity.google/docs/rules.md and
// https://antigravity.google/docs/customization-discovery/

/// Antigravity truncates a rule file over this size.
const ANTIGRAVITY_MAX_BYTES: u64 = 24_000;

fn antigravity_file(r: &mut Report, path: &Path, why: &str) {
    r.load(path, why);
    let n = size(path);
    if n > ANTIGRAVITY_MAX_BYTES {
        r.trap(
            Some(path),
            format!(
                "truncated: {} bytes; Antigravity reads only the first 24,000 bytes of a rule file",
                n
            ),
        );
    }
}

fn antigravity(place: &Place) -> Report {
    let mut r = Report::new("antigravity", "Google Antigravity");
    let env = place.env;
    let g = env.home.join(".gemini");
    for p in [
        g.join("AGENTS.md"),
        g.join("GEMINI.md"),
        g.join("config").join("AGENTS.md"),
        g.join("config").join("GEMINI.md"),
    ] {
        if file(&p) {
            antigravity_file(&mut r, &p, "global");
        }
    }
    for f in markdown_under(&g.join("config").join("rules"), &["md"], 1000) {
        r.later(&f, "global rule: when it applies is set by its frontmatter");
    }

    // From the repository root (if any) down to the directory.
    let root = place
        .dir
        .ancestors()
        .find(|d| d.join(".git").exists())
        .unwrap_or(&place.dir)
        .to_path_buf();
    for d in root_down(&place.dir)
        .into_iter()
        .filter(|d| d.starts_with(&root))
    {
        if d == g {
            // ~/.gemini's files are the global ones above.
            continue;
        }
        let label = if d == place.dir {
            "workspace"
        } else {
            "repository root or parent"
        };
        let mut both = 0;
        for p in [
            d.join("AGENTS.md"),
            d.join("GEMINI.md"),
            d.join(".agents").join("AGENTS.md"),
            d.join(".agents").join("GEMINI.md"),
        ] {
            if file(&p) {
                if p.parent() == Some(d.as_path()) {
                    both += 1;
                }
                antigravity_file(&mut r, &p, label);
            }
        }
        if both == 2 {
            r.unverified("the docs don't say whether Antigravity reads both AGENTS.md and GEMINI.md in one directory");
        }
        for rules in [
            d.join(".agents").join("rules"),
            d.join(".agent").join("rules"),
        ] {
            if let Ok(names) = fs::read_dir(&rules) {
                let mut files: Vec<PathBuf> = names
                    .flatten()
                    .map(|e| e.path())
                    .filter(|p| p.is_file() && p.extension().and_then(|e| e.to_str()) == Some("md"))
                    .collect();
                files.sort();
                for f in files {
                    r.later(
                        &f,
                        "workspace rule: when it applies is set by its frontmatter",
                    );
                }
            }
        }
    }
    if r.loaded().next().is_none() {
        r.note("no instruction files found");
    }
    shared_gemini_md(&mut r, env, "Gemini CLI");
    r.note("Antigravity doesn't read files in subdirectories below the workspace; global and always-on rules share a 20,000-token budget");
    r
}

// ---------------------------------------------------------------------------
// Grok Build
// https://github.com/xai-org/grok-build: crates/codegen/xai-grok-agent/src/prompt/agents_md.rs,
// crates/codegen/xai-grok-config/src/compat.rs and the user guide's 12-project-rules.md

/// The instruction file names Grok Build looks for in each directory, in order
/// (`INSTRUCTION_FILENAMES`); it loads every one that exists.
const GROK_NAMES: &[&str] = &[
    "Agents.md",
    "Claude.md",
    "CLAUDE.md",
    "CLAUDE.local.md",
    "AGENT.md",
    "AGENTS.md",
];

/// One `[compat.<vendor>]` cell: the `GROK_<VENDOR>_<CELL>_ENABLED` variable,
/// else config.toml, else on.
fn grok_compat(
    env: &Env,
    config: Option<&toml_edit::DocumentMut>,
    vendor: &str,
    cell: &str,
) -> bool {
    let var = format!(
        "GROK_{}_{}_ENABLED",
        vendor.to_ascii_uppercase(),
        cell.to_ascii_uppercase()
    );
    if let Some(v) = env.vars.get(&var).map(|v| v.trim().to_ascii_lowercase())
        && !v.is_empty()
    {
        return !matches!(v.as_str(), "0" | "false" | "no" | "off");
    }
    config
        .and_then(|d| d.get("compat"))
        .and_then(|c| c.get(vendor))
        .and_then(|v| v.get(cell))
        .and_then(|i| i.as_bool())
        .unwrap_or(true)
}

/// `*.md` directly in `dir` (not below), sorted by name.
fn md_in(dir: &Path) -> Vec<PathBuf> {
    let Ok(rd) = fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut files: Vec<PathBuf> = rd
        .flatten()
        .map(|e| e.path())
        .filter(|p| {
            p.is_file()
                && p.extension()
                    .and_then(|e| e.to_str())
                    .is_some_and(|e| e.eq_ignore_ascii_case("md"))
        })
        .collect();
    files.sort_by(|a, b| a.file_name().cmp(&b.file_name()));
    files
}

/// Load `path`, unless the same file (a link, or the home directory inside
/// the project) was loaded already.
fn grok_load(r: &mut Report, seen: &mut Vec<PathBuf>, path: &Path, why: String) {
    let key = fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    if seen.contains(&key) {
        r.skip(path, "already loaded above (the same file)");
    } else {
        seen.push(key);
        r.load(path, why);
    }
}

/// The rules in `dir`: all load; one whose frontmatter scopes it to some files is a trap.
fn grok_rules(r: &mut Report, seen: &mut Vec<PathBuf>, dir: &Path, label: &str) {
    for f in md_in(dir) {
        let globs = read_text_opt(&f).ok().flatten().and_then(|text| {
            let doc = frontmatter::parse(&text);
            // kitbag writes such a rule with a first line saying which files
            // it is for, which the model does see.
            if doc
                .body
                .trim_start()
                .starts_with("_Applies when working with files matching:")
            {
                return None;
            }
            ["paths", "globs", "applyTo"]
                .iter()
                .map(|k| frontmatter::glob_list(doc.data.get(*k)))
                .find(|g| !g.is_empty())
        });
        grok_load(r, seen, &f, label.to_string());
        if let Some(globs) = globs {
            r.trap(
                Some(&f),
                format!("its frontmatter limits it to {}, but Grok Build strips frontmatter and loads every rule, for every file", globs.join(", ")),
            );
        }
    }
}

/// The instruction files Grok Build finds in `dir`, in its order, under their
/// real names. On macOS and Windows names match in any case, and two names
/// that reach the same file count once.
fn grok_named(dir: &Path, os: Os) -> Vec<PathBuf> {
    let Ok(rd) = fs::read_dir(dir) else {
        return Vec::new();
    };
    let entries: Vec<String> = rd
        .flatten()
        .filter(|e| e.path().is_file())
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .collect();
    let mut out: Vec<PathBuf> = Vec::new();
    for name in GROK_NAMES {
        let found = entries.iter().find(|e| {
            if os == Os::Linux {
                e.as_str() == *name
            } else {
                e.eq_ignore_ascii_case(name)
            }
        });
        if let Some(e) = found {
            let p = dir.join(e);
            if !out.contains(&p) {
                out.push(p);
            }
        }
    }
    out
}

/// On Linux, a file named like one of Grok's instruction files in another
/// case (`agents.md`) isn't read.
fn grok_wrong_case(r: &mut Report, dir: &Path, os: Os) {
    if os != Os::Linux {
        return;
    }
    let Ok(rd) = fs::read_dir(dir) else {
        return;
    };
    let mut names: Vec<String> = rd
        .flatten()
        .filter(|e| e.path().is_file())
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    for n in names {
        if !GROK_NAMES.contains(&n.as_str())
            && GROK_NAMES.iter().any(|g| g.eq_ignore_ascii_case(&n))
        {
            r.skip(
                &dir.join(&n),
                format!("wrong name: Grok Build reads only exact names such as AGENTS.md, not {n}"),
            );
        }
    }
}

fn grok_build(place: &Place) -> Report {
    let mut r = Report::new("grok-build", "Grok Build");
    let env = place.env;
    let home = crate::adapters::grok_build::grok_home(env);
    let config = read_toml(&home.join("config.toml"));
    let cell = |vendor: &str, c: &str| grok_compat(env, config.as_ref(), vendor, c);
    let (claude_agents, claude_rules) = (cell("claude", "agents"), cell("claude", "rules"));
    let (cursor_agents, cursor_rules) = (cell("cursor", "agents"), cell("cursor", "rules"));

    let mut seen: Vec<PathBuf> = Vec::new();

    // 1. Home: $GROK_HOME, then ~/.claude and ~/.cursor when their cells are on.
    for p in grok_named(&home, env.os) {
        grok_load(&mut r, &mut seen, &p, "global".into());
    }
    grok_rules(&mut r, &mut seen, &home.join("rules"), "global rule");
    for (vendor, dir, agents, rules_on) in [
        (
            "claude",
            env.home.join(".claude"),
            claude_agents,
            claude_rules,
        ),
        (
            "cursor",
            env.home.join(".cursor"),
            cursor_agents,
            cursor_rules,
        ),
    ] {
        for p in grok_named(&dir, env.os) {
            if agents {
                grok_load(
                    &mut r,
                    &mut seen,
                    &p,
                    format!("global ({vendor} compatibility)"),
                );
            } else {
                r.skip(&p, format!("[compat.{vendor}] agents is off"));
            }
        }
        if rules_on {
            grok_rules(
                &mut r,
                &mut seen,
                &dir.join("rules"),
                &format!("global rule ({vendor} compatibility)"),
            );
        } else {
            for f in md_in(&dir.join("rules")) {
                r.skip(&f, format!("[compat.{vendor}] rules is off"));
            }
        }
    }
    // [paths] extra_rule_dirs: absolute or ~/ entries only.
    let extra: Vec<String> = config
        .as_ref()
        .and_then(|d| d.get("paths"))
        .and_then(|p| p.get("extra_rule_dirs"))
        .and_then(|i| i.as_array())
        .map(|a| {
            a.iter()
                .filter_map(|v| v.as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default();
    for e in extra {
        let dir = match e.strip_prefix("~/") {
            Some(rest) => env.home.join(rest),
            None if Path::new(&e).is_absolute() => PathBuf::from(&e),
            None => {
                r.note(format!("[paths] extra_rule_dirs entry '{e}' is neither absolute nor ~/..., so Grok Build loads nothing from it"));
                continue;
            }
        };
        grok_rules(&mut r, &mut seen, &dir, "rule from [paths] extra_rule_dirs");
    }

    // 2. Project: every directory from the git root down to this one (only
    // this one outside git). Needs folder trust.
    let root = place.dir.ancestors().find(|d| d.join(".git").exists());
    let dirs: Vec<PathBuf> = match root {
        Some(root) => root_down(&place.dir)
            .into_iter()
            .filter(|d| d.starts_with(root))
            .collect(),
        None => vec![place.dir.clone()],
    };
    let before = r.loaded().count();
    for d in &dirs {
        let label = if *d == place.dir {
            "project"
        } else {
            "repository root or parent"
        };
        for p in grok_named(d, env.os) {
            grok_load(&mut r, &mut seen, &p, label.to_string());
        }
        for name in ["CLAUDE.md", "CLAUDE.local.md"] {
            let p = d.join(".claude").join(name);
            if !file(&p) {
                continue;
            }
            if claude_agents {
                grok_load(
                    &mut r,
                    &mut seen,
                    &p,
                    format!("{label} (Claude compatibility)"),
                );
            } else {
                r.skip(&p, "[compat.claude] agents is off");
            }
        }
        grok_wrong_case(&mut r, d, env.os);
        grok_rules(
            &mut r,
            &mut seen,
            &d.join(".grok").join("rules"),
            &format!("{label} rule"),
        );
        for (vendor, on) in [("claude", claude_rules), ("cursor", cursor_rules)] {
            let dir = d.join(format!(".{vendor}")).join("rules");
            if on {
                grok_rules(
                    &mut r,
                    &mut seen,
                    &dir,
                    &format!("{label} rule ({vendor} compatibility)"),
                );
            } else {
                for f in md_in(&dir) {
                    r.skip(&f, format!("[compat.{vendor}] rules is off"));
                }
            }
        }
    }
    if r.loaded().count() > before {
        r.unverified("Grok Build loads a project's files only once the folder is trusted (`grok --trust` or the prompt); kitbag can't tell whether this one is. Files the repository's .gitignore ignores are skipped too");
    }
    if root.is_none() {
        r.note(
            "not inside a git repository: Grok Build reads only this directory, not its parents",
        );
    }
    if r.loaded().next().is_none() {
        r.note("no instruction files found");
    }
    r.note("every file listed loads, in this order; on a conflict the later (deeper) one wins. Instruction files in subdirectories are read later, when Grok works there");
    r
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn normalize_resolves_dots_by_name() {
        let base = std::env::temp_dir();
        let p = normalize(&base.join("a").join("..").join("b").join(".")).unwrap();
        assert_eq!(p, normalize(&base).unwrap().join("b"));
    }

    #[test]
    fn agents_mode_comes_from_the_plugin_options() {
        let opt = |id: &str, v: &str| json!({"pluginConfigs": {id: {"options": {"instructionFiles": v}}}});
        assert_eq!(claude_agents_mode(None), AgentsMode::Fallback);
        assert_eq!(
            claude_agents_mode(Some(&opt("cc-plugin-agents-md@builtin", "claude-md"))),
            AgentsMode::ClaudeOnly
        );
        assert_eq!(
            claude_agents_mode(Some(&opt("agents-md@builtin", "claude-md-and-agents-md"))),
            AgentsMode::Both
        );
        assert_eq!(
            claude_agents_mode(Some(&opt("cc-plugin-agents-md@builtin", "managed-only"))),
            AgentsMode::ManagedOnly
        );
        assert_eq!(
            claude_agents_mode(Some(&opt("cc-plugin-agents-md@builtin", "something-new"))),
            AgentsMode::Other("something-new".into())
        );
    }

    #[test]
    fn gemini_file_name_is_a_string_or_a_list() {
        assert_eq!(
            gemini_names(Some(&json!({"context": {"fileName": "AGENTS.md"}}))),
            Some(vec!["AGENTS.md".to_string()])
        );
        assert_eq!(
            gemini_names(Some(&json!({"context": {"fileName": ["A.md", "B.md"]}}))),
            Some(vec!["A.md".to_string(), "B.md".to_string()])
        );
        assert_eq!(gemini_names(Some(&json!({}))), None);
    }
}
