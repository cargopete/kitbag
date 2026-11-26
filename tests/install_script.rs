//! The install scripts download release assets by name, so their names must match
//! what `.github/workflows/release.yml` (taiki-e/upload-rust-binary-action with
//! `checksum: sha256`) uploads: `kitbag-<target>.tar.gz` / `.zip` and
//! `kitbag-<target>.sha256`. v0.1.0-rc.1 shipped with the scripts asking for
//! `kitbag-<target>.tar.gz.sha256`, which doesn't exist, so every install failed.
use std::fs;
use std::path::Path;

#[cfg(unix)]
const UNIX_TARGETS: &[&str] = &[
    "x86_64-unknown-linux-musl",
    "aarch64-unknown-linux-musl",
    "x86_64-apple-darwin",
    "aarch64-apple-darwin",
];

fn repo_file(name: &str) -> String {
    fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join(name)).unwrap()
}

#[test]
fn install_scripts_ask_for_the_checksum_names_the_release_uploads() {
    let sh = repo_file("install.sh");
    let ps1 = repo_file("install.ps1");
    for (name, text) in [("install.sh", &sh), ("install.ps1", &ps1)] {
        assert!(
            !text.contains(".tar.gz.sha256") && !text.contains(".zip.sha256"),
            "{name} asks for <archive>.sha256, but the release uploads kitbag-<target>.sha256"
        );
        assert!(
            !text.contains("$archive.sha256"),
            "{name} derives the checksum name from the archive name"
        );
    }
    assert!(ps1.contains("-pc-windows-msvc.sha256"), "install.ps1");
}

#[test]
fn install_ps1_fails_closed_and_replaces_atomically() {
    // The script is not run here (no PowerShell needed); this checks its text.
    let ps1 = repo_file("install.ps1");
    assert!(
        ps1.contains("-notmatch '^[0-9a-f]{64}$'"),
        "checksum shape check"
    );
    assert!(
        ps1.contains("Get-Command Get-FileHash"),
        "missing hasher check"
    );
    assert!(ps1.contains("[System.IO.File]::Replace("), "atomic replace");
    assert!(
        !ps1.contains("Copy-Item $exe.FullName (Join-Path $dir 'kitbag.exe')"),
        "copies straight over the installed exe"
    );
}

/// A fake release server: `serve/v9.9.9/kitbag-<target>.tar.gz` plus its
/// checksum file, and a `curl` in `bin/` that serves them.
#[cfg(unix)]
struct Release {
    tmp: tempfile::TempDir,
}

#[cfg(unix)]
impl Release {
    fn new(version_line: &str) -> Release {
        use sha2::{Digest, Sha256};
        use std::process::Command;

        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        let serve = root.join("serve/v9.9.9");
        let stage = root.join("stage");
        fs::create_dir_all(&serve).unwrap();
        fs::create_dir_all(&stage).unwrap();
        let fake = stage.join("kitbag");
        write_exec(&fake, &format!("#!/bin/sh\necho '{version_line}'\n"));
        for target in UNIX_TARGETS {
            let archive = format!("kitbag-{target}.tar.gz");
            let st = Command::new("tar")
                .arg("-czf")
                .arg(serve.join(&archive))
                .arg("-C")
                .arg(&stage)
                .arg("kitbag")
                .status()
                .unwrap();
            assert!(st.success());
            let hash = hex::encode(Sha256::digest(fs::read(serve.join(&archive)).unwrap()));
            // Same shape as upload-rust-binary-action: "<hash>  <archive>".
            fs::write(
                serve.join(format!("kitbag-{target}.sha256")),
                format!("{hash}  {archive}\n"),
            )
            .unwrap();
        }
        // A `curl` that serves .../releases/download/<tag>/<file> from `serve/..`.
        let bin = root.join("bin");
        fs::create_dir_all(&bin).unwrap();
        write_exec(
            &bin.join("curl"),
            &format!(
                "#!/bin/sh\nurl=''; out=''\nwhile [ $# -gt 0 ]; do case \"$1\" in -o) out=\"$2\"; shift;; https://*) url=\"$1\";; esac; shift; done\nf=\"{}/${{url##*/releases/download/}}\"\n[ -f \"$f\" ] || exit 22\ncp \"$f\" \"$out\"\n",
                root.join("serve").display()
            ),
        );
        Release { tmp }
    }

    fn root(&self) -> &Path {
        self.tmp.path()
    }

    fn bin(&self) -> std::path::PathBuf {
        self.root().join("bin")
    }

    fn checksum_files(&self) -> Vec<std::path::PathBuf> {
        UNIX_TARGETS
            .iter()
            .map(|t| self.root().join(format!("serve/v9.9.9/kitbag-{t}.sha256")))
            .collect()
    }

    /// Runs install.sh with `bin/` in front of `path`.
    fn install(&self, dest: &Path, path: &str) -> (bool, String) {
        let out = std::process::Command::new("/bin/sh")
            .arg(Path::new(env!("CARGO_MANIFEST_DIR")).join("install.sh"))
            .env("PATH", format!("{}:{path}", self.bin().display()))
            .env("KITBAG_VERSION", "v9.9.9")
            .env("KITBAG_INSTALL_DIR", dest)
            .output()
            .unwrap();
        (
            out.status.success(),
            String::from_utf8_lossy(&out.stderr).into_owned(),
        )
    }
}

#[cfg(unix)]
fn write_exec(path: &Path, text: &str) {
    use std::os::unix::fs::PermissionsExt;
    fs::write(path, text).unwrap();
    fs::set_permissions(path, fs::Permissions::from_mode(0o755)).unwrap();
}

#[cfg(unix)]
fn system_path() -> String {
    std::env::var("PATH").unwrap_or_default()
}

/// A CPU or OS the script doesn't cover; nothing to check there.
#[cfg(unix)]
fn unsupported(stderr: &str) -> bool {
    stderr.contains("unsupported")
}

#[cfg(unix)]
#[test]
fn install_sh_installs_from_release_shaped_assets() {
    let release = Release::new("kitbag 9.9.9");
    let dest = release.root().join("installed");
    let (ok, stderr) = release.install(&dest, &system_path());
    if unsupported(&stderr) {
        return;
    }
    assert!(ok, "install.sh failed:\n{stderr}");
    assert!(stderr.contains("installed kitbag 9.9.9"), "{stderr}");
    assert!(dest.join("kitbag").is_file());
}

#[cfg(unix)]
#[test]
fn install_sh_replaces_an_existing_binary_by_renaming() {
    use std::io::Read;
    use std::os::unix::fs::MetadataExt;

    let release = Release::new("kitbag 9.9.9");
    let dest = release.root().join("installed");
    fs::create_dir_all(&dest).unwrap();
    let old_text = "#!/bin/sh\necho 'kitbag 0.0.1'\n";
    write_exec(&dest.join("kitbag"), old_text);
    let old_inode = fs::metadata(dest.join("kitbag")).unwrap().ino();
    // A process still holding the old binary open must keep seeing all of it.
    let mut held = fs::File::open(dest.join("kitbag")).unwrap();
    // The old script wrote through a fixed `kitbag.tmp`; a link planted there
    // must not be followed.
    let victim = release.root().join("victim");
    fs::write(&victim, "untouched\n").unwrap();
    std::os::unix::fs::symlink(&victim, dest.join("kitbag.tmp")).unwrap();

    let (ok, stderr) = release.install(&dest, &system_path());
    if unsupported(&stderr) {
        return;
    }
    assert!(ok, "install.sh failed:\n{stderr}");
    assert!(stderr.contains("installed kitbag 9.9.9"), "{stderr}");
    assert_ne!(
        fs::metadata(dest.join("kitbag")).unwrap().ino(),
        old_inode,
        "the binary was overwritten in place instead of renamed over"
    );
    let mut seen = String::new();
    held.read_to_string(&mut seen).unwrap();
    assert_eq!(seen, old_text);
    assert_eq!(fs::read_to_string(&victim).unwrap(), "untouched\n");
    let mut left: Vec<String> = fs::read_dir(&dest)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    left.sort();
    assert_eq!(left, ["kitbag", "kitbag.tmp"], "temp file left behind");
}

#[cfg(unix)]
#[test]
fn install_sh_refuses_an_empty_checksum_even_if_the_hasher_prints_nothing() {
    let release = Release::new("kitbag 9.9.9");
    for f in release.checksum_files() {
        fs::write(f, "").unwrap();
    }
    // A sha256sum that fails without output: before, "" == "" passed the check.
    write_exec(&release.bin().join("sha256sum"), "#!/bin/sh\nexit 1\n");
    let dest = release.root().join("installed");
    let (ok, stderr) = release.install(&dest, &system_path());
    if unsupported(&stderr) {
        return;
    }
    assert!(!ok, "install.sh accepted an empty checksum:\n{stderr}");
    assert!(
        stderr.contains("does not hold a SHA-256 checksum"),
        "{stderr}"
    );
    assert!(!dest.join("kitbag").exists());
}

#[cfg(unix)]
#[test]
fn install_sh_refuses_when_the_hasher_fails() {
    let release = Release::new("kitbag 9.9.9");
    write_exec(&release.bin().join("sha256sum"), "#!/bin/sh\nexit 1\n");
    let dest = release.root().join("installed");
    let (ok, stderr) = release.install(&dest, &system_path());
    if unsupported(&stderr) {
        return;
    }
    assert!(!ok, "install.sh installed without a checksum:\n{stderr}");
    assert!(stderr.contains("could not compute the SHA-256"), "{stderr}");
    assert!(!dest.join("kitbag").exists());
}

#[cfg(unix)]
#[test]
fn install_sh_refuses_without_a_hasher() {
    // A PATH with everything install.sh uses except sha256sum and shasum.
    let release = Release::new("kitbag 9.9.9");
    let tools = release.root().join("tools");
    fs::create_dir_all(&tools).unwrap();
    for tool in [
        "uname", "tar", "gzip", "mkdir", "mktemp", "cut", "tr", "rm", "cp", "chmod", "mv", "find",
        "head", "sh",
    ] {
        let Some(found) = std::env::split_paths(&system_path())
            .map(|d| d.join(tool))
            .find(|p| p.is_file())
        else {
            return; // an unusual system; the other tests cover the script
        };
        std::os::unix::fs::symlink(found, tools.join(tool)).unwrap();
    }
    let dest = release.root().join("installed");
    let (ok, stderr) = release.install(&dest, &tools.display().to_string());
    if unsupported(&stderr) {
        return;
    }
    assert!(!ok, "install.sh installed without verifying:\n{stderr}");
    assert!(
        stderr.contains("sha256sum or shasum is required"),
        "{stderr}"
    );
    assert!(!dest.join("kitbag").exists());
}
