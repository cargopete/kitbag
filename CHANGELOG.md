# Changelog

All notable changes to this project are documented here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and the project uses
[Semantic Versioning](https://semver.org/).

## [Unreleased]

### Added

- Portable profile format (`kitbag.toml`, `instructions.md`, `rules/`, `commands/`, `skills/`), format version 1.
- Adapters for Claude Code, Cursor, OpenAI Codex CLI, Gemini CLI and Claude Desktop, at user and project scope.
- `init`, `capture`, `apply`, `diff`, `undo`, `history`, `doctor` and `list-targets` commands.
- Secret redaction on capture, native env-reference rendering on apply, masking in diffs.
- Format-preserving JSON/JSONC and TOML merges, managed blocks in shared markdown files.
- Transactional apply with backups, rollback on failure and undo.
- Profiles from git URLs (`github:owner/repo`, `https://…`, `git@…`, `file://…`).

### Fixed

- xAI, Groq, Perplexity, Tavily, Supabase, Replicate, OpenRouter and Firecrawl keys are recognised (free text, env values, headers and args).
- Free-text redaction of URL credentials replaces only the password, instead of swallowing the host and the Markdown after it. Placeholder passwords in documentation examples are left alone.
- Header arguments without a space after the colon (`X-Api-Key:…`, `Authorization:Bearer …`, as in the mcp-remote docs) no longer make capture refuse to write the profile after redacting them.
- Applying a captured profile back to the tool it came from no longer overwrites the original files (or managed blocks) with their redacted copies; files that differ in other ways get a warning about the `[REDACTED]` markers.
- Capture decodes UTF-16 (with a byte-order mark) and drops a UTF-8 byte-order mark in text items; files in other encodings are skipped with a warning instead of aborting the capture.
- A UTF-8 byte-order mark at the start of a JSON/JSONC/TOML config no longer fails with "invalid JSON"; it is kept when the file is written back.
- Capture follows symbolic links inside a skill only when they stay inside the skill. Escaping links, loops, dangling links and unreadable entries are skipped with a warning.
- Capture skips files over the 2 MiB profile limit (and skills beyond 200 files) with a warning, so it no longer writes profiles that `apply` refuses.
- Capture lists Claude Code subagents, output styles and plugins it found but doesn't carry, and drops an `outputStyle` setting that names a custom style.
- Malformed server entries (`"mcpServers": []`, a server that isn't an object) are reported instead of silently ignored.
- `capture` with several tools (`--from all`, `detected` or a list) reports a tool whose config can't be read and captures the rest; it fails only if nothing could be read. A single `--from` still fails loudly.
