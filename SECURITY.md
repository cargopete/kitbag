# Security

kitbag reads and writes the configuration of your AI coding tools, which often holds
credentials and controls what an agent may run. Please treat it accordingly.

## Status (v0.1, early release)

- **Secret redaction on `capture` is best-effort.** kitbag replaces values it recognises as
  secrets with `${ENV_VAR}` references: well-known token formats, values of secret-named
  fields, auth headers, secret-looking arguments, URL passwords and URL tokens, in MCP
  config, frontmatter, permission rules and free text. It is still a heuristic and can miss
  a format it doesn't know. **Review a captured profile before you share or commit it.**
- **Apply profiles only from sources you trust.** A profile can add MCP servers (which run
  commands or connect to remote endpoints). From git sources, kitbag doesn't add or change
  MCP servers unless you name them with `--allow-git-mcp NAME[,NAME]` (or pass the flag
  without names for all); the plan lists each one it held back with its command and
  arguments or URL and the names of its env vars and headers. It also drops permission allow
  rules and other permissive settings, skips commands and skills that run shell snippets,
  strips `hooks` and `allowed-tools` frontmatter, and doesn't resolve your env vars into the
  profile's servers, unless you pass `--allow-risky-settings` (`--trust-source`), which
  also allows every MCP server. Every plan lists the MCP servers it adds or changes. Use
  `kitbag diff <source>` and read the plan before applying.
- `apply` backs up every file it changes and `kitbag undo` restores them.

Known issues are tracked with the
[`security` label](https://github.com/cargopete/kitbag/issues?q=is%3Aissue+label%3Asecurity).

## Reporting a problem

If you find a way kitbag leaks a secret or applies something it shouldn't, please report it
through GitHub's **private vulnerability reporting** (Security tab, "Report a vulnerability")
if it is enabled for the repository, or open an issue **without any real secret values** and
with a minimal reproduction using fake ones.
