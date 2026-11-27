//! Drift detection for the instructions and rules kitbag manages.
//!
//! Every apply records a hash of each managed instruction block and rule file
//! it leaves in place (`applied.json` in the state directory, next to the
//! transaction backups). `kitbag status` compares three versions of each item:
//! what is on disk now, what the last apply left there, and what the profile
//! would write now. `kitbag sync` uses the result to apply profile changes
//! without clobbering edits made in the tool.
//!
//! MCP server entries kitbag writes are recorded the same way, in
//! `applied-servers.json`: a server disabled for a tool in the profile is only
//! removed from it when kitbag put it there and it is unchanged since.
//!
//! The same goes for the rule, command, skill and agent files an apply writes
//! (`applied-files.json`): one the profile disables for a tool is only removed
//! from it when this profile wrote that file and it is unchanged since.

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
const SERVERS_FILE: &str = "applied-servers.json";
const FILES_FILE: &str = "applied-files.json";
/// Records kept per item; older ones only matter after several undos.
const KEEP_PER_ITEM: usize = 10;

/// The text of one item in a file: the inner content of managed block
/// `block`, or the whole file. `None` when the file or block doesn't exist.
pub fn item_text(bytes: Option<&[u8]>, block: Option<&str>) -> Option<String> {
    let text = decode_text(bytes?.to_vec()).ok()?;
    match block {
        None => Some(text),
        Some(id) if id.starts_with(YAML_ENTRY_PREFIX) => yaml_entry_text(&text, id),
        Some(id) => block::split(&text)
            .blocks
            .into_iter()
            .find(|(b, _)| b == id)
            .map(|(_, inner)| inner),
    }
}

/// Start of the block id of an item that is not a marked block but one text
/// field of an entry in a YAML list (Continue keeps its instructions in an
/// entry of `rules` in `config.yaml`). See [`yaml_entry_block`].
pub const YAML_ENTRY_PREFIX: &str = "yaml-entry:";

/// Block id of the `field` of the entry of list `section` whose `name` is `name`.
pub fn yaml_entry_block(section: &str, name: &str, field: &str) -> String {
    format!("{YAML_ENTRY_PREFIX}{section}:{name}:{field}")
}

/// The text field a [`yaml_entry_block`] id names, in the YAML `text`.
fn yaml_entry_text(text: &str, id: &str) -> Option<String> {
    let mut parts = id.strip_prefix(YAML_ENTRY_PREFIX)?.splitn(3, ':');
    let (section, name, field) = (parts.next()?, parts.next()?, parts.next()?);
    let doc = crate::edit::yaml::parse_map(Some(text), Path::new("")).ok()?;
    doc.get(section)?
        .as_array()?
        .iter()
        .find(|v| v.get("name").and_then(|n| n.as_str()) == Some(name))?
        .get(field)?
        .as_str()
        .map(str::to_string)
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
    /// The transaction that wrote it. An apply with nothing to write records
    /// the transaction of the record it supersedes, or none for an item that
    /// had no record yet.
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

pub(crate) fn now_ms() -> u128 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0)
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
    record_servers(state_dir, plan, env, profile, tx)?;
    record_files(state_dir, plan, env, profile, tx)?;
    if plan.items.is_empty() {
        return Ok(());
    }
    // An apply with nothing to write has no transaction of its own, and undo
    // can only discount records that name one. So it doesn't record an item
    // whose standing record already says the same, and ties any other to the
    // transaction of the record it supersedes: undoing that one undoes this
    // record too. Only an item with no standing record gets one without a
    // transaction, as a baseline no undo can make wrong.
    let standing = match tx {
        Some(_) => BTreeMap::new(),
        None => last_applied(state_dir)?,
    };
    let now = now_ms();
    let mut all = load_all(state_dir)?;
    let before = all.len();
    for i in &plan.items {
        let planned = i.planned.as_deref().map(hash);
        let tx = match tx {
            Some(t) => Some(t.to_string()),
            None => match standing
                .get(&(i.target.clone(), i.path.clone(), i.block.clone()))
                .filter(|r| r.matches(env))
            {
                Some(r) if r.hash == planned => continue,
                Some(r) => r.tx.clone(),
                None => None,
            },
        };
        all.push(Record {
            target: i.target.clone(),
            path: i.path.clone(),
            block: i.block.clone(),
            item: i.item.clone(),
            scope: env.scope.to_string(),
            project: (env.scope == Scope::Project).then(|| env.project.clone()),
            profile: profile.to_string(),
            hash: planned,
            tx,
            at_ms: now,
        });
    }
    if all.len() == before {
        return Ok(());
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

/// An MCP server entry an apply wrote into a tool's config, or removed from it.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ServerRecord {
    pub target: String,
    /// The real config file.
    pub path: PathBuf,
    /// The server's name.
    pub name: String,
    /// `user` or `project`.
    pub scope: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub project: Option<PathBuf>,
    /// Name of the profile that was applied.
    pub profile: String,
    /// Where that profile came from (see [`crate::source::Resolved::source`]).
    /// Records written before kitbag recorded it have none.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
    /// [`entry_hash`] of the entry as written; `None` when the apply removed it.
    pub hash: Option<String>,
    /// The apply left the entry as it was and turned the server off through a
    /// setting of the tool's own (a name in a list, see
    /// [`crate::adapters::common::sync_off_list`]), so a later apply that
    /// enables it again takes the name out of that list.
    #[serde(default, skip_serializing_if = "is_false")]
    pub off: bool,
    /// The transaction that wrote it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tx: Option<String>,
    pub at_ms: u128,
}

fn is_false(b: &bool) -> bool {
    !*b
}

/// A hash of one server entry (JSON, or a Codex table as JSON) that doesn't
/// depend on key order.
pub fn entry_hash(v: &serde_json::Value) -> String {
    use serde_json::Value;
    fn sorted(v: &Value) -> Value {
        match v {
            Value::Object(m) => {
                let mut keys: Vec<&String> = m.keys().collect();
                keys.sort();
                Value::Object(
                    keys.into_iter()
                        .map(|k| (k.clone(), sorted(&m[k])))
                        .collect(),
                )
            }
            Value::Array(a) => Value::Array(a.iter().map(sorted).collect()),
            other => other.clone(),
        }
    }
    sha256(Some(sorted(v).to_string().as_bytes())).unwrap_or_default()
}

/// Where the next apply copies an unreadable `applied-servers.json` before
/// starting it afresh.
fn servers_backup(state_dir: &Path) -> PathBuf {
    state_dir.join(format!("{SERVERS_FILE}.bak"))
}

/// The server records, and a warning when the file can't be parsed (it is
/// then treated as empty, so nothing is removed, and the next apply that
/// writes a server copies it to `.bak` and replaces it).
fn load_servers(state_dir: &Path) -> Result<(Vec<ServerRecord>, Option<String>)> {
    let path = state_dir.join(SERVERS_FILE);
    match fs::read_to_string(&path) {
        Ok(t) => Ok(match serde_json::from_str(&t) {
            Ok(v) => (v, None),
            Err(e) => (
                Vec::new(),
                Some(format!(
                    "could not read {} ({e}), so no MCP server is removed for being disabled in the profile; the next apply starts the file afresh and keeps the old one as {}",
                    path.display(),
                    servers_backup(state_dir).display()
                )),
            ),
        }),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok((Vec::new(), None)),
        Err(e) => Err(e).with_context(|| format!("reading {}", path.display())),
    }
}

/// For every (target, file, server) at `env`'s scope (and project), the
/// record of the latest apply that still stands (not undone), and a warning
/// if the records can't be read.
pub fn applied_servers(state_dir: &Path, env: &Env) -> Result<(Vec<ServerRecord>, Option<String>)> {
    let undone: Vec<String> = Store::new(state_dir)
        .list()?
        .into_iter()
        .filter(|t| t.status != Status::Applied)
        .map(|t| t.id)
        .collect();
    let mut latest: BTreeMap<(String, PathBuf, String), ServerRecord> = BTreeMap::new();
    let (records, warning) = load_servers(state_dir)?;
    for r in records {
        let matches = r.scope == env.scope.to_string()
            && (env.scope == Scope::User || r.project.as_deref() == Some(env.project.as_path()));
        // Only records of a written transaction count: without one, nothing was written.
        if !matches || r.tx.as_ref().is_none_or(|t| undone.contains(t)) {
            continue;
        }
        latest.insert((r.target.clone(), r.path.clone(), r.name.clone()), r);
    }
    Ok((latest.into_values().collect(), warning))
}

/// Remember the MCP server entries `plan` wrote or removed, once written as `tx`.
fn record_servers(
    state_dir: &Path,
    plan: &Plan,
    env: &Env,
    profile: &str,
    tx: Option<&str>,
) -> Result<()> {
    let Some(tx) = tx else {
        return Ok(());
    };
    if plan.server_writes.is_empty() {
        return Ok(());
    }
    let now = now_ms();
    let (mut all, unreadable) = load_servers(state_dir)?;
    if unreadable.is_some() {
        // The file is about to be started afresh; keep what was there so the
        // records can still be recovered by hand.
        let path = state_dir.join(SERVERS_FILE);
        let data = fs::read(&path).with_context(|| format!("reading {}", path.display()))?;
        write_atomic(&servers_backup(state_dir), &data, Some(0o600))?;
    }
    for w in &plan.server_writes {
        all.push(ServerRecord {
            target: w.target.clone(),
            path: w.path.clone(),
            name: w.name.clone(),
            scope: env.scope.to_string(),
            project: (env.scope == Scope::Project).then(|| env.project.clone()),
            profile: profile.to_string(),
            source: plan.source.clone(),
            hash: w.hash.clone(),
            off: w.off,
            tx: Some(tx.to_string()),
            at_ms: now,
        });
    }
    // Keep the newest few per server.
    let mut seen: BTreeMap<(String, PathBuf, String, String, Option<PathBuf>), usize> =
        BTreeMap::new();
    let mut kept: Vec<ServerRecord> = all
        .into_iter()
        .rev()
        .filter(|r| {
            let key = (
                r.target.clone(),
                r.path.clone(),
                r.name.clone(),
                r.scope.clone(),
                r.project.clone(),
            );
            let n = seen.entry(key).or_default();
            *n += 1;
            *n <= KEEP_PER_ITEM
        })
        .collect();
    kept.reverse();
    fs::create_dir_all(state_dir).with_context(|| format!("creating {}", state_dir.display()))?;
    let json = serde_json::to_string_pretty(&kept)? + "\n";
    write_atomic(&state_dir.join(SERVERS_FILE), json.as_bytes(), Some(0o600))
}

/// A rule, command, skill or agent an apply wrote to a tool, or removed from it.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct FileRecord {
    pub target: String,
    /// The real file, or for a skill its directory.
    pub path: PathBuf,
    /// `rule <name>`, `command <name>`, `skill <name>` or `agent <name>`.
    pub item: String,
    /// `user` or `project`.
    pub scope: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub project: Option<PathBuf>,
    /// Name of the profile that was applied.
    pub profile: String,
    /// Where that profile came from (see [`crate::source::Resolved::source`]).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
    /// [`hash`] of the file as written ([`files_hash`] for a skill); `None`
    /// when the apply removed it.
    pub hash: Option<String>,
    /// For a skill: the hash of each file's content, by path under `path`.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub files: BTreeMap<String, String>,
    /// The transaction that wrote it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tx: Option<String>,
    pub at_ms: u128,
}

/// A hash of a file's bytes, for the files of a skill.
pub fn content_hash(bytes: &[u8]) -> String {
    sha256(Some(bytes)).unwrap_or_default()
}

/// One hash for the files of a skill (path -> [`content_hash`]).
pub fn files_hash(files: &BTreeMap<String, String>) -> String {
    let all: String = files.iter().map(|(p, h)| format!("{p}\0{h}\n")).collect();
    content_hash(all.as_bytes())
}

fn files_backup(state_dir: &Path) -> PathBuf {
    state_dir.join(format!("{FILES_FILE}.bak"))
}

/// The file records, and a warning when the file can't be parsed (it is then
/// treated as empty, so nothing is removed, and the next apply that writes
/// one copies it to `.bak` and replaces it).
fn load_files(state_dir: &Path) -> Result<(Vec<FileRecord>, Option<String>)> {
    let path = state_dir.join(FILES_FILE);
    match fs::read_to_string(&path) {
        Ok(t) => Ok(match serde_json::from_str(&t) {
            Ok(v) => (v, None),
            Err(e) => (
                Vec::new(),
                Some(format!(
                    "could not read {} ({e}), so no rule, command, skill or agent is removed for being disabled in the profile; the next apply starts the file afresh and keeps the old one as {}",
                    path.display(),
                    files_backup(state_dir).display()
                )),
            ),
        }),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok((Vec::new(), None)),
        Err(e) => Err(e).with_context(|| format!("reading {}", path.display())),
    }
}

/// For every (target, file, item) at `env`'s scope (and project), the record
/// of the latest apply that still stands (not undone), and a warning if the
/// records can't be read.
pub fn applied_files(state_dir: &Path, env: &Env) -> Result<(Vec<FileRecord>, Option<String>)> {
    let undone: Vec<String> = Store::new(state_dir)
        .list()?
        .into_iter()
        .filter(|t| t.status != Status::Applied)
        .map(|t| t.id)
        .collect();
    let mut latest: BTreeMap<(String, PathBuf, String), FileRecord> = BTreeMap::new();
    let (records, warning) = load_files(state_dir)?;
    for r in records {
        let matches = r.scope == env.scope.to_string()
            && (env.scope == Scope::User || r.project.as_deref() == Some(env.project.as_path()));
        if !matches || r.tx.as_ref().is_none_or(|t| undone.contains(t)) {
            continue;
        }
        latest.insert((r.target.clone(), r.path.clone(), r.item.clone()), r);
    }
    Ok((latest.into_values().collect(), warning))
}

/// Remember the rule, command, skill and agent files `plan` wrote or removed,
/// once written as `tx`.
fn record_files(
    state_dir: &Path,
    plan: &Plan,
    env: &Env,
    profile: &str,
    tx: Option<&str>,
) -> Result<()> {
    let Some(tx) = tx else {
        return Ok(());
    };
    if plan.file_writes.is_empty() {
        return Ok(());
    }
    let now = now_ms();
    let (mut all, unreadable) = load_files(state_dir)?;
    if unreadable.is_some() {
        let path = state_dir.join(FILES_FILE);
        let data = fs::read(&path).with_context(|| format!("reading {}", path.display()))?;
        write_atomic(&files_backup(state_dir), &data, Some(0o600))?;
    }
    for w in &plan.file_writes {
        all.push(FileRecord {
            target: w.target.clone(),
            path: w.path.clone(),
            item: w.item.clone(),
            scope: env.scope.to_string(),
            project: (env.scope == Scope::Project).then(|| env.project.clone()),
            profile: profile.to_string(),
            source: plan.source.clone(),
            hash: w.hash.clone(),
            files: w.files.clone(),
            tx: Some(tx.to_string()),
            at_ms: now,
        });
    }
    // Keep the newest few per item.
    let mut seen: BTreeMap<(String, PathBuf, String, String, Option<PathBuf>), usize> =
        BTreeMap::new();
    let mut kept: Vec<FileRecord> = all
        .into_iter()
        .rev()
        .filter(|r| {
            let key = (
                r.target.clone(),
                r.path.clone(),
                r.item.clone(),
                r.scope.clone(),
                r.project.clone(),
            );
            let n = seen.entry(key).or_default();
            *n += 1;
            *n <= KEEP_PER_ITEM
        })
        .collect();
    kept.reverse();
    fs::create_dir_all(state_dir).with_context(|| format!("creating {}", state_dir.display()))?;
    let json = serde_json::to_string_pretty(&kept)? + "\n";
    write_atomic(&state_dir.join(FILES_FILE), json.as_bytes(), Some(0o600))
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
    /// A rule file, or text outside kitbag's block in an instructions file,
    /// that exists only in the tool: a one-off that never made it back to the
    /// profile. Not counted as drift unless `status --strict`; `sync --from
    /// <target> --adopt` pulls it into the profile.
    ToolOnly,
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
            State::ToolOnly => "tool only",
        }
    }

    /// Does this differ from the profile (or, without one, from the last apply)?
    pub fn is_drift(self) -> bool {
        !matches!(
            self,
            State::InSync | State::Unsupported | State::Removed | State::ToolOnly
        )
    }

    /// Is this in the tool but not in the profile (counted as drift only by
    /// `status --strict`)?
    pub fn is_tool_only(self) -> bool {
        matches!(self, State::Removed | State::ToolOnly)
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

/// Something in a tool that the profile doesn't have and kitbag didn't write.
#[derive(Debug, Clone)]
pub struct ToolOnly {
    pub target: String,
    /// The rule file, or the instructions file holding the text.
    pub path: PathBuf,
    pub what: ToolOnlyKind,
}

#[derive(Debug, Clone)]
pub enum ToolOnlyKind {
    /// A rule file of its own, as capture read it (redacted).
    Rule {
        rule: Rule,
        /// An earlier apply of this profile wrote it; the rule has since been
        /// taken out of the profile.
        was_applied: bool,
    },
    /// Paragraphs outside kitbag's blocks that the profile's instructions
    /// don't hold (as on disk, not redacted).
    Text { paragraphs: Vec<String> },
}

impl ToolOnly {
    /// `rule hotfix`, `unmanaged text`.
    pub fn item(&self) -> String {
        match &self.what {
            ToolOnlyKind::Rule { rule, .. } => format!("rule {}", rule.name),
            ToolOnlyKind::Text { .. } => "unmanaged text".to_string(),
        }
    }

    pub fn detail(&self) -> String {
        match &self.what {
            ToolOnlyKind::Rule {
                was_applied: true, ..
            } => "applied from this profile before, then taken out of it; apply leaves the file"
                .to_string(),
            ToolOnlyKind::Rule { .. } => {
                "rule file only in this tool, not in the profile".to_string()
            }
            ToolOnlyKind::Text { paragraphs } => format!(
                "{} paragraph(s) outside kitbag's block, not in the profile's instructions",
                paragraphs.len()
            ),
        }
    }
}

/// The same file, comparing real paths where they resolve.
pub fn same_file(a: &Path, b: &Path) -> bool {
    a == b
        || match (a.canonicalize(), b.canonicalize()) {
            (Ok(x), Ok(y)) => x == y,
            _ => false,
        }
}

/// What `targets` hold that `profile` doesn't, at `env`'s scope: rule files
/// of their own that aren't the profile's (and weren't written by an apply of
/// another profile), and, in the instructions files where `plan` manages the
/// profile's block, paragraphs outside kitbag's blocks that the profile's
/// instructions don't hold. `captured` is each target's capture, by id
/// (`None` when it failed). `plan` is the plan of `profile` for `targets`.
pub fn tool_only(
    state_dir: &Path,
    env: &Env,
    profile: &Profile,
    plan: &Plan,
    captured: &[(&str, Option<&Captured>)],
) -> Result<Vec<ToolOnly>> {
    let last = last_applied(state_dir)?;
    let block_id = crate::model::slugify(&profile.name);
    let mut out = Vec::new();
    for (id, cap) in captured {
        if !plan.targets.iter().any(|t| t == id) {
            continue;
        }
        if let Some(cap) = cap {
            for (name, path) in &cap.rule_files {
                if profile.rules.iter().any(|r| &r.name == name)
                    || plan
                        .items
                        .iter()
                        .any(|i| i.target == *id && same_file(&i.path, path))
                {
                    continue;
                }
                let Some(rule) = cap.profile.rules.iter().find(|r| &r.name == name) else {
                    continue;
                };
                // Written by an apply that still stands: whose was it?
                let record = last.values().find(|r| {
                    r.target == *id
                        && r.block.is_none()
                        && r.hash.is_some()
                        && r.matches(env)
                        && same_file(&r.path, path)
                });
                let was_applied = match record {
                    Some(r) if r.profile != profile.name => continue,
                    Some(_) => true,
                    None => false,
                };
                out.push(ToolOnly {
                    target: id.to_string(),
                    path: path.clone(),
                    what: ToolOnlyKind::Rule {
                        rule: rule.clone(),
                        was_applied,
                    },
                });
            }
        }
        let Some(instructions) = profile.instructions.as_deref() else {
            continue;
        };
        let mut seen: Vec<&Path> = Vec::new();
        for i in &plan.items {
            if i.target != *id
                || i.block.as_deref() != Some(block_id.as_str())
                || seen.contains(&i.path.as_path())
            {
                continue;
            }
            seen.push(&i.path);
            let Some(text) = item_text(read_opt(&i.path)?.as_deref(), None) else {
                continue;
            };
            let paragraphs =
                block::paragraphs_missing_from(&block::split(&text).outside, instructions);
            if !paragraphs.is_empty() {
                out.push(ToolOnly {
                    target: id.to_string(),
                    path: i.path.clone(),
                    what: ToolOnlyKind::Text { paragraphs },
                });
            }
        }
    }
    Ok(out)
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
/// and rule files become `rules/*.md`. The `adopt`ed tool-only rules become
/// `rules/*.md` too, and tool-only text is added to the end of
/// `instructions.md` (the tool's file keeps it where it is: apply doesn't
/// repeat in a block what the file already holds). Everything pulled back is
/// redacted the way `capture` does it, and the result must pass the same leak
/// scan.
pub fn pull_back(
    profile: &Profile,
    dir: &Path,
    items: &[&Item],
    adopt: &[ToolOnly],
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
                // Apply leaves out of the block what the file already has
                // outside it; keep those paragraphs in the profile.
                let outside = item_text(read_opt(&i.path)?.as_deref(), None)
                    .map(|t| block::split(&t).outside)
                    .unwrap_or_default();
                let text = match profile.instructions.as_deref() {
                    Some(old) => block::with_omitted_paragraphs(text, old, &outside),
                    None => text.to_string(),
                };
                cap.profile.instructions = None;
                add_instructions(&mut cap, &i.target, &text, &i.path);
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
            Some(b) if b.starts_with(YAML_ENTRY_PREFIX) && i.item == "instructions" => {
                // An entry of a YAML list: the plain-text entries next to it
                // are the "outside" text apply leaves out of it.
                let section = b[YAML_ENTRY_PREFIX.len()..].split(':').next().unwrap_or("");
                let outside = read_opt(&i.path)?
                    .and_then(|b| decode_text(b).ok())
                    .and_then(|t| crate::edit::yaml::parse_map(Some(&t), &i.path).ok())
                    .and_then(|m| m.get(section).and_then(|v| v.as_array().cloned()))
                    .unwrap_or_default()
                    .iter()
                    .filter_map(|v| v.as_str())
                    .collect::<Vec<_>>()
                    .join("\n\n");
                let text = match profile.instructions.as_deref() {
                    Some(old) => block::with_omitted_paragraphs(text, old, &outside),
                    None => text.to_string(),
                };
                cap.profile.instructions = None;
                add_instructions(&mut cap, &i.target, &text, &i.path);
                next.instructions = cap.profile.instructions.take();
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
    for t in adopt {
        match &t.what {
            ToolOnlyKind::Rule { rule, .. } => {
                // Applying a redacted rule back would replace the tool's file,
                // secret and all, with the redacted copy.
                if crate::secrets::has_redaction_marker(&rule.body)
                    || rule
                        .description
                        .as_deref()
                        .is_some_and(crate::secrets::has_redaction_marker)
                {
                    cap.warn(
                        &t.target,
                        format!(
                            "rule '{}' ({}) looks like it holds a secret, so it was not adopted; move it into the profile by hand",
                            rule.name,
                            t.path.display()
                        ),
                    );
                    continue;
                }
                if next.rules.iter().any(|r| r.name == rule.name) {
                    continue;
                }
                next.rules.push(rule.clone());
            }
            ToolOnlyKind::Text { paragraphs } => {
                // As for rules: other tools would get the redacted copy.
                let (clean, secret): (Vec<&String>, Vec<&String>) = paragraphs
                    .iter()
                    .partition(|p| crate::secrets::redact_text(p).1.is_empty());
                if !secret.is_empty() {
                    cap.warn(
                        &t.target,
                        format!(
                            "{} paragraph(s) of {} look like they hold a secret, so they were not adopted; move them into the profile by hand",
                            secret.len(),
                            t.path.display()
                        ),
                    );
                }
                if clean.is_empty() {
                    continue;
                }
                let text = clean
                    .iter()
                    .map(|p| p.as_str())
                    .collect::<Vec<_>>()
                    .join("\n\n");
                cap.profile.instructions = next.instructions.take();
                add_instructions(&mut cap, &t.target, &text, &t.path);
                next.instructions = cap.profile.instructions.take();
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
    fn entry_hash_ignores_key_order() {
        let a = serde_json::json!({"command": "npx", "env": {"A": "1", "B": "2"}});
        let b = serde_json::json!({"env": {"B": "2", "A": "1"}, "command": "npx"});
        assert_eq!(entry_hash(&a), entry_hash(&b));
        let c = serde_json::json!({"command": "npx", "env": {"A": "1"}});
        assert_ne!(entry_hash(&a), entry_hash(&c));
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
