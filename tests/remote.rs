//! `apply --remote` / `diff --remote`, against a stub `ssh` that runs the remote
//! command on this machine with an empty environment and a throwaway home. No
//! network, no real ssh agent, no real home directory.

mod common;

#[cfg(windows)]
#[test]
fn remote_says_it_is_not_supported_on_windows() {
    let sb = common::Sandbox::empty();
    sb.cli()
        .args(["diff", "prof", "--remote", "you@example.com"])
        .assert()
        .failure()
        .stderr(predicates::str::contains("not supported on Windows"));
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

    #[test]
    fn git_sources_project_scope_and_incompatible_flags_are_refused() {
        let host = Host::new();
        let prof = profile(&host);
        let url = host
            .sb
            .git_profile("gitprof", &[("kitbag.toml", "kitbag = 1\nname = \"g\"\n")]);
        host.cli()
            .args([
                "diff",
                &url,
                "--remote",
                "you@stub-host",
                "--remote-kitbag",
                BIN,
            ])
            .assert()
            .failure()
            .stderr(contains("not a git URL"));
        host.cli()
            .arg("diff")
            .arg(&prof)
            .args(["--remote", "you@stub-host", "--scope", "project"])
            .assert()
            .failure()
            .stderr(contains("user scope only"));
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
}
