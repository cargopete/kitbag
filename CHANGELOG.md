# Changelog

All notable changes to this project are documented here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and the project uses
[Semantic Versioning](https://semver.org/).

## [Unreleased]

## [0.1.3] - 2026-11-26

### Added

- New target `antigravity` for Google Antigravity (app, IDE and CLI), for MCP servers and instructions at user and project scope. Servers go to `~/.gemini/config/mcp_config.json` or `.agents/mcp_config.json`, with `serverUrl` for remote servers and `disabled` for disabled ones; your own keys on an entry (`disabledTools`, `timeoutSeconds` ...) are kept. Antigravity's docs describe no env-reference syntax, so references are resolved at apply time like Claude Desktop (`--secrets never` keeps placeholders) and the file ends up mode 0600; SSE servers are skipped. Instructions go to `~/.gemini/GEMINI.md`, the same file and managed block Gemini CLI uses, so applying to both writes one copy, and to `AGENTS.md` in a project, shared with Codex and Cursor. Capture also reads the other instructions files Antigravity loads. Rules, skills and workflows are not supported yet (#27).
- Per-tool MCP servers: a server in the profile can list the only tools it applies to (`targets = ["claude-code"]`) or the tools it is kept out of (`disabled_on = ["cursor"]`), so one shared profile can keep a heavy server out of the tools that don't need it. Setting both or an empty `targets` is an error. An unknown target name (a typo, or a tool from a newer kitbag) is a warning listing the valid targets and matches no tool, so the profile still loads. `only` and `except` still load as older names, an empty `only` still means no restriction (with a warning), and setting a field under both names is an error naming both; capture writes the new names. `apply`, `diff` and `sync` note each server they skip (`skipped on cursor: disabled in profile`). A server kitbag applied to a tool before and the profile now disables there is removed from it in the same undoable apply, but only when kitbag's records (new `applied-servers.json` in the state directory) show the same profile, by name and source (local directory or git URL), wrote that entry and it is unchanged since; servers added by hand, by another profile or edited are left, and the plan says why. From a git source a removal needs `--allow-git-mcp` or `--trust-source`, like an addition, and is listed as not removed otherwise. `status` shows such servers per tool. `capture --force` into an existing profile keeps `targets` / `disabled_on` (#33).
- `claude` is accepted as an alias for the `claude-code` target in `--to` and `--from` (`apply`, `diff`, `status`, `sync`, `doctor`, `capture`), in an MCP server's `targets` / `disabled_on` (and their older names `only` / `except`) and as a `[settings.claude]` table, read as `[settings.claude-code]` (having both tables is an error naming both). It is turned into `claude-code` as soon as it is read, so output, state files and captured profiles still say `claude-code`. A target listed more than once in `targets` / `disabled_on` (`["cursor", "cursor"]`, `["claude", "claude-code"]`, or across the old and new names) is kept once. It never matches `claude-desktop` (#55).

### Changed

- `kitbag sync PROFILE` holds back only the managed instructions block or rule file that was edited by hand, not the whole tool: one edit to `~/.claude/CLAUDE.md` no longer keeps new MCP servers, rules, commands and skills from reaching Claude Code. Everything else is applied in the same undoable transaction, the plan lists each held-back item (`Skipped: claude-code instructions`) with the `--from <target>` / `--overwrite` hint, and sync still exits with status 1 when it held anything back. A held-back item keeps its old record, so `status` still shows it as edited. `--from` and `--overwrite` work as before (#43).

### Fixed

- A bare `--allow-git-mcp` placed before the source (`kitbag apply --allow-git-mcp github:me/profile`) no longer takes the source as a server name; it means "allow all". A value that looks like a path or URL, or names a profile directory (one with a `kitbag.toml`), is treated as the source. `--allow-git-mcp NAME[,NAME]` and `--allow-git-mcp=NAME` work as before. (#41)
- `apply` and `sync` no longer say "up to date" about a target they skipped; they print `Skipped: <targets>` and the notes say why. With `--no-overwrite`, an apply that kept existing items says "Nothing written; kept N existing item(s)" instead of "up to date" (#38, #42).
- `kitbag undo ""` (or only whitespace) is an error instead of matching any transaction (#38).
- `doctor --to <target>` only checks that target and rejects unknown ones like `apply --to` does, before printing anything (#38).
- Skill scripts keep their executable bit through capture, the profile and apply on Unix. From a git source the bit is only kept with `--trust-source`, and the plan says so otherwise (#38).
- Capturing commands or rules whose nested paths flatten to the same name (`team-build.md` and `team/build.md`) now fails naming both files, instead of silently keeping one (#38).
- `capture --force` into a profile with `.mdc` rules no longer fails looking for a `.md` file (#38).
- `--no-overwrite` now also keeps existing managed instruction blocks and existing skill support files, and reports each one it kept. A profile that no longer has instructions still removes the old managed block (#38).
- Capture matched secrets it redacted to MCP servers by location, so a server whose name contains a dot (`a.b`) also had its secrets counted under server `a`: they were listed twice and could raise a false "different secret values" warning. Each redaction now records its server (#53).
- A JSON config where a key kitbag edits appears twice (two `mcpServers`, two servers with the name of one kitbag writes, a `command` key twice inside such a server, a setting kitbag writes twice) is refused with an error naming the file and the key. kitbag used to edit the first copy while the tools read the last, so `diff --exit-code` reported no change. Duplicates kitbag doesn't edit, such as a top-level `theme` twice or a key twice in another server, are left alone (#38).
- Edits to JSON and TOML configs and to managed blocks in instruction files keep a file's Windows (`\r\n`) line endings. They used to write the lines they added with `\n`. A file is taken to use `\r\n` when most of its line breaks are `\r\n`; its other lines are then written with `\r\n` too (#38).
- Rules, commands, skills and skill files named after a Windows device (`CON`, `nul.md`, `COM1`, `aux.sh` ...), or whose names differ only in case (`Build.md` and `build.md`), are refused when a profile is loaded or captured, on every OS, naming each one: profiles move between machines, and such files can't be created on Windows or are one file on Windows and macOS (#38).
- Backups of files with very long names no longer fail to be written: the file name part of a backup's name is cut to 100 bytes, without trailing dots or spaces, and the number in front keeps it unique (#38).
- Loading a profile reads at most 64 MiB in all, besides the 2 MiB limit per file; a larger profile is refused with an error naming its directory, and capture refuses to write one (#38).
- The README and `--help` now say where state goes with `--home DIR`: `KITBAG_STATE_DIR` if set, else `DIR/.local/state/kitbag`, never `XDG_STATE_HOME` or `LOCALAPPDATA`. A test covers it (#38).
- A profile `name` with control characters (a newline, tab, escape ..., also at the very end) or invisible text-direction and zero-width characters (U+202A to U+202E, U+2066 to U+2069, U+200B to U+200D, U+FEFF) is refused when the profile is loaded, and `init --name` and `capture --name` refuse one before writing anything. Such a name used to be accepted and stored with each apply in the state directory (#38).
- The README no longer says plain `apply` shows a diff: it lists what it will change and asks; `kitbag diff` and `apply --dry-run` show the full diff (#38).

### Security

- Credentials in a git source URL (`https://user:token@host/...`, `https://<token>@host/...`, `file://user:pass@...`) are passed to `git` and nowhere else: the plan, clone errors (including git's own message), `history`, `undo` and the transaction log show the URL without them. `history` and `undo` also hide them in applies recorded by older versions. An SSH URL keeps its username (#37).
- Git sources over cleartext `http://` or `git://` are refused, as is an `https://` source that git ends up fetching over one (a server redirect or a `url.<base>.insteadOf` rewrite), also when `GIT_ALLOW_PROTOCOL` is set in the environment, unless `--allow-insecure-source` is given (#37).
- `undo` checks every backup against the SHA-256 recorded when it was taken. If one changed since the apply, undo stops before restoring anything, names the file and its backup, and asks for `--force`; with `--force` it restores the backup and warns about it. Applies recorded by older versions carry the same hash and are checked too; an entry without one is restored as before. The hash is stored next to the backups, so it catches a backup that changed on its own, not someone who can write to the state directory (#37).
- Diffs mask private keys that span lines (PEM `-----BEGIN ... PRIVATE KEY-----` blocks) in existing config and instruction files. Each key is shown as one line, `•••••••• (private key, sha256:1a2b3c4d)`, so a changed key still shows up as a change. Diffs and plans also mask whatever capture would redact from text (`export DB_PASSWORD=...`, `Bearer ...`, webhook and query tokens) and, as capture does, a value under a secret-sounding name however short (#37).
- Capture redacts hex-only tokens in URL paths (32 hex digits or more, such as `https://mcp.example.com/u/3f9a.../sse` or a classic 40-digit GitHub token), which the random-looking-segment check missed because it needs upper and lower case. Numeric ids, short ids and SHAs, and full commit SHAs (40 or 64 hex digits) after `commit`, `blob`, `tree` and the like or on `raw.githubusercontent.com` are kept (#37).
- Two different secrets captured under the same name no longer end up behind one `${VAR}`. Within one server (an `API_KEY` in `env` and another in a `docker -e` argument, two passwords in two URLs, two tokens in one command) the second gets a numbered variable such as `${API_KEY_2}`, and the same secret repeated keeps its one variable. Across servers the prefixed name gets a number too when it is already taken. The same server captured from two tools with different secret values is now reported, as a server that differs is (#37).

## [0.1.2] - 2026-11-26

### Added

- `kitbag status [PROFILE]` shows, per tool, whether each managed instructions block and rule file is `in sync`, `tool edited`, `profile changed`, `both changed`, `not applied` or `unsupported` (Cursor user rules live in app storage). Applies now record a hash of each one in the state directory. Without a profile it shows what changed since the last apply. `--exit-code` exits 1 on drift, `--json` prints rows (#32).
- `kitbag sync PROFILE` applies profile changes but leaves a tool whose managed instructions were edited by hand alone, and says so. `--from <target>` pulls that tool's edits back into the profile (redacted like capture, refused if anything still looks like a secret) and applies them to the others, in one undoable transaction; `--overwrite` replaces the edits, with backups. From a git source, `--from` is refused (#32).
- Second-machine sync: capture writes paths under the home directory as `~/...` in MCP server commands, arguments, env values and `cwd` and in Claude Code `permissions`, and apply expands them for the machine it runs on. Re-applying to the original machine changes nothing. `kitbag status PROFILE` also lists MCP servers, commands and skills that were added, changed or exist only in the tool, by name. The README has a "Second machine / dotfiles" section with the recommended flow (#31).

### Security

- `undo --force` no longer follows a symbolic link that appeared or changed after the apply. Each apply records where every file's directory really was; undo leaves a file alone, even with `--force`, when its directory now resolves elsewhere or the file is now a link, reverts the rest and exits with status 1. Applies recorded by 0.1.1 are checked against the recorded path: a symbolic link in it at or below the home directory (or the project directory of a project-scope apply) is refused, while links the system keeps above them, such as `/var` -> `/private/var` on macOS, are fine. `init --force` and `capture --force` replace symbolic links inside the profile directory instead of writing through them, and `init` without `--force` treats a dangling link as an existing file (#35).
- MCP servers from git sources are opt-in. Servers a git profile would add or change are not applied by default; the plan lists each one (command and arguments or URL, env var and header names) under "MCP servers not applied (from a git source)". `--allow-git-mcp NAME[,NAME]` (repeatable) applies the named ones, `--allow-git-mcp` without a value all of them, and `--trust-source` / `--allow-risky-settings` implies it. Servers already in place, and local profiles, are unaffected (#36).

## [0.1.1] - 2026-11-25

### Security

- Profiles from git sources are untrusted by default. They no longer add `permissions.allow` rules (#1), install commands or skills that run shell snippets (``!`cmd` ``, Gemini CLI's `!{cmd}`), or keep `hooks` and `allowed-tools` in skill and command frontmatter (#2). The plan lists what was left out; `--allow-risky-settings` (new alias: `--trust-source`) applies it anyway. `ask` and `deny` rules still apply.
- Local env vars are no longer resolved into MCP servers that a git-sourced profile defines (Claude Desktop, parts of Codex); pass `--secrets resolve` to opt in (#3).
- An MCP server whose command, arguments or URL changes is replaced as a whole, so `npx -y evil-pkg` can no longer inherit the `trust`, `headersHelper`, OAuth or credential keys given to `npx -y good-pkg`. The plan names the dropped keys (#4).
- Capture redacts more secrets: in rule, command and skill frontmatter and in Claude Code permission rules (#5); any value under a secret-sounding name however short, more names (`PGPASSWORD`, `*_PASSPHRASE`, `APITOKEN`, `X-Auth` ...), `${VAR:-literal}` fallbacks and password-only URLs such as `redis://:pass@host` (#6); tokens in URLs wherever they appear, including webhook paths and tokens used as the username (#7); and literal secrets next to a `${VAR}` reference in the same value (#8). `doctor` flags anything left behind.
- With `--project`, every write must resolve inside the real project root, so a symbolic link can't redirect it elsewhere; the apply stops before writing anything. `capture --project` skips, with a warning, project files that link outside the project (such as `~/.aws/credentials`).
- Files that `apply` writes a resolved secret into, from your environment or a `${VAR:-fallback}`, end up mode 0600 on Unix, also when they existed with looser permissions. `undo` puts the old mode back.
- Errors about a broken TOML, JSON or JSONC file give the path, line, column and parser message only. They no longer quote the offending line, which could hold a secret.
- Control characters and bidi overrides in profile or config text are printed as escapes (`\x1b`, `\r`, `\u{202e}`) in diffs, plans, notes, `doctor`, `history` and errors, so a profile can't rewrite or hide lines of the review on a terminal. Files are written unchanged.

### Changed

- `--secrets` defaults to `never` for profiles from git sources (#3).
- Every plan lists each MCP server it adds or changes, with its command and arguments or URL and the names of its env vars and headers (#3).
- The broad-permission warning for local profiles ignores case and extra whitespace, and also names `Bash(*:*)`, whole-file-system `Read`/`Edit`/`Write` rules, interpreters, package runners, containers and piping into a shell (#1). Local profiles also get a warning listing hooks, tool grants and shell snippets (#2).
- A changed argument list, including a version bump such as `pkg` to `pkg@latest`, now counts as a different server: your own keys on that entry (such as `timeout`) are dropped, with a note. Differences in env references alone don't count (#4).

### Fixed

- `doctor` and `apply` flag a `${VAR:-fallback}` whose literal fallback looks like a secret, and diffs mask it (#3).
- A secret written from a `${VAR:-fallback}` is no longer described as "resolved from your environment".
- Permission rules holding `[REDACTED]` markers are not applied back to the tool (#5).

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

[Unreleased]: https://github.com/cargopete/kitbag/compare/v0.1.3...HEAD
[0.1.3]: https://github.com/cargopete/kitbag/compare/v0.1.2...v0.1.3
[0.1.2]: https://github.com/cargopete/kitbag/compare/v0.1.1...v0.1.2
[0.1.1]: https://github.com/cargopete/kitbag/compare/v0.1.0...v0.1.1
[0.1.0]: https://github.com/cargopete/kitbag/releases/tag/v0.1.0
