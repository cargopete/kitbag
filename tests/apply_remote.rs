//! `apply --remote` / `diff --remote`, against a stub `ssh` that runs the remote
//! command on this machine with an empty environment and a throwaway home. No
//! network, no real ssh agent, no real home directory.

mod common;

/// A Windows machine drives `--remote` too: kitbag gets as far as running
/// `ssh` (here: none on PATH, so nothing reaches the network).
#[cfg(windows)]
mod windows_client {
    use predicates::prelude::*;
    use predicates::str::contains;

    use crate::common;

    fn no_ssh(sb: &common::Sandbox) -> std::path::PathBuf {
        let empty = sb.path("nobin");
        std::fs::create_dir_all(&empty).unwrap();
        empty
    }

    #[test]
    fn remote_runs_ssh_from_a_windows_machine() {
        let sb = common::Sandbox::empty();
        let prof = sb.path("prof");
        std::fs::create_dir_all(&prof).unwrap();
        std::fs::write(prof.join("kitbag.toml"), "kitbag = 1\nname = \"prof\"\n").unwrap();
        sb.cli()
            .env("PATH", no_ssh(&sb))
            .arg("diff")
            .arg(&prof)
            .args(["--remote", "you@example.com", "--to", "claude-code"])
            .assert()
            .failure()
            .stderr(contains("could not run `ssh`"))
            .stderr(contains("not supported on Windows").not());
    }

    #[test]
    fn undo_remote_runs_ssh_from_a_windows_machine() {
        let sb = common::Sandbox::empty();
        sb.cli()
            .env("PATH", no_ssh(&sb))
            .args(["undo", "--remote", "you@example.com"])
            .assert()
            .failure()
            .stderr(contains("could not run `ssh`"))
            .stderr(contains("not supported on Windows").not());
    }
}

/// The Windows host's scripts, run by this machine's own Windows PowerShell
/// exactly as ssh would hand them over (no ssh, no network). USERPROFILE,
/// LOCALAPPDATA and TEMP point into the sandbox.
#[cfg(windows)]
mod windows_powershell {
    use std::fs;
    use std::io::Write;
    use std::path::{Path, PathBuf};
    use std::process::{Command, Stdio};

    use kitbag::remote;
    use sha2::{Digest, Sha256};

    use crate::common;

    const BIN: &str = env!("CARGO_BIN_EXE_kitbag");

    struct Win {
        sb: common::Sandbox,
    }

    impl Win {
        fn new() -> Option<Self> {
            let ok = Command::new("powershell.exe")
                .args(["-NoProfile", "-NonInteractive", "-Command", "exit 0"])
                .status()
                .is_ok_and(|s| s.success());
            if !ok {
                eprintln!("skipped: no Windows PowerShell here");
                return None;
            }
            let sb = common::Sandbox::empty();
            for d in ["profile", "lad", "temp"] {
                fs::create_dir_all(sb.path(d)).unwrap();
            }
            Some(Win { sb })
        }

        fn keep(&self) -> PathBuf {
            self.sb.path("lad").join(r"kitbag\bin\kitbag.exe")
        }

        /// Run an ssh command (`powershell.exe ...`) with `input` on stdin.
        fn run(&self, command: &str, input: &[u8]) -> (bool, String) {
            let root = std::env::var("SystemRoot").unwrap_or_else(|_| r"C:\Windows".into());
            let mut words = command.split(' ');
            let mut child = Command::new(words.next().unwrap())
                .args(words)
                .env("USERPROFILE", self.sb.path("profile"))
                .env("LOCALAPPDATA", self.sb.path("lad"))
                .env("TEMP", self.sb.path("temp"))
                .env("TMP", self.sb.path("temp"))
                .env(
                    "PATH",
                    format!(r"{root}\System32;{root}\System32\WindowsPowerShell\v1.0"),
                )
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .spawn()
                .unwrap();
            let _ = child.stdin.take().unwrap().write_all(input);
            let out = child.wait_with_output().unwrap();
            (
                out.status.success(),
                String::from_utf8_lossy(&out.stdout).into_owned(),
            )
        }

        fn script(&self, script: &str) -> (bool, String) {
            let (command, payload) = remote::ps_script_invocation(script).unwrap();
            self.run(&command, payload.as_bytes())
        }
    }

    fn mark(out: &str, key: &str) -> Option<String> {
        out.lines()
            .filter_map(|l| l.strip_prefix(&format!("kitbag-remote:{key}=")))
            .map(|v| v.trim_end().to_string())
            .next()
    }

    #[test]
    fn probe_upload_run_and_cleanup_work_in_windows_powershell() {
        let Some(w) = Win::new() else { return };
        fs::create_dir_all(w.keep().parent().unwrap()).unwrap();
        fs::copy(BIN, w.keep()).unwrap();
        let (ok, out) = w.script(&remote::win_probe_script(None));
        assert!(ok, "{out}");
        assert_eq!(mark(&out, "os").as_deref(), Some("Windows_NT"), "{out}");
        let tmp = mark(&out, "tmp").expect("a temp dir");
        assert!(Path::new(&tmp).starts_with(w.sb.path("temp")), "{tmp}");
        assert!(
            Path::new(&tmp)
                .file_name()
                .unwrap()
                .to_string_lossy()
                .starts_with("kitbag-remote."),
            "{tmp}"
        );
        assert!(Path::new(&tmp).is_dir());
        assert_eq!(
            mark(&out, "kitbag").map(PathBuf::from),
            Some(w.keep()),
            "{out}"
        );
        assert!(mark(&out, "version").unwrap().starts_with("kitbag "));
        assert_eq!(
            mark(&out, "home").map(PathBuf::from),
            Some(w.sb.path("profile"))
        );

        // An explicit path that isn't there is an error, and its temp dir goes.
        let (ok, out2) = w.script(&remote::win_probe_script(Some("~/nowhere/kitbag.exe")));
        assert!(!ok);
        assert!(
            mark(&out2, "error")
                .unwrap()
                .contains("not an executable file"),
            "{out2}"
        );

        // Upload: the tar stream on stdin lands in a directory with an odd name.
        let staged = w.sb.path("staged");
        fs::create_dir_all(staged.join("rules")).unwrap();
        fs::write(staged.join("kitbag.toml"), "kitbag = 1\n").unwrap();
        fs::write(staged.join("rules/a.md"), "a").unwrap();
        let tar = Command::new("tar")
            .arg("-cf")
            .arg("-")
            .arg("-C")
            .arg(&staged)
            .arg(".")
            .output()
            .unwrap();
        assert!(tar.status.success());
        let dir = format!(r"{tmp}\it's $(x) prof");
        let up = remote::ps_command(&remote::win_upload_script(
            &dir,
            &format!(r"{tmp}\.upload.tar"),
        ))
        .unwrap();
        let (ok, out) = w.run(&up, &tar.stdout);
        assert!(ok, "{out}");
        assert_eq!(
            fs::read_to_string(Path::new(&dir).join("rules/a.md")).unwrap(),
            "a"
        );
        assert!(!Path::new(&tmp).join(".upload.tar").exists());

        // Run: kitbag gets exactly its arguments, output comes back.
        let keep = w.keep().display().to_string();
        let run =
            remote::ps_command(&remote::win_run_script(&keep, &["--version".into()])).unwrap();
        let (ok, out) = w.run(&run, b"");
        assert!(ok, "{out}");
        assert!(out.starts_with("kitbag "), "{out}");
        let run = remote::ps_command(&remote::win_run_script(
            &keep,
            &["no-such-command".into(), "it's \"q\" $(x)\\".into()],
        ))
        .unwrap();
        assert!(!w.run(&run, b"").0);

        // Cleanup removes it all, and nothing else.
        let clean = remote::ps_command(&remote::win_cleanup_script(&tmp)).unwrap();
        assert!(w.run(&clean, b"").0);
        assert!(!Path::new(&tmp).exists());
        assert!(w.keep().is_file());
    }

    fn arch() -> &'static str {
        if std::env::consts::ARCH == "aarch64" {
            "aarch64"
        } else {
            "x86_64"
        }
    }

    /// A release in `rel`: `kitbag.exe` zipped, and a `.sha256` holding
    /// `checksum(real hash)`.
    fn release(w: &Win, checksum: impl Fn(&str) -> String) -> PathBuf {
        let rel = w.sb.path("rel");
        let stage = w.sb.path("relstage");
        fs::create_dir_all(&rel).unwrap();
        fs::create_dir_all(&stage).unwrap();
        fs::copy(BIN, stage.join("kitbag.exe")).unwrap();
        let (zip, sum) = remote::windows_artifacts(arch());
        let ok = Command::new("powershell.exe")
            .args([
                "-NoProfile",
                "-NonInteractive",
                "-Command",
                "Compress-Archive -LiteralPath $env:KB_SRC -DestinationPath $env:KB_ZIP",
            ])
            .env("KB_SRC", stage.join("kitbag.exe"))
            .env("KB_ZIP", rel.join(&zip))
            .status()
            .unwrap()
            .success();
        assert!(ok);
        let real = hex::encode(Sha256::digest(fs::read(rel.join(&zip)).unwrap()));
        fs::write(rel.join(sum), checksum(&real)).unwrap();
        rel
    }

    fn download(w: &Win, rel: &Path) -> (bool, String, PathBuf) {
        let tmp = w.sb.path("temp").join("kitbag-remote.dltest");
        fs::create_dir_all(&tmp).unwrap();
        let script = remote::download_script_windows(
            &tmp.display().to_string(),
            "v9.9.9",
            &rel.display().to_string(),
            true,
        );
        let (ok, out) = w.script(&script);
        (ok, out, tmp)
    }

    #[test]
    fn a_windows_download_with_a_matching_checksum_is_kept() {
        let Some(w) = Win::new() else { return };
        let rel = release(&w, |h| format!("{h}  kitbag.zip\n"));
        let (ok, out, tmp) = download(&w, &rel);
        assert!(ok, "{out}");
        assert_eq!(
            mark(&out, "kitbag").map(PathBuf::from),
            Some(w.keep()),
            "{out}"
        );
        assert!(mark(&out, "version").unwrap().starts_with("kitbag "));
        fs::remove_dir_all(&tmp).unwrap();
        assert!(w.keep().is_file());
        let v = Command::new(w.keep()).arg("--version").output().unwrap();
        assert!(String::from_utf8_lossy(&v.stdout).starts_with("kitbag "));
        // Again: the kept copy is replaced in place.
        let (ok, out, _) = download(&w, &rel);
        assert!(ok, "{out}");
        assert!(w.keep().is_file());
        let leftovers: Vec<_> = fs::read_dir(w.keep().parent().unwrap())
            .unwrap()
            .map(|e| e.unwrap().file_name())
            .collect();
        assert_eq!(leftovers, vec![std::ffi::OsString::from("kitbag.exe")]);
    }

    #[test]
    fn a_windows_download_with_a_bad_or_missing_checksum_is_never_unpacked_or_kept() {
        for bad in [
            Some("0000000000000000000000000000000000000000000000000000000000000000  x\n"),
            Some("not-a-checksum\n"),
            Some(""),
            None,
        ] {
            let Some(w) = Win::new() else { return };
            let rel = release(&w, |_| bad.unwrap_or("").to_string());
            if bad.is_none() {
                fs::remove_file(rel.join(remote::windows_artifacts(arch()).1)).unwrap();
            }
            let (ok, out, tmp) = download(&w, &rel);
            assert!(!ok, "{bad:?}: {out}");
            assert!(mark(&out, "error").is_some(), "{bad:?}: {out}");
            assert!(!tmp.join(r"download\unpacked").exists(), "{bad:?}");
            assert!(!tmp.join(r"bin\kitbag.exe").exists(), "{bad:?}");
            assert!(!w.sb.path("lad").join("kitbag").exists(), "{bad:?}: kept");
        }
    }
}

#[cfg(unix)]
mod unix {
    use std::fs;
    use std::os::unix::fs::PermissionsExt;
    use std::path::{Path, PathBuf};
    use std::process::Command;

    use predicates::prelude::*;
    use predicates::str::contains;

    use crate::common::*;

    const BIN: &str = env!("CARGO_BIN_EXE_kitbag");
    const FAKE_TOKEN: &str = "fake-local-token-value-1234";

    /// A stub ssh and the "remote host" it runs commands on.
    struct Host {
        sb: Sandbox,
    }

    impl Host {
        fn new() -> Self {
            let sb = Sandbox::empty();
            for d in ["rhome", "rtmp", "fakebin", "rpath"] {
                fs::create_dir_all(sb.path(d)).unwrap();
            }
            let script = format!(
                r#"#!/bin/sh
printf '%s\n' "$*" >> '{calls}'
[ "$1" = "--" ] || {{ echo "stub ssh: expected -- before the host" >&2; exit 255; }}
host="$2"
cmd="$3"
case "$host" in
  *down*) echo "ssh: Could not resolve hostname $host" >&2; exit 255 ;;
esac
run() {{ env -i PATH='{rpath}' HOME='{rhome}' TMPDIR='{rtmp}' sh -c "$cmd"; }}
case "$cmd" in
  "mkdir -p"*) tee '{upload}' | run ;;
  *) run ;;
esac
"#,
                calls = sb.path("calls.log").display(),
                rpath = remote_path(&sb),
                rhome = sb.path("rhome").display(),
                rtmp = sb.path("rtmp").display(),
                upload = sb.path("upload.tar").display(),
            );
            let ssh = sb.path("fakebin/ssh");
            fs::write(&ssh, script).unwrap();
            fs::set_permissions(&ssh, fs::Permissions::from_mode(0o755)).unwrap();
            Host { sb }
        }

        fn rhome(&self) -> PathBuf {
            self.sb.path("rhome")
        }

        fn ssh(&self) -> PathBuf {
            self.sb.path("fakebin/ssh")
        }

        /// kitbag on the local side with the stub ssh first on PATH and a
        /// local secret in its environment.
        fn cli(&self) -> assert_cmd::Command {
            let mut c = self.sb.cli();
            let path = format!(
                "{}:{}",
                self.sb.path("fakebin").display(),
                std::env::var("PATH").unwrap_or_default()
            );
            c.env("PATH", path).env("TOKEN", FAKE_TOKEN);
            c
        }

        fn calls(&self) -> String {
            fs::read_to_string(self.sb.path("calls.log")).unwrap_or_default()
        }

        /// kitbag as it runs on the remote host.
        fn on_host(&self, args: &[&str]) -> std::process::Output {
            Command::new(BIN)
                .args(args)
                .env_clear()
                .env("HOME", self.rhome())
                .env("PATH", "/usr/bin:/bin")
                .env("NO_COLOR", "1")
                .output()
                .unwrap()
        }

        fn leftovers(&self) -> Vec<String> {
            fs::read_dir(self.sb.path("rtmp"))
                .unwrap()
                .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
                .collect()
        }
    }

    fn remote_path(sb: &Sandbox) -> String {
        format!("{}:/usr/bin:/bin", sb.path("rpath").display())
    }

    fn profile(host: &Host) -> PathBuf {
        let dir = host.sb.path("prof");
        fs::create_dir_all(dir.join("rules")).unwrap();
        fs::write(
            dir.join("kitbag.toml"),
            "kitbag = 1\nname = \"prof\"\n\n[mcp_servers.docs]\nurl = \"https://docs.example.com/mcp\"\nheaders = { Authorization = \"Bearer ${TOKEN}\" }\n",
        )
        .unwrap();
        fs::write(dir.join("instructions.md"), "Be brief on the remote.\n").unwrap();
        fs::write(dir.join("rules/style.md"), "Use tabs.\n").unwrap();
        dir
    }

    fn with_kitbag(host: &Host) {
        std::os::unix::fs::symlink(BIN, host.sb.path("rpath/kitbag")).unwrap();
    }

    fn remote_args<'a>(extra: &[&'a str]) -> Vec<&'a str> {
        let mut v = vec![
            "--remote",
            "you@stub-host",
            "--remote-kitbag",
            BIN,
            "--to",
            "claude-code",
        ];
        v.extend_from_slice(extra);
        v
    }

    fn tree_text(dir: &Path) -> String {
        let mut out = String::new();
        if let Ok(rd) = fs::read_dir(dir) {
            for e in rd.flatten() {
                if e.path().is_dir() {
                    out.push_str(&tree_text(&e.path()));
                } else if let Ok(t) = fs::read_to_string(e.path()) {
                    out.push_str(&t);
                }
            }
        }
        out
    }

    #[test]
    fn apply_remote_writes_on_the_host_keeps_refs_and_undo_is_there() {
        let host = Host::new();
        let prof = profile(&host);
        host.cli()
            .arg("apply")
            .arg(&prof)
            .args(remote_args(&["--yes"]))
            .assert()
            .success()
            .stdout(contains("you@stub-host").and(contains("Wrote")));

        // Written on the host, not here.
        let claude = host.rhome().join(".claude/CLAUDE.md");
        assert!(fs::read_to_string(claude).unwrap().contains("Be brief"));
        assert!(host.rhome().join(".claude/rules/style.md").exists());
        assert!(!host.sb.home().join(".claude").exists());
        assert!(!host.sb.state().exists());

        // The secret's value was never sent or written; the reference was.
        let wire = fs::read(host.sb.path("upload.tar")).unwrap();
        let wire = String::from_utf8_lossy(&wire).into_owned();
        assert!(wire.contains("${TOKEN}"));
        assert!(!wire.contains(FAKE_TOKEN));
        assert!(!host.calls().contains(FAKE_TOKEN));
        let written = tree_text(&host.rhome());
        assert!(written.contains("${TOKEN}"), "{written}");
        assert!(!written.contains(FAKE_TOKEN));

        // The host's own state directory has the transaction, and what was
        // copied there is gone.
        assert!(host.rhome().join(".local/state/kitbag").exists());
        assert_eq!(host.leftovers(), Vec::<String>::new());

        // Undo on the host reverts it.
        let out = host.on_host(&["undo"]);
        assert!(out.status.success(), "{out:?}");
        assert!(!host.rhome().join(".claude/CLAUDE.md").exists());
    }

    #[test]
    fn diff_remote_never_writes_and_passes_the_exit_status_on() {
        let host = Host::new();
        let prof = profile(&host);
        host.cli()
            .arg("diff")
            .arg(&prof)
            .args(remote_args(&["--exit-code"]))
            .assert()
            .code(1)
            .stdout(contains("Be brief on the remote."));
        assert!(!host.rhome().join(".claude").exists());
        assert_eq!(host.leftovers(), Vec::<String>::new());

        host.cli()
            .arg("apply")
            .arg(&prof)
            .args(remote_args(&["--yes"]))
            .assert()
            .success();
        host.cli()
            .arg("diff")
            .arg(&prof)
            .args(remote_args(&["--exit-code"]))
            .assert()
            .success();
    }

    #[test]
    fn dry_run_remote_writes_nothing() {
        let host = Host::new();
        let prof = profile(&host);
        host.cli()
            .arg("apply")
            .arg(&prof)
            .args(remote_args(&["--dry-run"]))
            .assert()
            .success()
            .stdout(contains("nothing written"));
        assert!(!host.rhome().join(".claude").exists());
    }

    #[test]
    fn without_yes_a_non_interactive_apply_refuses_before_any_ssh() {
        let host = Host::new();
        let prof = profile(&host);
        host.cli()
            .arg("apply")
            .arg(&prof)
            .args(remote_args(&[]))
            .assert()
            .code(2)
            .stderr(contains("pass --yes"));
        assert_eq!(host.calls(), "");
    }

    #[test]
    fn the_plan_is_shown_first_and_declining_changes_nothing() {
        let host = Host::new();
        let prof = profile(&host);
        let remote = kitbag::remote::Remote {
            ssh: host.ssh().display().to_string(),
            host: "you@stub-host".into(),
            kitbag: Some(BIN.into()),
            download: false,
            secrets: None,
            project: None,
        };
        let args = vec!["--to".to_string(), "claude-code".to_string()];
        let asked = std::cell::Cell::new(false);
        let code = kitbag::remote::run(
            &remote,
            &prof,
            "prof",
            &args,
            kitbag::remote::Flow::Apply {
                yes: false,
                dry_run: false,
            },
            &|_| {
                asked.set(true);
                // By now the plan was made but nothing was written.
                assert!(!host.rhome().join(".claude").exists());
                Ok(false)
            },
        )
        .unwrap();
        assert!(asked.get());
        assert_eq!(code, 1);
        assert!(!host.rhome().join(".claude").exists());
        assert_eq!(host.leftovers(), Vec::<String>::new());

        let code = kitbag::remote::run(
            &remote,
            &prof,
            "prof",
            &args,
            kitbag::remote::Flow::Apply {
                yes: false,
                dry_run: false,
            },
            &|_| Ok(true),
        )
        .unwrap();
        assert_eq!(code, 0);
        assert!(host.rhome().join(".claude/CLAUDE.md").exists());
        assert_eq!(host.leftovers(), Vec::<String>::new());
    }

    #[test]
    fn a_literal_secret_in_the_profile_is_refused_and_nothing_is_sent() {
        let host = Host::new();
        let prof = profile(&host);
        let leaked = "ghp_aB3dE5gH7jK9mN1pQ3sT5vW7yZ9bC1dE3fG5";
        fs::write(
            prof.join("kitbag.toml"),
            format!(
                "kitbag = 1\nname = \"prof\"\n\n[mcp_servers.gh]\ncommand = \"gh-mcp\"\nenv = {{ GITHUB_TOKEN = \"{leaked}\" }}\n"
            ),
        )
        .unwrap();
        host.cli()
            .arg("apply")
            .arg(&prof)
            .args(remote_args(&["--yes"]))
            .assert()
            .failure()
            .stderr(contains("literal secret").and(contains(leaked).not()));
        assert_eq!(host.calls(), "");
    }

    #[test]
    fn env_and_git_files_are_not_sent() {
        let host = Host::new();
        let prof = profile(&host);
        fs::write(prof.join(".env"), "TOKEN=fake-env-file-value\n").unwrap();
        fs::create_dir_all(prof.join(".git")).unwrap();
        fs::write(prof.join(".git/config"), "[core]\n").unwrap();
        host.cli()
            .arg("apply")
            .arg(&prof)
            .args(remote_args(&["--yes"]))
            .assert()
            .success();
        let listing = Command::new("tar")
            .arg("-tf")
            .arg(host.sb.path("upload.tar"))
            .output()
            .unwrap();
        let listing = String::from_utf8_lossy(&listing.stdout).into_owned();
        assert!(listing.contains("kitbag.toml"), "{listing}");
        assert!(!listing.contains(".env"), "{listing}");
        assert!(!listing.contains(".git"), "{listing}");
    }

    #[test]
    fn a_kitbag_on_the_hosts_path_or_in_local_bin_is_used() {
        for local_bin in [false, true] {
            let host = Host::new();
            let prof = profile(&host);
            if local_bin {
                let dir = host.rhome().join(".local/bin");
                fs::create_dir_all(&dir).unwrap();
                std::os::unix::fs::symlink(BIN, dir.join("kitbag")).unwrap();
            } else {
                with_kitbag(&host);
            }
            host.cli()
                .arg("apply")
                .arg(&prof)
                .args(["--remote", "you@stub-host", "--to", "claude-code", "--yes"])
                .assert()
                .success()
                .stdout(contains("kitbag there:"));
            assert!(host.rhome().join(".claude/CLAUDE.md").exists());
        }
    }

    #[test]
    fn no_kitbag_on_the_host_stops_without_downloading_anything() {
        let host = Host::new();
        let prof = profile(&host);
        host.cli()
            .arg("apply")
            .arg(&prof)
            .args(["--remote", "you@stub-host", "--to", "claude-code", "--yes"])
            .assert()
            .failure()
            .stderr(contains("not installed").and(contains("--remote-download")));
        let calls = host.calls();
        assert!(
            !calls.contains("curl") && !calls.contains("wget"),
            "{calls}"
        );
        assert!(!host.rhome().join(".claude").exists());
        assert_eq!(host.leftovers(), Vec::<String>::new());
    }

    #[test]
    fn an_explicit_kitbag_that_is_not_there_is_an_error() {
        let host = Host::new();
        let prof = profile(&host);
        host.cli()
            .arg("diff")
            .arg(&prof)
            .args([
                "--remote",
                "you@stub-host",
                "--remote-kitbag",
                "/nonexistent/kitbag",
            ])
            .assert()
            .failure()
            .stderr(contains("--remote-kitbag"));
        assert_eq!(host.leftovers(), Vec::<String>::new());
    }

    #[test]
    fn an_unreachable_host_is_reported_as_an_ssh_failure() {
        let host = Host::new();
        let prof = profile(&host);
        host.cli()
            .arg("diff")
            .arg(&prof)
            .args(["--remote", "down.example", "--remote-kitbag", BIN])
            .assert()
            .failure()
            .stderr(contains("ssh to down.example failed"));
    }

    #[test]
    fn hosts_that_look_like_ssh_options_are_refused_before_ssh_runs() {
        let host = Host::new();
        let prof = profile(&host);
        host.cli()
            .arg("diff")
            .arg(&prof)
            .arg("--remote=-oProxyCommand=touch-x")
            .assert()
            .failure()
            .stderr(contains("looks like an ssh option"));
        assert_eq!(host.calls(), "");
    }

    const GIT_FILES: [(&str, &str); 2] = [
        (
            "kitbag.toml",
            "kitbag = 1\nname = \"gitprof\"\n\n[mcp_servers.docs]\nurl = \"https://docs.example.com/mcp\"\n",
        ),
        ("instructions.md", "Be brief from git.\n"),
    ];

    fn git_remote(host: &Host) -> (String, String) {
        let url = host.sb.git_profile("gitprof", &GIT_FILES);
        let sha = git_out(&host.sb.path("gitprof"), &["rev-parse", "HEAD"]);
        (url, sha.trim().to_string())
    }

    #[test]
    fn apply_remote_from_a_git_url_records_the_url_and_commit_on_the_host() {
        if !has_git() {
            return;
        }
        let host = Host::new();
        let (url, sha) = git_remote(&host);
        host.cli()
            .arg("apply")
            .arg(&url)
            .args(remote_args(&["--yes", "--trust-source"]))
            .assert()
            .success()
            .stdout(contains("Wrote"));
        let claude = host.rhome().join(".claude/CLAUDE.md");
        assert!(
            fs::read_to_string(claude)
                .unwrap()
                .contains("Be brief from git.")
        );
        assert!(tree_text(&host.rhome()).contains("docs.example.com"));
        assert_eq!(host.leftovers(), Vec::<String>::new());
        assert!(!host.sb.state().exists());

        let out = host.on_host(&["history", "--json"]);
        assert!(out.status.success(), "{out:?}");
        let history = String::from_utf8_lossy(&out.stdout).into_owned();
        assert!(history.contains(&url), "{history}");
        assert!(history.contains(&sha[..7]), "{history}");
        assert!(!history.contains("kitbag-remote."), "{history}");
        assert!(!history.contains("rtmp"), "{history}");
        let records = tree_text(&host.rhome().join(".local/state/kitbag"));
        assert!(!records.contains("kitbag-remote."), "{records}");
        assert!(records.contains(&url), "{records}");
        assert!(records.contains(&sha), "{records}");

        // The recorded source is one `status` and `sync` accept there, and
        // it is the commit applied, so nothing has drifted.
        let pinned = format!("{url}@{sha}");
        let out = host.on_host(&["status", &pinned, "--to", "claude-code", "--exit-code"]);
        assert!(out.status.success(), "{out:?}");
        let out = host.on_host(&["sync", &pinned, "--to", "claude-code", "--dry-run"]);
        assert!(out.status.success(), "{out:?}");

        // Undo there still works.
        let out = host.on_host(&["undo"]);
        assert!(out.status.success(), "{out:?}");
        assert!(!host.rhome().join(".claude/CLAUDE.md").exists());
    }

    #[test]
    fn a_git_source_without_trust_is_as_untrusted_on_the_host_as_it_is_here() {
        if !has_git() {
            return;
        }
        let host = Host::new();
        let (url, sha) = git_remote(&host);
        host.cli()
            .arg("apply")
            .arg(&url)
            .args(remote_args(&["--yes"]))
            .assert()
            .success()
            .stdout(contains("not applied"));
        // Instructions go in; the MCP server of a git source is withheld.
        assert!(host.rhome().join(".claude/CLAUDE.md").exists());
        assert!(!tree_text(&host.rhome()).contains("docs.example.com"));
        let history =
            String::from_utf8_lossy(&host.on_host(&["history", "--json"]).stdout).into_owned();
        assert!(history.contains(&sha[..7]), "{history}");
    }

    #[test]
    fn git_pin_and_cleartext_rules_are_checked_here_before_anything_is_sent() {
        if !has_git() {
            return;
        }
        let host = Host::new();
        let (url, _) = git_remote(&host);
        host.cli()
            .arg("diff")
            .arg(&url)
            .args(remote_args(&["--require-pin"]))
            .assert()
            .failure()
            .stderr(contains("not pinned"));
        host.cli()
            .arg("diff")
            .arg("http://127.0.0.1:9/x.git")
            .args(remote_args(&[]))
            .assert()
            .failure()
            .stderr(contains("without encryption"));
        assert_eq!(host.calls(), "");
    }

    #[test]
    fn a_secrets_bundle_is_never_taken_from_a_git_source() {
        if !has_git() {
            return;
        }
        let host = Host::new();
        let (url, _) = git_remote(&host);
        host.cli()
            .arg("diff")
            .arg(&url)
            .args(remote_args(&[
                "--remote-include-secrets-age",
                "--remote-secrets-identity",
                "/h/id",
            ]))
            .assert()
            .failure()
            .stderr(contains("git source"));
        host.cli()
            .arg("diff")
            .arg(&url)
            .args(remote_args(&[
                "--remote-secrets-from",
                "/h/s.age",
                "--remote-secrets-identity",
                "/h/id",
            ]))
            .assert()
            .failure()
            .stderr(contains("git source"));
        host.cli()
            .arg("diff")
            .arg(&url)
            .args(remote_args(&["--secrets-from", "x.age"]))
            .assert()
            .failure();
        assert_eq!(host.calls(), "");
    }

    #[test]
    fn a_git_source_with_remote_project_applies_at_the_host_project() {
        if !has_git() {
            return;
        }
        let host = Host::new();
        let (url, sha) = git_remote(&host);
        let proj = host.sb.path("hostproj");
        fs::create_dir_all(&proj).unwrap();
        host.cli()
            .arg("apply")
            .arg(&url)
            .args(remote_args(&["--yes", "--remote-project"]))
            .arg(&proj)
            .assert()
            .success();
        assert!(proj.join("CLAUDE.md").exists() || proj.join(".claude").exists());
        let history =
            String::from_utf8_lossy(&host.on_host(&["history", "--json"]).stdout).into_owned();
        assert!(history.contains(&sha[..7]), "{history}");
    }

    #[test]
    fn the_hidden_copy_flags_need_a_real_git_url_and_commit() {
        let host = Host::new();
        let prof = profile(&host);
        let p = prof.to_str().unwrap();
        for (src, sha) in [
            ("/etc", "0123456789012345678901234567890123456789"),
            (
                "https://u:pw@example.com/o/r.git",
                "0123456789012345678901234567890123456789",
            ),
            ("https://example.com/o/r.git", "abc"),
        ] {
            let out = host.on_host(&[
                "diff",
                p,
                "--to",
                "claude-code",
                "--copy-of-git",
                src,
                "--copy-of-commit",
                sha,
            ]);
            assert!(!out.status.success(), "{src}: {out:?}");
        }
    }

    #[test]
    fn project_scope_and_incompatible_flags_are_refused() {
        let host = Host::new();
        let prof = profile(&host);
        host.cli()
            .arg("diff")
            .arg(&prof)
            .args(["--remote", "you@stub-host", "--scope", "project"])
            .assert()
            .failure()
            .stderr(contains("--remote-project"));
        host.cli()
            .arg("diff")
            .arg(&prof)
            .args([
                "--remote",
                "you@stub-host",
                "--remote-download",
                "--remote-kitbag",
                BIN,
            ])
            .assert()
            .code(2);
        host.cli()
            .arg("diff")
            .arg(&prof)
            .arg("--remote-download")
            .assert()
            .code(2);
        assert_eq!(host.calls(), "");
    }

    #[test]
    fn ssh_gets_no_options_of_kitbags_own() {
        let host = Host::new();
        let prof = profile(&host);
        host.cli()
            .arg("diff")
            .arg(&prof)
            .args(remote_args(&[]))
            .assert()
            .success();
        for line in host.calls().lines() {
            assert!(line.starts_with("-- you@stub-host "), "{line}");
        }
    }

    // ---- Project paths on the host (#146).

    fn host_project(host: &Host) -> PathBuf {
        let p = host.rhome().join("work/myapp");
        fs::create_dir_all(&p).unwrap();
        p
    }

    #[test]
    fn remote_project_writes_project_scope_files_under_that_host_path() {
        let host = Host::new();
        let prof = profile(&host);
        let proj = host_project(&host);
        let p = proj.display().to_string();
        host.cli()
            .arg("apply")
            .arg(&prof)
            .args(remote_args(&["--remote-project", &p, "--yes"]))
            .assert()
            .success()
            .stdout(contains("Project scope").and(contains("Wrote")));
        let written = tree_text(&proj);
        assert!(written.contains("Be brief on the remote."), "{written}");
        assert!(proj.join(".claude/rules/style.md").exists());
        // Not user scope, not this machine.
        assert!(!host.rhome().join(".claude/CLAUDE.md").exists());
        assert!(!host.rhome().join(".claude/rules").exists());
        assert!(!host.sb.home().join(".claude").exists());
        assert_eq!(host.leftovers(), Vec::<String>::new());
        assert!(host.calls().contains("'--project'"), "{}", host.calls());

        // The host's own undo reverts it.
        let out = host.on_host(&["undo"]);
        assert!(out.status.success(), "{out:?}");
        assert!(!proj.join(".claude/rules/style.md").exists());
    }

    #[test]
    fn undo_remote_reverts_a_project_scope_apply() {
        let host = Host::new();
        let prof = profile(&host);
        let proj = host_project(&host);
        let p = proj.display().to_string();
        host.cli()
            .arg("apply")
            .arg(&prof)
            .args(remote_args(&["--remote-project", &p, "--yes"]))
            .assert()
            .success();
        assert!(proj.join(".claude/rules/style.md").exists());
        host.cli()
            .args(["undo", "--remote", "you@stub-host", "--remote-kitbag", BIN])
            .assert()
            .success();
        assert!(!proj.join(".claude/rules/style.md").exists());
    }

    #[test]
    fn diff_remote_project_plans_against_that_path_and_writes_nothing() {
        let host = Host::new();
        let prof = profile(&host);
        let proj = host_project(&host);
        let p = proj.display().to_string();
        host.cli()
            .arg("diff")
            .arg(&prof)
            .args(remote_args(&["--remote-project", &p]))
            .assert()
            .success()
            .stdout(contains("style.md"));
        assert_eq!(fs::read_dir(&proj).unwrap().count(), 0);
        assert!(!host.rhome().join(".claude").exists());
        assert_eq!(host.leftovers(), Vec::<String>::new());
    }

    #[test]
    fn local_project_flags_are_refused_with_remote_and_point_at_remote_project() {
        let host = Host::new();
        let prof = profile(&host);
        let here = host.sb.path("localproj");
        fs::create_dir_all(&here).unwrap();
        for extra in [
            vec!["--project", here.to_str().unwrap()],
            vec!["--scope", "project"],
        ] {
            host.cli()
                .arg("diff")
                .arg(&prof)
                .args(remote_args(&extra))
                .assert()
                .failure()
                .stderr(contains("--remote-project"));
        }
        host.cli()
            .arg("diff")
            .arg(&prof)
            .args(remote_args(&["--remote-project", "/x", "--scope", "user"]))
            .assert()
            .failure();
        // Needs --remote.
        host.cli()
            .arg("diff")
            .arg(&prof)
            .args(["--remote-project", "/x"])
            .assert()
            .code(2);
        assert_eq!(host.calls(), "");
    }

    #[test]
    fn remote_project_that_looks_like_an_option_is_refused_before_ssh() {
        let host = Host::new();
        let prof = profile(&host);
        for bad in ["", "-oProxyCommand=x", "--home", "/a\nb"] {
            host.cli()
                .arg("diff")
                .arg(&prof)
                .args(remote_args(&["--remote-project", bad]))
                .assert()
                .failure();
        }
        assert_eq!(host.calls(), "");
    }

    #[test]
    fn remote_project_tilde_means_the_host_home_and_odd_names_are_quoted() {
        let host = Host::new();
        let prof = profile(&host);
        let odd = host.rhome().join("it's $(touch pwned) app");
        fs::create_dir_all(&odd).unwrap();
        host.cli()
            .arg("apply")
            .arg(&prof)
            .args(remote_args(&[
                "--remote-project",
                "~/it's $(touch pwned) app",
                "--yes",
            ]))
            .assert()
            .success();
        assert!(odd.join(".claude/rules/style.md").exists());
        assert!(!host.rhome().join("pwned").exists());
        assert!(!Path::new("pwned").exists());
    }

    #[test]
    fn local_home_is_never_sent_as_the_hosts_home() {
        let host = Host::new();
        let prof = profile(&host);
        let proj = host_project(&host);
        let p = proj.display().to_string();
        let mine = host.sb.path("my-local-home");
        fs::create_dir_all(&mine).unwrap();
        host.cli()
            .env("KITBAG_HOME", &mine)
            .arg("apply")
            .arg(&prof)
            .args(remote_args(&["--remote-project", &p, "--yes"]))
            .arg("--home")
            .arg(&mine)
            .assert()
            .success();
        let calls = host.calls();
        assert!(!calls.contains("--home"), "{calls}");
        assert!(!calls.contains("my-local-home"), "{calls}");
        assert!(proj.join(".claude/rules/style.md").exists());
        assert!(!mine.join(".claude").exists());
    }

    // ---- Secrets bundles opened on the host (#139). Every value, passphrase
    // and key below is made up.

    const BUNDLE_VALUE: &str = "fake-bundle-value-for-remote-0139";
    const HOST_PASS: &str = "fake-host-passphrase-not-real";

    fn apply_yes(host: &Host, prof: &Path) {
        host.cli()
            .arg("apply")
            .arg(prof)
            .args(remote_args(&["--yes"]))
            .assert()
            .success();
    }

    fn undo_args<'a>(extra: &[&'a str]) -> Vec<&'a str> {
        let mut v = vec!["undo", "--remote", "you@stub-host", "--remote-kitbag", BIN];
        v.extend_from_slice(extra);
        v
    }

    /// Ids of the applies on the host that can still be undone.
    fn history_ids(host: &Host) -> Vec<String> {
        let out = host.on_host(&["history", "--json"]);
        let v: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
        v.as_array()
            .unwrap()
            .iter()
            .filter(|e| e["status"] == "applied")
            .map(|e| e["id"].as_str().unwrap().to_string())
            .collect()
    }

    #[test]
    fn undo_remote_reverts_the_hosts_most_recent_apply() {
        let host = Host::new();
        let prof = profile(&host);
        apply_yes(&host, &prof);
        assert!(host.rhome().join(".claude/CLAUDE.md").exists());
        assert_eq!(history_ids(&host).len(), 1);
        let uploads_before = fs::metadata(host.sb.path("upload.tar")).unwrap().len();

        host.cli()
            .args(undo_args(&[]))
            .assert()
            .success()
            .stdout(contains("Undid"));
        assert!(!host.rhome().join(".claude/CLAUDE.md").exists());
        assert!(!host.rhome().join(".claude/rules/style.md").exists());
        // Nothing was uploaded, nothing is left in the host's tmp, and the
        // transaction is no longer undoable.
        assert_eq!(
            fs::metadata(host.sb.path("upload.tar")).unwrap().len(),
            uploads_before
        );
        assert!(!host.calls().lines().last().unwrap().contains("tar -xf"));
        assert_eq!(host.leftovers(), Vec::<String>::new());
        assert_eq!(history_ids(&host).len(), 0);
        assert!(!host.sb.home().join(".claude").exists());
        assert!(!host.sb.state().exists());
    }

    #[test]
    fn undo_remote_takes_an_id_and_force_like_local_undo() {
        let host = Host::new();
        let prof = profile(&host);
        apply_yes(&host, &prof);
        let first = history_ids(&host).remove(0);
        fs::write(prof.join("instructions.md"), "Second apply.\n").unwrap();
        apply_yes(&host, &prof);
        assert_eq!(history_ids(&host).len(), 2);

        // The older apply can't be undone while a newer one exists...
        host.cli().args(undo_args(&[&first])).assert().failure();
        assert!(host.rhome().join(".claude/CLAUDE.md").exists());
        // ...unless forced.
        host.cli()
            .args(undo_args(&["--force", &first]))
            .assert()
            .success()
            .stdout(contains("Undid"));
        assert_eq!(history_ids(&host).len(), 1);
        // Without an id, the most recent one is next; the forced undo changed
        // its files, so the host asks for --force again.
        host.cli().args(undo_args(&[])).assert().failure();
        host.cli().args(undo_args(&["--force"])).assert().success();
        assert_eq!(history_ids(&host).len(), 0);
        assert_eq!(host.leftovers(), Vec::<String>::new());
    }

    #[test]
    fn undo_remote_passes_the_hosts_failure_on() {
        let host = Host::new();
        host.cli()
            .args(undo_args(&[]))
            .assert()
            .failure()
            .stdout(predicates::str::contains("Undid").not());
        assert_eq!(host.leftovers(), Vec::<String>::new());
    }

    #[test]
    fn a_kitbag_kept_in_the_data_dir_is_found_by_apply_and_undo() {
        let host = Host::new();
        let prof = profile(&host);
        let dir = host.rhome().join(".local/share/kitbag/bin");
        fs::create_dir_all(&dir).unwrap();
        std::os::unix::fs::symlink(BIN, dir.join("kitbag")).unwrap();
        host.cli()
            .arg("apply")
            .arg(&prof)
            .args(["--remote", "you@stub-host", "--to", "claude-code", "--yes"])
            .assert()
            .success()
            .stdout(
                contains(".local/share/kitbag/bin/kitbag").and(contains("kitbag undo --remote")),
            );
        assert!(host.rhome().join(".claude/CLAUDE.md").exists());
        host.cli()
            .args(["undo", "--remote", "you@stub-host"])
            .assert()
            .success()
            .stdout(contains("Undid"));
        assert!(!host.rhome().join(".claude/CLAUDE.md").exists());
        assert_eq!(host.leftovers(), Vec::<String>::new());
    }

    #[test]
    fn undo_remote_without_a_kitbag_fails_clearly_and_leaves_nothing() {
        let host = Host::new();
        host.cli()
            .args(["undo", "--remote", "you@stub-host"])
            .assert()
            .failure()
            .stderr(
                contains("not installed")
                    .and(contains("--remote-kitbag"))
                    .and(contains("--remote-download")),
            );
        let calls = host.calls();
        assert!(
            !calls.contains("curl") && !calls.contains("wget"),
            "{calls}"
        );
        assert_eq!(host.leftovers(), Vec::<String>::new());
        assert!(!host.rhome().join(".local").exists());
    }

    #[test]
    fn undo_remote_refuses_option_like_hosts_and_stray_flags_before_ssh() {
        let host = Host::new();
        host.cli()
            .args(["undo", "--remote=-oProxyCommand=touch-x"])
            .assert()
            .failure()
            .stderr(contains("looks like an ssh option"));
        host.cli()
            .args(["undo", "--remote-download"])
            .assert()
            .failure();
        host.cli()
            .args(["undo", "--remote", "h", "--secrets-from", "x"])
            .assert()
            .failure();
        assert_eq!(host.calls(), "");
    }

    #[test]
    fn undo_remote_sends_no_local_secret_anywhere() {
        let host = Host::new();
        let prof = profile(&host);
        apply_yes(&host, &prof);
        let out = host
            .cli()
            .args(undo_args(&[]))
            .assert()
            .success()
            .get_output()
            .clone();
        assert!(!host.calls().contains(FAKE_TOKEN));
        assert!(!String::from_utf8_lossy(&out.stdout).contains(FAKE_TOKEN));
        assert!(!String::from_utf8_lossy(&out.stderr).contains(FAKE_TOKEN));
        let wire = fs::read(host.sb.path("upload.tar")).unwrap();
        assert!(!String::from_utf8_lossy(&wire).contains(FAKE_TOKEN));
        assert!(!tree_text(&host.sb.path("rtmp")).contains(FAKE_TOKEN));
        assert!(!tree_text(&host.rhome()).contains(FAKE_TOKEN));
        assert!(!tree_text(&host.sb.home()).contains(FAKE_TOKEN));
    }

    fn bundle_profile(host: &Host) -> PathBuf {
        let dir = host.sb.path("bprof");
        fs::create_dir_all(&dir).unwrap();
        fs::write(
            dir.join("kitbag.toml"),
            "kitbag = 1\nname = \"bprof\"\n\n[mcp_servers.github]\ncommand = \"npx\"\nargs = [\"-y\", \"server-github\"]\nenv = { GITHUB_TOKEN = \"${GITHUB_TOKEN}\" }\n",
        )
        .unwrap();
        dir
    }

    fn sealed(seal: &kitbag::bundle::Seal) -> Vec<u8> {
        let mut b = kitbag::bundle::Bundle::default();
        b.insert(
            "GITHUB_TOKEN",
            kitbag::bundle::Secret::new(BUNDLE_VALUE),
            "test",
        )
        .unwrap();
        kitbag::bundle::seal(&b, seal).unwrap()
    }

    fn pass_sealed() -> Vec<u8> {
        sealed(&kitbag::bundle::Seal::Passphrase(
            age::secrecy::SecretString::from(HOST_PASS.to_string()),
        ))
    }

    /// Bytes under `dir`, every file, for leak checks.
    fn all_bytes(dir: &Path) -> Vec<(PathBuf, Vec<u8>)> {
        let mut out = Vec::new();
        if let Ok(rd) = fs::read_dir(dir) {
            for e in rd.flatten() {
                if e.file_type().unwrap().is_dir() {
                    out.extend(all_bytes(&e.path()));
                } else if let Ok(b) = fs::read(e.path()) {
                    out.push((e.path(), b));
                }
            }
        }
        out
    }

    fn holds(hay: &[u8], needle: &str) -> bool {
        hay.windows(needle.len()).any(|w| w == needle.as_bytes())
    }

    #[track_caller]
    fn assert_no_leak(host: &Host, outs: &[&std::process::Output], needles: &[&str]) {
        let calls = host.calls();
        for n in needles {
            assert!(!calls.contains(n), "ssh argv shows a secret: {calls}");
            for out in outs {
                assert!(!holds(&out.stdout, n), "local stdout shows a secret");
                assert!(!holds(&out.stderr, n), "local stderr shows a secret");
            }
            if let Ok(wire) = fs::read(host.sb.path("upload.tar")) {
                assert!(!holds(&wire, n), "the upload holds a secret in clear");
            }
            // Nothing on this machine's side either.
            for dir in [host.sb.home(), host.sb.state()] {
                for (p, b) in all_bytes(&dir) {
                    assert!(!holds(&b, n), "{} holds a secret", p.display());
                }
            }
            // The host's undo records hold no value.
            for (p, b) in all_bytes(&host.rhome().join(".local/state/kitbag")) {
                assert!(!holds(&b, n), "host undo record {} holds it", p.display());
            }
        }
    }

    fn desktop_config_on_host(host: &Host) -> Option<Vec<u8>> {
        all_bytes(&host.rhome())
            .into_iter()
            .find(|(p, _)| p.ends_with("claude_desktop_config.json"))
            .map(|(_, b)| b)
    }

    fn upload_listing(host: &Host) -> String {
        let out = Command::new("tar")
            .arg("-tf")
            .arg(host.sb.path("upload.tar"))
            .output()
            .unwrap();
        String::from_utf8_lossy(&out.stdout).into_owned()
    }

    #[test]
    fn local_secrets_from_is_still_refused_with_remote() {
        let host = Host::new();
        let prof = bundle_profile(&host);
        fs::write(prof.join("secrets.age"), pass_sealed()).unwrap();
        fs::write(host.sb.path("pass.txt"), format!("{HOST_PASS}\n")).unwrap();
        for sub in ["apply", "diff"] {
            let out = host
                .cli()
                .arg(sub)
                .arg(&prof)
                .args(remote_args(&["--secrets-from"]))
                .arg(prof.join("secrets.age"))
                .arg("--secrets-passphrase-file")
                .arg(host.sb.path("pass.txt"))
                .assert()
                .failure()
                .stderr(contains("--secrets-from can't be used with --remote"))
                .stderr(contains("--remote-secrets-from"))
                .get_output()
                .clone();
            assert_no_leak(&host, &[&out], &[BUNDLE_VALUE, HOST_PASS]);
        }
        assert_eq!(host.calls(), "");
    }

    #[test]
    fn a_bundle_on_the_host_is_opened_there_and_nothing_secret_crosses_ssh() {
        let host = Host::new();
        let prof = bundle_profile(&host);
        // A bundle next to the profile here is not the one used, and stays.
        fs::write(prof.join("secrets.age"), pass_sealed()).unwrap();
        fs::create_dir_all(host.rhome().join("vault")).unwrap();
        fs::write(host.rhome().join("vault/secrets.age"), pass_sealed()).unwrap();
        fs::write(
            host.rhome().join("vault/pass.txt"),
            format!("{HOST_PASS}\n"),
        )
        .unwrap();
        let bundle_path = host.rhome().join("vault/secrets.age");
        let pass_path = host.rhome().join("vault/pass.txt");
        let flags = |sub: &str, extra: &[&str]| {
            let mut c = host.cli();
            c.arg(sub)
                .arg(&prof)
                .args([
                    "--remote",
                    "you@stub-host",
                    "--remote-kitbag",
                    BIN,
                    "--to",
                    "claude-desktop",
                    "--remote-secrets-from",
                ])
                .arg(&bundle_path)
                .arg("--remote-secrets-passphrase-file")
                .arg(&pass_path)
                .args(extra);
            c
        };

        // diff on the host: it opened the bundle, and the value is masked.
        let diff = flags("diff", &[])
            .assert()
            .success()
            .stdout(contains("holds 1 variable(s)"))
            .stdout(contains("GITHUB_TOKEN"))
            .stdout(contains("••••"))
            .get_output()
            .clone();
        assert!(desktop_config_on_host(&host).is_none(), "diff wrote");
        let dry = flags("apply", &["--dry-run"])
            .assert()
            .success()
            .get_output()
            .clone();
        let applied = flags("apply", &["--yes"])
            .assert()
            .success()
            .stdout(contains("opened on you@stub-host from"))
            .get_output()
            .clone();

        // The host opened it: Claude Desktop there got the value.
        let desktop = desktop_config_on_host(&host).expect("written on the host");
        assert!(holds(&desktop, BUNDLE_VALUE));
        // The host paths went over ssh, quoted; no value or passphrase did.
        let calls = host.calls();
        assert!(calls.contains("'--secrets-from'"), "{calls}");
        assert!(
            calls.contains(&format!("'{}'", bundle_path.display())),
            "{calls}"
        );
        assert!(calls.contains("'--secrets-passphrase-file'"), "{calls}");
        assert_no_leak(&host, &[&diff, &dry, &applied], &[BUNDLE_VALUE, HOST_PASS]);
        // The local bundle stayed behind.
        assert!(!upload_listing(&host).contains("secrets.age"));
        assert_eq!(host.leftovers(), Vec::<String>::new());
    }

    #[test]
    fn the_profiles_encrypted_bundle_is_sent_only_with_the_include_flag() {
        let host = Host::new();
        let prof = bundle_profile(&host);
        let id = age::x25519::Identity::generate();
        let recipient = id.to_public().to_string();
        let id_text = {
            use age::secrecy::ExposeSecret;
            id.to_string().expose_secret().to_string()
        };
        let keys = kitbag::bundle::parse_recipients(&recipient).unwrap();
        fs::write(
            prof.join("secrets.age"),
            sealed(&kitbag::bundle::Seal::Keys(keys)),
        )
        .unwrap();
        fs::write(host.rhome().join("id.txt"), format!("{id_text}\n")).unwrap();

        // Without the flag, it stays here.
        host.cli()
            .arg("diff")
            .arg(&prof)
            .args(remote_args(&[]))
            .assert()
            .success();
        assert!(!upload_listing(&host).contains("secrets.age"));

        // With it, the ciphertext is sent and opened with the host's own
        // identity (a `~/` path means the host's home).
        let out = host
            .cli()
            .arg("apply")
            .arg(&prof)
            .args([
                "--remote",
                "you@stub-host",
                "--remote-kitbag",
                BIN,
                "--to",
                "claude-desktop",
                "--remote-include-secrets-age",
                "--remote-secrets-identity",
                "~/id.txt",
                "--yes",
            ])
            .assert()
            .success()
            .stdout(contains("encrypted secrets.age was sent"))
            .get_output()
            .clone();
        assert!(upload_listing(&host).contains("secrets.age"));
        let wire = fs::read(host.sb.path("upload.tar")).unwrap();
        assert!(holds(&wire, "age-encryption.org/v1"));
        let desktop = desktop_config_on_host(&host).expect("written on the host");
        assert!(holds(&desktop, BUNDLE_VALUE));
        assert!(host.calls().contains("\"$HOME\"'/id.txt'"));
        assert_no_leak(&host, &[&out], &[BUNDLE_VALUE, &id_text]);
        assert_eq!(host.leftovers(), Vec::<String>::new());
    }

    #[test]
    fn the_include_flag_refuses_a_plaintext_bundle_or_sidecar_and_sends_nothing() {
        for case in ["plain", "sidecar", "missing"] {
            let host = Host::new();
            let prof = bundle_profile(&host);
            match case {
                "plain" => fs::write(
                    prof.join("secrets.age"),
                    format!("GITHUB_TOKEN={BUNDLE_VALUE}\n"),
                )
                .unwrap(),
                "sidecar" => {
                    fs::write(prof.join("secrets.age"), pass_sealed()).unwrap();
                    fs::write(
                        prof.join("secrets.decrypted"),
                        format!("GITHUB_TOKEN={BUNDLE_VALUE}\n"),
                    )
                    .unwrap();
                }
                _ => {}
            }
            let out = host
                .cli()
                .arg("apply")
                .arg(&prof)
                .args(remote_args(&[
                    "--remote-include-secrets-age",
                    "--remote-secrets-passphrase-file",
                    "/srv/pass.txt",
                    "--yes",
                ]))
                .assert()
                .failure()
                .get_output()
                .clone();
            let err = String::from_utf8_lossy(&out.stderr).into_owned();
            assert!(
                err.contains("not sending") || err.contains("not found"),
                "{case}: {err}"
            );
            assert!(!host.sb.path("upload.tar").exists(), "{case}: sent");
            assert!(!host.calls().contains("mkdir -p"), "{case}");
            assert_no_leak(&host, &[&out], &[BUNDLE_VALUE]);
            assert_eq!(host.leftovers(), Vec::<String>::new());
        }
    }

    #[test]
    fn remote_secret_flags_need_a_host_unlock_path_and_each_other() {
        let host = Host::new();
        let prof = bundle_profile(&host);
        // No way to open it on the host: refused before ssh.
        host.cli()
            .arg("diff")
            .arg(&prof)
            .args(remote_args(&["--remote-secrets-from", "/srv/secrets.age"]))
            .assert()
            .failure()
            .stderr(contains("--remote-secrets-identity"));
        // An unlock path without a bundle, both bundle flags, or no --remote.
        for args in [
            &["--remote-secrets-passphrase-file", "/srv/p"][..],
            &[
                "--remote-secrets-from",
                "/srv/s.age",
                "--remote-include-secrets-age",
                "--remote-secrets-identity",
                "/srv/id",
            ][..],
        ] {
            host.cli()
                .arg("diff")
                .arg(&prof)
                .args(remote_args(args))
                .assert()
                .code(2);
        }
        host.cli()
            .arg("diff")
            .arg(&prof)
            .args(["--remote-secrets-from", "/srv/s.age"])
            .assert()
            .code(2);
        // A host path that looks like an option.
        host.cli()
            .arg("diff")
            .arg(&prof)
            .args(remote_args(&[
                "--remote-secrets-from=-oProxyCommand=x",
                "--remote-secrets-identity",
                "/srv/id",
            ]))
            .assert()
            .failure()
            .stderr(contains("not a usable path on the host"));
        assert_eq!(host.calls(), "");
    }

    // ---- Windows hosts (#148), against a stand-in for ssh to Windows
    // OpenSSH Server: it answers the OS check as cmd.exe does, refuses any
    // command that is not the encoded PowerShell invocation (as cmd.exe would
    // `sh`), decodes each script and does what it asks with C:\ mapped into
    // the sandbox (C:\Users\you is the host home, C:\Tools is on its PATH).
    // The PowerShell itself is run for real by the Windows-only tests below.
    mod windows_host {
        use super::*;

        const STUB: &str = r#"#!/usr/bin/env python3
import base64, os, random, re, shutil, string, subprocess, sys
ROOT, CALLS, SCRIPTS, UPLOAD, KITBAG = sys.argv.pop(1), '@CALLS@', '@SCRIPTS@', '@UPLOAD@', '@KITBAG@'
HOME = 'C:\\Users\\you'
LAD = HOME + '\\AppData\\Local'
TEMP = LAD + '\\Temp'
PS = 'powershell.exe -NoProfile -NonInteractive -ExecutionPolicy Bypass -EncodedCommand '
argv = sys.argv[1:]
with open(CALLS, 'a') as f:
    f.write(' '.join(argv) + '\n')
if len(argv) != 3 or argv[0] != '--':
    sys.stderr.write('stub ssh: expected -- HOST COMMAND\n')
    sys.exit(255)
cmd = argv[2]
if cmd == 'echo kitbag-remote:os=%OS%\\$env:OS':
    print('kitbag-remote:os=Windows_NT\\$env:OS\r')
    sys.exit(0)
if not cmd.startswith(PS):
    sys.stderr.write("'%s' is not recognized as an internal or external command\r\n" % cmd.split(' ')[0])
    sys.exit(1)
script = base64.b64decode(cmd[len(PS):]).decode('utf-16-le')
kind = re.match(r'# kitbag-remote:(\w+)\n', script).group(1)
if kind == 'stdin':
    script = base64.b64decode(''.join(sys.stdin.read().split())).decode('utf-8')
    kind = re.match(r'# kitbag-remote:(\w+)\n', script).group(1)
with open(SCRIPTS, 'a') as f:
    f.write(script + '\n----\n')
LIT = r"'((?:[^']|'')*)'"
def unq(s):
    return s.replace("''", "'")
v = {m.group(1): unq(m.group(2)) for m in re.finditer(r'^\$(\w+) = ' + LIT + '$', script, re.M)}
def is_win(p):
    return re.match(r'^[A-Za-z]:[\\/]', p) is not None
def local(p):
    if not is_win(p):
        sys.stderr.write('stub ssh: not a Windows path: %r\n' % p)
        sys.exit(2)
    return os.path.join(ROOT, p[0], *[x for x in re.split(r'[\\/]+', p[3:]) if x])
def say(k, val):
    print('kitbag-remote:%s=%s\r' % (k, val))
    sys.stdout.flush()
def split(s):
    args, i, n = [], 0, len(s)
    while i < n:
        while i < n and s[i] in ' \t':
            i += 1
        if i >= n:
            break
        cur, q = '', False
        while i < n and (q or s[i] not in ' \t'):
            if s[i] == '\\':
                j = i
                while j < n and s[j] == '\\':
                    j += 1
                k = j - i
                if j < n and s[j] == '"':
                    cur += '\\' * (k // 2)
                    if k % 2:
                        cur += '"'
                        j += 1
                else:
                    cur += '\\' * k
                i = j
            elif s[i] == '"':
                if q and i + 1 < n and s[i + 1] == '"':
                    cur += '"'
                    i += 2
                else:
                    q = not q
                    i += 1
            else:
                cur += s[i]
                i += 1
        args.append(cur)
    return args
def version(b):
    return subprocess.run([local(b), '--version'], capture_output=True, text=True).stdout.strip()
if kind == 'probe':
    m = re.search(r'^\$explicit = (?:' + LIT + r'|\(Join-Path \$env:USERPROFILE ' + LIT + r'\))$', script, re.M)
    explicit = unq(m.group(1)) if m.group(1) is not None else HOME + '\\' + unq(m.group(2))
    tmp = TEMP + '\\kitbag-remote.' + ''.join(random.choice(string.ascii_letters + string.digits) for _ in range(8))
    os.makedirs(local(tmp))
    say('os', 'Windows_NT')
    say('tmp', tmp)
    say('home', HOME)
    def find(c):
        if any(ch in c for ch in '\\/:'):
            return c if os.path.isfile(local(c)) else ''
        p = 'C:\\Tools\\' + c
        return p if os.path.isfile(local(p)) else ''
    b = ''
    for c in [explicit] if explicit else ['kitbag.exe', LAD + '\\kitbag\\bin\\kitbag.exe', HOME + '\\.cargo\\bin\\kitbag.exe']:
        b = find(c)
        if b:
            break
    if explicit and not b:
        say('error', '--remote-kitbag %s is not an executable file on this host' % explicit)
        shutil.rmtree(local(tmp))
        sys.exit(1)
    if b and version(b).startswith('kitbag '):
        say('kitbag', b)
        say('version', version(b))
    sys.exit(0)
if kind == 'download':
    assert "'kitbag-x86_64-pc-windows-msvc.zip'" in script and 'Get-FileHash' in script
    if os.path.exists(os.path.join(ROOT, 'download-fails')):
        say('error', 'checksum mismatch for kitbag-x86_64-pc-windows-msvc.zip; nothing was run')
        sys.exit(1)
    keep = LAD + '\\kitbag\\bin\\kitbag.exe'
    os.makedirs(os.path.dirname(local(keep)), exist_ok=True)
    shutil.copy(KITBAG, local(keep))
    os.chmod(local(keep), 0o755)
    say('kitbag', keep)
    say('version', version(keep))
    sys.exit(0)
if kind == 'upload':
    targs = split(v['tarArgs'])
    if targs != ['-xf', v['tar'], '-C', v['dir']]:
        sys.stderr.write('stub ssh: odd tar arguments %r\n' % targs)
        sys.exit(2)
    data = sys.stdin.buffer.read()
    with open(UPLOAD, 'wb') as f:
        f.write(data)
    os.makedirs(local(v['dir']), exist_ok=True)
    with open(local(v['tar']), 'wb') as f:
        f.write(data)
    rc = subprocess.run(['tar', '-xf', local(v['tar']), '-C', local(v['dir'])]).returncode
    os.remove(local(v['tar']))
    sys.exit(rc)
if kind == 'run':
    args = [local(a) if is_win(a) else a for a in split(v['argline'])]
    with open(SCRIPTS, 'a') as f:
        f.write('argv: %r\n----\n' % args)
    env = {'PATH': '/usr/bin:/bin', 'HOME': local(HOME), 'NO_COLOR': '1'}
    sys.exit(subprocess.run([local(v['file'])] + args, env=env, stdin=subprocess.DEVNULL).returncode)
if kind == 'cleanup':
    shutil.rmtree(local(v['tmp']))
    sys.exit(0)
sys.stderr.write('stub ssh: unknown script %s\n' % kind)
sys.exit(2)
"#;

        fn have_python() -> bool {
            Command::new("python3")
                .arg("--version")
                .output()
                .is_ok_and(|o| o.status.success())
        }

        /// A [`Host`] whose ssh is the Windows stand-in. `rhome` is
        /// `C:\Users\you`, `rtmp` its `%TEMP%`, `rpath` is `C:\Tools`.
        fn win_host() -> Option<Host> {
            if !have_python() {
                eprintln!("skipped: python3 is needed for the Windows ssh stand-in");
                return None;
            }
            let host = Host::new();
            let root = host.sb.path("winroot");
            fs::create_dir_all(root.join("C/Users")).unwrap();
            std::os::unix::fs::symlink(host.rhome(), root.join("C/Users/you")).unwrap();
            std::os::unix::fs::symlink(host.sb.path("rpath"), root.join("C/Tools")).unwrap();
            fs::create_dir_all(host.rhome().join("AppData/Local")).unwrap();
            std::os::unix::fs::symlink(
                host.sb.path("rtmp"),
                host.rhome().join("AppData/Local/Temp"),
            )
            .unwrap();
            let stub = STUB
                .replace("@CALLS@", &host.sb.path("calls.log").display().to_string())
                .replace(
                    "@SCRIPTS@",
                    &host.sb.path("scripts.log").display().to_string(),
                )
                .replace(
                    "@UPLOAD@",
                    &host.sb.path("upload.tar").display().to_string(),
                )
                .replace("@KITBAG@", BIN);
            let py = host.sb.path("winssh.py");
            fs::write(&py, stub).unwrap();
            fs::write(
                host.ssh(),
                format!(
                    "#!/bin/sh\nexec python3 '{}' '{}' \"$@\"\n",
                    py.display(),
                    root.display()
                ),
            )
            .unwrap();
            Some(host)
        }

        fn scripts(host: &Host) -> String {
            fs::read_to_string(host.sb.path("scripts.log")).unwrap_or_default()
        }

        fn keep(host: &Host) -> PathBuf {
            host.rhome().join("AppData/Local/kitbag/bin/kitbag.exe")
        }

        fn installed(host: &Host) {
            fs::create_dir_all(keep(host).parent().unwrap()).unwrap();
            std::os::unix::fs::symlink(BIN, keep(host)).unwrap();
        }

        fn win_args<'a>(extra: &[&'a str]) -> Vec<&'a str> {
            let mut v = vec!["--remote", "you@winbox", "--to", "claude-code"];
            v.extend_from_slice(extra);
            v
        }

        /// Every command went to ssh as encoded PowerShell, after the OS check.
        #[track_caller]
        fn only_powershell(host: &Host) {
            let calls = host.calls();
            let detect = r"-- you@winbox echo kitbag-remote:os=%OS%\$env:OS";
            assert!(calls.starts_with(detect), "{calls}");
            for l in calls.lines().filter(|l| *l != detect) {
                assert!(
                    l.starts_with("-- you@winbox powershell.exe -NoProfile -NonInteractive -ExecutionPolicy Bypass -EncodedCommand "),
                    "{l}"
                );
                assert!(!l.contains("sh -s") && !l.contains("mkdir -p"), "{l}");
            }
        }

        #[test]
        fn apply_on_a_windows_host_writes_there_and_cleans_up() {
            let Some(host) = win_host() else { return };
            installed(&host);
            let prof = profile(&host);
            host.cli()
                .arg("apply")
                .arg(&prof)
                .args(win_args(&["--yes"]))
                .assert()
                .success()
                .stdout(
                    contains(r"kitbag there: C:\Users\you\AppData\Local\kitbag\bin\kitbag.exe")
                        .and(contains("Wrote"))
                        .and(contains("kitbag undo --remote")),
                );
            only_powershell(&host);
            assert!(
                fs::read_to_string(host.rhome().join(".claude/CLAUDE.md"))
                    .unwrap()
                    .contains("Be brief")
            );
            assert!(!host.sb.home().join(".claude").exists());
            assert!(!host.sb.state().exists());
            // The temp dir was under %TEMP%, removed again.
            assert!(scripts(&host).contains(r"C:\Users\you\AppData\Local\Temp\kitbag-remote."));
            assert_eq!(host.leftovers(), Vec::<String>::new());
            // References went, values did not.
            let wire = String::from_utf8_lossy(&fs::read(host.sb.path("upload.tar")).unwrap())
                .into_owned();
            assert!(wire.contains("${TOKEN}") && !wire.contains(FAKE_TOKEN));
            assert!(!scripts(&host).contains(FAKE_TOKEN));
            assert!(!tree_text(&host.rhome()).contains(FAKE_TOKEN));
            // `undo --remote` there reverts it, uploading nothing.
            let uploads = fs::metadata(host.sb.path("upload.tar")).unwrap().len();
            host.cli()
                .args(["undo", "--remote", "you@winbox"])
                .assert()
                .success()
                .stdout(contains("Undid"));
            assert!(!host.rhome().join(".claude/CLAUDE.md").exists());
            assert_eq!(
                fs::metadata(host.sb.path("upload.tar")).unwrap().len(),
                uploads
            );
            assert!(!scripts(&host).contains("--home"));
            assert_eq!(host.leftovers(), Vec::<String>::new());
            only_powershell(&host);
        }

        #[test]
        fn diff_on_a_windows_host_writes_nothing() {
            let Some(host) = win_host() else { return };
            installed(&host);
            let prof = profile(&host);
            host.cli()
                .arg("diff")
                .arg(&prof)
                .args(win_args(&["--exit-code"]))
                .assert()
                .code(1)
                .stdout(contains("CLAUDE.md"));
            assert!(!host.rhome().join(".claude").exists());
            assert_eq!(host.leftovers(), Vec::<String>::new());
            only_powershell(&host);
        }

        #[test]
        fn remote_download_on_a_windows_host_keeps_kitbag_in_localappdata() {
            let Some(host) = win_host() else { return };
            let prof = profile(&host);
            // Without the flag: a clear stop that names the Windows places.
            host.cli()
                .arg("apply")
                .arg(&prof)
                .args(win_args(&["--yes"]))
                .assert()
                .failure()
                .stderr(contains("not installed").and(contains(r"%LOCALAPPDATA%\kitbag\bin")));
            assert!(!keep(&host).exists());
            // A failed check keeps nothing and runs nothing.
            fs::write(host.sb.path("winroot/download-fails"), "").unwrap();
            host.cli()
                .arg("apply")
                .arg(&prof)
                .args(win_args(&["--remote-download", "--yes"]))
                .assert()
                .failure()
                .stderr(contains("checksum mismatch"));
            assert!(!keep(&host).exists());
            assert!(!host.rhome().join(".claude").exists());
            assert_eq!(host.leftovers(), Vec::<String>::new());
            fs::remove_file(host.sb.path("winroot/download-fails")).unwrap();
            host.cli()
                .arg("apply")
                .arg(&prof)
                .args(win_args(&["--remote-download", "--yes"]))
                .assert()
                .success()
                .stderr(contains(
                    r"kept the verified kitbag at C:\Users\you\AppData\Local\kitbag\bin\kitbag.exe",
                ));
            assert!(keep(&host).is_file());
            assert!(host.rhome().join(".claude/CLAUDE.md").exists());
            assert!(scripts(&host).contains("# kitbag-remote:download"));
            assert_eq!(host.leftovers(), Vec::<String>::new());
            // Found there later without the flag.
            host.cli()
                .args(["undo", "--remote", "you@winbox"])
                .assert()
                .success()
                .stdout(contains("Undid"));
            assert!(keep(&host).is_file());
            only_powershell(&host);
        }

        #[test]
        fn secrets_are_still_refused_before_anything_is_sent_to_a_windows_host() {
            let Some(host) = win_host() else { return };
            installed(&host);
            let prof = profile(&host);
            fs::write(
                prof.join("kitbag.toml"),
                "kitbag = 1\nname = \"prof\"\n\n[mcp_servers.docs]\nurl = \"https://docs.example.com/mcp\"\nheaders = { Authorization = \"Bearer ghp_abcdefghijklmnopqrstuvwxyz0123456789\" }\n",
            )
            .unwrap();
            host.cli()
                .arg("apply")
                .arg(&prof)
                .args(win_args(&["--yes"]))
                .assert()
                .failure();
            let bprof = bundle_profile(&host);
            fs::write(bprof.join("secrets.age"), pass_sealed()).unwrap();
            host.cli()
                .arg("diff")
                .arg(&bprof)
                .args(win_args(&["--secrets-from"]))
                .arg(bprof.join("secrets.age"))
                .assert()
                .failure()
                .stderr(contains("--secrets-from can't be used with --remote"));
            assert_eq!(host.calls(), "");
        }

        #[test]
        fn host_paths_on_windows_mean_the_user_profile_and_are_quoted() {
            let Some(host) = win_host() else { return };
            installed(&host);
            let prof = bundle_profile(&host);
            let odd = host.rhome().join("it's $(touch pwned) `app`");
            fs::create_dir_all(&odd).unwrap();
            fs::create_dir_all(host.rhome().join("vault")).unwrap();
            fs::write(host.rhome().join("vault/secrets.age"), pass_sealed()).unwrap();
            fs::write(
                host.rhome().join("vault/pass.txt"),
                format!("{HOST_PASS}\n"),
            )
            .unwrap();
            let plain = profile(&host);
            host.cli()
                .arg("apply")
                .arg(&plain)
                .args(win_args(&[
                    "--remote-project",
                    "~/it's $(touch pwned) `app`",
                    "--yes",
                ]))
                .assert()
                .success()
                .stdout(contains("Project scope"));
            assert!(odd.join(".claude/rules/style.md").exists());
            let out = host
                .cli()
                .arg("apply")
                .arg(&prof)
                .args([
                    "--remote",
                    "you@winbox",
                    "--to",
                    "claude-desktop",
                    "--remote-secrets-from",
                    "~/vault/secrets.age",
                    "--remote-secrets-passphrase-file",
                    "~/vault/pass.txt",
                    "--yes",
                ])
                .assert()
                .success()
                .get_output()
                .clone();
            let s = scripts(&host);
            // `~/` became %USERPROFILE%, each path one argument.
            for arg in [
                r#""--project" "C:\Users\you\it''s $(touch pwned) `app`""#,
                r#""--secrets-from" "C:\Users\you\vault\secrets.age""#,
                r#""--secrets-passphrase-file" "C:\Users\you\vault\pass.txt""#,
            ] {
                assert!(s.contains(arg), "{arg}\n{s}");
            }
            assert!(!s.contains("$HOME"));
            // Opened on the host: the value reached Claude Desktop there.
            let desktop = desktop_config_on_host(&host).expect("written on the host");
            assert!(holds(&desktop, BUNDLE_VALUE));
            assert!(!host.rhome().join("pwned").exists());
            assert!(!Path::new("pwned").exists());
            assert!(!s.contains(BUNDLE_VALUE) && !s.contains(HOST_PASS));
            assert_no_leak(&host, &[&out], &[BUNDLE_VALUE, HOST_PASS]);
            assert_eq!(host.leftovers(), Vec::<String>::new());
            only_powershell(&host);
        }

        #[test]
        fn an_explicit_windows_kitbag_path_is_used_and_checked() {
            let Some(host) = win_host() else { return };
            let prof = profile(&host);
            fs::create_dir_all(host.rhome().join("my tools")).unwrap();
            std::os::unix::fs::symlink(BIN, host.rhome().join("my tools/kb.exe")).unwrap();
            host.cli()
                .arg("diff")
                .arg(&prof)
                .args(win_args(&["--remote-kitbag", "~/my tools/kb.exe"]))
                .assert()
                .success()
                .stdout(contains(r"kitbag there: C:\Users\you\my tools\kb.exe"));
            host.cli()
                .arg("diff")
                .arg(&prof)
                .args(win_args(&["--remote-kitbag", r"C:\nowhere\kitbag.exe"]))
                .assert()
                .failure()
                .stderr(contains("is not an executable file on this host"));
            assert_eq!(host.leftovers(), Vec::<String>::new());
        }

        #[test]
        fn a_windows_host_behind_git_bash_still_gets_the_windows_side() {
            // The ssh shell is POSIX (the OS check prints nothing Windows),
            // but `uname` says MINGW: kitbag must not run its POSIX side. With
            // no PowerShell there, it stops and says so.
            let host = Host::new();
            fs::write(
                host.sb.path("rpath/uname"),
                "#!/bin/sh\necho MINGW64_NT-10.0-19045\n",
            )
            .unwrap();
            fs::set_permissions(
                host.sb.path("rpath/uname"),
                fs::Permissions::from_mode(0o755),
            )
            .unwrap();
            with_kitbag(&host);
            let prof = profile(&host);
            host.cli()
                .arg("diff")
                .arg(&prof)
                .args(["--remote", "you@stub-host", "--to", "claude-code"])
                .assert()
                .failure()
                .stderr(contains("is a Windows host").and(contains("powershell.exe")));
            let calls = host.calls();
            assert!(calls.contains("powershell.exe -NoProfile"), "{calls}");
            assert!(!calls.contains("mkdir -p"), "{calls}");
            assert_eq!(host.leftovers(), Vec::<String>::new());
        }
    }
}
