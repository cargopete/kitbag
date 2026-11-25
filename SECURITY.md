# Security

kitbag reads and writes the configuration of your AI coding tools, which often holds
credentials and controls what an agent may run. Please treat it accordingly.

## Status (v0.1, early release)

- **Secret redaction on `capture` is best-effort.** kitbag replaces values it recognises as
  secrets with `${ENV_VAR}` references: well-known token formats, values of secret-named
  fields, auth headers, secret-looking arguments and URL passwords. Some formats are known
  to slip through today (for example unusual variable names, short values, `${VAR:-default}`
  fallbacks, tokens inside URL paths or query strings, and frontmatter of rules and
  commands). **Review a captured profile before you share or commit it.**
- **Apply profiles only from sources you trust.** A profile can add MCP servers (which run
  commands or connect to remote endpoints), permission rules, and skills or commands whose
  frontmatter can carry hooks. kitbag drops the most permissive settings from git sources
  unless you pass `--allow-risky-settings`, but that filter is not complete yet. Use
  `kitbag diff <source>` and read the plan before applying.
- `apply` backs up every file it changes and `kitbag undo` restores them.

Known issues are tracked with the
[`security` label](https://github.com/cargopete/kitbag/issues?q=is%3Aissue+label%3Asecurity).

## Reporting a problem

If you find a way kitbag leaks a secret or applies something it shouldn't, please report it
through GitHub's **private vulnerability reporting** (Security tab, "Report a vulnerability")
if it is enabled for the repository, or open an issue **without any real secret values** and
with a minimal reproduction using fake ones.
