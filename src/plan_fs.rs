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
    /// Files to give the executable bit when written (Unix).
    executable: std::collections::BTreeSet<PathBuf>,
    /// With confinement: the canonical form of `base`. Every write must land under it.
    root: Option<PathBuf>,
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

/// The UTF-8 byte-order mark some Windows editors put at the start of a file.
pub const UTF8_BOM: &[u8] = b"\xEF\xBB\xBF";

/// Decode a text file: UTF-8 (a byte-order mark is dropped), or UTF-16 LE/BE
/// with a byte-order mark. Anything else is returned unchanged as the error.
pub fn decode_text(bytes: Vec<u8>) -> std::result::Result<String, Vec<u8>> {
    let utf16 = |be: bool| -> Option<String> {
        let body = &bytes[2..];
        if !body.len().is_multiple_of(2) {
            return None;
        }
        let units = body.as_chunks::<2>().0.iter().map(|c| {
            if be {
                u16::from_be_bytes([c[0], c[1]])
            } else {
                u16::from_le_bytes([c[0], c[1]])
            }
        });
        char::decode_utf16(units)
            .collect::<Result<String, _>>()
            .ok()
    };
    let decoded = match bytes.get(..2) {
        Some([0xFF, 0xFE]) => utf16(false),
        Some([0xFE, 0xFF]) => utf16(true),
        _ => None,
    };
    if let Some(text) = decoded {
        return Ok(text);
    }
    let start = if bytes.starts_with(UTF8_BOM) { 3 } else { 0 };
    match std::str::from_utf8(&bytes[start..]) {
        Ok(t) => Ok(t.to_string()),
        Err(_) => Err(bytes),
    }
}

/// Read a text file (see [`decode_text`]), mapping "not found" to `None`.
pub fn read_text_opt(path: &Path) -> Result<Option<String>> {
    match read_opt(path)? {
        None => Ok(None),
        Some(b) => decode_text(b).map(Some).map_err(|_| {
            anyhow::anyhow!(
                "{} is not valid UTF-8 (or UTF-16 with a byte-order mark)",
                path.display()
            )
        }),
    }
}

/// What [`walk_confined`] found.
#[derive(Debug, Default)]
pub struct ConfinedWalk {
    /// `/`-separated relative paths of regular files, sorted.
    pub files: Vec<String>,
    /// Entries that were left out, with the reason.
    pub skipped: Vec<String>,
    /// More than `max_files` files were found.
    pub truncated: bool,
}

/// List the files of a directory the user pointed a tool at (a skill), for capture.
///
/// `dir` itself may be a symbolic link (say into a git checkout); its target is
/// the root. Inside it, a link is followed only if its target stays inside the
/// root. Links that leave it (`etc -> /etc`, `up -> ..`), loops, dangling links
/// and unreadable entries are skipped and reported. Hidden entries and
/// `node_modules` are skipped silently.
pub fn walk_confined(dir: &Path, max_files: usize) -> Result<ConfinedWalk> {
    let root = fs::canonicalize(dir).with_context(|| format!("resolving {}", dir.display()))?;
    let mut out = ConfinedWalk::default();
    let mut ancestors = vec![root.clone()];
    confined_inner(&root, dir, "", &mut ancestors, max_files, &mut out)
        .with_context(|| format!("listing {}", dir.display()))?;
    out.files.sort();
    Ok(out)
}

fn confined_inner(
    root: &Path,
    dir: &Path,
    prefix: &str,
    ancestors: &mut Vec<PathBuf>,
    max: usize,
    out: &mut ConfinedWalk,
) -> io::Result<()> {
    let mut entries: Vec<_> = fs::read_dir(dir)?.collect::<io::Result<Vec<_>>>()?;
    entries.sort_by_key(|e| e.file_name());
    let mut subdirs = Vec::new();
    for e in entries {
        let name = e.file_name().to_string_lossy().into_owned();
        if name.starts_with('.') || name == "node_modules" {
            continue;
        }
        let rel = if prefix.is_empty() {
            name.clone()
        } else {
            format!("{prefix}/{name}")
        };
        let parent = ancestors.last().expect("root is always there").clone();
        let ft = match e.file_type() {
            Ok(ft) => ft,
            Err(err) => {
                out.skipped.push(format!("{rel}: {err}"));
                continue;
            }
        };
        let (real, meta) = if ft.is_symlink() {
            let target = match fs::canonicalize(e.path()) {
                Ok(t) => t,
                Err(_) => {
                    out.skipped.push(format!("{rel}: dangling symbolic link"));
                    continue;
                }
            };
            if !target.starts_with(root) {
                out.skipped.push(format!(
                    "{rel}: symbolic link to {}, outside the skill",
                    target.display()
                ));
                continue;
            }
            match fs::metadata(&target) {
                Ok(m) => (target, m),
                Err(err) => {
                    out.skipped.push(format!("{rel}: {err}"));
                    continue;
                }
            }
        } else {
            match e.metadata() {
                Ok(m) => (parent.join(&name), m),
                Err(err) => {
                    out.skipped.push(format!("{rel}: {err}"));
                    continue;
                }
            }
        };
        if meta.is_dir() {
            subdirs.push((e.path(), rel, real));
        } else if meta.is_file() {
            if out.files.len() >= max {
                out.truncated = true;
                return Ok(());
            }
            out.files.push(rel);
        }
    }
    // Files first, so a skill over the limit still keeps its top-level SKILL.md.
    for (path, rel, real) in subdirs {
        if ancestors.contains(&real) {
            out.skipped.push(format!("{rel}: symbolic link loop"));
            continue;
        }
        if ancestors.len() > 8 {
            out.skipped.push(format!("{rel}: nested too deeply"));
            continue;
        }
        ancestors.push(real);
        let res = confined_inner(root, &path, &rel, ancestors, max, out);
        ancestors.pop();
        if let Err(err) = res {
            out.skipped.push(format!("{rel}: {err}"));
        }
        if out.truncated {
            return Ok(());
        }
    }
    Ok(())
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

/// Is `path` (made of an existing, canonical prefix plus not-yet-existing
/// plain components) inside `root`? `..` and other non-normal components in
/// the part that doesn't exist yet count as outside.
fn within(path: &Path, root: &Path) -> bool {
    let Ok(rest) = path.strip_prefix(root) else {
        return false;
    };
    rest.components()
        .all(|c| matches!(c, std::path::Component::Normal(_)))
}

fn keep_bom(before: Option<&[u8]>, content: Vec<u8>) -> Vec<u8> {
    if before.is_some_and(|b| b.starts_with(UTF8_BOM))
        && !content.starts_with(UTF8_BOM)
        && std::str::from_utf8(&content).is_ok()
    {
        [UTF8_BOM, &content].concat()
    } else {
        content
    }
}

/// Does anything exist at `path`, a dangling symbolic link included?
pub fn present(path: &Path) -> bool {
    fs::symlink_metadata(path).is_ok()
}

/// Remove the symbolic link at `path` itself (never its target).
fn remove_link(path: &Path) -> Result<()> {
    // On Windows a link to a directory is removed like a directory.
    fs::remove_file(path)
        .or_else(|e| fs::remove_dir(path).map_err(|_| e))
        .with_context(|| format!("removing the symbolic link {}", path.display()))
}

/// Write `content` to `rel` (`/`-separated) inside `dir`, a directory kitbag
/// owns (a profile). Symbolic links on the way, the file included, are
/// removed and replaced by a real directory or file, so the write never lands
/// on their target. `dir` itself may be a link (say into a dotfiles repo).
pub fn write_owned_file(dir: &Path, rel: &str, content: &[u8]) -> Result<()> {
    let parts: Vec<&str> = rel.split('/').collect();
    let mut cur = dir.to_path_buf();
    for (i, part) in parts.iter().enumerate() {
        cur.push(part);
        let last = i + 1 == parts.len();
        match fs::symlink_metadata(&cur) {
            Ok(m) if m.file_type().is_symlink() => remove_link(&cur)?,
            Ok(m) if !last && !m.is_dir() => {
                bail!("{} is not a directory", cur.display())
            }
            Ok(m) if last && m.is_dir() => {
                bail!("{} is a directory, expected a file", cur.display())
            }
            _ => {}
        }
        if !last && !cur.is_dir() {
            fs::create_dir(&cur).with_context(|| format!("creating {}", cur.display()))?;
        }
    }
    crate::txn::write_atomic(&cur, content, None)
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
    ///
    /// Confinement compares canonical paths, so it also holds when `base` is
    /// reached through a symbolic link itself (`~/src -> /data/src`).
    pub fn rooted(base: &Path, confine: bool) -> Self {
        PlanFs {
            staged: BTreeMap::new(),
            base: Some(base.to_path_buf()),
            executable: Default::default(),
            root: confine.then(|| base.canonicalize().unwrap_or_else(|_| base.to_path_buf())),
        }
    }

    /// The path a write to `path` really lands on.
    pub fn real_path(&self, path: &Path) -> Result<PathBuf> {
        let Some(root) = &self.root else {
            return self.resolve(path, 0);
        };
        // Spell the path from the canonical root, so links are checked from there.
        let path = match self.base.as_ref().and_then(|b| path.strip_prefix(b).ok()) {
            Some(rest) => root.join(rest),
            None => path.to_path_buf(),
        };
        let real = self.resolve(&path, 0)?;
        if !within(&real, root) {
            bail!(
                "{} resolves to {}, outside {}; refusing to write there",
                path.display(),
                real.display(),
                root.display()
            );
        }
        // Name it as the user spelled the project root (the same directory).
        match (&self.base, real.strip_prefix(root)) {
            (Some(base), Ok(rest)) => Ok(base.join(rest)),
            _ => Ok(real),
        }
    }

    fn resolve(&self, path: &Path, depth: usize) -> Result<PathBuf> {
        if depth > 16 {
            bail!("too many levels of symbolic links at {}", path.display());
        }
        let total = path.components().count();
        // Components above the base (e.g. macOS's /var -> /private/var) are trusted.
        let start = match self.root.as_ref().or(self.base.as_ref()) {
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
            if let Some(root) = &self.root
                && !target.starts_with(root)
            {
                bail!(
                    "{} is a symbolic link to {}, outside {}; refusing to write through it",
                    cur.display(),
                    target.display(),
                    root.display()
                );
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

    /// Current planned content of `path` as text. A UTF-8 byte-order mark is
    /// dropped here and put back by [`PlanFs::write`].
    pub fn read(&self, path: &Path) -> Result<Option<String>> {
        match self.read_bytes(path)? {
            None => Ok(None),
            Some(mut b) => {
                if b.starts_with(UTF8_BOM) {
                    b.drain(..UTF8_BOM.len());
                }
                String::from_utf8(b)
                    .map(Some)
                    .with_context(|| format!("{} is not valid UTF-8", path.display()))
            }
        }
    }

    pub fn exists(&self, path: &Path) -> bool {
        let real = self.real_path(path).unwrap_or_else(|_| path.to_path_buf());
        match self.staged.get(&real) {
            Some(s) => s.after.is_some(),
            None => real.exists(),
        }
    }

    /// Plan `content` for `path`. Writing identical content is a no-op. Text
    /// replacing a file that starts with a UTF-8 byte-order mark keeps the mark.
    pub fn write(&mut self, path: &Path, content: impl Into<Vec<u8>>) -> Result<()> {
        let content = content.into();
        let real = self.real_path(path)?;
        if let Some(s) = self.staged.get_mut(&real) {
            s.after = Some(keep_bom(s.before.as_deref(), content));
            return Ok(());
        }
        if real.is_dir() {
            bail!("{} is a directory, expected a file", path.display());
        }
        let before = read_opt(&real)?;
        let content = keep_bom(before.as_deref(), content);
        self.staged.insert(
            real,
            Staged {
                before,
                after: Some(content),
            },
        );
        Ok(())
    }

    /// Mark a file written by this plan as executable (Unix; ignored elsewhere).
    pub fn mark_executable(&mut self, path: &Path) -> Result<()> {
        let real = self.real_path(path)?;
        self.executable.insert(real);
        Ok(())
    }

    /// Whether `path` is already planned to be written with the executable bit.
    pub fn is_executable_planned(&self, path: &Path) -> bool {
        self.real_path(path)
            .is_ok_and(|real| self.executable.contains(&real))
    }

    /// Files planned to be written with the executable bit.
    pub fn executables(&self) -> std::collections::BTreeSet<PathBuf> {
        self.executable.clone()
    }

    /// Whether `path` is planned as executable but the file on disk isn't
    /// (Unix; always false elsewhere). Such a file is a change even when its
    /// content is the same.
    fn loses_executable(&self, path: &Path) -> bool {
        cfg!(unix)
            && self.executable.contains(path)
            && !crate::adapters::common::skill_file_executable(path)
    }

    /// Planned changes, in path order, excluding no-op writes. A write of the
    /// same content still counts when it restores a lost executable bit.
    pub fn changes(&self) -> Vec<FileChange> {
        self.staged
            .iter()
            .filter(|(p, s)| {
                s.before != s.after
                    || (s.before.is_some() && s.after.is_some() && self.loses_executable(p))
            })
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
    fn confined_walk_keeps_top_level_files_when_truncating() {
        let dir = tempfile::tempdir().unwrap();
        fs::create_dir_all(dir.path().join("A")).unwrap();
        for i in 0..5 {
            fs::write(dir.path().join(format!("A/{i}.md")), "").unwrap();
        }
        fs::write(dir.path().join("SKILL.md"), "").unwrap();
        let w = walk_confined(dir.path(), 3).unwrap();
        assert_eq!(w.files, vec!["A/0.md", "A/1.md", "SKILL.md"]);
        assert!(w.truncated);
    }

    #[test]
    fn decodes_text() {
        assert_eq!(decode_text(b"\xEF\xBB\xBFhi".to_vec()).unwrap(), "hi");
        assert_eq!(
            decode_text(vec![0xFF, 0xFE, b'h', 0, b'i', 0]).unwrap(),
            "hi"
        );
        assert_eq!(
            decode_text(vec![0xFE, 0xFF, 0, b'h', 0, b'i']).unwrap(),
            "hi"
        );
        assert!(decode_text(vec![0xFF, 0xFE, b'h']).is_err(), "odd length");
        assert!(decode_text(b"caf\xE9".to_vec()).is_err());
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
