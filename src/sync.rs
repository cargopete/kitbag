//! Drift detection for the instructions and rules kitbag manages.
//!
//! Every apply records a hash of each managed instruction block and rule file
//! it leaves in place (`applied.json` in the state directory, next to the
//! transaction backups). `kitbag status` compares three versions of each item:
//! what is on disk now, what the last apply left there, and what the profile
//! would write now. `kitbag sync` uses the result to apply profile changes
//! without clobbering edits made in the tool.

use std::collections::BTreeMap;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use crate::adapters::common::{RULES_BLOCK_SUFFIX, add_instructions, add_rule};
use crate::adapters::{Captured, Env, Note, Scope};
use crate::edit::block;
use crate::engine::{Item, Plan};
use crate::model::{Profile, Rule};
use crate::plan_fs::{FileChange, decode_text, join_rel, read_opt};
use crate::profile_io::{rule_from_markdown, rule_to_markdown, scan_for_secrets};
use crate::txn::{Status, Store, sha256, write_atomic};

const FILE: &str = "applied.json";
/// Records kept per item; older ones only matter after several undos.
const KEEP_PER_ITEM: usize = 10;

/// The text of one item in a file: the inner content of managed block
/// `block`, or the whole file. `None` when the file or block doesn't exist.
pub fn item_text(bytes: Option<&[u8]>, block: Option<&str>) -> Option<String> {
    let text = decode_text(bytes?.to_vec()).ok()?;
    match block {
        None => Some(text),
        Some(id) => block::split(&text)
            .blocks
            .into_iter()
            .find(|(b, _)| b == id)
            .map(|(_, inner)| inner),
    }
}

pub fn hash(text: &str) -> String {
    sha256(Some(text.replace("\r\n", "\n").trim().as_bytes())).unwrap_or_default()
}

/// Identifies an item: (target, real path, block id).
pub type Key = (String, PathBuf, Option<String>);

/// What an apply left in place for one item.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Record {
    pub target: String,
    pub path: PathBuf,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub block: Option<String>,
    pub item: String,
    /// `user` or `project`.
    pub scope: String,
    /// The project root, at project scope.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub project: Option<PathBuf>,
    /// Name of the profile that was applied.
    pub profile: String,
    /// [`hash`] of the item's text; `None` when the apply removed it.
    pub hash: Option<String>,
    /// The transaction that wrote it (none when the apply had nothing to write).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tx: Option<String>,
    pub at_ms: u128,
}

impl Record {
    /// Was this recorded for the scope (and project) of `env`?
    pub fn matches(&self, env: &Env) -> bool {
        self.scope == env.scope.to_string()
            && (env.scope == Scope::User || self.project.as_deref() == Some(env.project.as_path()))
    }

    pub fn key(&self) -> Key {
        (self.target.clone(), self.path.clone(), self.block.clone())
    }
}

fn file(state_dir: &Path) -> PathBuf {
    state_dir.join(FILE)
}

fn load_all(state_dir: &Path) -> Result<Vec<Record>> {
    let path = file(state_dir);
    match fs::read_to_string(&path) {
        Ok(t) => Ok(serde_json::from_str(&t).unwrap_or_default()),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(Vec::new()),
        Err(e) => Err(e).with_context(|| format!("reading {}", path.display())),
    }
}

/// The record of the latest apply that still stands (not undone) for every
/// item, keyed by (target, path, block).
pub fn last_applied(state_dir: &Path) -> Result<BTreeMap<Key, Record>> {
    let undone: Vec<String> = Store::new(state_dir)
        .list()?
        .into_iter()
        .filter(|t| t.status != Status::Applied)
        .map(|t| t.id)
        .collect();
    let mut out = BTreeMap::new();
    for r in load_all(state_dir)? {
        if r.tx.as_ref().is_some_and(|t| undone.contains(t)) {
            continue;
        }
        // Records are appended in order, so the last one wins.
        out.insert(r.key(), r);
    }
    Ok(out)
}

/// Remember what `plan` left in place, after it was written (or found to
/// have nothing to write).
pub fn record(
    state_dir: &Path,
    plan: &Plan,
    env: &Env,
    profile: &str,
    tx: Option<&str>,
) -> Result<()> {
    if plan.items.is_empty() {
        return Ok(());
    }
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0);
    let mut all = load_all(state_dir)?;
    for i in &plan.items {
        all.push(Record {
            target: i.target.clone(),
            path: i.path.clone(),
            block: i.block.clone(),
            item: i.item.clone(),
            scope: env.scope.to_string(),
            project: (env.scope == Scope::Project).then(|| env.project.clone()),
            profile: profile.to_string(),
            hash: i.planned.as_deref().map(hash),
            tx: tx.map(str::to_string),
            at_ms: now,
        });
    }
    // Keep the newest few per item.
    let mut seen: BTreeMap<Key, usize> = BTreeMap::new();
    let mut kept: Vec<Record> = all
        .into_iter()
        .rev()
        .filter(|r| {
            let n = seen.entry(r.key()).or_default();
            *n += 1;
            *n <= KEEP_PER_ITEM
        })
        .collect();
    kept.reverse();
    fs::create_dir_all(state_dir).with_context(|| format!("creating {}", state_dir.display()))?;
    let json = serde_json::to_string_pretty(&kept)? + "\n";
    write_atomic(&file(state_dir), json.as_bytes(), Some(0o600))
}

/// How one item compares across the tool, the last apply and the profile.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum State {
    InSync,
    /// Edited in the tool since the last apply; the profile hasn't changed.
    ToolEdited,
    /// The profile changed; the tool still has what was last applied.
    ProfileChanged,
    /// Both changed since the last apply (or there is no record of one and
    /// they differ): sync won't pick a side on its own.
    BothChanged,
    /// In the profile, not in the tool.
    NotApplied,
    /// The tool keeps this somewhere kitbag can't read or write.
    Unsupported,
    /// An MCP server, command or skill in the profile, not in the tool yet.
    Added,
    /// An MCP server, command or skill that differs from the profile.
    Changed,
    /// An MCP server, command or skill in the tool but not in the profile
    /// (apply is additive, so sync leaves it; not counted as drift).
    Removed,
}

impl State {
    pub fn as_str(self) -> &'static str {
        match self {
            State::InSync => "in sync",
            State::ToolEdited => "tool edited",
            State::ProfileChanged => "profile changed",
            State::BothChanged => "both changed",
            State::NotApplied => "not applied",
            State::Unsupported => "unsupported",
            State::Added => "added",
            State::Changed => "changed",
            State::Removed => "removed",
        }
    }

    /// Does this differ from the profile (or, without one, from the last apply)?
    pub fn is_drift(self) -> bool {
        !matches!(self, State::InSync | State::Unsupported | State::Removed)
    }

    /// Would syncing this overwrite an edit made in the tool?
    pub fn is_conflict(self) -> bool {
        matches!(self, State::ToolEdited | State::BothChanged)
    }
}

/// Compare an item's text in the tool (`current`), its hash at the last apply
/// (`applied`, `None` when there is no record) and the text the profile would
/// write (`planned`).
pub fn classify(
    current: Option<&str>,
    applied: Option<Option<&str>>,
    planned: Option<&str>,
) -> State {
    let c = current.map(hash);
    let p = planned.map(hash);
    if c == p {
        return State::InSync;
    }
    match applied {
        None if c.is_none() => State::NotApplied,
        None => State::BothChanged,
        Some(a) => {
            let a = a.map(str::to_string);
            if c == a {
                if c.is_none() {
                    State::NotApplied
                } else {
                    State::ProfileChanged
                }
            } else if p == a {
                State::ToolEdited
            } else {
                State::BothChanged
            }
        }
    }
}

/// Without a profile: has the tool's copy changed since the last apply?
pub fn classify_since_apply(current: Option<&str>, applied: Option<&str>) -> State {
    if current.map(hash).as_deref() == applied {
        State::InSync
    } else {
        State::ToolEdited
    }
}

/// A profile updated with edits pulled back from a tool.
#[derive(Debug)]
pub struct PullBack {
    pub profile: Profile,
    /// Writes to the profile directory (part of the same undoable apply).
    pub changes: Vec<FileChange>,
    pub notes: Vec<Note>,
}

/// Pull the edited `items` of one target back into `profile` (in `dir`):
/// its managed instructions block becomes `instructions.md`, folded rules
/// and rule files become `rules/*.md`. Everything pulled back is redacted the
/// way `capture` does it, and the result must pass the same leak scan.
pub fn pull_back(
    profile: &Profile,
    dir: &Path,
    items: &[&Item],
    block_id: &str,
) -> Result<PullBack> {
    let mut next = profile.clone();
    let mut cap = Captured::default();
    let rules_block = format!("{block_id}{RULES_BLOCK_SUFFIX}");
    for i in items {
        let Some(text) = i.current.as_deref() else {
            cap.warn(
                &i.target,
                format!(
                    "{} was removed from {}; that is not pulled back (remove it from the profile by hand)",
                    i.item,
                    i.path.display()
                ),
            );
            continue;
        };
        match i.block.as_deref() {
            Some(b) if b == block_id => {
                cap.profile.instructions = None;
                add_instructions(&mut cap, &i.target, text, &i.path);
                next.instructions = cap.profile.instructions.take();
            }
            Some(b) if b == rules_block => {
                let (rest, rules) = block::unfold_rules(text);
                if !rest.trim().is_empty() {
                    cap.warn(
                        &i.target,
                        format!(
                            "text outside the rule sections of the rules block in {} was not pulled back",
                            i.path.display()
                        ),
                    );
                }
                cap.profile.rules.clear();
                for r in rules {
                    add_rule(&mut cap, r, &i.target);
                }
                next.rules = std::mem::take(&mut cap.profile.rules);
            }
            Some(_) => {}
            None => {
                let Some(name) = i.item.strip_prefix("rule ") else {
                    continue;
                };
                let parsed = if i.path.extension().is_some_and(|e| e == "mdc") {
                    crate::adapters::cursor::rule_from_mdc(name, text)
                } else {
                    rule_from_markdown(name, text)
                };
                let old = profile.rules.iter().find(|r| r.name == name);
                let rule = Rule {
                    always_apply: parsed
                        .always_apply
                        .or_else(|| old.and_then(|o| o.always_apply)),
                    ..parsed
                };
                cap.profile.rules.clear();
                add_rule(&mut cap, rule, &i.target);
                if let Some(r) = cap.profile.rules.pop() {
                    match next.rules.iter_mut().find(|x| x.name == r.name) {
                        Some(x) => *x = r,
                        None => next.rules.push(r),
                    }
                }
            }
        }
    }
    next.sort();
    let leaks = scan_for_secrets(&next);
    if !leaks.is_empty() {
        anyhow::bail!(
            "refusing to pull these edits into the profile: they still look like they contain secrets{}",
            leaks
                .iter()
                .map(|l| format!("\n  - {l}"))
                .collect::<String>()
        );
    }
    let mut changes = Vec::new();
    let mut change = |path: PathBuf, after: Option<Vec<u8>>| -> Result<()> {
        let before = read_opt(&path)?;
        if before != after {
            changes.push(FileChange {
                path,
                before,
                after,
            });
        }
        Ok(())
    };
    if next.instructions != profile.instructions {
        change(
            dir.join("instructions.md"),
            next.instructions
                .as_ref()
                .map(|i| format!("{}\n", i.trim()).into_bytes()),
        )?;
    }
    // A rule may be stored as `.mdc`; keep its file name.
    let rule_path = |name: &str| {
        let mdc = join_rel(dir, &format!("rules/{name}.mdc"));
        if mdc.is_file() {
            mdc
        } else {
            join_rel(dir, &format!("rules/{name}.md"))
        }
    };
    for r in &next.rules {
        if profile.rules.iter().find(|x| x.name == r.name) != Some(r) {
            change(rule_path(&r.name), Some(rule_to_markdown(r).into_bytes()))?;
        }
    }
    for r in &profile.rules {
        if !next.rules.iter().any(|x| x.name == r.name) {
            change(rule_path(&r.name), None)?;
        }
    }
    Ok(PullBack {
        profile: next,
        changes,
        notes: cap.notes,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classifies_every_combination() {
        let (a, b, c) = ("one", "two", "three");
        let h = |s: &str| Some(hash(s));
        let ha = h(a);
        let applied = Some(ha.as_deref());
        assert_eq!(classify(Some(a), applied, Some(a)), State::InSync);
        assert_eq!(classify(Some(b), applied, Some(a)), State::ToolEdited);
        assert_eq!(classify(Some(a), applied, Some(b)), State::ProfileChanged);
        assert_eq!(classify(Some(b), applied, Some(c)), State::BothChanged);
        assert_eq!(classify(Some(b), applied, Some(b)), State::InSync);
        assert_eq!(classify(None, applied, Some(a)), State::ToolEdited);
        assert_eq!(classify(None, None, Some(a)), State::NotApplied);
        assert_eq!(classify(Some(b), None, Some(a)), State::BothChanged);
        assert_eq!(classify(Some(a), None, Some(a)), State::InSync);
        assert_eq!(classify(None, Some(None), Some(a)), State::NotApplied);
        // Whitespace at the ends and line endings don't count.
        assert_eq!(classify(Some("one\r\n"), applied, Some(a)), State::InSync);
        assert_eq!(classify_since_apply(Some(a), ha.as_deref()), State::InSync);
        assert_eq!(classify_since_apply(None, ha.as_deref()), State::ToolEdited);
    }

    #[test]
    fn finds_block_text() {
        let text = block::upsert(Some("mine\n"), "p", Some("Be brief."));
        assert_eq!(
            item_text(Some(text.as_bytes()), Some("p")).as_deref(),
            Some("Be brief.")
        );
        assert_eq!(item_text(Some(text.as_bytes()), Some("q")), None);
        assert_eq!(item_text(None, None), None);
    }
}
