//! Memory notes: what an agent has written down about you and your projects.
//!
//! Opt-in. Nothing here runs unless `capture --include memory` is given, and a
//! profile only holds notes if you captured them. Memory often holds personal
//! or client details, and whatever is in it is read into later sessions as
//! context, so a profile from a git source has its notes withheld unless the
//! source is trusted (`--trust-source`), like hooks.
//!
//! On disk the notes are plain Markdown under `memory/<tool>/<scope>/<file>`:
//!
//! ```text
//! memory/claude-code/<project>/MEMORY.md, <topic>.md   ~/.claude/projects/<project>/memory/
//! memory/codex/user/memories/..., memories_v2/...       $CODEX_HOME/memories/, memories_v2/
//! memory/hermes/default/MEMORY.md, USER.md              ~/.hermes/memories/
//! memory/hermes/profile-<name>/MEMORY.md, USER.md       ~/.hermes/profiles/<name>/memories/
//! ```
//!
//! No metadata is invented: the tool is the first path part, the scope the
//! second, and `type` / `modified` stay in the note's own frontmatter where
//! Claude Code put them. Notes are redacted like the rest of a capture.
//!
//! What the docs allow (re-read 2026-11-27):
//!
//! * Claude Code, <https://code.claude.com/docs/en/memory>: auto memory is
//!   `~/.claude/projects/<project>/memory/` with a `MEMORY.md` index and one
//!   topic file per memory (`type` in the frontmatter: `user`, `feedback`,
//!   `project`, `reference`; `modified` an ISO 8601 timestamp), "plain markdown
//!   you can edit or delete at any time". `<project>` is "derived from the git
//!   repository"; the docs give no recipe and no portable key (a remote URL),
//!   so notes are keyed by that folder name, which is a local key. Apply writes
//!   only into a project folder that already exists here with that name, and
//!   never changes a note that is there. `autoMemoryDirectory` (a custom
//!   location, read from managed, local, project or user `settings.json`,
//!   absolute or `~/`) is followed: its notes are kept under the scope
//!   `auto-memory-directory` and applied to the directory in effect on the
//!   target, if that exists; a directory a repository's settings file chose
//!   is skipped while `permissions.blockReadsOutsideWorkingDirectories` is on.
//!   `CLAUDE.local.md` is documented as personal
//!   project *instructions*, not memory; kitbag's instructions code is
//!   unchanged and it is not carried here.
//! * Codex, <https://learn.chatgpt.com/docs/customization/memories> (was
//!   developers.openai.com/codex/memories): off by default (`[features]
//!   memories = true`), `$CODEX_HOME/memories_v2/` for new homes and
//!   `$CODEX_HOME/memories/` for old ones. "Treat these files as generated
//!   state" and don't "rely on editing them by hand", with no import or file
//!   layout documented. They are captured read-only; nothing is written to them.
//! * Hermes, <https://hermes-agent.nousresearch.com/docs/user-guide/features/memory>:
//!   `~/.hermes/memories/MEMORY.md` and `USER.md`, a named profile under
//!   `~/.hermes/profiles/<name>/memories/`, entries separated by `§`, limits
//!   2,200 and 1,375 characters by default (`memory_char_limit` /
//!   `user_char_limit`). The page doesn't forbid editing the files and says
//!   other file tools can edit them; apply appends missing entries.
//!
//! Others: Cursor removed its Memories UI in 2.1 (forum staff pointing to
//! Rules; no docs page), Windsurf's memories are workspace-bound and its docs
//! name no folder, Gemini CLI's auto memory docs give no path, Grok Build's
//! docs show relative paths only, Copilot Memory is cloud-hosted, and
//! Antigravity's knowledge folder has no official page: none of them is read
//! or written.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use anyhow::Result;

use crate::adapters::{Adapter, Env, Level, Note, Scope, Work};
use crate::engine::CaptureResult;
use crate::model::{Profile, SkillFile, TARGETING_KEYS, Targeting};

pub const CLAUDE: &str = "claude-code";
pub const CODEX: &str = "codex";
pub const HERMES: &str = "hermes";

/// Largest note kitbag reads from a tool.
const MAX_NOTE_BYTES: u64 = 256 * 1024;
/// Most notes taken from one tool.
const MAX_NOTES: usize = 400;
/// Hermes's documented default limits, in characters.
const HERMES_MEMORY_LIMIT: usize = 2200;
const HERMES_USER_LIMIT: usize = 1375;

/// Why a target with a memory store of its own gets no notes, by target id.
const NO_STORE: &[(&str, &str)] = &[
    (
        "cursor",
        "Cursor removed its Memories UI in 2.1 and keeps nothing on disk to read",
    ),
    (
        "windsurf",
        "Windsurf's docs name no folder for its workspace-bound memories",
    ),
    (
        "gemini-cli",
        "Gemini CLI's auto memory docs give no storage path",
    ),
    (
        "antigravity",
        "Antigravity has no official page on its knowledge folder",
    ),
    (
        "grok-build",
        "Grok Build's memory page shows no base folder",
    ),
    (
        "copilot",
        "Copilot Memory is stored on GitHub, not on this machine",
    ),
];

/// A path part that is safe to create on any system.
pub fn safe_part(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 200
        && !s.starts_with('.')
        && !s.ends_with('.')
        && !crate::model::is_reserved_on_windows(s)
        && s.chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'))
}

/// Split a profile note path into (tool, scope, file path under the scope),
/// or `None` when it is not `<tool>/<scope>/<file>` with safe parts and `.md`.
pub fn split(path: &str) -> Option<(&str, &str, &str)> {
    let mut it = path.splitn(3, '/');
    let (tool, scope, rest) = (it.next()?, it.next()?, it.next()?);
    let ok = safe_part(tool)
        && safe_part(scope)
        && rest.ends_with(".md")
        && rest.split('/').all(safe_part);
    ok.then_some((tool, scope, rest))
}

/// Problems with the notes of a loaded profile (unsafe paths, not Markdown).
pub fn issues(p: &Profile) -> Vec<String> {
    p.memory
        .iter()
        .filter(|f| split(&f.path).is_none())
        .map(|f| {
            format!(
                "memory/{}: expected memory/<tool>/<scope>/<name>.md with plain file names",
                f.path
            )
        })
        .collect()
}

/// The note as plans and captures show it: its path, with the `type` and
/// `modified` its own frontmatter gives.
pub fn describe(f: &SkillFile) -> String {
    let text = String::from_utf8_lossy(&f.content);
    let data = crate::frontmatter::parse(&text).data;
    let get = |k: &str| data.get(k).and_then(|v| v.as_str()).map(str::to_string);
    let extra: Vec<String> = [("type", get("type")), ("modified", get("modified"))]
        .into_iter()
        .filter_map(|(k, v)| v.map(|v| format!("{k}: {v}")))
        .collect();
    if extra.is_empty() {
        format!("memory/{}", f.path)
    } else {
        format!("memory/{} ({})", f.path, extra.join(", "))
    }
}

fn info(target: &str, message: String) -> Note {
    Note {
        level: Level::Info,
        target: target.to_string(),
        message,
    }
}

fn warn(target: &str, message: String) -> Note {
    Note {
        level: Level::Warn,
        target: target.to_string(),
        message,
    }
}

fn claude_dir(env: &Env) -> PathBuf {
    env.location_var("CLAUDE_CONFIG_DIR")
        .unwrap_or_else(|| env.home.join(".claude"))
}

fn codex_dir(env: &Env) -> PathBuf {
    env.location_var("CODEX_HOME")
        .unwrap_or_else(|| env.home.join(".codex"))
}

fn hermes_dir(env: &Env) -> PathBuf {
    env.location_var("HERMES_HOME")
        .unwrap_or_else(|| env.home.join(".hermes"))
}

/// Profile scope for notes kept in a custom `autoMemoryDirectory`. The
/// directory itself holds `MEMORY.md` and the topic files; the scope is not a
/// project folder name, so it maps to whatever directory is in effect on the
/// machine the profile is applied to.
pub const CUSTOM_SCOPE: &str = "auto-memory-directory";

/// Where Claude Code's managed (policy) settings file is, read-only here.
/// `KITBAG_MANAGED_SETTINGS_DIR` replaces the system folder (tests, odd installs).
fn managed_settings(env: &Env) -> PathBuf {
    if let Some(d) = env
        .vars
        .get("KITBAG_MANAGED_SETTINGS_DIR")
        .filter(|v| !v.is_empty())
    {
        return PathBuf::from(d).join("managed-settings.json");
    }
    match env.os {
        crate::adapters::Os::Linux => PathBuf::from("/etc/claude-code/managed-settings.json"),
        crate::adapters::Os::MacOs => {
            PathBuf::from("/Library/Application Support/ClaudeCode/managed-settings.json")
        }
        crate::adapters::Os::Windows => {
            PathBuf::from(r"C:\Program Files\ClaudeCode\managed-settings.json")
        }
    }
}

/// What the settings say about a custom auto memory directory.
#[derive(Debug, PartialEq, Eq)]
pub enum CustomDir {
    /// `autoMemoryDirectory` is not set in any scope.
    Unset,
    /// The directory Claude Code uses.
    Dir(PathBuf),
    /// Set, but not followed; the reason is for the plan's notes.
    Skipped(String),
}

/// Resolve `autoMemoryDirectory` from the documented scopes, highest
/// precedence first: managed policy, the project's `settings.local.json` and
/// `settings.json`, then the user's `settings.json`. (`--settings` is not
/// something kitbag can see.) `read` gives a file's text, if there is one.
/// The project is the one kitbag runs in. These files are only read to find
/// the directory; none is written or carried.
pub fn custom_dir(env: &Env, read: &dyn Fn(&Path) -> Option<String>) -> CustomDir {
    // (file, supplied by the repository)
    let files = [
        (managed_settings(env), false),
        (
            env.project.join(".claude").join("settings.local.json"),
            true,
        ),
        (env.project.join(".claude").join("settings.json"), true),
        (claude_dir(env).join("settings.json"), false),
    ];
    let mut dir: Option<(serde_json::Value, bool, PathBuf)> = None;
    let mut block: Option<bool> = None;
    for (path, repo) in files {
        let Some(text) = read(&path) else { continue };
        let Ok(v) = serde_json::from_str::<serde_json::Value>(text.trim_start_matches('\u{feff}'))
        else {
            continue;
        };
        if let (None, Some(d)) = (&dir, v.get("autoMemoryDirectory")) {
            dir = Some((d.clone(), repo, path.clone()));
        }
        if block.is_none() {
            block = v
                .get("permissions")
                .and_then(|p| p.get("blockReadsOutsideWorkingDirectories"))
                .and_then(|b| b.as_bool());
        }
    }
    let Some((value, repo, from)) = dir else {
        return CustomDir::Unset;
    };
    let Some(raw) = value.as_str() else {
        return CustomDir::Skipped(format!(
            "autoMemoryDirectory in {} is not a string, so it is not followed",
            from.display()
        ));
    };
    if repo && block == Some(true) {
        return CustomDir::Skipped(format!(
            "autoMemoryDirectory in {} is chosen by a repository's settings file and permissions.blockReadsOutsideWorkingDirectories is on: Claude Code loads and saves no auto memory there, so kitbag does not read or write it",
            from.display()
        ));
    }
    let resolved = if let Some(rest) = raw.strip_prefix("~/") {
        Some(crate::plan_fs::join_rel(
            &env.home,
            rest.trim_end_matches('/'),
        ))
    } else if Path::new(raw).is_absolute() {
        Some(PathBuf::from(raw))
    } else {
        None
    };
    match resolved {
        Some(p) => CustomDir::Dir(p),
        None => CustomDir::Skipped(format!(
            "autoMemoryDirectory \"{raw}\" in {} is not an absolute path and does not start with ~/, so it is not followed (the default project memory folders are used)",
            from.display()
        )),
    }
}

/// Sorted names of the real directories directly in `dir`.
fn subdirs(dir: &Path) -> Vec<String> {
    let mut out: Vec<String> = std::fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .filter(|e| e.file_type().is_ok_and(|t| t.is_dir()))
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .collect();
    out.sort();
    out
}

/// Where captured notes go while one capture runs.
struct Sink<'a> {
    out: &'a mut CaptureResult,
    count: BTreeMap<&'static str, usize>,
}

impl Sink<'_> {
    /// Read the Markdown files under `dir` (all of them, or only `only`) into
    /// notes at `<tool>/<scope>/<prefix><file>`, redacted.
    fn read_dir(
        &mut self,
        tool: &'static str,
        scope: &str,
        prefix: &str,
        dir: &Path,
        only: Option<&[&str]>,
    ) {
        let Ok(walk) = crate::plan_fs::walk_confined(dir, MAX_NOTES) else {
            return;
        };
        self.out.sources.push(dir.to_path_buf());
        for s in walk.skipped {
            self.out
                .notes
                .push(warn(tool, format!("memory: skipped {s}")));
        }
        for rel in walk.files {
            if !rel.ends_with(".md") || only.is_some_and(|o| !o.contains(&rel.as_str())) {
                continue;
            }
            if self.count.get(tool).copied().unwrap_or(0) >= MAX_NOTES {
                self.out
                    .notes
                    .push(warn(tool, format!("memory: stopped at {MAX_NOTES} notes")));
                return;
            }
            let path = format!("{tool}/{scope}/{prefix}{rel}");
            if split(&path).is_none() {
                self.out.notes.push(warn(
                    tool,
                    format!("memory: skipped {path}: the profile can't hold that file name"),
                ));
                continue;
            }
            let real = dir.join(&rel);
            if std::fs::metadata(&real).is_ok_and(|m| m.len() > MAX_NOTE_BYTES) {
                self.out.notes.push(warn(
                    tool,
                    format!(
                        "memory: skipped {path}: larger than {} KiB",
                        MAX_NOTE_BYTES / 1024
                    ),
                ));
                continue;
            }
            let text = match crate::plan_fs::read_text_opt(&real) {
                Ok(Some(t)) => t,
                Ok(None) => continue,
                Err(e) => {
                    self.out
                        .notes
                        .push(warn(tool, format!("memory: skipped {path}: {e:#}")));
                    continue;
                }
            };
            let (text, kinds) = crate::secrets::redact_text(&text);
            if !kinds.is_empty() {
                let mut kinds = kinds;
                kinds.sort_unstable();
                kinds.dedup();
                self.out.notes.push(warn(
                    tool,
                    format!("memory: redacted {} from {path}", kinds.join(", ")),
                ));
            }
            *self.count.entry(tool).or_insert(0) += 1;
            self.out.profile.memory.push(SkillFile {
                path,
                content: text.into_bytes(),
                executable: false,
            });
        }
    }
}

/// Capture the notes of the selected tools into `out` (`capture --include
/// memory`). User scope only: every store here is per user.
pub fn capture(targets: &[&dyn Adapter], env: &Env, out: &mut CaptureResult) -> Result<()> {
    if env.scope != Scope::User {
        out.notes.push(info(
            "profile",
            "memory is only captured at user scope: the notes live in your home directory, not in the project".to_string(),
        ));
        return Ok(());
    }
    let mut sink = Sink {
        out,
        count: BTreeMap::new(),
    };
    for a in targets {
        match a.id() {
            CLAUDE => {
                let projects = claude_dir(env).join("projects");
                for p in subdirs(&projects) {
                    let mem = projects.join(&p).join("memory");
                    if !mem.is_dir() {
                        continue;
                    }
                    if !safe_part(&p) {
                        sink.out.notes.push(warn(
                            CLAUDE,
                            format!("memory: skipped project folder '{p}': the profile can't hold that name"),
                        ));
                        continue;
                    }
                    sink.read_dir(CLAUDE, &p, "", &mem, None);
                }
                match custom_dir(env, &|p| crate::plan_fs::read_text_opt(p).ok().flatten()) {
                    CustomDir::Unset => {}
                    CustomDir::Skipped(why) => sink.out.notes.push(info(CLAUDE, format!("memory: {why}"))),
                    CustomDir::Dir(d) if d.is_dir() => {
                        sink.out.notes.push(info(
                            CLAUDE,
                            format!("memory: following autoMemoryDirectory {}", d.display()),
                        ));
                        sink.read_dir(CLAUDE, CUSTOM_SCOPE, "", &d, None);
                    }
                    CustomDir::Dir(d) => sink.out.notes.push(info(
                        CLAUDE,
                        format!("memory: autoMemoryDirectory {} doesn't exist, nothing to capture from it", d.display()),
                    )),
                }
                if sink.count.contains_key(CLAUDE) {
                    sink.out.notes.push(info(
                        CLAUDE,
                        "memory: notes are keyed by Claude Code's own project folder name (derived from the repository and the path it is at; the docs give no portable key such as the remote URL), so they map to a project only on a machine that has a folder of the same name".to_string(),
                    ));
                }
            }
            CODEX => {
                for d in ["memories", "memories_v2"] {
                    let dir = codex_dir(env).join(d);
                    if dir.is_dir() {
                        sink.read_dir(CODEX, "user", &format!("{d}/"), &dir, None);
                    }
                }
                if sink.count.contains_key(CODEX) {
                    sink.out.notes.push(info(
                        CODEX,
                        "memory: Codex's notes are generated state that its docs say not to edit by hand, with no import layout, so they are kept as a read-only copy and never written back".to_string(),
                    ));
                }
            }
            HERMES => {
                let base = hermes_dir(env);
                let files = ["MEMORY.md", "USER.md"];
                let dir = base.join("memories");
                if dir.is_dir() {
                    sink.read_dir(HERMES, "default", "", &dir, Some(&files));
                }
                let profiles = base.join("profiles");
                for p in subdirs(&profiles) {
                    let dir = profiles.join(&p).join("memories");
                    if dir.is_dir() && safe_part(&p) {
                        sink.read_dir(HERMES, &format!("profile-{p}"), "", &dir, Some(&files));
                    }
                }
            }
            id => {
                if let Some((_, why)) = NO_STORE.iter().find(|(t, _)| *t == id) {
                    sink.out
                        .notes
                        .push(info(id, format!("memory: nothing to capture: {why}")));
                }
            }
        }
    }
    out.profile.memory.sort_by(|a, b| a.path.cmp(&b.path));
    let n = out.profile.memory.len();
    if n > 0 {
        out.notes.push(warn(
            "profile",
            format!(
                "captured {n} memory note(s) (--include memory). Notes can hold personal or client details: secrets were redacted, but read memory/ before committing or sharing this profile"
            ),
        ));
        let shown: Vec<String> = out.profile.memory.iter().take(30).map(describe).collect();
        for s in &shown {
            out.notes.push(info("profile", format!("  {s}")));
        }
        if n > shown.len() {
            out.notes.push(info(
                "profile",
                format!("  ... and {} more", n - shown.len()),
            ));
        }
    } else {
        out.notes.push(info(
            "profile",
            "no memory notes found for the selected tools".to_string(),
        ));
    }
    Ok(())
}

/// Hermes entries: text between `§` marks, trimmed, empty ones dropped.
pub fn hermes_entries(text: &str) -> Vec<String> {
    text.split('§')
        .map(str::trim)
        .filter(|e| !e.is_empty())
        .map(str::to_string)
        .collect()
}

/// Add the entries of `want` that `current` lacks, as new `§`-separated
/// entries after the existing text, and stop at `limit` characters. Returns
/// the text and how many entries were added and left out.
pub fn hermes_merge(current: &str, want: &str, limit: usize) -> (String, usize, usize) {
    let have = hermes_entries(current);
    let mut text = current.trim_end().to_string();
    let (mut added, mut left_out) = (0, 0);
    for e in hermes_entries(want) {
        if have.contains(&e) {
            continue;
        }
        let next = if text.is_empty() {
            e.clone()
        } else {
            format!("{text}\n§\n{e}")
        };
        if next.chars().count() > limit {
            left_out += 1;
            continue;
        }
        text = next;
        added += 1;
    }
    if added > 0 {
        text.push('\n');
    }
    (text, added, left_out)
}

/// The kitbag frontmatter key that opts a note into being folded into
/// instructions (off by default; capture never sets it).
pub const FOLD_KEY: &str = "fold_into_instructions";

/// What kitbag keeps in a note's own frontmatter: where it applies and whether
/// it is folded into instructions. Both are optional.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Settings {
    pub targeting: Targeting,
    pub fold: bool,
}

/// A note's kitbag settings, its content without exactly those keys (what a
/// tool's memory file gets), and the (errors, warnings) found reading them.
pub fn settings(f: &SkillFile) -> (Settings, Vec<u8>, Vec<String>, Vec<String>) {
    let Ok(text) = std::str::from_utf8(&f.content) else {
        return (Settings::default(), f.content.clone(), vec![], vec![]);
    };
    let mentions = TARGETING_KEYS
        .iter()
        .chain(std::iter::once(&FOLD_KEY))
        .any(|k| text.contains(k));
    if !mentions {
        return (Settings::default(), f.content.clone(), vec![], vec![]);
    }
    let mut data = crate::frontmatter::parse(text).data;
    let before: Vec<String> = data.keys().cloned().collect();
    let (targeting, mut errors, warnings) = Targeting::take_from(
        &mut data,
        &crate::adapters::ids(),
        |id| crate::adapters::canonical_id(id).to_string(),
        "note",
    );
    let fold = match data.remove(FOLD_KEY) {
        None => false,
        Some(serde_json::Value::Bool(b)) => b,
        Some(serde_json::Value::String(s)) if s == "true" || s == "false" => s == "true",
        Some(_) => {
            errors.push(format!("'{FOLD_KEY}' must be true or false"));
            false
        }
    };
    // Remove exactly the keys that were read as kitbag's (a legacy `only` that
    // is another tool's key, say, stays).
    let mut taken: Vec<&str> = before
        .iter()
        .filter(|k| !data.contains_key(*k))
        .map(String::as_str)
        .collect();
    taken.retain(|k| TARGETING_KEYS.contains(k) || *k == FOLD_KEY);
    let content = match crate::frontmatter::remove_keys(text, &taken) {
        Some(stripped) => stripped.into_bytes(),
        None => f.content.clone(),
    };
    (Settings { targeting, fold }, content, errors, warnings)
}

/// Problems with the kitbag settings of a loaded profile's notes.
pub fn setting_issues(p: &Profile) -> (Vec<String>, Vec<String>) {
    let (mut errors, mut warnings) = (Vec::new(), Vec::new());
    for f in &p.memory {
        let (_, _, e, w) = settings(f);
        errors.extend(e.into_iter().map(|e| format!("memory/{}: {e}", f.path)));
        warnings.extend(w.into_iter().map(|w| format!("memory/{}: {w}", f.path)));
    }
    (errors, warnings)
}

/// A re-capture keeps the kitbag settings of the notes already in the
/// profile (capture itself never sets them). Returns how many notes had some.
pub fn keep_settings(new: &mut Profile, old: &Profile) -> usize {
    let mut n = 0;
    for f in &mut new.memory {
        let Some(o) = old.memory.iter().find(|o| o.path == f.path) else {
            continue;
        };
        let (s, _, _, _) = settings(o);
        let mut entries = s.targeting.entries();
        if s.fold {
            entries.push((FOLD_KEY, serde_json::Value::Bool(true)));
        }
        if entries.is_empty() {
            continue;
        }
        if let Ok(text) = std::str::from_utf8(&f.content) {
            f.content = crate::frontmatter::add_entries(text, &entries).into_bytes();
            n += 1;
        }
    }
    n
}

/// Tools whose memory notes kitbag writes to a store of their own (or reads
/// read-only), so their notes are never folded into instructions.
fn has_store(target: &str) -> bool {
    matches!(target, CLAUDE | HERMES | CODEX)
}

/// The profile for `target` with the notes marked `fold_into_instructions`
/// appended to its instructions, when `target` has no memory store of its own
/// but kitbag writes instructions for it (`instructions_supported`); `None`
/// when nothing is folded. The instructions are then written by the tool's own
/// adapter, to the file and in the managed block it already uses.
pub fn fold_into(
    profile: &Profile,
    target: &str,
    instructions_supported: bool,
    env: &Env,
    work: &mut Work,
) -> Option<Profile> {
    if env.scope != Scope::User || has_store(target) || profile.memory.is_empty() {
        return None;
    }
    let mut parts = Vec::new();
    for f in &profile.memory {
        let (s, content, _, _) = settings(f);
        if !s.fold {
            continue;
        }
        let Some((tool, _, _)) = split(&f.path) else {
            continue;
        };
        if tool == CODEX {
            work.info(
                target,
                format!(
                    "memory/{}: not folded into instructions, Codex's memory files are generated state",
                    f.path
                ),
            );
            continue;
        }
        if let Some(why) = s.targeting.skip_reason(target) {
            work.info(
                target,
                format!(
                    "memory/{}: not folded into {target}'s instructions: {why}",
                    f.path
                ),
            );
            continue;
        }
        if !instructions_supported {
            work.info(
                target,
                format!(
                    "memory/{}: not folded, kitbag writes no instructions file for {target} at this scope",
                    f.path
                ),
            );
            continue;
        }
        if marked(f) {
            work.warn(
                target,
                format!(
                    "memory/{}: not folded into instructions, it holds [REDACTED] markers where capture removed secrets",
                    f.path
                ),
            );
            continue;
        }
        let body = crate::frontmatter::parse(&String::from_utf8_lossy(&content)).body;
        let body = body.trim();
        if body.is_empty() {
            continue;
        }
        work.info(
            target,
            format!("memory: folds {} into the instructions", describe(f)),
        );
        parts.push(format!("### Memory: {}\n\n{body}", f.path));
    }
    if parts.is_empty() {
        return None;
    }
    let mut p = profile.clone();
    let mut text = p.instructions.take().unwrap_or_default();
    if !text.trim().is_empty() {
        text = format!("{}\n\n", text.trim_end());
    }
    text.push_str(&parts.join("\n\n"));
    text.push('\n');
    p.instructions = Some(text);
    Some(p)
}

fn marked(f: &SkillFile) -> bool {
    std::str::from_utf8(&f.content).is_ok_and(crate::secrets::has_redaction_marker)
}

/// Plan the writes of the profile's notes for the `applied` targets (user
/// scope; the notes are listed in the plan's notes either way).
pub fn plan(profile: &Profile, applied: &[String], env: &Env, work: &mut Work) -> Result<()> {
    if profile.memory.is_empty() {
        return Ok(());
    }
    if env.scope != Scope::User {
        work.info(
            "profile",
            format!(
                "{} memory note(s) not applied: memory is applied at user scope only",
                profile.memory.len()
            ),
        );
        return Ok(());
    }
    // What a tool's memory file gets has no kitbag settings in it, and a note
    // its settings keep off its own tool is skipped.
    let stripped: Vec<SkillFile> = profile
        .memory
        .iter()
        .map(|f| SkillFile {
            content: settings(f).1,
            ..f.clone()
        })
        .collect();
    let mut by_tool: BTreeMap<&str, Vec<&SkillFile>> = BTreeMap::new();
    for (f, orig) in stripped.iter().zip(&profile.memory) {
        match split(&f.path) {
            Some((tool, _, _)) => {
                if let Some(why) = settings(orig).0.targeting.skip_reason(tool)
                    && applied.iter().any(|t| t == tool)
                {
                    work.info(tool, format!("memory/{}: skipped on {tool}: {why}", f.path));
                    continue;
                }
                by_tool.entry(tool).or_default().push(f);
            }
            None => work.warn(
                "profile",
                format!("memory/{}: not a valid note path, skipped", f.path),
            ),
        }
    }
    for (tool, files) in by_tool {
        if !applied.iter().any(|t| t == tool) {
            work.info(
                tool,
                format!(
                    "{} memory note(s) for {tool} not applied: it is not among the targets",
                    files.len()
                ),
            );
            continue;
        }
        match tool {
            CLAUDE => plan_claude(&files, env, work)?,
            HERMES => plan_hermes(&files, env, work)?,
            CODEX => work.info(
                CODEX,
                format!(
                    "{} memory note(s) not written: Codex's docs call its memory files generated state, not for hand-editing, and document no import layout",
                    files.len()
                ),
            ),
            other => work.info(
                other,
                format!(
                    "{} memory note(s) not written: no documented memory layout to write them to",
                    files.len()
                ),
            ),
        }
    }
    Ok(())
}

fn plan_claude(files: &[&SkillFile], env: &Env, work: &mut Work) -> Result<()> {
    let projects = claude_dir(env).join("projects");
    let custom = custom_dir(env, &|p| work.fs.read(p).ok().flatten());
    let managed_dir = managed_settings(env).parent().map(Path::to_path_buf);
    let (mut wrote, mut same, mut kept, mut missing) = (0, 0, Vec::new(), BTreeMap::new());
    let mut unfollowed: BTreeMap<String, usize> = BTreeMap::new();
    let mut index_stale = false;
    for f in files {
        let Some((_, project, rel)) = split(&f.path) else {
            continue;
        };
        if marked(f) {
            work.warn(
                CLAUDE,
                format!(
                    "memory/{}: not written, it holds [REDACTED] markers where capture removed secrets",
                    f.path
                ),
            );
            continue;
        }
        let base = if project == CUSTOM_SCOPE {
            match &custom {
                CustomDir::Dir(d) if managed_dir.as_ref().is_some_and(|m| d.starts_with(m)) => {
                    *unfollowed
                        .entry(
                            "it is in the managed-policy folder, which kitbag never writes"
                                .to_string(),
                        )
                        .or_insert(0) += 1;
                    continue;
                }
                CustomDir::Dir(d) if work.fs.exists(d) => d.clone(),
                CustomDir::Dir(d) => {
                    *unfollowed
                        .entry(format!("{} doesn't exist here", d.display()))
                        .or_insert(0) += 1;
                    continue;
                }
                CustomDir::Skipped(why) => {
                    *unfollowed.entry(why.clone()).or_insert(0) += 1;
                    continue;
                }
                CustomDir::Unset => {
                    *unfollowed
                        .entry("no autoMemoryDirectory is set here".to_string())
                        .or_insert(0) += 1;
                    continue;
                }
            }
        } else {
            let dir = projects.join(project);
            if !work.fs.exists(&dir) {
                *missing.entry(project.to_string()).or_insert(0) += 1;
                continue;
            }
            dir.join("memory")
        };
        let dest = crate::plan_fs::join_rel(&base, rel);
        match work.fs.read_bytes(&dest)? {
            Some(cur) if cur == f.content => same += 1,
            Some(_) => {
                if rel == "MEMORY.md" {
                    index_stale = true;
                }
                kept.push(describe(f));
            }
            None => {
                work.fs.write(&dest, f.content.clone())?;
                work.info(CLAUDE, format!("memory: writes {}", describe(f)));
                wrote += 1;
            }
        }
    }
    for (project, n) in &missing {
        work.info(
            CLAUDE,
            format!(
                "memory: {n} note(s) for project folder '{project}' not written: {} has no such folder here (Claude Code makes it the first time it runs in that project)",
                projects.display()
            ),
        );
    }
    for (why, n) in &unfollowed {
        work.info(
            CLAUDE,
            format!("memory: {n} note(s) from a custom autoMemoryDirectory not written: {why}"),
        );
    }
    if !kept.is_empty() {
        work.info(
            CLAUDE,
            format!(
                "memory: kept {} note(s) that already exist and differ (Claude Code's own are never overwritten): {}",
                kept.len(),
                kept.join(", ")
            ),
        );
    }
    if wrote > 0 && index_stale {
        work.info(
            CLAUDE,
            "memory: the existing MEMORY.md was kept, so the notes written may not be listed in it"
                .to_string(),
        );
    }
    if wrote + same > 0 {
        work.info(
            CLAUDE,
            format!("memory: {wrote} note(s) written, {same} already in place"),
        );
    }
    Ok(())
}

fn plan_hermes(files: &[&SkillFile], env: &Env, work: &mut Work) -> Result<()> {
    let base = hermes_dir(env);
    if !work.fs.exists(&base) {
        work.info(
            HERMES,
            format!(
                "memory: {} note(s) not written: {} doesn't exist (Hermes isn't set up here)",
                files.len(),
                base.display()
            ),
        );
        return Ok(());
    }
    for f in files {
        let Some((_, scope, rel)) = split(&f.path) else {
            continue;
        };
        let limit = match rel {
            "MEMORY.md" => HERMES_MEMORY_LIMIT,
            "USER.md" => HERMES_USER_LIMIT,
            _ => {
                work.info(
                    HERMES,
                    format!(
                        "memory/{}: not written, Hermes keeps only MEMORY.md and USER.md",
                        f.path
                    ),
                );
                continue;
            }
        };
        let home = match scope.strip_prefix("profile-") {
            Some(p) => base.join("profiles").join(p),
            None if scope == "default" => base.clone(),
            None => {
                work.info(
                    HERMES,
                    format!("memory/{}: unknown scope '{scope}', skipped", f.path),
                );
                continue;
            }
        };
        if !work.fs.exists(&home) {
            work.info(
                HERMES,
                format!(
                    "memory/{}: not written, {} doesn't exist here",
                    f.path,
                    home.display()
                ),
            );
            continue;
        }
        let dest = home.join("memories").join(rel);
        let cur = work.fs.read(&dest)?.unwrap_or_default();
        let want = String::from_utf8_lossy(&f.content);
        let (want, dropped) = without_marked_entries(&want);
        if dropped > 0 {
            work.warn(
                HERMES,
                format!(
                    "memory/{}: {dropped} entry(ies) not written, they hold [REDACTED] markers where capture removed secrets",
                    f.path
                ),
            );
        }
        let (text, added, left_out) = hermes_merge(&cur, &want, limit);
        if left_out > 0 {
            work.warn(
                HERMES,
                format!(
                    "memory/{}: {left_out} entry(ies) left out: {} would pass Hermes's default limit of {limit} characters (raise memory_char_limit / user_char_limit and apply again)",
                    f.path,
                    rel
                ),
            );
        }
        if added > 0 {
            work.fs.write(&dest, text.into_bytes())?;
            work.info(
                HERMES,
                format!(
                    "memory: adds {added} entry(ies) to {} ({})",
                    dest.display(),
                    describe(f)
                ),
            );
        }
    }
    Ok(())
}

/// The entries of a Hermes file without those holding redaction markers,
/// and how many were left out.
fn without_marked_entries(text: &str) -> (String, usize) {
    let entries = hermes_entries(text);
    let total = entries.len();
    let keep: Vec<String> = entries
        .into_iter()
        .filter(|e| !crate::secrets::has_redaction_marker(e))
        .collect();
    let dropped = total - keep.len();
    (keep.join("\n§\n"), dropped)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn splits_note_paths() {
        assert_eq!(
            split("claude-code/-home-me-proj/user_role.md"),
            Some(("claude-code", "-home-me-proj", "user_role.md"))
        );
        // `-` first is fine for a scope, `.`-first and `..` are not.
        assert!(split("codex/user/memories/a/b.md").is_some());
        assert!(split("claude-code/../x.md").is_none());
        assert!(split("claude-code/p/../x.md").is_none());
        assert!(split("claude-code/p/.hidden.md").is_none());
        assert!(split("claude-code/p/a b.md").is_none());
        assert!(split("claude-code/p/a.txt").is_none());
        assert!(split("claude-code/p/nul.md").is_none());
        assert!(split("claude-code/p").is_none());
        assert!(split("claude-code/p/a\\b.md").is_none());
    }

    #[test]
    fn describes_a_note_with_the_metadata_it_has() {
        let f = SkillFile {
            path: "claude-code/p/user_role.md".into(),
            content: b"---\nname: role\ntype: user\nmodified: 2026-11-01T10:00:00Z\n---\nhi\n"
                .to_vec(),
            executable: false,
        };
        assert_eq!(
            describe(&f),
            "memory/claude-code/p/user_role.md (type: user, modified: 2026-11-01T10:00:00Z)"
        );
        let g = SkillFile {
            path: "claude-code/p/MEMORY.md".into(),
            content: b"- [Role](user_role.md)\n".to_vec(),
            executable: false,
        };
        assert_eq!(describe(&g), "memory/claude-code/p/MEMORY.md");
    }

    #[test]
    fn hermes_merge_adds_only_missing_entries() {
        let (t, added, out) = hermes_merge("one\n§\ntwo\n", "two\n§\nthree", 100);
        assert_eq!((added, out), (1, 0));
        assert_eq!(t, "one\n§\ntwo\n§\nthree\n");
        let (t, added, _) = hermes_merge("", "a\n§\nb", 100);
        assert_eq!((t.as_str(), added), ("a\n§\nb\n", 2));
        // Nothing to add leaves the text alone.
        let (t, added, _) = hermes_merge("a\n§\nb\n", "b", 100);
        assert_eq!((t.as_str(), added), ("a\n§\nb", 0));
    }

    #[test]
    fn hermes_merge_stops_at_the_limit() {
        let (t, added, out) = hermes_merge("abc", "defg\n§\nh", 8);
        // "abc\n§\nh" is 7 characters; "abc\n§\ndefg" is 10.
        assert_eq!((added, out), (1, 1));
        assert!(t.starts_with("abc\n§\nh"));
    }
}
