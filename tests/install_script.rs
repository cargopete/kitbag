//! The install scripts download release assets by name, so their names must match
//! what `.github/workflows/release.yml` (taiki-e/upload-rust-binary-action with
//! `checksum: sha256`) uploads: `kitbag-<target>.tar.gz` / `.zip` and
//! `kitbag-<target>.sha256`. v0.1.0-rc.1 shipped with the scripts asking for
//! `kitbag-<target>.tar.gz.sha256`, which doesn't exist, so every install failed.
use std::fs;
use std::path::Path;

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

#[cfg(unix)]
#[test]
fn install_sh_installs_from_release_shaped_assets() {
    use sha2::{Digest, Sha256};
    use std::os::unix::fs::PermissionsExt;
    use std::process::Command;

    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    let serve = root.join("serve/v9.9.9");
    let stage = root.join("stage");
    fs::create_dir_all(&serve).unwrap();
    fs::create_dir_all(&stage).unwrap();
    let fake = stage.join("kitbag");
    fs::write(&fake, "#!/bin/sh\necho 'kitbag 9.9.9'\n").unwrap();
    fs::set_permissions(&fake, fs::Permissions::from_mode(0o755)).unwrap();
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
    let curl = bin.join("curl");
    fs::write(
        &curl,
        format!(
            "#!/bin/sh\nurl=''; out=''\nwhile [ $# -gt 0 ]; do case \"$1\" in -o) out=\"$2\"; shift;; https://*) url=\"$1\";; esac; shift; done\nf=\"{}/${{url##*/releases/download/}}\"\n[ -f \"$f\" ] || exit 22\ncp \"$f\" \"$out\"\n",
            root.join("serve").display()
        ),
    )
    .unwrap();
    fs::set_permissions(&curl, fs::Permissions::from_mode(0o755)).unwrap();

    let dest = root.join("installed");
    let path = format!(
        "{}:{}",
        bin.display(),
        std::env::var("PATH").unwrap_or_default()
    );
    let out = Command::new("sh")
        .arg(Path::new(env!("CARGO_MANIFEST_DIR")).join("install.sh"))
        .env("PATH", path)
        .env("KITBAG_VERSION", "v9.9.9")
        .env("KITBAG_INSTALL_DIR", &dest)
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&out.stderr);
    if stderr.contains("unsupported") {
        return; // a CPU or OS the script doesn't cover; nothing to check here
    }
    assert!(out.status.success(), "install.sh failed:\n{stderr}");
    assert!(stderr.contains("installed kitbag 9.9.9"), "{stderr}");
    assert!(dest.join("kitbag").is_file());
}
