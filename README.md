# kitbag

**Pack your AI agent setup once, unpack it into any coding tool.**

[![CI](https://github.com/cargopete/kitbag/actions/workflows/ci.yml/badge.svg)](https://github.com/cargopete/kitbag/actions/workflows/ci.yml)
[![License: MIT](https://img.shields.io/badge/license-MIT-blue.svg)](LICENSE)

kitbag captures your MCP servers, instructions, rules, slash commands, skills and settings
from the AI coding tools you already use. It stores them as a small, git-friendly **profile** with secrets
redacted and applies that profile to any other tool in one step. Claude Code, Cursor,
OpenAI Codex CLI, Gemini CLI, Claude Desktop, Google Antigravity, opencode (the last two
for MCP servers and instructions), GitHub Copilot (MCP servers, instructions, rules, prompt files and skills)
xAI's Grok Build CLI, Windsurf, Zed and Hermes Agent (the last three for MCP servers and instructions) are supported, at user and
project scope.

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
>   shell snippets unless you pass `--trust-source`. Hooks, which run commands by
>   themselves, are never written without `--apply-hooks` either, from any source.
>   `kitbag diff` shows what would change before you apply.
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
- **Safe by default.** `apply` lists what it will change and asks first; `kitbag diff` (or
  `apply --dry-run`) shows the full diff. It merges into existing configs without touching
  anything else (comments and formatting included), backs up every file,
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

# Someone else's bag: see what's in it, and what it would run and write, first
kitbag inspect github:someone/agents@v1.0 --to all
```

Starting from scratch? `kitbag init my-profile` writes a commented starter profile, and
[`examples/profile`](examples/profile) shows a realistic one.

## Targets

| Target id        | Tool              | Scopes        | MCP | Instructions | Rules      | Commands     | Skills | Agents | Settings |
|------------------|-------------------|---------------|-----|--------------|------------|--------------|--------|--------|----------|
| `claude-code`    | Claude Code       | user, project | ✓   | ✓            | ✓          | ✓            | ✓      | ✓      | ✓        |
| `cursor`         | Cursor            | user, project | ✓   | project      | project    | ✓            | ✓      | -      | -        |
| `codex`          | OpenAI Codex CLI  | user, project | ✓   | ✓            | folded     | user         | ✓      | -      | ✓        |
| `gemini-cli`     | Gemini CLI        | user, project | ✓   | ✓            | folded     | ✓            | ✓      | -      | ✓        |
| `claude-desktop` | Claude Desktop    | user          | ✓ (local servers) | -  | -          | -            | -      | -      | -        |
| `antigravity`    | Google Antigravity | user, project | ✓  | ✓            | ✓          | -            | ✓      | -      | -        |
| `opencode`       | opencode          | user, project | ✓   | ✓            | folded     | ✓            | ✓      | ✓      | -        |
| `copilot`        | GitHub Copilot (CLI, VS Code) | user, project | ✓ | ✓    | ✓          | project      | ✓      | ✓      | -        |
| `grok-build`     | Grok Build        | user, project | ✓   | ✓            | ✓*         | ✓            | ✓      | -      | -        |
| `windsurf`       | Windsurf          | user, project | user | ✓           | -          | -            | -      | -      | -        |
| `zed`            | Zed               | user, project | ✓   | ✓            | -          | -            | -      | -      | -        |
| `hermes`         | Hermes Agent      | user, project | user | ✓           | -          | -            | -      | -      | -        |

`claude` is accepted as an alias for `claude-code` wherever a target id is given (`--to`,
`--from`, `targets`, `disabled_on`, and `[settings.claude]`, read as `[settings.claude-code]`;
a profile with both tables is an error); kitbag always writes and prints `claude-code`. It
never means `claude-desktop`. Likewise `grok` is an alias for `grok-build`.

*folded*: the tool has no file-scoped rules, so rules become labelled sections of its single
instructions file. Each section records the rule's globs, and `capture` splits them back out
losslessly.

\*Grok Build has a rules directory, but no file-scoped or on-demand rules: every
rule there loads for every file. kitbag writes each rule to its own file with a first line saying
which files it is for (`_Applies when working with files matching: ..._`), and capture reads the
globs back from the frontmatter, which Grok strips before the model sees the rule.

Where things go:

| Target | User scope | Project scope |
|---|---|---|
| Claude Code | `~/.claude.json` (`mcpServers`), `~/.claude/{CLAUDE.md, rules/, commands/, skills/, agents/, settings.json}`; honours `CLAUDE_CONFIG_DIR` | `.mcp.json`, `CLAUDE.md` (or `.claude/CLAUDE.md`), `.claude/{rules,commands,skills,agents}/`, `.claude/settings.json`; capture also reads local-scope servers from `~/.claude.json` |
| Cursor | `~/.cursor/mcp.json`, `~/.cursor/commands/`, `~/.cursor/skills/` | `.cursor/mcp.json`, `.cursor/rules/*.mdc`, `AGENTS.md`, `.cursor/commands/`, `.cursor/skills/`; capture also reads legacy `.cursorrules` |
| Codex CLI | `$CODEX_HOME/config.toml` (default `~/.codex`), `$CODEX_HOME/AGENTS.md`, `$CODEX_HOME/prompts/`, `~/.agents/skills/` | `.codex/config.toml` (trusted projects only), `AGENTS.md`, `.agents/skills/` |
| Gemini CLI | `~/.gemini/settings.json`, `~/.gemini/GEMINI.md`, `~/.gemini/commands/*.toml`, `~/.gemini/skills/` | `.gemini/settings.json`, `GEMINI.md`, `.gemini/commands/`, `.gemini/skills/` |
| Claude Desktop | `claude_desktop_config.json` in `~/Library/Application Support/Claude` (macOS), `%APPDATA%\Claude` (Windows), `~/.config/Claude` (Linux) | n/a |
| Antigravity | `~/.gemini/config/mcp_config.json`, `~/.gemini/GEMINI.md` (the same file and managed block as Gemini CLI); capture also reads `~/.gemini/AGENTS.md` and `~/.gemini/config/{AGENTS,GEMINI}.md`; `~/.gemini/config/rules/*.md`, `~/.gemini/config/skills/` | `.agents/mcp_config.json`, `AGENTS.md` (shared with Codex and Cursor); capture also reads `GEMINI.md` and `.agents/{AGENTS,GEMINI}.md`; `.agents/rules/*.md`, `.agents/skills/` (the same directory as Codex's project skills) |
| opencode | `~/.config/opencode/opencode.json` or `opencode.jsonc` (`mcp.servers`), `~/.config/opencode/{AGENTS.md, commands/*.md, skills/<name>/, agents/*.md}`; honours `XDG_CONFIG_HOME` | `opencode.json(c)` or `.opencode/opencode.json(c)` (the first that exists, else `opencode.json`), `AGENTS.md` (shared with Codex, Cursor and Antigravity), `.opencode/{commands,skills,agents}/`; capture reads every config file there is |
| Grok Build | `$GROK_HOME/config.toml` (`[mcp_servers.<name>]`, default `~/.grok`), `$GROK_HOME/{AGENTS.md, rules/*.md, commands/*.md, skills/}` | `.grok/config.toml`, `AGENTS.md` (shared with Codex, Cursor, Antigravity and opencode), `.grok/{rules,commands,skills}/`; Grok reads project files only in folders you have trusted |
| Windsurf | `~/.codeium/windsurf/mcp_config.json` (`mcpServers`), `~/.codeium/windsurf/memories/global_rules.md` | `AGENTS.md` (shared with Codex, Cursor, Antigravity, opencode and Grok Build); capture also reads the legacy `.windsurfrules`; Windsurf's docs describe no project MCP file, so MCP servers are not written at project scope |
| Zed | `settings.json` in `~/.config/zed` (macOS), `$XDG_CONFIG_HOME/zed` or `~/.config/zed` (Linux), `%APPDATA%\Zed` (Windows) (`context_servers`), and `AGENTS.md` next to it (Zed's personal instructions) | `.zed/settings.json` (`context_servers`; Zed starts these only in a trusted worktree); instructions go to the first of `.rules`, `.cursorrules`, `.windsurfrules`, `.clinerules`, `.github/copilot-instructions.md`, `AGENT.md` and `AGENTS.md` that exists, since Zed reads only that one, and to `AGENTS.md` (shared with Codex, Cursor, Antigravity, opencode, Grok Build and Windsurf) when none does |
| Hermes Agent | `$HERMES_HOME/config.yaml` (`mcp_servers`, default `~/.hermes`), `$HERMES_HOME/SOUL.md` (Hermes's identity file, the only global file it reads for instructions; capture reads only a kitbag-managed block in it) | `.hermes.md` or `HERMES.md` when the project has one (Hermes loads it before `AGENTS.md`), otherwise `AGENTS.md` (shared with Codex, Cursor, Antigravity, opencode, Windsurf and Grok Build); Hermes documents no project MCP file, so MCP servers are not written at project scope |
| GitHub Copilot | `$COPILOT_HOME/mcp-config.json` (`mcpServers`, default `~/.copilot`), `$COPILOT_HOME/copilot-instructions.md`, `$COPILOT_HOME/instructions/*.instructions.md`, `$COPILOT_HOME/skills/`, `$COPILOT_HOME/agents/*.agent.md` | `.mcp.json` (shared with Claude Code, written in Claude Code's shape), `.github/copilot-instructions.md`, `.github/instructions/*.instructions.md` (`applyTo`), `.github/prompts/*.prompt.md`, `.github/skills/`, `.github/agents/*.agent.md`; capture also reads `.github/mcp.json`, legacy chat modes in `.github/chatmodes/` and `.vscode/mcp.json` (`servers`), and `disabledMcpServers` from Copilot's `settings.json` |

How each tool receives `${VAR}` references:

| Tool | Written as | Secret on disk? |
|---|---|---|
| Claude Code | `${VAR}` / `${VAR:-default}` (expanded by Claude Code) | no |
| Cursor | `${env:VAR}` (expanded by Cursor) | no |
| Gemini CLI | `${VAR}` (expanded by Gemini CLI; defaults resolved at apply time) | no |
| Codex CLI | `env_vars = ["VAR"]`, `bearer_token_env_var`, `env_http_headers` (read from the environment by Codex) | no, for same-name env vars, bearer tokens and whole-value headers. Anything else is resolved (see below) |
| Claude Desktop | resolved value (no interpolation support) | yes, unless `--secrets never` |
| Antigravity | resolved value (its docs describe no interpolation); remote servers get `serverUrl`, SSE servers are skipped | yes, unless `--secrets never` |
| opencode | `{env:VAR}` (expanded by opencode; defaults resolved at apply time); SSE servers are skipped | no |
| GitHub Copilot | `${VAR}` / `${VAR:-default}` (expanded by Copilot); capture also reads `${env:VAR}` from `.vscode/mcp.json` | no |
| Windsurf | `${env:VAR}` (expanded by Windsurf, in `command`, `args`, `env`, `serverUrl`, `url` and `headers`); remote servers get `serverUrl`, SSE servers are skipped | no |
| Zed | resolved value (its docs describe no interpolation); SSE servers are skipped | yes, unless `--secrets never` |
| Hermes Agent | `${VAR}` (expanded by Hermes in `config.yaml`, from its environment and `~/.hermes/.env`; defaults resolved at apply time); SSE servers get `transport: sse` | no |
| Grok Build | `${VAR}` / `${VAR:-default}` (expanded by Grok Build); an `Authorization: Bearer ${VAR}` header is written as `bearer_token_env_var = "VAR"` and read back the same way; SSE servers get `type = "sse"`, and capture reads a server named in `disabled_mcp_servers` as disabled | no |

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
├── skills/
│   └── release-notes/     # Agent Skills: SKILL.md + any supporting files
│       └── SKILL.md
├── agents/
│   └── reviewer.md        # custom agents (GitHub Copilot)
└── hooks/
    └── typecheck.sh       # scripts the [[hooks]] of kitbag.toml run (optional)
```

Every file is optional except `kitbag.toml`. A directory without one but with `rules/` at its
root is read as a [plugin](#plugins-rules-at-the-root-no-kitbagtoml).

### `kitbag.toml`

```toml
kitbag = 1                    # format version (required)
name = "my-setup"             # required; also the id of managed blocks
description = "Optional"

[mcp_servers.github]          # stdio server
command = "npx"
args = ["-y", "@modelcontextprotocol/server-github"]
env = { GITHUB_PERSONAL_ACCESS_TOKEN = "${GITHUB_PERSONAL_ACCESS_TOKEN}" }
# cwd = "/path"               # working directory (Codex, Gemini CLI, Antigravity, opencode, Grok Build)

[mcp_servers.linear]          # remote server; transport inferred from `url`
url = "https://mcp.linear.app/mcp"
headers = { Authorization = "Bearer ${LINEAR_API_KEY}" }
# transport = "sse"           # stdio | http | sse (only needed for legacy SSE)
# targets = ["claude-code"]   # only these tools (see "Per-tool MCP servers")
# disabled_on = ["cursor"]    # or every tool except these (also for rules, commands, skills, agents)
# disabled = true             # keep the definition; Codex and Grok Build write enabled = false, Antigravity and opencode disabled = true, others skip it

[settings.claude-code]        # per-tool settings, allow-listed keys only
model = "opus"
permissions = { allow = ["Bash(cargo test:*)"], deny = ["Read(./.env)"] }

[settings.codex]
model_reasoning_effort = "high"
```

Unknown keys are rejected with a precise error rather than silently ignored. The format is
versioned (`kitbag = 1`). Future incompatible changes will bump the number, and older kitbag
releases will refuse newer profiles instead of misreading them.

**Per-tool MCP servers.** One shared profile can keep a heavy server out of the tools that
don't need it. `targets` lists the only tools a server is applied to; `disabled_on` lists the
tools it is kept out of. Use one or the other, not both, and give `targets` at least one tool.
Target names are those of `kitbag list-targets` (`claude-code`, `cursor`, `codex`,
`gemini-cli`, `claude-desktop`, `antigravity`, `opencode`, `copilot`, `grok-build`, `windsurf`, `zed`, `hermes`). An unknown name (a typo, or a tool from a newer
kitbag) is a warning that lists the valid ones, and the name is ignored: it matches no tool.
`only` and `except` still load as older names for the same fields (an empty `only` still means
no restriction, with a warning); set each field under one name, not both. Capture writes the new
names. kitbag 0.1.2 and older don't know `targets` / `disabled_on` and refuse a profile that
uses them ("unknown field"), so keep `only` / `except` in a profile shared with people on those
versions.

```toml
[mcp_servers.playwright]       # browser work in Claude Code only
command = "npx"
args = ["@playwright/mcp@latest"]
targets = ["claude-code"]

[mcp_servers.context7]         # everywhere but Cursor
command = "npx"
args = ["-y", "@upstash/context7-mcp"]
disabled_on = ["cursor"]
```

The plan notes each server it skips (`MCP server 'playwright' skipped on cursor: disabled in
profile`). If kitbag applied a server to a tool earlier and the profile now disables it there,
apply removes it from that tool, with a backup, so `kitbag undo` puts it back. It only does so
when kitbag's own records (`applied-servers.json` in the state directory) show that an apply of
the same profile (the same name, from the same directory or git URL) wrote that entry and it
hasn't been edited since. A git URL is compared as written; only shorthand such as
`github:owner/repo` is expanded (to `https://github.com/owner/repo.git`), so
`https://github.com/o/r`, `https://github.com/o/r.git` and `https://github.com/o/r/` count as
different sources, and a server one of them added is left in place when another disables it.
To tell an edit, the records keep a SHA-256 of each whole entry as written. Where secrets are
resolved into a tool's config (Claude Desktop, Antigravity, and Codex fields it can't read
from the environment), that hash is taken over the resolved values; the values themselves are
not stored, and the file is created `0600`. A server you added by hand, one another profile added, or one you
edited stays, and the plan says why. From a git source, removing a server needs the same
opt-in as adding one (`--allow-git-mcp <names>` or `--trust-source`); without it the plan lists
it under "MCP servers not removed" and leaves it. A server that becomes enabled for a tool again
is an addition like any other. `capture --force` into an existing profile keeps the `targets`
and `disabled_on` of the servers it captures again, and keeps the profile's servers it found in
no tool (disabled everywhere, or for a tool not installed on this machine), with a note naming
them; `--prune` drops those instead.

**Per-tool rules, commands, skills and agents.** The same `targets` / `disabled_on` (and the
older `only` / `except`, with the same rules and warnings) work on a rule, command, skill or
agent. They are frontmatter keys in the item's markdown file: `rules/<name>.md`,
`commands/<name>.md`, `agents/<name>.md` and the `SKILL.md` of a skill. The list is YAML, or
comma-separated names in a string. kitbag reads them when it loads the profile and does not
pass them on: the files it writes into a tool carry no `targets` / `disabled_on`. Items without
them are written as before.

```markdown
---
description: Deploy to staging
disabled_on: [codex]       # every tool but Codex; or: targets: [claude-code]
allowed-tools: Bash(make deploy:*)
---

Run `make deploy` ...
```

An item not enabled for a tool is not written to it, and the plan says so (`command 'deploy'
skipped on codex: disabled in profile (listed in 'disabled_on')`). A rule that a tool takes
folded into its instructions file (Codex, Gemini CLI, ...) is left out of that block. If kitbag
wrote the item's file to a tool earlier and the profile now disables it there, apply removes it,
with a backup, so `kitbag undo` puts it back, under the same conditions as for MCP servers:
`applied-files.json` in the state directory must show that an apply of the same profile (the
same name and source) wrote that file, and it must be unchanged since (a skill: every file the
apply wrote, with its folder removed once empty, and files you added to it left). An item you
added by hand, one another profile wrote, or one you edited stays, and the plan says why. From a
git source the removal is listed in the plan like any change. `kitbag status` shows an item
disabled for a tool that is still in it (`apply removes it` or `apply leaves it`).
`capture --force` keeps the `targets` / `disabled_on` of the items it captures again, and keeps
the profile's items with them that it found in no tool; `--prune` drops those.

**Turning an item off or on from the command line.** `kitbag disable` and `kitbag enable` edit
that `targets` / `disabled_on` for you, in a profile directory on this machine (not a git source:
copy it first), and apply nothing; run `kitbag apply` afterwards.

```sh
kitbag disable github --on codex ~/dotagents      # MCP server 'github' off on Codex only
kitbag disable skill:review --on cursor,codex     # prefix when a name is used twice
kitbag enable github --on codex ~/dotagents       # on again there
kitbag disable deploy                             # off on every tool (profile: the current directory)
kitbag enable deploy                              # on everywhere again
```

The item is an MCP server, rule, command, skill or agent; a name that is more than one of them
needs `mcp:`, `rule:`, `command:`, `skill:` or `agent:` in front, and an unknown name or tool is
an error (`claude` is read as `claude-code`). Without `--on` an item is turned off on every
tool, written as a `disabled_on` naming each one (a tool added by a later kitbag is not in it);
enabling clears the list. Comments and everything else in the file stay as they are, `only` /
`except` are rewritten as `targets` / `disabled_on`, and the result is checked before the file is
replaced. Hooks have no per-tool setting and are not handled. Mapping a disabled server to a
tool's own off switch (`enabled = false` ...) is not done yet: apply still leaves it out of that
tool.

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

`env` blocks, `apiKeyHelper`, provider endpoints and similar keys are never carried. Hooks
are not a setting: they have their own opt-in path (see [Hooks](#hooks-hooks-in-kitbagtoml-and-hooks)).
Anything that lets agents act without asking gets an explicit warning in the plan. That
covers `permissions.defaultMode = bypassPermissions` (and `acceptEdits`, `dontAsk`, `auto`),
`permissions.additionalDirectories`, allow rules (broad ones such as `Bash(*)` or
`Bash(rm -rf *)` are named), `approval_policy = never` and `sandbox_mode = danger-full-access`.
It also covers commands and skills whose frontmatter pre-approves tools (`allowed-tools`) or
defines `hooks`, and commands and skills that run shell snippets (``!`cmd` `` in Claude Code,
`!{cmd}` in Gemini CLI commands), and custom agents and subagents that do any of these, start
MCP servers of their own (`mcp-servers`, `mcpServers`) or set a `permissionMode` that never
asks (`acceptEdits`, `auto`, `dontAsk`, `bypassPermissions`).

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
with a `kitbag.toml` or a plugin's `rules/`, is taken as the source, so `--allow-git-mcp github:me/profile` works;
write `--allow-git-mcp=NAME` to be explicit), or `--trust-source`, which implies it. Servers already
in place with the same definition are left as they are and aren't listed; servers from a local
directory apply as written. From a git source, commands and skills that run shell snippets are
skipped, `hooks` and `allowed-tools` are removed from the frontmatter of the rest, custom
agents with `hooks`, `allowed-tools`, `mcp-servers` / `mcpServers`, a permissive `permissionMode` or a shell snippet are skipped whole, and
skill scripts are not made executable, unless you pass the same flag. The warning for local profiles
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

### Agents: `agents/<name>.md`

Custom agents, carried by the `copilot` and `claude-code` targets: one agent per file, named
after the file. `description` is a field of its own and every other frontmatter key is kept
as it is, whichever tool the agent came from.

```markdown
---
description: Reviews pull requests without editing files
tools: ['read', 'search']
model: GPT-5
---

Review the change for bugs and missing tests. Don't edit files.
```

**Copilot** agents use Copilot's `.agent.md` format, so every key of its
[custom agent file reference](https://code.visualstudio.com/docs/agents/reference/custom-agent-file-reference)
(`name`, `argument-hint`, `tools`, `model`, `handoffs`, `mcp-servers`, `user-invocable` ...)
round-trips.

**Claude Code** subagents are markdown files in `~/.claude/agents/` (or
`$CLAUDE_CONFIG_DIR/agents/`) and `.claude/agents/`, with `name` and `description` frontmatter
and the system prompt as the body; see Claude Code's
[subagent docs](https://code.claude.com/docs/en/sub-agents). Capture reads `*.md` files there,
including sub-folders (`review/security.md` becomes `review-security`; two files that flatten
to one name are an error, as for commands), skips a `README.md` with a note, and keeps all the
frontmatter (`name`, `tools`, `disallowedTools`, `model`, `permissionMode`, `skills`,
`mcpServers`, `hooks`, `color` ...). Claude Code identifies a subagent by its `name` key, not
its file name, so that key is kept; an agent that has none (one captured from Copilot, say)
gets one when written, equal to its name in the profile. Apply writes `<name>.md`, never
Copilot's `.agent.md`.

Keys and values are not translated between tools. `tools` and `model` hold each tool's own
tool names and model ids, so an agent moved between Claude Code and Copilot needs them
checked by hand. Apply notes the keys Claude Code's docs don't list (Copilot's `handoffs`,
`mcp-servers` ...) and the Claude Code keys that Copilot has no counterpart for
(`disallowedTools`, `permissionMode`, `mcpServers` ...), and writes them as they are. Other
targets skip agents with a note. A plugin's `agents/` is not read.

Apply is additive and undo removes only the files kitbag wrote. From a git source, an agent with
`hooks`, `allowed-tools`, `mcp-servers` or `mcpServers` (servers it starts itself), a
`permissionMode` that acts without asking (`acceptEdits`, `auto`, `dontAsk`,
`bypassPermissions`) or a shell snippet is skipped unless you pass `--trust-source`. A plain
`tools` list only narrows what an agent may use, so it doesn't count. The plan warns about
such agents in a local profile.

### Hooks: `[[hooks]]` in `kitbag.toml`, and `hooks/`

Hooks are commands a tool runs by itself on an event (before a tool call, when a session
starts ...). That makes them as sensitive as an MCP server's command, so kitbag carries them
only when you ask, at every step. **Seven targets carry them**, each in the file its own docs
name; the others **warn in the plan that the hooks were not written there** (they are never
dropped silently):

| Target | Hook file (user / project) | Scripts go to | Docs |
|---|---|---|---|
| `claude-code` | `settings.json` `hooks` (`~/.claude/`, `.claude/`) | `hooks/kitbag/` next to it | [hooks](https://code.claude.com/docs/en/hooks) |
| `cursor` | `~/.cursor/hooks.json` / `.cursor/hooks.json` | `~/.cursor/hooks/kitbag/` | [hooks](https://cursor.com/docs/agent/hooks) |
| `codex` | `$CODEX_HOME/hooks.json` (default `~/.codex/`) / `.codex/hooks.json` | `hooks/kitbag/` next to it | [hooks](https://developers.openai.com/codex/hooks) |
| `gemini-cli` | `settings.json` `hooks` (`~/.gemini/`, `.gemini/`) | `hooks/kitbag/` next to it | [hooks](https://geminicli.com/docs/hooks/) |
| `grok-build` | `$GROK_HOME/hooks/kitbag.json` (default `~/.grok/`) / `.grok/hooks/kitbag.json` | `hooks/kitbag/` next to it | [hooks](https://docs.x.ai/build/features/hooks) |
| `antigravity` | `~/.gemini/config/hooks.json` / `.agents/hooks.json`, as a hook named `kitbag` | `~/.gemini/config/hooks/kitbag/` | [hooks](https://antigravity.google/docs/hooks) |
| `copilot` | `$COPILOT_HOME/hooks/kitbag.json` (default `~/.copilot/`) / `.github/hooks/kitbag.json` | `hooks/kitbag/` next to it | [hooks reference](https://docs.github.com/copilot/reference/hooks-reference) |
| `claude-desktop`, `opencode`, `windsurf`, `zed`, `hermes` | not carried: warn | none | see below |

Claude Desktop has no hook configuration of its own. opencode runs hooks only as
JavaScript/TypeScript plugins, which kitbag doesn't write. Windsurf's docs now point to Devin
Desktop's `.devin/hooks.v1.json` and give no user-level path or event list, so its hooks are
not verified and not written. Zed has no hook file kitbag has verified against its docs. Hermes
documents [shell hooks](https://hermes-agent.nousresearch.com/docs/user-guide/features/hooks)
in `config.yaml` (and gateway, plugin and outbound hook systems) that kitbag doesn't write yet. Capture reads a Grok Build or Copilot directory's every
`*.json`, and every hook of an Antigravity file that isn't `enabled: false`; apply writes
Grok Build's and Copilot's own `kitbag.json` and Antigravity's own `kitbag` hook, so undo
removes only that. The tools keep their own approval steps: Codex skips a new or changed hook
until you trust it with `/hooks`, Gemini CLI warns before running a project hook it hasn't
seen, Grok Build runs project hooks only in a trusted folder, and Copilot loads repository
hooks only for an admitted workspace. kitbag doesn't bypass any of them.

```toml
[[hooks]]
event = "pre-tool"                    # a neutral event name, see below
matcher = "Bash"                      # optional, as the tool reads it
command = "{{hooks_dir}}/typecheck.sh"
timeout = 30                          # optional, seconds

[hooks.extra]                         # the tool's other keys, kept as they are
statusMessage = "Type-checking"
```

A script a hook runs lives in the profile's `hooks/` directory (`hooks/typecheck.sh`), and
`{{hooks_dir}}` stands for where it is written on apply, so the profile holds no absolute path
of the machine it came from. `hooks.extra` keeps every other key of the tool's entry (Claude
Code's `args`, `shell`, `async`, `if`, `statusMessage` ...). Neutral events are
`session-start`, `session-end`, `prompt-submit`, `pre-tool`, `post-tool`, `post-tool-failure`,
`permission-request`, `notification`, `stop`, `subagent-start`, `subagent-stop`, `pre-compact`
and `post-compact`; an event only one tool has is `<target>:<Name>` (for example
`claude-code:FileChanged` or `cursor:beforeShellExecution`) and applies to that tool only.
Each tool maps the neutral names it has an equivalent for (Cursor's `pre-tool` is `preToolUse`,
Gemini CLI's is `BeforeTool`, Copilot's `stop` is `agentStop`); Gemini CLI's `BeforeAgent` and
`AfterAgent` and Antigravity's `Stop`, `PreInvocation` and `PostInvocation` have no documented
counterpart, so they are carried only as `gemini-cli:…` / `antigravity:…`. A target without an
equivalent for an event reports the hook; it is not guessed. A matcher is written as it is:
tools name their tools differently, so the plan warns to check it. Of the keys in `hooks.extra`,
each tool gets only those its docs list (Cursor `failClosed` and `loop_limit`, Codex
`statusMessage`, `async`, `commandWindows` and `additionalContextLimit`, Gemini CLI `name` and
`description`, Copilot `cwd` and `env`); a hook with another key is not written to that tool
(with a note), except labels such as `statusMessage`, which are left off. Gemini CLI takes
`timeout` in milliseconds and Copilot `timeoutSec`; kitbag converts. In a project, Antigravity's docs say nothing about what a hook command runs relative to, so a
hook that runs a script there is not written (user scope works).

**Capture** (`kitbag capture --include hooks`). Without the flag, hooks are counted and left
out, as before. With it, kitbag reads the hooks of every tool in the table above, from the
files listed there (for Claude Code, `~/.claude/settings.json`, or `.claude/settings.json` with
`--project`; `settings.local.json` is machine-specific and not read). It keeps `command` hooks; `http`, `mcp_tool`, `prompt` and
`agent` hooks, and events kitbag doesn't know, are reported and left out. When a command runs
a file (the command itself, or the file handed to `bash`, `sh`, `node`, `python3` and other
interpreters) that sits in the tool's config directory (or the project), the file is copied
to `hooks/` and the command rewritten to `{{hooks_dir}}/<path>`. Files it sources or calls in
turn are not followed. A script outside that directory is not copied, with a warning. Commands
and scripts are redacted like other text, and scripts that look like secret files are skipped.

**Apply** (`kitbag apply --apply-hooks`). Without `--apply-hooks` nothing is written for
hooks, with `--yes` or without: the plan lists every hook with its command in full and says
where its script is in the profile. With it, scripts go to the `hooks/kitbag/` directory in the
table above (`~/.claude/hooks/kitbag/`, `.claude/hooks/kitbag/` in a project ...), marked
executable where they were, and the hooks are added to the tool's hook file (an existing entry
with the same event, matcher and command is updated, others stay). In a project the command
reaches the scripts through what the tool documents: `${CLAUDE_PROJECT_DIR}`,
`${GEMINI_PROJECT_DIR}`, `${GROK_WORKSPACE_ROOT}`, `$(git rev-parse --show-toplevel)` for Codex,
a path relative to the project for Cursor and Copilot (with `cwd` set). Apply is additive: a profile without hooks removes none, and
`kitbag undo` puts back the settings and removes the script files kitbag wrote. A second apply
with nothing changed is "Up to date". A hook (or script) that capture removed a secret from
is not applied: edit it in the profile to read the secret from the environment. `kitbag
inspect`, `diff`, `sync` and `status` list hooks (`status` marks each `in sync`, `changed`,
`not applied` or `unsupported`, and lists the ones a git source withholds); `--dry-run` never
writes. If the profile has hooks and a target has no place for them, or no equivalent event, the
plan warns and says what it did not write.

**From a git source, `--apply-hooks` is not enough: it also needs `--trust-source`.** Without
both, the plan lists the hooks it withheld, command in full, and nothing is written for them.
This is on top of the removal of `hooks` from skill, command and agent frontmatter described
above, which still applies.

**Doctor.** Every hook kitbag applies is recorded with a hash of its entry and of its scripts
(`applied-hooks.json` in the state directory). `kitbag doctor` reports one that no longer
matches ("this hook no longer matches what kitbag applied") and counts it as a problem. Hooks
you added yourself are not looked at. Hook commands are never resolved from the environment by
kitbag: `${VAR}` stays in the command for the shell to expand, so no hook file holds a secret
kitbag resolved. Shell scripts need LF line endings to run; on Windows keep `hooks/*.sh` as LF
(`* text eol=lf` in the profile's `.gitattributes`).

### Plugins: `rules/` at the root, no `kitbag.toml`

`apply`, `diff`, `status`, `sync` and `doctor` also take a plugin directory, or a git URL to
one (`github:owner/plugin`, `https://…/repo.git//plugins/name`), following the idea of a
portable `rules/` directory in agent plugins. A plugin is a directory with a `rules/`
directory of `*.md` files at its root, one rule per file named after the file, and no
`kitbag.toml` (a directory with a `kitbag.toml` is always loaded as a profile):

```text
my-plugin/
├── .claude-plugin/plugin.json   # optional; its "name" names the plugin
├── rules/
│   ├── typescript.md
│   └── testing.md
├── commands/                    # optional, as in a profile
└── skills/                      # optional, as in a profile
```

A rule's frontmatter is the profile's: `globs` (a list or a comma-separated string),
`alwaysApply` and `description`; `paths` and `applyTo` are accepted as names for `globs`. A
rule without frontmatter, or without any of these fields, applies everywhere. Tools that
can't scope a rule to files (Codex, Gemini CLI) still load it, always on, in the folded rules
section labelled with its globs, as for a profile (see the table above).

kitbag reads the plugin in memory and writes nothing into it. The plugin's name, used for its
managed block and in kitbag's records, is the `name` in `.claude-plugin/plugin.json` or a
root `plugin.json`, if one is there; otherwise it is the directory's name (for a git URL, the
sub-directory's or the repository's) as a slug, like `My Plugin` -> `my-plugin`. A plugin has
no instructions, MCP servers or settings (an `instructions.md` in it is not read, with a
warning); `commands/` and `skills/` are read as in a profile, and `agents/` is not read,
since a plugin's agents are Claude Code subagents (plugins are not carried yet).
A plugin from a git URL is a git source, with the same rules as a git profile. `sync --from`
doesn't work on a plugin, since that writes the edits into a profile. `kitbag undo` takes the
plugin's rules out again (its own rule files and managed block, nothing you wrote), even
after the plugin directory is gone.

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
   Settings values under a secret-sounding name are redacted at any depth (`ui.apiToken`). In
   skill scripts and config files (anything but prose such as `.md`), a quoted value given to
   a secret-sounding name (`export DB_PASSWORD="…"`, `api_key = '…'`) is redacted however it
   looks; a skill file that is JSON (under any extension, comments allowed) or a config format
   (`.toml`, `.yaml`, `.ini`, `.env`, `.cnf`, `.tfvars` …) holding such a value is left out of the
   profile, with a warning.
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
   masked as `••••••••` in every diff. Diffs also mask what already looks like a credential
   in your existing files: known token formats, private keys that span several lines,
   passwords in URLs, `Bearer …`, JSON and TOML values (double- or single-quoted) under a
   secret-sounding name, a `"--token", "…"` or `"--token=…"` pair, also when the value sits
   on the next line as in a pretty-printed `args` array, a quoted random-looking value
   whatever it is called (a long mix of letters and digits, as capture redacts from MCP
   servers), and whatever capture would redact from free text. Ordinary values such as
   model names, package names, paths and UUIDs are shown as they are, and so are, unless
   they sit under a secret-sounding name, all-hex values (checksums, commit hashes, Mongo
   ObjectIds), image digests (`ghcr.io/acme/server@sha256:…`) and ULIDs. New files
   containing resolved values are created `0600`.
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
   the URL it connects to, and the names of the env vars and headers it sets, with anything
   in them that looks like a secret masked. Servers already in place and unchanged are not
   listed. From a git source, those servers are only applied when you opt in with
   `--allow-git-mcp`.

## Safe apply

- **Plan first.** `kitbag inspect` lists what a profile or plugin holds (instructions, rules,
  commands, skills, agents, MCP servers) with a safety summary: the commands its MCP servers
  run, the URLs they connect to, the env vars it references, the files it would install as
  executable, risky settings and anything that looks like a secret, all masked like a diff.
  With `--to` it also lists, per tool, the files `apply` would create or modify. It never
  writes or starts anything. `kitbag diff` and `kitbag apply --dry-run` show a unified diff.
  `kitbag apply` shows a summary and asks before writing. Non-interactive sessions need `--yes`.
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
  else's repository, kitbag refuses to write through links that leave the project or dangle,
  and checks again just before writing, so a link that changes while the plan waits for
  confirmation stops the apply before anything is written.
- **Backups and undo.** Every apply is a transaction. The originals are copied to the state
  directory first, writes are atomic, and a failure part-way through rolls back.
  `kitbag history` lists applies, and `kitbag undo [id]` restores them byte for byte. Undo
  refuses if you edited a file since, or if a backup no longer matches the SHA-256 recorded
  when it was taken, unless you pass `--force`. (The hash sits next to the backups, so it
  catches a backup that changed on its own, not someone who can write to the state directory.) A file whose directory now
  resolves somewhere else (a symbolic link appeared or changed since the apply), or that is
  now a link itself, is left alone even with `--force`: undo reverts the rest, names it and
  exits with status 1. If a file changes between planning and writing, the apply is refused.
  An apply that is killed part-way through (power loss, `kill -9`) stays `pending` in
  `kitbag history`; `kitbag doctor` reports it, and `kitbag undo <id>` puts back the files it
  had written and leaves the rest alone.
  Backups are kept until you delete them: there is no command that prunes old ones. On
  Unix they are written `0600` in `0700` directories, but a backup of a file that held
  resolved secrets holds them too. Each apply is one `backups/<id>/` directory in the state
  directory; deleting it removes that apply from `kitbag history` and from `kitbag undo`.
- **Profile directories.** `init --force` and `capture --force` replace a symbolic link inside
  the profile directory (a file or a sub-directory) with a regular file or directory; they
  never write through it.
- **State directory:** `$KITBAG_STATE_DIR` if set; else, with `--home DIR` (or
  `KITBAG_HOME`), `DIR/.local/state/kitbag`; else `$XDG_STATE_HOME/kitbag`, else
  `~/.local/state/kitbag` (`%LOCALAPPDATA%\kitbag` on Windows).

Apply is additive: removing a server from the profile does not remove it from tools you
already applied to. Use `kitbag undo`, or remove it in the tool. (Disabling a server for a
tool with `targets` / `disabled_on` does remove it there, if kitbag added it; see
"Per-tool MCP servers" and "Per-tool rules, commands, skills and agents".)

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
| `tool only` | a rule file, or text outside kitbag's block, that is only in the tool (see below) |

Without a profile, `kitbag status` shows what changed in each tool since the last apply.
`--exit-code` exits with status 1 on any drift, `--json` prints rows for scripts, and status
never writes.

With a profile, status also lists what a tool has that never made it back to the profile, as
`tool only`:

- a rule file of its own that the profile doesn't have, such as a one-off
  `~/.claude/rules/hotfix.md`, `.cursor/rules/hotfix.mdc` or `~/.grok/rules/hotfix.md` (rule files another profile
  applied are not listed);
- text outside kitbag's block in an instructions file where the profile has its block
  (`CLAUDE.md`, `AGENTS.md`, `GEMINI.md`), paragraph by paragraph, when the profile's
  instructions don't already hold it.

```console
  claude-code    rule hotfix            tool only        ~/.claude/rules/hotfix.md
                 rule file only in this tool, not in the profile
  claude-code    unmanaged text         tool only        ~/.claude/CLAUDE.md
                 2 paragraph(s) outside kitbag's block, not in the profile's instructions
```

Your own text around the block is allowed, so `tool only` (like `removed`) is not drift for
`--exit-code`; add `--strict` (`--exit-code --strict`) to count both. `kitbag diff` adds a
note naming them, and `kitbag doctor PROFILE` lists them without counting them as problems.

`kitbag sync <profile>` is `apply` that won't clobber hand edits: it applies `profile changed`
and `not applied` items (with the usual plan, diff, confirmation, backups and undo), but holds
back each managed block or rule file that was edited in the tool (`tool edited` or
`both changed`). Only that item is left alone: the tool's other instructions, rules, MCP
servers, commands and skills are still applied, in the same undoable transaction. The plan
lists each held-back item as `Skipped: claude-code instructions` with a note on how to resolve
it, and sync exits with status 1. Then either:

- `kitbag sync <profile> --from claude-code` pulls that tool's edited block or rule back into
  the profile (`instructions.md`, `rules/*.md`), redacted like `capture` and refused if it
  still looks like it holds a secret, and applies it to the other targets. The profile files
  are part of the same transaction, so `kitbag undo` reverts them too. Only the managed
  block is pulled back, not your own text around it.
- `kitbag sync <profile> --overwrite` replaces the edit with the profile's version (backed up).

`kitbag sync <profile> --from claude-code --adopt` also pulls in that tool's `tool only`
items, so the other tools get them:

- Each rule file becomes `rules/<name>.md`, and the tool's file is rewritten in kitbag's
  format (backed up). The plan warns when that drops something, such as a frontmatter key
  kitbag doesn't carry.
- The text is added to the end of `instructions.md`. It stays where it is in the tool's file:
  apply doesn't repeat in a block a paragraph the file already has, and pulling that block
  back later with `--from` keeps the paragraph in the profile.
- A rule or paragraph that looks like it holds a secret is left out with a warning, so no
  tool gets a redacted copy. So is a rule in a sub-directory (`rules/team/x.md`), and one that
  an earlier apply of this profile wrote and that was taken out of the profile since.

Without `--adopt`, `--from` names them and leaves them.

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

## Which instruction files does a tool load?

`kitbag doctor --why [DIR]` lists, per tool, the instruction files it reads when started in
`DIR` (the current directory by default), in load order, the ones it reads later or skips
and why, and known traps. It only looks at files; nothing is run or changed.

```console
$ kitbag doctor --why ~/code/app --to claude,codex
Instruction files for ~/code/app

Claude Code
  load 1     ~/.claude/CLAUDE.md  user instructions
  load 2     ~/code/CLAUDE.md  parent directory
  skip       ~/code/app/AGENTS.md  shadowed: Claude Code reads AGENTS.md only when there is no CLAUDE.md in or above the directory, and ~/code/CLAUDE.md exists
  note       set instructionFiles to "claude-md-and-agents-md" in ~/.claude/settings.json to read both, or @import AGENTS.md from CLAUDE.md

Codex CLI
  trap       ~/code/app/.codex/config.toml  ignored: Codex reads a project's .codex/config.toml only when the project is trusted, and ~/code/app is not marked trusted in ~/.codex/config.toml
  load 1     ~/code/app/AGENTS.md  project
  trap       ~/code/app/AGENTS.md  truncated: Codex reads only the first 32.0 KiB of its 40.2 KiB (project_doc_max_bytes is 32.0 KiB in total for all project files; raise it in config.toml)
```

Lines are `load N` (read at start, in that order), `later` (read when the agent touches
matching files, such as path-scoped rules), `skip` (exists but isn't read: wrong name, too
large, untrusted, shadowed by another file), `trap`, `note` and `unverified` (a question
the tool's documentation doesn't answer; kitbag says so instead of guessing). It knows about:

- **Claude Code**: the managed, user and `CLAUDE.md` / `.claude/CLAUDE.md` / `CLAUDE.local.md`
  files from the file system root down, `.claude/rules/`, and that any of those in or above
  the directory turns `AGENTS.md` off (unless `instructionFiles` in `~/.claude/settings.json`
  says otherwise). It flags a `~/.claude/CLAUDE.md` that links to a project's `AGENTS.md`
  (read twice) and the bug that skips `AGENTS.md` under the home directory.
- **Codex CLI**: `AGENTS.override.md` / `AGENTS.md` (one per directory, from the git root
  down), the 32 KiB total cap (`project_doc_max_bytes`), and a project `.codex/config.toml`
  that is ignored because the project isn't trusted.
- **Cursor**: `.cursor/rules/*.mdc` by rule type and `AGENTS.md`; user rules live in
  Cursor's app storage, not on disk, so they can't be listed.
- **Gemini CLI** (with `context.fileName`), **Antigravity** (including its 24,000-byte
  truncation) and **opencode** (`AGENTS.md`, else `CLAUDE.md`, per directory).
- **Grok Build**: `$GROK_HOME` files and rules, then Claude Code's and Cursor's home files and
  rules (as `[compat.claude]` / `[compat.cursor]` and the `GROK_*_ENABLED` variables allow),
  `[paths] extra_rule_dirs`, then every directory from the git root down, where every
  instruction file name that exists loads (`AGENTS.md` and `CLAUDE.md` alike). It flags a rule
  whose frontmatter limits it to some files, since Grok loads every rule for every file.

By default every tool that reads instruction files is shown; `--to` limits it, and `--home`
(or `KITBAG_HOME`) points it at a sandbox. Under `--home`, Claude Code's managed policy file is
not checked.

## How many tokens do my MCP servers cost?

Every MCP server's tool definitions are sent to the model before your first prompt.
`kitbag doctor --mcp-tokens` estimates that cost per tool, so you can see which servers to
switch off (with `targets` / `disabled_on`, see above). For example (the numbers are made up):

```console
$ kitbag doctor ~/dotfiles/kitbag --to claude,cursor --mcp-tokens
...
MCP tool definitions
  Claude Code
    github      ~5,830 tokens  26 tool(s)
    linear      skipped remote (http) server, not contacted: its URL and headers may carry credentials
    playwright  ~3,410 tokens  21 tool(s)
    total       ~9,240 tokens from 2 server(s)
  Cursor
    github      ~5,830 tokens  26 tool(s)
    linear      skipped remote (http) server, not contacted: its URL and headers may carry credentials
    playwright  skipped disabled for this tool in the profile (not in its 'targets'); not counted
    total       ~5,830 tokens from 1 server(s)
```

This **runs the servers' commands**, so it is off unless you pass the flag; plain `doctor`
starts nothing. That includes the servers in a project's own config (`.mcp.json` and the like)
under `--project`, so only use it on setups you trust. From a git `SOURCE`, a server is only
started if you allow it, as for `apply`: name it with `--allow-git-mcp NAMES`, or pass
`--allow-git-mcp` with no names or `--trust-source` to allow them all; the others are listed as
skipped with the flag to pass.

Each local server is started once, with its `${VAR}` references filled in from your
environment, asked for its tools, and stopped; it gets `--mcp-timeout SECS` (default 10) in
all. On Linux and macOS it runs in a process group of its own and the whole group is stopped,
so a wrapper such as `npx`, `uvx` or `sh -c` can't leave the real server running. On Windows
only the process kitbag started is stopped, and a bare command such as `npx` also finds
`npx.cmd` (through `PATHEXT`). A relative `cwd` (or a relative `command` such as
`./server.js`) is taken from where you run doctor, not from the profile or project. A server that fails to start, exits or times out is reported and the others
are still counted; this is not a doctor problem, so doctor still exits 0. A server needing a
variable that isn't set is not started. kitbag doesn't show or keep
anything a server prints: its error output is discarded and its tool list is only counted.
Remote servers are not contacted, since their URL and headers may carry credentials.

With a profile (`SOURCE`), its servers are counted for each tool they apply to, and a server
disabled for a tool is shown as skipped and not counted against it. Without one, the servers
in each tool's own config are counted, and ones disabled there are skipped.

The count is an estimate: the characters of each tool's name, description and input schema
as compact JSON, divided by 4. It is not any model's exact tokenizer, and tools may add some
wrapping of their own.

## Commands

| Command | What it does |
|---|---|
| `kitbag init [DIR]` | Write a commented starter profile |
| `kitbag capture [--from IDS] [-o DIR] [--dry-run] [--force] [--prune] [--include hooks]` | Read the current setup into a profile (secrets redacted). Writes only into a new or empty directory. `--force` replaces an existing profile's files and leaves anything else in the directory alone; the profile's MCP servers (and rules, commands, skills and agents with `targets` / `disabled_on`) found in no tool are kept unless `--prune`. With several tools, one whose config can't be read is reported and the rest are captured |
| `kitbag disable ITEM [--on IDS] [PROFILE]`, `kitbag enable ...` | Turn an MCP server, rule, command, skill or agent off or on for some tools (default: all) by editing its `targets` / `disabled_on` in a local profile; applies nothing |
| `kitbag diff <SOURCE> [--to IDS] [--exit-code]` | Show exactly what `apply` would change; never writes |
| `kitbag apply <SOURCE> [--to IDS] [--yes] [--dry-run] [--apply-hooks]` | Plan, confirm, write with backups; hooks only with `--apply-hooks` (and, from a git source, `--trust-source`) |
| `kitbag inspect <SOURCE> [--to IDS] [--json]` | What a profile or plugin holds, what in it runs commands, reads env vars, installs executables or connects to the network, and (for the tools in `--to`, default `detected`) which files `apply` would write; never writes or starts anything |
| `kitbag status [SOURCE] [--to IDS] [--exit-code [--strict]] [--json]` | Which managed instructions and rules differ from the profile, or changed since the last apply; never writes |
| `kitbag sync <SOURCE> [--to IDS] [--from ID [--adopt] \| --overwrite]` | Apply without overwriting instructions edited in a tool; pull an edit back with `--from`, and what is only in that tool with `--adopt` |
| `kitbag undo [ID] [--force]` | Revert the latest (or a specific) apply; with an ID, also an interrupted one |
| `kitbag history [--json]` | List applies |
| `kitbag doctor [SOURCE]` | Check installed tools' configs, a profile, required env vars, risky settings, interrupted applies |
| `kitbag doctor [SOURCE] --mcp-tokens [--mcp-timeout SECS] [--allow-git-mcp [NAMES]] [--trust-source]` | Also start each local MCP server to list its tools, and estimate the tokens its tool definitions take up in each tool |
| `kitbag doctor --why [DIR] [--to IDS]` | Which instruction files each tool loads in `DIR` (default: the current directory), in order, which it skips and why, and known traps |
| `kitbag list-targets [--json]` | Targets, what they support, which are detected, file locations |

Common options:

- `--to` / `--from` take target ids (comma-separated; `claude` means `claude-code`), `all`,
  or `detected` (the default).
- `--scope user|project` and `--project DIR` choose between global and per-project config.
- `--secrets resolve|never`, `--no-overwrite`, `--allow-git-mcp [NAMES]`,
  `--allow-risky-settings` (`--trust-source`) and `--apply-hooks` apply to `apply`, `diff` and `inspect`.
- `--path SUBDIR` selects a profile inside a repository.
- `--require-pin` (`apply`, `diff`, `inspect`, `status`, `sync`, `doctor`) refuses a git
  source that names no branch, tag or commit.
- `--home DIR` (or `KITBAG_HOME`) treats `DIR` as the home directory for sandboxes and tests.
  It also ignores `CODEX_HOME`, `CLAUDE_CONFIG_DIR`, `XDG_CONFIG_HOME` and `APPDATA`, and
  keeps backups and history in `DIR/.local/state/kitbag` (not `XDG_STATE_HOME` or
  `LOCALAPPDATA`) unless `KITBAG_STATE_DIR` is set, which always wins.

**Sources** are a local profile or plugin directory (see [Plugins](#plugins-rules-at-the-root-no-kitbagtoml)) or a git URL to one:

- `github:owner/repo`, `gh:owner/repo/sub/dir#ref`, `github:owner/repo@ref`, `gitlab:` and
  `bitbucket:` shorthands;
- `https://github.com/owner/repo/tree/<ref>/<dir>`;
- any `https://…/repo.git`, `git@host:owner/repo.git`, `ssh://` or `file://` URL, with
  `//sub/dir` to pick a sub-directory and `@ref` or `#ref` at the end to pin it.

A ref is a branch, a tag or a commit SHA, full or abbreviated: `github:you/dotagents@v1.2`,
`github:you/dotagents/work@3f9c2e1`, `https://git.example.com/a.git//sub#main`. `@ref` and
`#ref` mean the same; giving both is an error. The `@` of credentials or an SSH user
(`https://user:token@host/…`, `git@host:…`) is never read as a ref. Without a ref, kitbag
takes the repository's default branch. `--require-pin` refuses such an unpinned git source
(a branch or tag still counts as a pin).

Whatever the ref, kitbag fetches it, checks out the commit it points to and shows that
commit's full SHA in the plan, in `inspect` and in `status`. A successful apply records the
SHA with its transaction. When this machine already applied the same source (the URL and
sub-directory, whatever the ref) at another commit, `apply`, `diff` and `sync` warn that it
moved and print both SHAs before asking to go on, and `status` shows a `source commit` row as
`profile changed`, which `--exit-code` counts as drift. Without a source, `status` names the
commit of each git source it compares against. An apply that finds every file already matching still records the new SHA, in a transaction
with no files, when the source moved; if the SHA is the same, nothing is recorded. Applies
made before kitbag recorded SHAs have none, so there is nothing to compare with until the next one.

kitbag fetches with your own `git` (shallow, no prompts) into a temporary directory that is
removed afterwards; an abbreviated SHA, which `git fetch` can't ask for, fetches the
branches and tags in full to find it. Cleartext `http://` and `git://` sources are refused, because anyone on the
network could swap the profile, and so is an `https://` server that redirects to one; pass
`--allow-insecure-source` to accept them. Credentials in a URL (`https://user:token@host/…`)
are passed to `git` only: plans, errors, `history` and the transaction log show the URL
without them.

## Install

- **Cargo:** `cargo install kitbag` (from [crates.io](https://crates.io/crates/kitbag)). For unreleased changes,
  `cargo install --git https://github.com/cargopete/kitbag kitbag`, or `cargo install --path .` from a checkout.
- **Prebuilt binaries:** static builds for Linux (x86_64/aarch64, musl), macOS (Intel and
  Apple Silicon) and Windows (x86_64/arm64) are attached to each
  [GitHub Release](https://github.com/cargopete/kitbag/releases), with SHA-256 checksums and
  a signed build provenance attestation; check an archive with
  `gh attestation verify kitbag-<target>.tar.gz -R cargopete/kitbag`.
- **Script:** `curl -fsSL https://raw.githubusercontent.com/cargopete/kitbag/main/install.sh | sh`.
  It verifies the checksum (and stops if it can't), replaces an existing binary in one rename,
  and installs to `~/.local/bin`. Set `KITBAG_VERSION` or
  `KITBAG_INSTALL_DIR` to change either. On Windows, use
  `irm https://raw.githubusercontent.com/cargopete/kitbag/main/install.ps1 | iex`.
  The same scripts are attached to each [GitHub Release](https://github.com/cargopete/kitbag/releases),
  so you can pin the installer to a tag:
  `curl -fsSL https://github.com/cargopete/kitbag/releases/download/vX.Y.Z/install.sh | sh`.

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

No further candidates are named here; open an issue for the tool you miss.

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
- At project scope, Cursor, Codex and opencode all read `AGENTS.md`. When Cursor is targeted
  with Codex or opencode, Cursor also sees the folded rules section.
- **Antigravity** gets rules as files of their own, `rules/<name>.md` under `~/.gemini/config/` or
  `.agents/`, each with the `trigger` Antigravity requires (it silently discards a rule file
  without one): `always_on` for a rule that applies everywhere, `glob` with `globs` (one
  comma-separated string) for a file-scoped rule, `model_decision` for an on-demand rule with a
  description and `manual` (loaded only when you @-mention it, which the CLI can't do) for one
  without. Capture reads those files back (a file with no valid trigger is skipped with a note) and
  keeps a rule file's version of a rule that is also folded into `GEMINI.md`; a rule that is only
  folded there by another tool is left to that tool's capture. Rule files over 24,000 bytes are
  truncated by Antigravity, and the plan warns. Skills go to `skills/<name>/` in the same two places
  (name and description filled in when missing; `.agents/skills/` is the directory Codex uses at
  project scope, so applying both writes each skill once). Antigravity shares `~/.gemini/` with
  Gemini CLI: it still reads the rules block Gemini CLI folds into `~/.gemini/GEMINI.md`, so when
  both are targets at user scope it sees a rule twice (always on from the block, with its own
  trigger from its file), and the plan says so; the two tools' skills go to different directories
  (`~/.gemini/skills/` for Gemini CLI), which the Antigravity CLI reads both of and the app only
  `~/.gemini/config/skills/`. Workflows (`.agents/workflows/`, Antigravity's older commands) are
  not carried: Google has deprecated them in favour of skills, and its docs list two different
  global directories for them. Agents, hooks and plugins aren't carried either. kitbag leaves
  Antigravity's MCP tool approvals (Ask by default) alone.
- **opencode** 2 keeps MCP servers under `mcp.servers` and turns one off with `disabled: true`;
  opencode 1 kept them directly under `mcp` with `enabled: false`, and opencode 2 still reads
  that. kitbag writes the layout a config file already uses (`mcp.servers` for a new one), so a
  file opencode 1 reads stays readable, and capture reads both. In opencode 1's layout a server
  named `servers` or `timeout` would overwrite opencode's own key, so it is skipped with a
  warning; rename it in the profile. opencode reads
  `~/.claude/CLAUDE.md` (or a project's `CLAUDE.md`) only while there is no `AGENTS.md`; the
  plan notes when the `AGENTS.md` kitbag creates takes its place. Files listed under
  `instructions` and `{file:...}` references are not carried, and its tool permissions are left
  alone. opencode has no rule files (its rules are `AGENTS.md`), so profile rules are folded into
  `AGENTS.md` as labelled sections, as for Codex (the block is written once when both are
  targeted), and capture reads them back. Commands are `commands/<name>.md`, skills
  `skills/<name>/SKILL.md` and agents `agents/<name>.md` in `~/.config/opencode/` or
  `.opencode/`; opencode 1's singular `command/`, `skill/` and `agent/` are read, and written to
  when they are the folders that exist. A command keeps `description`, `agent`, `model` and
  `subagent` (`subtask`); an agent keeps the keys opencode documents (`mode`, `model`,
  `permissions` / `permission`, `steps`, `hidden`, `color`, ...) and its body is the system
  prompt. Keys other tools use (`argument-hint`, `allowed-tools`, a `tools` list, `permissionMode`,
  `hooks`, a `model` alias such as `sonnet`) are left out with a note, never translated. Agents
  and commands defined inside `opencode.json`, opencode 1's `mode/` agents and plugins are not
  carried. opencode also reads `.claude/skills`, so with Claude Code targeted too a skill is
  visible from both folders.
- **Grok Build** also reads Claude Code's files (`CLAUDE.md`, `.claude/rules`, `.claude/skills`,
  `.claude/commands`, `.mcp.json`) and `~/.agents/skills` by default. The plan notes when the
  same instructions would load twice, and that Codex's folded rules in a shared `AGENTS.md` sit
  next to `.grok/rules/`. Commands are Claude Code style markdown; Grok counts positional
  arguments from `$0`, so kitbag warns about commands that use `$1`-style placeholders. Grok
  Build agent definitions (`.grok/agents/`), personas, plugins, hooks and settings are not
  carried yet.
- **Windsurf** (now also shipped as Devin Desktop) keeps MCP servers in
  `~/.codeium/windsurf/mcp_config.json` and global rules in
  `~/.codeium/windsurf/memories/global_rules.md`; kitbag writes those, and nothing at project
  scope except `AGENTS.md`, since Windsurf's docs describe no project MCP file. Devin Desktop's
  own files (`~/.config/devin/mcp_config.json`, `.devin/`) are not written. A server marked
  `disabled` in the profile is not written (the docs list no such key), though capture reads
  `disabled: true`, and Windsurf-only keys on an entry are kept. SSE servers are skipped, since
  the docs don't say how the file marks one. Windsurf's docs have limited `global_rules.md` to
  6,000 characters, and kitbag warns when the instructions are longer. Rules
  (`.windsurf/rules/`), workflows, skills and agents are not carried yet.
- **Zed** keeps MCP servers ("context servers") under `context_servers` in `settings.json`, in
  the user config directory and in a project's `.zed/settings.json`; the file may hold comments
  and trailing commas, which are kept, and Zed's own keys on an entry (`timeout`, `oauth`, `source` ...)
  are kept too. A server the profile disables is written with `enabled: false`, and capture reads
  that back. Zed documents no env-reference syntax, so references are resolved when applied
  (`--secrets never` keeps `${VAR}` placeholders, which Zed will not expand); the file is then
  0600, and a secret written to a project's `.zed/settings.json` is warned about, since that
  file is usually committed. SSE servers are skipped; `cwd` is dropped. Servers from a Zed
  extension (no `command` or `url`) and the older layout where `command` is an object are not
  captured, and say so. Zed reads one project instructions file, the first that exists of
  `.rules`, `.cursorrules`, `.windsurfrules`, `.clinerules`, `.github/copilot-instructions.md`,
  `AGENT.md`, `AGENTS.md`, so kitbag writes its block there (counting
  `.github/copilot-instructions.md` when Copilot is a target too), and capture reads that file
  only, naming the others it ignores. The Rules Library lives in Zed's own database and is being
  replaced by skills and the personal `AGENTS.md`; kitbag doesn't read or write it. Rules,
  commands, skills (`~/.agents/skills/`, `.agents/skills/`), agents and settings are not carried
  yet. The config directory follows Zed's source (`XDG_CONFIG_HOME` on Linux, `~/.config/zed` on
  macOS); `FLATPAK_XDG_CONFIG_HOME` is not honoured.
- **Hermes Agent** (Nous Research) keeps MCP servers under `mcp_servers` in
  `~/.hermes/config.yaml` (or `$HERMES_HOME/config.yaml`; run kitbag with `HERMES_HOME` set to
  `~/.hermes/profiles/<name>` to use one of its profiles). kitbag edits only the entries it
  writes: the rest of the file, its comments and the servers you added stay as they are, and
  keys Hermes has that kitbag doesn't manage (`timeout`, `tools`, `auth`, `sampling` ...) are
  kept while the entry runs the same command or URL. A server marked `disabled` is written with
  `enabled: false`, and SSE servers with `transport: sse`. If `mcp_servers` is written in a form
  that can't be edited in place (a flow mapping with entries, an alias), the plan says so and
  leaves the file alone. Hermes documents no project MCP file, so none is written at project
  scope. Instructions go in a managed block in `SOUL.md`, next to what is already there (Hermes
  documents it as its identity file; it has no global AGENTS.md). Capture reads only that
  block, never the persona text around it (a fresh install seeds one), so it doesn't end up in
  the other tools' instructions, and says so; in a project, to `.hermes.md` or `HERMES.md` if there is
  one, since Hermes loads that before `AGENTS.md`, otherwise to `AGENTS.md`, with a warning when
  an `AGENTS.override.md` would shadow it. `.env`, `auth.json` and `mcp-tokens/` are never read
  or written (capture warns when it finds them), and Hermes's skills, memories, sessions and
  profiles are not carried yet.
- **GitHub Copilot** and Claude Code both read a project's `.mcp.json`. kitbag writes it in
  Claude Code's shape for both (no `cwd`, no `tools`; a `cwd` is dropped with a warning), so
  applying both writes each server once, and a server the profile keeps off one of them stays
  in the file while the other still gets it. Copilot CLI reads `.mcp.json` as is. VS Code reads
  an existing workspace `.mcp.json`; the experimental `chat.mcp.workspaceRootConfig.enabled`
  setting only controls whether its Add Server flow offers that file. VS Code's
  older `.vscode/mcp.json` is deprecated and Copilot CLI doesn't read it, so kitbag only reads it
  at capture. In the user file (`~/.copilot/mcp-config.json`)
  servers get `tools: ["*"]` unless the entry already lists its own tools, which are kept
  (Copilot CLI's reference marks `tools` required). Copilot CLI documents `$VAR`, `${VAR}` and
  `${VAR:-default}` in `env` and expansion in `headers`, and VS Code `${env:VAR}` in `command`,
  `args`, `env` and `headers`; neither says references expand in `url` or `cwd`, so kitbag
  still writes them as `${VAR}` there but warns. A bare `$VAR` in a Copilot MCP file is captured
  as a reference.
  Rules become `*.instructions.md` files: an always-on rule gets `applyTo: "**"`, an on-demand
  rule no `applyTo`. A server using VS Code's `${input:...}` or `${command:...}` variables in
  `.vscode/mcp.json` is skipped at capture. Commands become VS Code prompt files
  (`.github/prompts/<name>.prompt.md`) at project scope only: VS Code keeps user prompt files
  in its own profile storage, and Copilot CLI doesn't read prompt files (it reads Claude
  Code's `.claude/commands/`). A prompt file keeps `description`, `argument-hint` and VS Code's
  `name`, `agent`, `model` and `tools`; other command keys (`allowed-tools` ...) are left out with
  a note, and `$ARGUMENTS` is written as is with a warning, since VS Code documents
  `${input:NAME}` instead. Skills go to `$COPILOT_HOME/skills/` and `.github/skills/`, which
  Copilot CLI and VS Code both read; `name` and `description` are filled in when `SKILL.md`
  lacks them, and a name VS Code wouldn't load (not lowercase letters, digits and hyphens) is
  warned about. Custom agents go to `$COPILOT_HOME/agents/<name>.agent.md` (default
  `~/.copilot/agents/`) and `.github/agents/<name>.agent.md`, the folders Copilot CLI and
  VS Code both read; capture also reads plain `<name>.md` files there, which both accept, and
  apply writes back to such a file when it is the one already there. Chat modes are the old
  name of custom agents: capture reads `.github/chatmodes/*.chatmode.md` as agents, and apply
  writes them as `.agent.md` files in `.github/agents/`, as VS Code says to rename them, and
  leaves the old file alone (with a note). When two files have the same name, the
  `.agent.md` wins over a `.md`, and an agent over a chat mode. Agents VS Code keeps in its
  own profile storage aren't read. Copilot's own MCP and folder trust prompts are left alone.
- Apply is additive and doesn't delete entries that were removed from the profile (see above).
- **Not carried yet:** output styles (`output-styles/`)
  and plugins; hooks only for Claude Code, and only with `--include hooks` (see
  [Hooks](#hooks-hooks-in-kitbagtoml-and-hooks)). Capture lists what it found of these and
  leaves them out. It also drops
  `outputStyle` when it names a custom style (built-ins such as `Explanatory` are kept),
  since the style file wouldn't be in the profile.
- **Encodings.** Capture reads UTF-8 (with or without a byte-order mark) and UTF-16 with a
  byte-order mark. Instructions, rules and commands in any other encoding are skipped with a
  warning. Skill supporting files are copied byte for byte. A UTF-8 byte-order mark at the
  start of a JSON or TOML config is kept when kitbag writes the file back.
- **Size limits.** Profile files are limited to 2 MiB, skills to 200 files and a whole
  profile to 64 MiB. Capture skips files that are too large with a warning, and refuses to
  write a profile over the total, so it never writes a profile that can't be applied.

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
