//! `kitbag sessions export` / `import`: move a tool's own saved conversations
//! to a local directory and back, same tool only (Claude Code to Claude
//! Code, Codex CLI to Codex CLI).
//!
//! What is moved is the tool's native JSONL transcript, as documented:
//!
//! * Claude Code: `<config>/projects/<project>/<session-id>.jsonl`, where
//!   `<config>` is `~/.claude` or `$CLAUDE_CONFIG_DIR` and `<project>` is the
//!   working directory with every non-alphanumeric character replaced by `-`
//!   (<https://code.claude.com/docs/en/sessions>, "Where transcripts are
//!   stored"). `claude --resume /absolute/path/to/file.jsonl` resumes one.
//! * Codex CLI: `$CODEX_HOME/sessions/YYYY/MM/DD/rollout-*.jsonl` (default
//!   `~/.codex`).
//!
//! The entry format inside those files is internal to each tool and changes
//! between versions, so nothing here parses it as a schema: every line is
//! read as generic JSON (or plain text if it isn't), the strings in it are
//! redacted, and the line is written back. A line nothing was found in is
//! kept byte for byte.
//!
//! Sessions are never part of a profile, `capture`, `apply` or a tarball.
//! Redaction is on by default; `--encrypt` writes one age archive instead of
//! plain files. Import writes only into the destination tool's own directory
//! and never starts a tool.

use std::collections::BTreeSet;
use std::fs;
use std::io::Write;
use std::path::{Component, Path, PathBuf};

use anyhow::{Context, Result, bail};
use clap::{Args, Subcommand, ValueEnum};
use serde::{Deserialize, Serialize};

use crate::adapters::Env;
use crate::bundle::{self, Seal};
use crate::secrets;
use crate::secrets_cmd::{self, SealArgs, UnlockArgs};

/// The manifest file of a plain export.
pub const MANIFEST: &str = "kitbag-sessions.json";
/// The one file an encrypted export writes.
pub const ARCHIVE: &str = "sessions.age";
/// A plain export keeps the transcripts under this folder.
const FILES: &str = "files";
/// Most plaintext an encrypted archive may hold (it is built in memory).
const MAX_ARCHIVE_BYTES: u64 = 512 * 1024 * 1024;
/// Largest single transcript kitbag reads.
const MAX_FILE_BYTES: u64 = 256 * 1024 * 1024;

/// Tools whose sessions can be exported and imported.
#[derive(ValueEnum, Clone, Copy, Debug, PartialEq, Eq)]
pub enum SessionTool {
    #[value(name = "claude-code")]
    ClaudeCode,
    #[value(name = "codex")]
    Codex,
}

impl SessionTool {
    fn id(self) -> &'static str {
        match self {
            SessionTool::ClaudeCode => "claude-code",
            SessionTool::Codex => "codex",
        }
    }
    fn from_id(id: &str) -> Option<Self> {
        match id {
            "claude-code" => Some(SessionTool::ClaudeCode),
            "codex" => Some(SessionTool::Codex),
            _ => None,
        }
    }
    /// The tool's configuration directory, as every other command resolves it.
    fn root(self, env: &Env) -> PathBuf {
        match self {
            SessionTool::ClaudeCode => env
                .location_var("CLAUDE_CONFIG_DIR")
                .unwrap_or_else(|| env.home.join(".claude")),
            SessionTool::Codex => env
                .location_var("CODEX_HOME")
                .unwrap_or_else(|| env.home.join(".codex")),
        }
    }
}

/// How an export is encrypted (same flags and age mechanism as the secrets bundle).
#[derive(Args, Debug, Clone, Default)]
pub struct ExportSealArgs {
    /// Write one age-encrypted archive (sessions.age) instead of plain files.
    /// Encrypts to --secrets-recipient, or to a passphrase from
    /// --secrets-passphrase-file (a terminal asks if neither is given)
    #[arg(long)]
    pub encrypt: bool,
    /// With --encrypt: a file whose first line is the passphrase (age's
    /// scrypt). Never put a passphrase on the command line. Must not be
    /// inside -o DIR
    #[arg(long, value_name = "PATH")]
    pub secrets_passphrase_file: Option<PathBuf>,
    /// With --encrypt: encrypt to this age public key (age1...) or SSH public
    /// key, or a file of them. Repeatable. Can't be combined with a passphrase
    #[arg(long, value_name = "KEY")]
    pub secrets_recipient: Vec<String>,
}

impl ExportSealArgs {
    fn wanted(&self) -> bool {
        self.encrypt || self.secrets_passphrase_file.is_some() || !self.secrets_recipient.is_empty()
    }
}

#[derive(Subcommand, Debug)]
pub enum SessionsCmd {
    /// Copy one tool's saved conversations into a local directory (same tool
    /// only). Secrets are redacted unless --no-redact. Never part of a
    /// profile
    Export {
        /// The tool whose sessions to export
        #[arg(long, value_name = "TOOL")]
        from: SessionTool,
        /// The directory to write (created if missing; must be empty unless
        /// --force). Keep it outside any git repository
        #[arg(short = 'o', long, value_name = "DIR")]
        output: PathBuf,
        /// Claude Code only: one project, given as its directory under
        /// projects/ or as the working directory's path
        #[arg(long, value_name = "DIR", allow_hyphen_values = true)]
        project: Option<String>,
        /// Keep secrets in the transcripts. They routinely hold file
        /// contents, command output and credentials
        #[arg(long)]
        no_redact: bool,
        /// Write into a directory that is not empty
        #[arg(long)]
        force: bool,
        #[command(flatten)]
        seal: ExportSealArgs,
    },
    /// Restore an export into the same tool's own session directory. Starts
    /// nothing and loads nothing into any session
    Import {
        /// The tool to import into; must be the tool the export came from
        #[arg(long, value_name = "TOOL")]
        to: SessionTool,
        /// The export directory (or its sessions.age)
        dir: PathBuf,
        /// Overwrite a session file that already exists
        #[arg(long)]
        force: bool,
        #[command(flatten)]
        unlock: ImportUnlockArgs,
    },
}

/// How to open an encrypted export.
#[derive(Args, Debug, Clone, Default)]
pub struct ImportUnlockArgs {
    /// A file whose first line is the archive's passphrase (a terminal asks
    /// otherwise). Never put a passphrase on the command line
    #[arg(long, value_name = "PATH")]
    pub secrets_passphrase_file: Option<PathBuf>,
    /// An age identity file or unprotected SSH private key the archive was
    /// encrypted to. Repeatable
    #[arg(long, value_name = "PATH")]
    pub secrets_identity: Vec<PathBuf>,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
struct FileEntry {
    /// Relative to the tool's directory, `/`-separated.
    path: String,
    bytes: u64,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
struct Manifest {
    kitbag_sessions: u32,
    tool: String,
    kitbag_version: String,
    /// Whether secrets were redacted from the transcripts.
    redacted: bool,
    files: Vec<FileEntry>,
}

/// An export in memory: the manifest and each transcript's text.
#[derive(Serialize, Deserialize)]
struct Archive {
    manifest: Manifest,
    contents: Vec<String>,
}

// ---------------------------------------------------------------- redaction

/// Redact one transcript. Lines that are JSON have every string redacted
/// (and string values under secret-named keys); other lines are redacted as
/// text. A line with nothing found is returned untouched.
pub fn redact_transcript(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for line in text.split_inclusive('\n') {
        let body = line.trim_end_matches(['\n', '\r']);
        out.push_str(&redact_line(body));
        out.push_str(&line[body.len()..]);
    }
    out
}

fn redact_line(line: &str) -> String {
    if line.trim().is_empty() {
        return line.to_string();
    }
    match serde_json::from_str::<serde_json::Value>(line) {
        Ok(mut v) => {
            let kinds = secrets::redact_json_strings(&mut v);
            let fields = secrets::redact_secret_fields(&mut v, None);
            if kinds.is_empty() && !fields {
                line.to_string()
            } else {
                serde_json::to_string(&v).unwrap_or_else(|_| secrets::redact_text(line).0)
            }
        }
        Err(_) => secrets::redact_text(line).0,
    }
}

// ----------------------------------------------------------------- paths

/// `/`-separated relative path that stays inside the tool's directory.
fn safe_rel(rel: &str) -> Result<Vec<&str>> {
    let parts: Vec<&str> = rel.split('/').collect();
    for p in &parts {
        if p.is_empty()
            || *p == "."
            || *p == ".."
            || p.contains(['\\', ':', '\0'])
            || p.chars().any(char::is_control)
        {
            bail!("the export names an unsafe path ({rel:?}); refusing it");
        }
    }
    Ok(parts)
}

/// Is `rel` where `tool` documents its transcripts?
fn documented_path(tool: SessionTool, rel: &str) -> bool {
    let Ok(parts) = safe_rel(rel) else {
        return false;
    };
    let Some(name) = parts.last() else {
        return false;
    };
    match tool {
        // projects/<project>/<session-id>.jsonl
        SessionTool::ClaudeCode => {
            parts.len() == 3
                && parts[0] == "projects"
                && name.ends_with(".jsonl")
                && !name.contains(".orphaned-")
        }
        // sessions/YYYY/MM/DD/rollout-*.jsonl
        SessionTool::Codex => {
            parts.len() == 5
                && parts[0] == "sessions"
                && name.starts_with("rollout-")
                && name.ends_with(".jsonl")
        }
    }
}

/// Claude Code's project directory name for a working directory.
fn encode_cwd(path: &str) -> String {
    path.chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect()
}

/// A path made absolute and with its existing part resolved, to compare
/// where things really are.
fn resolved(path: &Path) -> PathBuf {
    let abs = std::path::absolute(path).unwrap_or_else(|_| path.to_path_buf());
    let mut tail = Vec::new();
    let mut cur = abs.as_path();
    loop {
        if let Ok(c) = cur.canonicalize() {
            let mut out = c;
            for t in tail.iter().rev() {
                out.push(t);
            }
            return out;
        }
        match (cur.parent(), cur.file_name()) {
            (Some(p), Some(n)) => {
                tail.push(n.to_os_string());
                cur = p;
            }
            _ => return abs,
        }
    }
}

// ----------------------------------------------------------------- export

struct Found {
    rel: String,
    path: PathBuf,
}

fn list_dir(dir: &Path) -> Vec<fs::DirEntry> {
    let mut v: Vec<_> = fs::read_dir(dir)
        .map(|r| r.flatten().collect())
        .unwrap_or_default();
    v.sort_by_key(|e| e.file_name());
    v
}

fn is_real_dir(e: &fs::DirEntry) -> bool {
    e.file_type().is_ok_and(|t| t.is_dir())
}
fn is_real_file(e: &fs::DirEntry) -> bool {
    e.file_type().is_ok_and(|t| t.is_file())
}

/// The transcripts under a Claude Code directory, optionally one project.
fn find_claude(root: &Path, project: Option<&str>) -> Result<Vec<Found>> {
    let projects = root.join("projects");
    let mut dirs = Vec::new();
    for e in list_dir(&projects).into_iter().filter(is_real_dir) {
        dirs.push(e.file_name().to_string_lossy().into_owned());
    }
    let chosen: Vec<String> = match project {
        None => dirs,
        Some(p) => {
            let name = if dirs.iter().any(|d| d == p) {
                p.to_string()
            } else {
                let abs = if p.contains(['/', '\\']) || Path::new(p).exists() {
                    std::path::absolute(p)?.to_string_lossy().into_owned()
                } else {
                    p.to_string()
                };
                let enc = encode_cwd(&abs);
                if enc.len() > 200 {
                    bail!(
                        "that path's project name is longer than 200 characters, so Claude Code shortens it and adds a hash kitbag does not reproduce; pass the directory name as it appears under {}",
                        projects.display()
                    );
                }
                enc
            };
            if !dirs.contains(&name) {
                bail!(
                    "no Claude Code project {name:?} under {}",
                    projects.display()
                );
            }
            vec![name]
        }
    };
    let mut out = Vec::new();
    for d in chosen {
        for e in list_dir(&projects.join(&d))
            .into_iter()
            .filter(is_real_file)
        {
            let name = e.file_name().to_string_lossy().into_owned();
            let rel = format!("projects/{d}/{name}");
            if documented_path(SessionTool::ClaudeCode, &rel) {
                out.push(Found {
                    rel,
                    path: e.path(),
                });
            }
        }
    }
    Ok(out)
}

/// The rollouts under `$CODEX_HOME/sessions/YYYY/MM/DD/`.
fn find_codex(root: &Path, skipped: &mut Vec<String>) -> Vec<Found> {
    let base = root.join("sessions");
    let mut out = Vec::new();
    for y in list_dir(&base).into_iter().filter(is_real_dir) {
        for m in list_dir(&y.path()).into_iter().filter(is_real_dir) {
            for d in list_dir(&m.path()).into_iter().filter(is_real_dir) {
                for f in list_dir(&d.path()).into_iter().filter(is_real_file) {
                    let rel = format!(
                        "sessions/{}/{}/{}/{}",
                        y.file_name().to_string_lossy(),
                        m.file_name().to_string_lossy(),
                        d.file_name().to_string_lossy(),
                        f.file_name().to_string_lossy()
                    );
                    if rel.ends_with(".jsonl.zst") {
                        skipped.push(format!(
                            "{rel}: compressed (.zst) rollouts are skipped; kitbag has no decompressor for them"
                        ));
                    } else if documented_path(SessionTool::Codex, &rel) {
                        out.push(Found {
                            rel,
                            path: f.path(),
                        });
                    }
                }
            }
        }
    }
    out
}

pub struct ExportOpts<'a> {
    pub env: &'a Env,
    pub from: SessionTool,
    pub output: &'a Path,
    pub project: Option<&'a str>,
    pub no_redact: bool,
    pub force: bool,
    pub seal: &'a ExportSealArgs,
}

pub fn export(o: &ExportOpts) -> Result<i32> {
    if o.project.is_some() && o.from != SessionTool::ClaudeCode {
        bail!("--project only applies to --from claude-code");
    }
    // Everything that can fail before anything is read or written.
    let sealing = if o.seal.wanted() {
        let plan = secrets_cmd::seal_plan(&SealArgs {
            secrets_passphrase_file: o.seal.secrets_passphrase_file.clone(),
            secrets_recipient: o.seal.secrets_recipient.clone(),
        })?;
        Some(plan)
    } else {
        None
    };
    let out_dir = resolved(o.output);
    let root = o.from.root(o.env);
    if out_dir.starts_with(resolved(&root)) {
        bail!(
            "-o {} is inside {}; pick a directory outside the tool's own",
            o.output.display(),
            root.display()
        );
    }
    if let Some(p) = &o.seal.secrets_passphrase_file
        && resolved(p).starts_with(&out_dir)
    {
        bail!(
            "the passphrase file is inside -o {}; keep it somewhere else",
            o.output.display()
        );
    }
    if out_dir.exists() {
        if !out_dir.is_dir() {
            bail!("{} exists and is not a directory", o.output.display());
        }
        if fs::read_dir(&out_dir)?.next().is_some() {
            if !o.force {
                bail!(
                    "{} is not empty; export into an empty directory, or pass --force to add to it (existing files with the same names are replaced)",
                    o.output.display()
                );
            }
            eprintln!(
                "warning: --force: writing into {}, which is not empty; files kitbag writes replace those of the same name",
                o.output.display()
            );
        }
    }
    let seal: Option<Seal> = match sealing {
        Some(plan) => Some(secrets_cmd::resolve_seal(plan)?),
        None => None,
    };
    if o.no_redact {
        eprintln!(
            "warning: --no-redact: these transcripts are exported as they are. They routinely hold file contents, command output and credentials; treat the export as secret."
        );
    }
    if in_git_repo(&out_dir) {
        eprintln!(
            "warning: {} is inside a git repository. Session exports are never part of a profile; don't commit them.",
            o.output.display()
        );
    }

    let mut skipped = Vec::new();
    let found = match o.from {
        SessionTool::ClaudeCode => find_claude(&root, o.project)?,
        SessionTool::Codex => find_codex(&root, &mut skipped),
    };
    let mut files = Vec::new();
    let mut contents = Vec::new();
    let mut total = 0u64;
    for f in found {
        let meta = fs::metadata(&f.path).with_context(|| format!("reading {}", f.rel))?;
        if meta.len() > MAX_FILE_BYTES {
            skipped.push(format!("{}: larger than 256 MiB, skipped", f.rel));
            continue;
        }
        let raw = fs::read(&f.path).with_context(|| format!("reading {}", f.rel))?;
        let Ok(text) = String::from_utf8(raw) else {
            skipped.push(format!(
                "{}: not UTF-8 text, skipped (it can't be redacted reliably)",
                f.rel
            ));
            continue;
        };
        let text = if o.no_redact {
            text
        } else {
            redact_transcript(&text)
        };
        total += text.len() as u64;
        if seal.is_some() && total > MAX_ARCHIVE_BYTES {
            bail!(
                "these sessions are too large for one encrypted archive (limit 512 MiB, built in memory); export a --project at a time, or leave out --encrypt"
            );
        }
        files.push(FileEntry {
            path: f.rel,
            bytes: text.len() as u64,
        });
        contents.push(text);
    }
    for s in &skipped {
        eprintln!("note: {s}");
    }
    if files.is_empty() {
        bail!(
            "no {} sessions found under {}; nothing was written",
            o.from.id(),
            root.display()
        );
    }
    let manifest = Manifest {
        kitbag_sessions: 1,
        tool: o.from.id().to_string(),
        kitbag_version: env!("CARGO_PKG_VERSION").to_string(),
        redacted: !o.no_redact,
        files,
    };
    let n = manifest.files.len();
    fs::create_dir_all(&out_dir).with_context(|| format!("creating {}", o.output.display()))?;
    private_dir(&out_dir);
    match seal {
        Some(how) => {
            let plain =
                zeroize::Zeroizing::new(serde_json::to_vec(&Archive { manifest, contents })?);
            let bytes = bundle::seal_bytes(&plain, &how)?;
            bundle::write_to(&out_dir.join(ARCHIVE), &bytes)?;
            println!(
                "Exported {n} {} session file(s) to {} (age-encrypted{}).",
                o.from.id(),
                out_dir.join(ARCHIVE).display(),
                if o.no_redact { "" } else { ", redacted" }
            );
        }
        None => {
            for (e, text) in manifest.files.iter().zip(&contents) {
                let dest = safe_join(&out_dir.join(FILES), &e.path)?;
                if let Some(parent) = dest.parent() {
                    fs::create_dir_all(parent)?;
                }
                crate::txn::write_atomic(&dest, text.as_bytes(), Some(0o600))?;
            }
            let mut m = serde_json::to_vec_pretty(&manifest)?;
            m.push(b'\n');
            crate::txn::write_atomic(&out_dir.join(MANIFEST), &m, Some(0o600))?;
            println!(
                "Exported {n} {} session file(s) to {}{}.",
                o.from.id(),
                out_dir.display(),
                if o.no_redact { "" } else { " (redacted)" }
            );
        }
    }
    if !o.no_redact {
        println!(
            "Secrets kitbag recognises were replaced by [REDACTED] markers; redaction is best effort, so keep the export private."
        );
    }
    Ok(0)
}

fn in_git_repo(dir: &Path) -> bool {
    dir.ancestors().any(|a| a.join(".git").exists())
}

#[cfg(unix)]
fn private_dir(dir: &Path) {
    use std::os::unix::fs::PermissionsExt;
    let _ = fs::set_permissions(dir, fs::Permissions::from_mode(0o700));
}
#[cfg(not(unix))]
fn private_dir(_dir: &Path) {}

fn safe_join(base: &Path, rel: &str) -> Result<PathBuf> {
    let mut p = base.to_path_buf();
    for part in safe_rel(rel)? {
        p.push(part);
    }
    if p.components().any(|c| matches!(c, Component::ParentDir)) {
        bail!("the export names an unsafe path ({rel:?}); refusing it");
    }
    Ok(p)
}

// ----------------------------------------------------------------- import

fn read_export(dir: &Path, unlock: &ImportUnlockArgs) -> Result<(Manifest, Vec<String>)> {
    let archive = if dir.is_file() {
        Some(dir.to_path_buf())
    } else if dir.join(ARCHIVE).is_file() {
        Some(dir.join(ARCHIVE))
    } else {
        None
    };
    if let Some(a) = archive {
        let meta = fs::metadata(&a).with_context(|| format!("reading {}", a.display()))?;
        if meta.len() > MAX_ARCHIVE_BYTES + 1024 * 1024 {
            bail!("{} is too large to be a sessions archive", a.display());
        }
        let bytes = fs::read(&a).with_context(|| format!("reading {}", a.display()))?;
        let u = secrets_cmd::unlock(
            &UnlockArgs {
                secrets_passphrase_file: unlock.secrets_passphrase_file.clone(),
                secrets_identity: unlock.secrets_identity.clone(),
            },
            &bytes,
        )?;
        let plain = bundle::open_bytes(&bytes, &u, MAX_ARCHIVE_BYTES + 1024 * 1024)
            .with_context(|| format!("opening {}", a.display()))?;
        let arc: Archive = serde_json::from_slice(&plain).map_err(|_| {
            anyhow::anyhow!("{} is not a sessions archive kitbag wrote", a.display())
        })?;
        if arc.manifest.files.len() != arc.contents.len() {
            bail!("{} is not a sessions archive kitbag wrote", a.display());
        }
        return Ok((arc.manifest, arc.contents));
    }
    let mpath = dir.join(MANIFEST);
    if !mpath.is_file() {
        bail!(
            "{} is not a kitbag sessions export (no {MANIFEST} or {ARCHIVE})",
            dir.display()
        );
    }
    let manifest: Manifest = serde_json::from_slice(&fs::read(&mpath)?)
        .map_err(|_| anyhow::anyhow!("{} is not a manifest kitbag wrote", mpath.display()))?;
    let mut contents = Vec::new();
    for e in &manifest.files {
        let p = safe_join(&dir.join(FILES), &e.path)?;
        let meta = fs::symlink_metadata(&p)
            .with_context(|| format!("the manifest lists {} but it is missing", e.path))?;
        if !meta.is_file() || meta.len() > MAX_FILE_BYTES {
            bail!("{} is not a regular file of a sensible size", e.path);
        }
        contents.push(
            String::from_utf8(fs::read(&p)?)
                .map_err(|_| anyhow::anyhow!("{} is not UTF-8 text", e.path))?,
        );
    }
    Ok((manifest, contents))
}

pub fn import(
    env: &Env,
    to: SessionTool,
    dir: &Path,
    force: bool,
    unlock: &ImportUnlockArgs,
) -> Result<i32> {
    let (manifest, contents) = read_export(dir, unlock)?;
    if manifest.kitbag_sessions != 1 {
        bail!(
            "this export has format {}; this kitbag reads format 1",
            manifest.kitbag_sessions
        );
    }
    let Some(from) = SessionTool::from_id(&manifest.tool) else {
        bail!(
            "this export is from {:?}, which kitbag can't import",
            manifest.tool
        );
    };
    if from != to {
        bail!(
            "this export is from {}; --to {} is refused. Sessions only move between the same tool (no conversion between tools)",
            from.id(),
            to.id()
        );
    }
    // Validate every path before writing any.
    let mut seen = BTreeSet::new();
    for e in &manifest.files {
        if !documented_path(to, &e.path) {
            bail!(
                "the export lists {:?}, which is not where {} keeps sessions; refusing it",
                e.path,
                to.id()
            );
        }
        if !seen.insert(e.path.clone()) {
            bail!("the export lists {:?} twice; refusing it", e.path);
        }
    }
    let root = to.root(env);
    let mut targets = Vec::new();
    for e in &manifest.files {
        targets.push(safe_join(&root, &e.path)?);
    }
    let clashes: Vec<&PathBuf> = targets
        .iter()
        .filter(|t| fs::symlink_metadata(t).is_ok())
        .collect();
    if !clashes.is_empty() && !force {
        let mut msg = format!(
            "{} session file(s) already exist; nothing was written. Pass --force to replace them:",
            clashes.len()
        );
        for c in clashes.iter().take(5) {
            msg.push_str(&format!("\n  {}", c.display()));
        }
        bail!(msg);
    }
    if !clashes.is_empty() {
        eprintln!(
            "warning: --force: replacing {} existing session file(s); the old contents are not kept",
            clashes.len()
        );
    }
    if !manifest.redacted {
        eprintln!("note: this export was made with --no-redact, so it may hold secrets.");
    }
    let mut written = Vec::new();
    for ((t, text), e) in targets.iter().zip(&contents).zip(&manifest.files) {
        if let Some(parent) = t.parent() {
            fs::create_dir_all(parent)?;
        }
        if fs::symlink_metadata(t).is_ok_and(|m| m.file_type().is_symlink()) {
            bail!(
                "{} is a symbolic link; kitbag won't write through it",
                t.display()
            );
        }
        if force {
            crate::txn::write_atomic(t, text.as_bytes(), Some(0o600))?;
        } else {
            let mut f = fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(t)
                .with_context(|| format!("creating {}", t.display()))?;
            f.write_all(text.as_bytes())?;
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                let _ = f.set_permissions(fs::Permissions::from_mode(0o600));
            }
        }
        let _ = e;
        written.push(t.clone());
    }
    println!(
        "Imported {} {} session file(s) into {}.",
        written.len(),
        to.id(),
        root.display()
    );
    println!("Nothing was started and nothing was loaded into any session.");
    match to {
        SessionTool::ClaudeCode => {
            println!("Resume one with:");
            for w in written.iter().take(10) {
                println!("  claude --resume {}", w.display());
            }
            if written.len() > 10 {
                println!("  ... and {} more", written.len() - 10);
            }
        }
        SessionTool::Codex => {
            println!(
                "The rollouts are under {}; kitbag doesn't name a resume command for Codex.",
                root.join("sessions").display()
            );
        }
    }
    Ok(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encodes_cwd_like_claude_code() {
        assert_eq!(encode_cwd("/home/u/my-repo.x"), "-home-u-my-repo-x");
    }

    #[test]
    fn unsafe_paths_are_refused() {
        for bad in ["../x", "a//b", "a/./b", "a\\b", "/abs", "a/b:c", ""] {
            assert!(safe_rel(bad).is_err(), "{bad}");
        }
        assert!(safe_rel("projects/a/b.jsonl").is_ok());
    }

    #[test]
    fn documented_paths() {
        assert!(documented_path(
            SessionTool::ClaudeCode,
            "projects/-a/s.jsonl"
        ));
        assert!(!documented_path(
            SessionTool::ClaudeCode,
            "projects/-a/s.orphaned-1-x.jsonl"
        ));
        assert!(!documented_path(SessionTool::ClaudeCode, "settings.json"));
        assert!(documented_path(
            SessionTool::Codex,
            "sessions/2026/01/02/rollout-x.jsonl"
        ));
        assert!(!documented_path(
            SessionTool::Codex,
            "sessions/2026/01/02/x.jsonl"
        ));
        assert!(!documented_path(SessionTool::Codex, "config.toml"));
    }

    #[test]
    fn untouched_lines_stay_byte_identical() {
        let text = "{ \"a\":  1 }\r\nnot json\n\n";
        assert_eq!(redact_transcript(text), text);
    }

    #[test]
    fn json_and_text_lines_are_redacted() {
        let t = "{\"m\":\"export API_TOKEN=fake-session-token-value-1234\"}\nAuthorization: Bearer fake-session-token-value-1234\n";
        let r = redact_transcript(t);
        assert!(!r.contains("fake-session-token-value-1234"), "{r}");
        assert!(r.contains("[REDACTED"), "{r}");
        assert_eq!(r.lines().count(), 2);
    }
}
