//! Transactions: every apply is backed up and can be undone.
//!
//! Before anything is written, the original of every file is copied into the
//! state directory. Writes are atomic (temp file + rename) and a failure
//! part-way through rolls back what was already written. `kitbag undo`
//! replays a transaction backwards, refusing to clobber files edited since.

use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::plan_fs::{FileChange, read_opt};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Status {
    Pending,
    Applied,
    RolledBack,
    Undone,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Entry {
    pub path: PathBuf,
    pub existed: bool,
    /// Backup file name inside the transaction directory (when the file existed).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub backup: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mode: Option<u32>,
    pub before_sha256: Option<String>,
    pub after_sha256: Option<String>,
    /// Canonical form of the file's directory once it was written. Undo
    /// refuses to touch the file if its directory now resolves elsewhere (a
    /// symbolic link appeared or changed). Missing in transactions recorded
    /// by kitbag 0.1.1 and older.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub real_parent: Option<PathBuf>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Transaction {
    pub id: String,
    /// Milliseconds since the Unix epoch.
    pub created_ms: u128,
    pub status: Status,
    pub description: String,
    pub targets: Vec<String>,
    pub entries: Vec<Entry>,
    /// Directories created by this transaction (removed on undo when empty).
    pub created_dirs: Vec<PathBuf>,
}

impl Transaction {
    pub fn created_at(&self) -> String {
        let secs = i64::try_from(self.created_ms / 1000).unwrap_or(0);
        time::OffsetDateTime::from_unix_timestamp(secs)
            .ok()
            .and_then(|t| {
                t.format(&time::macros::format_description!(
                    "[year]-[month]-[day] [hour]:[minute]:[second] UTC"
                ))
                .ok()
            })
            .unwrap_or_default()
    }
}

pub fn sha256(data: Option<&[u8]>) -> Option<String> {
    data.map(|d| hex::encode(Sha256::digest(d)))
}

/// Options for writing new files.
#[derive(Debug, Clone, Default)]
pub struct WriteOptions {
    /// Files that will contain resolved secrets; written owner-only (0600 on
    /// Unix), tightening existing files too. Undo restores the old mode.
    pub private: std::collections::BTreeSet<PathBuf>,
}

/// Write `content` to `path` atomically, preserving `mode` when given.
pub fn write_atomic(path: &Path, content: &[u8], mode: Option<u32>) -> Result<()> {
    let dir = path.parent().context("path has no parent directory")?;
    let mut tmp = tempfile::Builder::new()
        .prefix(".kitbag-")
        .suffix(".tmp")
        .tempfile_in(dir)
        .with_context(|| format!("creating a temporary file in {}", dir.display()))?;
    tmp.write_all(content)?;
    tmp.as_file().sync_all()?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(
            tmp.path(),
            fs::Permissions::from_mode(mode.unwrap_or(0o644)),
        )?;
    }
    #[cfg(not(unix))]
    let _ = mode;
    tmp.persist(path)
        .map_err(|e| e.error)
        .with_context(|| format!("writing {}", path.display()))?;
    Ok(())
}

/// The permission bits of `path` (Unix only; `None` elsewhere or if missing).
pub fn file_mode(path: &Path) -> Option<u32> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::metadata(path)
            .ok()
            .map(|m| m.permissions().mode() & 0o777)
    }
    #[cfg(not(unix))]
    {
        let _ = path;
        None
    }
}

/// Where `path` really is: the deepest part of it that exists, resolved
/// through every symbolic link, plus the rest as written. A dangling link is
/// an error.
pub fn real_location(path: &Path) -> io::Result<PathBuf> {
    let mut rest = Vec::new();
    let mut cur = path;
    loop {
        if fs::symlink_metadata(cur).is_ok() {
            let mut real = fs::canonicalize(cur)?;
            for c in rest.iter().rev() {
                real.push(c);
            }
            return Ok(real);
        }
        match (cur.parent(), cur.file_name()) {
            (Some(parent), Some(name)) => {
                rest.push(name.to_os_string());
                cur = parent;
            }
            _ => return Ok(path.to_path_buf()),
        }
    }
}

/// Why undo must not touch `e.path`, if it must not: `--force` overrides
/// "edited since", never this.
fn moved(e: &Entry) -> Option<String> {
    if fs::symlink_metadata(&e.path).is_ok_and(|m| m.file_type().is_symlink()) {
        return Some("it is now a symbolic link, and kitbag wrote a regular file there".into());
    }
    let parent = e.path.parent()?;
    let real = match real_location(parent) {
        Ok(r) => r,
        Err(err) => {
            return Some(format!(
                "{} can't be resolved ({err}); a symbolic link there may dangle",
                parent.display()
            ));
        }
    };
    match &e.real_parent {
        Some(rp) if *rp == real => None,
        Some(rp) => Some(format!(
            "{} now resolves to {}, not {} where kitbag wrote it (a symbolic link changed)",
            parent.display(),
            real.display(),
            rp.display()
        )),
        // Recorded by an older kitbag, which wrote to resolved paths: a
        // symbolic link anywhere in the directory now means it may have moved.
        None => parent
            .ancestors()
            .find(|a| fs::symlink_metadata(a).is_ok_and(|m| m.file_type().is_symlink()))
            .map(|link| {
                format!(
                    "{} is a symbolic link (to {}); this apply was recorded by an older kitbag, which didn't note where the file really was",
                    link.display(),
                    real.display()
                )
            }),
    }
}

/// `mkdir -p` that records which directories it created.
fn mkdirp(dir: &Path, created: &mut Vec<PathBuf>) -> Result<()> {
    let mut missing = Vec::new();
    let mut d = dir;
    while !d.exists() {
        missing.push(d.to_path_buf());
        match d.parent() {
            Some(p) => d = p,
            None => break,
        }
    }
    for m in missing.into_iter().rev() {
        fs::create_dir(&m).with_context(|| format!("creating {}", m.display()))?;
        created.push(m);
    }
    Ok(())
}

pub struct Store {
    dir: PathBuf,
}

impl Store {
    pub fn new(dir: impl Into<PathBuf>) -> Self {
        Store { dir: dir.into() }
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    fn backups(&self) -> PathBuf {
        self.dir.join("backups")
    }

    fn tx_dir(&self, id: &str) -> PathBuf {
        self.backups().join(id)
    }

    fn save(&self, tx: &Transaction) -> Result<()> {
        let json = serde_json::to_string_pretty(tx)? + "\n";
        write_atomic(
            &self.tx_dir(&tx.id).join("transaction.json"),
            json.as_bytes(),
            Some(0o600),
        )
    }

    fn create_private_dir(path: &Path) -> Result<()> {
        fs::create_dir_all(path).with_context(|| format!("creating {}", path.display()))?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(path, fs::Permissions::from_mode(0o700))?;
        }
        Ok(())
    }

    /// Write `changes` with backups. On failure, already-written files are restored.
    pub fn commit(
        &self,
        changes: &[FileChange],
        description: &str,
        targets: &[String],
        opts: &WriteOptions,
    ) -> Result<Transaction> {
        let last = self.list()?.last().map(|t| t.created_ms);
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_millis())
            .unwrap_or(0);
        // Strictly increasing timestamps keep "undo the latest" unambiguous.
        let created_ms = match last {
            Some(l) if l >= now => l + 1,
            _ => now,
        };
        let suffix = &hex::encode(Sha256::digest(format!(
            "{created_ms}{}{description}",
            std::process::id()
        )))[..6];
        let mut tx = Transaction {
            id: format!("{created_ms}-{suffix}"),
            created_ms,
            status: Status::Pending,
            description: description.to_string(),
            targets: targets.to_vec(),
            entries: Vec::new(),
            created_dirs: Vec::new(),
        };
        Self::create_private_dir(&self.backups())?;
        Self::create_private_dir(&self.tx_dir(&tx.id))?;

        // 1. Snapshot everything first, and make sure nothing changed since planning.
        if let Err(e) = self.snapshot(&mut tx, changes) {
            let _ = fs::remove_dir_all(self.tx_dir(&tx.id));
            return Err(e);
        }
        self.save(&tx)?;

        // 2. Write.
        let mut done = 0;
        let result: Result<()> = (|| {
            for (i, c) in changes.iter().enumerate() {
                match &c.after {
                    None => match fs::remove_file(&c.path) {
                        Err(e) if e.kind() != io::ErrorKind::NotFound => {
                            return Err(e)
                                .with_context(|| format!("removing {}", c.path.display()));
                        }
                        _ => {}
                    },
                    Some(content) => {
                        let dest = &c.path;
                        let parent = dest.parent().context("path has no parent")?;
                        mkdirp(parent, &mut tx.created_dirs)?;
                        // Files holding secrets are private even if they weren't before.
                        let mode = if opts.private.contains(&c.path) {
                            Some(0o600)
                        } else {
                            tx.entries[i].mode
                        };
                        write_atomic(dest, content, mode)?;
                    }
                }
                tx.entries[i].real_parent = c.path.parent().and_then(|p| real_location(p).ok());
                done = i + 1;
            }
            Ok(())
        })();
        if let Err(e) = result {
            let entries: Vec<Entry> = tx.entries[..done].to_vec();
            let _ = self.restore(&tx, &entries, &[]);
            tx.status = Status::RolledBack;
            let _ = self.save(&tx);
            return Err(e.context("apply failed; changes already written were rolled back"));
        }
        tx.status = Status::Applied;
        self.save(&tx)?;
        Ok(tx)
    }

    fn snapshot(&self, tx: &mut Transaction, changes: &[FileChange]) -> Result<()> {
        for (n, c) in changes.iter().enumerate() {
            let cur = read_opt(&c.path)?;
            if cur != c.before {
                bail!(
                    "{} changed on disk since the plan was made; re-run to get a fresh plan",
                    c.path.display()
                );
            }
            let mut entry = Entry {
                path: c.path.clone(),
                existed: cur.is_some(),
                backup: None,
                mode: None,
                before_sha256: sha256(cur.as_deref()),
                after_sha256: sha256(c.after.as_deref()),
                real_parent: None,
            };
            if let Some(data) = &cur {
                let name = format!(
                    "{:04}-{}",
                    n + 1,
                    c.path
                        .file_name()
                        .map(|f| f.to_string_lossy().into_owned())
                        .unwrap_or_default()
                );
                write_atomic(&self.tx_dir(&tx.id).join(&name), data, Some(0o600))?;
                entry.backup = Some(name);
                entry.mode = file_mode(&c.path);
            }
            tx.entries.push(entry);
        }
        Ok(())
    }

    /// Put `entries` back as they were before `tx`. Directories `tx` created
    /// are removed when empty, except around files in `kept`.
    fn restore(&self, tx: &Transaction, entries: &[Entry], kept: &[PathBuf]) -> Result<()> {
        for e in entries.iter().rev() {
            match (&e.backup, e.existed) {
                (Some(b), true) => {
                    let data = fs::read(self.tx_dir(&tx.id).join(b))
                        .with_context(|| format!("reading backup of {}", e.path.display()))?;
                    if let Some(parent) = e.path.parent() {
                        fs::create_dir_all(parent)?;
                    }
                    write_atomic(&e.path, &data, e.mode)?;
                }
                _ => match fs::remove_file(&e.path) {
                    Err(err) if err.kind() != io::ErrorKind::NotFound => {
                        return Err(err).with_context(|| format!("removing {}", e.path.display()));
                    }
                    _ => {}
                },
            }
        }
        for d in tx.created_dirs.iter().rev() {
            if kept.iter().any(|k| k.starts_with(d)) {
                continue;
            }
            let _ = fs::remove_dir(d); // only succeeds when empty
        }
        Ok(())
    }

    /// All transactions, oldest first.
    pub fn list(&self) -> Result<Vec<Transaction>> {
        let rd = match fs::read_dir(self.backups()) {
            Ok(rd) => rd,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(e) => {
                return Err(e).with_context(|| format!("listing {}", self.backups().display()));
            }
        };
        let mut out = Vec::new();
        for entry in rd {
            let path = entry?.path().join("transaction.json");
            // Ignore corrupt or foreign entries rather than failing every command.
            if let Ok(text) = fs::read_to_string(&path)
                && let Ok(tx) = serde_json::from_str::<Transaction>(&text)
            {
                out.push(tx);
            }
        }
        out.sort_by(|a, b| {
            a.created_ms
                .cmp(&b.created_ms)
                .then_with(|| a.id.cmp(&b.id))
        });
        Ok(out)
    }

    /// Files modified after `tx` was applied (undo would overwrite those edits).
    pub fn drift(&self, tx: &Transaction) -> Result<Vec<PathBuf>> {
        self.drift_in(&tx.entries)
    }

    fn drift_in(&self, entries: &[Entry]) -> Result<Vec<PathBuf>> {
        let mut out = Vec::new();
        for e in entries {
            if sha256(read_opt(&e.path)?.as_deref()) != e.after_sha256 {
                out.push(e.path.clone());
            }
        }
        Ok(out)
    }

    /// Undo transaction `id` (default: the most recent applied one).
    ///
    /// A file whose directory now resolves somewhere else than where it was
    /// written (a symbolic link appeared or changed), or that is now a link
    /// itself, is left alone even with `force`, and reported in
    /// [`Undone::refused`]; the rest is undone.
    pub fn undo(&self, id: Option<&str>, force: bool) -> Result<Undone> {
        let all = self.list()?;
        let mut tx = match id {
            Some(id) => {
                let matches: Vec<&Transaction> = all
                    .iter()
                    .filter(|t| t.id == id || t.id.starts_with(id))
                    .collect();
                match matches.as_slice() {
                    [] => bail!("no transaction matching '{id}' (see 'kitbag history')"),
                    [one] => (*one).clone(),
                    _ => bail!("'{id}' matches several transactions; use more characters"),
                }
            }
            None => all
                .iter()
                .rev()
                .find(|t| t.status == Status::Applied)
                .cloned()
                .ok_or_else(|| anyhow::anyhow!("nothing to undo"))?,
        };
        if tx.status != Status::Applied {
            bail!("transaction {} is {:?}, not applied", tx.id, tx.status);
        }
        let later: Vec<&str> = all
            .iter()
            .filter(|t| t.status == Status::Applied && t.created_ms > tx.created_ms)
            .map(|t| t.id.as_str())
            .collect();
        if !later.is_empty() && !force {
            bail!(
                "newer applies exist ({}); undo those first or pass --force",
                later.join(", ")
            );
        }
        let mut refused = Vec::new();
        let mut entries = Vec::new();
        for e in &tx.entries {
            match moved(e) {
                Some(why) => refused.push(Refused {
                    path: e.path.clone(),
                    reason: why,
                    backup: e.backup.as_ref().map(|b| self.tx_dir(&tx.id).join(b)),
                }),
                None => entries.push(e.clone()),
            }
        }
        let drifted = self.drift_in(&entries)?;
        if !drifted.is_empty() && !force {
            let list: Vec<String> = drifted
                .iter()
                .map(|p| format!("  {}", p.display()))
                .collect();
            bail!(
                "these files were modified after {} and undo would overwrite those edits:\n{}\nre-run with --force to restore them anyway",
                tx.id,
                list.join("\n")
            );
        }
        let kept: Vec<PathBuf> = refused.iter().map(|r| r.path.clone()).collect();
        self.restore(&tx, &entries, &kept)?;
        tx.status = Status::Undone;
        self.save(&tx)?;
        Ok(Undone { tx, refused })
    }
}

/// The result of [`Store::undo`].
#[derive(Debug)]
pub struct Undone {
    pub tx: Transaction,
    /// Files left alone because they are no longer where kitbag wrote them.
    pub refused: Vec<Refused>,
}

#[derive(Debug)]
pub struct Refused {
    pub path: PathBuf,
    pub reason: String,
    /// The backup of the original, when there was one.
    pub backup: Option<PathBuf>,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn change(path: &Path, after: Option<&str>) -> FileChange {
        FileChange {
            path: path.to_path_buf(),
            before: read_opt(path).unwrap(),
            after: after.map(|s| s.as_bytes().to_vec()),
        }
    }

    #[test]
    fn commit_and_undo() {
        let root = tempfile::tempdir().unwrap();
        let store = Store::new(root.path().join("state"));
        let existing = root.path().join("a.json");
        fs::write(&existing, "{}").unwrap();
        let created = root.path().join("new/dir/b.md");
        let tx = store
            .commit(
                &[
                    change(&existing, Some("{\"x\":1}")),
                    change(&created, Some("hi")),
                ],
                "test",
                &["t".into()],
                &WriteOptions::default(),
            )
            .unwrap();
        assert_eq!(fs::read_to_string(&existing).unwrap(), "{\"x\":1}");
        assert_eq!(fs::read_to_string(&created).unwrap(), "hi");
        assert_eq!(store.list().unwrap().len(), 1);

        let undone = store.undo(None, false).unwrap();
        assert_eq!(undone.tx.id, tx.id);
        assert!(undone.refused.is_empty());
        assert_eq!(fs::read_to_string(&existing).unwrap(), "{}");
        assert!(!created.exists());
        assert!(
            !root.path().join("new").exists(),
            "created directories are removed"
        );
        assert!(store.undo(None, false).is_err(), "nothing left to undo");
    }

    #[test]
    fn undo_refuses_to_clobber_later_edits() {
        let root = tempfile::tempdir().unwrap();
        let store = Store::new(root.path().join("state"));
        let f = root.path().join("f.txt");
        store
            .commit(
                &[change(&f, Some("kitbag"))],
                "t",
                &[],
                &WriteOptions::default(),
            )
            .unwrap();
        fs::write(&f, "user edit").unwrap();
        let e = store.undo(None, false).unwrap_err();
        assert!(e.to_string().contains("modified after"), "{e}");
        store.undo(None, true).unwrap();
        assert!(!f.exists());
    }

    #[test]
    fn refuses_stale_plans() {
        let root = tempfile::tempdir().unwrap();
        let store = Store::new(root.path().join("state"));
        let f = root.path().join("f.txt");
        fs::write(&f, "v1").unwrap();
        let c = change(&f, Some("v2"));
        fs::write(&f, "changed meanwhile").unwrap();
        assert!(
            store
                .commit(&[c], "t", &[], &WriteOptions::default())
                .is_err()
        );
        assert_eq!(fs::read_to_string(&f).unwrap(), "changed meanwhile");
        assert!(store.list().unwrap().is_empty());
    }

    #[test]
    fn undo_order_is_enforced() {
        let root = tempfile::tempdir().unwrap();
        let store = Store::new(root.path().join("state"));
        let f = root.path().join("f.txt");
        let first = store
            .commit(&[change(&f, Some("1"))], "a", &[], &WriteOptions::default())
            .unwrap();
        store
            .commit(&[change(&f, Some("2"))], "b", &[], &WriteOptions::default())
            .unwrap();
        assert!(
            store
                .undo(Some(&first.id), false)
                .unwrap_err()
                .to_string()
                .contains("newer applies")
        );
        store.undo(None, false).unwrap();
        store.undo(None, false).unwrap();
        assert!(!f.exists());
    }

    #[cfg(unix)]
    #[test]
    fn undo_never_follows_a_directory_swapped_for_a_link() {
        use std::os::unix::fs::symlink;
        for legacy in [false, true] {
            let root = tempfile::tempdir().unwrap();
            let store = Store::new(root.path().join("state"));
            let real = root.path().join("real");
            let kept = root.path().join("kept.txt");
            let tx = store
                .commit(
                    &[
                        change(&real.join("config.toml"), Some("x")),
                        change(&kept, Some("y")),
                    ],
                    "t",
                    &[],
                    &WriteOptions::default(),
                )
                .unwrap();
            assert_eq!(
                tx.entries[0].real_parent.as_deref(),
                Some(real.canonicalize().unwrap().as_path())
            );
            if legacy {
                // Transactions written by 0.1.1 have no `real_parent`.
                let file = store.tx_dir(&tx.id).join("transaction.json");
                let mut v: serde_json::Value =
                    serde_json::from_str(&fs::read_to_string(&file).unwrap()).unwrap();
                for e in v["entries"].as_array_mut().unwrap() {
                    e.as_object_mut().unwrap().remove("real_parent");
                }
                fs::write(&file, v.to_string()).unwrap();
            }
            let outside = root.path().join("outside");
            fs::rename(&real, &outside).unwrap();
            symlink(&outside, &real).unwrap();
            let done = store.undo(None, true).unwrap();
            assert_eq!(done.refused.len(), 1, "legacy: {legacy}");
            assert!(done.refused[0].reason.contains("symbolic link"));
            assert!(outside.join("config.toml").exists(), "legacy: {legacy}");
            assert!(!kept.exists(), "the other file is still undone");
        }
    }

    #[cfg(unix)]
    #[test]
    fn preserves_mode_and_creates_private_files() {
        use std::os::unix::fs::PermissionsExt;
        let root = tempfile::tempdir().unwrap();
        let store = Store::new(root.path().join("state"));
        let exe = root.path().join("x.sh");
        fs::write(&exe, "a").unwrap();
        fs::set_permissions(&exe, fs::Permissions::from_mode(0o755)).unwrap();
        let secret = root.path().join("s.json");
        store
            .commit(
                &[change(&exe, Some("b")), change(&secret, Some("{}"))],
                "t",
                &[],
                &WriteOptions {
                    private: [secret.clone()].into_iter().collect(),
                },
            )
            .unwrap();
        assert_eq!(
            fs::metadata(&exe).unwrap().permissions().mode() & 0o777,
            0o755
        );
        assert_eq!(
            fs::metadata(&secret).unwrap().permissions().mode() & 0o777,
            0o600
        );
    }
}
