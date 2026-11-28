# REVIEW.md

Instructions for Claude's managed Code Review on pull requests in this repository. The local `/code-review`
command does not read this file; it reads `CLAUDE.md`. Put rules you want in both places in `CLAUDE.md`.

## What matters most here
kitbag captures and applies a personal AI-coding-tool setup across many tools. It writes into users' real config files, so correctness and reversibility come first.

Flag these as Important:
- **Backups and undo.** Every write to a user's file must be planned, shown, backed up and undoable (`txn.rs`, `plan_fs.rs`, `engine.rs`). A write path that skips the backup or breaks `kitbag undo` is a finding.
- **Secrets.** Secrets stay as env refs and are redacted on capture. A change in `secrets.rs`, `oauth*.rs`, `mcp_tokens.rs` or any adapter that could write a literal secret to a profile or a log is a finding. Tests use obviously fake values.
- **Config edits must preserve the user's file.** The `edit/` modules (JSON, TOML, YAML, block) must keep comments, ordering and unrelated keys. Flag any rewrite that reformats a whole file.
- **Windows and macOS.** CI runs on Linux, macOS and Windows. Flag path separators, line endings, permission bits, and TOML or JSON backslash escaping. Unix-only behaviour and tests need `#[cfg(unix)]`.
- **Tests must not touch the real home directory.** Use the helpers in `tests/common`.
- **Behaviour change needs a CHANGELOG entry** under `## [Unreleased]`, ending with the issue number, and a README update only where that behaviour is documented.
- **Installers (`install.sh`, `install.ps1`).** Treat as supply-chain code. Flag unpinned downloads and missing checksum checks.
- **No AI attribution** anywhere: no Co-authored-by trailer, no "Generated with" line.

## Calibration
- Style and naming: Nit, at most 3 per review.
- `cargo fmt`, `clippy -D warnings`, `cargo test --locked` and `cargo doc` are CI's job; do not repeat what they catch.

## Skip
`Cargo.lock`, `docs/demo.gif`, files under `tests/fixtures`.
