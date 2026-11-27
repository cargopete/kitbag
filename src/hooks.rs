//! Hooks: shell commands a tool runs on events.
//!
//! A profile holds them as `[[hooks]]` in `kitbag.toml`, in a neutral shape
//! (event, matcher, command, timeout, and the tool's other keys in `extra`),
//! plus the scripts they run in `hooks/`. Hooks run automatically, so they are
//! opt-in at every step:
//!
//! * `capture --include hooks` reads them (without it they are only counted);
//! * `apply --apply-hooks` writes them, and a profile from a git source needs
//!   `--trust-source` as well;
//! * every hook command is shown in full in the plan.
//!
//! What an apply wrote is remembered in `applied-hooks.json` (state directory),
//! so `kitbag doctor` can say when a hook no longer matches it.
//!
//! Only Claude Code carries hooks so far.
//!
//! * <https://code.claude.com/docs/en/hooks>

use std::collections::BTreeMap;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::adapters::{Env, Scope};
use crate::engine::Plan;
use crate::model::Hook;
use crate::sync::entry_hash;
use crate::txn::{Status, Store, sha256, write_atomic};

/// What stands for the directory the scripts are written to, in a hook command
/// of a profile (`{{hooks_dir}}/check.sh` runs `hooks/check.sh` of the profile).
pub const DIR_PLACEHOLDER: &str = "{{hooks_dir}}";

/// Neutral event names and the Claude Code event each stands for.
pub const EVENTS: &[(&str, &str)] = &[
    ("session-start", "SessionStart"),
    ("session-end", "SessionEnd"),
    ("prompt-submit", "UserPromptSubmit"),
    ("pre-tool", "PreToolUse"),
    ("post-tool", "PostToolUse"),
    ("post-tool-failure", "PostToolUseFailure"),
    ("permission-request", "PermissionRequest"),
    ("notification", "Notification"),
    ("stop", "Stop"),
    ("subagent-start", "SubagentStart"),
    ("subagent-stop", "SubagentStop"),
    ("pre-compact", "PreCompact"),
    ("post-compact", "PostCompact"),
];

/// Every Claude Code hook event the documentation lists. Those without a
/// neutral name are carried as `claude-code:<Name>` and apply to Claude Code only.
pub const CLAUDE_EVENTS: &[&str] = &[
    "SessionStart",
    "Setup",
    "UserPromptSubmit",
    "UserPromptExpansion",
    "PreSkillShellCommand",
    "PreToolUse",
    "PermissionRequest",
    "PermissionDenied",
    "PostToolUse",
    "PostToolUseFailure",
    "PostToolBatch",
    "Notification",
    "MessageDisplay",
    "SubagentStart",
    "SubagentStop",
    "TaskCreated",
    "TaskCompleted",
    "Stop",
    "StopFailure",
    "TeammateIdle",
    "InstructionsLoaded",
    "ConfigChange",
    "CwdChanged",
    "DirectoryAdded",
    "FileChanged",
    "WorktreeCreate",
    "WorktreeRemove",
    "PreCompact",
    "PostCompact",
    "PreModelSwitch",
    "PostModelSwitch",
    "Elicitation",
    "ElicitationResult",
    "SessionEnd",
];

const CLAUDE_PREFIX: &str = "claude-code:";

/// The Claude Code event for a neutral event name, or `None` when it has none.
pub fn claude_event(event: &str) -> Option<&'static str> {
    if let Some(name) = event.strip_prefix(CLAUDE_PREFIX) {
        return CLAUDE_EVENTS.iter().copied().find(|e| *e == name);
    }
    EVENTS.iter().find(|(n, _)| *n == event).map(|(_, c)| *c)
}

/// The profile's name for a Claude Code event, or `None` when kitbag doesn't know the event.
pub fn neutral_event(claude: &str) -> Option<String> {
    if let Some((n, _)) = EVENTS.iter().find(|(_, c)| *c == claude) {
        return Some((*n).to_string());
    }
    CLAUDE_EVENTS
        .contains(&claude)
        .then(|| format!("{CLAUDE_PREFIX}{claude}"))
}

/// Characters a script path in the profile may have, so the command stays
/// correct without quoting wherever it is written.
pub fn safe_script_path(rel: &str) -> bool {
    !rel.is_empty()
        && rel
            .split('/')
            .all(|p| !p.is_empty() && p != "." && p != "..")
        && rel
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-' | '/' | '+' | '@'))
}

/// The strings of a hook that may name a script: its command and its `args`.
fn strings_mut(h: &mut Hook) -> impl Iterator<Item = &mut String> {
    let args = match h.extra.get_mut("args") {
        Some(Value::Array(a)) => Some(a),
        _ => None,
    };
    std::iter::once(&mut h.command).chain(args.into_iter().flat_map(|a| a.iter_mut()).filter_map(
        |v| match v {
            Value::String(s) => Some(s),
            _ => None,
        },
    ))
}

/// Paths (relative to `hooks/`) of the profile scripts a hook runs.
pub fn referenced_files(h: &Hook) -> Vec<String> {
    let mut texts: Vec<&str> = vec![h.command.as_str()];
    if let Some(Value::Array(a)) = h.extra.get("args") {
        texts.extend(a.iter().filter_map(Value::as_str));
    }
    let mut out = Vec::new();
    let marker = format!("{DIR_PLACEHOLDER}/");
    for t in texts {
        let mut rest = t;
        while let Some(i) = rest.find(&marker) {
            let after = &rest[i + marker.len()..];
            let end = after
                .find(|c: char| {
                    !(c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-' | '/' | '+' | '@'))
                })
                .unwrap_or(after.len());
            let rel = after[..end].to_string();
            if !out.contains(&rel) {
                out.push(rel);
            }
            rest = &after[end..];
        }
    }
    out
}

fn plain_path_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-' | '/' | '+' | '@' | ':' | '~')
}

/// Put `dir` in place of the placeholder in a shell command line: bare when
/// it has nothing the shell would act on, otherwise in double quotes (or as
/// it is, when the placeholder is already quoted).
fn insert_dir_in_shell(cmd: &str, dir: &str, plain: bool) -> String {
    let mut out = String::new();
    let mut quote: Option<char> = None;
    let mut rest = cmd;
    while !rest.is_empty() {
        if rest.starts_with(DIR_PLACEHOLDER) {
            if plain || quote.is_some() {
                out.push_str(dir);
            } else {
                out.push('"');
                out.push_str(dir);
                out.push('"');
            }
            rest = &rest[DIR_PLACEHOLDER.len()..];
            continue;
        }
        let c = rest.chars().next().expect("not empty");
        match (quote, c) {
            (None, '\'' | '"') => quote = Some(c),
            (Some(q), c) if c == q => quote = None,
            _ => {}
        }
        out.push(c);
        rest = &rest[c.len_utf8()..];
    }
    out
}

/// `h` with the script directory placeholder replaced by `dir` (`/`-separated),
/// or `None` when it uses the placeholder and `dir` has characters that
/// can't be put in a command safely (a quote, a `$`, a line break).
pub fn with_script_dir(h: &Hook, dir: &str) -> Option<Hook> {
    let mut h = h.clone();
    if !h.command.contains(DIR_PLACEHOLDER)
        && !matches!(h.extra.get("args"), Some(Value::Array(a)) if a.iter().any(|v| v.as_str().is_some_and(|s| s.contains(DIR_PLACEHOLDER))))
    {
        return Some(h);
    }
    let project = "${CLAUDE_PROJECT_DIR}/";
    let body = dir.strip_prefix(project).unwrap_or(dir);
    if body.contains(['"', '\'', '`', '$', '\n', '\r', '\\']) {
        return None;
    }
    let plain = body.chars().all(plain_path_char);
    let exec_form = matches!(h.extra.get("args"), Some(Value::Array(_)));
    if exec_form {
        for s in strings_mut(&mut h) {
            *s = s.replace(DIR_PLACEHOLDER, dir);
        }
    } else {
        h.command = insert_dir_in_shell(&h.command, dir, plain);
    }
    Some(h)
}

/// `h` with every occurrence of the path text `from` replaced by the profile placeholder form.
pub fn with_script_ref(h: &Hook, from: &str, rel: &str) -> Hook {
    let to = format!("{DIR_PLACEHOLDER}/{rel}");
    let mut h = h.clone();
    for s in strings_mut(&mut h) {
        *s = s.replace(from, &to);
    }
    h
}

/// The words of the first simple command of `cmd`, quotes removed. Scanning
/// stops at an unquoted `;`, `&`, `|`, `<`, `>` or line break: anything after
/// that is another command.
pub fn shell_words(cmd: &str) -> Vec<String> {
    let mut words = Vec::new();
    let mut cur = String::new();
    let mut started = false;
    let mut quote: Option<char> = None;
    for c in cmd.chars() {
        match quote {
            Some(q) if c == q => quote = None,
            Some(_) => cur.push(c),
            None => match c {
                '\'' | '"' => {
                    quote = Some(c);
                    started = true;
                }
                ';' | '&' | '|' | '<' | '>' | '\n' | '\r' => break,
                c if c.is_whitespace() => {
                    if started {
                        words.push(std::mem::take(&mut cur));
                        started = false;
                    }
                }
                c => {
                    cur.push(c);
                    started = true;
                }
            },
        }
    }
    if started {
        words.push(cur);
    }
    words
}

const INTERPRETERS: &[&str] = &[
    "bash",
    "sh",
    "zsh",
    "dash",
    "fish",
    "node",
    "nodejs",
    "deno",
    "bun",
    "python",
    "python3",
    "ruby",
    "perl",
    "php",
    "pwsh",
    "powershell",
];

/// The word of a hook's command line that names its script: the command
/// itself, or the file handed to an interpreter (`bash ~/x.sh`, `node x.js`).
/// `words` is the command's words, followed by its `args` in exec form.
pub fn script_word(words: &[String]) -> Option<&str> {
    let mut i = 0;
    // Leading `VAR=value` assignments.
    while words.get(i).is_some_and(|w| {
        w.split_once('=').is_some_and(|(k, _)| {
            !k.is_empty() && k.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
        })
    }) {
        i += 1;
    }
    let first = words.get(i)?;
    let base = first
        .rsplit(['/', '\\'])
        .next()
        .unwrap_or(first)
        .to_ascii_lowercase();
    let base = base.strip_suffix(".exe").unwrap_or(&base);
    if !INTERPRETERS.contains(&base) {
        return Some(first);
    }
    for w in &words[i + 1..] {
        if matches!(
            w.as_str(),
            "-c" | "-e" | "-E" | "--eval" | "-Command" | "-command"
        ) {
            return None;
        }
        if w.starts_with('-') {
            continue;
        }
        return Some(w);
    }
    None
}

/// A hook, for the plan and `inspect`: `pre-tool [Bash]: <command, in full>`.
pub fn describe(h: &Hook) -> String {
    let mut s = h.event.clone();
    if let Some(m) = &h.matcher {
        s.push_str(&format!(" [{m}]"));
    }
    s.push_str(": ");
    s.push_str(&h.command);
    if let Some(args) = h.extra.get("args") {
        s.push_str(&format!(" (args: {args})"));
    }
    s
}

/// Where the scripts of `profile` are named by hooks: file -> number of hooks.
pub fn script_uses(hooks: &[Hook]) -> BTreeMap<String, usize> {
    let mut out = BTreeMap::new();
    for h in hooks {
        for f in referenced_files(h) {
            *out.entry(f).or_default() += 1;
        }
    }
    out
}

/// Problems that make the hooks and scripts of a profile unusable.
pub fn validate(hooks: &[Hook], files: Option<&[crate::model::SkillFile]>) -> Vec<String> {
    let mut issues = Vec::new();
    for (i, h) in hooks.iter().enumerate() {
        let at = format!("hooks[{i}]");
        let name_ok = !h.event.is_empty()
            && h.event
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | ':'));
        if !name_ok {
            issues.push(format!(
                "{at}: 'event' must be a name like pre-tool, session-start or claude-code:PostToolBatch"
            ));
        }
        if h.command.trim().is_empty() {
            issues.push(format!("{at}: 'command' is empty"));
        }
        if h.timeout == Some(0) {
            issues.push(format!("{at}: 'timeout' must be at least 1 second"));
        }
        for f in referenced_files(h) {
            if !safe_script_path(&f) {
                issues.push(format!(
                    "{at}: script path '{f}' is not a plain relative path"
                ));
            } else if files.is_some_and(|files| !files.iter().any(|x| x.path == f)) {
                issues.push(format!(
                    "{at}: runs {DIR_PLACEHOLDER}/{f}, which is not in the profile's hooks/ directory"
                ));
            }
        }
    }
    issues
}

// ---------------------------------------------------------------------------
// Records of what an apply wrote

const FILE: &str = "applied-hooks.json";
/// Records kept per hook; older ones only matter after several undos.
const KEEP_PER_HOOK: usize = 10;

/// A hook entry an apply wrote into a tool's settings, with the scripts it runs.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct HookRecord {
    pub target: String,
    /// The real settings file.
    pub path: PathBuf,
    /// The tool's own event name.
    pub event: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub matcher: Option<String>,
    /// The command as written (what the doctor looks for).
    pub command: String,
    /// [`entry_hash`] of the entry as written.
    pub hash: String,
    /// The scripts written with it: real path and SHA-256 of the content.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub scripts: Vec<(PathBuf, String)>,
    /// `user` or `project`.
    pub scope: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub project: Option<PathBuf>,
    pub profile: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
    pub tx: String,
    pub at_ms: u128,
}

type Key = (String, PathBuf, String, Option<String>, String);

impl HookRecord {
    fn key(&self) -> Key {
        (
            self.target.clone(),
            self.path.clone(),
            self.event.clone(),
            self.matcher.clone(),
            self.command.clone(),
        )
    }
}

fn load(state_dir: &Path) -> Result<(Vec<HookRecord>, Option<String>)> {
    let path = state_dir.join(FILE);
    match fs::read_to_string(&path) {
        Ok(t) => Ok(match serde_json::from_str(&t) {
            Ok(v) => (v, None),
            Err(e) => (
                Vec::new(),
                Some(format!(
                    "could not read {} ({e}), so hooks kitbag applied are not checked; the next apply that writes a hook keeps it as {FILE}.bak and starts afresh",
                    path.display()
                )),
            ),
        }),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok((Vec::new(), None)),
        Err(e) => Err(e).with_context(|| format!("reading {}", path.display())),
    }
}

/// Remember the hooks `plan` wrote, once written as `tx`.
pub fn record(
    state_dir: &Path,
    plan: &Plan,
    env: &Env,
    profile: &str,
    tx: Option<&str>,
) -> Result<()> {
    let Some(tx) = tx else {
        return Ok(());
    };
    if plan.hook_writes.is_empty() {
        return Ok(());
    }
    let (mut all, unreadable) = load(state_dir)?;
    if unreadable.is_some() {
        let path = state_dir.join(FILE);
        let data = fs::read(&path).with_context(|| format!("reading {}", path.display()))?;
        write_atomic(&state_dir.join(format!("{FILE}.bak")), &data, Some(0o600))?;
    }
    let now = crate::sync::now_ms();
    for w in &plan.hook_writes {
        all.push(HookRecord {
            target: w.target.clone(),
            path: w.path.clone(),
            event: w.event.clone(),
            matcher: w.matcher.clone(),
            command: w.command.clone(),
            hash: w.hash.clone(),
            scripts: w.scripts.clone(),
            scope: env.scope.to_string(),
            project: (env.scope == Scope::Project).then(|| env.project.clone()),
            profile: profile.to_string(),
            source: plan.source.clone(),
            tx: tx.to_string(),
            at_ms: now,
        });
    }
    let mut seen: BTreeMap<(Key, String, Option<PathBuf>), usize> = BTreeMap::new();
    let mut kept: Vec<HookRecord> = all
        .into_iter()
        .rev()
        .filter(|r| {
            let n = seen
                .entry((r.key(), r.scope.clone(), r.project.clone()))
                .or_default();
            *n += 1;
            *n <= KEEP_PER_HOOK
        })
        .collect();
    kept.reverse();
    fs::create_dir_all(state_dir).with_context(|| format!("creating {}", state_dir.display()))?;
    let json = serde_json::to_string_pretty(&kept)? + "\n";
    write_atomic(&state_dir.join(FILE), json.as_bytes(), Some(0o600))
}

/// The record of the latest apply that still stands (not undone) for every
/// hook at `env`'s scope (and project), and a warning if the records can't be read.
pub fn standing(state_dir: &Path, env: &Env) -> Result<(Vec<HookRecord>, Option<String>)> {
    let (records, warning) = load(state_dir)?;
    if records.is_empty() {
        return Ok((records, warning));
    }
    let undone: Vec<String> = Store::new(state_dir)
        .list()?
        .into_iter()
        .filter(|t| t.status != Status::Applied)
        .map(|t| t.id)
        .collect();
    let mut latest: BTreeMap<Key, HookRecord> = BTreeMap::new();
    for r in records {
        let matches = r.scope == env.scope.to_string()
            && (env.scope == Scope::User || r.project.as_deref() == Some(env.project.as_path()));
        if !matches || undone.contains(&r.tx) {
            continue;
        }
        latest.insert(r.key(), r);
    }
    Ok((latest.into_values().collect(), warning))
}

/// Hooks kitbag applied that no longer match what it wrote: changed, removed,
/// or running a script that changed. Hooks kitbag didn't write are not looked at.
pub fn drift(records: &[HookRecord]) -> Vec<String> {
    let mut out = Vec::new();
    for r in records {
        let label = format!(
            "{} hook {}{}: {}",
            r.target,
            r.event,
            r.matcher
                .as_ref()
                .map(|m| format!(" [{m}]"))
                .unwrap_or_default(),
            r.command
        );
        let doc = fs::read_to_string(&r.path)
            .ok()
            .and_then(|t| crate::edit::json::parse_value(Some(&t), &r.path).ok());
        let Some(doc) = doc else {
            out.push(format!(
                "{label}: this hook no longer matches what kitbag applied ({} can't be read)",
                r.path.display()
            ));
            continue;
        };
        let handlers: Vec<&Value> = doc
            .get("hooks")
            .and_then(|h| h.get(&r.event))
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter(|g| g.get("matcher").and_then(Value::as_str) == r.matcher.as_deref())
            .filter_map(|g| g.get("hooks").and_then(Value::as_array))
            .flatten()
            .filter(|h| h.get("command").and_then(Value::as_str) == Some(r.command.as_str()))
            .collect();
        if handlers.is_empty() {
            out.push(format!(
                "{label}: this hook no longer matches what kitbag applied (removed or its command changed in {})",
                r.path.display()
            ));
        } else if !handlers.iter().any(|h| entry_hash(h) == r.hash) {
            out.push(format!(
                "{label}: this hook no longer matches what kitbag applied (edited in {})",
                r.path.display()
            ));
        }
        for (path, hash) in &r.scripts {
            match fs::read(path) {
                Ok(b) if sha256(Some(&b)).as_deref() == Some(hash.as_str()) => {}
                Ok(_) => out.push(format!(
                    "{label}: its script {} no longer matches what kitbag applied (edited)",
                    path.display()
                )),
                Err(_) => out.push(format!(
                    "{label}: its script {} no longer matches what kitbag applied (missing)",
                    path.display()
                )),
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::Map;

    fn words(s: &str) -> Vec<String> {
        shell_words(s)
    }

    #[test]
    fn events_map_both_ways() {
        assert_eq!(claude_event("pre-tool"), Some("PreToolUse"));
        assert_eq!(
            claude_event("claude-code:PostToolBatch"),
            Some("PostToolBatch")
        );
        assert_eq!(claude_event("claude-code:Nope"), None);
        assert_eq!(claude_event("on-save"), None);
        assert_eq!(neutral_event("PreToolUse").as_deref(), Some("pre-tool"));
        assert_eq!(
            neutral_event("FileChanged").as_deref(),
            Some("claude-code:FileChanged")
        );
        assert_eq!(neutral_event("MadeUp"), None);
        for (n, c) in EVENTS {
            assert!(CLAUDE_EVENTS.contains(c), "{c}");
            assert_eq!(claude_event(n), Some(*c));
        }
    }

    #[test]
    fn the_script_is_the_command_or_the_interpreters_file() {
        let pick = |s: &str| script_word(&words(s)).map(str::to_string);
        assert_eq!(
            pick("/h/.claude/hooks/a.sh"),
            Some("/h/.claude/hooks/a.sh".into())
        );
        assert_eq!(pick("bash /h/a.sh --fix"), Some("/h/a.sh".into()));
        assert_eq!(pick("FOO=1 node -r x \"/h/my dir/a.js\""), Some("x".into()));
        assert_eq!(pick("python3 \"/h/a b.py\""), Some("/h/a b.py".into()));
        assert_eq!(pick("bash -c 'echo hi'"), None);
        assert_eq!(pick("jq . /h/.claude/settings.json"), Some("jq".into()));
        // Only the first simple command counts.
        assert_eq!(pick("/h/a.sh && /h/b.sh"), Some("/h/a.sh".into()));
    }

    #[test]
    fn script_references_are_found_and_replaced() {
        let h = Hook {
            event: "pre-tool".into(),
            matcher: None,
            command: format!("bash {DIR_PLACEHOLDER}/lint/check.sh --x"),
            timeout: None,
            extra: serde_json::from_str(&format!(r#"{{"args":["{DIR_PLACEHOLDER}/b.js","-v"]}}"#))
                .unwrap(),
        };
        assert_eq!(referenced_files(&h), ["lint/check.sh", "b.js"]);
        // Exec form: the directory goes in as it is.
        let r = with_script_dir(&h, "/h/my dir/kitbag").unwrap();
        assert_eq!(r.command, "bash /h/my dir/kitbag/lint/check.sh --x");
        assert_eq!(r.extra["args"][0], "/h/my dir/kitbag/b.js");
        // Shell form: quoted when the directory needs it, not when it is quoted already.
        let shell = |cmd: &str, dir: &str| {
            let h = Hook {
                event: "stop".into(),
                matcher: None,
                command: cmd.into(),
                timeout: None,
                extra: Map::new(),
            };
            with_script_dir(&h, dir).map(|h| h.command)
        };
        let c = format!("bash {DIR_PLACEHOLDER}/a.sh");
        assert_eq!(shell(&c, "/h/.claude/k").unwrap(), "bash /h/.claude/k/a.sh");
        assert_eq!(
            shell(&c, "C:/Users/Jo Doe/k").unwrap(),
            "bash \"C:/Users/Jo Doe/k\"/a.sh"
        );
        assert_eq!(
            shell(&format!("bash \"{DIR_PLACEHOLDER}/a.sh\""), "/h/a b").unwrap(),
            "bash \"/h/a b/a.sh\""
        );
        assert_eq!(
            shell(&c, "${CLAUDE_PROJECT_DIR}/.claude/hooks/kitbag").unwrap(),
            "bash ${CLAUDE_PROJECT_DIR}/.claude/hooks/kitbag/a.sh"
        );
        assert_eq!(shell(&c, "/h/it's"), None);
        assert_eq!(shell("echo hi", "/h/it's").unwrap(), "echo hi");
    }

    #[test]
    fn hooks_are_checked_when_a_profile_loads() {
        let hook = |event: &str, command: &str| Hook {
            event: event.into(),
            matcher: None,
            command: command.into(),
            timeout: None,
            extra: Map::new(),
        };
        let file = |p: &str| crate::model::SkillFile {
            path: p.into(),
            content: Vec::new(),
            executable: false,
        };
        let ok = [hook("pre-tool", "{{hooks_dir}}/a.sh")];
        assert!(validate(&ok, Some(&[file("a.sh")])).is_empty());
        assert_eq!(validate(&ok, Some(&[])).len(), 1);
        let bad = [
            hook("", "x"),
            hook("pre tool", "x"),
            hook("stop", " "),
            hook("stop", "{{hooks_dir}}/../x.sh"),
        ];
        assert_eq!(validate(&bad, None).len(), 4);
    }

    #[test]
    fn script_paths_are_plain() {
        assert!(safe_script_path("a/b-c_d.sh"));
        assert!(!safe_script_path("../a.sh"));
        assert!(!safe_script_path("/a.sh"));
        assert!(!safe_script_path("a b.sh"));
        assert!(!safe_script_path("a//b"));
        assert!(!safe_script_path("a\\b"));
    }
}
