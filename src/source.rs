//! Where a profile comes from: a local directory or a git repository.

use std::path::{Component, Path, PathBuf};
use std::process::Command;
use std::sync::LazyLock;

use anyhow::{Result, bail};
use regex::Regex;
use tempfile::TempDir;

use crate::profile_io::{MANIFEST, PLUGIN_RULES};

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

/// Where the path of a git source starts: after `scheme://host`, after
/// `host:` of an scp-style `user@host:path`, or after `github:`. Any `@`
/// before it is userinfo (`https://user:token@host`, `git@host`), never a ref.
fn path_start(s: &str) -> usize {
    if let Some(i) = s.find("://") {
        let rest = i + 3;
        return s[rest..].find('/').map_or(s.len(), |j| rest + j);
    }
    s.find(':').map_or(0, |i| i + 1)
}

/// Split a trailing `@<ref>` off a source. Only an `@` in the path counts,
/// and not one that starts a path segment (`/@scope/pkg`). A `//sub/dir`
/// after the ref stays with the source, since a ref can't hold `//`.
fn split_at_ref(s: &str) -> (String, Option<String>) {
    let start = path_start(s);
    let at = s[start..]
        .match_indices('@')
        .map(|(i, _)| start + i)
        .find(|&i| i > start && !s[..i].ends_with('/'));
    let Some(at) = at else {
        return (s.to_string(), None);
    };
    let after = &s[at + 1..];
    let (r, sub) = match after.find("//") {
        Some(j) => (&after[..j], &after[j..]),
        None => (after, ""),
    };
    let rest = format!("{}{sub}", &s[..at]);
    (rest, Some(r.to_string()).filter(|r| !r.is_empty()))
}

/// Refuse a ref git would read as an option or a refspec, or never accept
/// as a branch, tag or commit name.
fn check_ref(r: &str) -> Result<()> {
    let bad = r.starts_with('-')
        || r.contains("..")
        || r.contains("@{")
        || r.chars()
            .any(|c| c.is_whitespace() || c.is_control() || ":~^?*[\\".contains(c));
    if bad {
        bail!(
            "'{}' is not a branch, tag or commit SHA git can fetch",
            strip_url_credentials(r)
        );
    }
    Ok(())
}

/// Recognise git sources:
///
/// * `github:owner/repo[/sub/dir][@ref|#ref]` (also `gh:`, `gitlab:`, `bitbucket:`)
/// * `https://github.com/owner/repo[.git][@ref|#ref]`
/// * `https://github.com/owner/repo/tree/<ref>/<sub/dir>`
/// * `git@host:owner/repo.git[@ref|#ref]`, `ssh://...`, `git+https://...`, `file://...`
///
/// Append `//sub/dir` to a URL to select a profile inside the repository.
/// A ref is a branch, a tag or a commit SHA; `@ref` and `#ref` mean the same,
/// and giving both is an error. `Ok(None)`: not a git source.
pub fn parse_git_source(src: &str) -> Result<Option<GitSource>> {
    let mut s = src.trim().to_string();
    let mut hash_ref = None;
    if let Some(i) = s.rfind('#')
        && i > 0
    {
        let r = s[i + 1..].to_string();
        s.truncate(i);
        if !r.is_empty() {
            hash_ref = Some(r);
        }
    }
    let (s, at_ref) = split_at_ref(&s);
    let Some(mut git) = parse_url(s) else {
        return Ok(None);
    };
    let given = match (at_ref, hash_ref) {
        (Some(a), Some(h)) => bail!(
            "'{}' names two refs, @{} and #{}; give one",
            strip_url_credentials(src.trim()),
            strip_url_credentials(&a),
            strip_url_credentials(&h)
        ),
        (a, h) => a.or(h),
    };
    // A ref given with @ or # wins over the one in a /tree/<ref>/ URL.
    if given.is_some() {
        git.reference = given;
    }
    if let Some(r) = &git.reference {
        check_ref(r)?;
    }
    Ok(Some(git))
}

/// [`parse_git_source`] once the ref is split off.
fn parse_url(mut s: String) -> Option<GitSource> {
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
            reference: None,
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
            reference: Some(c[2].to_string()),
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
        reference: None,
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
    /// The full SHA of the commit a git source was checked out at; `None`
    /// for a local directory.
    pub commit: Option<String>,
    /// The name a plugin (a directory without a `kitbag.toml`) gets when its
    /// manifest names none: the directory's name, or for a git source the
    /// sub-directory's or repository's, never the temporary clone's.
    pub name_hint: String,
    _tmp: Option<TempDir>,
}

impl Resolved {
    /// The profile directory is a copy of a git source made elsewhere (by
    /// `--remote`): it is untrusted like a clone, and its source and commit are
    /// the git ones. `source` must be a git source as [`Resolved::source`]
    /// has it, and `commit` a full hex SHA.
    pub fn treat_as_git_copy(&mut self, source: &str, commit: &str) -> Result<()> {
        if !matches!(commit.len(), 40 | 64) || !commit.chars().all(|c| c.is_ascii_hexdigit()) {
            bail!("--copy-of-commit must be a full commit SHA");
        }
        let plain = parse_git_source(source)?.is_some_and(|g| g.reference.is_none())
            && strip_url_credentials(source) == source;
        if !plain {
            bail!("--copy-of-git must be a git URL without a ref or credentials");
        }
        self.remote = true;
        self.source = source.to_string();
        self.commit = Some(commit.to_string());
        self.origin = format!("{source} @ {}", short_commit(commit));
        Ok(())
    }
}

/// How [`resolve`] may fetch a git source.
#[derive(Debug, Clone, Copy, Default)]
pub struct FetchOptions {
    /// Accept cleartext `http://` and `git://` sources.
    pub allow_insecure: bool,
    /// Refuse a git source that names no branch, tag or commit.
    pub require_pin: bool,
}

/// The short form of a commit SHA shown next to a source.
pub fn short_commit(sha: &str) -> &str {
    &sha[..sha.len().min(7)]
}

/// The last part of a git URL's path, without `.git`: `repo` for
/// `https://github.com/o/repo.git` or `git@host:o/repo`.
fn repo_name(url: &str) -> String {
    let path = url.trim_end_matches('/');
    let last = path.rsplit(['/', ':']).next().unwrap_or(path);
    last.strip_suffix(".git").unwrap_or(last).to_string()
}

fn safe_subdir(sub: &str) -> Result<PathBuf> {
    let p = Path::new(sub);
    if p.components().any(|c| !matches!(c, Component::Normal(_))) {
        bail!("subdirectory '{sub}' must be a relative path inside the repository");
    }
    Ok(p.to_path_buf())
}

/// Turn a command-line argument into a local profile directory, fetching git
/// sources at the branch, tag or commit they name (the default branch when
/// they name none). Cleartext `http://` and `git://` sources are refused
/// unless `opts.allow_insecure`, and unpinned git sources when
/// `opts.require_pin`.
pub fn resolve(src: &str, subdir: Option<&str>, opts: FetchOptions) -> Result<Resolved> {
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
        let name_hint = source
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        return Ok(Resolved {
            origin: dir.display().to_string(),
            source: source.display().to_string(),
            commit: None,
            name_hint,
            dir,
            remote: false,
            _tmp: None,
        });
    }
    let Some(git) = parse_git_source(src)? else {
        bail!(
            "'{}' is neither a local directory nor a git URL (try ./path, github:owner/repo or https://.../repo.git)",
            strip_url_credentials(src)
        );
    };
    // Credentials in the URL go to git, and nowhere else.
    let shown = strip_url_credentials(&git.url);
    if cleartext(&git.url) && !opts.allow_insecure {
        bail!(
            "refusing to fetch {shown} without encryption: anyone on the network could replace the profile. Use an https:// or SSH URL, or pass --allow-insecure-source if you trust the network"
        );
    }
    if opts.require_pin && git.reference.is_none() {
        bail!(
            "{shown} is not pinned (--require-pin): add @<sha|tag|branch> (or #<ref>) to the source, such as {}@v1.0",
            strip_url_credentials(src.trim())
        );
    }
    let tmp = tempfile::Builder::new().prefix("kitbag-").tempdir()?;
    let commit = fetch(&git, tmp.path(), opts.allow_insecure).map_err(|e| {
        // Running git at all failed (not installed): not the source's fault.
        if e.downcast_ref::<std::io::Error>().is_some() {
            return e;
        }
        anyhow::anyhow!(
            "could not clone {shown}{}: {}",
            git.reference
                .as_ref()
                .map(|r| format!(" (ref {r})"))
                .unwrap_or_default(),
            strip_url_credentials(&format!("{e:#}"))
        )
    })?;
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
    if !dir.join(MANIFEST).exists() && !dir.join(PLUGIN_RULES).is_dir() {
        bail!(
            "no {MANIFEST} (a kitbag profile) or {PLUGIN_RULES}/ directory (a plugin) found in {shown}{}",
            sub.as_ref().map(|s| format!(" ({s})")).unwrap_or_default()
        );
    }
    let name_hint = match &sub {
        Some(s) => Path::new(s)
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default(),
        None => repo_name(&shown),
    };
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
    origin.push_str(&format!(" @ {}", short_commit(&commit)));
    Ok(Resolved {
        dir,
        origin,
        source,
        commit: Some(commit),
        name_hint,
        remote: true,
        _tmp: Some(tmp),
    })
}

/// A `git` command that never prompts and, unless `allow_insecure`, may not
/// fetch over `http` or `git` even when an https server redirects there or a
/// `url.<base>.insteadOf` rewrite points there.
fn git_command(allow_insecure: bool) -> Command {
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
    cmd.env("GIT_TERMINAL_PROMPT", "0");
    cmd
}

/// The shallow `git fetch` of `git`'s ref (its default branch when it names
/// none) into the repository at `dest`. A commit SHA works too, where the
/// server lets clients fetch one (GitHub, GitLab and local repositories do).
fn fetch_command(git: &GitSource, dest: &Path, allow_insecure: bool) -> Command {
    let mut cmd = git_command(allow_insecure);
    cmd.arg("-C").arg(dest);
    cmd.args(["fetch", "--depth", "1", "--quiet", "--no-tags", "--"]);
    cmd.arg(&git.url)
        .arg(git.reference.as_deref().unwrap_or("HEAD"));
    cmd
}

/// Could `r` be an abbreviated (or, for a server that won't serve one by
/// SHA, a full) commit SHA?
fn looks_like_sha(r: &str) -> bool {
    (4..=64).contains(&r.len()) && r.bytes().all(|b| b.is_ascii_hexdigit())
}

/// Run `cmd`; its stdout, or an error with its stderr.
fn run(mut cmd: Command) -> Result<String> {
    let out = match cmd.output() {
        Ok(o) => o,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return Err(anyhow::Error::new(e).context(
                "git is required to apply a profile from a URL, but it was not found on PATH",
            ));
        }
        Err(e) => return Err(anyhow::Error::new(e).context("running git")),
    };
    if !out.status.success() {
        bail!("{}", String::from_utf8_lossy(&out.stderr).trim());
    }
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

/// Fetch `git` into the empty directory `dest`, check it out, and return the
/// full SHA of the commit checked out.
fn fetch(git: &GitSource, dest: &Path, allow_insecure: bool) -> Result<String> {
    let in_dest = |args: &[&str]| {
        let mut c = git_command(allow_insecure);
        c.arg("-C").arg(dest).args(args);
        c
    };
    run(in_dest(&["init", "--quiet"]))?;
    let wanted = match run(fetch_command(git, dest, allow_insecure)) {
        Ok(_) => "FETCH_HEAD".to_string(),
        // `git fetch` takes a full SHA only: for a short one, fetch the
        // branches and tags and look it up among their history.
        Err(e) => match git.reference.as_deref().filter(|r| looks_like_sha(r)) {
            Some(r) => {
                let mut all = in_dest(&["fetch", "--quiet", "--no-tags", "--"]);
                all.arg(&git.url).args([
                    "+refs/heads/*:refs/remotes/kitbag/*",
                    "+refs/tags/*:refs/tags/*",
                ]);
                run(all).map_err(|_| e)?;
                r.to_string()
            }
            None => return Err(e),
        },
    };
    let commit = run(in_dest(&[
        "rev-parse",
        "--verify",
        "--quiet",
        "--end-of-options",
        &format!("{wanted}^{{commit}}"),
    ]))
    .map_err(|_| anyhow::anyhow!("no commit {wanted} in the repository"))?;
    run(in_dest(&[
        "-c",
        "advice.detachedHead=false",
        "checkout",
        "--quiet",
        "--detach",
        &commit,
    ]))?;
    Ok(commit)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fetches_refuse_cleartext_transports_unless_allowed() {
        let src = parse_git_source("https://example.com/o/r.git")
            .unwrap()
            .unwrap();
        let args = |c: &Command| -> Vec<String> {
            c.get_args()
                .map(|a| a.to_string_lossy().into_owned())
                .collect()
        };
        let removes_allow_list = |c: &Command| {
            c.get_envs()
                .any(|(k, v)| k == "GIT_ALLOW_PROTOCOL" && v.is_none())
        };
        let no_prompt = |c: &Command| {
            c.get_envs()
                .any(|(k, v)| k == "GIT_TERMINAL_PROMPT" && v == Some("0".as_ref()))
        };
        let strict = fetch_command(&src, Path::new("dest"), false);
        let a = args(&strict);
        assert_eq!(
            a[..7],
            [
                "-c",
                "protocol.http.allow=never",
                "-c",
                "protocol.git.allow=never",
                "-C",
                "dest",
                "fetch"
            ]
        );
        assert!(removes_allow_list(&strict));
        assert!(no_prompt(&strict));
        let loose = fetch_command(&src, Path::new("dest"), true);
        assert!(!args(&loose).iter().any(|a| a.starts_with("protocol.")));
        assert!(!removes_allow_list(&loose));
        assert!(no_prompt(&loose));
        // Unpinned: the default branch.
        assert!(args(&loose).ends_with(&["--".into(), src.url.clone(), "HEAD".into()]));
        let pinned = parse_git_source("https://example.com/o/r.git@0123abcd")
            .unwrap()
            .unwrap();
        assert!(
            args(&fetch_command(&pinned, Path::new("dest"), false)).ends_with(&[
                "--".into(),
                pinned.url.clone(),
                "0123abcd".into()
            ])
        );
    }

    fn g(url: &str, r: Option<&str>, s: Option<&str>) -> Option<GitSource> {
        Some(GitSource {
            url: url.into(),
            reference: r.map(Into::into),
            subdir: s.map(Into::into),
        })
    }

    fn parse(src: &str) -> Option<GitSource> {
        parse_git_source(src).unwrap()
    }

    #[test]
    fn parses_sources() {
        assert_eq!(
            parse("github:me/dots"),
            g("https://github.com/me/dots.git", None, None)
        );
        assert_eq!(
            parse("gh:me/dots/agents/work#main"),
            g(
                "https://github.com/me/dots.git",
                Some("main"),
                Some("agents/work")
            )
        );
        assert_eq!(
            parse("https://github.com/me/dots/tree/v1/profile"),
            g(
                "https://github.com/me/dots.git",
                Some("v1"),
                Some("profile")
            )
        );
        assert_eq!(
            parse("https://github.com/me/dots"),
            g("https://github.com/me/dots", None, None)
        );
        assert_eq!(
            parse("https://git.example.com/me/dots.git//sub#dev"),
            g(
                "https://git.example.com/me/dots.git",
                Some("dev"),
                Some("sub")
            )
        );
        assert_eq!(
            parse("git@github.com:me/dots.git"),
            g("git@github.com:me/dots.git", None, None)
        );
        assert_eq!(
            parse("git+https://h/x.git"),
            g("https://h/x.git", None, None)
        );
        assert_eq!(parse("./my-profile"), None);
        assert_eq!(parse("https://example.com/page"), None);
    }

    const SHA: &str = "0123456789abcdef0123456789abcdef01234567";

    #[test]
    fn parses_at_refs() {
        let gh = "https://github.com/me/dots.git";
        assert_eq!(
            parse(&format!("github:me/dots@{SHA}")),
            g(gh, Some(SHA), None)
        );
        assert_eq!(parse("github:me/dots@v1.2.0"), g(gh, Some("v1.2.0"), None));
        assert_eq!(parse("github:me/dots#main"), g(gh, Some("main"), None));
        assert_eq!(
            parse("github:me/dots/sub#main"),
            g(gh, Some("main"), Some("sub"))
        );
        assert_eq!(
            parse("gh:me/dots/agents/work@feature/x"),
            g(gh, Some("feature/x"), Some("agents/work"))
        );
        assert_eq!(parse("github:me/dots@"), g(gh, None, None));
        assert_eq!(
            parse("https://git.example.com/me/dots.git//sub@abc1234"),
            g(
                "https://git.example.com/me/dots.git",
                Some("abc1234"),
                Some("sub")
            )
        );
        assert_eq!(
            parse("https://git.example.com/me/dots.git@v2//sub"),
            g(
                "https://git.example.com/me/dots.git",
                Some("v2"),
                Some("sub")
            )
        );
        assert_eq!(
            parse("git@github.com:me/dots.git@v1"),
            g("git@github.com:me/dots.git", Some("v1"), None)
        );
        assert_eq!(
            parse("ssh://git@h/me/dots.git@v1"),
            g("ssh://git@h/me/dots.git", Some("v1"), None)
        );
        assert_eq!(
            parse("https://github.com/me/dots/tree/main/p@v3"),
            g(gh, Some("v3"), Some("p")),
            "an explicit ref wins over the /tree/ one"
        );
    }

    #[test]
    fn userinfo_is_not_a_ref() {
        assert_eq!(
            parse("https://user:token@host/o/r.git"),
            g("https://user:token@host/o/r.git", None, None)
        );
        assert_eq!(
            parse("https://token@host/o/r.git#v1"),
            g("https://token@host/o/r.git", Some("v1"), None)
        );
        assert_eq!(
            parse("https://bob:p@ss@host/o/r.git@v1"),
            g("https://bob:p@ss@host/o/r.git", Some("v1"), None)
        );
        assert_eq!(
            parse("git@github.com:me/dots.git"),
            g("git@github.com:me/dots.git", None, None)
        );
        assert_eq!(
            parse("ssh://git@h/x.git"),
            g("ssh://git@h/x.git", None, None)
        );
        // An @ starting a path segment is part of the path.
        assert_eq!(
            parse("https://h/@scope/x.git"),
            g("https://h/@scope/x.git", None, None)
        );
    }

    #[test]
    fn two_refs_or_bad_refs_are_errors() {
        let e = parse_git_source("github:me/dots@v1#main").unwrap_err();
        assert!(e.to_string().contains("two refs"), "{e}");
        let e = parse_git_source("https://bob:pw@h/o/r.git@v1#main").unwrap_err();
        assert!(!e.to_string().contains("pw"), "{e}");
        for bad in [
            "github:me/dots@--upload-pack=x",
            "github:me/dots#a:b",
            "github:me/dots@a..b",
        ] {
            assert!(parse_git_source(bad).is_err(), "{bad}");
        }
        // Not a git source at all: no ref errors.
        assert_eq!(parse_git_source("./x@a#b").unwrap(), None);
    }

    #[test]
    fn short_commits() {
        assert_eq!(short_commit(SHA), "0123456");
        assert_eq!(short_commit("abc"), "abc");
        assert!(looks_like_sha("abc1234") && looks_like_sha(SHA));
        assert!(!looks_like_sha("main") && !looks_like_sha("abc"));
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
    fn repo_names() {
        assert_eq!(repo_name("https://github.com/o/plugin.git"), "plugin");
        assert_eq!(repo_name("https://gitlab.com/o/plugin/"), "plugin");
        assert_eq!(repo_name("git@github.com:o/plugin.git"), "plugin");
        assert_eq!(repo_name("git@host:plugin"), "plugin");
    }

    #[test]
    fn rejects_escaping_subdirs() {
        assert!(safe_subdir("../x").is_err());
        assert!(safe_subdir("/etc").is_err());
        assert!(safe_subdir("a/b").is_ok());
    }
}
