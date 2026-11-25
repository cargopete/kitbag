---
description: Rust conventions
globs:
  - "**/*.rs"
---

- Use `?` and `anyhow::Context` for errors in binaries; `thiserror` in libraries.
- No `unwrap()` outside tests and examples.
- Keep `cargo clippy --all-targets -- -D warnings` clean.
