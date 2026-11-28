//! `kitbag apply --remote` / `kitbag diff --remote`: run a profile on another
//! machine over SSH.
//!
//! kitbag shells out to the user's own `ssh`, so their `~/.ssh/config`, agent,
//! jump hosts and known-hosts handling apply exactly as when they type `ssh`
//! themselves; kitbag adds no options, stores no credentials and asks for no
//! passwords. The profile directory is copied to a private temporary directory
//! on the host, a kitbag there plans (and, after confirmation, applies) it, and
//! the directory is removed again. The remote kitbag records the apply in the
//! remote host's own state directory, so `kitbag undo` there undoes it.
//!
//! Nothing resolved is sent: the profile holds `${VAR}` references, a profile
//! that holds what looks like a literal secret is refused, and the remote
//! kitbag resolves references from the remote host's environment.
//!
//! A secrets bundle is opened on the host, never here: [`HostSecrets`] names
//! a `secrets.age` there (or the profile's own, copied only when it is
//! age-encrypted) and the host's passphrase file or identity, as paths on the
//! host. kitbag never reads those files, and only their paths go over ssh.

use std::fs;
use std::io::Write;
use std::path::Path;
use std::process::{Command, Stdio};

use anyhow::{Context, Result, bail};

use crate::profile_io::MAX_PROFILE_BYTES;

/// Where releases (and their `.sha256` files) are published.
pub const RELEASE_BASE: &str = "https://github.com/cargopete/kitbag/releases/download";

/// Prefix of the temporary directory kitbag creates on the remote host.
const TMP_PREFIX: &str = "kitbag-remote.";

/// Lines the scripts we run remotely use to report back; everything else the
/// remote shell prints (banners, motd) is ignored.
const MARK: &str = "kitbag-remote:";

#[derive(Debug, Clone)]
pub struct Remote {
    /// The ssh program: `ssh`, found on PATH like the user would type it.
    pub ssh: String,
    /// What is handed to `ssh` as the destination: `host`, `user@host`, or an
    /// `ssh://` URI.
    pub host: String,
    /// Path of kitbag on the remote host, instead of looking it up.
    pub kitbag: Option<String>,
    /// If the remote host has no kitbag, download the release matching this
    /// kitbag there, after verifying its checksum.
    pub download: bool,
    /// Open a secrets bundle on the host (`--remote-secrets-*`).
    pub secrets: Option<HostSecrets>,
    /// A project path on the host (`--remote-project`): the host's kitbag is
    /// run with `--project` for it, so project-scope files land there.
    pub project: Option<String>,
}

/// A path on the host given on the command line: not empty, not an option,
/// no control characters (it is shell-quoted when sent).
pub fn validate_host_path(flag: &str, p: &str) -> Result<()> {
    if p.is_empty() || p.starts_with('-') || p.chars().any(|c| c.is_control()) {
        bail!("{flag} {p:?} is not a usable path on the host");
    }
    Ok(())
}

/// Which bundle the host's kitbag opens.
#[derive(Debug, Clone)]
pub enum BundleOnHost {
    /// A `secrets.age` already on the host, at this path there.
    Path(String),
    /// The profile's own `secrets.age`, copied with the profile (it must be
    /// age-encrypted).
    Copied,
}

/// How the host opens a bundle: paths on the host only. None of these files
/// is read on this machine, and no passphrase, key or value is sent.
#[derive(Debug, Clone)]
pub struct HostSecrets {
    pub from: BundleOnHost,
    /// A passphrase file on the host.
    pub passphrase_file: Option<String>,
    /// Identity files on the host.
    pub identities: Vec<String>,
}

impl HostSecrets {
    /// Every host path is a plain, non-empty path: not an option, no control
    /// characters (they are shell-quoted when sent).
    pub fn validate(&self) -> Result<()> {
        let mut paths: Vec<(&str, &str)> = Vec::new();
        if let BundleOnHost::Path(p) = &self.from {
            paths.push(("--remote-secrets-from", p));
        }
        if let Some(p) = &self.passphrase_file {
            paths.push(("--remote-secrets-passphrase-file", p));
        }
        for p in &self.identities {
            paths.push(("--remote-secrets-identity", p));
        }
        for (flag, p) in paths {
            validate_host_path(flag, p)?;
        }
        Ok(())
    }

    /// The host kitbag's `--secrets-*` options, as shell words; `bundle` is
    /// the copied file's path there for [`BundleOnHost::Copied`].
    fn words(&self, copied: &str) -> Vec<String> {
        let from = match &self.from {
            BundleOnHost::Path(p) => path_word(p),
            BundleOnHost::Copied => sh_quote(copied),
        };
        let mut v = vec![sh_quote("--secrets-from"), from];
        if let Some(p) = &self.passphrase_file {
            v.push(sh_quote("--secrets-passphrase-file"));
            v.push(path_word(p));
        }
        for p in &self.identities {
            v.push(sh_quote("--secrets-identity"));
            v.push(path_word(p));
        }
        v
    }
}

#[derive(Debug, Clone, Copy)]
pub enum Flow {
    /// `kitbag diff` there; its exit status is passed on.
    Diff { exit_code: bool },
    /// `kitbag apply` there. Without `yes` the plan is shown first and the
    /// apply only happens after a local confirmation.
    Apply { yes: bool, dry_run: bool },
}

/// Quote `s` for a POSIX shell: single quotes, with embedded ones escaped.
pub fn sh_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', r"'\''"))
}

/// `ssh` would read a host starting with `-` as an option (`-oProxyCommand=...`),
/// and whitespace or control characters can't be part of a destination.
pub fn validate_host(host: &str) -> Result<()> {
    if host.is_empty() {
        bail!("--remote needs a host, e.g. --remote you@example.com");
    }
    if host.starts_with('-') {
        bail!("--remote {host:?} looks like an ssh option, not a host");
    }
    if host.chars().any(|c| c.is_whitespace() || c.is_control()) {
        bail!("--remote {host:?} is not a valid ssh destination");
    }
    Ok(())
}

fn ssh(r: &Remote, remote_command: &str) -> Command {
    // `ssh` as the user would type it, with none of its options set here, so
    // the user's own config decides; `--` keeps a host from being an option.
    let mut c = Command::new(&r.ssh);
    c.arg("--").arg(&r.host).arg(remote_command);
    c
}

/// Fail with a pointer at ssh when it itself failed (exit status 255).
fn ssh_status(host: &str, what: &str, code: Option<i32>) -> Result<i32> {
    match code {
        Some(255) | None => bail!(
            "ssh to {host} failed while {what}; check that `ssh {host}` works from this shell"
        ),
        Some(c) => Ok(c),
    }
}

/// Run `script` with `sh -s` on the host and return the lines it reported.
fn run_script(r: &Remote, script: &str) -> Result<Vec<(String, String)>> {
    let host = r.host.as_str();
    let mut child = ssh(r, "sh -s")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .spawn()
        .context("could not run `ssh`; is OpenSSH installed and on your PATH?")?;
    {
        let mut stdin = child.stdin.take().context("no stdin for ssh")?;
        // The remote script may exit before reading all of it.
        let _ = stdin.write_all(script.as_bytes());
    }
    let out = child.wait_with_output()?;
    let text = String::from_utf8_lossy(&out.stdout);
    let found: Vec<(String, String)> = text
        .lines()
        .filter_map(|l| l.strip_prefix(MARK))
        .filter_map(|l| l.split_once('='))
        .map(|(k, v)| (k.to_string(), v.trim_end().to_string()))
        .collect();
    if let Some((_, e)) = found.iter().find(|(k, _)| k == "error") {
        bail!("on {host}: {e}");
    }
    if !out.status.success() {
        ssh_status(host, "preparing the remote side", out.status.code())?;
        bail!("the remote setup on {host} failed ({})", out.status);
    }
    Ok(found)
}

fn get<'a>(found: &'a [(String, String)], key: &str) -> Option<&'a str> {
    found
        .iter()
        .find(|(k, _)| k == key)
        .map(|(_, v)| v.as_str())
        .filter(|v| !v.is_empty())
}

/// Shell word for a path on the host (`--remote-kitbag`,
/// `--remote-project`, `--remote-secrets-*`); `~/` means the remote home, which a quoted `~` would
/// not.
fn path_word(p: &str) -> String {
    match p.strip_prefix("~/") {
        Some(rest) => format!("\"$HOME\"{}", sh_quote(&format!("/{rest}"))),
        None => sh_quote(p),
    }
}

const PROBE: &str = r#"set -eu
umask 077
tmp="$(mktemp -d "${TMPDIR:-/tmp}/kitbag-remote.XXXXXX")" || { printf 'kitbag-remote:error=%s\n' 'could not create a temporary directory'; exit 1; }
printf 'kitbag-remote:tmp=%s\n' "$tmp"
# An error here stops the whole run, so take the directory with it.
bail() { printf 'kitbag-remote:error=%s\n' "$*"; rm -rf "$tmp"; exit 1; }
explicit=@EXPLICIT@
bin=''
if [ -n "$explicit" ]; then
  bin="$(command -v "$explicit" 2>/dev/null || true)"
  case "$bin" in
    /*) ;;
    *) bail "--remote-kitbag $explicit is not an executable file on this host" ;;
  esac
else
  # A non-interactive ssh shell often lacks ~/.local/bin (where install.sh puts
  # kitbag) on its PATH, so look there and in cargo's bin dir as well.
  # `--remote-download` keeps its verified copy in the XDG data dir.
  for c in kitbag "$HOME/.local/bin/kitbag" "$HOME/.cargo/bin/kitbag" "$HOME/.local/share/kitbag/bin/kitbag"; do
    p="$(command -v "$c" 2>/dev/null || true)"
    case "$p" in
      /*) bin="$p"; break ;;
    esac
  done
fi
if [ -n "$bin" ]; then
  ver="$("$bin" --version 2>/dev/null)" || ver=''
  case "$ver" in
    kitbag\ *) printf 'kitbag-remote:kitbag=%s\n' "$bin"; printf 'kitbag-remote:version=%s\n' "$ver" ;;
    *) if [ -n "$explicit" ]; then bail "$bin did not run as kitbag (--version printed: $ver)"; fi ;;
  esac
fi
"#;

/// Downloads the release for this host's OS and CPU into `$tmp/download`,
/// checks it against the published `.sha256` and only then unpacks and runs
/// it. Fails closed: a missing, malformed or mismatching checksum stops it
/// before anything is unpacked. Mirrors `install.sh`.
const DOWNLOAD: &str = r#"set -eu
umask 077
tmp=@TMP@
version=@VERSION@
base=@BASE@
die() { printf 'kitbag-remote:error=%s\n' "$*"; exit 1; }
command -v tar >/dev/null 2>&1 || die "tar is required on this host to unpack kitbag"
if command -v curl >/dev/null 2>&1; then
  fetch() { curl --proto '@PROTO@' --tlsv1.2 -fsSL "$1" -o "$2"; }
elif command -v wget >/dev/null 2>&1; then
  fetch() { wget -q --https-only "$1" -O "$2"; }
else
  die "curl or wget is required on this host to download kitbag"
fi
os="$(uname -s)"
arch="$(uname -m)"
case "$arch" in
  x86_64 | amd64) arch="x86_64" ;;
  arm64 | aarch64) arch="aarch64" ;;
  *) die "unsupported CPU architecture on this host: $arch" ;;
esac
case "$os" in
  Linux) target="$arch-unknown-linux-musl" ;;
  Darwin) target="$arch-apple-darwin" ;;
  *) die "unsupported OS on this host: $os" ;;
esac
archive="kitbag-$target.tar.gz"
checksum="kitbag-$target.sha256"
dl="$tmp/download"
mkdir -p "$dl" "$tmp/bin"
fetch "$base/$archive" "$dl/$archive" || die "download failed: $base/$archive (is $version a published release?)"
fetch "$base/$checksum" "$dl/$checksum" || die "checksum download failed: $base/$checksum"
is_sha256() {
  case "$1" in
    *[!0-9a-f]*) return 1 ;;
  esac
  [ "${#1}" -eq 64 ]
}
expected="$(cut -d ' ' -f 1 < "$dl/$checksum" | tr 'A-F' 'a-f')"
is_sha256 "$expected" || die "$checksum does not hold a SHA-256 checksum"
if command -v sha256sum >/dev/null 2>&1; then
  actual="$(sha256sum "$dl/$archive" | cut -d ' ' -f 1)"
elif command -v shasum >/dev/null 2>&1; then
  actual="$(shasum -a 256 "$dl/$archive" | cut -d ' ' -f 1)"
else
  die "sha256sum or shasum is required on this host to verify the download"
fi
actual="$(printf '%s' "$actual" | tr 'A-F' 'a-f')"
is_sha256 "$actual" || die "could not compute the SHA-256 of $archive"
[ "$expected" = "$actual" ] || die "checksum mismatch for $archive (expected $expected, got $actual); nothing was run"
mkdir -p "$dl/unpacked"
tar -xzf "$dl/$archive" -C "$dl/unpacked" || die "could not unpack $archive"
src="$dl/unpacked/kitbag"
[ -f "$src" ] || src="$(find "$dl/unpacked" -type f -name kitbag | head -n 1)"
[ -n "$src" ] && [ -f "$src" ] || die "archive did not contain kitbag"
cp "$src" "$tmp/bin/kitbag"
chmod 755 "$tmp/bin/kitbag"
ver="$("$tmp/bin/kitbag" --version 2>/dev/null)" || die "the downloaded kitbag did not run on this host"
# Verified and runs: keep it where a later `undo --remote` (or the probe) finds
# it. Written to a temporary name and renamed, so a failure here never leaves a
# half-written kitbag; $tmp itself is removed afterwards.
keep_dir="$HOME/.local/share/kitbag/bin"
(umask 022; mkdir -p "$keep_dir") || die "could not create $keep_dir"
cp "$tmp/bin/kitbag" "$keep_dir/.kitbag.new.$$" || die "could not keep kitbag in $keep_dir"
chmod 755 "$keep_dir/.kitbag.new.$$"
mv -f "$keep_dir/.kitbag.new.$$" "$keep_dir/kitbag" || { rm -f "$keep_dir/.kitbag.new.$$"; die "could not keep kitbag in $keep_dir"; }
printf 'kitbag-remote:kitbag=%s\n' "$keep_dir/kitbag"
printf 'kitbag-remote:version=%s\n' "$ver"
"#;

fn probe_script(explicit: Option<&str>) -> String {
    let word = explicit.map(path_word).unwrap_or_else(|| "''".into());
    PROBE.replace("@EXPLICIT@", &word)
}

/// The script that downloads and verifies a release, with `proto` the curl
/// protocol allow-list (`=https`; the tests use `=file`).
pub fn download_script(tmp: &str, version: &str, base: &str, proto: &str) -> String {
    DOWNLOAD
        .replace("@TMP@", &sh_quote(tmp))
        .replace("@VERSION@", &sh_quote(version))
        .replace("@BASE@", &sh_quote(base))
        .replace("@PROTO@", proto)
}

/// A directory made by the probe: absolute, ours by name, nothing odd in it.
fn check_tmp(tmp: &str) -> Result<()> {
    let name = Path::new(tmp)
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or_default();
    if !tmp.starts_with('/')
        || tmp.contains("..")
        || tmp.chars().any(|c| c.is_control())
        || !name.starts_with(TMP_PREFIX)
    {
        bail!("the remote host returned an unexpected temporary directory: {tmp:?}");
    }
    Ok(())
}

/// Copy the profile into `stage` for sending: regular files only, without
/// `.git` or `.env*` files, which are not part of a profile and may hold
/// secrets. Returns the number of files.
///
/// The profile's `secrets.age` stays behind unless `with_bundle`; then it is
/// copied only if it is an age-encrypted regular file, and the profile is
/// refused if it also holds a plaintext secrets file or what looks like an
/// age identity or private key (the host must use its own).
fn stage(from: &Path, to: &Path, total: &mut u64, with_bundle: bool) -> Result<usize> {
    let mut n = stage_dir(from, to, total, true)?;
    if with_bundle {
        let plain = crate::bundle::plaintext_secret_files(from);
        if !plain.is_empty() {
            bail!(
                "not sending secrets.age: the profile also holds plaintext secrets file(s) ({}); remove them first",
                plain.join(", ")
            );
        }
        if has_private_key(to)? {
            bail!(
                "not sending secrets.age: the profile holds what looks like an age identity or private key; keep it on the host and point --remote-secrets-identity at it there"
            );
        }
        let src = from.join(crate::bundle::FILE);
        let meta = fs::symlink_metadata(&src).with_context(|| {
            format!("--remote-include-secrets-age: {} not found", src.display())
        })?;
        if !meta.file_type().is_file() {
            bail!(
                "--remote-include-secrets-age: {} is not a regular file; not sending it",
                src.display()
            );
        }
        let bytes = crate::bundle::read_bundle(&src)?;
        if !crate::bundle::looks_like_age(&bytes) {
            bail!(
                "--remote-include-secrets-age: {} is not age-encrypted; not sending it",
                src.display()
            );
        }
        *total += bytes.len() as u64;
        let dest = to.join(crate::bundle::FILE);
        fs::write(&dest, &bytes)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&dest, fs::Permissions::from_mode(0o600))?;
        }
        n += 1;
    }
    Ok(n)
}

/// Does any staged file look like an age identity or a PEM private key?
fn has_private_key(dir: &Path) -> Result<bool> {
    const MARKS: [&[u8]; 2] = [b"AGE-SECRET-KEY-", b"PRIVATE KEY-----"];
    for e in fs::read_dir(dir)? {
        let e = e?;
        if e.file_type()?.is_dir() {
            if has_private_key(&e.path())? {
                return Ok(true);
            }
            continue;
        }
        let b = fs::read(e.path())?;
        if MARKS
            .iter()
            .any(|m| b.windows(m.len()).any(|w| w.eq_ignore_ascii_case(m)))
        {
            return Ok(true);
        }
    }
    Ok(false)
}

/// [`stage`] for one directory; `top` is the profile's own folder, where the
/// encrypted secrets bundle (and anything that looks like a plaintext one)
/// stays behind: no secret, even encrypted, is sent to the host here.
fn stage_dir(from: &Path, to: &Path, total: &mut u64, top: bool) -> Result<usize> {
    let mut n = 0;
    fs::create_dir_all(to)?;
    for entry in fs::read_dir(from).with_context(|| format!("reading {}", from.display()))? {
        let entry = entry?;
        let name = entry.file_name();
        let lossy = name.to_string_lossy();
        let kind = entry.file_type()?;
        if kind.is_symlink() {
            continue;
        }
        if kind.is_dir() {
            if lossy == ".git" {
                continue;
            }
            n += stage_dir(&entry.path(), &to.join(&name), total, false)?;
        } else if kind.is_file() {
            if lossy == ".env" || lossy.starts_with(".env.") {
                continue;
            }
            if top
                && (lossy == crate::bundle::FILE
                    || crate::bundle::plaintext_secret_files_named(&lossy))
            {
                continue;
            }
            *total += entry.metadata()?.len();
            if *total > MAX_PROFILE_BYTES {
                bail!(
                    "the profile is larger than {} MiB; not sending it",
                    MAX_PROFILE_BYTES / 1024 / 1024
                );
            }
            fs::copy(entry.path(), to.join(&name))?;
            n += 1;
        }
    }
    Ok(n)
}

/// Send the profile at `dir` to `remote_dir` on the host (a tar stream over ssh).
fn upload(r: &Remote, dir: &Path, remote_dir: &str) -> Result<usize> {
    let host = r.host.as_str();
    let staged = tempfile::tempdir()?;
    let mut total = 0;
    let with_bundle = matches!(
        r.secrets,
        Some(HostSecrets {
            from: BundleOnHost::Copied,
            ..
        })
    );
    let files = stage(dir, staged.path(), &mut total, with_bundle)?;
    let mut tar = Command::new("tar")
        .arg("-cf")
        .arg("-")
        .arg("-C")
        .arg(staged.path())
        .arg(".")
        .env("COPYFILE_DISABLE", "1")
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .spawn()
        .context("could not run `tar`")?;
    let tar_out = tar.stdout.take().context("no stdout from tar")?;
    let q = sh_quote(remote_dir);
    let status = ssh(r, &format!("mkdir -p {q} && tar -xf - -C {q}"))
        .stdin(Stdio::from(tar_out))
        .stdout(Stdio::null())
        .stderr(Stdio::inherit())
        .status()
        .context("could not run `ssh`; is OpenSSH installed and on your PATH?")?;
    let tar_status = tar.wait()?;
    if !tar_status.success() {
        bail!("tar failed while packing the profile ({tar_status})");
    }
    let code = ssh_status(host, "copying the profile", status.code())?;
    if code != 0 {
        bail!("copying the profile to {host} failed (remote exit status {code})");
    }
    Ok(files)
}

/// Run kitbag on the host with `words` (already shell-quoted), output going
/// straight to this terminal, and return its exit status.
fn run_kitbag(r: &Remote, bin: &str, words: &[String]) -> Result<i32> {
    let host = r.host.as_str();
    let cmd = std::iter::once(sh_quote(bin))
        .chain(words.iter().cloned())
        .collect::<Vec<_>>()
        .join(" ");
    let status = ssh(r, &cmd)
        .stdin(Stdio::null())
        .status()
        .context("could not run `ssh`; is OpenSSH installed and on your PATH?")?;
    ssh_status(host, "running kitbag", status.code())
}

/// Profile directory name on the remote host: the profile's own name when it
/// is a plain one (a plugin takes its name from its directory), else `profile`.
fn dir_name(hint: &str) -> String {
    if !hint.is_empty()
        && hint.len() <= 64
        && hint
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
        && !hint.starts_with('.')
    {
        hint.to_string()
    } else {
        "profile".to_string()
    }
}

/// Probe the host, run `f` with what it found and its temporary directory,
/// then always remove that directory (success or not).
fn with_host<T>(
    remote: &Remote,
    f: impl FnOnce(&[(String, String)], &str) -> Result<T>,
) -> Result<T> {
    if cfg!(windows) {
        bail!("--remote is not supported on Windows yet; run kitbag from WSL or on a Unix machine");
    }
    validate_host(&remote.host)?;
    let host = remote.host.as_str();
    let found = run_script(remote, &probe_script(remote.kitbag.as_deref()))?;
    let tmp = get(&found, "tmp")
        .context("the remote host did not report a temporary directory")?
        .to_string();
    check_tmp(&tmp)?;
    let result = f(&found, &tmp);
    // Always remove what was copied there, success or not.
    let cleanup = ssh(remote, &format!("rm -rf {}", sh_quote(&tmp)))
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .status();
    if !matches!(cleanup, Ok(s) if s.success()) {
        eprintln!("warning: could not remove {tmp} on {host}; it holds only the copied profile");
    }
    result
}

/// Copy `profile_dir` to the host and run `flow` there. `args` are the
/// `kitbag apply`/`diff` options after the source (`--to ...`).
pub fn run(
    remote: &Remote,
    profile_dir: &Path,
    name_hint: &str,
    args: &[String],
    flow: Flow,
    confirm: &dyn Fn(&str) -> Result<bool>,
) -> Result<i32> {
    with_host(remote, |found, tmp| {
        run_in(
            remote,
            profile_dir,
            name_hint,
            args,
            flow,
            confirm,
            found,
            tmp,
        )
    })
}

/// `kitbag undo` on the host, against the host's own state directory. Nothing
/// is uploaded and no secret is involved.
pub fn run_undo(remote: &Remote, id: Option<&str>, force: bool) -> Result<i32> {
    with_host(remote, |found, tmp| {
        let host = remote.host.as_str();
        let (bin, version) = find_kitbag(remote, found, tmp)?;
        note_version(host, &bin, &version);
        let mut words = vec![sh_quote("undo")];
        if force {
            words.push(sh_quote("--force"));
        }
        if let Some(id) = id {
            // After `--`, so an id can never be read as an option.
            words.push(sh_quote("--"));
            words.push(sh_quote(id));
        }
        run_kitbag(remote, &bin, &words)
    })
}

/// The kitbag to run on the host: the probe's, else a verified download.
fn find_kitbag(remote: &Remote, found: &[(String, String)], tmp: &str) -> Result<(String, String)> {
    let host = remote.host.as_str();
    Ok(match get(found, "kitbag") {
        Some(bin) => (
            bin.to_string(),
            get(found, "version").unwrap_or("").to_string(),
        ),
        None if remote.download => {
            let tag = format!("v{}", env!("CARGO_PKG_VERSION"));
            eprintln!(
                "kitbag is not installed on {host}; downloading {tag} there and verifying its checksum"
            );
            let base = format!("{RELEASE_BASE}/{tag}");
            let got = run_script(remote, &download_script(tmp, &tag, &base, "=https"))?;
            let bin = get(&got, "kitbag")
                .context("the remote host did not report the downloaded kitbag")?
                .to_string();
            eprintln!("kept the verified kitbag at {bin} on {host}");
            (bin, get(&got, "version").unwrap_or("").to_string())
        }
        None => bail!(
            "kitbag is not installed on {host} (looked on PATH, ~/.local/bin, ~/.cargo/bin and ~/.local/share/kitbag/bin). \
             Install it there (see the README), give its path with --remote-kitbag, \
             or add --remote-download to download v{} there with its checksum verified",
            env!("CARGO_PKG_VERSION")
        ),
    })
}

fn note_version(host: &str, bin: &str, version: &str) {
    let local = format!("kitbag {}", env!("CARGO_PKG_VERSION"));
    if version != local {
        eprintln!(
            "note: {host} has {} at {bin}, this machine has {local}; a profile written for a newer kitbag may not apply fully there",
            if version.is_empty() {
                "an unknown kitbag version"
            } else {
                version
            }
        );
    }
}

#[allow(clippy::too_many_arguments)]
fn run_in(
    remote: &Remote,
    profile_dir: &Path,
    name_hint: &str,
    args: &[String],
    flow: Flow,
    confirm: &dyn Fn(&str) -> Result<bool>,
    found: &[(String, String)],
    tmp: &str,
) -> Result<i32> {
    let host = remote.host.as_str();
    let (bin, version) = find_kitbag(remote, found, tmp)?;
    note_version(host, &bin, &version);
    let sub = match flow {
        Flow::Diff { .. } => "diff",
        Flow::Apply { .. } => "apply",
    };
    let dir = format!("{tmp}/{}", dir_name(name_hint));
    let files = upload(remote, profile_dir, &dir)?;
    println!(
        "Sent the profile ({files} files, no secrets resolved) to {host}; kitbag there: {bin}\n"
    );
    if let Some(p) = &remote.project {
        println!("Project scope: files go under {p} on {host}\n");
    }
    if let Some(s) = &remote.secrets {
        match &s.from {
            BundleOnHost::Path(p) => println!(
                "The secrets bundle is opened on {host} from {p}, with that host's own passphrase file or identity\n"
            ),
            BundleOnHost::Copied => println!(
                "The profile's encrypted secrets.age was sent with it and is opened on {host} with that host's own passphrase file or identity\n"
            ),
        }
    }
    let secrets_words = remote
        .secrets
        .as_ref()
        .map(|s| s.words(&format!("{dir}/{}", crate::bundle::FILE)))
        .unwrap_or_default();
    let command = |extra: &[&str]| -> Vec<String> {
        let mut v = vec![sh_quote(sub), sh_quote(&dir)];
        v.extend(args.iter().map(|a| sh_quote(a)));
        if let Some(p) = &remote.project {
            v.push(sh_quote("--project"));
            v.push(path_word(p));
        }
        v.extend(secrets_words.iter().cloned());
        v.extend(extra.iter().map(|s| sh_quote(s)));
        v
    };
    match flow {
        Flow::Diff { exit_code } => run_kitbag(
            remote,
            &bin,
            &command(if exit_code { &["--exit-code"] } else { &[] }),
        ),
        Flow::Apply { dry_run: true, .. } => run_kitbag(remote, &bin, &command(&["--dry-run"])),
        Flow::Apply { yes: true, .. } => {
            let code = run_kitbag(remote, &bin, &command(&["--yes"]))?;
            if code == 0 {
                print_undo_hint(host);
            }
            Ok(code)
        }
        Flow::Apply { .. } => {
            let code = run_kitbag(remote, &bin, &command(&["--dry-run"]))?;
            if code != 0 {
                return Ok(code);
            }
            if !confirm(&format!("\nApply this on {host}?"))? {
                println!("Cancelled; nothing was changed on {host}.");
                return Ok(1);
            }
            let code = run_kitbag(remote, &bin, &command(&["--yes"]))?;
            if code == 0 {
                print_undo_hint(host);
            }
            Ok(code)
        }
    }
}

fn print_undo_hint(host: &str) {
    println!(
        "\nUndo on {host} with: kitbag undo --remote {} (or run `kitbag undo` there)",
        sh_quote(host)
    );
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;
    use std::path::PathBuf;

    fn staged_files(dir: &Path) -> Result<Vec<PathBuf>> {
        staged_files_with(dir, false)
    }

    fn staged_files_with(dir: &Path, with_bundle: bool) -> Result<Vec<PathBuf>> {
        let t = tempfile::tempdir()?;
        let mut total = 0;
        stage(dir, t.path(), &mut total, with_bundle)?;
        let mut out = Vec::new();
        fn walk(base: &Path, dir: &Path, out: &mut Vec<PathBuf>) -> Result<()> {
            for e in fs::read_dir(dir)? {
                let e = e?;
                if e.file_type()?.is_dir() {
                    walk(base, &e.path(), out)?;
                } else {
                    out.push(e.path().strip_prefix(base)?.to_path_buf());
                }
            }
            Ok(())
        }
        walk(t.path(), t.path(), &mut out)?;
        out.sort();
        Ok(out)
    }

    #[test]
    fn quotes_for_a_posix_shell() {
        assert_eq!(sh_quote("a b"), "'a b'");
        assert_eq!(sh_quote("it's"), r"'it'\''s'");
        assert_eq!(sh_quote("$(rm -rf ~)"), "'$(rm -rf ~)'");
    }

    #[test]
    fn hosts_that_ssh_would_read_as_options_are_refused() {
        assert!(validate_host("you@example.com").is_ok());
        assert!(validate_host("example.com").is_ok());
        assert!(validate_host("ssh://you@example.com:2222").is_ok());
        assert!(validate_host("").is_err());
        assert!(validate_host("-oProxyCommand=touch /tmp/x").is_err());
        assert!(validate_host("host name").is_err());
        assert!(validate_host("host\nname").is_err());
    }

    #[test]
    fn tilde_paths_mean_the_remote_home() {
        assert_eq!(path_word("~/bin/kitbag"), "\"$HOME\"'/bin/kitbag'");
        assert_eq!(path_word("/opt/kitbag"), "'/opt/kitbag'");
    }

    #[test]
    fn only_our_own_temp_dirs_are_accepted() {
        assert!(check_tmp("/tmp/kitbag-remote.AbC123").is_ok());
        assert!(check_tmp("/tmp/other").is_err());
        assert!(check_tmp("relative/kitbag-remote.x").is_err());
        assert!(check_tmp("/tmp/../kitbag-remote.x").is_err());
        assert!(check_tmp("/tmp/kitbag-remote.x\n/etc").is_err());
    }

    #[test]
    fn staging_leaves_out_git_env_files_and_links() {
        let src = tempfile::tempdir().unwrap();
        let p = src.path();
        fs::create_dir_all(p.join("rules")).unwrap();
        fs::create_dir_all(p.join(".git")).unwrap();
        fs::write(p.join("kitbag.toml"), "name = \"x\"\n").unwrap();
        fs::write(p.join("rules/a.md"), "a").unwrap();
        fs::write(p.join(".git/config"), "x").unwrap();
        fs::write(p.join(".env"), "TOKEN=fake").unwrap();
        fs::write(p.join(".env.local"), "TOKEN=fake").unwrap();
        std::os::unix::fs::symlink("/etc/hostname", p.join("link")).unwrap();
        // The encrypted bundle and a plaintext one stay behind at the top;
        // a rule that happens to be called secrets.md does not.
        fs::write(p.join("secrets.age"), "x").unwrap();
        fs::write(p.join("secrets.json"), "x").unwrap();
        fs::write(p.join("rules/secrets.md"), "s").unwrap();
        let files = staged_files(p).unwrap();
        assert_eq!(
            files,
            vec![
                PathBuf::from("kitbag.toml"),
                PathBuf::from("rules/a.md"),
                PathBuf::from("rules/secrets.md")
            ]
        );
    }

    const AGE_HEAD: &str = "age-encryption.org/v1\n-> X25519 fake\n";

    fn bundle_profile() -> tempfile::TempDir {
        let src = tempfile::tempdir().unwrap();
        fs::write(src.path().join("kitbag.toml"), "name = \"x\"\n").unwrap();
        src
    }

    #[test]
    fn with_the_include_flag_only_an_age_encrypted_bundle_is_staged() {
        let src = bundle_profile();
        let p = src.path();
        fs::write(p.join("secrets.age"), AGE_HEAD).unwrap();
        // Without the flag it stays behind, as before.
        assert_eq!(staged_files(p).unwrap(), vec![PathBuf::from("kitbag.toml")]);
        assert_eq!(
            staged_files_with(p, true).unwrap(),
            vec![PathBuf::from("kitbag.toml"), PathBuf::from("secrets.age")]
        );
        let t = tempfile::tempdir().unwrap();
        stage(p, t.path(), &mut 0, true).unwrap();
        let mode = fs::metadata(t.path().join("secrets.age"))
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o600);
    }

    #[test]
    fn with_the_include_flag_anything_but_an_encrypted_bundle_is_refused() {
        // Missing.
        let src = bundle_profile();
        let err = staged_files_with(src.path(), true).unwrap_err();
        assert!(format!("{err:#}").contains("not found"), "{err:#}");
        // Not age-encrypted.
        let src = bundle_profile();
        fs::write(src.path().join("secrets.age"), "GITHUB_TOKEN=fake-plain\n").unwrap();
        let err = staged_files_with(src.path(), true).unwrap_err();
        assert!(format!("{err:#}").contains("not age-encrypted"), "{err:#}");
        // A link to one.
        let src = bundle_profile();
        let real = src.path().join("elsewhere.bin");
        fs::write(&real, AGE_HEAD).unwrap();
        std::os::unix::fs::symlink(&real, src.path().join("secrets.age")).unwrap();
        let err = staged_files_with(src.path(), true).unwrap_err();
        assert!(format!("{err:#}").contains("not a regular file"), "{err:#}");
        // A plaintext sidecar next to it.
        let src = bundle_profile();
        fs::write(src.path().join("secrets.age"), AGE_HEAD).unwrap();
        fs::write(src.path().join("secrets.json"), "{}").unwrap();
        let err = staged_files_with(src.path(), true).unwrap_err();
        assert!(format!("{err:#}").contains("secrets.json"), "{err:#}");
        // An identity or private key anywhere in the profile.
        for (name, text) in [
            ("id.txt", "AGE-SECRET-KEY-1FAKEFAKEFAKE\n"),
            (
                "keys/id_ed25519",
                "-----BEGIN OPENSSH PRIVATE KEY-----\nfake\n",
            ),
        ] {
            let src = bundle_profile();
            fs::write(src.path().join("secrets.age"), AGE_HEAD).unwrap();
            let f = src.path().join(name);
            fs::create_dir_all(f.parent().unwrap()).unwrap();
            fs::write(f, text).unwrap();
            let err = staged_files_with(src.path(), true).unwrap_err();
            assert!(format!("{err:#}").contains("identity"), "{err:#}");
        }
    }

    #[test]
    fn host_secret_paths_are_quoted_words_and_never_options() {
        let s = HostSecrets {
            from: BundleOnHost::Path("~/vault/secrets.age".into()),
            passphrase_file: None,
            identities: vec!["/etc/it's/id.txt".into()],
        };
        s.validate().unwrap();
        assert_eq!(
            s.words("/unused"),
            vec![
                "'--secrets-from'".to_string(),
                "\"$HOME\"'/vault/secrets.age'".to_string(),
                "'--secrets-identity'".to_string(),
                r"'/etc/it'\''s/id.txt'".to_string(),
            ]
        );
        let copied = HostSecrets {
            from: BundleOnHost::Copied,
            passphrase_file: Some("/srv/pass.txt".into()),
            identities: vec![],
        };
        assert_eq!(
            copied.words("/tmp/kitbag-remote.x/p/secrets.age"),
            vec![
                "'--secrets-from'".to_string(),
                "'/tmp/kitbag-remote.x/p/secrets.age'".to_string(),
                "'--secrets-passphrase-file'".to_string(),
                "'/srv/pass.txt'".to_string(),
            ]
        );
        for bad in ["", "-oProxyCommand=x", "a\nb"] {
            let s = HostSecrets {
                from: BundleOnHost::Path(bad.into()),
                passphrase_file: None,
                identities: vec!["/id".into()],
            };
            assert!(s.validate().is_err(), "{bad:?}");
        }
    }

    fn target() -> String {
        let arch = match std::env::consts::ARCH {
            "aarch64" => "aarch64",
            _ => "x86_64",
        };
        match std::env::consts::OS {
            "macos" => format!("{arch}-apple-darwin"),
            _ => format!("{arch}-unknown-linux-musl"),
        }
    }

    /// A fake release in `dir`: a `kitbag` that records that it ran, its
    /// archive, and a `.sha256` file holding `checksum_line`.
    fn release(dir: &Path, marker: &Path, checksum: impl Fn(&str) -> String) {
        let stage = dir.join("stage");
        fs::create_dir_all(&stage).unwrap();
        let bin = stage.join("kitbag");
        fs::write(
            &bin,
            format!(
                "#!/bin/sh\ntouch '{}'\necho 'kitbag 9.9.9'\n",
                marker.display()
            ),
        )
        .unwrap();
        fs::set_permissions(&bin, fs::Permissions::from_mode(0o755)).unwrap();
        let archive = format!("kitbag-{}.tar.gz", target());
        let ok = Command::new("tar")
            .arg("-czf")
            .arg(dir.join(&archive))
            .arg("-C")
            .arg(&stage)
            .arg("kitbag")
            .status()
            .unwrap()
            .success();
        assert!(ok);
        let out = Command::new("sha256sum")
            .arg(dir.join(&archive))
            .output()
            .unwrap();
        let real = String::from_utf8(out.stdout)
            .unwrap()
            .split(' ')
            .next()
            .unwrap()
            .to_string();
        fs::write(
            dir.join(format!("kitbag-{}.sha256", target())),
            checksum(&real),
        )
        .unwrap();
    }

    fn run_download(dir: &Path, tmp: &Path, home: &Path) -> (bool, String) {
        let base = format!("file://{}", dir.display());
        let script = download_script(tmp.to_str().unwrap(), "v9.9.9", &base, "=file");
        let mut child = Command::new("sh")
            .arg("-s")
            .env("HOME", home)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .unwrap();
        child
            .stdin
            .take()
            .unwrap()
            .write_all(script.as_bytes())
            .unwrap();
        let out = child.wait_with_output().unwrap();
        (
            out.status.success(),
            String::from_utf8_lossy(&out.stdout).into_owned(),
        )
    }

    fn have_tools() -> bool {
        ["curl", "tar", "sha256sum"].iter().all(|t| {
            Command::new(t)
                .arg("--version")
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status()
                .is_ok()
        })
    }

    #[test]
    fn a_download_with_a_matching_checksum_is_run() {
        if !have_tools() {
            return;
        }
        let rel = tempfile::tempdir().unwrap();
        let tmp = tempfile::tempdir().unwrap();
        let home = tempfile::tempdir().unwrap();
        let marker = rel.path().join("ran");
        release(rel.path(), &marker, |h| format!("{h}  kitbag.tar.gz\n"));
        let (ok, out) = run_download(rel.path(), tmp.path(), home.path());
        assert!(ok, "{out}");
        let kept = home.path().join(".local/share/kitbag/bin/kitbag");
        assert!(
            out.contains(&format!("kitbag-remote:kitbag={}", kept.display())),
            "{out}"
        );
        assert!(kept.is_file());
        assert_ne!(fs::metadata(&kept).unwrap().permissions().mode() & 0o111, 0);
        // The kept copy survives the removal of the temporary directory.
        let tmp_path = tmp.path().to_path_buf();
        fs::remove_dir_all(&tmp_path).unwrap();
        assert!(kept.is_file());
        assert_eq!(
            Command::new(&kept)
                .arg("--version")
                .output()
                .unwrap()
                .stdout,
            b"kitbag 9.9.9\n"
        );
        fs::create_dir_all(&tmp_path).unwrap();
        assert!(out.contains("kitbag-remote:version=kitbag 9.9.9"), "{out}");
        assert!(marker.exists());
    }

    #[test]
    fn a_download_with_a_wrong_checksum_is_never_unpacked_or_run() {
        if !have_tools() {
            return;
        }
        for bad in [
            "0000000000000000000000000000000000000000000000000000000000000000  x\n".to_string(),
            "not-a-checksum\n".to_string(),
            String::new(),
        ] {
            let rel = tempfile::tempdir().unwrap();
            let tmp = tempfile::tempdir().unwrap();
            let home = tempfile::tempdir().unwrap();
            let marker = rel.path().join("ran");
            release(rel.path(), &marker, |_| bad.clone());
            let (ok, out) = run_download(rel.path(), tmp.path(), home.path());
            assert!(
                !home.path().join(".local").exists(),
                "kept an unverified kitbag"
            );
            assert!(!ok, "{out}");
            assert!(out.contains("kitbag-remote:error="), "{out}");
            assert!(!marker.exists(), "an unverified download was run");
            assert!(!tmp.path().join("download/unpacked").exists());
            assert!(!tmp.path().join("bin/kitbag").exists());
        }
    }

    #[test]
    fn a_download_without_a_checksum_file_is_refused() {
        if !have_tools() {
            return;
        }
        let rel = tempfile::tempdir().unwrap();
        let tmp = tempfile::tempdir().unwrap();
        let home = tempfile::tempdir().unwrap();
        let marker = rel.path().join("ran");
        release(rel.path(), &marker, |h| h.to_string());
        fs::remove_file(rel.path().join(format!("kitbag-{}.sha256", target()))).unwrap();
        let (ok, out) = run_download(rel.path(), tmp.path(), home.path());
        assert!(!ok, "{out}");
        assert!(!home.path().join(".local").exists());
        assert!(!marker.exists());
    }
}
