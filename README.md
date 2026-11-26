# kitbag

**Pack your AI agent setup once, unpack it into any coding tool.**

[![CI](https://github.com/cargopete/kitbag/actions/workflows/ci.yml/badge.svg)](https://github.com/cargopete/kitbag/actions/workflows/ci.yml)
[![License: MIT](https://img.shields.io/badge/license-MIT-blue.svg)](LICENSE)

kitbag captures your MCP servers, instructions, rules, slash commands, skills and settings
from the AI coding tools you already use. It stores them as a small, git-friendly **profile** with secrets
redacted and applies that profile to any other tool in one step. Claude Code, Cursor,
OpenAI Codex CLI, Gemini CLI and Claude Desktop are supported, at user and project scope.

> [!IMPORTANT]
> **Status: early release (v0.1).** kitbag works today and is tested against real setups,
> and it's improving quickly. Two things to know while it matures:
>
> - **Secret redaction is best-effort.** It catches the common key formats, secret-named
>   fields, auth headers and URL passwords, but it is known to miss some formats (see the
>   [open security issues](https://github.com/cargopete/kitbag/issues?q=is%3Aissue+is%3Aopen+label%3Asecurity)).
>   Review a captured profile before you share or commit it.
> - **Only apply profiles from git sources you trust.** A remote profile can define MCP
>   servers that run commands on your machine. kitbag doesn't add or change them unless you
>   name them with `--allow-git-mcp`, and drops the profile's permission rules, hooks and
>   shell snippets unless you pass `--trust-source`. `kitbag diff` shows what would change
>   before you apply.
>
> Details and how to report a problem: [SECURITY.md](SECURITY.md).

```console
$ kitbag capture --from claude-code -o ~/dotagents     # read your setup, redact secrets
$ kitbag apply ~/dotagents --to cursor,codex           # show the plan, ask, write, back up
$ kitbag undo                                          # changed your mind
```

## Why

Every new agent harness starts empty. You re-add the same MCP servers, copy `CLAUDE.md` into
`AGENTS.md`, rewrite rules in yet another frontmatter dialect, and paste API keys into one more
JSON file. kitbag makes that setup portable: the setup lives in one versioned profile (in your
dotfiles or a team repo), and each tool gets it in its own native format.

- **One format, many tools.** Each adapter knows where its tool keeps its config and how it
  spells things, e.g. `${env:VAR}` vs `${VAR}`, `.mdc` vs `paths:` frontmatter, TOML vs JSON.
- **Secrets are kept out of the profile (best-effort).** Values are redacted to `${ENV_VAR}`
  references on capture; review the profile before sharing it. On apply, they are written in each tool's own interpolation syntax wherever it has one.
- **Safe by default.** `apply` shows a diff and asks first. It merges into existing configs
  without touching anything else (comments and formatting included), backs up every file,
  and `kitbag undo` reverts.
- **A single static binary** for macOS, Linux and Windows. No runtime needed.

## 30-second quickstart

```sh
# Install (pick one)
cargo install kitbag
curl -fsSL https://raw.githubusercontent.com/cargopete/kitbag/main/install.sh | sh

# See which tools kitbag found and what each supports
kitbag list-targets

# Capture what you have now (tools detected automatically, or pass --from)
kitbag capture -o ~/dotagents
git -C ~/dotagents init && git -C ~/dotagents add -A && git -C ~/dotagents commit -m "My agent setup"

# Preview, then apply to another tool
kitbag diff  ~/dotagents --to codex
kitbag apply ~/dotagents --to codex

# On a new machine: apply straight from git
kitbag apply github:you/dotagents --to all
```

Starting from scratch? `kitbag init my-profile` writes a commented starter profile, and
[`examples/profile`](examples/profile) shows a realistic one.

## Targets

| Target id        | Tool              | Scopes        | MCP | Instructions | Rules      | Commands     | Skills | Settings |
|------------------|-------------------|---------------|-----|--------------|------------|--------------|--------|----------|
| `claude-code`    | Claude Code       | user, project | ✓   | ✓            | ✓          | ✓            | ✓      | ✓        |
| `cursor`         | Cursor            | user, project | ✓   | project      | project    | ✓            | ✓      | -        |
| `codex`          | OpenAI Codex CLI  | user, project | ✓   | ✓            | folded     | user         | ✓      | ✓        |
| `gemini-cli`     | Gemini CLI        | user, project | ✓   | ✓            | folded     | ✓            | ✓      | ✓        |
| `claude-desktop` | Claude Desktop    | user          | ✓ (local servers) | -  | -          | -            | -      | -        |

*folded*: the tool has no file-scoped rules, so rules become labelled sections of its single
instructions file. Each section records the rule's globs, and `capture` splits them back out
losslessly.

Where things go:

| Target | User scope | Project scope |
|---|---|---|
| Claude Code | `~/.claude.json` (`mcpServers`), `~/.claude/{CLAUDE.md, rules/, commands/, skills/, settings.json}`; honours `CLAUDE_CONFIG_DIR` | `.mcp.json`, `CLAUDE.md` (or `.claude/CLAUDE.md`), `.claude/{rules,commands,skills}/`, `.claude/settings.json`; capture also reads local-scope servers from `~/.claude.json` |
| Cursor | `~/.cursor/mcp.json`, `~/.cursor/commands/`, `~/.cursor/skills/` | `.cursor/mcp.json`, `.cursor/rules/*.mdc`, `AGENTS.md`, `.cursor/commands/`, `.cursor/skills/`; capture also reads legacy `.cursorrules` |
| Codex CLI | `$CODEX_HOME/config.toml` (default `~/.codex`), `$CODEX_HOME/AGENTS.md`, `$CODEX_HOME/prompts/`, `~/.agents/skills/` | `.codex/config.toml` (trusted projects only), `AGENTS.md`, `.agents/skills/` |
| Gemini CLI | `~/.gemini/settings.json`, `~/.gemini/GEMINI.md`, `~/.gemini/commands/*.toml`, `~/.gemini/skills/` | `.gemini/settings.json`, `GEMINI.md`, `.gemini/commands/`, `.gemini/skills/` |
| Claude Desktop | `claude_desktop_config.json` in `~/Library/Application Support/Claude` (macOS), `%APPDATA%\Claude` (Windows), `~/.config/Claude` (Linux) | n/a |

How each tool receives `${VAR}` references:

| Tool | Written as | Secret on disk? |
|---|---|---|
| Claude Code | `${VAR}` / `${VAR:-default}` (expanded by Claude Code) | no |
| Cursor | `${env:VAR}` (expanded by Cursor) | no |
| Gemini CLI | `${VAR}` (expanded by Gemini CLI; defaults resolved at apply time) | no |
| Codex CLI | `env_vars = ["VAR"]`, `bearer_token_env_var`, `env_http_headers` (read from the environment by Codex) | no, for same-name env vars, bearer tokens and whole-value headers. Anything else is resolved (see below) |
| Claude Desktop | resolved value (no interpolation support) | yes, unless `--secrets never` |

## The profile format

A profile is a directory you can read, diff and review in a pull request:

```text
my-profile/
├── kitbag.toml            # manifest: MCP servers + per-tool settings
├── instructions.md        # always-on instructions -> CLAUDE.md / AGENTS.md / GEMINI.md
├── rules/
│   ├── rust.md            # scoped or on-demand rules (frontmatter below)
│   └── security-review.md
├── commands/
│   └── review.md          # reusable prompts / slash commands
└── skills/
    └── release-notes/     # Agent Skills: SKILL.md + any supporting files
        └── SKILL.md
```

Every file is optional except `kitbag.toml`.

### `kitbag.toml`

```toml
kitbag = 1                    # format version (required)
name = "my-setup"             # required; also the id of managed blocks
description = "Optional"

[mcp_servers.github]          # stdio server
command = "npx"
args = ["-y", "@modelcontextprotocol/server-github"]
env = { GITHUB_PERSONAL_ACCESS_TOKEN = "${GITHUB_PERSONAL_ACCESS_TOKEN}" }
# cwd = "/path"               # working directory (Codex, Gemini CLI)

[mcp_servers.linear]          # remote server; transport inferred from `url`
url = "https://mcp.linear.app/mcp"
headers = { Authorization = "Bearer ${LINEAR_API_KEY}" }
# transport = "sse"           # stdio | http | sse (only needed for legacy SSE)
# only = ["claude-code"]      # restrict to these targets
# except = ["claude-desktop"] # or exclude some
# disabled = true             # keep the definition; Codex writes enabled = false, others skip it

[settings.claude-code]        # per-tool settings, allow-listed keys only
model = "opus"
permissions = { allow = ["Bash(cargo test:*)"], deny = ["Read(./.env)"] }

[settings.codex]
model_reasoning_effort = "high"
```

Unknown keys are rejected with a precise error rather than silently ignored. The format is
versioned (`kitbag = 1`). Future incompatible changes will bump the number, and older kitbag
releases will refuse newer profiles instead of misreading them.

**Env references.** `${NAME}` takes its value from the environment. `${NAME:-fallback}`
falls back to a non-secret default (it is written to disk wherever the variable is unset, so
`doctor` and `apply` flag a fallback that looks like a secret). References work in `command`, `args`, `env`, `cwd`, `url`
and `headers`.

**Settings allow-list.** A profile may come from someone else's repository, so only settings
with a bounded effect are applied. Anything else is dropped with a warning:

| Target | Allowed keys |
|---|---|
| `claude-code` | `model`, `effortLevel`, `permissions`, `outputStyle`, `alwaysThinkingEnabled`, `includeCoAuthoredBy`, `cleanupPeriodDays` |
| `codex` | `model`, `model_reasoning_effort`, `model_reasoning_summary`, `model_verbosity`, `approval_policy`, `sandbox_mode`, `web_search`, `file_opener` |
| `gemini-cli` | `model`, `ui` |

Hooks, `env` blocks, `apiKeyHelper`, provider endpoints and similar keys are never carried.
Anything that lets agents act without asking gets an explicit warning in the plan. That
covers `permissions.defaultMode = bypassPermissions` (and `acceptEdits`, `dontAsk`, `auto`),
`permissions.additionalDirectories`, allow rules (broad ones such as `Bash(*)` or
`Bash(rm -rf *)` are named), `approval_policy = never` and `sandbox_mode = danger-full-access`.
It also covers commands and skills whose frontmatter pre-approves tools (`allowed-tools`) or
defines `hooks`, and commands and skills that run shell snippets (``!`cmd` `` in Claude Code,
`!{cmd}` in Gemini CLI commands).

**From a git source, the riskiest settings are dropped** unless you pass
`--allow-risky-settings` (or its alias `--trust-source`): permissive `defaultMode`s,
`additionalDirectories`, every `permissions.allow` rule, `approval_policy = never` and
`sandbox_mode = danger-full-access`. `ask` and `deny` rules still apply, since they restrict.
The plan lists everything it left out. Profiles in a local directory apply them as written,
with the warning above.

**From a git source, MCP servers are opt-in.** A stdio server is a command that runs on your
machine whenever the tool starts, so servers that a git profile would add, or change, are not
applied by default. The plan lists each one under "MCP servers not applied (from a git
source)" with its command and arguments (or URL) and the names of its env vars and headers.
Pass `--allow-git-mcp docs,search` (repeatable) to apply the ones you name, `--allow-git-mcp`
with no value for all of them (a value that looks like a path or URL, or names a directory
with a `kitbag.toml`, is taken as the source, so `--allow-git-mcp github:me/profile` works;
write `--allow-git-mcp=NAME` to be explicit), or `--trust-source`, which implies it. Servers already
in place with the same definition are left as they are and aren't listed; servers from a local
directory apply as written. From a git source, commands and skills that run shell snippets are
skipped, `hooks` and `allowed-tools` are removed from the frontmatter of the rest, and skill
scripts are not made executable, unless you pass the same flag. The warning for local profiles
names broad rules, judged case-insensitively and ignoring extra whitespace: bare tools (`Bash`, `Write`), any command (`Bash(*:*)`), the whole file system
(`Read(//**)`, `Edit(~/**)`), shells, interpreters, package runners and containers with open
arguments (`Bash(npx:*)`, `Bash(docker run:*)`), and piping into a shell (`… | sh`).

### Rules: `rules/<name>.md`

```markdown
---
description: Rust conventions    # shown to agents that pick rules by description
globs:                           # file-scoped; a list or "a,b" string
  - "**/*.rs"
alwaysApply: false               # optional, see below
---

Rule text in Markdown.
```

| Frontmatter | Meaning | Cursor | Claude Code | Codex / Gemini |
|---|---|---|---|---|
| no globs | always on | `alwaysApply: true` | rule without `paths` | folded section |
| `globs` | when matching files are in play | `globs:` | `paths:` | folded section, labelled with the globs |
| no globs, `alwaysApply: false` | on demand, by description | agent-requested rule | always loaded (noted) | folded section, "use when relevant" |

### Commands: `commands/<name>.md`

```markdown
---
description: Review the staged changes
argument-hint: "[focus area]"
---

Review the staged diff. Focus on: $ARGUMENTS
```

`$ARGUMENTS` is the text after the command. Gemini CLI gets `{{args}}` in a `.toml` command,
Codex gets a custom prompt, and Cursor gets a plain Markdown command. Other frontmatter keys,
such as Claude Code's `allowed-tools`, are preserved and passed only to Claude Code.

### Skills: `skills/<name>/`

Standard [Agent Skills](https://agentskills.io): a directory with `SKILL.md` (frontmatter
`name` and `description`) and any scripts or references. Skills are copied as-is, and
`name`/`description` are added for tools that require them.

## Secrets: the security model

1. **Capture redacts.** Values that look like credentials become `${ENV_VAR}` references, and
   capture prints each replacement (`mcp_servers.github.env.GITHUB_TOKEN -> ${GITHUB_TOKEN}`).
   A value counts as a credential if any of these hold:
   - it matches a known token format: GitHub, Anthropic, OpenAI, OpenRouter, xAI, Groq,
     Perplexity, Tavily, Firecrawl, Replicate, Supabase, Stripe, Slack, AWS, Google, GitLab,
     npm, PyPI, Hugging Face, Linear, Notion, Sentry, SendGrid, Shopify, DigitalOcean, JWTs,
     private keys, or passwords in URLs;
   - it sits under a secret-sounding name (`*_TOKEN`, `*_KEY`, `*PASSWORD`, `*_PASSPHRASE`,
     `*_SECRET_ID`, `*AUTH`, `Authorization`, or a header mentioning auth, key, token, secret,
     session or cookie), however short. Empty values, booleans, small numbers, file paths and
     `${VAR}` references don't count. Only the `${…}` spans of a value are exempt: in
     `Bearer tok ${X}` or `pw-${SUFFIX}`, the literal part is still checked;
   - it is a high-entropy opaque string.

   Redaction covers env values, headers, URL credentials (`user:pass@` and `:pass@`), query
   parameters and the literal fallback of `${VAR:-fallback}` (it becomes `${VAR}`). URLs are
   checked wherever they appear (`url`, args, `--flag=URL`, env values): tokens in the path
   (Slack and Discord webhooks, Zapier-style `/mcp/s/<token>/`, random-looking segments, and
   hex-only segments of 32 digits or more, except a full commit SHA of 40 or 64 digits after
   `commit`, `blob`, `tree` and the like) and a
   token used as the username (`https://<token>@host`) are replaced; `git@` or `bob@` stay. It also covers
   secrets passed through `args`, as in `--api-key=…`, `--token …`, `NAME=value` (e.g.
   `docker -e`), `Authorization: Bearer …` (e.g. `mcp-remote --header`) and URLs.
   Known token formats are also redacted from instructions, rules, commands and skill files,
   frontmatter included, and so are values given to secret-sounding names there
   (`DB_PASSWORD=…`, `X-Api-Key: …`, `--token …`, `Bearer …`). Claude Code permission rules
   in `settings.json` get the same treatment; a rule with a redacted part is not applied back.
   In free text, secret-named query values and webhook path tokens are replaced too.
   There, a token becomes `[REDACTED:<kind>]`, and in a URL only the password is replaced
   (`postgres://admin:[REDACTED]@db/app`), so the Markdown around it survives. Placeholder
   passwords in documentation examples (`user:pass@`, `:password@`, `:<password>@`,
   `:${DB_PASSWORD}@`, `:xxx@`) are left alone.
   Credential files inside skills are left out of the profile with a warning. This covers
   key files, `credentials.json`, `.env`, JSON/TOML/YAML files with secret-named values, and
   binaries containing tokens. Symbolic links inside a skill are followed only when they
   stay inside that skill, so a link such as `etc -> /etc`, `aws -> ~/.aws` or `up -> ..`
   is skipped with a warning. A skill directory that is itself a link (say into a git
   checkout) works as usual.
   Two servers with different secrets under the same key get separate variables
   (`API_KEY`, `MONGO_API_KEY`), and identical secrets share one. Two different secrets
   that would get the same name, in one server or across servers, get a numbered one
   (`API_KEY_2`) instead of sharing it.
2. **The profile is checked before it is written.** If anything still looks like a credential
   (the same checks as capture), kitbag refuses to write the profile at all. `kitbag doctor`
   runs the same check on any profile.
3. **Apply prefers references.** Tools that expand env vars get references in their own syntax,
   so the secret never touches disk. Codex gets `env_vars`, `bearer_token_env_var` and
   `env_http_headers`, which read from the environment at runtime.
4. **Resolution is a last resort, and visible.** Some values have nowhere to go but the file,
   such as anything for Claude Desktop, or a Codex env var whose name differs from its source.
   kitbag resolves those from your environment at apply time and warns per field. The value is
   masked as `••••••••` in every diff. Diffs also mask anything that already looks like a
   credential in your existing files, including private keys that span several lines, and
   anything capture would redact. New files containing resolved values are created `0600`.
   `--secrets never` keeps the `${VAR}` placeholder instead. It is the default for profiles
   from git sources, so your environment is never resolved into servers someone else defined
   unless you pass `--secrets resolve` (or `--trust-source`). Unset variables are reported and
   left as placeholders.
5. **Profiles from git are untrusted input.** kitbag rejects symlinks inside a profile, both
   files and directories, including a `--path` sub-directory. A symlink could otherwise copy
   files such as `~/.ssh/id_rsa` into a tool's config. Profile text can't break out of its
   managed block, because kitbag markers inside content are neutralised. It rejects path
   tricks in names and sub-directories, applies only allow-listed settings, and lists every
   MCP server it adds or changes before you confirm: the command and arguments it runs, or
   the URL it connects to, and the names of the env vars and headers it sets. From a git
   source, those servers are only applied when you opt in with `--allow-git-mcp`.

## Safe apply

- **Plan first.** `kitbag diff` and `kitbag apply --dry-run` show a unified diff. `kitbag apply`
  shows a summary and asks before writing. Non-interactive sessions need `--yes`.
  `kitbag diff --exit-code` fails when a machine has drifted from the profile, which is useful in CI.
- **Merge, don't clobber.** JSON and TOML edits go through format-preserving editors: comments,
  key order and indentation of everything else stay as they were. A server kitbag updates keeps
  keys it doesn't manage, such as `startup_timeout_sec`, `timeout` or `trust`, as long as it
  still runs the same command or connects to the same URL. A same-named server that now points
  somewhere else is replaced as a whole, so `trust`, `headersHelper` or OAuth settings granted
  to the old one don't carry over; the plan names the keys it dropped.
  `--no-overwrite` keeps existing same-named entries.
- **Managed blocks.** In files you also edit (`CLAUDE.md`, `AGENTS.md`, `GEMINI.md`) kitbag owns
  only the text between `<!-- kitbag:begin <name> -->` and `<!-- kitbag:end <name> -->`.
  Re-applying replaces the block in place. Paragraphs already in your own text are not
  duplicated into the block.
- **Idempotent.** Applying the same profile twice changes nothing.
- **Redacted text never replaces your originals.** A captured profile holds `[REDACTED…]`
  where capture removed a secret from free text. When applying it back to a file that is
  the original of that redacted copy, kitbag keeps your file and says so. If the file
  differs in other ways too, it is replaced, with a warning that the new content contains
  redaction markers.
- **Symlinks are respected, and confined.** At user scope, a config file that is a symlink
  (say into a dotfiles repo) is written through: the plan names the real file, and the link
  stays a link, including after undo. At project scope, where the files may come from someone
  else's repository, kitbag refuses to write through links that leave the project or dangle.
- **Backups and undo.** Every apply is a transaction. The originals are copied to the state
  directory first, writes are atomic, and a failure part-way through rolls back.
  `kitbag history` lists applies, and `kitbag undo [id]` restores them byte for byte. Undo
  refuses if you edited a file since, or if a backup no longer matches the SHA-256 recorded
  when it was taken, unless you pass `--force`. (The hash sits next to the backups, so it
  catches a backup that changed on its own, not someone who can write to the state directory.) A file whose directory now
  resolves somewhere else (a symbolic link appeared or changed since the apply), or that is
  now a link itself, is left alone even with `--force`: undo reverts the rest, names it and
  exits with status 1. If a file changes between planning and writing, the apply is refused.
- **Profile directories.** `init --force` and `capture --force` replace a symbolic link inside
  the profile directory (a file or a sub-directory) with a regular file or directory; they
  never write through it.
- **State directory:** `$KITBAG_STATE_DIR`, else `$XDG_STATE_HOME/kitbag`, else
  `~/.local/state/kitbag` (`%LOCALAPPDATA%\kitbag` on Windows).

Apply is additive: removing a server from the profile does not remove it from tools you
already applied to. Use `kitbag undo`, or remove it in the tool.

## Keeping instructions in sync

Each tool reads its own instructions file (`~/.claude/CLAUDE.md`, `~/.codex/AGENTS.md`,
`~/.gemini/GEMINI.md`, project `AGENTS.md`, rule files), and sooner or later one copy gets
edited by hand. Every apply records a hash of each managed block and rule file it leaves in
place (in the state directory, next to the backups), so kitbag can tell who changed what:

```console
$ kitbag status ~/dotagents
  claude-code    instructions           tool edited      ~/.claude/CLAUDE.md
  claude-code    rule rust              in sync          ~/.claude/rules/rust.md
  codex          instructions           profile changed  ~/.codex/AGENTS.md
  cursor         instructions           unsupported
                 Cursor keeps user rules in its settings (app storage), not in files; ...
```

| State | Meaning |
|---|---|
| `in sync` | the tool has what the profile would write |
| `tool edited` | edited in the tool since the last apply; the profile hasn't changed |
| `profile changed` | the profile changed; the tool still has what was last applied |
| `both changed` | both changed (or they differ and there is no record of an apply) |
| `not applied` | in the profile, not in the tool yet |
| `unsupported` | the tool keeps it where kitbag can't read or write (Cursor user rules) |

Without a profile, `kitbag status` shows what changed in each tool since the last apply.
`--exit-code` exits with status 1 on any drift, `--json` prints rows for scripts, and status
never writes.

`kitbag sync <profile>` is `apply` that won't clobber hand edits: it applies `profile changed`
and `not applied` items (with the usual plan, diff, confirmation, backups and undo), but leaves
a target whose instructions were edited in the tool alone and says so (exit status 1). Then
either:

- `kitbag sync <profile> --from claude-code` pulls that tool's edited block or rule back into
  the profile (`instructions.md`, `rules/*.md`), redacted like `capture` and refused if it
  still looks like it holds a secret, and applies it to the other targets. The profile files
  are part of the same transaction, so `kitbag undo` reverts them too. Only the managed
  block is pulled back, not your own text around it.
- `kitbag sync <profile> --overwrite` replaces the edit with the profile's version (backed up).

From a git source, `sync` works like `apply` with all the remote restrictions, and `--from`
is refused, since kitbag can't write back into a repository it cloned.

## Second machine / dotfiles

The recommended way to keep several machines in step:

1. On the machine that's set up, capture into a directory of your dotfiles (or a private)
   repository and commit it:

   ```sh
   kitbag capture -o ~/dotfiles/kitbag
   git -C ~/dotfiles add kitbag && git -C ~/dotfiles commit -m "agent setup"
   ```

2. On the other machine, clone the repository and sync from the clone:

   ```sh
   git clone git@github.com:you/dotfiles ~/dotfiles
   kitbag sync ~/dotfiles/kitbag --to all
   ```

   A local clone is a local profile, so it is trusted like any directory you own: its MCP
   servers, permission rules and hooks apply as written. Applying `github:you/dotfiles --path
   kitbag` directly is treated as a remote profile instead: MCP servers need `--allow-git-mcp`
   (or `--trust-source`, which also lifts the other remote restrictions).

3. Later, `kitbag status ~/dotfiles/kitbag` shows what drifted on this machine: managed
   instructions and rules (see [Keeping instructions in sync](#keeping-instructions-in-sync)),
   and MCP servers, commands and skills that were `added` to the profile, `changed`, or exist
   only in the tool (`removed`), by name only. If you edited `CLAUDE.md` on this machine,
   `kitbag sync ~/dotfiles/kitbag --from claude-code` pulls the edit into the profile; commit
   and push it from there.

What keeps a profile portable:

- **Home-relative paths.** Capture writes paths under your home directory as `~/...` in MCP
  server commands, arguments (also `--flag=/home/you/x`), env values and `cwd`, and in Claude
  Code's `permissions` (`Read(//home/you/notes/**)` becomes `Read(~/notes/**)`, which Claude
  Code understands). Apply expands `~` to the home directory of the machine it runs on, with
  that OS's separators, so a profile captured on macOS applies on Linux or Windows.
- **No machine-specific state.** Only the keys in the settings allow-list are captured, so
  Codex's `[projects."/abs/path"]` trust entries and Claude Code's per-project state in
  `~/.claude.json` stay on the machine.
- **Secrets travel separately.** The profile holds `${VAR}` references; set the variables on
  each machine (`kitbag doctor ~/dotfiles/kitbag` lists the ones that are unset).

## Commands

| Command | What it does |
|---|---|
| `kitbag init [DIR]` | Write a commented starter profile |
| `kitbag capture [--from IDS] [-o DIR] [--dry-run] [--force]` | Read the current setup into a profile (secrets redacted). Writes only into a new or empty directory. `--force` replaces an existing profile's files and leaves anything else in the directory alone. With several tools, one whose config can't be read is reported and the rest are captured |
| `kitbag diff <SOURCE> [--to IDS] [--exit-code]` | Show exactly what `apply` would change; never writes |
| `kitbag apply <SOURCE> [--to IDS] [--yes] [--dry-run]` | Plan, confirm, write with backups |
| `kitbag status [SOURCE] [--to IDS] [--exit-code] [--json]` | Which managed instructions and rules differ from the profile, or changed since the last apply; never writes |
| `kitbag sync <SOURCE> [--to IDS] [--from ID \| --overwrite]` | Apply without overwriting instructions edited in a tool; pull an edit back with `--from` |
| `kitbag undo [ID] [--force]` | Revert the latest (or a specific) apply |
| `kitbag history [--json]` | List applies |
| `kitbag doctor [SOURCE]` | Check installed tools' configs, a profile, required env vars, risky settings |
| `kitbag list-targets [--json]` | Targets, what they support, which are detected, file locations |

Common options:

- `--to` / `--from` take target ids (comma-separated), `all`, or `detected` (the default).
- `--scope user|project` and `--project DIR` choose between global and per-project config.
- `--secrets resolve|never`, `--no-overwrite`, `--allow-git-mcp [NAMES]` and
  `--allow-risky-settings` (`--trust-source`) apply to `apply` and `diff`.
- `--path SUBDIR` selects a profile inside a repository.
- `--home DIR` (or `KITBAG_HOME`) treats `DIR` as the home directory for sandboxes and tests.
  It also ignores `CODEX_HOME`, `CLAUDE_CONFIG_DIR`, `XDG_CONFIG_HOME` and `APPDATA`.

**Sources** are a local directory or a git URL:

- `github:owner/repo`, `gh:owner/repo/sub/dir#ref`, `gitlab:` and `bitbucket:` shorthands;
- `https://github.com/owner/repo/tree/<ref>/<dir>`;
- any `https://…/repo.git`, `git@host:owner/repo.git`, `ssh://` or `file://` URL, with
  `//sub/dir` to pick a sub-directory and `#ref` for a branch or tag.

kitbag clones with your own `git` (shallow, no prompts) into a temporary directory that is
removed afterwards. Cleartext `http://` and `git://` sources are refused, because anyone on the
network could swap the profile, and so is an `https://` server that redirects to one; pass
`--allow-insecure-source` to accept them. Credentials in a URL (`https://user:token@host/…`)
are passed to `git` only: plans, errors, `history` and the transaction log show the URL
without them.

## Install

- **Cargo:** `cargo install kitbag` (from [crates.io](https://crates.io/crates/kitbag)). For unreleased changes,
  `cargo install --git https://github.com/cargopete/kitbag kitbag`, or `cargo install --path .` from a checkout.
- **Prebuilt binaries:** static builds for Linux (x86_64/aarch64, musl), macOS (Intel and
  Apple Silicon) and Windows (x86_64/arm64) are attached to each
  [GitHub Release](https://github.com/cargopete/kitbag/releases), with SHA-256 checksums.
- **Script:** `curl -fsSL https://raw.githubusercontent.com/cargopete/kitbag/main/install.sh | sh`.
  It verifies the checksum and installs to `~/.local/bin`. Set `KITBAG_VERSION` or
  `KITBAG_INSTALL_DIR` to change either. On Windows, use
  `irm https://raw.githubusercontent.com/cargopete/kitbag/main/install.ps1 | iex`.

Building needs Rust 1.88 or newer.

## Adding an adapter

An adapter is one file in `src/adapters/` implementing the `Adapter` trait:

```rust
pub trait Adapter: Sync {
    fn id(&self) -> &'static str;              // "windsurf"
    fn name(&self) -> &'static str;            // "Windsurf"
    fn docs(&self) -> &'static str;            // link to the tool's config docs
    fn scopes(&self) -> &'static [Scope];      // User, Project
    fn support(&self, scope: Scope) -> [(Feature, Support); 6];
    fn locations(&self, env: &Env) -> Vec<(&'static str, PathBuf)>;
    fn detect(&self, env: &Env) -> bool;
    fn capture(&self, env: &Env) -> Result<Captured>;
    fn apply(&self, profile: &Profile, env: &Env, opts: &ApplyOptions, work: &mut Work) -> Result<()>;
}
```

1. **Research the real format** from the tool's docs: file locations per OS and scope, MCP keys,
   which env-reference syntax it expands (if any), and its rules and commands formats. Put the
   findings in the module doc comment, as the existing adapters do.
2. **Capture** by reading files with `plan_fs::read_text_opt`. Build `McpServer`s and hand them
   to `common::import_server`, which normalises references and redacts secrets. Use
   `capture_instructions`, `capture_markdown_dir` and `capture_skills_dir` for the rest.
3. **Apply** must never touch disk. Read and write through `work.fs` (a `PlanFs`). Use the
   helpers: `merge_json_servers` (format-preserving, keeps unmanaged keys),
   `write_instructions` (managed blocks, optional rule folding), `write_owned`, `write_skill`,
   `render_server` with the tool's `RefStyle`, or `Resolver` when the tool can't expand
   references. Report anything lossy with `work.info` or `work.warn`.
4. **Register** it in `adapters::all()`.
5. **Test** it: add realistic configs under `tests/fixtures/{home,project}`, add capture and apply
   cases in `tests/adapters.rs`, and include it in the round-trip and idempotency tests.

Good next candidates: Windsurf, Zed, VS Code / GitHub Copilot, OpenCode.

## Format notes and known limitations

- **Cursor user rules** live in Cursor's settings UI, not in files, so kitbag can't write or read
  them at user scope. It says so in the plan. At project scope, rules go to `.cursor/rules/*.mdc`
  and instructions to `AGENTS.md`.
- **User-level Cursor commands** (`~/.cursor/commands`) are written as documented by the
  community. Some Cursor versions have been reported not to load them.
- **Codex custom prompts** are written for commands at user scope only. Codex now favours skills,
  and kitbag notes this. Codex loads `.codex/config.toml` only in trusted projects and reads the
  first 32 KiB of `AGENTS.md` by default. kitbag warns about both.
- **Claude Desktop** has no official Linux build. The `~/.config/Claude` path matches community
  builds.
- At project scope, Cursor and Codex both read `AGENTS.md`. When both are targeted, Cursor also
  sees Codex's folded rules section.
- Apply is additive and doesn't delete entries that were removed from the profile (see above).
- **Not carried yet:** Claude Code subagents (`agents/`), output styles (`output-styles/`),
  plugins and hooks. Capture lists what it found of these and leaves them out. It also drops
  `outputStyle` when it names a custom style (built-ins such as `Explanatory` are kept),
  since the style file wouldn't be in the profile.
- **Encodings.** Capture reads UTF-8 (with or without a byte-order mark) and UTF-16 with a
  byte-order mark. Instructions, rules and commands in any other encoding are skipped with a
  warning. Skill supporting files are copied byte for byte. A UTF-8 byte-order mark at the
  start of a JSON or TOML config is kept when kitbag writes the file back.
- **Size limits.** Profile files are limited to 2 MiB and skills to 200 files. Capture skips
  anything larger with a warning, so it never writes a profile that can't be applied.

## Development

```sh
cargo test                                  # unit + integration tests (fixtures, temp dirs only)
cargo clippy --all-targets -- -D warnings
cargo fmt --check
```

The tests never touch your real config. They copy `tests/fixtures` into temporary directories
and run the CLI with `KITBAG_HOME` and `KITBAG_STATE_DIR` pointing there.

## License

[MIT](LICENSE) © Petko Pavlovski
