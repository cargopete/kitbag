//! Claude Code hooks, in `settings.json`.
//!
//! ```json
//! { "hooks": { "PreToolUse": [ { "matcher": "Bash",
//!     "hooks": [ { "type": "command", "command": "...", "timeout": 30 } ] } ] } }
//! ```
//!
//! Only `command` hooks are carried (`http`, `mcp_tool`, `prompt` and `agent`
//! hooks are reported and left out). Every other key of a command hook (`args`,
//! `shell`, `async`, `if`, `statusMessage` ...) is kept as it is.
//!
//! * <https://code.claude.com/docs/en/hooks>
//!
//! Scripts a hook runs are copied into the profile's `hooks/` directory and
//! written back to `<claude dir>/hooks/kitbag/` (`.claude/hooks/kitbag/` in a
//! project), which the hook command reaches through `{{hooks_dir}}`.

use std::collections::BTreeMap;
use std::path::{Component, Path, PathBuf};

use anyhow::Result;
use serde_json::{Map, Value, json};

use super::common::{skill_file_executable, write_owned};
use super::{ApplyOptions, Captured, Env, HookWrite, Scope, Work};
use crate::edit::json::JsonDoc;
use crate::hooks;
use crate::model::{Hook, SkillFile};
use crate::plan_fs::{join_rel, read_opt};
use crate::profile_io::{MAX_FILE_BYTES, max_file_size};
use crate::secrets::{
    has_redaction_marker, redact_json_strings, redact_support_file, redact_text, sensitive_file,
};

use super::claude_code::ID;

/// Where a hook script lands, below the Claude config directory.
const SCRIPTS_SUBDIR: [&str; 2] = ["hooks", "kitbag"];

// ---------------------------------------------------------------------------
// Capture

/// The path a hook command word names, if it names one kitbag can follow:
/// `${CLAUDE_PROJECT_DIR}/...`, `~/...`, `$HOME/...`, an absolute path or (in
/// a project) `./...`.
fn resolve(word: &str, env: &Env) -> Option<PathBuf> {
    let project = (env.scope == Scope::Project).then(|| env.project.to_string_lossy().into_owned());
    let home = env.home.to_string_lossy().into_owned();
    let mut w = word.to_string();
    for (pre, with) in [
        ("${CLAUDE_PROJECT_DIR}", project.clone()),
        ("$CLAUDE_PROJECT_DIR", project.clone()),
        ("${HOME}", Some(home.clone())),
        ("$HOME", Some(home.clone())),
    ] {
        if let Some(rest) = w.strip_prefix(pre) {
            w = format!("{}{rest}", with?);
            break;
        }
    }
    if let Some(rest) = w.strip_prefix("~/") {
        w = format!("{home}/{rest}");
    } else if let Some(rest) = w.strip_prefix("./") {
        w = format!("{}/{rest}", project?);
    }
    if w.contains(['$', '`']) {
        return None;
    }
    let path = PathBuf::from(w);
    (path.is_absolute() && !path.components().any(|c| c == Component::ParentDir)).then_some(path)
}

/// The profile's path for a script at `path` below `base`: relative to it,
/// without a leading `.claude/` (project) and `hooks/`.
fn profile_path(path: &Path, base: &Path, env: &Env) -> Option<String> {
    let rel = path.strip_prefix(base).ok()?;
    let parts: Vec<String> = rel
        .components()
        .map(|c| c.as_os_str().to_string_lossy().into_owned())
        .collect();
    let mut parts: &[String] = &parts;
    if env.scope == Scope::Project && parts.first().is_some_and(|p| p == ".claude") {
        parts = &parts[1..];
    }
    if parts.len() > 1 && parts[0] == "hooks" {
        parts = &parts[1..];
    }
    let rel = parts.join("/");
    hooks::safe_script_path(&rel).then_some(rel)
}

/// Copy the script of `hook` into the profile and point the hook at the copy.
/// Returns the hook as it goes in the profile.
fn take_script(
    cap: &mut Captured,
    env: &Env,
    base: &Path,
    hook: Hook,
    seen: &mut BTreeMap<String, PathBuf>,
) -> Hook {
    let mut words = vec![hook.command.clone()];
    let exec_form = matches!(hook.extra.get("args"), Some(Value::Array(_)));
    if let Some(Value::Array(a)) = hook.extra.get("args") {
        words.extend(a.iter().filter_map(Value::as_str).map(str::to_string));
    }
    if !exec_form {
        words = hooks::shell_words(&hook.command);
    }
    let Some(word) = hooks::script_word(&words).map(str::to_string) else {
        return hook;
    };
    let Some(path) = resolve(&word, env) else {
        return hook;
    };
    let shown = path.display().to_string();
    if !path.is_file() {
        if word.contains('/') {
            cap.warn(
                ID,
                format!(
                    "hook {}: the script {shown} was not found, so the command is kept as it is",
                    hook.event
                ),
            );
        }
        return hook;
    }
    if !path.starts_with(base) {
        cap.warn(
            ID,
            format!(
                "hook {}: {shown} is outside {}, so it was not copied into the profile; the command keeps a path that only exists on this machine",
                hook.event,
                base.display()
            ),
        );
        return hook;
    }
    if env.scope == Scope::Project && cap.escapes(&path, ID) {
        return hook;
    }
    let skip = |cap: &mut Captured, why: String| {
        cap.warn(
            ID,
            format!(
                "hook {}: script {shown} was not copied ({why}); the command is kept as it is",
                hook.event
            ),
        );
    };
    let Some(rel) = profile_path(&path, base, env) else {
        skip(
            cap,
            "its path has characters kitbag can't keep portable, or it is not below the Claude config directory".into(),
        );
        return hook;
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
            return hook;
        }
        Err(e) => {
            skip(cap, e.to_string());
            return hook;
        }
        _ => {}
    }
    let bytes = match read_opt(&path) {
        Ok(Some(b)) => b,
        Ok(None) => return hook,
        Err(e) => {
            skip(cap, format!("{e:#}"));
            return hook;
        }
    };
    if let Some(why) = sensitive_file(&rel, &bytes) {
        skip(cap, why);
        return hook;
    }
    let content = match String::from_utf8(bytes) {
        Ok(text) => {
            let (red, kinds) = redact_support_file(&rel, &text);
            if !kinds.is_empty() {
                cap.warn(
                    ID,
                    format!("redacted {} from hook script hooks/{rel}", kinds.join(", ")),
                );
            }
            red.into_bytes()
        }
        Err(e) => e.into_bytes(),
    };
    if let Some(other) = seen.get(&rel)
        && *other != path
    {
        skip(
            cap,
            format!("{} is already captured as hooks/{rel}", other.display()),
        );
        return hook;
    }
    seen.insert(rel.clone(), path.clone());
    if !cap.profile.hook_files.iter().any(|f| f.path == rel) {
        cap.sources.push(path.clone());
        cap.profile.hook_files.push(SkillFile {
            path: rel.clone(),
            content,
            executable: skill_file_executable(&path),
        });
    }
    hooks::with_script_ref(&hook, &word, &rel)
}

/// Read the `hooks` of a settings file into the profile (`capture --include
/// hooks`): command hooks only, their scripts copied, everything redacted.
pub(super) fn capture(cap: &mut Captured, env: &Env, doc: &Map<String, Value>, base: &Path) {
    let Some(value) = doc.get("hooks") else {
        return;
    };
    let Value::Object(events) = value else {
        cap.warn(
            ID,
            "settings.json: 'hooks' is not an object; no hooks captured",
        );
        return;
    };
    let mut seen = BTreeMap::new();
    let mut count = 0;
    for (event, groups) in events {
        let Some(neutral) = hooks::neutral_event(event) else {
            cap.warn(
                ID,
                format!("hooks: skipped event '{event}', which kitbag doesn't know"),
            );
            continue;
        };
        let Value::Array(groups) = groups else {
            cap.warn(ID, format!("hooks: '{event}' is not a list; skipped"));
            continue;
        };
        for group in groups {
            let matcher = group
                .get("matcher")
                .and_then(Value::as_str)
                .map(str::to_string);
            let Some(list) = group.get("hooks").and_then(Value::as_array) else {
                continue;
            };
            for h in list {
                let Value::Object(h) = h else { continue };
                let ty = h.get("type").and_then(Value::as_str).unwrap_or("command");
                if ty != "command" {
                    cap.warn(
                        ID,
                        format!("hooks: a {ty} hook on {event} was not captured (kitbag carries command hooks only)"),
                    );
                    continue;
                }
                let Some(command) = h.get("command").and_then(Value::as_str) else {
                    cap.warn(
                        ID,
                        format!("hooks: a hook on {event} has no command; skipped"),
                    );
                    continue;
                };
                let timeout = h.get("timeout").and_then(Value::as_u64).filter(|t| *t > 0);
                let extra: Map<String, Value> = h
                    .iter()
                    .filter(|(k, v)| match k.as_str() {
                        "type" | "command" => false,
                        "timeout" => timeout.is_none() || !v.is_number(),
                        _ => true,
                    })
                    .map(|(k, v)| (k.clone(), v.clone()))
                    .collect();
                let hook = Hook {
                    event: neutral.clone(),
                    matcher: matcher.clone(),
                    command: command.to_string(),
                    timeout,
                    extra,
                };
                let mut hook = take_script(cap, env, base, hook, &mut seen);
                let home = env.home.to_string_lossy();
                if !home.is_empty() && hook.command.contains(home.as_ref()) {
                    cap.warn(
                        ID,
                        format!(
                            "hook {}: the command mentions {home}, which is specific to this machine; edit it in the profile",
                            hook.event
                        ),
                    );
                }
                let (red, mut kinds) = redact_text(&hook.command);
                hook.command = red;
                for v in hook.extra.values_mut() {
                    kinds.extend(redact_json_strings(v));
                }
                kinds.dedup();
                if !kinds.is_empty() {
                    cap.warn(
                        ID,
                        format!("redacted {} from a hook on {event}", kinds.join(", ")),
                    );
                }
                if !cap.profile.hooks.contains(&hook) {
                    cap.profile.hooks.push(hook);
                    count += 1;
                }
            }
        }
    }
    if count > 0 {
        let scripts = cap.profile.hook_files.len();
        cap.info(
            ID,
            format!(
                "captured {count} hook(s) and {scripts} script(s) from settings.json; they run commands by themselves, so applying them needs --apply-hooks"
            ),
        );
    }
}

/// How many command and other hooks a settings file has (for the note when
/// they are not captured).
pub(super) fn count(doc: &Map<String, Value>) -> usize {
    doc.get("hooks")
        .and_then(Value::as_object)
        .map(|m| {
            m.values()
                .filter_map(Value::as_array)
                .flatten()
                .filter_map(|g| g.get("hooks").and_then(Value::as_array))
                .map(Vec::len)
                .sum()
        })
        .unwrap_or(0)
}

// ---------------------------------------------------------------------------
// Apply

fn sha(bytes: &[u8]) -> String {
    crate::txn::sha256(Some(bytes)).unwrap_or_default()
}

/// The handler as it goes into `settings.json`.
fn handler(h: &Hook) -> Value {
    let mut m = Map::new();
    m.insert("type".into(), json!("command"));
    m.insert("command".into(), json!(h.command));
    if let Some(t) = h.timeout {
        m.insert("timeout".into(), json!(t));
    }
    for (k, v) in &h.extra {
        m.insert(k.clone(), v.clone());
    }
    Value::Object(m)
}

/// The text standing for the script directory in a command: the project
/// variable Claude Code expands itself, or the directory.
fn script_dir_text(env: &Env, dir: &Path) -> String {
    match env.scope {
        Scope::Project => "${CLAUDE_PROJECT_DIR}/.claude/hooks/kitbag".to_string(),
        Scope::User => dir.to_string_lossy().replace('\\', "/"),
    }
}

/// Plan the hooks of `profile` for Claude Code: list every one in the plan,
/// and, with `--apply-hooks`, write the scripts and the settings entries.
pub(super) fn apply(
    profile: &crate::model::Profile,
    env: &Env,
    opts: &ApplyOptions,
    work: &mut Work,
    settings: &Path,
    claude_dir: &Path,
) -> Result<()> {
    if profile.hooks.is_empty() {
        return Ok(());
    }
    let scripts_dir = SCRIPTS_SUBDIR
        .iter()
        .fold(claude_dir.to_path_buf(), |p, s| p.join(s));
    let dir_text = script_dir_text(env, &scripts_dir);
    let files: BTreeMap<&str, &SkillFile> = profile
        .hook_files
        .iter()
        .map(|f| (f.path.as_str(), f))
        .collect();
    let describe = |h: &Hook| {
        let scripts: Vec<String> = hooks::referenced_files(h)
            .iter()
            .map(|f| match files.get(f.as_str()) {
                Some(file) => format!(
                    "runs the profile's script hooks/{f} ({} bytes, copied to {})",
                    file.content.len(),
                    join_rel(&scripts_dir, f).display()
                ),
                None => format!("runs hooks/{f}, which the profile does not have"),
            })
            .collect();
        // As it would be written, not with the profile's placeholder.
        let shown = hooks::with_script_dir(h, &dir_text).unwrap_or_else(|| h.clone());
        let mut s = hooks::describe(&shown);
        if !scripts.is_empty() {
            s.push_str(&format!(" [{}]", scripts.join("; ")));
        }
        s
    };
    if !opts.apply_hooks {
        work.warn(
            ID,
            format!(
                "{} hook(s) in the profile not applied: hooks run commands by themselves; pass --apply-hooks to apply them (nothing is written for them without it). They would run:",
                profile.hooks.len()
            ),
        );
        for h in &profile.hooks {
            work.info(ID, format!("hook {}", describe(h)));
        }
        return Ok(());
    }

    // A script capture redacted a secret from would run with a marker where
    // the secret was: neither it nor the hooks that run it are applied.
    let redacted = |bytes: &[u8]| std::str::from_utf8(bytes).is_ok_and(has_redaction_marker);
    let broken_scripts: Vec<&str> = files
        .iter()
        .filter(|(_, f)| redacted(&f.content))
        .map(|(rel, _)| *rel)
        .collect();
    for rel in &broken_scripts {
        work.warn(
            ID,
            format!(
                "hook script hooks/{rel} not written: capture removed a secret from it ([REDACTED] marker); edit it in the profile to read the secret from the environment"
            ),
        );
    }
    // The scripts first: the hooks that run them are only added if they can be.
    let mut script_hashes: BTreeMap<String, String> = BTreeMap::new();
    let mut script_changed: Vec<String> = Vec::new();
    for (rel, f) in files
        .iter()
        .filter(|(rel, _)| !broken_scripts.contains(rel))
    {
        let path = join_rel(&scripts_dir, rel);
        if write_owned(
            work,
            &path,
            &f.content,
            opts,
            ID,
            &format!("hook script '{rel}'"),
        )? {
            if f.executable {
                work.fs.mark_executable(&path)?;
            }
            script_changed.push((*rel).to_string());
        }
        script_hashes.insert((*rel).to_string(), sha(&f.content));
    }

    let text = work.fs.read(settings)?;
    let mut doc = JsonDoc::parse(text.as_deref(), settings)?;
    doc.check_unique_keys(&[], &["hooks"])?;
    let mut all = match doc.get(&["hooks"]) {
        None => Map::new(),
        Some(Value::Object(m)) => m,
        Some(_) => {
            work.warn(
                ID,
                format!(
                    "'hooks' in {} is not an object, so no hooks were written",
                    settings.display()
                ),
            );
            return Ok(());
        }
    };
    let mut changed = false;
    let mut writes: Vec<HookWrite> = Vec::new();
    for h in &profile.hooks {
        let what = describe(h);
        let Some(event) = hooks::claude_event(&h.event) else {
            work.warn(
                ID,
                format!(
                    "hook not applied: '{}' has no equivalent event in Claude Code ({what})",
                    h.event
                ),
            );
            continue;
        };
        let marked = has_redaction_marker(&h.command)
            || h.extra.values().any(|v| {
                crate::secrets::json_strings(v)
                    .into_iter()
                    .any(has_redaction_marker)
            });
        if marked
            || hooks::referenced_files(h)
                .iter()
                .any(|f| broken_scripts.contains(&f.as_str()))
        {
            work.warn(
                ID,
                format!(
                    "hook not applied: capture removed a secret from it or its script ([REDACTED] marker); edit it in the profile to read the secret from the environment ({what})"
                ),
            );
            continue;
        }
        let missing: Vec<String> = hooks::referenced_files(h)
            .into_iter()
            .filter(|f| !files.contains_key(f.as_str()))
            .collect();
        if !missing.is_empty() {
            work.warn(
                ID,
                format!(
                    "hook not applied: its script hooks/{} is not in the profile ({what})",
                    missing.join(", hooks/")
                ),
            );
            continue;
        }
        let rendered = match hooks::with_script_dir(h, &dir_text) {
            Some(r) => r,
            None => {
                work.warn(
                    ID,
                    format!(
                        "hook not applied: the script directory {dir_text} has characters that can't be put in a command safely ({what})"
                    ),
                );
                continue;
            }
        };
        let entry = handler(&rendered);
        work.info(ID, format!("hook {what}"));
        let Value::Array(groups) = all
            .entry(event.to_string())
            .or_insert_with(|| Value::Array(Vec::new()))
        else {
            work.warn(
                ID,
                format!(
                    "'hooks.{event}' in {} is not a list; hook not applied",
                    settings.display()
                ),
            );
            continue;
        };
        let at = groups
            .iter()
            .position(|g| g.get("matcher").and_then(Value::as_str) == h.matcher.as_deref());
        let mut wrote = false;
        match at {
            None => {
                let mut g = Map::new();
                if let Some(m) = &h.matcher {
                    g.insert("matcher".into(), json!(m));
                }
                g.insert("hooks".into(), json!([entry.clone()]));
                groups.push(Value::Object(g));
                wrote = true;
            }
            Some(i) => {
                let Some(Value::Array(list)) = groups[i].get_mut("hooks") else {
                    work.warn(
                        ID,
                        format!(
                            "a hooks entry for {event} in {} has no 'hooks' list; hook not applied",
                            settings.display()
                        ),
                    );
                    continue;
                };
                let same = list.iter().position(|x| {
                    x.get("command").and_then(Value::as_str) == Some(rendered.command.as_str())
                        && x.get("type")
                            .and_then(Value::as_str)
                            .is_none_or(|t| t == "command")
                });
                match same {
                    None => {
                        list.push(entry.clone());
                        wrote = true;
                    }
                    Some(j) if list[j] == entry => {}
                    Some(j) if opts.overwrite => {
                        list[j] = entry.clone();
                        wrote = true;
                    }
                    Some(_) => work.warn(
                        ID,
                        format!(
                            "kept the existing hook with the same command in {} because --no-overwrite was given ({what})",
                            settings.display()
                        ),
                    ),
                }
            }
        }
        let rels = hooks::referenced_files(h);
        if wrote || rels.iter().any(|r| script_changed.contains(r)) {
            changed |= wrote;
            writes.push(HookWrite {
                target: ID.to_string(),
                path: settings.to_path_buf(),
                event: event.to_string(),
                matcher: h.matcher.clone(),
                command: rendered.command.clone(),
                hash: crate::sync::entry_hash(&entry),
                scripts: rels
                    .iter()
                    .map(|r| (join_rel(&scripts_dir, r), script_hashes[r].clone()))
                    .collect(),
            });
        }
    }
    if changed {
        doc.set(&["hooks"], &Value::Object(all));
        work.fs.write(settings, doc.render())?;
    }
    work.hook_writes.extend(writes);
    Ok(())
}
