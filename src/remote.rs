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
//!
//! The host may be Linux, macOS or Windows (OpenSSH Server). A first, small
//! command ([`DETECT`]) tells them apart whatever the host's login shell is
//! (`cmd.exe`, PowerShell or a POSIX shell). A POSIX host gets `sh -s` scripts
//! as before. A Windows host never gets a POSIX script: every command is
//! `powershell.exe -NoProfile -NonInteractive -ExecutionPolicy Bypass
//! -EncodedCommand <Base64 UTF-16LE>`, which runs the same way under any
//! `DefaultShell` (the Base64 alphabet has nothing a shell interprets). Values
//! go into those scripts only as PowerShell single-quoted literals
//! ([`ps_quote`]), and the host's kitbag and `tar.exe` are started through
//! `System.Diagnostics.Process` with a command line built here by the MSVC
//! rules ([`win_arg`]), so neither PowerShell's own native-argument passing nor
//! `cmd.exe` ever re-parses an argument.

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

    /// The host kitbag's `--secrets-*` options; `copied` is the copied
    /// file's path there for [`BundleOnHost::Copied`].
    fn args(&self, copied: &str) -> Vec<Word> {
        let from = match &self.from {
            BundleOnHost::Path(p) => Word::HostPath(p.clone()),
            BundleOnHost::Copied => Word::lit(copied),
        };
        let mut v = vec![Word::lit("--secrets-from"), from];
        if let Some(p) = &self.passphrase_file {
            v.push(Word::lit("--secrets-passphrase-file"));
            v.push(Word::HostPath(p.clone()));
        }
        for p in &self.identities {
            v.push(Word::lit("--secrets-identity"));
            v.push(Word::HostPath(p.clone()));
        }
        v
    }

    /// [`HostSecrets::args`] as POSIX shell words.
    #[cfg(all(test, unix))]
    fn words(&self, copied: &str) -> Vec<String> {
        self.args(copied).iter().map(Word::posix).collect()
    }
}

/// One argument for the host's kitbag, quoted for the host's shell only when
/// the command is put together.
#[derive(Debug, Clone)]
enum Word {
    /// Sent as it is.
    Lit(String),
    /// A path on the host given on the command line: `~/` there means the
    /// host user's home (`$HOME`, or `%USERPROFILE%` on Windows).
    HostPath(String),
}

impl Word {
    fn lit(s: &str) -> Self {
        Word::Lit(s.to_string())
    }

    /// The word for a POSIX shell.
    fn posix(&self) -> String {
        match self {
            Word::Lit(s) => sh_quote(s),
            Word::HostPath(p) => path_word(p),
        }
    }

    /// The argument itself on a Windows host whose user profile is `home`.
    fn windows(&self, home: &str) -> String {
        match self {
            Word::Lit(s) => s.clone(),
            Word::HostPath(p) => win_host_path(p, home),
        }
    }
}

/// What kind of host the probe found.
#[derive(Debug, Clone)]
enum Side {
    /// Linux or macOS: POSIX `sh` scripts.
    Posix,
    /// Windows: PowerShell, run explicitly. `home` is `%USERPROFILE%` there.
    Windows { home: String },
}

impl Side {
    /// `dir` and `name` joined with the host's separator.
    fn join(&self, dir: &str, name: &str) -> String {
        match self {
            Side::Posix => format!("{dir}/{name}"),
            Side::Windows { .. } => format!("{dir}\\{name}"),
        }
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
    run_remote(r, "sh -s", script.as_bytes(), "the remote setup")
}

/// Run a PowerShell `script` on a Windows host and return the lines it
/// reported. The script goes over stdin (Base64 of its UTF-8, read by
/// [`PS_STDIN`]), as the probe and download are too long for `cmd.exe`'s
/// command line.
fn run_ps_script(r: &Remote, script: &str) -> Result<Vec<(String, String)>> {
    let (command, payload) = ps_script_invocation(script)?;
    run_remote(
        r,
        &command,
        payload.as_bytes(),
        "the remote setup (PowerShell)",
    )
}

/// The ssh command and the stdin that run a long PowerShell `script` on a
/// Windows host, as the probe and download are run.
#[doc(hidden)]
pub fn ps_script_invocation(script: &str) -> Result<(String, String)> {
    let mut payload = base64(script.as_bytes());
    payload.push('\n');
    Ok((ps_command(PS_STDIN)?, payload))
}

/// Run `command` on the host with `input` on its stdin and return the
/// [`MARK`] lines it printed.
fn run_remote(
    r: &Remote,
    command: &str,
    input: &[u8],
    what: &str,
) -> Result<Vec<(String, String)>> {
    let host = r.host.as_str();
    let script = input;
    let mut child = ssh(r, command)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .spawn()
        .context("could not run `ssh`; is OpenSSH installed and on your PATH?")?;
    {
        let mut stdin = child.stdin.take().context("no stdin for ssh")?;
        // The remote script may exit before reading all of it.
        let _ = stdin.write_all(script);
    }
    let out = child.wait_with_output()?;
    let found = marks(&String::from_utf8_lossy(&out.stdout));
    if let Some((_, e)) = found.iter().find(|(k, _)| k == "error") {
        bail!("on {host}: {e}");
    }
    if !out.status.success() {
        ssh_status(host, "preparing the remote side", out.status.code())?;
        bail!("{what} on {host} failed ({})", out.status);
    }
    Ok(found)
}

/// The `kitbag-remote:key=value` lines in `text` (a trailing `\r` from a
/// Windows host dropped).
fn marks(text: &str) -> Vec<(String, String)> {
    text.lines()
        .filter_map(|l| l.trim_start().strip_prefix(MARK))
        .filter_map(|l| l.split_once('='))
        .map(|(k, v)| (k.to_string(), v.trim_end().to_string()))
        .collect()
}

/// The first command sent: prints `Windows_NT` in the reported value on a
/// Windows host whatever its `DefaultShell`, and never on a POSIX one.
/// `cmd.exe` expands `%OS%`; PowerShell expands `$env:OS` after the literal
/// backslash; a POSIX shell (sh, bash, zsh, fish, csh) expands neither, as
/// `\$` is a literal `$` there. A Windows host whose shell is Git Bash or
/// another POSIX shell is caught by the POSIX probe's `uname` instead.
pub const DETECT: &str = r"echo kitbag-remote:os=%OS%\$env:OS";

/// Is the host Windows? Asked with [`DETECT`]; anything but an ssh failure
/// that doesn't print `Windows_NT` means a POSIX host, as before.
fn detect_windows(r: &Remote) -> Result<bool> {
    let out = ssh(r, DETECT)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .output()
        .context("could not run `ssh`; is OpenSSH installed and on your PATH?")?;
    if !out.status.success() {
        ssh_status(&r.host, "checking the host's OS", out.status.code())?;
    }
    Ok(reports_windows(&marks(&String::from_utf8_lossy(
        &out.stdout,
    ))))
}

fn reports_windows(found: &[(String, String)]) -> bool {
    found
        .iter()
        .any(|(k, v)| k == "os" && v.contains("Windows_NT"))
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

// ---- Windows hosts.

/// How every command on a Windows host starts: Windows PowerShell, named
/// explicitly so the host's `DefaultShell` (`cmd.exe` unless an admin changed
/// it) only has to start it, with no profile, no prompts and no execution
/// policy in the way of a script given on the command line.
pub const PS_EXE: &str = "powershell.exe -NoProfile -NonInteractive -ExecutionPolicy Bypass";

/// `cmd.exe` takes at most 8191 characters on a command line.
const MAX_WIN_COMMAND: usize = 8000;

/// Where `install.ps1` and `--remote-download` keep kitbag on a Windows host,
/// under `%LOCALAPPDATA%`.
pub const WIN_KEEP: &str = r"kitbag\bin\kitbag.exe";

/// The ssh command that runs `script` on a Windows host:
/// `powershell.exe ... -EncodedCommand` with the script as Base64 of its
/// UTF-16LE text.
pub fn ps_command(script: &str) -> Result<String> {
    let utf16: Vec<u8> = script.encode_utf16().flat_map(u16::to_le_bytes).collect();
    let c = format!("{PS_EXE} -EncodedCommand {}", base64(&utf16));
    if c.len() > MAX_WIN_COMMAND {
        bail!(
            "the command for the Windows host is too long for its command line; use shorter paths or fewer options"
        );
    }
    Ok(c)
}

/// Standard Base64 (with padding).
pub fn base64(bytes: &[u8]) -> String {
    const T: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for c in bytes.chunks(3) {
        let n = (u32::from(c[0]) << 16)
            | (u32::from(*c.get(1).unwrap_or(&0)) << 8)
            | u32::from(*c.get(2).unwrap_or(&0));
        out.push(T[(n >> 18) as usize & 63] as char);
        out.push(T[(n >> 12) as usize & 63] as char);
        out.push(if c.len() > 1 {
            T[(n >> 6) as usize & 63] as char
        } else {
            '='
        });
        out.push(if c.len() > 2 {
            T[n as usize & 63] as char
        } else {
            '='
        });
    }
    out
}

/// A PowerShell single-quoted literal: nothing in it is expanded (`$`,
/// backticks and `$(...)` stay text). PowerShell also ends such a string at
/// the typographic single quotes, so those are doubled like `'`.
pub fn ps_quote(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('\'');
    for c in s.chars() {
        if matches!(c, '\'' | '\u{2018}' | '\u{2019}' | '\u{201A}' | '\u{201B}') {
            out.push(c);
        }
        out.push(c);
    }
    out.push('\'');
    out
}

/// One argument on a Windows command line, quoted by the rules the MSVC
/// runtime (and Rust's `std::env::args`) use to split it again: always in
/// double quotes, a `"` as `\"`, and backslashes doubled only where they come
/// before a quote.
pub fn win_arg(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    let mut backslashes = 0;
    for c in s.chars() {
        match c {
            '\\' => backslashes += 1,
            '"' => {
                out.extend(std::iter::repeat_n('\\', backslashes * 2 + 1));
                out.push('"');
                backslashes = 0;
            }
            _ => {
                out.extend(std::iter::repeat_n('\\', backslashes));
                out.push(c);
                backslashes = 0;
            }
        }
    }
    out.extend(std::iter::repeat_n('\\', backslashes * 2));
    out.push('"');
    out
}

/// A Windows command line from `args`.
fn win_command_line(args: &[String]) -> String {
    args.iter()
        .map(|a| win_arg(a))
        .collect::<Vec<_>>()
        .join(" ")
}

/// A host path on a Windows host: `~/` (or `~\`) means the user's profile
/// directory there, `home`, never a POSIX `$HOME`.
fn win_host_path(p: &str, home: &str) -> String {
    match p.strip_prefix("~/").or_else(|| p.strip_prefix("~\\")) {
        Some(rest) => format!(
            "{}\\{}",
            home.trim_end_matches(['\\', '/']),
            rest.replace('/', "\\")
        ),
        None => p.to_string(),
    }
}

/// [`win_host_path`] as a PowerShell expression, for the probe, which runs
/// before `home` is known.
fn ps_path_expr(p: &str) -> String {
    match p.strip_prefix("~/").or_else(|| p.strip_prefix("~\\")) {
        Some(rest) => format!(
            "(Join-Path $env:USERPROFILE {})",
            ps_quote(&rest.replace('/', "\\"))
        ),
        None => ps_quote(p),
    }
}

/// An absolute Windows path: `C:\...` or `C:/...`, no `..`, no control
/// characters.
fn is_win_abs(p: &str) -> bool {
    let b = p.as_bytes();
    b.len() >= 3
        && b[0].is_ascii_alphabetic()
        && b[1] == b':'
        && (b[2] == b'\\' || b[2] == b'/')
        && !p.split(['\\', '/']).any(|c| c == "..")
        && !p.chars().any(|c| c.is_control())
}

/// The release archive and checksum names for a Windows host's CPU
/// (`x86_64` or `aarch64`), as `release.yml` publishes them and `install.ps1`
/// fetches them.
pub fn windows_artifacts(arch: &str) -> (String, String) {
    (
        format!("kitbag-{arch}-pc-windows-msvc.zip"),
        format!("kitbag-{arch}-pc-windows-msvc.sha256"),
    )
}

/// Runs the PowerShell script that follows it on stdin (Base64 of its UTF-8,
/// so no console code page can garble it). A script reports a failure as a
/// `kitbag-remote:error=` line and throws `kitbag-remote-failed`; anything
/// else it throws is reported here.
const PS_STDIN: &str = "# kitbag-remote:stdin
$ErrorActionPreference = 'Stop'
$b = [Console]::In.ReadToEnd()
$s = [Text.Encoding]::UTF8.GetString([Convert]::FromBase64String(($b -replace '\\s', '')))
try { & ([scriptblock]::Create($s)) } catch { if (\"$_\" -ne 'kitbag-remote-failed') { [Console]::Out.WriteLine('kitbag-remote:error=' + \"$_\") }; exit 1 }
exit 0
";

/// Shared by the Windows scripts: report lines in UTF-8.
const PS_SAY: &str = r#"try { [Console]::OutputEncoding = New-Object System.Text.UTF8Encoding $false } catch { }
function Say([string]$k, [string]$v) { [Console]::Out.WriteLine("kitbag-remote:$k=$v") }
"#;

/// Starts `$file` with the command line `$argline` (built by [`win_arg`]),
/// not through PowerShell's own argument passing or `cmd.exe`, copies its
/// output to ours byte for byte, gives it an empty stdin, and returns its
/// exit code.
const PS_NATIVE: &str = r#"function Invoke-Native([string]$file, [string]$argline) {
  $psi = New-Object System.Diagnostics.ProcessStartInfo
  $psi.FileName = $file
  $psi.Arguments = $argline
  $psi.UseShellExecute = $false
  $psi.RedirectStandardInput = $true
  $psi.RedirectStandardOutput = $true
  $psi.RedirectStandardError = $true
  $p = [System.Diagnostics.Process]::Start($psi)
  $p.StandardInput.Close()
  $o = $p.StandardOutput.BaseStream.CopyToAsync([Console]::OpenStandardOutput())
  $e = $p.StandardError.BaseStream.CopyToAsync([Console]::OpenStandardError())
  $p.WaitForExit()
  $o.Wait()
  $e.Wait()
  $p.ExitCode
}
"#;

/// The Windows probe: makes `%TEMP%\kitbag-remote.XXXXXXXX` and looks for
/// kitbag: `--remote-kitbag`, else `kitbag.exe` on PATH, else
/// `%LOCALAPPDATA%\kitbag\bin\kitbag.exe` (install.ps1, `--remote-download`),
/// else `%USERPROFILE%\.cargo\bin\kitbag.exe`.
const WIN_PROBE: &str = r#"# kitbag-remote:probe
$ErrorActionPreference = 'Stop'
@SAY@if (-not $env:USERPROFILE -or -not $env:LOCALAPPDATA) { Say 'error' 'USERPROFILE or LOCALAPPDATA is not set on this host'; throw 'kitbag-remote-failed' }
$explicit = @EXPLICIT@
$tmp = Join-Path ([IO.Path]::GetTempPath()) ('kitbag-remote.' + [IO.Path]::GetRandomFileName().Replace('.', ''))
New-Item -ItemType Directory -Path $tmp | Out-Null
Say 'os' 'Windows_NT'
Say 'tmp' $tmp
Say 'home' $env:USERPROFILE
function Bail([string]$m) { Say 'error' $m; Remove-Item -LiteralPath $tmp -Recurse -Force -ErrorAction SilentlyContinue; throw 'kitbag-remote-failed' }
function Find([string]$c) {
  if ($c.IndexOfAny([char[]]'\/:') -ge 0) {
    if (Test-Path -LiteralPath $c -PathType Leaf) { return (Get-Item -LiteralPath $c).FullName }
    return ''
  }
  $a = Get-Command -Name $c -CommandType Application -ErrorAction SilentlyContinue | Select-Object -First 1
  if ($a) { return $a.Path }
  return ''
}
$bin = ''
if ($explicit) {
  $bin = Find $explicit
  if (-not $bin) { Bail "--remote-kitbag $explicit is not an executable file on this host" }
} else {
  foreach ($c in @('kitbag.exe', (Join-Path $env:LOCALAPPDATA '@KEEP@'), (Join-Path $env:USERPROFILE '.cargo\bin\kitbag.exe'))) {
    $bin = Find $c
    if ($bin) { break }
  }
}
if ($bin) {
  $ver = ''
  try { $ver = "$(& $bin --version 2>$null)".Trim() } catch { $ver = '' }
  if ($ver -like 'kitbag *') { Say 'kitbag' $bin; Say 'version' $ver }
  elseif ($explicit) { Bail "$bin did not run as kitbag (--version printed: $ver)" }
}
"#;

/// The Windows download: the same fail-closed checks as `install.ps1` and
/// [`download_script`]. The `.zip` is unpacked only after its SHA-256 matched
/// the published one (both 64 hex digits), and the keep path is written only
/// after the unpacked `kitbag.exe` ran, atomically as `install.ps1` does.
const WIN_DOWNLOAD: &str = r#"# kitbag-remote:download
$ErrorActionPreference = 'Stop'
$ProgressPreference = 'SilentlyContinue'
@SAY@function Die([string]$m) { Say 'error' $m; throw 'kitbag-remote-failed' }
$tmp = @TMP@
$version = @VERSION@
$base = @BASE@
$local = @LOCAL@
if (-not $env:LOCALAPPDATA) { Die 'LOCALAPPDATA is not set on this host' }
$a = ''
try { $a = "$([System.Runtime.InteropServices.RuntimeInformation]::OSArchitecture)" } catch { $a = '' }
if (-not $a) { $a = if ($env:PROCESSOR_ARCHITEW6432) { $env:PROCESSOR_ARCHITEW6432 } else { "$env:PROCESSOR_ARCHITECTURE" } }
$arch = switch ($a) { 'X64' { 'x86_64' } 'AMD64' { 'x86_64' } 'Arm64' { 'aarch64' } default { '' } }
if (-not $arch) { Die "unsupported CPU architecture on this host: $a" }
if ($arch -eq 'aarch64') { $archive = @ZIP_ARM@; $checksum = @SUM_ARM@ } else { $archive = @ZIP_X64@; $checksum = @SUM_X64@ }
if (-not $local -and -not $base.StartsWith('https://')) { Die "not downloading from a non-HTTPS address: $base" }
if (-not (Get-Command Get-FileHash -ErrorAction SilentlyContinue)) { Die 'Get-FileHash is required on this host to verify the download (PowerShell 4 or later)' }
$dl = Join-Path $tmp 'download'
$binDir = Join-Path $tmp 'bin'
New-Item -ItemType Directory -Force -Path $dl | Out-Null
New-Item -ItemType Directory -Force -Path $binDir | Out-Null
function Fetch([string]$name, [string]$out) {
  if ($local) { Copy-Item -LiteralPath (Join-Path $base $name) -Destination $out }
  else {
    try { [Net.ServicePointManager]::SecurityProtocol = [Net.ServicePointManager]::SecurityProtocol -bor [Net.SecurityProtocolType]::Tls12 } catch { }
    Invoke-WebRequest -UseBasicParsing -Uri "$base/$name" -OutFile $out
  }
}
$zip = Join-Path $dl $archive
$sum = Join-Path $dl $checksum
try { Fetch $archive $zip } catch { Die "download failed: $base/$archive (is $version a published release?)" }
try { Fetch $checksum $sum } catch { Die "checksum download failed: $base/$checksum" }
$expected = "$(Get-Content -LiteralPath $sum -Raw)".Trim().Split(' ')[0].ToLowerInvariant()
if ($expected -cnotmatch '^[0-9a-f]{64}$') { Die "$checksum does not hold a SHA-256 checksum" }
$actual = "$((Get-FileHash -LiteralPath $zip -Algorithm SHA256).Hash)".ToLowerInvariant()
if ($actual -cnotmatch '^[0-9a-f]{64}$') { Die "could not compute the SHA-256 of $archive" }
if ($expected -cne $actual) { Die "checksum mismatch for $archive (expected $expected, got $actual); nothing was run" }
$unpacked = Join-Path $dl 'unpacked'
try { Expand-Archive -LiteralPath $zip -DestinationPath $unpacked -Force } catch { Die "could not unpack $archive" }
$exe = Get-ChildItem -LiteralPath $unpacked -Recurse -Filter 'kitbag.exe' | Where-Object { -not $_.PSIsContainer } | Select-Object -First 1
if (-not $exe) { Die "archive did not contain kitbag.exe" }
$run = Join-Path $binDir 'kitbag.exe'
Copy-Item -LiteralPath $exe.FullName -Destination $run
$ver = ''
# $global: as a plain assignment here would only make a local copy.
$global:LASTEXITCODE = 1
try { $ver = "$(& $run --version 2>$null)".Trim() } catch { $ver = '' }
if ($global:LASTEXITCODE -ne 0 -or -not $ver) { Die 'the downloaded kitbag did not run on this host' }
# Verified and runs: keep it where install.ps1 puts kitbag, so a later
# `undo --remote` (or the probe) finds it. Copied to a new name next to it and
# swapped in with one rename, so a failure never leaves a half-written exe.
$keep = Join-Path $env:LOCALAPPDATA '@KEEP@'
$keepDir = Split-Path -Parent $keep
$new = Join-Path $keepDir ".kitbag.exe.$([Guid]::NewGuid()).tmp"
try {
  New-Item -ItemType Directory -Force -Path $keepDir | Out-Null
  Copy-Item -LiteralPath $run -Destination $new
  if (Test-Path -LiteralPath $keep) { [IO.File]::Replace($new, $keep, [NullString]::Value) } else { [IO.File]::Move($new, $keep) }
} catch {
  Die "could not keep kitbag in ${keepDir}: $_"
} finally {
  if (Test-Path -LiteralPath $new) { Remove-Item -LiteralPath $new -Force }
}
Say 'kitbag' $keep
Say 'version' $ver
"#;

#[doc(hidden)]
pub fn win_probe_script(explicit: Option<&str>) -> String {
    let word = explicit.map(ps_path_expr).unwrap_or_else(|| "''".into());
    WIN_PROBE
        .replace("@SAY@", PS_SAY)
        .replace("@KEEP@", WIN_KEEP)
        .replace("@EXPLICIT@", &word)
}

/// The PowerShell script that downloads and verifies a Windows release into
/// `tmp` and keeps it. `local` takes `base` as a directory to copy from
/// instead of an `https://` URL (the tests use it); otherwise only
/// `https://` is fetched.
pub fn download_script_windows(tmp: &str, version: &str, base: &str, local: bool) -> String {
    let (zip_x64, sum_x64) = windows_artifacts("x86_64");
    let (zip_arm, sum_arm) = windows_artifacts("aarch64");
    WIN_DOWNLOAD
        .replace("@SAY@", PS_SAY)
        .replace("@KEEP@", WIN_KEEP)
        .replace("@TMP@", &ps_quote(tmp))
        .replace("@VERSION@", &ps_quote(version))
        .replace("@BASE@", &ps_quote(base))
        .replace("@LOCAL@", if local { "$true" } else { "$false" })
        .replace("@ZIP_X64@", &ps_quote(&zip_x64))
        .replace("@SUM_X64@", &ps_quote(&sum_x64))
        .replace("@ZIP_ARM@", &ps_quote(&zip_arm))
        .replace("@SUM_ARM@", &ps_quote(&sum_arm))
}

/// Runs `bin` on a Windows host with `args`, its output going to ours, and
/// exits with its exit code.
#[doc(hidden)]
pub fn win_run_script(bin: &str, args: &[String]) -> String {
    format!(
        "# kitbag-remote:run\n$ErrorActionPreference = 'Stop'\n$file = {}\n$argline = {}\n{PS_NATIVE}exit (Invoke-Native $file $argline)\n",
        ps_quote(bin),
        ps_quote(&win_command_line(args)),
    )
}

/// Saves the tar stream on stdin to `tar` (inside the temporary directory)
/// and unpacks it into `dir` with Windows' own `tar.exe` (bsdtar; a GNU tar
/// from Git would read `C:` as a host name).
#[doc(hidden)]
pub fn win_upload_script(dir: &str, tar: &str) -> String {
    let tar_args = win_command_line(&["-xf".into(), tar.into(), "-C".into(), dir.into()]);
    format!(
        "# kitbag-remote:upload
$ErrorActionPreference = 'Stop'
$dir = {}
$tar = {}
$tarArgs = {}
{PS_NATIVE}New-Item -ItemType Directory -Force -Path $dir | Out-Null
$in = [Console]::OpenStandardInput()
$f = [IO.File]::Create($tar)
try {{ $in.CopyTo($f) }} finally {{ $f.Close() }}
$exe = Join-Path $env:SystemRoot 'System32\\tar.exe'
if (-not (Test-Path -LiteralPath $exe -PathType Leaf)) {{ [Console]::Error.WriteLine('tar.exe (Windows 10 or later) is required on this host to unpack the profile'); exit 1 }}
$code = Invoke-Native $exe $tarArgs
Remove-Item -LiteralPath $tar -Force
exit $code
",
        ps_quote(dir),
        ps_quote(tar),
        ps_quote(&tar_args),
    )
}

/// Removes the temporary directory on a Windows host.
#[doc(hidden)]
pub fn win_cleanup_script(tmp: &str) -> String {
    format!(
        "# kitbag-remote:cleanup\n$ErrorActionPreference = 'Stop'\n$tmp = {}\nRemove-Item -LiteralPath $tmp -Recurse -Force\n",
        ps_quote(tmp)
    )
}

const PROBE: &str = r#"set -eu
umask 077
# A Windows host whose ssh shell is Git Bash, MSYS2 or Cygwin is still a Windows
# host: it gets the Windows release and Windows paths, so hand over to that.
case "$(uname -s 2>/dev/null || true)" in
  MINGW* | MSYS* | CYGWIN* | Windows_NT*) printf 'kitbag-remote:os=Windows_NT\n'; exit 0 ;;
esac
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

/// A directory made by the probe: absolute (`/...`, or `C:\...` / `C:/...`
/// on a Windows host), ours by name, nothing odd in it.
fn check_tmp(tmp: &str, windows: bool) -> Result<()> {
    let name = if windows {
        tmp.rsplit(['\\', '/']).next().unwrap_or_default()
    } else {
        Path::new(tmp)
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or_default()
    };
    let absolute = if windows {
        is_win_abs(tmp)
    } else {
        tmp.starts_with('/')
    };
    let suffix = name.strip_prefix(TMP_PREFIX).unwrap_or_default();
    if !absolute
        || tmp.contains("..")
        || tmp.chars().any(|c| c.is_control())
        || suffix.is_empty()
        || (windows && !suffix.chars().all(|c| c.is_ascii_alphanumeric()))
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

/// Send the profile at `dir` to `remote_dir` on the host (a tar stream over
/// ssh); `tmp` is the host's temporary directory.
fn upload(r: &Remote, side: &Side, dir: &Path, remote_dir: &str, tmp: &str) -> Result<usize> {
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
    let command = match side {
        Side::Posix => {
            let q = sh_quote(remote_dir);
            format!("mkdir -p {q} && tar -xf - -C {q}")
        }
        Side::Windows { .. } => ps_command(&win_upload_script(
            remote_dir,
            &side.join(tmp, ".upload.tar"),
        ))?,
    };
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
    let status = ssh(r, &command)
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

/// Run kitbag on the host with `words`, output going straight to this
/// terminal, and return its exit status.
fn run_kitbag(r: &Remote, side: &Side, bin: &str, words: &[Word]) -> Result<i32> {
    let host = r.host.as_str();
    let cmd = match side {
        Side::Posix => std::iter::once(sh_quote(bin))
            .chain(words.iter().map(Word::posix))
            .collect::<Vec<_>>()
            .join(" "),
        Side::Windows { home } => {
            let args: Vec<String> = words.iter().map(|w| w.windows(home)).collect();
            ps_command(&win_run_script(bin, &args))?
        }
    };
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

/// Find out what the host is and probe it: a temporary directory and the
/// kitbag there. A host that [`DETECT`] or the POSIX probe reports as Windows
/// gets the Windows probe.
fn probe(remote: &Remote) -> Result<(Side, Vec<(String, String)>)> {
    let host = remote.host.as_str();
    let mut windows = detect_windows(remote)?;
    if !windows {
        let found = run_script(remote, &probe_script(remote.kitbag.as_deref()))?;
        if get(&found, "tmp").is_some() || !reports_windows(&found) {
            return Ok((Side::Posix, found));
        }
        windows = true;
    }
    debug_assert!(windows);
    let found = run_ps_script(remote, &win_probe_script(remote.kitbag.as_deref())).with_context(
        || format!("{host} is a Windows host, but kitbag could not prepare its side there with PowerShell (powershell.exe)"),
    )?;
    let home = get(&found, "home")
        .filter(|h| is_win_abs(h))
        .with_context(|| {
            format!("{host} did not report a usable user profile directory (USERPROFILE)")
        })?
        .to_string();
    Ok((Side::Windows { home }, found))
}

/// Probe the host, run `f` with what it found and its temporary directory,
/// then always remove that directory (success or not).
fn with_host<T>(
    remote: &Remote,
    f: impl FnOnce(&Side, &[(String, String)], &str) -> Result<T>,
) -> Result<T> {
    validate_host(&remote.host)?;
    let host = remote.host.as_str();
    let (side, found) = probe(remote)?;
    let windows = matches!(side, Side::Windows { .. });
    let tmp = get(&found, "tmp")
        .context("the remote host did not report a temporary directory")?
        .to_string();
    check_tmp(&tmp, windows)?;
    let result = f(&side, &found, &tmp);
    // Always remove what was copied there, success or not.
    let cleanup = match side {
        Side::Posix => Ok(format!("rm -rf {}", sh_quote(&tmp))),
        Side::Windows { .. } => ps_command(&win_cleanup_script(&tmp)),
    };
    let cleanup = cleanup.and_then(|c| {
        Ok(ssh(remote, &c)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .status()?)
    });
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
    with_host(remote, |side, found, tmp| {
        run_in(
            remote,
            side,
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
    with_host(remote, |side, found, tmp| {
        let host = remote.host.as_str();
        let (bin, version) = find_kitbag(remote, side, found, tmp)?;
        note_version(host, &bin, &version);
        let mut words = vec![Word::lit("undo")];
        if force {
            words.push(Word::lit("--force"));
        }
        if let Some(id) = id {
            // After `--`, so an id can never be read as an option.
            words.push(Word::lit("--"));
            words.push(Word::lit(id));
        }
        run_kitbag(remote, side, &bin, &words)
    })
}

/// The kitbag to run on the host: the probe's, else a verified download.
fn find_kitbag(
    remote: &Remote,
    side: &Side,
    found: &[(String, String)],
    tmp: &str,
) -> Result<(String, String)> {
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
            let got = match side {
                Side::Posix => run_script(remote, &download_script(tmp, &tag, &base, "=https"))?,
                Side::Windows { .. } => {
                    run_ps_script(remote, &download_script_windows(tmp, &tag, &base, false))?
                }
            };
            let bin = get(&got, "kitbag")
                .context("the remote host did not report the downloaded kitbag")?
                .to_string();
            eprintln!("kept the verified kitbag at {bin} on {host}");
            (bin, get(&got, "version").unwrap_or("").to_string())
        }
        None => bail!(
            "kitbag is not installed on {host} (looked on PATH, {}). \
             Install it there (see the README), give its path with --remote-kitbag, \
             or add --remote-download to download v{} there with its checksum verified",
            match side {
                Side::Posix => "~/.local/bin, ~/.cargo/bin and ~/.local/share/kitbag/bin",
                Side::Windows { .. } => r"%LOCALAPPDATA%\kitbag\bin and %USERPROFILE%\.cargo\bin",
            },
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
    side: &Side,
    profile_dir: &Path,
    name_hint: &str,
    args: &[String],
    flow: Flow,
    confirm: &dyn Fn(&str) -> Result<bool>,
    found: &[(String, String)],
    tmp: &str,
) -> Result<i32> {
    let host = remote.host.as_str();
    let (bin, version) = find_kitbag(remote, side, found, tmp)?;
    note_version(host, &bin, &version);
    let sub = match flow {
        Flow::Diff { .. } => "diff",
        Flow::Apply { .. } => "apply",
    };
    let dir = side.join(tmp, &dir_name(name_hint));
    let files = upload(remote, side, profile_dir, &dir, tmp)?;
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
        .map(|s| s.args(&side.join(&dir, crate::bundle::FILE)))
        .unwrap_or_default();
    let command = |extra: &[&str]| -> Vec<Word> {
        let mut v = vec![Word::lit(sub), Word::lit(&dir)];
        v.extend(args.iter().map(|a| Word::lit(a)));
        if let Some(p) = &remote.project {
            v.push(Word::lit("--project"));
            v.push(Word::HostPath(p.clone()));
        }
        v.extend(secrets_words.iter().cloned());
        v.extend(extra.iter().map(|s| Word::lit(s)));
        v
    };
    let run = |words: &[Word]| run_kitbag(remote, side, &bin, words);
    match flow {
        Flow::Diff { exit_code } => run(&command(if exit_code { &["--exit-code"] } else { &[] })),
        Flow::Apply { dry_run: true, .. } => run(&command(&["--dry-run"])),
        Flow::Apply { yes: true, .. } => {
            let code = run(&command(&["--yes"]))?;
            if code == 0 {
                print_undo_hint(host);
            }
            Ok(code)
        }
        Flow::Apply { .. } => {
            let code = run(&command(&["--dry-run"]))?;
            if code != 0 {
                return Ok(code);
            }
            if !confirm(&format!("\nApply this on {host}?"))? {
                println!("Cancelled; nothing was changed on {host}.");
                return Ok(1);
            }
            let code = run(&command(&["--yes"]))?;
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

#[cfg(test)]
mod win_tests {
    use super::*;

    const NASTY: [&str; 9] = [
        "plain",
        "it's",
        "'; Remove-Item -Recurse C:\\ ; '",
        "$(Remove-Item C:\\x)",
        "`$env:USERPROFILE`n",
        "a \"quoted\" word",
        "smart \u{2018}quotes\u{2019} \u{201A}x\u{201B}",
        "trailing backslash \\",
        "C:\\Program Files\\it's \"odd\" \\\\",
    ];

    fn unbase64(s: &str) -> Vec<u8> {
        const T: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
        let mut out = Vec::new();
        for chunk in s.as_bytes().chunks(4) {
            let pad = chunk.iter().filter(|&&c| c == b'=').count();
            let mut n = 0u32;
            for &c in chunk {
                let v = if c == b'=' {
                    0
                } else {
                    T.iter().position(|&x| x == c).unwrap() as u32
                };
                n = (n << 6) | v;
            }
            out.push((n >> 16) as u8);
            if pad < 2 {
                out.push((n >> 8) as u8);
            }
            if pad < 1 {
                out.push(n as u8);
            }
        }
        out
    }

    /// PowerShell's reading of a single-quoted literal: `None` if the string
    /// ends anywhere but at the very end (anything after it would run).
    fn ps_unquote(lit: &str) -> Option<String> {
        const Q: [char; 5] = ['\'', '\u{2018}', '\u{2019}', '\u{201A}', '\u{201B}'];
        let mut it = lit.chars().peekable();
        if !Q.contains(&it.next()?) {
            return None;
        }
        let mut out = String::new();
        while let Some(c) = it.next() {
            if Q.contains(&c) {
                if matches!(it.peek(), Some(n) if Q.contains(n)) {
                    out.push(it.next().unwrap());
                } else {
                    return it.next().is_none().then_some(out);
                }
            } else {
                out.push(c);
            }
        }
        None
    }

    /// How the MSVC runtime (and Rust on Windows) splits a command line.
    fn msvc_split(s: &str) -> Vec<String> {
        let c: Vec<char> = s.chars().collect();
        let (mut i, mut args) = (0, Vec::new());
        while i < c.len() {
            while i < c.len() && (c[i] == ' ' || c[i] == '\t') {
                i += 1;
            }
            if i >= c.len() {
                break;
            }
            let (mut cur, mut quoted) = (String::new(), false);
            while i < c.len() && (quoted || (c[i] != ' ' && c[i] != '\t')) {
                if c[i] == '\\' {
                    let start = i;
                    while i < c.len() && c[i] == '\\' {
                        i += 1;
                    }
                    let n = i - start;
                    if i < c.len() && c[i] == '"' {
                        cur.extend(std::iter::repeat_n('\\', n / 2));
                        if n % 2 == 1 {
                            cur.push('"');
                            i += 1;
                        }
                    } else {
                        cur.extend(std::iter::repeat_n('\\', n));
                    }
                } else if c[i] == '"' {
                    if quoted && c.get(i + 1) == Some(&'"') {
                        cur.push('"');
                        i += 2;
                    } else {
                        quoted = !quoted;
                        i += 1;
                    }
                } else {
                    cur.push(c[i]);
                    i += 1;
                }
            }
            args.push(cur);
        }
        args
    }

    #[test]
    fn base64_matches_the_standard_vectors() {
        for (i, o) in [
            ("", ""),
            ("f", "Zg=="),
            ("fo", "Zm8="),
            ("foo", "Zm9v"),
            ("foob", "Zm9vYg=="),
            ("fooba", "Zm9vYmE="),
            ("foobar", "Zm9vYmFy"),
        ] {
            assert_eq!(base64(i.as_bytes()), o);
            assert_eq!(unbase64(o), i.as_bytes());
        }
    }

    #[test]
    fn powershell_literals_never_end_early() {
        for s in NASTY {
            let q = ps_quote(s);
            assert_eq!(ps_unquote(&q).as_deref(), Some(s), "{q}");
        }
        assert_eq!(ps_quote("$(x)"), "'$(x)'");
        assert_eq!(ps_quote("it's"), "'it''s'");
    }

    #[test]
    fn windows_command_lines_split_back_into_the_same_arguments() {
        let mut args: Vec<String> = NASTY.iter().map(|s| s.to_string()).collect();
        args.push(String::new());
        args.push("--to".into());
        args.push("tab\there".into());
        let line = win_command_line(&args);
        assert_eq!(msvc_split(&line), args, "{line}");
        assert_eq!(win_arg(r"C:\a b\"), r#""C:\a b\\""#);
        assert_eq!(win_arg(r#"a"b"#), r#""a\"b""#);
    }

    #[test]
    fn the_ssh_command_for_windows_is_encoded_powershell_and_nothing_else() {
        let script = win_run_script(r"C:\kb\kitbag.exe", &["diff".into(), "it's $(x)".into()]);
        let c = ps_command(&script).unwrap();
        let b64 = c
            .strip_prefix(
                "powershell.exe -NoProfile -NonInteractive -ExecutionPolicy Bypass -EncodedCommand ",
            )
            .unwrap();
        assert!(
            b64.bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"+/=".contains(&b))
        );
        let units: Vec<u16> = unbase64(b64)
            .chunks(2)
            .map(|p| u16::from_le_bytes([p[0], p[1]]))
            .collect();
        assert_eq!(String::from_utf16(&units).unwrap(), script);
        // Values only as literals: the command line is one quoted string.
        assert!(
            script.contains("$file = 'C:\\kb\\kitbag.exe'\n"),
            "{script}"
        );
        assert!(
            script.contains("$argline = '\"diff\" \"it''s $(x)\"'\n"),
            "{script}"
        );
        assert!(!script.contains("sh -s"));
        // Too long for cmd.exe: refused, not cut off.
        assert!(ps_command(&"x".repeat(4000)).is_err());
    }

    #[test]
    fn the_os_check_tells_windows_shells_from_posix_ones() {
        let says = |out: &str| reports_windows(&marks(out));
        // cmd.exe, PowerShell, and a POSIX shell, given DETECT.
        assert!(says("kitbag-remote:os=Windows_NT\\$env:OS\r\n"));
        assert!(says("banner\r\nkitbag-remote:os=%OS%\\Windows_NT\r\n"));
        assert!(!says("kitbag-remote:os=%OS%$env:OS\n"));
        assert!(!says(""));
        assert!(DETECT.starts_with("echo ") && !DETECT.contains(['\'', '"', '|', '&', ';']));
    }

    #[test]
    fn windows_temp_dirs_are_absolute_ours_and_plain() {
        let ok = r"C:\Users\you\AppData\Local\Temp\kitbag-remote.ab12CD34";
        assert!(check_tmp(ok, true).is_ok());
        assert!(check_tmp("C:/Users/you/AppData/Local/Temp/kitbag-remote.ab12", true).is_ok());
        for bad in [
            r"C:\Users\you\AppData\Local\Temp\other",
            r"C:\Users\you\..\kitbag-remote.ab12",
            r"Temp\kitbag-remote.ab12",
            r"\\server\share\kitbag-remote.ab12",
            "C:\\Temp\\kitbag-remote.ab12\n",
            r"C:\Temp\kitbag-remote.",
            r"C:\Temp\kitbag-remote.a'b",
            "/tmp/kitbag-remote.ab12",
        ] {
            assert!(check_tmp(bad, true).is_err(), "{bad:?}");
        }
        // A POSIX host still needs a leading `/`.
        assert!(check_tmp(ok, false).is_err());
    }

    #[test]
    fn tilde_paths_mean_the_windows_user_profile() {
        let home = r"C:\Users\you";
        assert_eq!(
            win_host_path("~/src/my app", home),
            r"C:\Users\you\src\my app"
        );
        assert_eq!(
            win_host_path(r"~\vault\s.age", home),
            r"C:\Users\you\vault\s.age"
        );
        assert_eq!(win_host_path(r"D:\work", home), r"D:\work");
        assert_eq!(
            Word::HostPath("~/id.txt".into()).windows(home),
            r"C:\Users\you\id.txt"
        );
        assert!(
            !Word::HostPath("~/id.txt".into())
                .windows(home)
                .contains("HOME")
        );
        assert_eq!(
            ps_path_expr("~/bin/kitbag.exe"),
            r"(Join-Path $env:USERPROFILE 'bin\kitbag.exe')"
        );
        assert_eq!(
            ps_path_expr(r"C:\it's\kitbag.exe"),
            r"'C:\it''s\kitbag.exe'"
        );
        assert!(
            win_probe_script(Some("~/kb.exe"))
                .contains("$explicit = (Join-Path $env:USERPROFILE 'kb.exe')\n")
        );
    }

    #[test]
    fn windows_downloads_use_the_msvc_zip_and_its_checksum() {
        assert_eq!(
            windows_artifacts("x86_64"),
            (
                "kitbag-x86_64-pc-windows-msvc.zip".to_string(),
                "kitbag-x86_64-pc-windows-msvc.sha256".to_string()
            )
        );
        assert_eq!(
            windows_artifacts("aarch64").0,
            "kitbag-aarch64-pc-windows-msvc.zip"
        );
        let s = download_script_windows(r"C:\T\kitbag-remote.x", "v1.0.0", RELEASE_BASE, false);
        for name in [
            "'kitbag-x86_64-pc-windows-msvc.zip'",
            "'kitbag-x86_64-pc-windows-msvc.sha256'",
            "'kitbag-aarch64-pc-windows-msvc.zip'",
            "'kitbag-aarch64-pc-windows-msvc.sha256'",
        ] {
            assert!(s.contains(name), "{name}");
        }
        assert!(!s.contains(".tar.gz") && !s.contains("musl"));
        assert!(s.contains("$local = $false\n"));
    }

    #[test]
    fn the_windows_download_checks_before_it_unpacks_or_keeps_anything() {
        let s = download_script_windows(r"C:\T\kitbag-remote.x", "v1.0.0", RELEASE_BASE, false);
        let at = |needle: &str| s.find(needle).unwrap_or_else(|| panic!("{needle}"));
        let checks = [
            "$expected -cnotmatch '^[0-9a-f]{64}$'",
            "$actual -cnotmatch '^[0-9a-f]{64}$'",
            "$expected -cne $actual",
        ];
        let unpack = at("Expand-Archive");
        let keep = at("New-Item -ItemType Directory -Force -Path $keepDir");
        let ran = at("& $run --version");
        for c in checks {
            assert!(at(c) < unpack, "{c}");
        }
        assert!(unpack < ran && ran < keep);
        // Only the keep step touches %LOCALAPPDATA%\kitbag, and only at the end.
        assert_eq!(s.matches(WIN_KEEP).count(), 1);
        assert!(at(WIN_KEEP) > ran);
        // Every check fails by Die, which throws: nothing after it runs.
        assert!(
            s.contains("function Die([string]$m) { Say 'error' $m; throw 'kitbag-remote-failed' }")
        );
        // No other hasher, no plain-HTTP fetch.
        assert!(!s.contains("MD5") && !s.contains("SHA1") && !s.contains("http://"));
        assert!(s.contains("$base.StartsWith('https://')"));
    }

    /// A PowerShell to run scripts with, if this machine has one.
    fn powershell() -> Option<&'static str> {
        ["powershell.exe", "pwsh"].into_iter().find(|p| {
            Command::new(p)
                .args(["-NoProfile", "-NonInteractive", "-Command", "exit 0"])
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status()
                .is_ok_and(|s| s.success())
        })
    }

    #[test]
    fn powershell_runs_quoted_values_as_text() {
        let Some(ps) = powershell() else {
            return;
        };
        for s in NASTY {
            // The value comes back Base64-encoded, so no console code page
            // is involved; nothing in it may have run.
            let script = format!(
                "[Console]::Out.Write([Convert]::ToBase64String([Text.Encoding]::UTF8.GetBytes({})))",
                ps_quote(s)
            );
            let cmd = ps_command(&script).unwrap();
            let mut words = cmd.split(' ');
            words.next();
            let out = Command::new(ps).args(words).output().unwrap();
            assert!(out.status.success(), "{s:?}: {out:?}");
            assert_eq!(
                String::from_utf8_lossy(&out.stdout).trim(),
                base64(s.as_bytes()),
                "{s:?}"
            );
        }
    }
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
        assert!(check_tmp("/tmp/kitbag-remote.AbC123", false).is_ok());
        assert!(check_tmp("/tmp/other", false).is_err());
        assert!(check_tmp("relative/kitbag-remote.x", false).is_err());
        assert!(check_tmp("/tmp/../kitbag-remote.x", false).is_err());
        assert!(check_tmp("/tmp/kitbag-remote.x\n/etc", false).is_err());
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
