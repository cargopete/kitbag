//! `kitbag sessions export` / `import` (#11, first slice): same-tool moves of
//! Claude Code and Codex CLI transcripts, redacted by default, optionally in
//! an age archive. Every secret here is a made-up one; the tests look for it
//! in the export, the output, the state directory and everywhere else in the
//! sandbox, and for a real home directory that must stay untouched.

mod common;

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use age::secrecy::ExposeSecret;
use common::*;
use predicates::prelude::*;
use predicates::str::contains;

const TOKEN: &str = "fake-session-token-value-1234";
const PASS: &str = "fake-test-passphrase-not-real";
const CLAUDE_REL: &str = ".claude/projects/-work-demo/11111111-aaaa-bbbb-cccc-222222222222.jsonl";
const CODEX_REL: &str = ".codex/sessions/2026/01/02/rollout-2026-01-02T03-04-05-abc.jsonl";

fn transcript() -> String {
    [
        // a message
        format!(
            r#"{{"type":"user","message":{{"role":"user","content":"please run export API_TOKEN={TOKEN} for me"}}}}"#
        ),
        // a tool result
        format!(
            r#"{{"type":"user","message":{{"content":[{{"type":"tool_result","content":"SESSION_TOKEN={TOKEN}\nok"}}]}}}}"#
        ),
        // a header and a URL in a tool call
        format!(
            r#"{{"type":"assistant","message":{{"content":[{{"type":"tool_use","input":{{"command":"curl -H 'Authorization: Bearer {TOKEN}' 'https://api.example.com/v1?token={TOKEN}'"}}}}]}}}}"#
        ),
        r#"{"type":"assistant","message":{"content":"hello, nothing secret here"}}"#.to_string(),
    ]
    .join("\n")
        + "\n"
}

fn write(sb: &Sandbox, rel: &str, text: &str) {
    let p = sb.path(rel);
    fs::create_dir_all(p.parent().unwrap()).unwrap();
    fs::write(p, text).unwrap();
}

fn seed(sb: &Sandbox) {
    write(sb, &format!("home/{CLAUDE_REL}"), &transcript());
    write(sb, &format!("home/{CODEX_REL}"), &transcript());
}

fn tree(dir: &Path) -> BTreeMap<PathBuf, Vec<u8>> {
    fn walk(dir: &Path, out: &mut BTreeMap<PathBuf, Vec<u8>>) {
        let Ok(rd) = fs::read_dir(dir) else { return };
        for e in rd.flatten() {
            let p = e.path();
            if p.is_dir() {
                walk(&p, out);
            } else {
                out.insert(p.clone(), fs::read(&p).unwrap_or_default());
            }
        }
    }
    let mut out = BTreeMap::new();
    walk(dir, &mut out);
    out
}

/// The whole sandbox, without `skip` (the export directory).
fn snapshot(sb: &Sandbox, skip: &Path) -> BTreeMap<PathBuf, Vec<u8>> {
    tree(sb.tmp.path())
        .into_iter()
        .filter(|(p, _)| !p.starts_with(skip))
        .collect()
}

fn assert_no_token(files: &BTreeMap<PathBuf, Vec<u8>>) {
    for (p, b) in files {
        assert!(
            !String::from_utf8_lossy(b).contains(TOKEN),
            "the token is in {}",
            p.display()
        );
    }
}

fn export(sb: &Sandbox, from: &str, out: &str) -> assert_cmd::Command {
    let mut c = sb.cli();
    c.args(["sessions", "export", "--from", from, "-o"])
        .arg(sb.path(out));
    c
}

#[test]
fn export_redacts_and_writes_only_to_the_directory() {
    for (tool, rel) in [("claude-code", CLAUDE_REL), ("codex", CODEX_REL)] {
        let sb = Sandbox::empty();
        seed(&sb);
        let real_home = sb.path("realhome");
        fs::create_dir_all(&real_home).unwrap();
        let before = snapshot(&sb, &sb.path("out"));
        let assert = export(&sb, tool, "out")
            .env("HOME", &real_home)
            .env("USERPROFILE", &real_home)
            .assert()
            .success()
            .stdout(contains("Exported 1"));
        let o = assert.get_output().clone();
        let out =
            String::from_utf8_lossy(&o.stdout).to_string() + &String::from_utf8_lossy(&o.stderr);
        assert!(!out.contains(TOKEN));
        let written = tree(&sb.path("out"));
        assert_no_token(&written);
        let joined: String = written
            .values()
            .map(|b| String::from_utf8_lossy(b).to_string())
            .collect();
        assert!(joined.contains("[REDACTED"), "{joined}");
        assert!(joined.contains("hello, nothing secret here"));
        let manifest = sb.json("out/kitbag-sessions.json");
        assert_eq!(manifest["tool"], tool);
        assert_eq!(manifest["redacted"], true);
        assert_eq!(manifest["kitbag_version"], env!("CARGO_PKG_VERSION"));
        // Everything else in the sandbox is as it was: no state, no real home.
        assert_eq!(snapshot(&sb, &sb.path("out")), before, "{tool}");
        assert!(!sb.state().exists());
        assert!(tree(&real_home).is_empty());
        // The source transcript still has its token (export only copies).
        assert!(sb.read(&format!("home/{rel}")).contains(TOKEN));
    }
}

#[test]
fn no_redact_warns_and_stays_in_the_directory() {
    let sb = Sandbox::empty();
    seed(&sb);
    let before = snapshot(&sb, &sb.path("out"));
    export(&sb, "claude-code", "out")
        .arg("--no-redact")
        .assert()
        .success()
        .stderr(contains("--no-redact").and(contains("credentials")));
    let written = tree(&sb.path("out"));
    assert!(
        written
            .values()
            .any(|b| String::from_utf8_lossy(b).contains(TOKEN)),
        "--no-redact keeps the content"
    );
    assert_eq!(sb.json("out/kitbag-sessions.json")["redacted"], false);
    assert_eq!(snapshot(&sb, &sb.path("out")), before);
}

fn passphrase_file(sb: &Sandbox) -> PathBuf {
    let p = sb.path("secret/pass.txt");
    fs::create_dir_all(p.parent().unwrap()).unwrap();
    fs::write(&p, format!("{PASS}\n")).unwrap();
    p
}

#[test]
fn encrypted_export_is_age_only_and_imports_back() {
    let sb = Sandbox::empty();
    seed(&sb);
    let pass = passphrase_file(&sb);
    let before = snapshot(&sb, &sb.path("out"));
    let assert = export(&sb, "claude-code", "out")
        .args(["--encrypt", "--secrets-passphrase-file"])
        .arg(&pass)
        .assert()
        .success();
    let o = assert.get_output().clone();
    let said = String::from_utf8_lossy(&o.stdout).to_string() + &String::from_utf8_lossy(&o.stderr);
    assert!(!said.contains(TOKEN) && !said.contains(PASS));
    let written = tree(&sb.path("out"));
    assert_eq!(written.len(), 1, "only the archive: {:?}", written.keys());
    let (name, bytes) = written.iter().next().unwrap();
    assert!(name.ends_with("sessions.age"));
    assert!(bytes.starts_with(b"age-encryption.org/v1\n"));
    assert_no_token(&written);
    assert_eq!(snapshot(&sb, &sb.path("out")), before);

    // Import into a different sandbox: wrong passphrase first.
    let dst = Sandbox::empty();
    let bad = dst.path("bad.txt");
    fs::write(&bad, "fake-wrong-passphrase\n").unwrap();
    dst.cli()
        .args(["sessions", "import", "--to", "claude-code"])
        .arg(sb.path("out"))
        .arg("--secrets-passphrase-file")
        .arg(&bad)
        .assert()
        .failure();
    assert!(!dst.path("home/.claude").exists());

    let assert = dst
        .cli()
        .args(["sessions", "import", "--to", "claude-code"])
        .arg(sb.path("out"))
        .arg("--secrets-passphrase-file")
        .arg(&pass)
        .assert()
        .success()
        .stdout(contains("claude --resume "));
    let o = assert.get_output().clone();
    let said = String::from_utf8_lossy(&o.stdout).to_string() + &String::from_utf8_lossy(&o.stderr);
    assert!(!said.contains(TOKEN));
    let restored = dst.read(&format!("home/{CLAUDE_REL}"));
    assert!(restored.contains("[REDACTED") && !restored.contains(TOKEN));
    assert_no_token(&tree(&dst.path("home")));
    // No decrypt leftovers anywhere in either sandbox.
    for s in [&sb, &dst] {
        for p in tree(s.tmp.path()).keys() {
            let n = p.file_name().unwrap().to_string_lossy().to_string();
            assert!(!n.ends_with(".tmp"), "leftover {}", p.display());
        }
        assert_no_token(&tree(&s.state()));
    }
}

#[test]
fn encrypted_export_to_a_key_imports_with_the_identity() {
    let sb = Sandbox::empty();
    seed(&sb);
    let id = age::x25519::Identity::generate();
    let idfile = sb.path("secret/id.txt");
    fs::create_dir_all(idfile.parent().unwrap()).unwrap();
    fs::write(&idfile, format!("{}\n", id.to_string().expose_secret())).unwrap();
    export(&sb, "codex", "out")
        .args(["--secrets-recipient", &id.to_public().to_string()])
        .assert()
        .success();
    assert_no_token(&tree(&sb.path("out")));
    let dst = Sandbox::empty();
    dst.cli()
        .args(["sessions", "import", "--to", "codex"])
        .arg(sb.path("out"))
        .arg("--secrets-identity")
        .arg(&idfile)
        .assert()
        .success();
    assert!(dst.read(&format!("home/{CODEX_REL}")).contains("[REDACTED"));
}

#[test]
fn passphrase_file_inside_the_export_directory_is_refused() {
    let sb = Sandbox::empty();
    seed(&sb);
    let p = sb.path("out/pass.txt");
    fs::create_dir_all(p.parent().unwrap()).unwrap();
    fs::write(&p, format!("{PASS}\n")).unwrap();
    export(&sb, "claude-code", "out")
        .args(["--secrets-passphrase-file"])
        .arg(&p)
        .assert()
        .failure()
        .stderr(contains("passphrase file is inside"));
}

#[test]
fn import_never_starts_a_tool_and_writes_only_the_tools_directory() {
    let src = Sandbox::empty();
    seed(&src);
    export(&src, "claude-code", "out").assert().success();

    let dst = Sandbox::empty();
    // A git profile next to it, which must stay untouched.
    write(&dst, "profile/kitbag.toml", "name = \"p\"\n");
    // Stand-ins for the tools that record being run.
    let bin = dst.path("bin");
    fs::create_dir_all(&bin).unwrap();
    let marker = dst.path("marker");
    #[cfg(unix)]
    for tool in ["claude", "codex"] {
        use std::os::unix::fs::PermissionsExt;
        let f = bin.join(tool);
        fs::write(
            &f,
            format!("#!/bin/sh\necho ran >> '{}'\n", marker.display()),
        )
        .unwrap();
        fs::set_permissions(&f, fs::Permissions::from_mode(0o755)).unwrap();
    }
    let before = snapshot(&dst, &dst.path("home/.claude"));
    let path = format!(
        "{}:{}",
        bin.display(),
        std::env::var("PATH").unwrap_or_default()
    );
    dst.cli()
        .env("PATH", path)
        .args(["sessions", "import", "--to", "claude-code"])
        .arg(src.path("out"))
        .assert()
        .success()
        .stdout(contains("Nothing was started"));
    assert!(!marker.exists(), "a tool was started");
    assert_eq!(snapshot(&dst, &dst.path("home/.claude")), before);
    let new: Vec<_> = tree(&dst.path("home/.claude")).into_keys().collect();
    assert_eq!(new.len(), 1);
    assert!(new[0].ends_with("11111111-aaaa-bbbb-cccc-222222222222.jsonl"));
    assert!(!dst.state().exists());
}

#[test]
fn only_the_same_tool_can_import() {
    let sb = Sandbox::empty();
    seed(&sb);
    export(&sb, "claude-code", "c").assert().success();
    export(&sb, "codex", "x").assert().success();
    let dst = Sandbox::empty();
    dst.cli()
        .args(["sessions", "import", "--to", "codex"])
        .arg(sb.path("c"))
        .assert()
        .failure()
        .stderr(contains("claude-code").and(contains("refused")));
    dst.cli()
        .args(["sessions", "import", "--to", "claude-code"])
        .arg(sb.path("x"))
        .assert()
        .failure()
        .stderr(contains("codex").and(contains("refused")));
    assert!(!dst.path("home/.codex").exists() && !dst.path("home/.claude").exists());
    // Tools without a documented path are not accepted at all.
    sb.cli()
        .args(["sessions", "export", "--from", "cursor", "-o"])
        .arg(sb.path("n"))
        .assert()
        .failure();
}

#[test]
fn existing_sessions_are_not_overwritten_without_force() {
    let sb = Sandbox::empty();
    seed(&sb);
    export(&sb, "codex", "out").assert().success();
    let dst = Sandbox::empty();
    write(&dst, &format!("home/{CODEX_REL}"), "my own rollout\n");
    dst.cli()
        .args(["sessions", "import", "--to", "codex"])
        .arg(sb.path("out"))
        .assert()
        .failure()
        .stderr(contains("--force"));
    assert_eq!(dst.read(&format!("home/{CODEX_REL}")), "my own rollout\n");
    dst.cli()
        .args(["sessions", "import", "--to", "codex", "--force"])
        .arg(sb.path("out"))
        .assert()
        .success()
        .stderr(contains("replacing"));
    assert_ne!(dst.read(&format!("home/{CODEX_REL}")), "my own rollout\n");
}

#[test]
fn export_refuses_a_non_empty_directory_without_force() {
    let sb = Sandbox::empty();
    seed(&sb);
    write(&sb, "out/keep.txt", "mine");
    export(&sb, "claude-code", "out")
        .assert()
        .failure()
        .stderr(contains("--force"));
    assert_eq!(sb.read("out/keep.txt"), "mine");
    export(&sb, "claude-code", "out")
        .arg("--force")
        .assert()
        .success()
        .stderr(contains("not empty"));
    assert_eq!(sb.read("out/keep.txt"), "mine");
}

#[test]
fn project_filter_picks_one_claude_project() {
    let sb = Sandbox::empty();
    seed(&sb);
    write(
        &sb,
        "home/.claude/projects/-other/22222222.jsonl",
        &transcript(),
    );
    export(&sb, "claude-code", "out")
        .args(["--project", "-work-demo"])
        .assert()
        .success()
        .stdout(contains("Exported 1"));
    export(&sb, "claude-code", "all")
        .assert()
        .success()
        .stdout(contains("Exported 2"));
    export(&sb, "claude-code", "none")
        .args(["--project", "-nope"])
        .assert()
        .failure();
    export(&sb, "codex", "cx")
        .args(["--project", "x"])
        .assert()
        .failure();
}

#[test]
fn locations_are_honoured_and_the_home_flag_sandboxes_them() {
    let sb = Sandbox::empty();
    let cfg = sb.path("cfg");
    let rel = "projects/-work-demo/33333333.jsonl";
    write(&sb, &format!("cfg/{rel}"), &transcript());
    // Without --home, CLAUDE_CONFIG_DIR is the source.
    let mut c = sb.cli();
    c.env_remove("KITBAG_HOME")
        .env("HOME", sb.home())
        .env("USERPROFILE", sb.home())
        .env("CLAUDE_CONFIG_DIR", &cfg)
        .args(["sessions", "export", "--from", "claude-code", "-o"])
        .arg(sb.path("out"))
        .assert()
        .success()
        .stdout(contains("Exported 1"));
    // ... and the destination of an import.
    let dst = Sandbox::empty();
    let dcfg = dst.path("dcfg");
    let mut c = dst.cli();
    c.env_remove("KITBAG_HOME")
        .env("HOME", dst.home())
        .env("USERPROFILE", dst.home())
        .env("CLAUDE_CONFIG_DIR", &dcfg)
        .args(["sessions", "import", "--to", "claude-code"])
        .arg(sb.path("out"))
        .assert()
        .success();
    assert!(dcfg.join(rel).is_file());
    assert!(!dst.path("home/.claude").exists());
    // With --home the variable is ignored, as for every command.
    let dst2 = Sandbox::empty();
    dst2.cli()
        .env("CLAUDE_CONFIG_DIR", dst2.path("decoy"))
        .args(["sessions", "import", "--to", "claude-code"])
        .arg(sb.path("out"))
        .assert()
        .success();
    assert!(dst2.path("home/.claude").join(rel).is_file());
    assert!(!dst2.path("decoy").exists());
}

#[test]
fn a_tampered_manifest_cannot_write_outside_the_tools_directory() {
    let sb = Sandbox::empty();
    seed(&sb);
    export(&sb, "claude-code", "out").assert().success();
    let m = sb.read("out/kitbag-sessions.json");
    let m = m.replace("projects/-work-demo/", "projects/../../");
    fs::write(sb.path("out/kitbag-sessions.json"), m).unwrap();
    let dst = Sandbox::empty();
    dst.cli()
        .args(["sessions", "import", "--to", "claude-code"])
        .arg(sb.path("out"))
        .assert()
        .failure();
    assert!(tree(&dst.path("home")).is_empty());
}
