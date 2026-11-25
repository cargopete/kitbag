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
