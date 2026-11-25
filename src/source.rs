//! Where a profile comes from: a local directory or a git repository.

use std::path::{Component, Path, PathBuf};
use std::process::Command;
use std::sync::LazyLock;

use anyhow::{Context, Result, bail};
use regex::Regex;
use tempfile::TempDir;

use crate::profile_io::MANIFEST;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GitSource {
    pub url: String,
    pub reference: Option<String>,
    pub subdir: Option<String>,
}

static SHORTHAND: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^(github|gh|gitlab|bitbucket):([\w.-]+)/([\w.-]+?)(?:\.git)?((?:/[^/]+)*)/?$")
        .expect("valid regex")
});
static DOUBLE_SLASH: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^(.+?://[^/]*/.+?)//(.+)$").expect("valid regex"));
static TREE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"^(https://(?:github\.com|gitlab\.com)/[\w.-]+/[\w.-]+)/(?:-/)?tree/([^/]+)(?:/(.+))?$",
    )
    .expect("valid regex")
});
static HOSTED: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^https://(github\.com|gitlab\.com|bitbucket\.org)/[\w.-]+/[\w.-]+/?$")
        .expect("valid regex")
});

/// Recognise git sources:
///
/// * `github:owner/repo[/sub/dir][#ref]` (also `gh:`, `gitlab:`, `bitbucket:`)
/// * `https://github.com/owner/repo[.git][#ref]`
/// * `https://github.com/owner/repo/tree/<ref>/<sub/dir>`
/// * `git@host:owner/repo.git[#ref]`, `ssh://...`, `git+https://...`, `file://...`
///
/// Append `//sub/dir` to a URL to select a profile inside the repository.
pub fn parse_git_source(src: &str) -> Option<GitSource> {
    let mut s = src.trim().to_string();
    let mut reference = None;
    if let Some(i) = s.rfind('#')
        && i > 0
    {
        let r = s[i + 1..].to_string();
        s.truncate(i);
        if !r.is_empty() {
            reference = Some(r);
        }
    }
    if let Some(c) = SHORTHAND.captures(&s) {
        let host = match &c[1] {
            "gitlab" => "gitlab.com",
            "bitbucket" => "bitbucket.org",
            _ => "github.com",
        };
        let sub = c
            .get(4)
            .map(|m| m.as_str().trim_start_matches('/').to_string())
            .filter(|x| !x.is_empty());
        return Some(GitSource {
            url: format!("https://{host}/{}/{}.git", &c[2], &c[3]),
            reference,
            subdir: sub,
        });
    }
    let mut subdir = None;
    if let Some(c) = DOUBLE_SLASH.captures(&s) {
        subdir = Some(c[2].trim_end_matches('/').to_string());
        s = c[1].to_string();
    }
    if let Some(c) = TREE.captures(&s) {
        return Some(GitSource {
            url: format!("{}.git", &c[1]),
            reference: reference.or_else(|| Some(c[2].to_string())),
            subdir: subdir.or_else(|| {
                c.get(3)
                    .map(|m| m.as_str().trim_end_matches('/').to_string())
            }),
        });
    }
    let is_git = (s.starts_with("git@") && s.contains(':'))
        || ["ssh://", "git://", "git+https://", "git+ssh://", "file://"]
            .iter()
            .any(|p| s.starts_with(p))
        || ((s.starts_with("https://") || s.starts_with("http://"))
            && s.trim_end_matches('/').ends_with(".git"))
        || HOSTED.is_match(&s);
    if !is_git {
        return None;
    }
    Some(GitSource {
        url: s.strip_prefix("git+").unwrap_or(&s).to_string(),
        reference,
        subdir,
    })
}

/// A profile directory ready to load. Temporary clones are removed on drop.
pub struct Resolved {
    pub dir: PathBuf,
    /// Human-readable origin for messages and the transaction log.
    pub origin: String,
    /// True when the profile came from a git URL rather than a local directory.
    pub remote: bool,
    _tmp: Option<TempDir>,
}

fn safe_subdir(sub: &str) -> Result<PathBuf> {
    let p = Path::new(sub);
    if p.components().any(|c| !matches!(c, Component::Normal(_))) {
        bail!("subdirectory '{sub}' must be a relative path inside the repository");
    }
    Ok(p.to_path_buf())
}

/// Turn a command-line argument into a local profile directory, cloning git sources.
pub fn resolve(src: &str, subdir: Option<&str>) -> Result<Resolved> {
    let local = Path::new(src);
    if local.exists() {
        let dir = if local.is_dir() {
            local.to_path_buf()
        } else {
            local.parent().map(Path::to_path_buf).unwrap_or_default()
        };
        let dir = match subdir {
            Some(s) => dir.join(safe_subdir(s)?),
            None => dir,
        };
        if !dir.is_dir() {
            bail!("{} does not exist", dir.display());
        }
        let dir = std::path::absolute(&dir)?;
        return Ok(Resolved {
            origin: dir.display().to_string(),
            dir,
            remote: false,
            _tmp: None,
        });
    }
    let Some(git) = parse_git_source(src) else {
        bail!(
            "'{src}' is neither a local directory nor a git URL (try ./path, github:owner/repo or https://.../repo.git)"
        );
    };
    let tmp = tempfile::Builder::new().prefix("kitbag-").tempdir()?;
    let mut cmd = Command::new("git");
    cmd.args(["clone", "--depth", "1", "--quiet", "--no-tags"]);
    if let Some(r) = &git.reference {
        cmd.args(["--branch", r]);
    }
    cmd.arg("--").arg(&git.url).arg(tmp.path());
    cmd.env("GIT_TERMINAL_PROMPT", "0");
    let out = match cmd.output() {
        Ok(o) => o,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            bail!("git is required to apply a profile from a URL, but it was not found on PATH")
        }
        Err(e) => return Err(e).context("running git"),
    };
    if !out.status.success() {
        bail!(
            "could not clone {}{}: {}",
            git.url,
            git.reference
                .as_ref()
                .map(|r| format!(" (ref {r})"))
                .unwrap_or_default(),
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    let sub = subdir.map(str::to_string).or(git.subdir.clone());
    let dir = match &sub {
        Some(s) => tmp.path().join(safe_subdir(s)?),
        None => tmp.path().to_path_buf(),
    };
    if !dir.join(MANIFEST).exists() {
        bail!(
            "no {MANIFEST} found in {}{}",
            git.url,
            sub.as_ref().map(|s| format!(" ({s})")).unwrap_or_default()
        );
    }
    let commit = Command::new("git")
        .arg("-C")
        .arg(tmp.path())
        .args(["rev-parse", "--short", "HEAD"])
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .unwrap_or_default();
    let mut origin = git.url.clone();
    if let Some(r) = &git.reference {
        origin.push_str(&format!("#{r}"));
    }
    if let Some(s) = &sub {
        origin.push_str(&format!("//{s}"));
    }
    if !commit.is_empty() {
        origin.push_str(&format!(" @ {commit}"));
    }
    Ok(Resolved {
        dir,
        origin,
        remote: true,
        _tmp: Some(tmp),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn g(url: &str, r: Option<&str>, s: Option<&str>) -> Option<GitSource> {
        Some(GitSource {
            url: url.into(),
            reference: r.map(Into::into),
            subdir: s.map(Into::into),
        })
    }

    #[test]
    fn parses_sources() {
        assert_eq!(
            parse_git_source("github:me/dots"),
            g("https://github.com/me/dots.git", None, None)
        );
        assert_eq!(
            parse_git_source("gh:me/dots/agents/work#main"),
            g(
                "https://github.com/me/dots.git",
                Some("main"),
                Some("agents/work")
            )
        );
        assert_eq!(
            parse_git_source("https://github.com/me/dots/tree/v1/profile"),
            g(
                "https://github.com/me/dots.git",
                Some("v1"),
                Some("profile")
            )
        );
        assert_eq!(
            parse_git_source("https://github.com/me/dots"),
            g("https://github.com/me/dots", None, None)
        );
        assert_eq!(
            parse_git_source("https://git.example.com/me/dots.git//sub#dev"),
            g(
                "https://git.example.com/me/dots.git",
                Some("dev"),
                Some("sub")
            )
        );
        assert_eq!(
            parse_git_source("git@github.com:me/dots.git"),
            g("git@github.com:me/dots.git", None, None)
        );
        assert_eq!(
            parse_git_source("git+https://h/x.git"),
            g("https://h/x.git", None, None)
        );
        assert_eq!(parse_git_source("./my-profile"), None);
        assert_eq!(parse_git_source("https://example.com/page"), None);
    }

    #[test]
    fn rejects_escaping_subdirs() {
        assert!(safe_subdir("../x").is_err());
        assert!(safe_subdir("/etc").is_err());
        assert!(safe_subdir("a/b").is_ok());
    }
}
