# kitbag

**Pack your AI agent setup once, unpack it into any coding tool.**

[![CI](https://github.com/cargopete/kitbag/actions/workflows/ci.yml/badge.svg)](https://github.com/cargopete/kitbag/actions/workflows/ci.yml)
[![License: MIT](https://img.shields.io/badge/license-MIT-blue.svg)](LICENSE)

![kitbag capturing a Claude Code setup, applying it to Cursor, and undoing](docs/demo.gif)

Compared with ruler, rulesync and caliber, kitbag is for your personal setup: capture MCP servers, rules and skills from the tools you already use, keep secrets as env refs, and apply the same profile on every tool and every machine, with a plan and undo.

| | kitbag | ruler | rulesync | caliber |
|---|---|---|---|---|
| Starts from | The setup already on your tools (`capture`) | A `.ruler/` folder in the project | Unified `.rulesync/` files | An agent-written setup, then continuous sync |
| Ships as | One static binary (macOS, Linux, Windows) | Node.js | npm | npm / `npx` |

kitbag captures your MCP servers, instructions, rules, slash commands, skills and settings
from the AI coding tools you already use. It stores them as a small, git-friendly **profile** with secrets
redacted and applies that profile to any other tool in one step. Claude Code, Cursor,
OpenAI Codex CLI, Gemini CLI, Claude Desktop, Google Antigravity, opencode (the last two
for MCP servers and instructions), GitHub Copilot (MCP servers, instructions, rules, prompt files and skills; user-level commands as skills)
xAI's Grok Build CLI, Windsurf, Zed, Hermes Agent, Amp, Cline, Roo Code and Continue (the last seven for MCP servers and instructions; Hermes also for skills and shell hooks, Zed and Amp also for skills, Windsurf also for commands (workflows) and, in a project, rules and skills, Continue also for project rules and, at user scope, commands, Cline and Roo Code also for rules, commands and skills) are supported, at user and
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

# A public starter profile, with no setup and nothing written
kitbag diff github:cargopete/dotagents --to all
```

Starting from scratch? `kitbag init my-profile` writes a commented starter profile, and
[`examples/profile`](examples/profile) shows a realistic one. The same profile is published
at [`github:cargopete/dotagents`](https://github.com/cargopete/dotagents), which you can
preview with `kitbag diff github:cargopete/dotagents --to all`.

## Targets

| Target id        | Tool              | Scopes        | MCP | Instructions | Rules      | Commands                | Skills | Agents | Output styles | Settings |
|------------------|-------------------|---------------|-----|--------------|------------|-------------------------|--------|--------|---------------|----------|
| `claude-code`    | Claude Code       | user, project | ✓   | ✓            | ✓          | ✓                       | ✓      | ✓      | ✓             | ✓        |
| `cursor`         | Cursor            | user, project | ✓   | project      | project    | ✓                       | ✓      | ✓      | -             | user     |
| `codex`          | OpenAI Codex CLI  | user, project | ✓   | ✓            | folded     | user                    | ✓      | ✓      | -             | ✓        |
| `gemini-cli`     | Gemini CLI        | user, project | ✓   | ✓            | folded     | ✓                       | ✓      | ✓      | -             | ✓        |
| `claude-desktop` | Claude Desktop    | user          | ✓ (local servers) | -  | -          | -                       | -      | -      | -             | -        |
| `antigravity`    | Google Antigravity | user, project | ✓  | ✓            | ✓          | as skills               | ✓      | ✓      | -             | -        |
| `opencode`       | opencode          | user, project | ✓   | ✓            | folded     | ✓                       | ✓      | ✓      | -             | ✓        |
| `copilot`        | GitHub Copilot (CLI, VS Code) | user, project | ✓ | ✓    | ✓          | project; user as skills | ✓      | ✓      | -             | user     |
| `grok-build`     | Grok Build        | user, project | ✓   | ✓            | ✓*         | ✓                       | ✓      | -      | -             | -        |
| `windsurf`       | Windsurf          | user, project | user | ✓           | project    | ✓                       | project | -     | -             | -        |
| `zed`            | Zed               | user, project | ✓   | ✓            | -          | -                       | ✓      | -      | -             | user     |
| `hermes`         | Hermes Agent      | user, project | user | ✓           | -          | -                       | ✓      | -      | user          | user     |
| `amp`            | Amp               | user, project | ✓   | ✓            | -          | as skills               | ✓      | -      | -             | user     |
| `cline`          | Cline             | user, project | user | ✓           | ✓          | ✓                       | ✓      | -      | -             | -        |
| `roo`            | Roo Code (alias: `roo-code`) | user, project | ✓ | ✓    | ✓          | ✓                       | ✓      | -      | -             | -        |
| `continue`       | Continue          | user, project | ✓   | ✓            | project    | user                    | -      | -      | -             | -        |
| `vscode`         | VS Code (editor; `[editor.vscode]`) | user | -  | -            | -          | -                       | -      | -      | -             | user     |

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
| Claude Code | `~/.claude.json` (`mcpServers`), `~/.claude/{CLAUDE.md, rules/, commands/, skills/, agents/, settings.json}` (plugins, by reference: `enabledPlugins` and `extraKnownMarketplaces` in `settings.json`; `plugins/installed_plugins.json` is read for versions); honours `CLAUDE_CONFIG_DIR` | `.mcp.json`, `CLAUDE.md` (or `.claude/CLAUDE.md`), `.claude/{rules,commands,skills,agents}/`, `.claude/settings.json` (also `enabledPlugins` and `extraKnownMarketplaces`); capture also reads local-scope servers from `~/.claude.json` |
| Cursor | `~/.cursor/mcp.json`, `~/.cursor/commands/`, `~/.cursor/skills/`, `~/.cursor/agents/*.md` | `.cursor/mcp.json`, `.cursor/rules/*.mdc`, `AGENTS.md`, `.cursor/commands/`, `.cursor/skills/`, `.cursor/agents/*.md`; capture also reads legacy `.cursorrules` |
| Codex CLI | `$CODEX_HOME/config.toml` (default `~/.codex`; plugins, by reference: `[marketplaces.<name>]` and `[plugins."<name>@<marketplace>"]`), `$CODEX_HOME/AGENTS.md`, `$CODEX_HOME/prompts/`, `~/.agents/skills/`, `$CODEX_HOME/agents/*.toml` | `.codex/config.toml` (trusted projects only; also the same plugin tables), `AGENTS.md`, `.agents/skills/`, `.codex/agents/*.toml` |
| Gemini CLI | `~/.gemini/settings.json`, `~/.gemini/GEMINI.md`, `~/.gemini/commands/*.toml`, `~/.gemini/skills/`, `~/.gemini/agents/*.md` | `.gemini/settings.json`, `GEMINI.md`, `.gemini/commands/`, `.gemini/skills/`, `.gemini/agents/*.md` |
| Claude Desktop | `claude_desktop_config.json` in `~/Library/Application Support/Claude` (macOS), `%APPDATA%\Claude` (Windows), `~/.config/Claude` (Linux) | n/a |
| Antigravity | `~/.gemini/config/mcp_config.json`, `~/.gemini/GEMINI.md` (the same file and managed block as Gemini CLI); capture also reads `~/.gemini/AGENTS.md` and `~/.gemini/config/{AGENTS,GEMINI}.md`; `~/.gemini/config/rules/*.md`, `~/.gemini/config/skills/` (also where commands go, as skills), `~/.gemini/config/agents/<name>.md` (plugins, by reference: the `entries` of `~/.gemini/config/plugins.json`) | `.agents/mcp_config.json`, `AGENTS.md` (shared with Codex and Cursor); capture also reads `GEMINI.md` and `.agents/{AGENTS,GEMINI}.md`; `.agents/rules/*.md`, `.agents/skills/` (the same directory as Codex's project skills; commands go there too, as skills), `.agents/agents/<name>.md` (plugins, by reference: the `entries` of `.agents/plugins.json`) |
| opencode | `~/.config/opencode/opencode.json` or `opencode.jsonc` (`mcp.servers`), `~/.config/opencode/{AGENTS.md, commands/*.md, skills/<name>/, agents/*.md}` (plugins, by reference: the npm packages in the config's `plugins` array); honours `XDG_CONFIG_HOME` | `opencode.json(c)` or `.opencode/opencode.json(c)` (the first that exists, else `opencode.json`), `AGENTS.md` (shared with Codex, Cursor and Antigravity), `.opencode/{commands,skills,agents}/`; capture reads every config file there is |
| Grok Build | `$GROK_HOME/config.toml` (`[mcp_servers.<name>]`, default `~/.grok`; plugin folders, by reference: `[plugins] paths`), `$GROK_HOME/{AGENTS.md, rules/*.md, commands/*.md, skills/}` | `.grok/config.toml`, `AGENTS.md` (shared with Codex, Cursor, Antigravity and opencode), `.grok/{rules,commands,skills}/`; Grok reads project files only in folders you have trusted |
| Windsurf | `~/.codeium/windsurf/mcp_config.json` (`mcpServers`), `~/.codeium/windsurf/memories/global_rules.md`, `~/.codeium/windsurf/global_workflows/<name>.md` | `AGENTS.md` (shared with Codex, Cursor, Antigravity, opencode and Grok Build); capture also reads the legacy `.windsurfrules`; `.windsurf/rules/<name>.md`, `.windsurf/workflows/<name>.md`, `.windsurf/skills/<name>/`; Windsurf's docs describe no project MCP file, so MCP servers are not written at project scope |
| Zed | `settings.json` in `~/.config/zed` (macOS), `$XDG_CONFIG_HOME/zed` or `~/.config/zed` (Linux), `%APPDATA%\Zed` (Windows) (`context_servers`; extensions, by reference: `auto_install_extensions`), and `AGENTS.md` next to it (Zed's personal instructions), `~/.agents/skills/<name>/` (skills) | `.zed/settings.json` (`context_servers`; Zed starts these only in a trusted worktree); instructions go to the first of `.rules`, `.cursorrules`, `.windsurfrules`, `.clinerules`, `.github/copilot-instructions.md`, `AGENT.md`, `AGENTS.md`, `CLAUDE.md` and `GEMINI.md` that exists, since Zed reads only that one, and to `AGENTS.md` (shared with Codex, Cursor, Antigravity, opencode, Grok Build and Windsurf) when none does; `.agents/skills/<name>/` (skills; Amp writes its project skills to the same folder) |
| Hermes Agent | `$HERMES_HOME/config.yaml` (`mcp_servers`, `agent.personalities` and `display.personality`, default `~/.hermes`), `$HERMES_HOME/SOUL.md` (Hermes's identity file, the only global file it reads for instructions; capture reads only a kitbag-managed block in it), `$HERMES_HOME/skills/<name>/` (skills; capture also looks in category folders such as `skills/devops/<name>/`, and skips the skills Hermes bundled or installed from its hub) | `.hermes.md` or `HERMES.md` when the project has one (Hermes loads it before `AGENTS.md`), otherwise `AGENTS.md` (shared with Codex, Cursor, Antigravity, opencode, Windsurf and Grok Build); Hermes documents no project MCP file, so MCP servers are not written at project scope; `.hermes/skills/<name>/` (skills; Hermes loads them once you run `hermes skills trust`, and also reads `.agents/skills/`, which kitbag leaves to the tools that own it) |
| Amp | `~/.config/amp/settings.json` or `settings.jsonc` (`amp.mcpServers`; `%USERPROFILE%\.config\amp` on Windows), `~/.config/amp/AGENTS.md`, `~/.config/agents/skills/<name>/` | `.amp/settings.json` or `settings.jsonc` (`amp.mcpServers`; Amp asks you to approve these servers, `amp mcp approve`), `AGENTS.md` (shared with Codex, Cursor, Antigravity, opencode, Windsurf, Grok Build and Hermes), or `AGENT.md` / `CLAUDE.md` when that is the file Amp reads, `.agents/skills/<name>/` |
| Cline | `~/.cline/data/settings/cline_mcp_settings.json` (`mcpServers`; `CLINE_MCP_SETTINGS_PATH` names the file, `CLINE_DATA_DIR` replaces `~/.cline/data`, `CLINE_DIR` replaces `~/.cline`), `~/.cline/rules/kitbag.md` (instructions: a managed block in a file of its own; Cline reads every file in that directory), `~/.cline/rules/<name>.md`, `~/.cline/workflows/<name>.md`, `~/.cline/skills/<name>/`; capture also reads `~/Documents/Cline/Rules/`, `~/Cline/Rules/`, `~/Documents/Cline/Workflows/` and `~/.agents/skills/` | `AGENTS.md` (shared with Codex, Cursor, Antigravity, opencode, Grok Build, Windsurf and Zed), `.cline/rules/<name>.md`, `.cline/workflows/<name>.md`, `.cline/skills/<name>/`; capture also reads the older `.clinerules/` (rules, `workflows/`, `skills/`) and `.agents/skills/`; Cline's docs describe no project MCP file, so MCP servers are not written at project scope |
| Roo Code | `mcp_settings.json` in `<editor user data>/User/globalStorage/rooveterinaryinc.roo-cline/settings/` (`mcpServers`; the editor is VS Code, else VS Code Insiders, VSCodium or Cursor, whichever has the extension's folder: `~/.config/Code` or `$XDG_CONFIG_HOME/Code` on Linux, `~/Library/Application Support/Code` on macOS, `%APPDATA%\Code` on Windows), `~/.roo/rules/kitbag.md` (instructions: a managed block in a file of its own; Roo reads every file in that directory), `~/.roo/rules/<name>.md`, `~/.roo/commands/<name>.md`, `~/.roo/skills/<name>/`; capture also reads `~/.agents/skills/` | `.roo/mcp.json` (`mcpServers`; wins over the user file for a server of the same name), `AGENTS.md` (shared with Codex, Cursor, Antigravity, opencode, Grok Build, Windsurf and Zed), `.roo/rules/<name>.md`, `.roo/commands/<name>.md`, `.roo/skills/<name>/`; capture also reads `.agents/skills/` |
| Continue | `~/.continue/config.yaml` (`%USERPROFILE%\.continue\config.yaml` on Windows): `mcpServers`, for instructions a `kitbag-instructions` entry of `rules`, and for commands the `prompts` list | `.continue/mcpServers/*.yaml` (a server already defined in one of those files is edited there, new ones go to `kitbag.yaml`; JSON files there are read, not written), `.continue/rules/<name>.md`, and the instructions in `.continue/rules/kitbag-instructions.md`; `AGENTS.md` is not used, since Continue's docs don't mention it |
| GitHub Copilot | `$COPILOT_HOME/mcp-config.json` (`mcpServers`, default `~/.copilot`; plugins, by reference: `enabledPlugins` and `extraKnownMarketplaces` in `$COPILOT_HOME/settings.json`), `$COPILOT_HOME/copilot-instructions.md`, `$COPILOT_HOME/instructions/*.instructions.md`, `$COPILOT_HOME/skills/` (commands too, as skills), `$COPILOT_HOME/agents/*.agent.md` | `.mcp.json` (shared with Claude Code, written in Claude Code's shape), `.github/copilot-instructions.md`, `.github/instructions/*.instructions.md` (`applyTo`), `.github/prompts/*.prompt.md`, `.github/skills/`, `.github/agents/*.agent.md`; capture also reads `.github/mcp.json`, legacy chat modes in `.github/chatmodes/` and `.vscode/mcp.json` (`servers`), and `disabledMcpServers` from Copilot's `settings.json`; plugins, by reference: `enabledPlugins` and `extraKnownMarketplaces` in `.github/copilot/settings.json` |

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
| Amp | `${VAR}` (expanded by Amp in `amp.mcpServers`; defaults resolved at apply time); SSE servers are written as a plain `url`, since Amp has no transport key | no |
| Cline | resolved value (its docs describe no interpolation); remote servers get `type: streamableHttp` or `sse` | yes, unless `--secrets never` |
| Roo Code | `${env:VAR}` (expanded by Roo Code; defaults resolved at apply time); remote servers get `type: streamable-http` or `sse` | no |
| Continue | `${{ secrets.VAR }}` in `args` and `env`, Continue's own syntax, which it fills from its locally stored secrets (kitbag never reads or writes them); `command`, `cwd`, `url` and headers have no documented syntax, so a reference there is resolved at apply time (file written 0600; `--secrets never` keeps the placeholder); SSE servers get `type: sse`, other remote ones `type: streamable-http` | no, in `args` and `env`; yes elsewhere, unless `--secrets never` |
| Grok Build | `${VAR}` / `${VAR:-default}` (expanded by Grok Build); an `Authorization: Bearer ${VAR}` header is written as `bearer_token_env_var = "VAR"` and read back the same way; SSE servers get `type = "sse"`, and capture reads a server named in `disabled_mcp_servers` as disabled | no |

A resolved value comes from your environment, or, with `apply --secrets-from`, from the
encrypted bundle for the variables it holds (see item 6 of
[Secrets: the security model](#secrets-the-security-model)); the rows that read references
never get a value from the bundle.

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
├── output-styles/
│   └── terse.md           # output styles (Claude Code; Hermes personalities)
├── hooks/
│   └── typecheck.sh       # scripts the [[hooks]] of kitbag.toml run (optional)
├── memory/
│   └── claude-code/…      # memory notes, only with `capture --include memory` (optional)
└── secrets.age            # encrypted secret values, only with `capture --include secrets` (optional, gitignored unless you run `kitbag secrets commit`)
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
# disabled_on = ["cursor"]    # or every tool except these (also for rules, commands, skills, agents, output styles)
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

**Config schemas: `[targets.<id>]`.** A tool's own config format can change under a profile
(opencode 2 moved its MCP servers from `mcp.<name>` to `mcp.servers.<name>`), so capture records
which format a tool's files were in, for the tools where the files say so:

```toml
[targets.opencode]
schema = "v2"        # opencode: "v1" (mcp.<name>, `enabled`) or "v2" (mcp.servers.<name>, `disabled`)

[targets.continue]
schema = "v1"        # Continue: the `schema:` of ~/.continue/config.yaml (v1 is the only one its docs give)
```

Only those two tools declare schemas today; for the rest nothing is recorded, and a profile
without `[targets]` applies exactly as before. A tool with no servers or other content in the
profile, or whose files don't show a schema (an opencode config with no MCP servers), records
none. Nothing is run to find out: kitbag reads the config file it reads anyway. The tool's
version number is not recorded.

What `apply` does with it, per tool, checked against the tool's config on this machine:

| Profile says | The tool's config uses | `apply` |
|---|---|---|
| nothing, or the same | anything kitbag writes | as before |
| a schema kitbag knows | another one it knows | writes the profile in the tool's schema, and says so (opencode: `v1` ↔ `v2`; a server is the same in a profile either way, and `${VAR}` references are kept as they are, never resolved) |
| a schema kitbag doesn't know (a newer kitbag's, or a typo) | anything | **refuses the whole apply**, with nothing written, also for a git source and with `--trust-source` |
| anything | a schema kitbag can't write (`schema: v2` in Continue's `config.yaml`) | skips that tool with a warning and leaves its files alone; the other tools are applied |

`kitbag doctor` shows the schema of each tool it finds, and with a profile lists a mismatch
(`opencode: captured from schema 'v2', the installed tool uses 'v1'; apply writes 'v1'`) or a
claim it would refuse; the refusal is a problem (exit 1). The format number stays `kitbag = 1`:
the section is an addition, but kitbag 0.1.10 and older reject `[targets]` as an unknown key
("unknown field"), so a profile captured from opencode or Continue with this kitbag does not
load there. There is no `kitbag migrate`: the profile holds servers in kitbag's own shape, so
rewriting the label would change nothing a tool reads. Recapture to update the recorded schema.
Schemas for the other targets, and a tool version number, are dated skips (2026-11-27): official
docs don't name a kitbag-writable schema or version field for them (Gemini CLI's flat-to-nested
settings move still has no mapping).

**Per-tool MCP servers.** One shared profile can keep a heavy server out of the tools that
don't need it. `targets` lists the only tools a server is applied to; `disabled_on` lists the
tools it is kept out of. Use one or the other, not both, and give `targets` at least one tool.
Target names are those of `kitbag list-targets` (`claude-code`, `cursor`, `codex`,
`gemini-cli`, `claude-desktop`, `antigravity`, `opencode`, `copilot`, `grok-build`, `windsurf`, `zed`, `hermes`, `amp`, `cline`, `roo`, `continue`). An unknown name (a typo, or a tool from a newer
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
apply turns it off with the tool's own switch where the tool has one (table below), keeping the
entry, and removes it otherwise. Either way it makes a backup, so `kitbag undo` puts it back.
It only does so when kitbag's own records (`applied-servers.json` in the state directory) show that an apply of
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
is an addition like any other: the switch is cleared. `capture --force` into an existing profile keeps the `targets`
and `disabled_on` of the servers it captures again, and keeps the profile's servers it found in
no tool (disabled everywhere, or for a tool not installed on this machine), with a note naming
them; `--prune` drops those instead.

The switch each tool has, from its own documentation (the entry stays and is marked, the
plan lists it under "MCP servers turned off", and `status` reads it as in sync):

| Tool | Where the server is turned off |
| --- | --- |
| Codex, Grok Build | `enabled = false` on the `[mcp_servers.<name>]` table |
| opencode | `disabled: true` on the entry (opencode 2); `enabled: false` in opencode 1's layout |
| Cline, Roo Code, Antigravity | `disabled: true` on the entry |
| Zed, Hermes Agent | `enabled: false` on the entry |
| Claude Code, in a project | the name in `disabledMcpjsonServers` of `.claude/settings.local.json` ("Reject specific servers from a project's `.mcp.json`"); the entry stays in `.mcp.json` |
| Gemini CLI, user scope | the name in `mcp.excluded` of `~/.gemini/settings.json`; Gemini's docs list no `disabled` key on a server entry |
| GitHub Copilot CLI, user scope | the name in `disabledMcpServers` of `$COPILOT_HOME/settings.json` ("configured but not started") |

A name kitbag puts in one of those lists is taken out again when the server is enabled, and only
if kitbag's record says it put it there: a name you listed yourself stays. Capture reads the
same switches back, so a server off in some tools and on in others comes back with a
`disabled_on` naming those tools, and one that every tool that has it keeps off with
`disabled = true`. The other tools have no documented switch kitbag can write, so the entry is
removed as before: Claude Code at user scope (its `/mcp` toggle is kept per project in
`~/.claude.json`), Gemini CLI and Copilot at project scope (the settings file there is the one a
team shares), Cursor, Claude Desktop, Windsurf, Continue and Amp. Gemini CLI also keeps its own
state in `~/.gemini/mcp-server-enablement.json`, whose format its docs don't give, so kitbag
doesn't write it.

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

The item is an MCP server, rule, command, skill, agent or output style; a name that is more than one of them
needs `mcp:`, `rule:`, `command:`, `skill:`, `agent:` or `output-style:` in front, and an unknown name or tool is
an error (`claude` is read as `claude-code`). Without `--on` an item is turned off on every
tool, written as a `disabled_on` naming each one (a tool added by a later kitbag is not in it);
enabling clears the list. Comments and everything else in the file stay as they are, `only` /
`except` are rewritten as `targets` / `disabled_on`, and the result is checked before the file is
replaced. Hooks have no per-tool setting and are not handled. When you run `kitbag apply`
afterwards, a server you turned off for a tool that has its own switch is kept there and
switched off (see the table above) instead of removed; enabling it again switches it back on.
Rules, commands, skills, agents and output styles have no documented switch, so their files are removed.

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
| `hermes` | `display`, of which only `personality` is written |
| `cursor` | the Cursor Agent CLI's `~/.cursor/cli-config.json`, user scope only: `approvalMode`, `permissions` (`allow`, `deny`), `editor` (`vimMode`), `notifications`, `hints`, `rewind`, `suggestNextPrompt`, `display`, `attribution` |
| `copilot` | Copilot CLI's `~/.copilot/settings.json` (`$COPILOT_HOME`), user scope only: `model`, `effortLevel`, `defaultMode`, `defaultPermissionMode`, `allowedUrls`, `deniedUrls`, `permissions` (only `disableBypassPermissionsMode`), `theme`, `banner`, `beep`, `beepOnSchedule`, `editorMode`, `includeCoAuthoredBy`, `renderMarkdown`, `showReasoning`, `showTimestamps`, `showTipsOnStartup`, `transcriptView`, `screenReader`, `scrollbar`, `sidebar`, `streamerMode` |
| `opencode` | `opencode.json`: `model`, `default_agent`, `permissions` (opencode 2's list of `{action, resource, effect}` rules). Also opencode 2's global `cli.json` (`~/.config/opencode/cli.json`, `$XDG_CONFIG_HOME/opencode/cli.json`), user scope only, in the same `[settings.opencode]` table (the names don't collide; each key goes to its own file): `theme` (`name`, `mode`), `animations`, `cursor`, `mouse`, `scroll`, `prompt`, `session`, `tabs`, `diffs`, `attention` (not `sounds`), `highlights`, `terminal`, `mini`, each cut down to the documented keys with values checked against the documented values and ranges |
| `zed` | `agent`, user scope only, of which only `default_model`, `default_profile`, `tool_permissions`, `single_file_review`, `notify_when_agent_waiting`, `play_sound_when_agent_done`, `expand_edit_card`, `expand_terminal_card`, `thinking_display`, `use_modifier_to_send` and `show_turn_stats` are kept |
| `amp` | user scope only: `amp.showCosts`, `amp.notifications.enabled`, `amp.terminal.copyOnSelect`, `amp.terminal.detailsExpandedByDefault`, `amp.thread.autoArchiveOnQuit`, `amp.git.commit.ampThread.enabled`, `amp.git.commit.coauthor.enabled`, `amp.tools.disable` |
| every other target | none yet |

The new rows follow each tool's own docs (checked 2026-11-27): Cursor's CLI configuration and
permissions pages, Copilot CLI's configuration-directory reference, opencode's config and
permissions pages, Zed's all-settings and tool-permissions pages, and Amp's settings page. Where
the docs give a key a shape or values kitbag can't check (Cursor's `model` object, `sandbox`;
Amp's `amp.updates.mode`) or the key reaches a provider, proxy, hook or command
(`apiKeyHelper`-style keys, `amp.url`, `amp.proxy`, Copilot's `proxyUrl`, `statusLine.command`,
`hooks`, `sandbox`, Zed's `terminal_init_command`, `language_models` and `profiles`), it is not
carried. Gaps, on purpose: Cursor's docs name no `settings.json` path or agent keys for the
*editor*, and the editor keeps some settings in a `state.vscdb` database with no documented
layout, so kitbag never reads or writes either. opencode's theme and terminal settings are in
opencode 2's `cli.json` is carried at user scope (there is no project file), without `plugins`, `keybinds`, `leader`, `experimental`, `debug` and `attention.sounds` (file paths); other keys already in the file are left alone, and a file kitbag creates gets `$schema` `https://opencode.ai/v2/cli.json`. `session.permissions = autoaccept` accepts every permission request, so it is warned about and withheld from a git source unless `--allow-risky-settings` or `--trust-source` (docs read 2026-11-28). Amp's docs name no model setting. Copilot's, Zed's
and Amp's project-level files are not written, since their docs don't say these keys apply
there. Cursor's CLI file is created with `"version": 1` when it doesn't exist (its docs say the
CLI repairs missing fields) and follows `CURSOR_CONFIG_DIR`, then `XDG_CONFIG_HOME/cursor` on
Linux, as those docs describe. For opencode, rules from a profile go in front of the rules the
file already has, because the last matching rule wins and your own rules should keep the last
word; a file in opencode 1's layout (a `permission` object) gets no `permissions`.

`env` blocks, `apiKeyHelper`, provider endpoints and similar keys are never carried. Hooks
are not a setting: they have their own opt-in path (see [Hooks](#hooks-hooks-in-kitbagtoml-and-hooks)).
Anything that lets agents act without asking gets an explicit warning in the plan. That
covers `permissions.defaultMode = bypassPermissions` (and `acceptEdits`, `dontAsk`, `auto`),
`permissions.additionalDirectories`, allow rules (broad ones such as `Bash(*)` or
`Bash(rm -rf *)` are named), `approval_policy = never` and `sandbox_mode = danger-full-access`.
For the newer targets it also covers Cursor's `approvalMode = unrestricted` (or `auto-review`) and
`permissions.allow` entries, Copilot's `defaultPermissionMode = assisted` or `allow-all`,
`defaultMode = autopilot` and `allowedUrls`, opencode's `allow` rules, and Zed's
`tool_permissions` that allow (`default = "allow"`, or any `always_allow` pattern).
It also covers commands and skills whose frontmatter pre-approves tools (`allowed-tools`) or
defines `hooks`, and commands and skills that run shell snippets (``!`cmd` `` in Claude Code,
`!{cmd}` in Gemini CLI commands), and custom agents and subagents that do any of these, start
MCP servers of their own (`mcp-servers`, `mcpServers`) or set a `permissionMode` that never
asks (`acceptEdits`, `auto`, `dontAsk`, `bypassPermissions`).

**From a git source, the riskiest settings are dropped** unless you pass
`--allow-risky-settings` (or its alias `--trust-source`): permissive `defaultMode`s,
`additionalDirectories`, every `permissions.allow` rule, `approval_policy = never` and
`sandbox_mode = danger-full-access`, and the same newer ones: Cursor's `approvalMode` and
`permissions.allow`, Copilot's `defaultPermissionMode`, `defaultMode = autopilot` and
`allowedUrls`, opencode's `allow` rules and Zed's allowing `tool_permissions`. `ask` and `deny`
rules still apply, since they restrict.
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

### MCP OAuth: `[mcp_servers.<name>.oauth]`

A remote server that needs a pre-registered OAuth client can carry the client's *configuration*,
so the server is ready to sign in on the tool you apply to:

```toml
[mcp_servers.linear]
url = "https://mcp.example.com/mcp"

[mcp_servers.linear.oauth]
client_id = "your-client-id"
client_secret = "${LINEAR_CLIENT_SECRET}"   # an env reference only, never the value
scopes = ["read", "write"]
callback_port = 8080
```

Other keys: `enabled` (`false` for a server that doesn't use OAuth, such as one with a header
credential; `true` marks one that does), `redirect_uri`, `auth_server_metadata_url`,
`authorization_url`, `token_url`, `issuer` and `resource`. Each is written under the tool's own
name and only where the tool's official docs name a key (read 2026-11-27):

| Tool | Written | Sign in with |
|---|---|---|
| `claude-code` | `oauth.clientId`, `callbackPort`, `scopes` (one space-separated string), `authServerMetadataUrl`. Its docs keep the client secret in the keychain or a credentials file, "not in your config", so none is written | `/mcp`, or `claude mcp login <name>` |
| `gemini-cli` | `oauth.enabled`, `clientId`, `clientSecret`, `authorizationUrl`, `issuer`, `tokenUrl`, `scopes`, `redirectUri` (`tokenParamName`, `audiences` and the other keys on the entry are left as they are) | `/mcp auth <name>` |
| `opencode` | `oauth.client_id`, `client_secret` (as `{env:NAME}`), `scope` (space-separated), `callback_port`, `redirect_uri`, `auth_server_metadata_url`, or `oauth: false` | `opencode mcp auth <name>` (or `/mcps`) |
| `codex` | `oauth.client_id`, `oauth.callback_port`, `scopes`, `oauth_resource`. `oauth.client_secret` is "stored in configuration" as plain text and no env reference is documented, so none is written | `codex mcp login <name>` |
| `cursor` | `auth.CLIENT_ID`, `auth.CLIENT_SECRET` (as `${env:NAME}`); the docs don't say what type `scopes` has, so they are not written | none named in the docs |
| `hermes` | `auth: oauth`, `oauth.client_id`, `client_secret` (`${VAR}`), `redirect_uri`; its `redirect_host`, `redirect_port`, `timeout` and `flow` are left as they are | `hermes mcp login <name>` |

Capture reads the same keys from each tool's own server entry, and only there: no keychain, no
token file, no credentials store is opened. Two tools that each name some of a server's OAuth
settings make one server with both sets.

**What is never carried in the profile.** Access tokens, refresh tokens, expiry times, and the
files and keychain items that hold them are never in `kitbag.toml`, never in git, never printed,
and kitbag never starts a sign-in. Only Gemini CLI's documented token file can travel, encrypted
and opt-in (see [MCP OAuth tokens in the secrets bundle](#mcp-oauth-tokens-in-the-secrets-bundle));
the others (`$CODEX_HOME/.credentials.json`, Hermes's `auth.json` and `mcp-tokens/`, opencode's
database, Amp's and Claude Code's stored sign-ins, any keychain) are not read. A client secret is a
credential: it is carried only as an env reference (`${VAR}`, or the tool's own `{env:VAR}` /
`${env:VAR}` spelling, which capture turns back into `${VAR}`). A literal secret found in a
tool's config is left out of the profile with a warning that says so (never what it was), and a
literal in a `kitbag.toml` is refused. Where the tool has no documented place for the secret, it
isn't written there at all, and the plan says so.

**A different URL, a different server.** OAuth settings are written to an entry only if it is new
or still connects to the same URL. An entry that now points somewhere else loses the old
entry's OAuth settings (as before) and does not get the profile's either; the plan warns, and you
sign in again.

**Sign-in checklist.** After the plan and again after an apply, kitbag lists the servers that
still need a sign-in on each tool, with that tool's own command: servers whose profile has
OAuth settings (or `auth: oauth` from Hermes), and, on opencode, remote servers with no header
credentials, which its docs say use OAuth unless `oauth` is `false`. Local servers and
`oauth.enabled = false` servers are never listed, and neither is every HTTP server. Signing in
is yours to do; across tools, sign in again.

**Documented skips (2026-11-27).** GitHub Copilot's MCP docs don't name `oauthClientId`,
`oauthScopes`, `oauthPublicClient` or `oauthGrantType`, so those stay unread, as before. Zed
documents an in-app sign-in prompt and no keys. Amp's `amp.mcpServers` has no `oauth` keys (its
docs show only `amp mcp oauth login` flags, whose use needs `--server-url` and more), so its
existing `oauth` object is kept while the URL is unchanged, as before, and nothing is captured
or listed. Claude Desktop's docs name none. A *literal* `client_secret` that capture leaves out
of the profile is carried in `secrets.age` when you pass `--include secrets` (see item 6 of
[Secrets: the security model](#secrets-the-security-model)).

#### MCP OAuth tokens in the secrets bundle

A signed-in Gemini CLI session can move to another machine with the same opt-in flags as other
secrets: `kitbag capture --include secrets` and `kitbag apply --secrets-from`. There is no other
flag, and a bare capture never reads a token file.

- **What.** Gemini CLI's MCP docs say tokens are "stored securely in
  `~/.gemini/mcp-oauth-tokens.json`", refreshed when expired and validated before each
  connection (read 2026-11-27). The docs give the path, not the layout, so kitbag carries the
  file's bytes as they are, inside `secrets.age` only, tagged with the tool (`gemini-cli`) and the
  names and URLs of the remote servers in Gemini CLI's config at capture. It never parses, merges,
  validates or refreshes the tokens; Gemini CLI does that. Capture reads it only when you capture
  from `gemini-cli`.
- **Same tool only.** On apply the store is written only to `~/.gemini/mcp-oauth-tokens.json`,
  and only when `gemini-cli` is among the targets. `--to cursor` (or any other tool) never writes
  it and never copies it into another tool's files: sign in there.
- **A changed URL means a new sign-in.** If a server recorded with the store now has another
  URL, in the profile or in Gemini CLI's own `settings.json`, the store is not written; the plan
  says which server moved and lists `/mcp auth <name>` for its servers.
- **No clobbering.** The file is written only if it is missing or empty, `0600` on Unix (on
  Windows it keeps the access of the folder it is in, like `secrets.age`). An existing, non-empty
  token file is left as it is, since no documented layout says how to merge one; the plan says so
  and lists the servers to sign in to.
- **Not in undo, plan, diff or logs.** The file is written after the other files and outside
  the undo transaction, so no token is in the plan, the diff, the output or the state directory.
  The plan names only the tool and the path. **`kitbag undo` does not remove or revert it:** sign
  out in Gemini CLI, or delete the file. `kitbag diff` opens the bundle only with
  `--secrets-from` and never writes the token file; `kitbag inspect` never opens it.
- **`kitbag secrets list`** names the store (`gemini-cli MCP OAuth tokens
  (~/.gemini/mcp-oauth-tokens.json)`), never its content.
- **Never from git; on a `--remote` host only from a bundle opened there.** As for every bundle,
  `--secrets-from` is refused for a git source and with `--remote`; with `--remote-secrets-from`
  the host's kitbag opens a bundle on the host and writes the token file there (see
  [Applying on a remote host over SSH](#applying-on-a-remote-host-over-ssh)).
- A bundle with a token store is format 2; one without is still written as format 1, and both
  open.

**Skipped on purpose (official docs checked 2026-11-27); sign in again with the checklist:**
Codex's config reference names `mcp_oauth_credentials_store = auto | file | keyring` but no
file path or layout, so its store isn't read (`codex mcp login <name>`). Claude Code's MCP docs
say tokens are "stored securely and refreshed automatically" and name no path; its
`~/.claude/.credentials.json` holds *login* credentials and is never touched (`/mcp` or `claude
mcp login <name>`). opencode keeps credentials in a SQLite database its docs say not to edit
(`opencode mcp auth <name>`). Amp keeps sign-ins on ampcode.com. Cursor, Hermes, Zed, Copilot
and Claude Desktop name no local token file. No OS keychain is read. There is no official
merge schema for an existing Gemini token file, and kitbag does not refresh or validate tokens
(Gemini's docs say it does both). That is the rest of #18.

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
Codex gets a custom prompt, and Cursor gets a plain Markdown command. Amp, Antigravity and
Copilot (at user scope) have no commands folder kitbag can write, and their docs migrate
commands into skills, so a command is written as a skill there (`skills/<name>/SKILL.md` with
`name`, `description` and the prompt as the body; Copilot's also carries `argument-hint` and
`disable-model-invocation: true`, as VS Code's prompt-file migration does, so only `/<name>`
runs it). A skill has no arguments template, so the plan notes what is lost (`$ARGUMENTS`,
other frontmatter), and a command named like a skill of the profile is skipped with a warning. Other frontmatter keys,
such as Claude Code's `allowed-tools`, are preserved and passed only to Claude Code.

### Skills: `skills/<name>/`

Standard [Agent Skills](https://agentskills.io): a directory with `SKILL.md` (frontmatter
`name` and `description`) and any scripts or references. Skills are copied as-is, and
`name`/`description` are added for tools that require them.

### Agents: `agents/<name>.md`

Custom agents, carried by the `claude-code`, `copilot`, `opencode`, `cursor`, `gemini-cli`,
`codex` and `antigravity` targets: one agent per file, named after the file. `description` is a field of its own and every other frontmatter key is kept
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

**Cursor**, **Gemini CLI**, **Antigravity** and **Codex** each get the file their own docs
describe, and only the keys those docs list; a key a tool doesn't list is left out and the plan
names it. Values are never translated, so `model` and `tools` are written as they are (with a
note) when the tool lists the key and the value has the type its docs give.

| Target | Where | Keys written (besides the body) |
|---|---|---|
| `cursor` | `~/.cursor/agents/<name>.md`, `.cursor/agents/<name>.md` ([docs](https://cursor.com/docs/subagents)) | `name` (only when the agent has one), `description`, `model`, `readonly`, `is_background` |
| `gemini-cli` | `~/.gemini/agents/<name>.md`, `.gemini/agents/<name>.md` ([docs](https://geminicli.com/docs/core/subagents/)) | `name` (a slug: lowercase letters, numbers, `-`, `_`; else the profile name is used), `description` (required), `kind: local`, `tools` (a list), `mcpServers` (a map), `model`, `temperature`, `max_turns`, `timeout_mins` |
| `antigravity` | `~/.gemini/config/agents/<name>.md`, `.agents/agents/<name>.md` ([docs](https://antigravity.google/docs/subagents)); capture also reads `<name>/agent.md` | `name`, `description` (required), `tools` (a list), `mainAgent`, `subagent`, `model` (only `inherit`, `flash` or `pro`), `commandExecutionPolicy` (`off`, `auto`, `eager` or `sandbox`), `mcpServers`, `skills`, `plugins` |
| `codex` | `$CODEX_HOME/agents/<name>.toml`, `.codex/agents/<name>.toml` ([docs](https://developers.openai.com/codex/subagents)) | `name`, `description` (required), `developer_instructions` (the body, required), `model`, `model_reasoning_effort`, `model_reasoning_summary`, `model_verbosity`, `[features]`, `[skills]` |

Codex's agents are TOML, not markdown: capture turns `developer_instructions` into the body, and
a `name` that differs from the file name is kept as a `name` key, since Codex identifies an agent
by that key. An agent that lacks what a tool requires (a description, or for Codex instructions)
is skipped with a warning, not given invented text. Antigravity warns that a `tools` entry it
doesn't know may make the subagent hang, so the plan repeats that whenever `tools` is written.
Cursor also reads `~/.claude/agents/` and `.claude/agents/`; with both tools targeted an agent
lands in both places, and Cursor's own folder is the one kitbag captures for Cursor.

**Targets without agent files.** Re-checked against each tool's official docs on 2026-11-27;
none of these gets agents, and the plan says agents were skipped there:

| Target | What the docs say today | Why it is skipped |
|---|---|---|
| `cline` | The [customization overview](https://docs.cline.bot/customization/overview) lists `agents/` ("Global agent definitions" under `~/.cline/`, "Project agent definitions" under `.cline/`) and nothing else; the [subagents](https://docs.cline.bot/features/subagents), [agent teams](https://docs.cline.bot/cli/agent-teams) and SDK agent pages describe no definition file | folder named, file format and frontmatter not documented |
| `grok-build` | [docs.x.ai subagents](https://docs.x.ai/build/features/subagents): "Add or override types under `.grok/agents/` or `~/.grok/agents/`"; the settings reference only has `[subagents]` switches and per-type model routing | folder named, no format on docs.x.ai (the format appears only in the `xai-org/grok-build` repo guide, which kitbag does not treat as official) |
| `amp` | [Modes & Models](https://ampcode.com/docs/models-and-subagents): "Plugins can define custom agents and custom subagents" | custom agents are plugins, not files (#12) |
| `continue` | `.continue/agents/*.md` are the local agents `cn check --agent` and CI runs ("Local agents — markdown files in `.continue/agents/*.md`", each one a check that reviews your diff); the docs never call them subagents | check files, not subagents |
| `roo` | [Custom modes](https://roocodeinc.github.io/Roo-Code/features/custom-modes): `custom_modes.yaml` and `.roomodes`; "agent" and "subagent" do not appear | modes are not documented as agents |
| `zed` | [Agent profiles](https://zed.dev/docs/ai/agent-profiles): "Profiles are stored under `agent.profiles` in your settings"; subagents only get `agent.subagent_model` | no agent files |
| `windsurf` | The [Cascade docs](https://docs.devin.ai/desktop/cascade/cascade) cover workflows and skills, no agent definitions | no agent files |
| `hermes` | [Delegation](https://hermes-agent.nousresearch.com/docs/user-guide/features/delegation): subagents are set in the `delegation:` block of `config.yaml` and per `delegate_task` call | no agent files |
| `claude-desktop` | no official Claude Desktop page documents agent files; subagent files are documented for Claude Code (`~/.claude/agents/`) only | no agent files |

Kitbag writes no Devin folders (`.devin/`, `~/.config/devin/`). If one of these tools later
documents an agent file with a format, it can be added with the same helpers as the four above.

Apply is additive and undo removes only the files kitbag wrote. From a git source, an agent with
`hooks`, `allowed-tools`, `mcp-servers` or `mcpServers` (servers it starts itself), a
`permissionMode` that acts without asking (`acceptEdits`, `auto`, `dontAsk`,
`bypassPermissions`), an Antigravity `commandExecutionPolicy` of `auto` or `eager`, or a shell
snippet is skipped unless you pass `--trust-source`. A plain
`tools` list only narrows what an agent may use, so it doesn't count. The plan warns about
such agents in a local profile.

### Output styles: `output-styles/<name>.md`

Output styles set Claude's role, tone and response format for a whole session. **Claude Code**
has them as a file, and **Hermes Agent** has the comparable personalities (see below); no other
target is written. Claude Code's
[output styles page](https://code.claude.com/docs/en/output-styles) (checked 2026-11-27) says
a custom style is "a Markdown file: frontmatter for metadata, then the instructions for
Claude", saved at one of three levels: user `~/.claude/output-styles`, project
`.claude/output-styles`, and managed policy (`.claude/output-styles` inside the managed
settings directory, which kitbag never writes). The file name is the style's name unless `name`
is set in the frontmatter, and all frontmatter fields are optional: `name`, `description`,
`keep-coding-instructions` (default `false`) and `force-for-plugin` (plugin styles only).

```markdown
---
name: Diagrams first
description: Lead every explanation with a diagram
keep-coding-instructions: true
---

When explaining code, start with a Mermaid diagram, then explain in prose.
```

Capture reads `*.md` files in the folder itself (the docs name no subfolders, so a nested
file is skipped with a note), skips a `README.md`, and keeps `description` and every other
frontmatter key as it is. A file name with characters kitbag's names don't allow (a space) is
flattened (`My style.md` becomes `My-style.md`) and gets `name: My style` so Claude Code still
calls the style what it did. Apply writes `<name>.md` to `~/.claude/output-styles/`
(`$CLAUDE_CONFIG_DIR/output-styles/`) or the project's `.claude/output-styles/`.

The `outputStyle` setting (`~/.claude/settings.json` or the project's `.claude/settings.json`)
is carried in `[settings.claude-code]` like the other settings, whatever it names: a
built-in style (`Proactive`, `Concise`, `Explanatory`, `Learning`, `default`), a style in the
profile, or one kitbag has no file for (a plugin's or a managed one), which is kept with a
note, since Claude Code falls back to its default style when it can't find the name. The
value is case-sensitive in Claude Code.

`/output-style` and the `/config` menu save the choice somewhere else: the project's
`.claude/settings.local.json` ([output styles](https://code.claude.com/docs/en/output-styles) and
[settings](https://code.claude.com/docs/en/settings), checked 2026-11-27). kitbag does not read or write
the choice there, because the settings page defines that file as the "Project local" scope,
"You, in this one project only", for "Personal overrides for one project", which Claude Code
keeps out of git itself (it adds `**/.claude/settings.local.json` to your global git excludes
the first time it writes the file; one made by hand needs your own `.gitignore` entry), and
places at the repository root, in a worktree at the main checkout's root, and beside
`settings.json` outside git, on Windows or when the root isn't yours. Carrying it would turn
one project's personal override into a default for every project (the profile's setting also
applies at user scope) or write a personal file into a place kitbag cannot tell, and kitbag does
not touch your git excludes. A capture of a project that has `outputStyle` there says so and
takes the value in `settings.json`; an apply never changes the local file, whose value still
wins over the one kitbag writes (local settings sit above shared project settings). To carry a
style you picked with `/output-style`, put the same name in `outputStyle` of `settings.json`.
Nothing else remains on #15: the local file is not carried, on purpose.
(The one other key kitbag writes in that file is `disabledMcpjsonServers`, the documented way to
reject a server of a project's `.mcp.json`.) The managed-policy folder and a plugin's
`output-styles/` are never read or written either: the first belongs to your administrator and the
second to the plugin.

Apply is additive and undo removes only the files and keys kitbag wrote. The style text is a
prompt sent with every request, so `kitbag diff` shows it in full, including for a git
source; capture redacts secrets in it and `kitbag doctor` flags any left in a profile. A
style has no way to run commands, so a git source's styles are not held back (review them
anyway: they steer everything Claude says). `targets` / `disabled_on` in the frontmatter work as
for the other items, and `kitbag disable output-style:<name>` sets them.

**Hermes Agent** (user scope only) has personalities: named pieces of prompt text that
`/personality <name>` adds to the system prompt. Hermes's
[personality page](https://hermes-agent.nousresearch.com/docs/user-guide/features/personality)
(checked 2026-11-27) says custom ones go in `config.yaml` under `agent.personalities`, each name
mapped to a string (its example writes it as a `>` block scalar), that "a top-level
`personalities:` block works too; `agent.personalities` wins if the same name appears in both",
and that the choice is "stored as a name in `display.personality`". Capture reads both blocks
(`agent.personalities` wins) into `output-styles/<name>.md`, the body being the personality's
text; a value that is not text, or a name that can't be a file name, is skipped with a note.
Apply writes `agent.personalities.<name>` in `config.yaml`, editing the file in place: your
other keys, comments and personalities stay, a multi-line text is written as a `|` block
scalar, and a name Hermes already has with other text is replaced unless `--no-overwrite`
(the same text, even in the top-level block, is up to date). A personality has no description
or other keys, so only the style's text is written, and the plan says when a description or
`keep-coding-instructions` was left out. Disabling a style for Hermes (`targets` /
`disabled_on`) takes out an entry kitbag added and leaves one you changed or added. A style
captured from Claude Code reaches Hermes, and a personality captured from Hermes reaches
Claude Code, as it does for any target that carries styles; use `targets` / `disabled_on` to
keep one on one tool. The picked name is carried as `[settings.hermes.display]` `personality`
(only that key of `display`; `none`, `default` and `neutral`, which mean no personality, are
not carried, and a name that is neither one of Hermes's fourteen built-ins nor in the file is kept
with a note), and written to `display.personality`. The text is prompt text, so `kitbag diff`
shows it in full, including for a git source, and capture redacts secrets in it. If
`agent`, `personalities` or `display` is written in a form kitbag can't edit in place (a flow
mapping), the plan says so and leaves the file alone.

`SOUL.md` is **not** a personality here: the docs call it Hermes's single identity file (the
first slot of the system prompt, a durable tone and identity for the whole install) and name no
named personalities in it. kitbag keeps writing the instructions in a managed block there and
reading only that block; the persona text around it is neither captured nor changed.

Not written, with the docs checked 2026-11-27: **Grok Build** names `.grok/personas/*.toml`,
`~/.grok/personas/*.toml` and a `[subagents.personas]` table, "behavioral overlays only (tone,
focus, contracts)" ([subagents](https://docs.x.ai/build/features/subagents)), but its pages
(subagents, settings and the settings reference) give no key or example for a persona, so
there is no format to write; **Codex**'s `personality` is documented as a "Deprecated
compatibility setting" that no longer changes the style (`friendly` / `pragmatic`), and
`features.personality` is ignored; **Gemini CLI**'s `GEMINI_SYSTEM_MD` replaces the whole
built-in system prompt (and `output.format` is `text` or `json`), not a style file; **Roo
Code**'s custom modes also set tool access, so they are agents, not styles. The other
targets' docs describe only ordinary rules or instructions files. No style text is folded
into instructions files behind a flag (the optional idea in the issue is not done).

### Hooks: `[[hooks]]` in `kitbag.toml`, and `hooks/`

Hooks are commands a tool runs by itself on an event (before a tool call, when a session
starts ...). That makes them as sensitive as an MCP server's command, so kitbag carries them
only when you ask, at every step. **Eight targets carry them**, each in the file its own docs
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
| `hermes` | `$HERMES_HOME/config.yaml` `hooks:` (default `~/.hermes/`), user scope only; shell hooks only | `agent-hooks/kitbag/` next to it | [hooks](https://hermes-agent.nousresearch.com/docs/user-guide/features/hooks) |
| `claude-desktop`, `opencode`, `windsurf`, `zed`, `amp`, `cline`, `roo`, `continue` | not carried: warn | none | see below |

**Why the last row is not carried** (official docs re-read 2026-11-27; the plan warns and names
the reason, nothing is dropped silently):

- **Claude Desktop**: no hook configuration of its own. Claude Code's
  [hooks docs](https://code.claude.com/docs/en/hooks) say the same hook events fire in the Desktop app, from
  Claude Code's `settings.json`, which the `claude-code` target carries.
- **opencode**: hooks exist only as [JavaScript/TypeScript plugins](https://opencode.ai/docs/plugins/);
  plugins are #12, not written here.
- **Amp**: event handlers (`amp.on(...)`) exist only in TypeScript/JavaScript
  [plugins](https://ampcode.com/docs/markdown/customize/plugins) (#12); the manual describes no
  hook file.
- **Cline**: its [hooks page](https://docs.cline.bot/customization/hooks) now says only "See details under
  SDK Plugins". Those are TypeScript code ([SDK plugins](https://docs.cline.bot/sdk/plugins.md): `beforeRun`,
  `beforeTool` ...), with no hook file, so nothing is written. A `.clinerules/hooks/` /
  `~/Documents/Cline/Hooks` script layout is described by third-party mirrors and search
  snippets, but not by the official page kitbag could read today, so it stays unwritten
  (capture still lists `.clinerules/hooks/` as left behind).
- **Windsurf**: its hooks page now redirects to Devin Desktop's docs, which name only
  `.devin/hooks.v1.json` ("in your project or at the user level", no path) and send you to Devin's
  hooks pages for events. Devin folders are parked in #20: kitbag writes no `.devin/`,
  `~/.devin/`, `~/.config/devin/` or `hooks.v1.json`, and no Windsurf hook file.
- **Zed**, **Roo Code**, **Continue**: their docs describe no hooks (Zed's AI docs and
  `llms.txt`, Roo's docs site map, Continue's config reference), so there is no file to write.

Capture reads a Grok Build or Copilot directory's every
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

**Hermes Agent** has four hook systems; kitbag carries only its
[shell hooks](https://hermes-agent.nousresearch.com/docs/user-guide/features/hooks), the `hooks:`
block of `config.yaml`, where each event (`pre_tool_call`, `post_tool_call`, `pre_llm_call`,
`on_session_start` ...) has a list of `{matcher, command, timeout, fail_closed}` entries. Hermes
documents no project hook file, so hooks are written at user scope only (at project scope the
plan says so and writes nothing); `HERMES_HOME` is honoured. The neutral events it has an
equivalent for are `session-start` (`on_session_start`), `prompt-submit` (`pre_llm_call`, which
Hermes's docs name as the counterpart of Claude Code's `UserPromptSubmit`), `pre-tool`,
`post-tool`, `subagent-start` and `subagent-stop`; `stop`, `post-tool-failure`, `notification`,
`permission-request` and `pre-compact` have none and are reported, and so are `session-end` and
`post-compact`, because Hermes's docs don't tie its `on_session_finalize` (teardown) or
`post_compaction` to them. Its own events are `hermes:<event>`: `hermes:on_session_finalize`,
`hermes:post_compaction`, `hermes:on_session_end` (which fires after every turn), or any other of its events, such as
`hermes:post_llm_call`. A matcher is written only for `pre_tool_call` and
`post_tool_call`, the events Hermes applies it to; its `fail_closed` (and Cursor's `failClosed`)
is kept. kitbag edits `config.yaml` in place: the entries of an event it adds to are rewritten
(comments inside that event's list go, and your own entries there may be re-quoted), and your other events, `hooks_auto_accept`, MCP servers
and comments stay; a layout it can't edit in place (for example `hooks:` written as a one-line
flow mapping) is reported and left alone. Scripts go to `~/.hermes/agent-hooks/kitbag/`, the
directory Hermes's docs suggest, and never to `~/.hermes/hooks/`, where Hermes loads gateway
hooks. **Hermes asks you to approve each new hook** (`hermes chat` prompts the first time; the
gateway, cron and CI need `--accept-hooks`, `HERMES_ACCEPT_HOOKS=1` or an entry in
`shell-hooks-allowlist.json`), and kitbag neither writes that file nor sets `hooks_auto_accept`,
so applied hooks wait for your approval (`hermes hooks list`). Not carried, and left as they
are: **outbound webhooks** (`hooks.outbound` in the same block, never overwritten or removed),
**gateway hooks** (`~/.hermes/hooks/<name>/HOOK.yaml` + `handler.py`) and **plugin hooks**
(`ctx.register_hook()`); capture warns when it finds any of them.

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

### Memory notes: `memory/` (opt-in)

What an agent has written down about you and your projects. **Off by default**: a bare
`kitbag capture` never reads it, and a profile only holds notes if you ran
`kitbag capture --include memory`. Notes often hold personal or client details, so capture
redacts secrets like the rest of a profile and lists every note it took (with the `type` and
`modified` its own frontmatter gives); read `memory/` before you commit or share the profile.
Memory is captured and applied at user scope only.

```text
memory/<tool>/<scope>/<file>.md
memory/claude-code/<project>/MEMORY.md, <topic>.md   <- ~/.claude/projects/<project>/memory/
memory/codex/user/memories_v2/…                      <- $CODEX_HOME/memories_v2/ (or memories/)
memory/hermes/default/MEMORY.md, USER.md             <- ~/.hermes/memories/
memory/hermes/profile-<name>/MEMORY.md, USER.md      <- ~/.hermes/profiles/<name>/memories/
```

Plain Markdown, no extra metadata: the tool and scope are in the path, and `type` / `modified`
stay in the note's own frontmatter. Docs re-read 2026-11-27 (`code.claude.com/docs/en/memory`,
`learn.chatgpt.com/docs/customization/memories`, `hermes-agent.nousresearch.com/docs/user-guide/features/memory`):

| Tool | Captured | Applied |
| --- | --- | --- |
| Claude Code | `MEMORY.md` and topic files of every `~/.claude/projects/<project>/memory/` | Into a `<project>` folder that already exists here with that name, only files that aren't there (a note that exists and differs is kept, never overwritten; an existing `MEMORY.md` is kept too, so added notes may not be listed in it). The docs say `<project>` is "derived from the git repository" but give no recipe and no portable key such as the remote URL, so notes are keyed by that folder name, a local key: they land only on a machine whose folder has the same name, and kitbag doesn't compute one. A custom `autoMemoryDirectory` (absolute or starting with `~/`, from the managed, project-local, project or user `settings.json`, highest precedence first; those files are read only to find it) is captured as `memory/claude-code/auto-memory-directory/` and applied into the directory the setting names on the target when that directory exists; a relative value is skipped with a note, a directory chosen by a repository's `.claude/settings.json` or `settings.local.json` is neither read nor written while `permissions.blockReadsOutsideWorkingDirectories` is on (Claude Code loads and saves no auto memory there), and kitbag never writes into the managed-policy folder. The project is the one kitbag is run in; `--settings` isn't seen |
| Codex | `*.md` under `$CODEX_HOME/memories/` and `memories_v2/`, as a read-only copy | Never. The docs call these files generated state and say not to rely on editing them by hand; they document no import layout |
| Hermes | `MEMORY.md` and `USER.md` of the default home and of each `profiles/<name>/` | Entries (separated by `§`) the file lacks are added at the end, up to the documented default limits (2,200 and 1,375 characters; raise `memory_char_limit` / `user_char_limit` and apply again for more). Needs the Hermes home (or profile folder) to exist |

A note that still holds a `[REDACTED]` marker is not written (Hermes: that entry is left out). Apply
is additive and `kitbag undo` puts back what it changed; the notes are listed in the plan. They are
read into later sessions as context, so a profile from a **git source** has them withheld unless you
pass `--trust-source`. Notes apply only to their own tool, and only if that tool is among the
targets. A note can carry kitbag's own frontmatter keys (kept in the profile, never written into a
tool's note file, and never set by capture; a re-capture keeps them): `targets` or `disabled_on`
(not both; `only` / `except` are accepted; unknown ids are warned about and ignored) skip the note
when its own tool is left out, and `fold_into_instructions: true` also appends the note's text
(without its frontmatter) to the instructions kitbag already writes for a target that has no memory
store (for instance Gemini CLI's `GEMINI.md`), in the same managed block as the profile's
instructions, so text outside it is untouched. Targeting never copies a note into another tool's
store (a Claude note does not go into Hermes's folder), Claude Code and Hermes are not folded into,
Codex's generated-state notes are never folded, a note with `[REDACTED]` markers is not folded, and a
git source's notes (folds included) still need `--trust-source`. The missing portable `<project>` recipe, Codex's generated memory files,
and the stores below that official docs don't name are dated skips (2026-11-27). Not carried, with the reason: `CLAUDE.local.md` (documented as personal project
*instructions*, not memory); Cursor (its Memories UI was removed in 2.1, nothing on disk); Windsurf
(memories are workspace-bound and the docs name no folder); Gemini CLI (its auto memory page gives
no path); Antigravity (no official page on its knowledge folder); Grok Build (its memory page shows
relative paths only); Copilot (Copilot Memory is stored on GitHub; VS Code's experimental
`.github/copilot-memories.jsonl` has no documented format); Cline's Memory Bank (a convention in
your own project files); Amp, opencode, Roo Code, Continue and Zed document no memory store.

### Claude Code plugins, by reference: `[plugins.claude-code]`

A Claude Code plugin is someone else's code (it can bundle hooks, MCP servers, commands, agents
and skills), so kitbag never copies one. It carries only what Claude Code itself stores to find
and enable it: the marketplaces Claude Code knows (`extraKnownMarketplaces`) and the plugins it
has enabled (`enabledPlugins`), in a per-target section of `kitbag.toml`. `claude-code`,
[`copilot` and `codex`](#copilot-cli-and-codex-plugins-by-reference-pluginscopilot-pluginscodex),
[`opencode`](#opencode-plugins-by-reference-pluginsopencode),
[`antigravity`](#antigravity-plugin-folders-by-reference-pluginsantigravity) and
[`zed` and `grok-build`](#zed-extensions-and-grok-build-plugin-folders-by-reference-pluginszed-pluginsgrok-build)
have such a section; every other tool's plugin system is a documented skip (see
[Plugins not carried](#plugins-not-carried-and-what-was-re-checked)), and kitbag never
translates an id from one tool to another. A `[plugins.<other tool>]` section is
ignored with a warning.

```toml
[plugins.claude-code.marketplaces.acme-tools]     # the name Claude Code registers it under
source = { source = "github", repo = "acme/tools", ref = "v1.2.0" }   # Claude Code's own source object
autoUpdate = false                                # optional, Claude Code's own key
# disabled_on = ["claude-code"]                   # or targets = [...]; as for an MCP server

[plugins.claude-code.enabled."formatter@acme-tools"]   # <name>@<marketplace>, as in enabledPlugins
version = "1.2.0"                                 # optional: what installed_plugins.json recorded
```

A `source` is one of Claude Code's documented kinds: `github` (`repo`, optional `ref` and `path`),
`git` (`url`, optional `ref` and `path`), `url` (`url`) and `file` / `directory` (`path`; a path
in your home directory is stored as `~/...` and expanded on the machine you apply to). `ref` is
the pin: Claude Code has no setting to pin an individual plugin, so `version` is only a record,
shown in the plan and not enforced. Docs checked 2026-11-27:
[install](https://code.claude.com/docs/en/plugins/install),
[loading](https://code.claude.com/docs/en/plugins/loading),
[organization settings](https://code.claude.com/docs/en/plugins/org).

**Capture** reads `enabledPlugins` and `extraKnownMarketplaces` from `~/.claude/settings.json`
(`CLAUDE_CONFIG_DIR` honoured), or from `.claude/settings.json` with `--project`
(`settings.local.json` is not read). Plugins set to `true` are kept; `false` ones, ids that
don't come from a marketplace (`@inline`, `@skills-dir`, `@synced`, the last coming from your
claude.ai account) and marketplaces that kitbag can't carry are listed and left out: an inline
`settings` marketplace, one with a `source` kitbag doesn't know, one with request `headers` (they
can hold credentials) and a URL with credentials in it. At user scope the version of a plugin's
user-scope install is read from `plugins/installed_plugins.json` (its layout is only described in
the docs as "`scope`, `installPath` and `version`", so an entry that isn't in that form is simply
not versioned); an install there that `settings.json` doesn't enable is counted and left out. Nothing under `plugins/` (the cache, marketplace checkouts) is read into the
profile, and `enabledPlugins` / `extraKnownMarketplaces` are not in the `settings` allow-list
either. `capture --force` keeps the `targets` / `disabled_on` of the plugins it captures again.

**Apply** adds the profile's `extraKnownMarketplaces` entries and `enabledPlugins` keys to the
same settings file (user or project scope), and nothing else; it never downloads, installs or
runs a plugin, and never writes under `~/.claude/plugins`. What Claude Code does next is its
own: at user scope it fetches the declared marketplaces and the enabled plugins that aren't
cached yet the next time it starts (or run `claude plugin install <name>@<marketplace>` now).
At project scope an enabled plugin is **not** installed by the settings file: each person runs
`claude plugin install <name>@<marketplace> --scope project` once per clone, and the project's
marketplaces are registered only after the folder is trusted. The plan says which of the two
applies, and lists every marketplace (with its source) and plugin it adds. An entry already in
the file is left alone; a marketplace that differs is replaced (its `source` whole, other keys of
the entry stay), a plugin set to `false` is set to `true`, and `--no-overwrite` keeps both
and warns. A second apply is "Up to date", and `kitbag undo` restores the settings file as it
was, so plugins you installed some other way stay. Apply is additive: removing a plugin from the
profile, or disabling it with `disabled_on`, doesn't uninstall or disable it in Claude Code
(`claude plugin disable`). Plugins in `settings.local.json` are never written.

**From a git source**, plugins and marketplaces are withheld unless you pass `--trust-source`
(plugins can bundle hooks, MCP servers and commands, the same class of trust as hooks): the plan
lists each marketplace with its source and each plugin, and nothing is written for them. `kitbag
inspect` lists them under what runs commands. Not carried yet: `CLAUDE_CODE_PLUGIN_CACHE_DIR`
(the plugins directory is read at `~/.claude/plugins`), the `additionalMarketplaces` alias of
`extraKnownMarketplaces`, managed settings.

### opencode plugins, by reference: `[plugins.opencode]`

opencode loads plugins from the npm packages listed in the `plugins` array of its config
(`plugin` in opencode 1), which it installs itself at startup, and from code in its `plugins/`
folders. kitbag carries only the packages, by reference: the id is the npm spec as opencode
reads it (`opencode-x`, `opencode-x@1.2.0`, `@scope/x`; a version in the id is the pin).
opencode has no marketplaces, and `version` isn't used.

```toml
[plugins.opencode.enabled."@acme/opencode-plugin@1.2.0"]
# disabled_on = ["opencode"]                      # or targets = [...]; as for an MCP server
[plugins.opencode.enabled."opencode-notify"]
```

**Capture** reads the array from every opencode config file at the scope (`~/.config/opencode`
or the project's `opencode.json(c)` / `.opencode/opencode.json(c)`), in both its `plugins` and
`plugin` spellings, and in opencode's string, `{"package": ...}` and `[spec, options]` forms.
Left out, with a note: local paths and `file:` entries (code on that machine), anything that
isn't an npm spec (the docs don't give a git spec's syntax, so that is UNVERIFIED and not
written), `-name` entries (a plugin turned off) and the `options` of a plugin, which can hold
keys. The `plugins/` folders are plugin code and are never read into a profile.

**Apply** appends the packages that aren't listed yet to the array the config file already has
(`plugins`, or `plugin` when that is the key there or the file is in opencode 1's MCP layout; a
new array is `plugins`), in the same file the MCP servers go to, and writes nothing else. It
never installs or runs a plugin or touches `plugins/` or the cache: opencode installs the
packages itself the next time it starts, and the plan says so. A package already listed, in any
form, is left alone, including one you turned off with a `-` entry; a different pinned version
of a plain entry is replaced (not with `--no-overwrite`, and never an entry that has options). A
second apply is "Up to date", and `kitbag undo` restores the config as it was, so plugins you
added another way stay. Apply is additive. **From a git source** the packages are withheld
unless you pass `--trust-source` (a plugin runs inside opencode with its permissions); the
plan lists each one, and `inspect` lists them under what runs commands. Docs checked 2026-11-27:
[plugins](https://opencode.ai/docs/plugins/), [config](https://opencode.ai/docs/config/),
[migrating from v1](https://opencode.ai/docs/migrate-v1/). Whether opencode 2 still reads the
singular `plugin` key isn't said there, so a file kitbag creates uses `plugins`.

### Antigravity plugin folders, by reference: `[plugins.antigravity]`

An Antigravity plugin is a folder with a `plugin.json`. `agy plugin install` (from its
marketplace, a GitHub URL or a local path) unpacks one into `~/.gemini/config/plugins/<name>/`
(or a workspace's `.agents/plugins/`), and that is the install: the docs name no file that
records where it came from. What they do document, on the
[customization discovery](https://antigravity.google/docs/customization-discovery/) page, is the
`plugins.json` discovery manifest: in `~/.gemini/config/` for all your projects or in a
project's `.agents/`, with `entries` that each take a `path` (a folder to scan one level deep, or
a single plugin's folder) and optional `include_only` and `exclude` lists. That is a by-reference
mechanism, and it is the one kitbag writes: the id is the folder's path, and nothing else.

```toml
[plugins.antigravity.enabled."~/team/plugins/formatter"]   # a plugin folder (or a folder of them) on this machine
include_only = ["strict"]                         # optional: Antigravity's own entry fields
exclude = ["slow"]
# disabled_on = ["antigravity"]                   # or targets = [...]; as for an MCP server

[plugins.antigravity.enabled."tools/plugins/fmt"]  # in a project profile: a path inside the project
```

Antigravity doesn't expand `~`, so kitbag stores a path in your home directory as `~/...` and
writes it out in full on the machine you apply to. There are no marketplaces to list (the docs
give no setting for them) and no `version`.

**Capture** reads `entries` from `~/.gemini/config/plugins.json`, or `.agents/plugins.json` with
`--project`. At user scope a relative path (which Antigravity reads from `~/.gemini/config/`) is
stored in full; at project scope only a path inside the project is kept. Left out, with a note:
an entry without a `path` or with an `include_only` / `exclude` that isn't a list of names, a
path starting with `~` (Antigravity never loads it), `inherits` (other manifests on that
machine), the top-level `exclude`, and, at project scope, a path outside the project. The
plugins that `agy plugin install` put in `plugins/` are counted and left out: they are the
plugin code, and there is nothing to say where they came from, so install them again with `agy
plugin install` on the other machine. Nothing in `plugins/` is ever read into a profile.

**Apply** adds the missing folders to `entries` of the same manifest, and writes nothing else.
It never installs, fetches, copies or enables a plugin, and never touches `plugins/`: the
folder has to be on the machine already (the plan warns when it isn't), and Antigravity keeps
its own enabled / disabled state (`agy plugin enable|disable`). A folder that is already listed,
however its path is written (absolute, with a trailing slash, relative to `~/.gemini/config/`), is left alone; one
whose `include_only` / `exclude` differ from the profile's is updated (not with
`--no-overwrite`, which keeps it and warns), and the other keys of an entry stay. A relative
path is written only at project scope and only if it stays inside the project, because it means
something else in each scope and a project's manifest is committed (Antigravity's changelog says
it skips a workspace entry that points elsewhere); anything else is listed in the plan as not
written. A second apply is "Up to date", and `kitbag undo` restores the manifest as it was.
Apply is additive: removing a folder from the profile doesn't remove it from the manifest.
**From a git source** the folders are withheld unless you pass `--trust-source` (a plugin can
bundle hooks and MCP servers that run on your machine): the plan lists each one, and `inspect`
lists them under what runs commands. Not covered: where Antigravity keeps a plugin's enabled
state (its changelog says `config.json` in `~/.gemini/config/`, but no page documents the keys,
so kitbag doesn't guess them), and a plugin whose manifest says `"disabled": true` starts
disabled the first time Antigravity loads it. Docs checked 2026-11-27:
[customization discovery](https://antigravity.google/docs/customization-discovery/),
[plugins](https://antigravity.google/docs/plugins),
[build plugins](https://antigravity.google/docs/build-plugins/),
[settings](https://antigravity.google/docs/settings/),
[changelog](https://antigravity.google/docs/changelog/).

### Copilot CLI and Codex plugins, by reference: `[plugins.copilot]`, `[plugins.codex]`

Both tools list plugins and marketplaces in a settings file, so kitbag carries those lists the way
it carries Claude Code's, and never a plugin or a marketplace checkout. The ids are each tool's
own (`<name>@<marketplace>`) and are not translated. Checked against the official docs on
2026-11-27.

**Copilot CLI** reads `enabledPlugins` (`"<name>@<marketplace>": true`, "declarative plugin
auto-install") and `extraKnownMarketplaces` (`"<name>": {"source": ..., "autoUpdate": bool}`) from
`~/.copilot/settings.json` (`$COPILOT_HOME`) and a repository's `.github/copilot/settings.json`
(committed, so it applies to everyone who works in the repository and is also read by Copilot in
the cloud); a person can turn an entry off with `false` in `.github/copilot/settings.json`'s
local twin, `settings.local.json`. A marketplace `source` is `github` (`repo`, `ref`, `path`), `git`
(`url`, `ref`, `path`) or `directory` (`path`); the `url` and `file` sources of Claude Code are not
documented there, so a profile with one is refused. `autoUpdate` only counts in user settings (a
repository's is accepted and ignored). The built-in marketplace is `awesome-copilot`. The user
file is the CLI's; the repository file is also read by Copilot in the cloud and, for workspace
plugin recommendations and enablement, by VS Code (which also reads `.claude/settings.json`, where
`[plugins.claude-code]` writes at project scope). VS Code's own user-level settings are not carried
(see the end of this section). Managed (enterprise or MDM) entries can't be re-pointed or
turned off locally, and kitbag never writes the managed files.

```toml
[plugins.copilot.marketplaces.acme-tools]
source = { source = "github", repo = "acme/tools", ref = "v1.2.0" }
autoUpdate = true

[plugins.copilot.enabled."formatter@acme-tools"]
```

**Codex** reads `[marketplaces.<name>]` (`source_type` = `git` or `local`, `source` (a Git
repository location, or the absolute path of a local marketplace root), optional `ref` and, for git,
`sparse_paths`) and `[plugins."<name>@<marketplace>"]` with `enabled` from `config.toml` in
`$CODEX_HOME` (`~/.codex`), and from `.codex/config.toml` in a project that is marked as trusted.
The built-in marketplace is `openai-curated`. The profile holds Codex's own keys:

```toml
[plugins.codex.marketplaces.acme]
source = { source_type = "git", source = "https://github.com/acme/plugins.git", ref = "v1.2.0" }

[plugins.codex.marketplaces.mine]
source = { source_type = "local", source = "~/team/marketplace" }   # ~/ is expanded on apply

[plugins.codex.enabled."fmt@acme"]
```

**Capture** keeps the marketplaces and the plugins that are on (`enabled = true`). Counted and left
out, with a note: plugins set to `false`, `[plugins.*]` tables without `enabled`, the per-plugin
`mcp_servers` settings (approval modes and tool lists), `[plugins._default]` (Codex), plugins
whose marketplace isn't declared (named in the note: only the plugin is carried), a marketplace
with a source kind kitbag doesn't carry, with credentials in its URL, or (Copilot) with `headers`.
A local marketplace or `directory` path under your home directory is written as `~/...`.
Nothing under `~/.copilot/installed-plugins/` or `~/.codex/plugins/` is read.

**Apply** adds the profile's marketplaces and plugins to the settings file, in the same file as the
MCP servers, and nothing else: a marketplace already there with the same source is left alone, a
different one is replaced (Codex: key by key, so the keys Codex keeps itself stay), a plugin
switched off is switched on, and `--no-overwrite` keeps all of those and says so. The plan lists
every marketplace (with its source) and plugin it adds, and says that kitbag doesn't install
anything: Copilot CLI installs the `enabledPlugins` entries itself, and Codex fetches a configured
marketplace and installs or refreshes its configured plugins itself (`codex plugin marketplace
upgrade` does it now; exactly when Codex does it on its own is UNVERIFIED, the docs say "during
marketplace refresh"). A second apply is "Up to date", and `kitbag undo` restores the file; apply
is additive, so removing a plugin from the profile doesn't remove it from the file. **From a git
source** the plugins and marketplaces are withheld unless you pass `--trust-source` (a plugin can
bundle hooks, MCP servers and skills): the plan lists each one and `inspect` lists them under what
runs commands.

Docs: Copilot CLI,
[configuration directory](https://docs.github.com/en/copilot/reference/copilot-cli-reference/cli-config-dir-reference)
(user and repository settings, `enabledPlugins`, `extraKnownMarketplaces`),
[enterprise managed settings](https://docs.github.com/en/copilot/reference/enterprise-managed-settings-reference)
(the marketplace `source` kinds),
[plugins](https://docs.github.com/en/copilot/concepts/customizations/plugins); Codex,
[config reference](https://learn.chatgpt.com/docs/config-file/config-reference) (`marketplaces.<name>.*`,
`plugins.<plugin>.enabled`),
[packaging plugins](https://developers.openai.com/plugins/build/plugins) (enabling a plugin for a
repo, `codex plugin marketplace add`).

### Zed extensions and Grok Build plugin folders, by reference: `[plugins.zed]`, `[plugins.grok-build]`

**Zed** installs the extensions listed in `auto_install_extensions` of its user `settings.json`
(`{"html": true, "docker-compose": false}`: `true` installs, `false` means never) from its gallery,
by itself. The profile lists the extension ids; no extension files are copied, and Zed's
`installed/` and `work/` folders are never read.

```toml
[plugins.zed.enabled."html"]
[plugins.zed.enabled."toml"]
```

Capture carries the ids set to `true` and counts the ones set to `false`. Apply adds missing ids as
`true` (an id set to `false` is switched to `true`, not with `--no-overwrite`), keeps the comments of
the JSONC file, and is additive. It writes at user scope only: Zed's docs describe the setting
only for the user's `settings.json`, not a project's `.zed/settings.json` (UNVERIFIED), so project
scope says so and writes nothing. An extension can bundle language servers and MCP servers, so
from a git source it is withheld unless you pass `--trust-source`.
Docs (2026-11-27): [installing extensions](https://zed.dev/docs/extensions/installing-extensions),
[all settings](https://zed.dev/docs/reference/all-settings) (`auto_install_extensions`).

**Grok Build** loads plugins from `./.grok/plugins/`, `~/.grok/plugins/`, marketplace installs, the
`--plugin-dir` flag, and "extra paths under `[plugins] paths` in `~/.grok/config.toml`". That list
of folders is the by-reference layout, and kitbag carries it like Antigravity's, with the folder's
path as the id (`~/...` for the home directory, which kitbag expands when it applies, because the docs
don't say Grok Build expands `~`):

```toml
[plugins.grok-build.enabled."~/team/plugins/formatter"]
```

Capture reads `paths` in the user `config.toml`; a relative path is not carried (the docs don't say
what it is relative to, UNVERIFIED), and the names in `[plugins] enabled` / `disabled` are counted
and left out, since they name plugins that have to be installed first. Apply appends the missing
folders to `[plugins] paths` of the user config (a folder that is already listed, however it is
written, is left alone), warns when the folder doesn't exist on this machine, and never copies or
installs a plugin. Project scope writes nothing: `.grok/config.toml` may hold `[plugins]`, but
the docs don't say what a relative path in it means, and an absolute one is specific to one
machine. Marketplace sources (`[[marketplace.sources]]` in `config.toml` and
`~/.grok/plugins/known_marketplaces.json`) are named in the docs without a layout for their
entries (UNVERIFIED), so they are not written, and a profile with `[plugins.grok-build.marketplaces]`
is refused. Grok Build also reads Claude Code's marketplaces and plugins by itself, which
`[plugins.claude-code]` already carries. Plugins from a git source need `--trust-source`.
Docs (2026-11-27): [skills, plugins and marketplaces](https://docs.x.ai/build/features/skills-plugins-marketplaces),
[settings reference](https://docs.x.ai/build/settings/reference).

### Plugins not carried, and what was re-checked

Checked against each tool's official docs on 2026-11-27; none of these documents a way to list
a plugin by reference that kitbag could write without copying plugin code or running an install.

- **Antigravity's installed plugins** (the folders in `~/.gemini/config/plugins/` made by `agy
  plugin install`, from its marketplaces `antigravity-plugins-official` and
  `antigravity-plugins-partners`, a GitHub URL or a local path): the folder is the install and
  holds the plugin code, `agy plugin list --json` reports their `source` and `enabled`
  state but no file that holds it is documented (the changelog says `config.json`, with no keys),
  and no setting lists marketplaces. Only the local folders in `plugins.json` are carried, above.
  ([plugins](https://antigravity.google/docs/plugins), [build plugins](https://antigravity.google/docs/build-plugins/))
- **Gemini CLI**: "Gemini CLI loads extensions from `<home>/.gemini/extensions`", and `gemini
  extensions install` "creates a copy of the extension" there; each extension holds a
  `gemini-extension.json`. The docs name no file that records an install source or whether it is
  enabled (extensions are on by default; `--scope user|workspace` turns one off, and the docs
  don't say where that is kept), and no `settings.json` key lists them, so `extension-enablement.json`
  stays UNVERIFIED and kitbag doesn't guess it. The one by-reference route is the `gemini
  extensions link <path>` command, which creates a symbolic link to a development directory;
  that is a command rather than a documented layout, and a link's target is a directory on that
  machine, so it isn't carried. The docs also say Gemini CLI was replaced by Antigravity CLI for
  unpaid-tier and Google One users on June 18th, 2026.
  ([extensions](https://geminicli.com/docs/extensions/), [reference](https://geminicli.com/docs/extensions/reference/))
- **Claude Desktop**: the only filesystem layout its docs give for plugins that you could set up
  is the organization plugins directory (`/Library/Application Support/Claude/org-plugins/` on
  macOS, `C:\Program Files\Claude\org-plugins\` on Windows, none on Linux). An administrator owns
  it (root or administrators only, on purpose) and it holds the plugins themselves, so kitbag
  doesn't write there. A user's own plugins are added in the app and, in the docs of Claude Desktop on third-party platforms, kept under
  `local-agent-mode-sessions/.../cowork_plugins/` in its data directory (`~/Library/Application
  Support/Claude-3p/`, `%LOCALAPPDATA%\Claude-3p\`, `~/.config/Claude-3p/`), which holds the
  plugin code and whose full path the docs elide; they list no file to record one by reference.
  Marketplaces are set by an administrator (`allowedPluginMarketplaces` and the other policy
  keys) or in the app. A Code session reads Claude Code's own `~/.claude/` setup, which is
  carried under `[plugins.claude-code]`.
  ([extensions](https://claude.com/docs/third-party/claude-desktop/extensions),
  [data storage](https://claude.com/docs/third-party/claude-desktop/data-storage))
- **Amp**: plugins are `.ts` / `.js` files or `<name>/index.ts` folders in `.amp/plugins/`,
  `~/.config/amp/plugins/` (`$XDG_CONFIG_HOME/amp/plugins/`, `%USERPROFILE%\.config\amp\plugins\`
  on Windows) or the hosted personal and workspace repositories; `amp plugins add <url>` and
  the share-URL import end with files in one of those places. The docs describe no
  `settings.json` key or manifest that lists plugins by reference, and no marketplace, so
  there is only plugin code to copy. ([plugins](https://ampcode.com/docs/markdown/customize/plugins))
- **Cline**: plugins are SDK code, passed to a session as `extensions` or as `pluginPaths`
  (absolute paths to `.ts` files), or installed with `cline plugin install` (file URLs, npm, git
  or local paths), and the docs don't say where that records an install or that a settings
  file lists them. ([SDK plugins](https://docs.cline.bot/sdk/plugins.md))

- **Cursor**: plugins are installed in the **Customize** page of the app (project or user scope,
  stored where the docs don't say), team marketplaces are set in the dashboard (Plugins & MCPs)
  by an admin, and the one folder named, `~/.cursor/plugins/local`, holds the plugin code itself
  (a symlink there loads only if it points to a directory inside that folder). The docs name no
  settings key or file that lists a plugin or marketplace by reference.
  ([plugins](https://cursor.com/docs/plugins), [reference](https://cursor.com/docs/reference/plugins))
- **Hermes Agent**: `hermes plugins install owner/repo` clones into `~/.hermes/plugins/<name>/`
  and records the source and revision in a `.install-metadata.json` inside the install, so the
  folder is the install. `plugins.enabled` in `config.yaml` is a list of plugin *names* that are
  only enabled once installed ("plugins install disabled by default, enabling is an explicit
  step"), and a `hermes-pack.yaml` is read only by `hermes plugins pack install`, a command that
  clones the repositories and asks for consent plugin by plugin. A name list without the install,
  or a file only a network install reads, is not a by-reference layout, so none is written.
  ([plugins](https://hermes-agent.nousresearch.com/docs/user-guide/features/plugins))
- **Windsurf**: the plugins its docs describe are the Windsurf plugin for JetBrains IDEs (an
  editor integration, in maintenance mode) and Cascade's MCP "plugins", which are the servers in
  `mcp_config.json` that kitbag already carries as MCP servers. Its editor extensions come from
  the Open VSX marketplace (a Marketplace URL setting in the app), and no page lists them by
  reference for an AI tool.
  ([plugins](https://docs.devin.ai/windsurf/plugins/getting-started), [MCP](https://docs.windsurf.com/plugins/cascade/mcp))
- **Roo Code**: no plugin system. Its Marketplace installs MCP servers (`mcp_settings.json`,
  `.roo/mcp.json`) and modes (`custom_modes.yaml`, `.roomodes`) into files, with no key that lists
  marketplace items by reference.
  ([marketplace](https://roocodeinc.github.io/Roo-Code/features/marketplace))
- **Continue**: no plugin system; the hub's blocks (models, rules, MCP servers, prompts) are
  configuration items that `config.yaml` references with `uses`, not code a tool installs. ([blocks](https://docs.continue.dev/hub/blocks/intro))

Every other target with a plugin or extension system is carried above or is one of these skips,
except the one gap below. What each carried tool still
leaves to the tool itself: Claude Code, Copilot CLI and Codex fetch the plugins; opencode installs
its packages; Zed downloads its extensions; Antigravity and Grok Build load the folders and keep
their enabled state.

**Documented, not carried (dated skip, 2026-11-27; #12 closes):** VS Code's user-level plugin settings for the
Copilot agent. Re-read 2026-11-27 ([agent plugins](https://code.visualstudio.com/docs/agent-customization/agent-plugins),
[plugin reference](https://code.visualstudio.com/docs/agents/reference/agent-plugin-reference),
[AI settings](https://code.visualstudio.com/docs/agents/reference/ai-settings),
[settings files](https://code.visualstudio.com/docs/configure/settings)). Two Experimental settings
go in `settings.json`: `chat.plugins.marketplaces` (default
`["github/copilot-plugins", "github/awesome-copilot"]`; an array of `owner/repo` with an optional
`#<ref>` (a branch, tag or 40-character commit SHA), an HTTPS or SCP-style git remote, or a
`file:///` URI of a marketplace already cloned) and `chat.pluginLocations` (default `{}`; a map of
plugin directory to `true` / `false`, "to register an existing development directory without
installing it"). Both are by reference and their shapes are documented, and the settings page names
the default profile's user file for Code (`%APPDATA%\Code\User\settings.json`, `~/Library/Application
Support/Code/User/settings.json`, `~/.config/Code/User/settings.json`). They are still not written,
because the docs leave four things open that kitbag would otherwise have to guess:

- **Which file.** Other profiles keep their settings in `.../User/profiles/<profile ID>/settings.json`,
  and no page says how to tell which profile is active; Code Insiders and VSCodium are not named at
  all. Remote windows use the separate "Remote settings" file, which has no documented path.
- **Replace or add.** `chat.plugins.marketplaces` is described as "additional" marketplaces, but its
  default value already holds the two built-in ones. No page says whether a user's array replaces
  that default or is added to it, so a write that is meant to be additive could drop the built-in
  marketplaces (UNVERIFIED either way).
- **Enablement.** `chat.plugins.enabled` is `false` by default, can be managed by an organization,
  and the docs say nothing on whether a tool other than VS Code itself should switch it on. A
  marketplace "does not install or enable" its plugins, and `chat.plugins.enabledPlugins` is only
  described for enterprise policy.
- **A tool name.** The `copilot` target is Copilot CLI and the repository's
  `.github/copilot/settings.json`; VS Code's own file would need a separate target (the docs give
  no reason to mix the two) and a profile shape for marketplaces without a name.

Nothing in the `.github/copilot/settings.json` carry above depends on this: VS Code reads
`enabledPlugins` and `extraKnownMarketplaces` from that file, which `[plugins.copilot]` and
`[plugins.claude-code]` already write. Also not writable from the docs, and therefore not a gap
in kitbag: Grok Build's marketplace sources and project-level `[plugins] paths` (no entry shape
or path meaning is documented), and Zed's project-level `auto_install_extensions` (UNVERIFIED).

### Editor extensions, AI settings and AI keybindings: `[editor.vscode]`, `[editor.zed]`

An optional `[editor.<id>]` section carries a small, allow-listed slice of an editor: extension
ids, a few AI settings, and a few AI keybindings. It is not an editor sync (no theme, font,
window, tab or format-on-save settings, no snippets, no Settings Sync, no `settings.json` dump);
the editors have Settings Sync for that. Everything below was read from the official docs on
2026-11-27; what those docs don't name is not carried.

```toml
[editor.vscode]
extensions = ["Continue.continue", "saoudrizwan.claude-dev"]   # exact publisher.extension, optional @version

[editor.vscode.settings]
"chat.agent.enabled" = true
"github.copilot.enable" = { "*" = true, markdown = false }

[[editor.zed.keybindings]]
context = "Workspace"                                          # optional, as in keymap.json
bindings = { "ctrl-alt-a" = "agent::Toggle" }
```

`kitbag capture --from vscode` reads the AI keys of `User/settings.json` and runs
`code --list-extensions`; `kitbag capture --from zed` reads the AI bindings of `keymap.json`.
`apply --to vscode` merges the settings into the existing JSONC file (comments and your other keys
stay), and `apply --to zed` appends the bindings as a block at the end of `keymap.json` (Zed lets
the later binding win; a binding the file already has is not repeated, and one it binds
differently is noted). Both are idempotent, and `kitbag undo` restores the files.

**VS Code, default profile at user scope; workspace settings at project scope.** At user scope the
settings go to `User/settings.json` (`%APPDATA%\Code\User` on Windows,
`~/Library/Application Support/Code/User` on macOS, `~/.config/Code/User` on Linux).
`User/profiles/<id>/` is never written: VS Code's profiles page says how to see the active profile
in the UI but gives no way for a program to tell, so kitbag doesn't guess. With `--project`, the
same allow-listed settings (and no others) are captured from and merged into the project's
`.vscode/settings.json` (the directory is created if needed); `User/settings.json` is not written
then, and `.vscode/settings.json` is not written at user scope. Extensions stay a user-level
`code --install-extension` concern: none is listed or installed at project scope. `code
--user-data-dir` and portable mode are not followed.

**What is carried.**

- Extensions (`code --install-extension publisher.extension[@version]`), by the exact id each
  extension's own official page publishes, matched without regard to case and never rewritten
  (and never switched between the Microsoft Marketplace and Open VSX, or installed from a VSIX
  path or a link):
  `Continue.continue` ([docs.continue.dev](https://docs.continue.dev/ide-extensions/install)),
  `saoudrizwan.claude-dev` for Cline ([cline.bot/install](https://cline.bot/install)) and
  `RooVeterinaryInc.roo-cline` for Roo Code
  ([Roo Code docs](https://roocodeinc.github.io/Roo-Code/getting-started/installing)).
  The ids of GitHub Copilot's extensions are **not** carried: no official GitHub or VS Code page
  I could find publishes a `publisher.extension` id for them (UNVERIFIED which ids are current),
  so they are a documented skip.
- Settings, from VS Code's [AI settings reference](https://code.visualstudio.com/docs/copilot/reference/copilot-settings),
  with the type shown there (a value of another type is refused): `chat.agent.enabled`,
  `chat.agent.maxRequests` (1 to 1000), `chat.checkpoints.enabled`, `chat.commandCenter.enabled`,
  `chat.disableAIFeatures`, `chat.useAgentSkills`, `chat.useAgentsMdFile`, `chat.useClaudeMdFile`,
  `github.copilot.editor.enableCodeActions`, `github.copilot.enable` (language id = true/false),
  `github.copilot.nextEditSuggestions.enabled` and
  `github.copilot.chat.codeGeneration.useInstructionFiles`. All are booleans, a count or a map of
  booleans, so no value can hold a token. Nothing flagged Experimental or Preview there, and none
  of the auto-approve, terminal, sandbox, endpoint or model keys.
- Zed keybindings, from [Zed's keybindings docs](https://zed.dev/docs/key-bindings): only plain
  string actions (no arguments) that [Zed's action list](https://zed.dev/docs/all-actions) names:
  `agent::AddSelectionToThread`, `agent::CycleFavoriteModels`, `agent::ExpandMessageEditor`,
  `agent::Keep`, `agent::KeepAll`, `agent::NewThread`, `agent::OpenAgentDiff`, `agent::Reject`,
  `agent::RejectAll`, `agent::Toggle`, `agent::ToggleFocus`, `agent::ToggleModelSelector`,
  `agent::ToggleNewThreadMenu`, `agent::ToggleProfileSelector`, `agent::ToggleThinkingMode`,
  `agent::UndoLastReject` and `assistant::InlineAssist`. Not the actions that approve a tool call
  (`agent::AllowAlways`, `agent::AllowOnce`), not bindings with arguments, not the rest of your
  keymap. Zed's extensions stay in `[plugins.zed]` and its agent settings in `[settings.zed]`;
  `[editor.zed]` refuses `extensions` and `settings` rather than repeat them.

**Extensions run code.** Every plan lists each extension as `publisher.extension` (with its
version when the profile pins one) and whether it is already installed, will be installed, or
can't be because `code` is not on `PATH` (a warning; nothing is called installed). Only extensions
that are not installed yet are installed, after the files are written, and only once you
confirm (or pass `--yes`); `--dry-run` and `diff` install nothing. From a git source no extension
is installed without `--trust-source` (the settings are plain data and still apply), and the plan
names each one withheld. `inspect` lists them among what a profile runs. An id that isn't on the
list above is dropped with a note, so a hand-written profile can't install anything else.

**Undo restores `settings.json` and `keymap.json`, and uninstalls the extensions that apply
installed.** kitbag records, on the apply's transaction, each extension it newly and successfully
installed, and `kitbag undo` runs `code --uninstall-extension` for exactly those. One you already
had, one that failed to install, one withheld by `--trust-source` and one skipped (no `code`, or
you declined) is never recorded, so undo never removes it. If `code` is not on `PATH` or the
uninstall fails, undo says so, leaves that extension installed and still restores the files; one you
already uninstalled yourself is fine. Transactions recorded by older versions have no list, so
their undo uninstalls nothing.
`--home` redirects the files but not `code`: `code` lists and installs for whoever runs kitbag.

**Not carried, and why** (official docs checked 2026-11-27):

- VS Code's `keybindings.json`: its [keybindings page](https://code.visualstudio.com/docs/configure/keybindings)
  describes the file, the evaluation order and the `-command` removal syntax but names no path
  for it (the profiles page only says profiles are stored "similar to your user settings and
  keyboard shortcuts"), so kitbag doesn't write there.
- Cursor's editor: [Cursor's VS Code migration page](https://cursor.com/docs/configuration/migrations/vscode)
  describes an import of extensions, themes, settings and keybindings, and names no `settings.json`
  or `keybindings.json` path and no `cursor --install-extension` / `--list-extensions`. Skipped
  (the Cursor Agent CLI's own settings are `[settings.cursor]`). `state.vscdb` is never read.
- Windsurf: its docs now redirect to Devin Desktop's, which name no editor settings path and no
  extension command, and say extensions can't be installed through any marketplace on Devin Desktop. Skipped;
  Devin folders are not written (see #20).
- VSCodium: its [site](https://vscodium.com/) names no settings path, keybindings path or
  `codium` command. VS Code Insiders: the profiles page says only that its folder is
  `Code - Insiders` (profiles location), not its settings or command, so it is skipped too.
- Non-default profiles (`User/profiles/<id>/`), Zed's project
  keymap (Zed's keybindings docs, checked 2026-11-27, name only the user file
  `~/.config/zed/keymap.json` / `%APPDATA%\Zed\keymap.json`, not a project keymap),
  Continue as an editor contribution beyond its extension id, and the Experimental
  `chat.plugins.marketplaces` / `chat.pluginLocations` (#12) are left alone.

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
since a plugin's agents are Claude Code subagents (this is kitbag's own plugin directory; Claude
Code's plugins are carried by reference, see the section above).
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
   kitbag resolves those from your environment at apply time (or from the bundle of
   `--secrets-from`, item 6) and warns per field. The value is
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
   `--allow-git-mcp`. A git source never supplies a secrets bundle: a `secrets.age` in a
   checkout is not read, and `--secrets-from` is refused for it.
6. **An opt-in encrypted secrets bundle: `secrets.age`.** A bare capture never writes a secret
   value. `kitbag capture --include secrets` additionally writes the values it just redacted
   (the literals in MCP env, headers, args and URLs, and a literal OAuth `client_secret`, which
   a bare capture leaves out of the profile) to **one** file next to `kitbag.toml`, encrypted with
   [age](https://age-encryption.org) (the format `rage` also reads): a map of `${VAR}` name to
   value, plus where capture found each. `kitbag.toml` keeps only the `${VAR}` references, and no
   plaintext copy (`secrets.json`, `secrets.toml`, a temporary file) is ever written.
   ```sh
   kitbag capture -o ~/dotagents --include secrets --secrets-passphrase-file ~/.kitbag-pass
   kitbag capture -o ~/dotagents --include secrets --secrets-recipient age1ql3z7hjy...
   kitbag secrets list ~/dotagents --secrets-passphrase-file ~/.kitbag-pass   # names only
   kitbag secrets set NEW_TOKEN ~/dotagents --value-file ~/new-token.txt \
     --secrets-passphrase-file ~/.kitbag-pass
   kitbag secrets rotate ~/dotagents --secrets-passphrase-file ~/.kitbag-pass \
     --new-secrets-passphrase-file ~/.kitbag-pass-2
   kitbag apply ~/dotagents --to cursor --secrets-from ~/dotagents/secrets.age \
     --secrets-passphrase-file ~/.kitbag-pass
   ```
   - **Encryption** is age's own: a passphrase (age's scrypt), or public keys (`age1...`, SSH
     `ssh-ed25519` / `ssh-rsa`, or a file of them, one per line; repeat `--secrets-recipient`).
     age doesn't mix a passphrase with keys, so kitbag refuses both together. With neither flag a
     terminal is asked for a passphrase twice; without a terminal capture stops before writing
     anything. To open it: `--secrets-passphrase-file`, or `--secrets-identity` with an age
     identity file or an unprotected SSH private key (passphrase-protected SSH keys aren't
     supported yet), or a terminal prompt. A passphrase is never taken from the command line, and
     is never printed.
   - **Where it goes.** `secrets.age` is written `0600` on Unix (on Windows it keeps the access
     of the folder it is in), and kitbag adds it to the profile's `.gitignore`, creating one if
     needed. Keeping it out of git stays the default; committing it is opt-in with
     `kitbag secrets commit` (below). Capturing again with `--force` and
     `--include secrets` replaces it; a bare re-capture leaves it as it is.
   - **Memory only.** Values are in memory only while a bundle is sealed or opened (zeroed on
     drop, and printed as `<redacted>` by every `Debug`). `apply --secrets-from PATH` decrypts
     the bundle and says which of the profile's variables it holds. Tools that read env
     references keep getting `${VAR}` references (in their own syntax), never a value from the
     bundle, and you set those variables in your environment. **Only where a value already has
     to be written because the tool can't read env references** (the "resolved value" rows of
     the table under [Targets](#targets): Claude Desktop, Antigravity, Zed, Cline,
     the Codex fields it can't read from the environment, and Continue's `command`, `cwd`,
     `url` and headers) does a variable the bundle holds take its value from the bundle; one it
     doesn't hold comes from your environment as before. Each such field is named in a warning
     and masked in the diff, the file is written `0600` on Unix, and `--secrets never` still
     keeps the placeholder. No value goes into the plan, the diff, the output, a log, or the
     state directory.
   - **The undo rule for those files.** A backup is only ever of what a file held *before* the
     apply. If it held no bundle value then, it is backed up as usual, and `kitbag undo` puts
     that content back (without the value). If it already holds one (a re-apply), it is written
     **outside the undo record**: no backup and no entry, like the MCP OAuth token store, the
     plan says so, and `kitbag undo` leaves that file as it is (re-apply, or edit it by hand).
     kitbag remembers such files by path only (`bundle-files.json` in the state directory), so
     a later apply without `--secrets-from` or with `--secrets never` doesn't back one up either;
     a path is forgotten when undo puts back its content from before the value, or when kitbag
     removes the file. The undo record keeps only a SHA-256 of each file as written, never its
     content.
   - `--secrets-from` is a local file only: it is refused for a git source (a profile you didn't
     write could name any variable) and with `--remote` (nothing is sent to the host). A bundle
     can be opened **on** the host instead, with `--remote-secrets-from` (see
     [Applying on a remote host over SSH](#applying-on-a-remote-host-over-ssh)). `kitbag diff`
     takes `--secrets-from` too and shows the bundle's values masked.
   - **`kitbag secrets list [FILE|DIR]`** prints variable names and capture locations, and any
     MCP OAuth token store by tool and path, never values.
   - **`kitbag secrets set NAME [FILE|DIR] --value-file PATH`** adds a variable, or replaces
     its value (a replaced one keeps where capture found it). The value comes only from
     `--value-file` (`-` is stdin; one trailing newline is dropped), never from the command
     line, and is never printed. An existing bundle is opened with `--secrets-passphrase-file`
     or `--secrets-identity` and written back the same way: a passphrase bundle with the same
     passphrase, a key bundle to the keys you give again with `--secrets-recipient`. A missing
     bundle is created, encrypted to `--secrets-passphrase-file` or `--secrets-recipient`
     (`0600`, and listed in the `.gitignore` of a profile directory). Every other variable and
     token store stays.
   - **`kitbag secrets rotate [FILE|DIR]`** changes only the encryption: it opens the bundle with
     the current `--secrets-passphrase-file` / `--secrets-identity` and writes the same variables
     and token stores encrypted to `--new-secrets-passphrase-file` or `--new-secrets-recipient`
     (one of them is required, never both, and a new passphrase must differ from the old one).
   - **`kitbag secrets commit [FILE|DIR]`** is the opt-in way to keep the already encrypted
     `secrets.age` in git (capture never does it, and never commits). It refuses a missing file, a
     file that isn't age-encrypted, a git URL, and a profile directory holding a plaintext secrets
     file (`.env`, `secrets.json` ...); it never stages a passphrase file, identity file or any
     sidecar. **Recommended: encrypt to keys** (`--secrets-recipient`, or `kitbag secrets rotate
     --new-secrets-recipient KEY`): only a holder of a matching identity can open it. For such a
     bundle it removes kitbag's `secrets.age` line from `.gitignore` (every other line stays) and,
     if the directory is a git repository, runs `git add` on `secrets.age` and `.gitignore` only,
     then prints the `git commit` to run; with no repository it skips `git add` and says so.
     Running it again once the file is tracked does nothing. **A passphrase-encrypted bundle
     gets a loud warning**: anyone who gets the repository (a clone, a backup, its history) can
     attack the passphrase offline. It is refused unless you also pass `--allow-passphrase-commit`
     (`--yes` doesn't count), and the warning is printed even then. Like the other `secrets`
     commands it writes no undo record and has no `--remote`.
   - Both `set` and `rotate` write the new ciphertext to a temporary file in the same directory and rename it over
     the old one, so a failure leaves the bundle as it was and no plaintext file, backup or
     sidecar is ever written. They work on a file on this machine only (a git URL is refused),
     have no `--remote`, and write no undo record.
   - **`kitbag doctor SOURCE`** fails when the profile directory holds a plaintext secrets file
     (`secrets.json`, `secrets.toml`, `secrets.decrypted`, `.env` ...), when `secrets.age` isn't an
     age file, or when `kitbag.toml` itself holds a literal secret, and warns when a git
     repository neither ignores nor tracks `secrets.age`. A tracked, age-encrypted `secrets.age`
     (the `secrets commit` opt-in) isn't nagged about, except for a warning when it is passphrase
     encrypted; a tracked file that isn't age-encrypted still fails.
   - **Left as follow-ups, so #9 can close:** an OS keychain or helper is a dated skip (2026-11-27): official docs name none
     kitbag should write. Gemini CLI's MCP OAuth token file is carried (#18, see
     [MCP OAuth tokens in the secrets bundle](#mcp-oauth-tokens-in-the-secrets-bundle)); other
     tools' tokens are not.
   - **Skipped on purpose (checked 2026-11-27).** kitbag doesn't read the OS keychain, shell
     profiles, or tools' undocumented auth files (Codex's `~/.codex/auth.json`, Hermes's `.env`
     and `auth.json`, the Claude Code and Cursor sign-ins): only values it already redacts from
     the configs it captures go in the bundle.

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
tool with `targets` / `disabled_on` does remove it there, or turn it off where the tool has its
own switch, if kitbag added it; see
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

## Applying on a remote host over SSH

For an SSH remote-development box, a devcontainer you can `ssh` into, or a cloud VM, whose
home has none of your setup, add `--remote` to `apply` or `diff`:

```sh
kitbag diff  ~/dotagents --remote you@devbox --to claude-code --exit-code
kitbag apply ~/dotagents --remote you@devbox --to claude-code
```

kitbag runs your own `ssh` (your `~/.ssh/config`, agent, jump hosts and known-hosts checks apply
as when you type `ssh you@devbox`; kitbag adds no options, stores no credentials and asks for
no password). It copies the profile to a private temporary directory on the host, runs the
kitbag there to show you the plan and diff in your terminal, asks you to confirm (`--yes` skips
that; `--dry-run` stops after the plan), applies, and removes the copy. `diff --remote` shows
what `apply` would change there and writes nothing; with `--exit-code` it exits 1 when there is
a difference. The apply is recorded in the **host's** state directory, so you revert it on the
host: `kitbag undo --remote you@devbox` (add an ID to undo a specific apply, or `--force`, as with a
local undo), or `ssh you@devbox kitbag undo` there. `undo --remote` runs the host's kitbag against
the host's own state directory (`~/.local/state/kitbag`, or its `XDG_STATE_HOME`); it uploads
nothing and sends no secret. It takes `--remote-kitbag PATH` and `--remote-download` like apply;
`--home` and `KITBAG_HOME` are never passed to the host.

Which kitbag runs on the host: `--remote-kitbag PATH` if you give one; otherwise `kitbag` on its
`PATH`, or in `~/.local/bin`, `~/.cargo/bin` (where `install.sh` and `cargo install` put it,
though a non-interactive ssh shell often lacks them on `PATH`) or `~/.local/share/kitbag/bin`
(where a `--remote-download` copy is kept, below). If there is none, kitbag stops and
says so. Only with `--remote-download` does it fetch this version's release (the same
`kitbag-<target>.tar.gz` that `install.sh` uses, for the host's OS and CPU) onto the host, check it
against the published `kitbag-<target>.sha256` and unpack and run it only if they match (a missing,
malformed or mismatching checksum stops it). Once it has been verified and has run
`--version` there, it is kept at `~/.local/share/kitbag/bin/kitbag` (mode 0755; nothing is written
there if any check fails), so a later `undo --remote` or `apply --remote` finds it without the flag;
the rest of the download and the copied profile are deleted. The download runs on the host and needs `curl` or `wget`,
`tar` and `sha256sum` or `shasum` there.

What is sent, and what is not:

- The profile's files, except `.git`, `.env` and `.env.*` files, `secrets.age` and symlinks. No resolved
  secret ever leaves this machine: the profile holds `${VAR}` references, the host's kitbag
  resolves them from the host's own environment (so set the variables there; `--secrets never`
  keeps them as references in the tools that can't read them), and a profile that holds what looks
  like a literal secret is refused (nothing is sent; replace it with a `${VAR}` reference). There is
  no option to send values.
- Only the host name given to `--remote` reaches `ssh`; one starting with `-` is refused.

**Secrets bundles on the host.** `--secrets-from` names a file on this machine, so it is refused
with `--remote`: nothing is decrypted here, and no passphrase, key or value is sent. Instead, let the
host's kitbag open a bundle **on the host**, with the host's own key or passphrase file:

```sh
# A secrets.age already on the host, opened there with an identity file there:
kitbag apply ~/dotagents --remote you@devbox --to claude-desktop \
  --remote-secrets-from '~/vault/secrets.age' --remote-secrets-identity '~/.config/kitbag/id.txt'
```

- `--remote-secrets-from HOST_PATH` is the path of a `secrets.age` on the host; the host's
  kitbag runs `apply --secrets-from` (or `diff --secrets-from`) on it, so it behaves exactly as
  there: values go only into tools that can't read env references, never into the plan, the diff
  or the host's undo records, and the plan and diff printed here show them masked.
- `--remote-secrets-identity HOST_PATH` (repeatable) or `--remote-secrets-passphrase-file
  HOST_PATH` says how the host opens it; one is required, as there is no terminal there to ask.
  These are paths on the host: kitbag never reads them here, and only the paths (shell-quoted;
  a quoted `~/` means the host's home) go over ssh. An identity file on the host is the
  recommended way.
- `--remote-include-secrets-age` also copies the profile's own `secrets.age` with the profile,
  and opens that copy on the host. Only an age-encrypted regular file is sent; the run is
  refused (nothing sent) when it is missing or not age-encrypted, when a plaintext secrets file
  sits next to it, or when the profile holds what looks like an age identity or private key.
  The passphrase or identity is never copied, so the host still needs its own
  `--remote-secrets-identity` or `--remote-secrets-passphrase-file`. Encrypt a bundle you send
  this way to the host's key (`capture --include secrets --secrets-recipient` or `secrets rotate
  --new-secrets-recipient`) rather than to a passphrase, and do not copy passphrase files to the
  host. The copy is removed with the rest of the temporary directory. Without this flag,
  `secrets.age` stays behind as before.

Project scope and workspaces: `--remote-project HOST_PATH` applies at project scope under a path
**on the host**, as `kitbag apply --project HOST_PATH` does there. `--project` and `--scope project`
name a path on this machine, so they are still refused with `--remote` (the message points here);
kitbag never sends your local working directory. `HOST_PATH` is quoted for the host's shell; `~/`
means the host's home, and a value that starts with `-` is refused before `ssh` runs.

```sh
# VS Code Remote-SSH: the folder you opened on the host
kitbag apply ~/dotagents --remote you@devbox --to claude-code --remote-project ~/src/myapp
# kitbag runs inside a default devcontainer you can ssh into: the workspace is
# bind-mounted at /workspaces/<folder-name> unless devcontainer.json sets
# workspaceFolder / workspaceMount, so pass whatever path it has there
kitbag apply ~/dotagents --remote you@devcontainer --to claude-code --remote-project /workspaces/myapp
```

kitbag does not detect or enter containers; `--remote` reaches whatever the `ssh` destination
reaches. `kitbag undo --remote` reverts a project-scope apply too (the host's state directory is
the same). The host's own default home and state directory are used.

Limits of this first version, each a follow-up: the source must be a profile or plugin directory
on this machine (not a git URL; clone it first) (#147), and not on Windows (#148). `--home` and
`KITBAG_HOME` only affect this machine and are never sent to the host. The host's history records
the temporary directory as the apply's source, so
`kitbag status SOURCE` and `sync` there can't compare against it (also #147). Sending secret
values is not offered: `--secrets-from` is refused with `--remote`, and a `secrets.age` (unless
`--remote-include-secrets-age`, encrypted only) and anything named like a plaintext secrets file
at the top of the profile are not copied to the host.

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
| `kitbag capture [--from IDS] [-o DIR] [--dry-run] [--force] [--prune] [--include hooks,memory,secrets] [--secrets-passphrase-file PATH \| --secrets-recipient KEY]` | Read the current setup into a profile (secrets redacted). Writes only into a new or empty directory. `--force` replaces an existing profile's files and leaves anything else in the directory alone; the profile's MCP servers (and rules, commands, skills and agents with `targets` / `disabled_on`) found in no tool are kept unless `--prune`. With several tools, one whose config can't be read is reported and the rest are captured |
| `kitbag disable ITEM [--on IDS] [PROFILE]`, `kitbag enable ...` | Turn an MCP server, rule, command, skill, agent or output style off or on for some tools (default: all) by editing its `targets` / `disabled_on` in a local profile; applies nothing |
| `kitbag diff <SOURCE> [--to IDS] [--exit-code] [--secrets-from PATH] [--remote HOST [--remote-secrets-from HOST_PATH] [--remote-project HOST_PATH]]` | Show exactly what `apply` would change; never writes. With `--remote`, on that host over ssh ([details](#applying-on-a-remote-host-over-ssh)) |
| `kitbag apply <SOURCE> [--to IDS] [--yes] [--dry-run] [--apply-hooks] [--secrets-from PATH] [--remote HOST [--remote-kitbag PATH \| --remote-download] [--remote-secrets-from HOST_PATH \| --remote-include-secrets-age] [--remote-project HOST_PATH]]` | Plan, confirm, write with backups; hooks only with `--apply-hooks` (and, from a git source, `--trust-source`). With `--remote`, copies the profile to the host over ssh and applies it there, with the host's own undo |
| `kitbag inspect <SOURCE> [--to IDS] [--json]` | What a profile or plugin holds, what in it runs commands, reads env vars, installs executables or connects to the network, and (for the tools in `--to`, default `detected`) which files `apply` would write; never writes or starts anything |
| `kitbag status [SOURCE] [--to IDS] [--exit-code [--strict]] [--json]` | Which managed instructions and rules differ from the profile, or changed since the last apply; never writes |
| `kitbag sync <SOURCE> [--to IDS] [--from ID [--adopt] \| --overwrite]` | Apply without overwriting instructions edited in a tool; pull an edit back with `--from`, and what is only in that tool with `--adopt` |
| `kitbag undo [ID] [--force] [--remote HOST [--remote-kitbag PATH \| --remote-download]]` | Revert the latest (or a specific) apply; with an ID, also an interrupted one. With `--remote`, undoes on the host, using its state directory; uploads nothing |
| `kitbag history [--json]` | List applies |
| `kitbag doctor [SOURCE]` | Check installed tools' configs, a profile, required env vars, risky settings, interrupted applies |
| `kitbag doctor [SOURCE] --mcp-tokens [--mcp-timeout SECS] [--allow-git-mcp [NAMES]] [--trust-source]` | Also start each local MCP server to list its tools, and estimate the tokens its tool definitions take up in each tool |
| `kitbag doctor --why [DIR] [--to IDS]` | Which instruction files each tool loads in `DIR` (default: the current directory), in order, which it skips and why, and known traps |
| `kitbag secrets list [FILE\|DIR] [--secrets-passphrase-file PATH \| --secrets-identity PATH]` | The variable names in an encrypted `secrets.age` (never values) |
| `kitbag secrets set NAME [FILE\|DIR] --value-file PATH [--secrets-passphrase-file PATH \| --secrets-identity PATH --secrets-recipient KEY]` | Add or replace one variable in `secrets.age` (created if missing); the value comes from a file or stdin (`-`), never the command line, and is never shown |
| `kitbag secrets commit [FILE\|DIR] [--allow-passphrase-commit]` | Opt in to keeping the encrypted `secrets.age` in git: un-ignore and `git add` it (never commits); recipients mode recommended, a passphrase bundle needs the flag and gets a warning |
| `kitbag secrets rotate [FILE\|DIR] [--secrets-passphrase-file PATH \| --secrets-identity PATH] (--new-secrets-passphrase-file PATH \| --new-secrets-recipient KEY)` | Encrypt `secrets.age` again to a new passphrase or new keys; the same variables and token stores, rewritten in place with no plaintext file |
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
    fn schemas(&self) -> &'static [SchemaSpec]; // optional: config schemas it reads and writes
    fn detect_schema(&self, env: &Env) -> Option<String>; // optional: the one in use, from files
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

No further candidates are named here; open an issue for the tool you miss. Issues labelled `good first issue` for new targets (currently #20–#28) are a good place to start; follow this section.

## Format notes and known limitations

- **Cursor's editor settings** have no documented file: its docs name no `settings.json` path or agent/model keys, and some are kept in a `state.vscdb` database with no documented layout, so kitbag leaves both alone. Only the Agent CLI's `cli-config.json` is carried (see the settings allow-list).
- **Cursor user rules** live in Cursor's settings UI, not in files, so kitbag can't write or read
  them at user scope. It says so in the plan. At project scope, rules go to `.cursor/rules/*.mdc`
  and instructions to `AGENTS.md`.
- **User-level Cursor commands** (`~/.cursor/commands`) are written as documented by the
  community. Some Cursor versions have been reported not to load them.
- **Codex custom prompts** are written for commands at user scope only. Codex now favours skills,
  and kitbag notes this. Codex loads `.codex/config.toml` only in trusted projects and reads the
  first 32 KiB of `AGENTS.md` by default. kitbag warns about both.
- **Commands not carried, by design** (official docs re-read 2026-11-27; the Commands column says `-` or `user`
  for these, and nothing here is left to write, so the commands work in #16 is complete).
  Hermes Agent ([skills](https://hermes-agent.nousresearch.com/docs/user-guide/features/skills)):
  "Every installed skill is automatically available as a slash command", and the only way to
  make a skill from something you already have is `/learn`; no commands folder and no mapping
  from commands or prompt files to skills. Its `quick_commands` in `config.yaml` map a slash
  command to a shell command or another slash command and, in its docs' words, "string-only
  prompt shortcuts are not supported". Skills are already carried, but profile `commands/` are
  not rewritten as Hermes skills, since no mapping is documented (Amp, Antigravity and Copilot's
  user scope are the ones whose docs describe one). Codex
  ([custom prompts](https://learn.chatgpt.com/docs/custom-prompts), formerly
  developers.openai.com/codex/custom-prompts): "Custom prompts are deprecated. Use skills for
  reusable instructions"; prompts live in `~/.codex/prompts` (top-level Markdown files only) and
  are "not shared through your repository", so there is no project path, and no migration to
  skills is described; the user path stays written. Claude Desktop (the chat app) has no
  standalone commands folder: the [Desktop app docs](https://code.claude.com/docs/en/desktop)
  list slash commands as built-ins, skills and plugin skills only, and official docs
  (claude.com/docs/third-party/claude-desktop/extensions) place slash-command definitions in a
  plugin's `commands/` directory (plugins are #12; the only plugin folder those docs name is an
  administrator-owned organization directory, which kitbag doesn't write). Copilot's user-level
  prompt files live in VS Code's profile storage, with no path the docs name; user-level commands
  are written as skills instead (see the Copilot note). Continue's
  [prompts guide](https://docs.continue.dev/customize/deep-dives/prompts) and
  [config reference](https://docs.continue.dev/reference) (`invokable: true` in a markdown
  file, `prompts` in `config.yaml`) name no prompts folder, so only the `config.yaml` list is
  written. Zed: its [text threads](https://zed.dev/docs/ai/text-threads) and
  [extensions](https://zed.dev/docs/extensions/slash-commands) docs have slash commands only from
  an extension (`extension.toml` plus code), not from a file kitbag could write, and the AI
  overview names no commands folder (a user-defined commands directory is only a feature request,
  zed-industries/zed discussion 57943). Antigravity workflows: see its note.
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
  not written: Google has deprecated them in favour of skills, and its docs list two different
  global directories for them (the migration guide scans `global_workflows/` and `workflows/`).
  Its workflows-to-skills migration guide (docs checked 2026-11-27) turns a workflow into
  `skills/<name>/SKILL.md` (the body kept, `name` and `description` in the frontmatter, run as
  `/<name>`), so a profile's commands are written that way, to the skill folders above. Hooks are
  carried (see Hooks) and so are agents (see Agents); plugins aren't. kitbag leaves
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
  and commands defined inside `opencode.json` and opencode 1's `mode/` agents are not carried
  (hooks exist only as plugins, see Hooks). Plugins are carried by reference, see
  [opencode plugins](#opencode-plugins-by-reference-pluginsopencode). That is everything
  opencode's docs describe for MCP servers, instructions, rules, commands, skills and agents. opencode also reads
  `.claude/skills` and `~/.claude/skills` (and `.agents/skills`, `~/.agents/skills`), so with
  Claude Code targeted too a skill is visible from both folders.
- **Grok Build** also reads Claude Code's files (`CLAUDE.md`, `.claude/rules`, `.claude/skills`,
  `.claude/commands`, `.mcp.json`) and `~/.agents/skills` by default. The plan notes when the
  same instructions would load twice, and that Codex's folded rules in a shared `AGENTS.md` sit
  next to `.grok/rules/`. Commands are Claude Code style markdown; Grok counts positional
  arguments from `$0`, so kitbag warns about commands that use `$1`-style placeholders. Grok
  Build agent definitions (`.grok/agents/`) are not carried: docs.x.ai names the folder but gives
  no file format (see [Agents](#agents-agentsnamemd)). Personas are not carried either: the docs
  name `personas/*.toml` but no format (see [Output styles](#output-styles-output-stylesnamemd)).
  Plugins and settings are not carried yet; hooks are (see Hooks).
- **Windsurf** (now also shipped as Devin Desktop) keeps MCP servers in
  `~/.codeium/windsurf/mcp_config.json` and global rules in
  `~/.codeium/windsurf/memories/global_rules.md`; kitbag writes those, and nothing at project
  scope except `AGENTS.md`, since Windsurf's docs describe no project MCP file. Devin Desktop's
  own files (`~/.config/devin/mcp_config.json`, `.devin/`) are not written. A server marked
  `disabled` in the profile is not written (the docs list no such key), though capture reads
  `disabled: true`, and Windsurf-only keys on an entry are kept. SSE servers are skipped, since
  the docs don't say how the file marks one. Windsurf's docs have limited `global_rules.md` to
  6,000 characters, and kitbag warns when the instructions are longer.
  Rules, commands and skills are files of their own. A rule is `.windsurf/rules/<name>.md` with
  a `trigger` in its frontmatter: `always_on`, `glob` with `globs` (several patterns are written
  as one comma-separated string, since the docs show a single one) or, for an on-demand rule,
  `model_decision` with its description. Capture reads those three; a `manual` rule (the profile
  has no such mode), a rule with no `trigger` (the docs don't say when it applies) and a `glob`
  rule with no `globs` are left behind and named in the capture notes, and so are the files in
  `.windsurf/` and in `~/.codeium/windsurf/memories/` and `global_workflows/` that capture does not
  read, and anything under `.devin/`. Commands are Windsurf's workflows, `/<name>`, in
  `~/.codeium/windsurf/global_workflows/` and `.windsurf/workflows/`: the prompt as the body, with
  the description as `description` in frontmatter when the command has one (the docs name a
  description but give no syntax, so that form is unverified), and a warning when a file is over
  the docs' 12,000-character limit. Skills go to `.windsurf/skills/<name>/` (Windsurf's docs call
  it the legacy workspace folder, still read) with their bundled files. Windsurf's docs, now at
  docs.devin.ai, name no user-level rules or skills folder of Windsurf's own, only Devin Desktop's
  `~/.devin/rules/` and `~/.config/devin/skills/`, so rules and skills are not carried at user
  scope. Windsurf has no agent definition files (see [Agents](#agents-agentsnamemd)), and hooks are not written: Windsurf's hooks page now
  redirects to Devin Desktop's docs (see Hooks, #20).
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
  `AGENT.md`, `AGENTS.md`, `CLAUDE.md`, `GEMINI.md` (Zed's docs, checked 2026-11-27 at
  zed.dev/docs/ai/instructions), so kitbag writes its block there (counting
  `.github/copilot-instructions.md` when Copilot is a target too), and capture reads that file
  only, naming the others it ignores. The Rules Library lives in Zed's own database and is being
  replaced by skills and the personal `AGENTS.md`; kitbag doesn't read or write it, and capture
  says so. Skills are carried (Zed's docs, checked 2026-11-27 at zed.dev/docs/ai/skills): a
  skill is a folder with a `SKILL.md` (`name` and `description`, both required, and optionally
  `disable-model-invocation`) and any files it brings, in `~/.agents/skills/` and a project's
  `.agents/skills/`, the only folders those docs name, so that is where kitbag writes and
  capture reads (other tools' skill folders, `.claude/skills` among them, are not read by Zed
  and not touched). Zed loads only a name of lowercase letters, digits and single hyphens, up to
  64, that matches the folder, so any other skill name is skipped with a warning; a description
  over 1,024 characters is warned about. Zed loads a project's skills only in a trusted
  worktree, asks you before the agent first runs a skill you added, and skips a skill folder
  nested below the skills folder; kitbag leaves all of that to Zed. Amp also writes a project's
  skills to `.agents/skills/`, so applying both leaves one copy, and kitbag warns when a skill
  that declares its own MCP servers (which kitbag leaves out for Amp) would differ between the
  two. Zed's docs name no folder of its own for skills, nor a rules file or a commands folder
  (the Rules Library is the only place they live), so rules and commands are not carried; agents
  (profiles live in `settings.json`, see [Agents](#agents-agentsnamemd)) and Zed's docs describe no hooks (see Hooks). Settings are carried at user scope, for the `agent` keys in the allow-list above (the rest of `settings.json`, such as `language_models`, is left as it is). The skills path for Windows is
  `%USERPROFILE%\.agents\skills` here, which the docs write as `~/.agents/skills/`. The config directory follows Zed's source (`XDG_CONFIG_HOME` on Linux, `~/.config/zed` on
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
  or written (capture warns when it finds them), and Hermes's sessions, profiles and
  skill bundles are not carried, and its memories only with `--include memory` (capture says which of the others it found and left). Skills
  (docs: hermes-agent.nousresearch.com/docs/user-guide/features/skills, checked 2026-11-27)
  go to `$HERMES_HOME/skills/<name>/` ("the primary directory and source of truth") and, in a
  project, to `.hermes/skills/<name>/`, Hermes's own project folder, never to `.agents/skills/`,
  which Amp and Zed write to (the plan notes a skill of the same name there, since Hermes then
  refuses both). Hermes finds skills at any depth and groups them in category folders
  (`skills/mlops/axolotl/`), so capture looks in those too, and apply updates a skill where it
  already is and otherwise writes it directly in `skills/`. The skills Hermes put there itself,
  named in `skills/.bundled_manifest` or in the `installed` list of `skills/.hub/lock.json`, are
  not captured and not overwritten (a profile skill of the same name is skipped with a warning).
  Hermes loads project skills only after you run `hermes skills trust` in the repository, and
  the plan says so. `skills.external_dirs` in `config.yaml` is not captured or edited. Hermes's
  docs name no rules folder and no commands folder (reusable prompts are skills, and every
  skill is already a slash command), so rules and commands are skipped for Hermes and the plan
  says so. Its shell hooks (the `hooks:` block of `config.yaml`) are carried
  with `--include hooks` / `--apply-hooks` (see Hooks above).
- **Amp** keeps MCP servers under the flat key `amp.mcpServers` in `~/.config/amp/settings.json`
  or `settings.jsonc` (the same path under `%USERPROFILE%` on Windows; Amp's docs name no
  variable that moves it) and in a workspace's `.amp/settings.json` or `settings.jsonc`. kitbag
  edits the file in place, so comments, trailing commas, your other `amp.*` settings, your own
  servers and keys of yours on an entry (`includeTools`, ...) stay. If both spellings exist,
  `settings.json` is used and the plan says the docs don't say which wins. Amp expands `${VAR}`
  itself, so references are written as they are and no secret is written to disk. Capture reads
  one scope at a time and doesn't merge them; when a workspace and your user settings define the
  same server name, Amp uses the workspace's, and the plan says so. **Amp asks you to approve a
  workspace's MCP servers before it runs them** (`amp mcp approve <name>`, `amp mcp doctor`);
  kitbag never approves one and writes no `amp.mcpTrustedServers` or `amp.mcpPermissions`.
  A server that uses `${amp:id-token}` (an Amp orb sends its Amp ID token to the server) is neither
  captured nor written. `cwd` is dropped with a warning, a server the profile disables is not
  written (Amp documents no flag), and an SSE server is written as a plain `url` with a warning,
  since Amp has no transport key. Servers defined on ampcode.com (`amp mcp remote`), servers
  bundled in a skill's `mcp.json`, and `oauth` / `includeTools` on an entry are not captured, and
  capture says so; the display, git trailer and `amp.tools.disable` settings in the allow-list above are carried at user scope, while `amp.mcpPermissions`, `amp.dangerouslyAllowAll`, `amp.url` and the other settings are not, and Amp's hooks exist only as plugins (see Hooks, #12). Only the settings
  file at the project root is used; Amp also searches parent directories. Instructions go to
  `~/.config/amp/AGENTS.md` (Amp also reads `~/.config/AGENTS.md`, which kitbag leaves alone and
  names), and in a project to the file Amp reads: `AGENTS.md`, else `AGENT.md`, else `CLAUDE.md`
  (so no `AGENTS.md` is created that would hide them from Amp), as a managed block shared with
  the other tools using that file. Amp has no rules directory (it uses `@`-mentions in `AGENTS.md`
  and files with `globs` frontmatter), so rules are not written for it.
  **Skills** are `<name>/SKILL.md` folders with their bundled files. kitbag writes them to
  `~/.config/agents/skills/<name>/` (where `amp skill add --global` installs; under `--home`
  like the rest of `~/.config/amp`) and, in a project, `.agents/skills/<name>/`, and never to the
  `.claude/skills` folders Amp also reads, which are Claude Code's. Capture reads, in Amp's order
  (the first skill of a name wins and the plan names the one it skipped), `~/.config/agents/skills`,
  `~/.agents/skills`, `~/.config/amp/skills` and `~/.claude/skills` for the user, and
  `.agents/skills` and `.claude/skills` in the project (not parent directories), leaving out the
  Claude-compatible folders when `amp.skills.disableClaudeCodeSkills` is set and the first two
  when `amp.skills.disableGlobalAgentsSkills` is. Only skills directly under those folders are
  read; `amp.skills.path` folders, `~/.claude/plugins/cache` and skill repositories that are not
  plain folders are not. Amp reads user skills before a project's, so a user skill of the same
  name hides a project one, and the plan says so. A skill's own MCP servers (`mcp.json` next to
  `SKILL.md`, or an `mcpServers` key in its frontmatter) are not carried: Amp starts them when it
  finds the skill and its docs describe no approval for them, so capture leaves `mcp.json` out,
  apply writes neither it nor the `mcpServers` key, and both warn; put the servers in the
  profile's `mcp_servers` instead. **Commands:** Amp removed custom commands in favour of skills
  ([announcement](https://ampcode.com/news/slashing-custom-commands)), so kitbag follows its
  migration and writes a profile's commands as skills (`code-review.md` becomes
  `code-review/SKILL.md` with `name` and `description`), never into a commands folder. A command's
  argument hint, other frontmatter and `$ARGUMENTS` / `$1` substitution have no counterpart and
  are listed in the plan; a command that shares a name with a skill is skipped with a warning.
  Capture brings such a skill back as a skill, not a command.
- **Cline** keeps MCP servers under `mcpServers` in `cline_mcp_settings.json`, in
  `~/.cline/data/settings/` (the CLI, the IDE extensions and the SDK share it; `CLINE_MCP_SETTINGS_PATH`,
  `CLINE_DATA_DIR` and `CLINE_DIR` move it, in that order of precedence). The older location in
  the editor's `globalStorage/saoudrizwan.claude-dev/settings/` is not read or written. Local servers
  get `command`, `args` and `env`; remote ones `url`, `headers` and `type`: `streamableHttp`, or `sse`,
  which Cline's docs also give as the default for a URL with no `type`, so capture reads it that way.
  A server marked `disabled` is written with `disabled: true` (read back by capture), and Cline's own
  keys on an entry (`autoApprove`, `timeout` ...) are kept while the entry runs the same command or URL.
  Cline documents no env-reference syntax, so references are resolved when applied (`--secrets never`
  keeps `${VAR}` placeholders, which Cline will not expand) and the file is then 0600; `cwd` is
  dropped with a warning. Nothing else in the data directory is read: it also holds the provider
  settings, with API keys. Instructions go in a managed block in `~/.cline/rules/kitbag.md`, one of
  the global rules directories Cline reads, and to `AGENTS.md` in a project (Cline's docs list it);
  only instructions go there, so `kitbag` can't be a rule name at user scope. Rules are files of
  their own: `<name>.md` in `~/.cline/rules/` and `.cline/rules/`. A rule with globs gets a `paths`
  list in YAML frontmatter, the one conditional Cline documents; any other rule is plain markdown, and
  an on-demand rule (Cline has none) is written always-on with a first line saying when to use it.
  Capture reads the `*.md` files directly in `~/.cline/rules/`, `~/Documents/Cline/Rules/`,
  `~/Cline/Rules/`, `.cline/rules/` and `.clinerules/` (a `paths: []` rule, which never applies, is
  skipped) and lists what it leaves: nested or non-markdown files, and `.clinerules/hooks/`. It does not
  take `.cursorrules`, `.windsurfrules` or `AGENTS.md`, which Cline also detects but which belong to
  other tools. A rule that already exists in one of the other folders is updated there instead of
  copied. Commands are Cline's workflows: `<name>.md` in `~/.cline/workflows/` and `.cline/workflows/`
  (read also from `~/Documents/Cline/Workflows/` and `.clinerules/workflows/`), plain markdown with
  the prompt as the body. Cline's docs name those folders but describe no file format, so kitbag writes
  no frontmatter and a command's description and argument hint are not carried. Skills are
  `<name>/SKILL.md` directories in `~/.cline/skills/` and `.cline/skills/`, the only folders kitbag
  writes; capture also reads `.clinerules/skills/` and `.agents/skills/` (`~/.agents/skills/` at user
  scope), which belong to other tools and are never written, and the plan says when a skill of the
  same name is in one of them. The name in `SKILL.md` must match the directory name. Rules that an
  older kitbag folded into `kitbag.md` are removed from it on apply (user scope) and still read by
  capture. Agents are not carried (Cline's docs name the folders but no file format, see [Agents](#agents-agentsnamemd)), and Cline's hooks are not carried (see Hooks).
- **Roo Code** (the `RooVeterinaryInc.roo-cline` extension) keeps MCP servers under `mcpServers` in
  `mcp_settings.json` in the extension's global storage, and in a project's `.roo/mcp.json`, which
  wins for a server of the same name. Roo's docs don't give the global path; the file name and the
  `settings` folder come from its source, and the folder is under the editor's user data directory
  (see the table). Roo can run in other editors, so kitbag takes the first of VS Code, VS Code
  Insiders, VSCodium and Cursor whose folder for the extension exists, VS Code when none does (the
  plan warns), and capture says when more than one has it; the `roo-cline.customStoragePath` setting
  is not read, so a user who set it has to copy the file. Under `--home` the editor directories are
  looked for under that home. Local servers get `command`, `args`, `env` and `cwd`; remote ones `url`,
  `headers` and the `type` Roo requires, `streamable-http` or `sse`; a URL with no `type` is skipped
  by capture, since Roo rejects it. `disabled: true` is written for a disabled server; `alwaysAllow`,
  `timeout`, `watchPaths` and `disabledTools` on an entry are kept. References are written as
  `${env:VAR}`, which Roo expands (its docs show it in `args`; its source expands the whole entry),
  so no secret is written. Instructions go in a managed block in `~/.roo/rules/kitbag.md` (Roo reads
  every file in that directory) and to `AGENTS.md` in a project (Roo reads it by default); only
  instructions go there, so `kitbag` can't be a rule name at user scope. Rules are files of their own:
  `<name>.md` in `~/.roo/rules/` and `.roo/rules/`, which Roo reads recursively (capture takes every
  `*.md` under them, a sub-folder's name joined to the file's). Roo's docs describe no frontmatter for
  rules, so none is written: a rule with globs, or an on-demand one, loads every time, with a first
  line saying when it applies (`_Applies when working with files matching: ..._`), which capture reads
  back into the rule's globs. Roo uses a project's `.roorules` only while `.roo/rules/` is empty, so
  kitbag never creates an empty `.roo/rules/`, and warns when its rule files would hide a `.roorules`.
  Mode-specific rules (`rules-<mode>/`, `.roorules-<mode>`) and `.roorules` are not captured or
  written (kitbag has no notion of Roo's modes), and capture lists them. Commands are
  `<name>.md` in `~/.roo/commands/` and `.roo/commands/` with `description`, `argument-hint` and
  `mode` frontmatter, the three keys Roo documents; the file name is the command name. A `mode` is
  Roo's own key (it names a Roo mode), so capture keeps it with the command and the plan says when it
  is written. Skills are `<name>/SKILL.md` directories in `~/.roo/skills/` and `.roo/skills/`, the
  only folders kitbag writes; capture also reads the cross-agent `~/.agents/skills/` and
  `.agents/skills/`, never written, and lists mode-specific `skills-<mode>/` folders without reading
  them. Roo needs a skill name of 1-64 lowercase letters, digits and single hyphens that matches the
  directory, so a skill with another name is skipped with a warning. Rules that an older kitbag folded
  into `kitbag.md` are removed from it on apply (user scope) and still read by capture. Custom
  modes (Roo's docs call them modes, not agents) and settings are not carried, and Roo's docs describe no hooks (see Hooks). A project's servers run what `.roo/mcp.json` says:
  kitbag writes no `alwaysAllow`, so Roo's own tool approval applies.
- **Continue** (VS Code and JetBrains) keeps user settings in `~/.continue/config.yaml`
  (`%USERPROFILE%\.continue\config.yaml` on Windows). kitbag edits it in place: comments, your
  models, prompts and other keys stay, and only the `mcpServers` entries it writes (and one
  `rules` entry, `kitbag-instructions`) change; a layout it can't edit in place (a flow list, a
  tab-indented one) is reported and left alone. Continue's server names are free text
  (`Browser search`); capture turns one into a kitbag name (`browser-search`), and apply finds
  the entry again under its own name, so it is not added twice. A local server takes `command`,
  `args`, `env` and `cwd`; a remote one `type: sse` or `type: streamable-http`, `url` and
  headers under `requestOptions` (the docs give a server's `requestOptions` the format of a
  model's). Keys kitbag doesn't manage (`connectionTimeout`, other `requestOptions`) are kept
  while the entry runs the same thing. Continue has no way to switch a server off in a file,
  so a server the profile disables is not written; servers from a Hub block (`uses:`) are not
  captured. Instructions: capture reads the plain-text entries of `rules` and the
  `kitbag-instructions` entry; apply writes only the paragraphs the plain-text entries don't
  already hold. The deprecated `config.json` is never read or written, and kitbag won't create
  a `config.yaml` next to one without it, because Continue would then stop loading the JSON
  (and the models in it). In a project, Continue's block files in `.continue/mcpServers/`
  (YAML, or the Claude Desktop / Cursor / Cline JSON shape) are captured; a server already in a
  YAML file there is edited in that file, new ones go to `kitbag.yaml`, and one that lives in a
  JSON file is left alone with a note. Rules are `.continue/rules/<name>.md` (`name`,
  `description`, `globs`, `alwaysApply`; `regex` isn't carried), and the instructions a managed
  block in `.continue/rules/kitbag-instructions.md`. Continue's docs describe no rules folder in
  `~/.continue`, so rules are written at project scope only. Commands are Continue's prompts, at
  user scope only: capture reads the entries of `prompts` in `config.yaml` that are written out
  there (`name`, `description`, `prompt`; a name with spaces becomes a kitbag name, as for
  servers) as commands, and apply writes each command as such an entry, the form Continue's
  reference shows, replacing an entry of the same name (keys of yours on it, such as
  `invokable`, stay) and leaving the others, including Hub prompts (`uses:`), alone. Continue's
  docs describe no prompts folder and no argument placeholder, so a project's `.continue/prompts`
  is not read or written (capture says so), `$ARGUMENTS` is written as it is (with a note), and
  the argument hint and other command frontmatter have no place and are dropped. A prompt kitbag
  wrote stays in `config.yaml` if the profile later disables the command for Continue (remove it
  by hand); undo restores the file. `kitbag status` tracks the `kitbag-instructions` entry like
  any managed instructions block: `tool edited`, `profile changed` and `both changed` are
  reported, and `sync --from continue` pulls an edit to it back into the profile. Models,
  context providers, docs, Hub blocks, YAML rule files and the other things under `.continue/`
  are not carried; Continue's docs mention no skills, so they are not carried either.
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
  (`.github/prompts/<name>.prompt.md`) at project scope: VS Code keeps user prompt files
  in its own profile storage, which its docs give no path for, and Copilot CLI's docs don't
  list prompt files among what it reads. At user scope a command is written as a skill,
  `$COPILOT_HOME/skills/<name>/SKILL.md`, as VS Code's migration guide converts prompt files
  (docs checked 2026-11-27; the Local harness that loads prompt files is deprecated there for
  February 2027): `name`, `description`, `argument-hint` and `disable-model-invocation: true`
  (it only runs when you type `/<name>`); `agent`, `model` and `tools` are not carried, as the
  guide says. A prompt file keeps `description`, `argument-hint` and VS Code's
  `name`, `agent`, `model` and `tools`; other command keys (`allowed-tools` ...) are left out with
  a note, and `$ARGUMENTS` is written as is with a warning, since VS Code documents
  `${input:NAME}` instead. Skills go to `$COPILOT_HOME/skills/` and `.github/skills/`, which
  Copilot CLI and VS Code both read (they also read `.claude/skills` and `.agents/skills`, which
  kitbag doesn't write for Copilot, as `.github/skills` and `~/.copilot/skills` already reach both); `name` and `description` are filled in when `SKILL.md`
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
- **Not carried yet:** hooks only for Claude Code, and only with `--include hooks` (see
  [Hooks](#hooks-hooks-in-kitbagtoml-and-hooks)). Capture lists what it found of these and
  leaves them out. Output styles are carried for Claude Code and, as personalities, Hermes Agent (see
  [Output styles](#output-styles-output-stylesnamemd)), and `outputStyle` is kept even when it
  names a style kitbag has no file for.
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
