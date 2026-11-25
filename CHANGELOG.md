# Changelog

All notable changes to this project are documented here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and the project uses
[Semantic Versioning](https://semver.org/).

## [Unreleased]

### Security

- Capture redacts secrets in the frontmatter of rules and commands (`description`, `allowed-tools` ...) and in Claude Code permission rules (`Bash(curl -H 'X-Api-Key: …' *)`), and `doctor` flags any left there. Free text also loses values assigned to secret-sounding names (`export DB_PASSWORD=…`, `password: …`, `--token …`, `Authorization: Bearer …`). Permission rules holding `[REDACTED]` markers are not applied back (#5).
- More secret shapes in MCP config are redacted on capture: any value under a secret-sounding name however short (`DB_PASSWORD = "rootE"`), more names (`PGPASSWORD`, `*_PASSPHRASE`, `*_SECRET_ID`, `APITOKEN`, `*AUTH`, custom headers such as `X-Auth` or `X-Session`), the literal in `${VAR:-literal}` (kept as `${VAR}`), slash-prefixed tokens, and password-only URL userinfo (`redis://:pass@host`). Empty values, booleans, small numbers, file paths and `${VAR}` references are still left alone (#6).
- Tokens in URLs are redacted wherever a URL appears (`url`, args, `--flag=URL`, `NAME=URL`, env values, free text): secret-named query parameters, Slack and Discord webhook paths, Zapier-style `/mcp/s/<token>/` paths, random-looking path segments in MCP config, and a token used as the username (`https://<token>@host`, HTTP(S) and WebSocket only; `git@` and short usernames stay) (#7).
- A value holding an env reference is no longer exempt from redaction as a whole: only the `${…}` spans are, and literal secrets next to them (`api_key=…-${X}`, `Bearer … ${X}`, `Authorization:Bearer … ${X}` args, `postgres://u:…${X}@db`) are redacted (#8).

- Profiles from git sources no longer add any `permissions.allow` rule unless `--allow-risky-settings` (new alias: `--trust-source`) is given; the plan lists the rules it left out. `ask` and `deny` rules still apply. The broad-rule warning for local profiles now ignores case and extra whitespace, and also names `Bash(*:*)`, whole-file-system `Read`/`Edit`/`Write` rules, interpreters, package runners, containers and piping into a shell (#1).
- Profiles from git sources no longer install commands or skills that run shell snippets (``!`cmd` ``, Gemini CLI's `!{cmd}`), and `hooks` and `allowed-tools` are removed from the frontmatter of the rest, unless `--allow-risky-settings` is given. Local profiles get a warning listing hooks, tool grants and shell snippets (#2).
- `--secrets` defaults to `never` for profiles from git sources, so local env vars are no longer resolved into servers a remote profile defines (Claude Desktop, parts of Codex); pass `--secrets resolve` to opt in. Every plan lists each MCP server it adds or changes, with its command and arguments or URL and the names of its env vars and headers. `doctor` and `apply` flag a `${VAR:-fallback}` whose literal fallback looks like a secret (#3).
- Replacing an MCP server whose command or URL changed no longer carries over the old entry's other keys (`trust`, `headersHelper`, `oauth`, `bearer_token_env_var`, `env_http_headers` ...); the entry is replaced as a whole and the plan names the dropped keys. Servers that keep their command or URL still keep your own keys (#4).
- With `--project`, every write must resolve inside the real (canonical) project root, so a symbolic link can no longer redirect it elsewhere, also when the project path itself goes through a link; the apply stops before writing anything. `capture --project` skips, with a warning, project files and directories (`CLAUDE.md`, rules, MCP config ...) that are links to somewhere outside the project, such as `~/.aws/credentials`.

## [0.1.0] - 2026-11-25

### Added

- Portable profile format (`kitbag.toml`, `instructions.md`, `rules/`, `commands/`, `skills/`), format version 1.
- Adapters for Claude Code, Cursor, OpenAI Codex CLI, Gemini CLI and Claude Desktop, at user and project scope.
- `init`, `capture`, `apply`, `diff`, `undo`, `history`, `doctor` and `list-targets` commands.
- Secret redaction on capture, native env-reference rendering on apply, masking in diffs.
- Format-preserving JSON/JSONC and TOML merges, managed blocks in shared markdown files.
- Transactional apply with backups, rollback on failure and undo.
- Profiles from git URLs (`github:owner/repo`, `https://…`, `git@…`, `file://…`).

### Changed

- Profiles from git sources no longer apply settings that let agents act without asking (permissive `defaultMode`, `additionalDirectories`, broad allow rules, Codex `approval_policy = never` / `sandbox_mode = danger-full-access`) unless `--allow-risky-settings` is given. Local profiles are unchanged. `additionalDirectories` and broad rules with arguments such as `Bash(rm -rf *)` are now flagged as risky.

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
- `install.sh` and `install.ps1` download `kitbag-<target>.sha256`, the checksum name the release workflow uploads (found in the v0.1.0-rc.1 dry run).

[Unreleased]: https://github.com/cargopete/kitbag/compare/v0.1.0...HEAD
[0.1.0]: https://github.com/cargopete/kitbag/releases/tag/v0.1.0
