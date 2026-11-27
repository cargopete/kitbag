//! Shared helpers for integration tests. Every test works on copies of the
//! fixtures in a temporary directory; nothing touches the real home directory.
#![allow(dead_code)]

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

use kitbag::adapters::{self, Adapter, ApplyOptions, Env, Os, Scope, SecretsMode};
use kitbag::engine::{self, Plan};
use kitbag::model::Profile;
use tempfile::TempDir;

pub fn fixtures() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures")
}

pub fn copy_dir(from: &Path, to: &Path) {
    fs::create_dir_all(to).unwrap();
    for e in fs::read_dir(from).unwrap() {
        let e = e.unwrap();
        let dest = to.join(e.file_name());
        if e.file_type().unwrap().is_dir() {
            copy_dir(&e.path(), &dest);
        } else {
            fs::copy(e.path(), dest).unwrap();
        }
    }
}

/// A sandbox with `home/` and `project/` directories.
pub struct Sandbox {
    pub tmp: TempDir,
}

impl Sandbox {
    /// Empty home and project.
    pub fn empty() -> Self {
        let tmp = tempfile::tempdir().unwrap();
        fs::create_dir_all(tmp.path().join("home")).unwrap();
        fs::create_dir_all(tmp.path().join("project")).unwrap();
        Sandbox { tmp }
    }

    /// Home and project populated from the fixtures.
    pub fn fixtures() -> Self {
        let s = Self::empty();
        copy_dir(&fixtures().join("home"), &s.home());
        copy_dir(&fixtures().join("project"), &s.project());
        s
    }

    pub fn home(&self) -> PathBuf {
        self.tmp.path().join("home")
    }
    pub fn project(&self) -> PathBuf {
        self.tmp.path().join("project")
    }
    pub fn state(&self) -> PathBuf {
        self.tmp.path().join("state")
    }
    pub fn path(&self, rel: &str) -> PathBuf {
        self.tmp.path().join(rel)
    }
    pub fn read(&self, rel: &str) -> String {
        fs::read_to_string(self.path(rel)).unwrap_or_else(|e| panic!("reading {rel}: {e}"))
    }
    pub fn json(&self, rel: &str) -> serde_json::Value {
        serde_json::from_str(&self.read(rel)).unwrap()
    }

    pub fn env(&self, scope: Scope) -> Env {
        self.env_with(scope, &[])
    }

    pub fn env_with(&self, scope: Scope, vars: &[(&str, &str)]) -> Env {
        Env {
            scope,
            home: self.home(),
            project: self.project(),
            vars: vars
                .iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect::<HashMap<_, _>>(),
            locations: HashMap::new(),
            os: Os::Linux,
        }
    }

    /// Run the CLI inside the sandbox with a clean environment.
    pub fn cli(&self) -> assert_cmd::Command {
        let mut c = assert_cmd::Command::cargo_bin("kitbag").unwrap();
        c.env_clear()
            .env("PATH", std::env::var("PATH").unwrap_or_default())
            .env("KITBAG_HOME", self.home())
            .env("KITBAG_STATE_DIR", self.state())
            .env("NO_COLOR", "1")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_CONFIG_GLOBAL", null_device())
            .current_dir(self.tmp.path());
        if cfg!(windows) {
            for k in ["SYSTEMROOT", "TEMP", "TMP", "USERPROFILE"] {
                if let Ok(v) = std::env::var(k) {
                    c.env(k, v);
                }
            }
        }
        c
    }
}

impl Sandbox {
    /// Write `files` (relative path, content) into a new git repository at
    /// `rel` and return its `file://` URL: a remote profile for tests.
    pub fn git_profile(&self, rel: &str, files: &[(&str, &str)]) -> String {
        let repo = self.path(rel);
        for (name, content) in files {
            let p = repo.join(name);
            fs::create_dir_all(p.parent().unwrap()).unwrap();
            fs::write(p, content).unwrap();
        }
        git(&repo, &["init", "-q"]);
        git(&repo, &["add", "."]);
        git(&repo, &["commit", "-q", "-m", "profile"]);
        file_url(&repo)
    }
}

/// Claude Desktop's config file relative to the sandbox root, for the CLI,
/// which uses the real OS's location (under `--home`, `APPDATA` is ignored).
pub fn desktop_config() -> &'static str {
    if cfg!(target_os = "macos") {
        "home/Library/Application Support/Claude/claude_desktop_config.json"
    } else if cfg!(windows) {
        "home/AppData/Roaming/Claude/claude_desktop_config.json"
    } else {
        "home/.config/Claude/claude_desktop_config.json"
    }
}

pub fn null_device() -> &'static str {
    if cfg!(windows) { "NUL" } else { "/dev/null" }
}

pub fn has_git() -> bool {
    let found = std::process::Command::new("git")
        .arg("--version")
        .output()
        .is_ok();
    if !found {
        eprintln!("git not installed; skipping");
    }
    found
}

pub fn file_url(path: &Path) -> String {
    let p = path.display().to_string().replace('\\', "/");
    if p.starts_with('/') {
        format!("file://{p}")
    } else {
        format!("file:///{p}")
    }
}

pub fn git(dir: &Path, args: &[&str]) {
    let ok = std::process::Command::new("git")
        .args([
            "-c",
            "user.name=t",
            "-c",
            "user.email=t@example.com",
            "-c",
            "init.defaultBranch=main",
            "-c",
            "commit.gpgsign=false",
        ])
        .args(args)
        .current_dir(dir)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", null_device())
        .status()
        .unwrap()
        .success();
    assert!(ok, "git {args:?}");
}

/// Run git in `dir` like [`git`] and return its trimmed output.
pub fn git_out(dir: &Path, args: &[&str]) -> String {
    let out = std::process::Command::new("git")
        .args(args)
        .current_dir(dir)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", null_device())
        .output()
        .unwrap();
    assert!(out.status.success(), "git {args:?}");
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

pub fn targets(ids: &[&str]) -> Vec<&'static dyn Adapter> {
    ids.iter().map(|id| adapters::get(id).unwrap()).collect()
}

pub fn opts(profile: &Profile) -> ApplyOptions {
    ApplyOptions {
        overwrite: true,
        secrets: SecretsMode::Resolve,
        block_id: kitbag::model::slugify(&profile.name),
        applied_servers: Vec::new(),
        source: None,
        git_removals: None,
        hold: Default::default(),
    }
}

pub fn plan(profile: &Profile, ids: &[&str], env: &Env) -> Plan {
    engine::plan(profile, &targets(ids), env, &opts(profile)).unwrap()
}

/// Plan and write to disk.
pub fn apply(sb: &Sandbox, profile: &Profile, ids: &[&str], env: &Env) -> Plan {
    let p = plan(profile, ids, env);
    engine::commit(&p, "test", &sb.state()).unwrap();
    p
}

pub fn capture(ids: &[&str], env: &Env) -> engine::CaptureResult {
    engine::capture(&targets(ids), env, "test").unwrap()
}

/// The planned content of `path`, as text.
pub fn planned(p: &Plan, path: &Path) -> Option<String> {
    p.changes
        .iter()
        .find(|c| c.path == path)
        .and_then(|c| c.after.as_ref())
        .map(|b| String::from_utf8(b.clone()).unwrap())
}

pub fn all_text_under(dir: &Path) -> String {
    let mut out = String::new();
    for rel in kitbag::plan_fs::walk_files(dir, 10_000).unwrap() {
        if let Ok(t) = fs::read_to_string(kitbag::plan_fs::join_rel(dir, &rel)) {
            out.push_str(&t);
        }
    }
    out
}
