//! A copy-on-write view of the file system used to plan changes.
//!
//! Adapters never write to disk. They read and write through [`PlanFs`], which
//! records the intended content of every touched file. The result is a list of
//! [`FileChange`]s that can be shown as a diff, or committed as a transaction.

use std::collections::BTreeMap;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};

/// One planned change to a file. `None` means "does not exist".
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileChange {
    pub path: PathBuf,
    pub before: Option<Vec<u8>>,
    pub after: Option<Vec<u8>>,
}

impl FileChange {
    pub fn kind(&self) -> ChangeKind {
        match (&self.before, &self.after) {
            (None, Some(_)) => ChangeKind::Create,
            (Some(_), None) => ChangeKind::Delete,
            _ => ChangeKind::Modify,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChangeKind {
    Create,
    Modify,
    Delete,
}

#[derive(Debug, Clone)]
struct Staged {
    before: Option<Vec<u8>>,
    after: Option<Vec<u8>>,
}

#[derive(Debug, Default)]
pub struct PlanFs {
    staged: BTreeMap<PathBuf, Staged>,
    base: Option<PathBuf>,
    confine: bool,
}

/// Read a file, mapping "not found" to `None`.
pub fn read_opt(path: &Path) -> Result<Option<Vec<u8>>> {
    match fs::read(path) {
        Ok(b) => Ok(Some(b)),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(e) if e.kind() == io::ErrorKind::NotADirectory => Ok(None),
        Err(e) => Err(e).with_context(|| format!("reading {}", path.display())),
    }
}

/// Read a UTF-8 text file, mapping "not found" to `None`.
pub fn read_text_opt(path: &Path) -> Result<Option<String>> {
    match read_opt(path)? {
        None => Ok(None),
        Some(b) => String::from_utf8(b)
            .map(Some)
            .with_context(|| format!("{} is not valid UTF-8", path.display())),
    }
}

/// Recursively list regular files under `dir` as `/`-separated relative paths, sorted.
/// Missing directories yield an empty list. Hidden entries (`.git`, `.DS_Store`)
/// and `node_modules` are skipped.
pub fn walk_files(dir: &Path, max_files: usize) -> Result<Vec<String>> {
    let mut out = Vec::new();
    walk_inner(dir, "", 0, max_files, &mut out)?;
    out.sort();
    Ok(out)
}

fn walk_inner(
    dir: &Path,
    prefix: &str,
    depth: usize,
    max: usize,
    out: &mut Vec<String>,
) -> Result<()> {
    if depth > 8 {
        return Ok(());
    }
    let rd = match fs::read_dir(dir) {
        Ok(rd) => rd,
        Err(e)
            if e.kind() == io::ErrorKind::NotFound || e.kind() == io::ErrorKind::NotADirectory =>
        {
            return Ok(());
        }
        Err(e) => return Err(e).with_context(|| format!("listing {}", dir.display())),
    };
    let mut entries: Vec<_> = rd.collect::<io::Result<Vec<_>>>()?;
    entries.sort_by_key(|e| e.file_name());
    for e in entries {
        if out.len() >= max {
            return Ok(());
        }
        let name = e.file_name().to_string_lossy().into_owned();
        if name.starts_with('.') || name == "node_modules" {
            continue;
        }
        let rel = if prefix.is_empty() {
            name.clone()
        } else {
            format!("{prefix}/{name}")
        };
        // Follows symlinks: this is only used on the user's own tool config.
        let meta = fs::metadata(e.path());
        match meta {
            Ok(m) if m.is_dir() => walk_inner(&e.path(), &rel, depth + 1, max, out)?,
            Ok(m) if m.is_file() => out.push(rel),
            _ => {}
        }
    }
    Ok(())
}

/// Immediate subdirectories of `dir` (sorted names). Missing dir -> empty.
pub fn subdirs(dir: &Path) -> Result<Vec<String>> {
    let rd = match fs::read_dir(dir) {
        Ok(rd) => rd,
        Err(e)
            if e.kind() == io::ErrorKind::NotFound || e.kind() == io::ErrorKind::NotADirectory =>
        {
            return Ok(Vec::new());
        }
        Err(e) => return Err(e).with_context(|| format!("listing {}", dir.display())),
    };
    let mut out = Vec::new();
    for e in rd {
        let e = e?;
        if fs::metadata(e.path()).is_ok_and(|m| m.is_dir()) {
            out.push(e.file_name().to_string_lossy().into_owned());
        }
    }
    out.sort();
    Ok(out)
}

/// Join a `/`-separated relative path onto `base` using native separators.
pub fn join_rel(base: &Path, rel: &str) -> PathBuf {
    rel.split('/')
        .fold(base.to_path_buf(), |p, part| p.join(part))
}

impl PlanFs {
    pub fn new() -> Self {
        Self::default()
    }

    /// A planner for writes under `base` (the home directory or project root).
    /// Symbolic links below `base` are resolved so the plan names the file that
    /// really changes. With `confine`, links that lead outside `base` (or
    /// dangle) are refused: a cloned project must not redirect writes elsewhere.
    pub fn rooted(base: &Path, confine: bool) -> Self {
        PlanFs {
            staged: BTreeMap::new(),
            base: Some(base.to_path_buf()),
            confine,
        }
    }

    /// The path a write to `path` really lands on.
    pub fn real_path(&self, path: &Path) -> Result<PathBuf> {
        self.resolve(path, 0)
    }

    fn resolve(&self, path: &Path, depth: usize) -> Result<PathBuf> {
        if depth > 16 {
            bail!("too many levels of symbolic links at {}", path.display());
        }
        let total = path.components().count();
        // Components above the base (e.g. macOS's /var -> /private/var) are trusted.
        let start = match &self.base {
            Some(b) if path.starts_with(b) => b.components().count(),
            _ => total.saturating_sub(1),
        };
        let mut cur = PathBuf::new();
        for (i, c) in path.components().enumerate() {
            cur.push(c);
            if i < start {
                continue;
            }
            let Ok(meta) = fs::symlink_metadata(&cur) else {
                break;
            };
            if !meta.file_type().is_symlink() {
                continue;
            }
            let Ok(target) = fs::canonicalize(&cur) else {
                bail!(
                    "{} is a dangling symbolic link; kitbag won't write through it",
                    cur.display()
                );
            };
            if self.confine
                && let Some(base) = &self.base
            {
                let root = base.canonicalize().unwrap_or_else(|_| base.clone());
                if !target.starts_with(&root) {
                    bail!(
                        "{} is a symbolic link to {}, outside {}; refusing to write through it",
                        cur.display(),
                        target.display(),
                        base.display()
                    );
                }
            }
            let rest: PathBuf = path.components().skip(i + 1).collect();
            let next = if rest.as_os_str().is_empty() {
                target
            } else {
                target.join(rest)
            };
            return self.resolve(&next, depth + 1);
        }
        Ok(path.to_path_buf())
    }

    /// Current planned content of `path` (falls back to disk).
    pub fn read_bytes(&self, path: &Path) -> Result<Option<Vec<u8>>> {
        let real = self.real_path(path).unwrap_or_else(|_| path.to_path_buf());
        match self.staged.get(&real) {
            Some(s) => Ok(s.after.clone()),
            None => {
                if real.is_dir() {
                    bail!("{} is a directory, expected a file", path.display());
                }
                read_opt(&real)
            }
        }
    }

    pub fn read(&self, path: &Path) -> Result<Option<String>> {
        match self.read_bytes(path)? {
            None => Ok(None),
            Some(b) => String::from_utf8(b)
                .map(Some)
                .with_context(|| format!("{} is not valid UTF-8", path.display())),
        }
    }

    pub fn exists(&self, path: &Path) -> bool {
        let real = self.real_path(path).unwrap_or_else(|_| path.to_path_buf());
        match self.staged.get(&real) {
            Some(s) => s.after.is_some(),
            None => real.exists(),
        }
    }

    /// Plan `content` for `path`. Writing identical content is a no-op.
    pub fn write(&mut self, path: &Path, content: impl Into<Vec<u8>>) -> Result<()> {
        let content = content.into();
        let real = self.real_path(path)?;
        if let Some(s) = self.staged.get_mut(&real) {
            s.after = Some(content);
            return Ok(());
        }
        if real.is_dir() {
            bail!("{} is a directory, expected a file", path.display());
        }
        let before = read_opt(&real)?;
        self.staged.insert(
            real,
            Staged {
                before,
                after: Some(content),
            },
        );
        Ok(())
    }

    /// Planned changes, in path order, excluding no-op writes.
    pub fn changes(&self) -> Vec<FileChange> {
        self.staged
            .iter()
            .filter(|(_, s)| s.before != s.after)
            .map(|(p, s)| FileChange {
                path: p.clone(),
                before: s.before.clone(),
                after: s.after.clone(),
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn overlays_and_tracks_changes() {
        let dir = tempfile::tempdir().unwrap();
        let a = dir.path().join("a.txt");
        fs::write(&a, "old").unwrap();
        let mut p = PlanFs::new();
        assert_eq!(p.read(&a).unwrap().as_deref(), Some("old"));
        p.write(&a, "new").unwrap();
        assert_eq!(p.read(&a).unwrap().as_deref(), Some("new"));
        let b = dir.path().join("sub/b.txt");
        assert!(!p.exists(&b));
        p.write(&b, "x").unwrap();
        assert!(p.exists(&b));
        // Writing the original content back is a no-op change.
        let c = dir.path().join("c.txt");
        fs::write(&c, "same").unwrap();
        p.write(&c, "same").unwrap();
        let ch = p.changes();
        assert_eq!(ch.len(), 2);
        assert_eq!(ch[0].kind(), ChangeKind::Modify);
        assert_eq!(ch[1].kind(), ChangeKind::Create);
        // Disk untouched.
        assert_eq!(fs::read_to_string(&a).unwrap(), "old");
        assert!(!b.exists());
    }

    #[cfg(unix)]
    #[test]
    fn symlinks_are_resolved_and_confined() {
        use std::os::unix::fs::symlink;
        let dir = tempfile::tempdir().unwrap();
        let project = dir.path().join("project");
        let outside = dir.path().join("victim");
        fs::create_dir_all(&project).unwrap();
        fs::create_dir_all(&outside).unwrap();
        fs::write(outside.join("authorized_keys"), "key").unwrap();
        symlink(outside.join("authorized_keys"), project.join("CLAUDE.md")).unwrap();
        symlink(outside.join("newdir/evil.md"), project.join("AGENTS.md")).unwrap();
        symlink(&outside, project.join(".cursor")).unwrap();
        let mut p = PlanFs::rooted(&project, true);
        for f in ["CLAUDE.md", "AGENTS.md", ".cursor/mcp.json"] {
            let e = p.write(&project.join(f), "x").unwrap_err();
            assert!(e.to_string().contains("symbolic link"), "{f}: {e}");
        }
        // Unconfined (user scope): writes land on the link target.
        let mut p = PlanFs::rooted(&project, false);
        p.write(&project.join("CLAUDE.md"), "x").unwrap();
        assert_eq!(
            p.changes()[0].path,
            outside.join("authorized_keys").canonicalize().unwrap()
        );
        assert!(
            p.write(&project.join("AGENTS.md"), "x").is_err(),
            "dangling links are refused"
        );
    }

    #[test]
    fn walks_sorted_and_skips_hidden() {
        let dir = tempfile::tempdir().unwrap();
        fs::create_dir_all(dir.path().join("x/y")).unwrap();
        fs::create_dir_all(dir.path().join(".git")).unwrap();
        fs::write(dir.path().join("b.md"), "").unwrap();
        fs::write(dir.path().join("x/y/a.md"), "").unwrap();
        fs::write(dir.path().join(".git/HEAD"), "").unwrap();
        assert_eq!(
            walk_files(dir.path(), 100).unwrap(),
            vec!["b.md", "x/y/a.md"]
        );
        assert!(
            walk_files(&dir.path().join("missing"), 100)
                .unwrap()
                .is_empty()
        );
    }
}
