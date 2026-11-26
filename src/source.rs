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
/// `scheme://userinfo@` anywhere in a text. The userinfo runs to the last `@`
/// before the path, so a raw `@` inside a password is covered too.
static URL_USERINFO: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)\b([a-z][a-z0-9+.-]*://)([^\s/?#]*)@").expect("valid regex"));

/// `text` with the credentials of every URL in it removed:
/// `https://user:token@host/x` becomes `https://host/x`, and so does
/// `https://token@host/x`, since a token often stands in for the username.
/// An SSH URL keeps its username (`ssh://git@host/x`), which is not a secret,
/// but loses any password. Used for every source URL kitbag prints or stores.
pub fn strip_url_credentials(text: &str) -> String {
    URL_USERINFO
        .replace_all(text, |c: &regex::Captures| {
            let scheme = &c[1];
            let ssh = ["ssh://", "git+ssh://"]
                .iter()
                .any(|p| scheme.eq_ignore_ascii_case(p));
            match (ssh, c[2].split_once(':')) {
                (true, None) => c[0].to_string(),
                (true, Some((user, _))) => format!("{scheme}{user}@"),
                (false, _) => scheme.to_string(),
            }
        })
        .into_owned()
}

/// Is `url` fetched without encryption (`http://`, `git://`)? Anyone on the
/// network path could then swap the profile for their own.
fn cleartext(url: &str) -> bool {
    let u = url.to_ascii_lowercase();
    u.starts_with("http://") || u.starts_with("git://")
}

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
    /// Where the profile comes from, stable across applies: the local
    /// directory, or the git URL without credentials (and the sub-directory,
    /// if any), without the ref or commit.
    pub source: String,
    _tmp: Option<TempDir>,
}

fn safe_subdir(sub: &str) -> Result<PathBuf> {
    let p = Path::new(sub);
    if p.components().any(|c| !matches!(c, Component::Normal(_))) {
        bail!("subdirectory '{sub}' must be a relative path inside the repository");
    }
    Ok(p.to_path_buf())
}

/// Turn a command-line argument into a local profile directory, cloning git
/// sources. Cleartext `http://` and `git://` sources are refused unless
/// `allow_insecure`.
pub fn resolve(src: &str, subdir: Option<&str>, allow_insecure: bool) -> Result<Resolved> {
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
        let source = dir.canonicalize().unwrap_or_else(|_| dir.clone());
        return Ok(Resolved {
            origin: dir.display().to_string(),
            source: source.display().to_string(),
            dir,
            remote: false,
            _tmp: None,
        });
    }
    let Some(git) = parse_git_source(src) else {
        bail!(
            "'{}' is neither a local directory nor a git URL (try ./path, github:owner/repo or https://.../repo.git)",
            strip_url_credentials(src)
        );
    };
    // Credentials in the URL go to git, and nowhere else.
    let shown = strip_url_credentials(&git.url);
    if cleartext(&git.url) && !allow_insecure {
        bail!(
            "refusing to fetch {shown} without encryption: anyone on the network could replace the profile. Use an https:// or SSH URL, or pass --allow-insecure-source if you trust the network"
        );
    }
    let tmp = tempfile::Builder::new().prefix("kitbag-").tempdir()?;
    let out = match clone_command(&git, tmp.path(), allow_insecure).output() {
        Ok(o) => o,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            bail!("git is required to apply a profile from a URL, but it was not found on PATH")
        }
        Err(e) => return Err(e).context("running git"),
    };
    if !out.status.success() {
        bail!(
            "could not clone {shown}{}: {}",
            git.reference
                .as_ref()
                .map(|r| format!(" (ref {r})"))
                .unwrap_or_default(),
            strip_url_credentials(String::from_utf8_lossy(&out.stderr).trim())
        );
    }
    let sub = subdir.map(str::to_string).or(git.subdir.clone());
    let dir = match &sub {
        Some(s) => {
            let rel = safe_subdir(s)?;
            // A symlinked sub-directory could point anywhere on this machine.
            let mut cur = tmp.path().to_path_buf();
            for part in rel.components() {
                cur.push(part);
                if std::fs::symlink_metadata(&cur).is_ok_and(|m| m.file_type().is_symlink()) {
                    bail!("'{s}' is a symbolic link in the repository; refusing to follow it");
                }
            }
            cur
        }
        None => tmp.path().to_path_buf(),
    };
    if !dir.join(MANIFEST).exists() {
        bail!(
            "no {MANIFEST} found in {shown}{}",
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
    let mut source = shown.clone();
    if let Some(s) = &sub {
        source.push_str(&format!("//{s}"));
    }
    let mut origin = shown;
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
        source,
        remote: true,
        _tmp: Some(tmp),
    })
}

/// The `git clone` for `git` into `dest`. Unless `allow_insecure`, git may
/// not fetch over `http` or `git` even when an https server redirects there
/// or a `url.<base>.insteadOf` rewrite points there.
fn clone_command(git: &GitSource, dest: &Path, allow_insecure: bool) -> Command {
    let mut cmd = Command::new("git");
    if !allow_insecure {
        cmd.args([
            "-c",
            "protocol.http.allow=never",
            "-c",
            "protocol.git.allow=never",
        ]);
        // GIT_ALLOW_PROTOCOL, when set, overrides every protocol.*.allow
        // setting, `-c` included. Config from the environment
        // (GIT_CONFIG_COUNT, GIT_CONFIG_PARAMETERS) does not: `-c` wins.
        cmd.env_remove("GIT_ALLOW_PROTOCOL");
    }
    cmd.args(["clone", "--depth", "1", "--quiet", "--no-tags"]);
    if let Some(r) = &git.reference {
        cmd.args(["--branch", r]);
    }
    cmd.arg("--").arg(&git.url).arg(dest);
    cmd.env("GIT_TERMINAL_PROMPT", "0");
    cmd
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clones_refuse_cleartext_transports_unless_allowed() {
        let src = parse_git_source("https://example.com/o/r.git").unwrap();
        let args = |c: &Command| -> Vec<String> {
            c.get_args()
                .map(|a| a.to_string_lossy().into_owned())
                .collect()
        };
        let removes_allow_list = |c: &Command| {
            c.get_envs()
                .any(|(k, v)| k == "GIT_ALLOW_PROTOCOL" && v.is_none())
        };
        let strict = clone_command(&src, Path::new("dest"), false);
        let a = args(&strict);
        assert_eq!(
            a[..5],
            [
                "-c",
                "protocol.http.allow=never",
                "-c",
                "protocol.git.allow=never",
                "clone"
            ]
        );
        assert!(removes_allow_list(&strict));
        let loose = clone_command(&src, Path::new("dest"), true);
        assert!(!args(&loose).iter().any(|a| a.starts_with("protocol.")));
        assert!(!removes_allow_list(&loose));
        assert!(args(&loose).ends_with(&["--".into(), src.url.clone(), "dest".into()]));
    }

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
    fn url_credentials_are_stripped() {
        let s = strip_url_credentials;
        assert_eq!(s("https://bob:pw@h/x.git"), "https://h/x.git");
        assert_eq!(s("https://ghp_faketoken@h/x.git"), "https://h/x.git");
        assert_eq!(s("file://bob:pw@/srv/x"), "file:///srv/x");
        assert_eq!(
            s("https://bob:p@ss@h/x"),
            "https://h/x",
            "raw @ in a password"
        );
        assert_eq!(s("ssh://git@h/x.git"), "ssh://git@h/x.git");
        assert_eq!(s("git+ssh://git:pw@h/x.git"), "git+ssh://git@h/x.git");
        assert_eq!(
            s("fatal: unable to access 'https://bob:pw@h/x.git/': 403"),
            "fatal: unable to access 'https://h/x.git/': 403"
        );
        // Untouched: no userinfo, scp-style SSH, an @ only in the path.
        for same in [
            "https://h/x.git",
            "git@github.com:me/x.git",
            "https://h/@scope/pkg",
            "apply ./p -> codex (user scope)",
        ] {
            assert_eq!(s(same), same);
        }
    }

    #[test]
    fn cleartext_urls_are_recognised() {
        assert!(cleartext("http://h/x.git"));
        assert!(cleartext("HTTP://h/x.git"));
        assert!(cleartext("git://h/x.git"));
        for ok in ["https://h/x.git", "ssh://h/x", "git@h:x.git", "file:///x"] {
            assert!(!cleartext(ok), "{ok}");
        }
    }

    #[test]
    fn rejects_escaping_subdirs() {
        assert!(safe_subdir("../x").is_err());
        assert!(safe_subdir("/etc").is_err());
        assert!(safe_subdir("a/b").is_ok());
    }
}
