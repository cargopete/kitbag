//! Command-line interface.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::io::{BufRead, IsTerminal, Write};
use std::path::{Path, PathBuf};

use anstream::{eprintln, print, println};
use anyhow::{Context, Result, bail};
use clap::{Args, Parser, Subcommand};
use serde_json::json;

use crate::adapters::{
    self, Adapter, ApplyOptions, Env, Feature, LOCATION_VARS, Os, Scope, SecretsMode,
};
use crate::edit::{json as jsonedit, toml as tomledit};
use crate::engine::{self, Plan};
use crate::mcp_tokens;
use crate::model::{McpServer, Profile, Transport, slugify};
use crate::plan_fs::read_text_opt;
use crate::policy;
use crate::profile_io::{self, MANIFEST};
use crate::secrets::{RefStyle, RenderState, render_refs};
use crate::source;
use crate::txn::{Status, Store};
use crate::ui::{self, BOLD, CYAN, DIM, GREEN, RED, YELLOW, paint, tildify, visible, visible_line};
use crate::why;

const ABOUT: &str = "Pack your AI agent setup once, unpack it into any coding tool.";

const AFTER_HELP: &str = "\
Sources (for apply / diff / inspect / status / sync / doctor) are a profile
directory, a plugin directory (rules/<name>.md at its root, no kitbag.toml), or
a git URL to either, optionally pinned to a branch, tag or commit with @ or #:
  ./my-profile   ./my-plugin   github:you/dotagents   gh:you/dotfiles/agents#main
  github:you/dotagents@v1.2   https://github.com/you/dotagents.git@3f9c2e1
  git@github.com:you/dotagents.git

Examples:
  kitbag capture --from claude-code,cursor -o ~/dotagents
  kitbag inspect github:you/dotagents@v1.2 --to all
  kitbag diff ~/dotagents --to codex
  kitbag apply github:you/dotagents --to all
  kitbag apply ~/dotagents --project . --to cursor,codex
  kitbag apply ~/dotagents --to gemini-cli,antigravity
  kitbag status ~/dotagents
  kitbag sync ~/dotagents --from claude-code
  kitbag doctor --why . --to claude,codex
  kitbag undo";

#[derive(Parser, Debug)]
#[command(name = "kitbag", version, about = ABOUT, after_help = AFTER_HELP, propagate_version = true)]
pub struct Cli {
    /// Treat DIR as the home directory, and ignore CODEX_HOME / CLAUDE_CONFIG_DIR /
    /// COPILOT_HOME / HERMES_HOME / XDG_CONFIG_HOME / APPDATA (for sandboxes and testing). State
    /// goes under DIR too unless KITBAG_STATE_DIR is set
    #[arg(long, global = true, value_name = "DIR", env = "KITBAG_HOME")]
    home: Option<PathBuf>,

    #[command(subcommand)]
    command: Cmd,
}

#[derive(Subcommand, Debug)]
enum Cmd {
    /// Create a starter profile
    Init {
        /// Directory to create
        #[arg(default_value = "kitbag-profile")]
        dir: PathBuf,
        /// Profile name (default: the directory name)
        #[arg(long)]
        name: Option<String>,
        /// Replace an existing profile in DIR
        #[arg(long)]
        force: bool,
    },
    /// Capture your current setup into a profile (secrets are redacted)
    Capture {
        /// Targets to read: ids (comma-separated: claude-code (alias: claude), cursor,
        /// codex, gemini-cli, claude-desktop, antigravity, opencode, copilot, grok-build, windsurf, zed, hermes, amp, cline, roo (alias: roo-code), continue
        /// (alias: grok)), 'all' or 'detected'
        #[arg(long, value_name = "IDS", default_value = "detected")]
        from: String,
        #[command(flatten)]
        place: Place,
        /// Where to write the profile
        #[arg(short, long, value_name = "DIR", default_value = "kitbag-profile")]
        out: PathBuf,
        /// Profile name (default: the output directory name)
        #[arg(long)]
        name: Option<String>,
        /// Replace an existing profile in the output directory
        #[arg(long)]
        force: bool,
        /// With --force, drop the existing profile's MCP servers (and its rules, commands,
        /// skills and agents with targets / disabled_on) that weren't found in any tool (by
        /// default they are kept)
        #[arg(long)]
        prune: bool,
        /// Show what would be captured without writing anything
        #[arg(long)]
        dry_run: bool,
        /// Also capture what is left out by default. 'hooks': the hooks of the
        /// tools that have them (Claude Code, Cursor, Codex, Gemini CLI, Grok
        /// Build, Antigravity, Copilot, Hermes), and the scripts they run
        /// (redacted like everything else). Hooks run commands by themselves,
        /// so applying them needs --apply-hooks
        #[arg(long, value_enum, value_delimiter = ',', value_name = "WHAT")]
        include: Vec<Include>,
    },
    /// Apply a profile: shows the plan, then asks before writing anything
    Apply {
        /// Profile or plugin directory, or a git URL to one
        source: String,
        #[command(flatten)]
        targets: TargetArgs,
        /// Write without asking
        #[arg(short, long)]
        yes: bool,
        /// Show the full plan and diff, write nothing
        #[arg(long)]
        dry_run: bool,
        #[command(flatten)]
        apply: ApplyArgs,
    },
    /// Show exactly what 'apply' would change (never writes)
    Diff {
        /// Profile or plugin directory, or a git URL to one
        source: String,
        #[command(flatten)]
        targets: TargetArgs,
        #[command(flatten)]
        apply: ApplyArgs,
        /// Exit with status 1 when there are differences (for CI drift checks)
        #[arg(long)]
        exit_code: bool,
    },
    /// Show what a profile or plugin holds, what in it runs commands, reads
    /// env vars or connects to the network, and (for the tools in --to) which
    /// files 'apply' would write. Never writes or starts anything
    Inspect {
        /// Profile or plugin directory, or a git URL to one
        source: String,
        #[command(flatten)]
        targets: TargetArgs,
        #[command(flatten)]
        apply: ApplyArgs,
        /// Machine-readable output
        #[arg(long)]
        json: bool,
    },
    /// Show where the instructions and rules in each tool differ from the
    /// profile, or (without PROFILE) changed since the last apply. Never writes
    Status {
        /// Profile or plugin directory, or a git URL to one, to compare with
        source: Option<String>,
        #[command(flatten)]
        args: StatusArgs,
    },
    /// Apply a profile without overwriting instructions edited in the tool
    /// since the last apply (see 'kitbag status')
    Sync {
        /// Profile or plugin directory, or a git URL to one
        source: String,
        #[command(flatten)]
        targets: TargetArgs,
        #[command(flatten)]
        sync: SyncArgs,
        #[command(flatten)]
        apply: ApplyArgs,
    },
    /// Turn an MCP server, rule, command, skill, agent or output style off for one tool, or
    /// all of them, in a profile on this machine (sets 'disabled_on' /
    /// 'targets'; applies nothing, run 'kitbag apply' after)
    #[command(after_help = TOGGLE_HELP)]
    Disable {
        #[command(flatten)]
        args: ToggleArgs,
    },
    /// Turn an MCP server, rule, command, skill, agent or output style back on for one
    /// tool, or all of them, in a profile on this machine (applies nothing,
    /// run 'kitbag apply' after)
    #[command(after_help = TOGGLE_HELP)]
    Enable {
        #[command(flatten)]
        args: ToggleArgs,
    },
    /// Revert the most recent apply (or the one with this id)
    Undo {
        /// Transaction id or unique prefix (see 'kitbag history')
        id: Option<String>,
        /// Undo even if files were edited since, or newer applies exist
        #[arg(long)]
        force: bool,
    },
    /// List previous applies that can be undone
    History {
        /// Machine-readable output
        #[arg(long)]
        json: bool,
    },
    /// Check a profile, your environment and the installed tools
    Doctor {
        /// Profile or plugin directory, or a git URL to one, to check
        source: Option<String>,
        #[command(flatten)]
        targets: TargetArgs,
        /// Instead, show which instruction files each tool loads for DIR
        /// (default: the current directory), in order, which files it skips
        /// and why, and known traps. Shows every tool that reads instruction
        /// files unless --to names some
        #[arg(
            long,
            value_name = "DIR",
            num_args = 0..=1,
            default_missing_value = ".",
            conflicts_with_all = ["source", "scope", "project", "path", "allow_insecure_source", "require_pin", "mcp_tokens"]
        )]
        why: Option<PathBuf>,
        /// Also estimate the tokens each MCP server's tool definitions take up,
        /// per tool. This starts every configured local server just long enough
        /// to list its tools, so it runs their commands, including servers in
        /// a project's config under --project; only use it on setups you trust.
        /// Plain doctor starts nothing. Remote servers are not contacted. With
        /// SOURCE, counts the profile's servers for each tool they apply to
        /// (from a git source, only those allowed by --allow-git-mcp or
        /// --trust-source); without, the servers in each tool's config. A
        /// server that fails is reported but is not a problem. The estimate is
        /// about 4 characters per token of each tool's name, description and
        /// input schema.
        #[arg(long)]
        mcp_tokens: bool,
        /// With --mcp-tokens and a git SOURCE, its servers are not started
        /// unless allowed here: give their names (comma-separated,
        /// repeatable), or no value to allow all
        #[arg(
            long,
            value_name = "NAMES",
            num_args = 0..=1,
            default_missing_value = "*",
            value_delimiter = ',',
            action = clap::ArgAction::Append,
            requires = "mcp_tokens"
        )]
        allow_git_mcp: Vec<String>,
        /// With --mcp-tokens, start every server of a git SOURCE
        #[arg(long, requires = "mcp_tokens")]
        trust_source: bool,
        /// Seconds each server gets to list its tools with --mcp-tokens
        #[arg(
            long,
            value_name = "SECS",
            default_value_t = 10,
            value_parser = clap::value_parser!(u64).range(1..=600),
            requires = "mcp_tokens"
        )]
        mcp_timeout: u64,
    },
    /// Show supported tools, what each supports, and which are installed
    ListTargets {
        #[command(flatten)]
        place: Place,
        /// Machine-readable output
        #[arg(long)]
        json: bool,
    },
}

const TOGGLE_HELP: &str = "\
Only the profile's files change: its 'targets' / 'disabled_on' on the item.
Nothing is written to a tool until you run 'kitbag apply' (which then skips the
item on a tool it is off for, and removes what kitbag wrote there earlier, or
turns an MCP server off with the tool's own switch where it has one).

The profile must be a directory on this machine; to change one from a git
source, copy it first. A name used by more than one kind of item needs a
prefix: mcp:NAME, rule:NAME, command:NAME, skill:NAME or agent:NAME. Hooks have
no per-tool setting and can't be enabled or disabled here.

Examples:
  kitbag disable github --on codex ~/dotagents
  kitbag disable skill:review --on cursor,codex
  kitbag enable github --on codex ~/dotagents
  kitbag disable deploy            (off everywhere)
  kitbag enable deploy             (on everywhere again)";

#[derive(Args, Debug, Clone)]
struct ToggleArgs {
    /// MCP server, rule, command, skill, agent or output style name; with a prefix
    /// (mcp:, rule:, command:, skill:, agent:, output-style:) when the name is not unique
    item: String,
    /// Profile directory [default: the current directory]
    profile: Option<String>,
    /// Only for these tools: ids, comma-separated (claude-code (alias: claude),
    /// cursor, codex, gemini-cli, claude-desktop, antigravity, opencode,
    /// copilot, grok-build, windsurf, zed, hermes, amp, cline, roo (alias: roo-code), continue). Without it, every tool
    #[arg(long, value_name = "IDS", value_delimiter = ',')]
    on: Vec<String>,
}

/// What `capture --include` adds to a capture.
#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
enum Include {
    /// Hooks, and the scripts they run
    Hooks,
}

#[derive(Args, Debug, Clone)]
struct Place {
    /// Global config (user) or a project's config
    #[arg(long, value_enum)]
    scope: Option<Scope>,
    /// Project root; implies --scope project
    #[arg(long, value_name = "DIR")]
    project: Option<PathBuf>,
}

#[derive(Args, Debug, Clone)]
struct TargetArgs {
    /// Targets to write: ids (comma-separated: claude-code (alias: claude), cursor,
    /// codex, gemini-cli, claude-desktop, antigravity, opencode, copilot, grok-build, windsurf, zed, hermes, amp, cline, roo (alias: roo-code), continue
    /// (alias: grok)), 'all' or 'detected'
    #[arg(long, value_name = "IDS", default_value = "detected")]
    to: String,
    #[command(flatten)]
    place: Place,
    /// Profile directory inside a git repository
    #[arg(long, value_name = "SUBDIR")]
    path: Option<String>,
    /// Accept a git source over cleartext http:// or git://, which anyone on
    /// the network could tamper with
    #[arg(long)]
    allow_insecure_source: bool,
    /// Refuse a git source that names no branch, tag or commit: pin it with
    /// @<sha|tag|branch> or #<ref>
    #[arg(long)]
    require_pin: bool,
}

impl TargetArgs {
    fn fetch(&self) -> source::FetchOptions {
        source::FetchOptions {
            allow_insecure: self.allow_insecure_source,
            require_pin: self.require_pin,
        }
    }
}

#[derive(Args, Debug, Clone)]
struct StatusArgs {
    /// Targets to compare: ids (comma-separated: claude-code (alias: claude),
    /// cursor, codex, gemini-cli, claude-desktop, antigravity, opencode, copilot, grok-build, windsurf, zed, hermes, amp, cline, roo (alias: roo-code), continue
    /// (alias: grok)), 'all' or 'detected'
    /// [default: detected; without PROFILE, every target applied to]
    #[arg(long, value_name = "IDS")]
    to: Option<String>,
    #[command(flatten)]
    place: Place,
    /// Profile directory inside a git repository
    #[arg(long, value_name = "SUBDIR")]
    path: Option<String>,
    /// Accept a git source over cleartext http:// or git://, which anyone on
    /// the network could tamper with
    #[arg(long)]
    allow_insecure_source: bool,
    /// Refuse a git source that names no branch, tag or commit: pin it with
    /// @<sha|tag|branch> or #<ref>
    #[arg(long)]
    require_pin: bool,
    /// Exit with status 1 when anything drifted
    #[arg(long)]
    exit_code: bool,
    /// With --exit-code, also count what is only in the tool ('tool only'
    /// rule files and text, 'removed' MCP servers, commands, skills, agents)
    #[arg(long, requires = "exit_code")]
    strict: bool,
    /// Machine-readable output
    #[arg(long)]
    json: bool,
}

#[derive(Args, Debug, Clone)]
struct SyncArgs {
    /// Pull this target's edits to the managed instructions and rules back into
    /// the profile (redacted, like capture), then apply them to the others;
    /// ID is a target id (claude-code (alias: claude), cursor, codex, gemini-cli,
    /// claude-desktop, antigravity, opencode, copilot, grok-build (alias: grok), windsurf, zed, hermes, amp, cline, roo (alias: roo-code), continue)
    #[arg(long, value_name = "ID")]
    from: Option<String>,
    /// With --from: also pull in what is only in that tool ('tool only' in
    /// 'kitbag status'): rule files the profile doesn't have, and text outside
    /// kitbag's block in its instructions file (added to the end of the
    /// profile's instructions; the tool's file keeps it where it is)
    #[arg(long, requires = "from")]
    adopt: bool,
    /// Replace instructions edited in the tool with the profile's (backed up)
    #[arg(long)]
    overwrite: bool,
    /// Write without asking
    #[arg(short, long)]
    yes: bool,
    /// Show the full plan and diff, write nothing
    #[arg(long)]
    dry_run: bool,
}

#[derive(Args, Debug, Clone, Default)]
struct ApplyArgs {
    /// Keep existing same-named entries instead of replacing them
    #[arg(long)]
    no_overwrite: bool,
    /// For tools that can't read env vars: resolve ${VAR} now, or never write secret
    /// values [default: resolve; never for git sources]
    #[arg(long, value_enum)]
    secrets: Option<SecretsMode>,
    /// Trust a git source like a local profile: also apply settings that let
    /// agents act without asking (bypassPermissions, permissions.allow rules,
    /// approval_policy = never ...). Local profiles always apply them, with a warning
    #[arg(long, visible_alias = "trust-source")]
    allow_risky_settings: bool,
    /// Write the profile's hooks: commands the tool runs by itself on events.
    /// Without this the plan only lists them, command in full. A git source
    /// also needs --trust-source
    #[arg(long)]
    apply_hooks: bool,
    /// From a git source, MCP servers that would be added or changed are not
    /// applied unless allowed here: give their names (comma-separated,
    /// repeatable), or no value to allow all. --trust-source allows all
    #[arg(
        long,
        value_name = "NAMES",
        num_args = 0..=1,
        default_missing_value = "*",
        value_delimiter = ',',
        action = clap::ArgAction::Append
    )]
    allow_git_mcp: Vec<String>,
}

impl ApplyArgs {
    /// Which MCP servers a git source may add or change.
    fn git_mcp(&self) -> engine::GitMcp {
        git_mcp(self.allow_risky_settings, &self.allow_git_mcp)
    }
}

/// Which MCP servers of a git source `--trust-source` (`all`) and
/// `--allow-git-mcp NAMES` let in.
fn git_mcp(all: bool, names: &[String]) -> engine::GitMcp {
    if all || names.iter().any(|n| n == "*") {
        engine::GitMcp::All
    } else {
        engine::GitMcp::Only(
            names
                .iter()
                .map(|n| n.trim().to_string())
                .filter(|n| !n.is_empty())
                .collect(),
        )
    }
}

/// Does this argument look like a profile source (path or git URL) rather
/// than a list of MCP server names? A bare word only counts when it is a
/// directory holding a `kitbag.toml` or a plugin's `rules/`, so
/// `--allow-git-mcp docs` keeps meaning the server `docs` even when the
/// current directory has a `docs/` folder.
fn looks_like_source(arg: &str) -> bool {
    let dir = std::path::Path::new(arg);
    arg.contains(['/', '\\', ':'])
        || arg.starts_with(['.', '~'])
        || arg.ends_with(".git")
        || dir.join(crate::profile_io::MANIFEST).is_file()
        || crate::profile_io::is_plugin(dir)
}

/// A bare `--allow-git-mcp` followed by the profile source would otherwise
/// swallow the source as a server name. Rewrite it to `--allow-git-mcp=*`
/// (all servers) when the next argument looks like a source.
pub fn normalize_args<I>(args: I) -> Vec<std::ffi::OsString>
where
    I: IntoIterator<Item = std::ffi::OsString>,
{
    let mut out: Vec<std::ffi::OsString> = Vec::new();
    let mut args = args.into_iter().peekable();
    while let Some(a) = args.next() {
        if a == "--" {
            out.push(a);
            out.extend(args);
            break;
        }
        if a == "--allow-git-mcp"
            && args
                .peek()
                .is_some_and(|n| n.to_str().is_some_and(looks_like_source))
        {
            out.push("--allow-git-mcp=*".into());
        } else {
            out.push(a);
        }
    }
    out
}

/// Target label of notes about the profile as a whole.
const PROFILE_NOTE: &str = "profile";

struct Ctx {
    env: Env,
    state_dir: PathBuf,
}

fn context(home_flag: Option<&Path>, place: Option<&Place>) -> Result<Ctx> {
    let vars: HashMap<String, String> = std::env::vars().collect();
    let sandboxed = home_flag.is_some();
    let home = match home_flag {
        Some(h) => std::path::absolute(h)?,
        None => {
            std::env::home_dir().context("could not determine your home directory; pass --home")?
        }
    };
    let mut scope = Scope::User;
    let mut project = std::env::current_dir()?;
    if let Some(p) = place {
        if let Some(s) = p.scope {
            scope = s;
        }
        if let Some(dir) = &p.project {
            if p.scope == Some(Scope::User) {
                bail!("--project can't be combined with --scope user");
            }
            scope = Scope::Project;
            project = std::path::absolute(dir)?;
        }
    }
    // With --home the real locations must never leak in: that is the sandbox guarantee.
    let locations = if sandboxed {
        HashMap::new()
    } else {
        LOCATION_VARS
            .iter()
            .filter_map(|k| vars.get(*k).map(|v| ((*k).to_string(), v.clone())))
            .collect()
    };
    let state_dir = match vars.get("KITBAG_STATE_DIR").filter(|v| !v.is_empty()) {
        Some(d) => PathBuf::from(d),
        None if sandboxed => home.join(".local/state/kitbag"),
        None => match (
            Os::current(),
            vars.get("XDG_STATE_HOME"),
            vars.get("LOCALAPPDATA"),
        ) {
            (Os::Windows, _, Some(l)) => PathBuf::from(l).join("kitbag"),
            (_, Some(x), _) if !x.is_empty() => PathBuf::from(x).join("kitbag"),
            _ => home.join(".local/state/kitbag"),
        },
    };
    Ok(Ctx {
        env: Env {
            scope,
            home,
            project,
            vars,
            locations,
            os: Os::current(),
            include_hooks: false,
        },
        state_dir,
    })
}

fn select_targets(spec: &str, env: &Env, flag: &str) -> Result<Vec<&'static dyn Adapter>> {
    let ids: Vec<&str> = spec
        .split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .collect();
    match ids.as_slice() {
        [] | ["detected"] => {
            let found: Vec<&'static dyn Adapter> = adapters::all()
                .iter()
                .copied()
                .filter(|a| a.scopes().contains(&env.scope) && a.detect(env))
                .collect();
            if found.is_empty() {
                bail!(
                    "no supported tools detected at {} scope; pass --{flag} <ids> (see 'kitbag list-targets')",
                    env.scope
                );
            }
            eprintln!(
                "{}",
                paint(
                    DIM,
                    format!(
                        "Detected: {}  (choose with --{flag})",
                        found.iter().map(|a| a.id()).collect::<Vec<_>>().join(", ")
                    )
                )
            );
            Ok(found)
        }
        _ if ids.contains(&"all") => Ok(adapters::all()
            .iter()
            .copied()
            .filter(|a| a.scopes().contains(&env.scope))
            .collect()),
        _ => {
            let mut out: Vec<&'static dyn Adapter> = Vec::new();
            for id in ids {
                let a = adapters::get(id).with_context(|| {
                    format!(
                        "unknown target '{id}'; known targets: {}",
                        adapters::ids().join(", ")
                    )
                })?;
                if !out.iter().any(|x| x.id() == a.id()) {
                    out.push(a);
                }
            }
            Ok(out)
        }
    }
}

fn print_notes(notes: &[adapters::Note]) {
    for n in ui::sorted_notes(notes) {
        println!("{}", ui::format_note(n));
    }
}

fn confirm(question: &str) -> Result<bool> {
    print!("{question} [y/N] ");
    std::io::stdout().flush()?;
    let mut line = String::new();
    std::io::stdin().lock().read_line(&mut line)?;
    Ok(matches!(
        line.trim().to_ascii_lowercase().as_str(),
        "y" | "yes"
    ))
}

pub fn run(cli: Cli) -> Result<i32> {
    let home = cli.home.as_deref();
    match cli.command {
        Cmd::Init { dir, name, force } => cmd_init(&dir, name, force),
        Cmd::Capture {
            from,
            place,
            out,
            name,
            force,
            prune,
            dry_run,
            include,
        } => {
            let mut ctx = context(home, Some(&place))?;
            ctx.env.include_hooks = include.contains(&Include::Hooks);
            cmd_capture(&ctx, &from, &out, name, force, prune, dry_run)
        }
        Cmd::Apply {
            source,
            targets,
            yes,
            dry_run,
            apply,
        } => cmd_apply(
            &context(home, Some(&targets.place))?,
            &source,
            &targets,
            &apply,
            Mode::Apply { yes, dry_run },
        ),
        Cmd::Diff {
            source,
            targets,
            apply,
            exit_code,
        } => cmd_apply(
            &context(home, Some(&targets.place))?,
            &source,
            &targets,
            &apply,
            Mode::Diff { exit_code },
        ),
        Cmd::Inspect {
            source,
            targets,
            apply,
            json,
        } => cmd_inspect(
            &context(home, Some(&targets.place))?,
            &source,
            &targets,
            &apply,
            json,
        ),
        Cmd::Status { source, args } => {
            cmd_status(&context(home, Some(&args.place))?, source.as_deref(), &args)
        }
        Cmd::Sync {
            source,
            targets,
            sync,
            apply,
        } => cmd_sync(
            &context(home, Some(&targets.place))?,
            &source,
            &targets,
            &apply,
            &sync,
        ),
        Cmd::Disable { args } => cmd_toggle(false, &args),
        Cmd::Enable { args } => cmd_toggle(true, &args),
        Cmd::Undo { id, force } => cmd_undo(&context(home, None)?, id.as_deref(), force),
        Cmd::History { json } => cmd_history(&context(home, None)?, json),
        Cmd::Doctor {
            why: Some(dir),
            targets,
            ..
        } => cmd_doctor_why(home, &dir, &targets.to),
        Cmd::Doctor {
            source,
            targets,
            mcp_tokens,
            mcp_timeout,
            allow_git_mcp,
            trust_source,
            ..
        } => cmd_doctor(
            &context(home, Some(&targets.place))?,
            source.as_deref(),
            &targets,
            mcp_tokens.then(|| McpTokens {
                timeout: std::time::Duration::from_secs(mcp_timeout),
                git: git_mcp(trust_source, &allow_git_mcp),
            }),
        ),
        Cmd::ListTargets { place, json } => cmd_list_targets(&context(home, Some(&place))?, json),
    }
}

// ---------------------------------------------------------------------------

const STARTER_MANIFEST: &str = r#"# kitbag profile: https://github.com/cargopete/kitbag
# Secrets are never stored here. Use ${ENV_VAR} references; they are
# resolved from your environment (or by the tool itself) when you apply.
kitbag = 1
name = "NAME"
description = "My AI agent setup"

# MCP servers. stdio servers need `command`; remote ones need `url`.
# [mcp_servers.github]
# command = "npx"
# args = ["-y", "@modelcontextprotocol/server-github"]
# env = { GITHUB_PERSONAL_ACCESS_TOKEN = "${GITHUB_PERSONAL_ACCESS_TOKEN}" }
#
# [mcp_servers.linear]
# url = "https://mcp.linear.app/mcp"
# headers = { Authorization = "Bearer ${LINEAR_API_KEY}" }
# targets = ["claude-code", "cursor"]   # only these tools; or: disabled_on = ["codex"]

# Per-tool settings (allow-listed keys only; see README).
# [settings.claude-code]
# model = "opus"
#
# [settings.codex]
# model_reasoning_effort = "high"
"#;

const STARTER_INSTRUCTIONS: &str = "# Instructions\n\nWrite instructions every agent should follow here.\nThey become CLAUDE.md, AGENTS.md, GEMINI.md ... in each tool.\n";

const STARTER_RULE: &str = "---\ndescription: Example rule, scoped to Rust files\nglobs:\n  - \"**/*.rs\"\n---\n\nPrefer `?` over `unwrap()` outside tests.\n";

const STARTER_COMMAND: &str = "---\ndescription: Review the staged changes\nargument-hint: \"[focus]\"\n---\n\nReview the staged diff for bugs and unclear code. Focus on: $ARGUMENTS\n";

/// Refuses a `--name` the profile could not be loaded with.
fn check_name_flag(name: Option<&str>) -> Result<()> {
    if let Some(n) = name
        && let Err(why) = profile_io::check_profile_name(n)
    {
        bail!("--name {why}");
    }
    Ok(())
}

/// Does `src` look like a git URL (or shorthand) rather than a directory?
fn looks_remote(src: &str) -> bool {
    src.contains("://")
        || src.starts_with("github:")
        || src.starts_with("gh:")
        || src.starts_with("git@")
        || src.ends_with(".git")
}

fn cmd_toggle(enable: bool, a: &ToggleArgs) -> Result<i32> {
    let verb = if enable { "enable" } else { "disable" };
    let src = a.profile.as_deref().unwrap_or(".");
    let dir = Path::new(src);
    if !dir.is_dir() {
        if looks_remote(src) {
            bail!(
                "'kitbag {verb}' edits a profile directory on this machine, not a git source; clone or copy {src} first, edit the copy, and apply that"
            );
        }
        bail!("{src} is not a directory; give the profile directory to edit");
    }
    let out = crate::toggle::toggle(dir, &a.item, enable, &a.on)?;
    let all = adapters::ids();
    let what = format!("{} '{}'", out.kind.label(), out.name);
    let file = out.file.display();
    if out.changed {
        println!(
            "{} {what}: {}  (was: {})",
            paint(GREEN, if enable { "Enabled" } else { "Disabled" }),
            crate::toggle::describe(&out.after, &all),
            crate::toggle::describe(&out.before, &all),
        );
        println!("  edited {file}");
        println!(
            "{}",
            paint(
                DIM,
                format!("Nothing was applied. Run 'kitbag apply {src}' to update your tools.")
            )
        );
    } else {
        println!(
            "{what} is already {}; {file} is unchanged.",
            crate::toggle::describe(&out.after, &all)
        );
    }
    Ok(0)
}

fn cmd_init(dir: &Path, name: Option<String>, force: bool) -> Result<i32> {
    let starter = [
        MANIFEST,
        "instructions.md",
        "rules/example.md",
        "commands/review.md",
    ];
    if !force
        && let Some(f) = starter
            .iter()
            .find(|f| crate::plan_fs::present(&crate::plan_fs::join_rel(dir, f)))
    {
        bail!(
            "{} already exists; pass --force to replace the starter files",
            crate::plan_fs::join_rel(dir, f).display()
        );
    }
    if dir.join(MANIFEST).exists() && !force {
        bail!(
            "{} already contains a profile; pass --force to replace it",
            dir.display()
        );
    }
    check_name_flag(name.as_deref())?;
    let name = name.unwrap_or_else(|| {
        let base = std::path::absolute(dir)
            .ok()
            .and_then(|d| d.file_name().map(|f| f.to_string_lossy().into_owned()));
        slugify(&base.unwrap_or_default())
    });
    let files = [
        (
            MANIFEST,
            STARTER_MANIFEST.replace("\"NAME\"", &serde_json::to_string(&name)?),
        ),
        ("instructions.md", STARTER_INSTRUCTIONS.to_string()),
        ("rules/example.md", STARTER_RULE.to_string()),
        ("commands/review.md", STARTER_COMMAND.to_string()),
    ];
    std::fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
    for (rel, content) in &files {
        // Links in the profile are replaced, never written through.
        crate::plan_fs::write_owned_file(dir, rel, content.as_bytes())?;
    }
    println!(
        "{} profile '{}' in {}",
        paint(GREEN, "Created"),
        visible_line(&name),
        visible_line(&dir.display().to_string())
    );
    for (rel, _) in &files {
        println!("  {rel}");
    }
    println!(
        "\nNext: edit the files, then preview with {}",
        paint(
            BOLD,
            format!(
                "kitbag diff {} --to all",
                visible_line(&dir.display().to_string())
            )
        )
    );
    Ok(0)
}

fn cmd_capture(
    ctx: &Ctx,
    from: &str,
    out: &Path,
    name: Option<String>,
    force: bool,
    prune: bool,
    dry_run: bool,
) -> Result<i32> {
    let env = &ctx.env;
    check_name_flag(name.as_deref())?;
    let targets = select_targets(from, env, "from")?;
    let name = name.unwrap_or_else(|| {
        let base = std::path::absolute(out)
            .ok()
            .and_then(|d| d.file_name().map(|f| f.to_string_lossy().into_owned()));
        slugify(&base.unwrap_or_else(|| "my-setup".into()))
    });
    let mut r = engine::capture(&targets, env, &name)?;
    // Re-capturing into a profile keeps the per-tool targeting set there, and
    // the servers no tool has (disabled everywhere, or for a tool not
    // installed here) unless --prune.
    let old = if out.join(profile_io::MANIFEST).is_file() {
        profile_io::load(out).ok()
    } else {
        None
    };
    let (mut kept_items, mut uncaptured_items) = (Vec::new(), Vec::new());
    let (kept, uncaptured) = match &old {
        Some(old) => {
            kept_items = profile_io::keep_item_targeting(&mut r.profile, old);
            kept_items.extend(crate::plugins::keep_targeting(&mut r.profile, old));
            if !prune {
                uncaptured_items = profile_io::keep_uncaptured_items(&mut r.profile, old);
            }
            let kept = profile_io::keep_targeting(&mut r.profile, old);
            let uncaptured = if prune {
                old.mcp_servers
                    .keys()
                    .filter(|n| !r.profile.mcp_servers.contains_key(*n))
                    .cloned()
                    .collect()
            } else {
                profile_io::keep_uncaptured_servers(&mut r.profile, old)
            };
            (kept, uncaptured)
        }
        None => (Vec::new(), Vec::new()),
    };
    if let Some(old) = old.as_ref().filter(|_| !env.include_hooks) {
        let n = profile_io::keep_hooks(&mut r.profile, old);
        if n > 0 {
            r.notes.push(adapters::Note {
                level: adapters::Level::Info,
                target: PROFILE_NOTE.to_string(),
                message: format!(
                    "kept the {n} hook(s) of the existing profile: hooks are only captured with --include hooks"
                ),
            });
        }
    }
    if !kept.is_empty() {
        r.notes.push(adapters::Note {
            level: adapters::Level::Info,
            target: PROFILE_NOTE.to_string(),
            message: format!(
                "kept 'targets' / 'disabled_on' from the existing profile for MCP server(s): {}",
                kept.join(", ")
            ),
        });
    }
    if !kept_items.is_empty() {
        r.notes.push(adapters::Note {
            level: adapters::Level::Info,
            target: PROFILE_NOTE.to_string(),
            message: format!(
                "kept 'targets' / 'disabled_on' from the existing profile for: {}",
                kept_items.join(", ")
            ),
        });
    }
    if !uncaptured_items.is_empty() {
        r.notes.push(adapters::Note {
            level: adapters::Level::Info,
            target: PROFILE_NOTE.to_string(),
            message: format!(
                "kept rule(s), command(s), skill(s), agent(s) and output style(s) of the existing profile that no tool has and that name tools in 'targets' / 'disabled_on': {}; pass --prune to drop them",
                uncaptured_items.join(", ")
            ),
        });
    }
    if !uncaptured.is_empty() {
        let message = if prune {
            format!(
                "dropped MCP server(s) of the existing profile that no tool has (--prune): {}",
                uncaptured.join(", ")
            )
        } else {
            format!(
                "kept MCP server(s) of the existing profile that no tool has (disabled, or for a tool not installed here): {}; pass --prune to drop them",
                uncaptured.join(", ")
            )
        };
        r.notes.push(adapters::Note {
            level: adapters::Level::Info,
            target: PROFILE_NOTE.to_string(),
            message,
        });
    }
    let p = &r.profile;
    println!("{} ({} scope)", paint(BOLD, "Captured"), env.scope);
    for s in &r.sources {
        println!("  {} {}", paint(DIM, "read"), tildify(s, &env.home));
    }
    println!(
        "\n  {} MCP server(s), {} rule(s), {} command(s), {} skill(s){}{}{}{}{}{}",
        p.mcp_servers.len(),
        p.rules.len(),
        p.commands.len(),
        p.skills.len(),
        agent_count(p),
        output_style_count(p),
        hook_count(p),
        plugin_count(p),
        if p.instructions.is_some() {
            ", instructions"
        } else {
            ""
        },
        if p.settings.is_empty() {
            String::new()
        } else {
            format!(
                ", settings for {}",
                visible_line(&p.settings.keys().cloned().collect::<Vec<_>>().join(", "))
            )
        }
    );
    if !r.redactions.is_empty() {
        println!(
            "\n{}",
            paint(
                YELLOW,
                "Secrets replaced with env references (set these when applying):"
            )
        );
        for red in &r.redactions {
            println!(
                "  {}  ->  ${{{}}}  {}",
                visible_line(&red.location),
                paint(BOLD, visible_line(&red.env_var)),
                paint(DIM, format!("({})", visible_line(red.reason)))
            );
        }
    }
    if !r.notes.is_empty() {
        println!();
        print_notes(&r.notes);
    }
    if p.is_empty() {
        println!("\nNothing to capture.");
        return Ok(0);
    }
    if dry_run {
        println!("\n{} nothing written (--dry-run)", paint(DIM, "Dry run:"));
        return Ok(0);
    }
    profile_io::save(p, out, force)?;
    println!(
        "\n{} {}",
        paint(GREEN, "Wrote profile to"),
        visible_line(&out.display().to_string())
    );
    let req = profile_io::required_env(p);
    if !req.is_empty() {
        println!(
            "  Env vars this profile expects: {}",
            visible_line(&req.keys().cloned().collect::<Vec<_>>().join(", "))
        );
    }
    println!(
        "  Review it, commit it to git, then apply it anywhere with {}",
        paint(
            BOLD,
            format!(
                "kitbag apply {} --to <tool>",
                visible_line(&out.display().to_string())
            )
        )
    );
    Ok(0)
}

enum Mode {
    Apply { yes: bool, dry_run: bool },
    Diff { exit_code: bool },
}

fn load_source(
    src: &str,
    path: Option<&str>,
    fetch: source::FetchOptions,
) -> Result<(source::Resolved, Profile)> {
    let resolved = source::resolve(src, path, fetch)?;
    let profile = profile_io::load_named(&resolved.dir, &resolved.name_hint)?;
    for w in &profile.warnings {
        eprintln!("{} {}", paint(YELLOW, "warning:"), visible_line(w));
    }
    Ok((resolved, profile))
}

fn print_plan_summary(plan: &Plan, env: &Env) {
    for c in &plan.changes {
        println!("  {}", change_line(c, env));
    }
}

/// `create  ~/.claude/CLAUDE.md  +3 -0`: one file a plan writes.
fn change_line(c: &crate::plan_fs::FileChange, env: &Env) -> String {
    let stats = ui::line_stats(c)
        .filter(|_| !ui::mode_only(c))
        .map(|(a, d)| {
            format!(
                "{} {}",
                paint(GREEN, format!("+{a}")),
                paint(RED, format!("-{d}"))
            )
        })
        .unwrap_or_else(|| {
            if ui::mode_only(c) {
                paint(DIM, "+x")
            } else {
                paint(DIM, "binary")
            }
        });
    format!(
        "{}  {}  {}",
        ui::change_label(c),
        tildify(&c.path, &env.home),
        stats
    )
}

fn print_plan_extras(plan: &Plan, remote: bool) {
    if !plan.notes.is_empty() {
        println!();
        print_notes(&plan.notes);
    }
    if !plan.risky.is_empty() {
        println!(
            "\n{}",
            paint(YELLOW, "Lets agents act without asking, or runs commands:")
        );
        for r in &plan.risky {
            println!("  - {}", visible_line(r));
        }
    }
    // Shown as in the profile; anything that still looks like a secret is masked,
    // and control characters can't rewrite the terminal.
    let shown = |s: &str| {
        let masked = crate::secrets::mask(&crate::secrets::mask_line(s), &plan.sensitive);
        visible_line(&masked).into_owned()
    };
    let list = |servers: &[engine::ServerReview]| {
        for s in servers {
            println!(
                "  {}: {}  {}",
                shown(&s.name),
                paint(BOLD, shown(&s.endpoint)),
                paint(DIM, format!("({})", s.targets.join(", ")))
            );
            for (label, keys) in [("env", &s.env), ("headers", &s.headers)] {
                if !keys.is_empty() {
                    println!("      {label}: {}", shown(&keys.join(", ")));
                }
            }
        }
    };
    if !plan.withheld.is_empty() {
        println!(
            "\n{}",
            paint(
                YELLOW,
                "MCP servers not applied (from a git source). Each runs a command on this machine or connects to a remote service when the tool starts:"
            )
        );
        list(&plan.withheld);
        let names: Vec<String> = plan.withheld.iter().map(|s| shown(&s.name)).collect();
        println!(
            "  To apply them, pass {} (or name fewer), or --trust-source for everything.",
            paint(BOLD, format!("--allow-git-mcp {}", names.join(",")))
        );
    }
    if !plan.withheld_removals.is_empty() {
        println!(
            "\n{}",
            paint(
                YELLOW,
                "MCP servers not removed (disabled for these tools in the profile, which is from a git source):"
            )
        );
        for s in &plan.withheld_removals {
            println!(
                "  {}  {}",
                shown(&s.name),
                paint(DIM, format!("({})", s.targets.join(", ")))
            );
        }
        let names: Vec<String> = plan
            .withheld_removals
            .iter()
            .map(|s| shown(&s.name))
            .collect();
        println!(
            "  To remove them, pass {} (or name fewer), or --trust-source for everything.",
            paint(BOLD, format!("--allow-git-mcp {}", names.join(",")))
        );
    }
    if !plan.removed_servers.is_empty() {
        println!(
            "\n{}",
            paint(
                YELLOW,
                "MCP servers removed (disabled for these tools in the profile; kitbag added them there before):"
            )
        );
        for s in &plan.removed_servers {
            println!(
                "  {}  {}",
                shown(&s.name),
                paint(DIM, format!("({})", s.targets.join(", ")))
            );
        }
    }
    if !plan.turned_off_servers.is_empty() {
        println!(
            "\n{}",
            paint(
                YELLOW,
                "MCP servers turned off (disabled for these tools in the profile; kitbag added them there before, so they stay in the tool's config, marked off):"
            )
        );
        for s in &plan.turned_off_servers {
            println!(
                "  {}  {}",
                shown(&s.name),
                paint(DIM, format!("({})", s.targets.join(", ")))
            );
        }
    }
    if !plan.removed_items.is_empty() {
        println!(
            "\n{}",
            paint(
                YELLOW,
                "Rules, commands, skills, agents and output styles removed (disabled for these tools in the profile; kitbag added them there before):"
            )
        );
        for (target, item) in &plan.removed_items {
            println!("  {}  {}", shown(item), paint(DIM, format!("({target})")));
        }
    }
    if !plan.servers.is_empty() {
        println!(
            "\n{}",
            paint(
                YELLOW,
                if remote {
                    "This profile comes from a remote repository. Review the MCP servers it adds or changes:"
                } else {
                    "MCP servers added or changed:"
                }
            )
        );
        list(&plan.servers);
    }
    if !plan.missing_env.is_empty() {
        println!(
            "\n{} {}",
            paint(
                YELLOW,
                "Unset env vars (needed by tools that can't read the environment):"
            ),
            visible_line(
                &plan
                    .missing_env
                    .iter()
                    .cloned()
                    .collect::<Vec<_>>()
                    .join(", ")
            )
        );
    }
}

fn cmd_apply(ctx: &Ctx, src: &str, t: &TargetArgs, a: &ApplyArgs, mode: Mode) -> Result<i32> {
    let (resolved, profile) = load_source(src, t.path.as_deref(), t.fetch())?;
    let targets = select_targets(&t.to, &ctx.env, "to")?;
    let (profile, mut plan) = plan_apply(ctx, &resolved, profile, &targets, a, &BTreeSet::new())?;
    if matches!(mode, Mode::Diff { .. }) {
        let found = tool_only_items(ctx, &profile, &plan, &targets)?;
        plan.notes.extend(tool_only_notes(&found, &ctx.env));
    }
    present(ctx, &resolved, &profile, &plan, mode)
}

/// An MCP server as `inspect` shows it: redacted as capture would redact
/// it, then masked like a diff line, so no secret in the profile is shown.
struct ServerView {
    name: String,
    transport: &'static str,
    /// Runs a command on this machine (rather than connecting to a URL).
    local: bool,
    /// The command line, or the URL without credentials.
    endpoint: String,
    env: Vec<(String, String)>,
    headers: Vec<(String, String)>,
    /// Where the profile holds what looks like a secret.
    secrets: Vec<String>,
}

fn server_view(name: &str, s: &McpServer) -> ServerView {
    let (r, redactions) = crate::secrets::redact_server(name, s);
    // Redaction puts a made-up `${VAR}` where a literal secret was; the
    // profile references no such variable, so show the mask instead.
    let made_up: Vec<String> = redactions
        .iter()
        .filter(|x| x.reason != "secret-fallback")
        .map(|x| crate::secrets::reference(&x.env_var))
        .collect();
    let show = |v: &str| {
        let mut out = crate::secrets::mask_line(&source::strip_url_credentials(v));
        for m in &made_up {
            out = out.replace(m.as_str(), "••••••••");
        }
        visible_line(&out).into_owned()
    };
    let local = r.command.is_some() && (r.url.is_none() || r.transport() == Transport::Stdio);
    let endpoint = if local {
        show(
            &std::iter::once(r.command.clone().unwrap_or_default())
                .chain(r.args.iter().cloned())
                .collect::<Vec<_>>()
                .join(" "),
        )
    } else {
        show(r.url.as_deref().unwrap_or_default())
    };
    let pairs = |m: &BTreeMap<String, String>| -> Vec<(String, String)> {
        m.iter().map(|(k, v)| (show(k), show(v))).collect()
    };
    ServerView {
        name: show(name),
        transport: r.transport().as_str(),
        local,
        endpoint,
        env: pairs(&r.env),
        headers: pairs(&r.headers),
        secrets: redactions
            .iter()
            .map(|x| format!("{}: looks like a secret ({})", x.location, x.reason))
            .collect(),
    }
}

/// `kitbag inspect`: what a source holds and what applying it would do,
/// without writing anything or starting any server.
fn cmd_inspect(ctx: &Ctx, src: &str, t: &TargetArgs, a: &ApplyArgs, as_json: bool) -> Result<i32> {
    let env = &ctx.env;
    let (resolved, profile) = load_source(src, t.path.as_deref(), t.fetch())?;
    let plugin = profile_io::is_plugin(&resolved.dir);
    let servers: Vec<ServerView> = profile
        .mcp_servers
        .iter()
        .map(|(n, s)| server_view(n, s))
        .collect();
    let names = |v: Vec<&String>| -> Vec<String> {
        v.into_iter()
            .map(|n| visible_line(n).into_owned())
            .collect()
    };
    let rules = names(profile.rules.iter().map(|r| &r.name).collect());
    let commands = names(profile.commands.iter().map(|c| &c.name).collect());
    let skills = names(profile.skills.iter().map(|s| &s.name).collect());
    let agents = names(profile.agents.iter().map(|s| &s.name).collect());
    let output_styles = names(profile.output_styles.iter().map(|s| &s.name).collect());
    let settings = names(profile.settings.keys().collect());
    let hooks: Vec<String> = profile
        .hooks
        .iter()
        .map(|h| visible_line(&crate::secrets::mask_line(&crate::hooks::describe(h))).into_owned())
        .collect();
    let plugins: Vec<String> = crate::plugins::list(&profile)
        .iter()
        .map(|l| visible_line(l).into_owned())
        .collect();
    let instructions = profile
        .instructions
        .as_ref()
        .map(|i| (i.len(), i.lines().count()));
    // Safety summary.
    let env_vars: Vec<String> = profile_io::required_env(&profile)
        .into_keys()
        .map(|k| visible_line(&k).into_owned())
        .collect();
    let executable: Vec<String> = profile
        .skills
        .iter()
        .flat_map(|s| {
            s.files
                .iter()
                .filter(|f| f.executable)
                .map(move |f| format!("skills/{}/{}", s.name, f.path))
        })
        .map(|p| visible_line(&p).into_owned())
        .collect();
    let mut secrets: Vec<String> = servers.iter().flat_map(|s| s.secrets.clone()).collect();
    for found in profile_io::scan_for_secrets(&profile) {
        let at = found.split(": ").next().unwrap_or_default();
        if !secrets.iter().any(|s| s.split(": ").next() == Some(at)) {
            secrets.push(found);
        }
    }
    let secrets: Vec<String> = secrets
        .iter()
        .map(|s| visible_line(s).into_owned())
        .collect();
    let mut risky: Vec<String> = profile
        .settings
        .iter()
        .flat_map(|(target, set)| {
            let (allowed, _) = policy::filter(target, Some(set));
            policy::risky(target, &allowed)
        })
        .collect();
    risky.extend(policy::risky_content(&profile));
    risky.extend(crate::plugins::review_lines(&profile));
    let risky: Vec<String> = risky
        .iter()
        .map(|r| {
            visible_line(&crate::secrets::mask_line(&source::strip_url_credentials(
                r,
            )))
            .into_owned()
        })
        .collect();
    // Files per tool, planned one tool at a time like `apply --to <id>`.
    let spec: Vec<&str> =
        t.to.split(',')
            .map(str::trim)
            .filter(|x| !x.is_empty())
            .collect();
    let none_detected = matches!(spec.as_slice(), [] | ["detected"])
        && !adapters::all()
            .iter()
            .any(|x| x.scopes().contains(&env.scope) && x.detect(env));
    let targets = if none_detected {
        Vec::new()
    } else {
        select_targets(&t.to, env, "to")?
    };
    let mut files = Vec::new();
    for target in &targets {
        let (_, plan) = plan_apply(
            ctx,
            &resolved,
            profile.clone(),
            &[*target],
            a,
            &BTreeSet::new(),
        )?;
        files.push((target.id(), plan));
    }

    if as_json {
        let server_json: Vec<_> = servers
            .iter()
            .map(|s| {
                let map = |v: &[(String, String)]| -> serde_json::Map<String, serde_json::Value> {
                    v.iter().map(|(k, v)| (k.clone(), json!(v))).collect()
                };
                json!({
                    "name": s.name, "transport": s.transport, "local": s.local,
                    "endpoint": s.endpoint, "env": map(&s.env), "headers": map(&s.headers),
                })
            })
            .collect();
        let files_json: serde_json::Map<String, serde_json::Value> = files
            .iter()
            .map(|(id, plan)| {
                let skipped = plan.skipped.iter().any(|x| x == id);
                (
                    (*id).to_string(),
                    json!({
                        "skipped": skipped,
                        "files": plan.changes.iter().map(|c| json!({
                            "path": c.path.display().to_string(),
                            "change": match c.kind() {
                                crate::plan_fs::ChangeKind::Create => "create",
                                crate::plan_fs::ChangeKind::Modify => "modify",
                                crate::plan_fs::ChangeKind::Delete => "delete",
                            },
                        })).collect::<Vec<_>>(),
                        "withheld_mcp_servers": plan.withheld.iter().map(|w| visible_line(&w.name).into_owned()).collect::<Vec<_>>(),
                    }),
                )
            })
            .collect();
        let v = json!({
            "name": visible_line(&profile.name),
            "kind": if plugin { "plugin" } else { "profile" },
            "origin": visible_line(&resolved.origin),
            "source": visible_line(&resolved.source),
            "commit": resolved.commit,
            "instructions": instructions.map(|(bytes, lines)| json!({"bytes": bytes, "lines": lines})),
            "rules": rules, "commands": commands, "skills": skills, "agents": agents,
            "output_styles": output_styles,
            "settings": settings,
            "hooks": hooks,
            "plugins": plugins,
            "mcp_servers": server_json,
            "safety": {
                "commands": servers.iter().filter(|s| s.local).map(|s| json!({"name": s.name, "command": s.endpoint})).collect::<Vec<_>>(),
                "urls": servers.iter().filter(|s| !s.local).map(|s| json!({"name": s.name, "url": s.endpoint})).collect::<Vec<_>>(),
                "env": env_vars, "executable": executable, "secrets": secrets, "risky": risky,
            },
            "scope": env.scope.to_string(),
            "files": if none_detected { serde_json::Value::Null } else { serde_json::Value::Object(files_json) },
        });
        println!("{}", serde_json::to_string_pretty(&v)?);
        return Ok(0);
    }

    println!(
        "{} '{}' from {}",
        paint(BOLD, if plugin { "Plugin" } else { "Profile" }),
        visible_line(&profile.name),
        visible_line(&resolved.origin)
    );
    if let Some(c) = &resolved.commit {
        println!("  commit {}", visible_line(c));
    }
    if let Some(d) = &profile.description {
        println!("  {}", paint(DIM, visible_line(d)));
    }
    let list = |label: &str, items: &[String]| {
        let shown = if items.is_empty() {
            paint(DIM, "none")
        } else {
            items.join(", ")
        };
        println!("  {:<15} {shown}", format!("{label} ({})", items.len()));
    };
    println!("\n{}", paint(BOLD, "Contents"));
    match instructions {
        Some((bytes, lines)) => println!(
            "  {:<15} instructions.md ({lines} line(s), {bytes} bytes)",
            "instructions"
        ),
        None => println!("  {:<15} {}", "instructions", paint(DIM, "none")),
    }
    list("rules", &rules);
    list("commands", &commands);
    list("skills", &skills);
    list("agents", &agents);
    list("output styles", &output_styles);
    if !hooks.is_empty() {
        println!("  {:<15}", format!("hooks ({})", hooks.len()));
        for h in &hooks {
            println!("    {h}");
        }
    }
    if !plugins.is_empty() {
        println!("  {:<15}", format!("plugins ({})", plugins.len()));
        for p in &plugins {
            println!("    {p}");
        }
    }
    if !settings.is_empty() {
        list("settings", &settings);
    }
    list(
        "mcp servers",
        &servers.iter().map(|s| s.name.clone()).collect::<Vec<_>>(),
    );
    for s in &servers {
        println!(
            "    {}: {}  {}",
            s.name,
            paint(BOLD, &s.endpoint),
            paint(DIM, format!("({})", s.transport))
        );
        for (label, pairs) in [("env", &s.env), ("headers", &s.headers)] {
            if !pairs.is_empty() {
                let shown: Vec<String> = pairs.iter().map(|(k, v)| format!("{k}={v}")).collect();
                println!("        {label}: {}", shown.join(", "));
            }
        }
    }

    println!("\n{}", paint(BOLD, "Safety"));
    let mut any = false;
    let mut section = |title: &str, lines: Vec<String>| {
        if lines.is_empty() {
            return;
        }
        any = true;
        println!("  {}", paint(YELLOW, title));
        for l in lines {
            println!("    {l}");
        }
    };
    section(
        "Runs local commands (MCP servers, started by the tool):",
        servers
            .iter()
            .filter(|s| s.local)
            .map(|s| format!("{}: {}", s.name, s.endpoint))
            .collect(),
    );
    section(
        "Connects to (MCP servers):",
        servers
            .iter()
            .filter(|s| !s.local)
            .map(|s| format!("{}: {} ({})", s.name, s.endpoint, s.transport))
            .collect(),
    );
    section(
        "Env vars it references:",
        if env_vars.is_empty() {
            Vec::new()
        } else {
            vec![env_vars.join(", ")]
        },
    );
    section(
        if resolved.remote {
            "Executable files (not marked executable from a git source without --trust-source):"
        } else {
            "Executable files:"
        },
        executable.clone(),
    );
    section(
        "Lets agents act without asking, or runs commands:",
        risky.clone(),
    );
    section(
        "Looks like secrets written in the profile (shown redacted):",
        secrets.clone(),
    );
    if !any {
        println!(
            "  {} runs no commands, connects to no URLs, references no env vars and installs no executable files.",
            paint(GREEN, "Nothing to review:")
        );
    }

    println!();
    if none_detected {
        println!(
            "{} no supported tools detected at {} scope; pass --to <ids> to list the files 'apply' would write.",
            paint(BOLD, "Files:"),
            env.scope
        );
        return Ok(0);
    }
    println!(
        "{} ({}; nothing is written)",
        paint(BOLD, "Files 'apply' would write"),
        where_label(env)
    );
    for (id, plan) in &files {
        println!("  {id}");
        if plan.skipped.iter().any(|x| x == id) {
            let why = plan
                .notes
                .iter()
                .find(|n| n.target == *id)
                .map(|n| n.message.clone())
                .unwrap_or_else(|| "skipped".into());
            println!("    {}", paint(DIM, visible_line(&why)));
            continue;
        }
        if plan.changes.is_empty() {
            println!("    {}", paint(DIM, "nothing to write (already matches)"));
        }
        for c in &plan.changes {
            println!("    {}", change_line(c, env));
        }
        if !plan.withheld.is_empty() {
            println!(
                "    {}",
                paint(
                    DIM,
                    format!(
                        "without the MCP servers {} (from a git source; see --allow-git-mcp)",
                        plan.withheld
                            .iter()
                            .map(|w| visible_line(&w.name).into_owned())
                            .collect::<Vec<_>>()
                            .join(", ")
                    )
                )
            );
        }
    }
    println!(
        "\n{}",
        paint(
            DIM,
            "See the changes with 'kitbag diff' and the same arguments."
        )
    );
    Ok(0)
}

/// Plan an apply of `profile`, with the restrictions for git sources: risky
/// settings and content dropped, secrets not resolved, MCP servers opt-in.
/// Returns the profile as applied (after those restrictions) and the plan.
/// The managed items in `hold` are left as they are in the tool.
fn plan_apply(
    ctx: &Ctx,
    resolved: &source::Resolved,
    mut profile: Profile,
    targets: &[&'static dyn Adapter],
    a: &ApplyArgs,
    hold: &BTreeSet<(PathBuf, Option<String>)>,
) -> Result<(Profile, Plan)> {
    let env = &ctx.env;
    // A profile from someone else's repository doesn't get to turn off the
    // tools' safety prompts unless the user says so.
    let mut dropped = Vec::new();
    if resolved.remote && !a.allow_risky_settings {
        for (target, settings) in &mut profile.settings {
            for d in policy::drop_risky(target, settings) {
                dropped.push((target.clone(), d));
            }
        }
        for d in policy::drop_risky_content(&mut profile) {
            dropped.push((PROFILE_NOTE.to_string(), d));
        }
    }
    // Don't hand local secrets to servers someone else defined.
    let untrusted = resolved.remote && !a.allow_risky_settings;
    // Hooks run commands by themselves: from a git source they are withheld
    // unless it is trusted, and they are listed in the plan either way.
    let withheld_hooks = if untrusted {
        policy::drop_hooks(&mut profile)
    } else {
        Vec::new()
    };
    // Plugins can bundle hooks, MCP servers and commands, and the tool fetches
    // them itself: from a git source they are withheld unless it is trusted.
    let withheld_plugins = if untrusted {
        crate::plugins::drop_untrusted(&mut profile)
    } else {
        Vec::new()
    };
    let secrets = a.secrets.unwrap_or(if untrusted {
        SecretsMode::Never
    } else {
        SecretsMode::Resolve
    });
    // Skill scripts from someone else's repository are not made executable
    // unless the source is trusted, like shell snippets.
    let mut exec_dropped = false;
    if untrusted {
        for f in profile.skills.iter_mut().flat_map(|s| s.files.iter_mut()) {
            exec_dropped |= std::mem::take(&mut f.executable);
        }
    }
    let (applied_servers, records_warning) = crate::sync::applied_servers(&ctx.state_dir, env)?;
    let (applied_files, files_warning) = crate::sync::applied_files(&ctx.state_dir, env)?;
    let allow = a.git_mcp();
    let opts = ApplyOptions {
        overwrite: !a.no_overwrite,
        secrets,
        block_id: slugify(&profile.name),
        applied_servers,
        source: Some(resolved.source.clone()),
        git_removals: resolved.remote.then(|| allow.clone()),
        hold: hold.clone(),
        // Hooks from a git source need --trust-source on top of --apply-hooks.
        apply_hooks: a.apply_hooks && !untrusted,
        applied_files,
    };
    let mut plan = if resolved.remote {
        let unknown: Vec<String> = match &allow {
            engine::GitMcp::Only(names) => names
                .iter()
                .filter(|n| !profile.mcp_servers.contains_key(*n))
                .cloned()
                .collect(),
            engine::GitMcp::All => Vec::new(),
        };
        let mut plan = engine::plan_untrusted(&mut profile, targets, env, &opts, &allow)?;
        for n in unknown {
            plan.notes.push(adapters::Note {
                level: adapters::Level::Warn,
                target: PROFILE_NOTE.to_string(),
                message: format!("--allow-git-mcp: no MCP server named '{n}' in the profile"),
            });
        }
        plan
    } else {
        engine::plan(&profile, targets, env, &opts)?
    };
    for w in [records_warning, files_warning].into_iter().flatten() {
        plan.notes.push(adapters::Note {
            level: adapters::Level::Warn,
            target: PROFILE_NOTE.to_string(),
            message: w,
        });
    }
    if untrusted && a.secrets.is_none() && !plan.kept_env.is_empty() {
        plan.notes.push(adapters::Note {
            level: adapters::Level::Info,
            target: PROFILE_NOTE.to_string(),
            message: format!(
                "{} not resolved from your environment, because the profile comes from a remote repository (pass --secrets resolve to resolve them)",
                plan.kept_env.iter().map(|v| format!("${{{v}}}")).collect::<Vec<_>>().join(", ")
            ),
        });
    }
    if exec_dropped && !plan.targets.is_empty() {
        plan.notes.push(adapters::Note {
            level: adapters::Level::Info,
            target: PROFILE_NOTE.to_string(),
            message: "skill scripts are not marked executable, because the profile comes from a remote repository (pass --trust-source to keep the executable bit)".to_string(),
        });
    }
    // Said for every target the profile is applied to, with each hook in full
    // under the first and a count under the others.
    for (i, target) in plan.targets.clone().iter().enumerate() {
        if withheld_hooks.is_empty() {
            break;
        }
        if i == 0 {
            for h in &withheld_hooks {
                plan.notes.push(adapters::Note {
                    level: adapters::Level::Warn,
                    target: target.clone(),
                    message: format!(
                        "hook not applied, because the profile comes from a remote repository: {h} (needs --trust-source and --apply-hooks)"
                    ),
                });
            }
        } else {
            plan.notes.push(adapters::Note {
                level: adapters::Level::Warn,
                target: target.clone(),
                message: format!(
                    "{} hook(s) not applied, because the profile comes from a remote repository (needs --trust-source and --apply-hooks)",
                    withheld_hooks.len()
                ),
            });
        }
    }
    for (target, d) in withheld_plugins {
        if plan.targets.contains(&target) {
            plan.notes.push(adapters::Note {
                level: adapters::Level::Warn,
                target,
                message: format!(
                    "not applied, because the profile comes from a remote repository: {d} (needs --trust-source)"
                ),
            });
        }
    }
    for (target, d) in dropped {
        if plan.targets.contains(&target) || (target == PROFILE_NOTE && !plan.targets.is_empty()) {
            plan.notes.push(adapters::Note {
                level: adapters::Level::Warn,
                target,
                message: format!(
                    "not applied, because the profile comes from a remote repository: {d} (pass --allow-risky-settings to apply it)"
                ),
            });
        }
    }
    Ok((profile, plan))
}

/// The end of an "Up to date" line when the plan left in place MCP servers the
/// profile disables (added by hand, by another profile or source, or edited),
/// so the line doesn't read as if the tools matched the profile exactly.
fn left_in_place(plan: &Plan) -> &'static str {
    let kept = plan.notes.iter().any(|n| {
        n.level == adapters::Level::Warn
            && n.message.starts_with("MCP server '")
            && n.message.contains("so it was left in place")
    });
    if kept {
        ", except for MCP servers it disables that were left in place (see below)"
    } else {
        ""
    }
}

fn print_skipped(plan: &Plan) {
    if !plan.skipped.is_empty() {
        println!(
            "{} {} (see the notes below for why)",
            paint(YELLOW, "Skipped:"),
            plan.skipped.join(", ")
        );
    }
    print_held_back(plan);
}

/// The managed items `sync` left alone because they were edited in the tool.
fn print_held_back(plan: &Plan) {
    if !plan.held_back.is_empty() {
        let items: Vec<String> = plan
            .held_back
            .iter()
            .map(|i| format!("{} {}", i.target, i.item))
            .collect();
        println!(
            "{} {} (edited in the tool; see the notes below)",
            paint(YELLOW, "Skipped:"),
            items.join(", ")
        );
    }
}

fn where_label(env: &Env) -> String {
    match env.scope {
        Scope::User => "user scope".to_string(),
        Scope::Project => format!("project {}", tildify(&env.project, &env.home)),
    }
}

/// The commit this machine last applied the git source `resolved` at, when
/// it differs from the one just fetched: the source moved since.
fn source_moved(ctx: &Ctx, resolved: &source::Resolved) -> Result<Option<String>> {
    let Some(now) = &resolved.commit else {
        return Ok(None);
    };
    Ok(Store::new(&ctx.state_dir)
        .applied_commit(&resolved.source)?
        .map(|g| g.commit)
        .filter(|before| before != now))
}

fn print_source_moved(resolved: &source::Resolved, before: &str) {
    println!(
        "{} {} moved since it was last applied on this machine:\n  applied  {}\n  now      {}\n",
        paint(YELLOW, "warning:"),
        visible_line(&resolved.source),
        visible_line(before),
        visible_line(resolved.commit.as_deref().unwrap_or_default())
    );
}

/// Show a plan, then (in apply mode) confirm and write it, and record what
/// the managed instructions look like afterwards for `kitbag status`.
fn present(
    ctx: &Ctx,
    resolved: &source::Resolved,
    profile: &Profile,
    plan: &Plan,
    mode: Mode,
) -> Result<i32> {
    let env = &ctx.env;
    let where_ = where_label(env);
    println!(
        "{} '{}' from {}",
        paint(BOLD, "Profile"),
        visible_line(&profile.name),
        visible_line(&resolved.origin),
    );
    if let Some(c) = &resolved.commit {
        println!("  commit {}", visible_line(c));
    }
    println!(
        "  -> {} ({where_})\n",
        visible_line(&plan.targets.join(", "))
    );
    if let Some(before) = source_moved(ctx, resolved)? {
        print_source_moved(resolved, &before);
    }
    let show_diff = matches!(mode, Mode::Diff { .. } | Mode::Apply { dry_run: true, .. });
    let record = |tx: Option<&str>| -> Result<()> {
        if matches!(mode, Mode::Apply { dry_run: false, .. }) {
            crate::sync::record(&ctx.state_dir, plan, env, &profile.name, tx)?;
            crate::hooks::record(&ctx.state_dir, plan, env, &profile.name, tx)?;
        }
        Ok(())
    };
    if plan.changes.is_empty() {
        if plan.targets.is_empty() {
            println!("Nothing to apply.");
            print_skipped(plan);
        } else if plan.withheld.is_empty() && plan.withheld_removals.is_empty() {
            let kept = plan
                .notes
                .iter()
                .filter(|n| n.message.contains("because --no-overwrite was given"))
                .count();
            if kept > 0 {
                println!(
                    "Nothing written; kept {kept} existing item(s) because of --no-overwrite."
                );
                print_skipped(plan);
            } else if !plan.held_back.is_empty() {
                println!("Nothing to write besides the items edited in the tool.");
                print_skipped(plan);
            } else if !plan.skipped.is_empty() {
                println!(
                    "{} {} already matches the profile{}.",
                    paint(GREEN, "Up to date:"),
                    plan.targets.join(", "),
                    left_in_place(plan)
                );
                print_skipped(plan);
            } else {
                println!(
                    "{} every target already matches the profile{}.",
                    paint(GREEN, "Up to date:"),
                    left_in_place(plan)
                );
            }
        } else {
            println!("Nothing to write without the MCP servers listed below.");
            print_held_back(plan);
        }
        print_plan_extras(plan, resolved.remote);
        // The files already match, but the source moved: remember the commit
        // in a transaction with no files, so `status` stops calling it drift.
        if matches!(mode, Mode::Apply { dry_run: false, .. })
            && !plan.targets.is_empty()
            && plan.withheld.is_empty()
            && plan.withheld_removals.is_empty()
            && let (Some(commit), Some(_)) = (&resolved.commit, source_moved(ctx, resolved)?)
        {
            let desc = format!(
                "apply {} -> {} ({}), files unchanged",
                resolved.origin,
                plan.targets.join(", "),
                where_
            );
            let git = crate::txn::GitCommit {
                source: resolved.source.clone(),
                commit: commit.clone(),
            };
            let tx = Store::new(&ctx.state_dir).commit_from(
                &[],
                &desc,
                &plan.targets,
                &crate::txn::WriteOptions::default(),
                Some(git),
            )?;
            println!(
                "Recorded commit {} for {}; no files changed. Undo with {}",
                visible_line(commit),
                visible_line(&resolved.source),
                paint(BOLD, format!("kitbag undo {}", tx.id))
            );
        }
        record(None)?;
        return Ok(0);
    }
    if show_diff {
        for c in &plan.changes {
            print!(
                "{}",
                ui::render_diff(c, &tildify(&c.path, &env.home), &plan.sensitive, usize::MAX)
            );
        }
        println!();
    }
    print_plan_summary(plan, env);
    print_held_back(plan);
    print_plan_extras(plan, resolved.remote);
    match mode {
        Mode::Diff { exit_code } => Ok(i32::from(exit_code)),
        Mode::Apply { dry_run: true, .. } => {
            println!("\n{} nothing written (--dry-run)", paint(DIM, "Dry run:"));
            Ok(0)
        }
        Mode::Apply { yes, .. } => {
            if !yes {
                if !std::io::stdin().is_terminal() {
                    bail!(
                        "not writing without confirmation in a non-interactive session; review with 'kitbag diff', then pass --yes"
                    );
                }
                println!();
                if !confirm(&format!(
                    "Write {} file(s)? Backups are kept; 'kitbag undo' reverts.",
                    plan.changes.len()
                ))? {
                    println!("Aborted; nothing written.");
                    return Ok(1);
                }
            }
            let desc = format!(
                "apply {} -> {} ({})",
                resolved.origin,
                plan.targets.join(", "),
                where_
            );
            let git = resolved.commit.as_ref().map(|c| crate::txn::GitCommit {
                source: resolved.source.clone(),
                commit: c.clone(),
            });
            match engine::commit_from(plan, &desc, &ctx.state_dir, git)? {
                Some(tx) => {
                    record(Some(&tx.id))?;
                    println!(
                        "\n{} {} file(s). Undo with {}",
                        paint(GREEN, "Wrote"),
                        tx.entries.len(),
                        paint(BOLD, format!("kitbag undo {}", tx.id))
                    );
                }
                None => {
                    record(None)?;
                    println!("Nothing to write.");
                }
            }
            Ok(0)
        }
    }
}

/// One row of `kitbag status`.
struct StatusRow {
    target: String,
    item: String,
    path: Option<PathBuf>,
    state: crate::sync::State,
    detail: Option<String>,
}

/// Why a target has nowhere for instructions at this scope.
fn unsupported_detail(a: &dyn Adapter, env: &Env) -> String {
    if a.id() == "cursor" && env.scope == Scope::User {
        "Cursor keeps user rules in its settings (app storage), not in files; paste them there, or use --project".to_string()
    } else {
        format!(
            "{} has no instructions file at {} scope",
            a.name(),
            env.scope
        )
    }
}

/// Instruction items of `plan`, each compared with the last apply.
fn instruction_rows(ctx: &Ctx, plan: &Plan) -> Result<Vec<(StatusRow, engine::Item)>> {
    let last = crate::sync::last_applied(&ctx.state_dir)?;
    let mut rows = Vec::new();
    for i in &plan.items {
        let rec = last
            .get(&(i.target.clone(), i.path.clone(), i.block.clone()))
            .filter(|r| r.matches(&ctx.env));
        let state = crate::sync::classify(
            i.current.as_deref(),
            rec.map(|r| r.hash.as_deref()),
            i.planned.as_deref(),
        );
        let detail = (rec.is_none() && state == crate::sync::State::BothChanged)
            .then(|| "differs, and there is no record of an apply".to_string());
        rows.push((
            StatusRow {
                target: i.target.clone(),
                item: i.item.clone(),
                path: Some(i.path.clone()),
                state,
                detail,
            },
            i.clone(),
        ));
    }
    Ok(rows)
}

/// What the planned targets hold that `profile` doesn't (see
/// [`crate::sync::tool_only`]).
fn tool_only_items(
    ctx: &Ctx,
    profile: &Profile,
    plan: &Plan,
    targets: &[&'static dyn Adapter],
) -> Result<Vec<crate::sync::ToolOnly>> {
    let caps: Vec<(&str, Option<adapters::Captured>)> = targets
        .iter()
        .filter(|a| plan.targets.iter().any(|t| t == a.id()))
        .map(|a| {
            let cap = (a.supports(ctx.env.scope, Feature::Rules) != adapters::Support::None)
                .then(|| a.capture(&ctx.env).ok())
                .flatten();
            (a.id(), cap)
        })
        .collect();
    let refs: Vec<(&str, Option<&adapters::Captured>)> =
        caps.iter().map(|(id, c)| (*id, c.as_ref())).collect();
    crate::sync::tool_only(&ctx.state_dir, &ctx.env, profile, plan, &refs)
}

/// One note per target naming what is only in it, for `diff`.
fn tool_only_notes(found: &[crate::sync::ToolOnly], env: &Env) -> Vec<adapters::Note> {
    let mut by_target: BTreeMap<&str, Vec<String>> = BTreeMap::new();
    for t in found {
        by_target
            .entry(t.target.as_str())
            .or_default()
            .push(format!("{} ({})", t.item(), tildify(&t.path, &env.home)));
    }
    by_target
        .into_iter()
        .map(|(target, items)| adapters::Note {
            level: adapters::Level::Info,
            target: target.to_string(),
            message: format!(
                "only in the tool, not in the profile: {}. 'kitbag status' lists them; 'kitbag sync PROFILE --from {target} --adopt' pulls them into the profile",
                items.join(", ")
            ),
        })
        .collect()
}

/// MCP servers, commands, skills and agents of `profile` (as loaded, before any
/// remote restrictions) compared with each planned target: names only.
/// Status rows for the profile's hooks: the ones that would be written (as
/// with `--apply-hooks`) are `not applied` or `changed`, the others in sync;
/// hooks a target has no place or event for, or a git source withholds, say so.
fn hook_rows(
    ctx: &Ctx,
    resolved: &source::Resolved,
    loaded: &Profile,
    targets: &[&'static dyn Adapter],
) -> Result<Vec<StatusRow>> {
    use crate::adapters::hook_files::{format_for, no_hooks_reason};
    use crate::sync::State;
    let mut rows = Vec::new();
    if loaded.hooks.is_empty() {
        return Ok(rows);
    }
    let args = ApplyArgs {
        apply_hooks: true,
        ..ApplyArgs::default()
    };
    let (kept, plan) = plan_apply(
        ctx,
        resolved,
        loaded.clone(),
        targets,
        &args,
        &BTreeSet::new(),
    )?;
    let withheld = kept.hooks.is_empty();
    for a in targets {
        let id = a.id();
        if !plan.targets.iter().any(|t| t == id) {
            continue;
        }
        let row = |item: String, state, detail: Option<String>, path: Option<PathBuf>| StatusRow {
            target: id.to_string(),
            item,
            path,
            state,
            detail,
        };
        let Some(fmt) = format_for(id) else {
            let why = no_hooks_reason(id).unwrap_or("kitbag doesn't know where it keeps hooks");
            for h in &loaded.hooks {
                rows.push(row(
                    format!("hook {}", crate::hooks::describe(h)),
                    State::Unsupported,
                    Some(format!("not written: {why}")),
                    None,
                ));
            }
            continue;
        };
        let mut writes: Vec<(&str, Option<&str>)> = Vec::new();
        for w in plan.hook_writes.iter().filter(|w| w.target == id) {
            writes.push((w.event.as_str(), w.matcher.as_deref()));
            let there = fmt.read_doc(&w.path).is_some_and(|d| {
                !fmt.find_handlers(&d, &w.event, w.matcher.as_deref(), &w.command)
                    .is_empty()
            });
            let item = format!(
                "hook {}{}: {}",
                w.event,
                w.matcher
                    .as_ref()
                    .map(|m| format!(" [{m}]"))
                    .unwrap_or_default(),
                w.command
            );
            let state = if there {
                State::Changed
            } else {
                State::NotApplied
            };
            rows.push(row(item, state, None, Some(w.path.clone())));
        }
        for h in &loaded.hooks {
            let item = format!("hook {}", crate::hooks::describe(h));
            match fmt.native_event(&h.event) {
                None => rows.push(row(
                    item,
                    State::Unsupported,
                    Some(format!("no equivalent event in {}", fmt.name)),
                    None,
                )),
                Some(_) if withheld => rows.push(row(
                    item,
                    State::NotApplied,
                    Some(
                        "withheld: the profile comes from a remote repository (needs --trust-source and --apply-hooks)"
                            .to_string(),
                    ),
                    None,
                )),
                Some(_)
                    if plan
                        .hook_skipped
                        .iter()
                        .any(|(t, d)| t == id && *d == crate::hooks::describe(h)) =>
                {
                    rows.push(row(
                        item,
                        State::NotApplied,
                        Some("apply refuses to write it; see the warning 'hook not applied' in 'kitbag diff'".to_string()),
                        None,
                    ));
                }
                Some(ev) if !writes.iter().any(|(e, _)| *e == ev) => {
                    rows.push(row(item, State::InSync, None, None));
                }
                Some(_) => {}
            }
        }
    }
    Ok(rows)
}

fn inventory_rows(
    env: &Env,
    profile: &Profile,
    plan: &Plan,
    targets: &[&'static dyn Adapter],
) -> Vec<StatusRow> {
    use crate::sync::State;
    let mut rows = Vec::new();
    for a in targets {
        let id = a.id();
        if !plan.targets.iter().any(|t| t == id) {
            continue;
        }
        // What the tool has now; without it, "added" and "changed" can't be told apart.
        let present = a.capture(env).ok().map(|c| c.profile);
        let changed_server = |n: &str| {
            plan.servers
                .iter()
                .chain(&plan.withheld)
                .any(|s| s.name == n && s.targets.iter().any(|t| t == id))
        };
        let changed_item =
            |item: &str| plan.changed_items.iter().any(|(t, i)| t == id && i == item);
        let mut disabled_rows = Vec::new();
        let skipped = profile.skipped_items(id);
        let mut add = |item: String, in_profile: bool, changed: bool, in_tool: Option<bool>| {
            let state = match (in_profile, changed, in_tool) {
                (true, true, Some(false)) => State::Added,
                (true, true, _) => State::Changed,
                (true, false, Some(false)) => return,
                (true, false, _) => State::InSync,
                (false, _, Some(true)) => State::Removed,
                (false, _, _) => return,
            };
            rows.push(StatusRow {
                target: id.to_string(),
                item,
                path: None,
                state,
                detail: None,
            });
        };
        if a.supports(env.scope, Feature::Mcp) != adapters::Support::None {
            let mut names: Vec<&String> = profile
                .mcp_servers
                .iter()
                .filter(|(_, s)| s.applies_to(id))
                .map(|(n, _)| n)
                .collect();
            if let Some(p) = &present {
                names.extend(
                    p.mcp_servers
                        .keys()
                        .filter(|n| !profile.mcp_servers.contains_key(*n)),
                );
            }
            for n in names {
                let in_profile = profile.mcp_servers.get(n).is_some_and(|s| s.applies_to(id));
                let in_tool = present.as_ref().map(|p| p.mcp_servers.contains_key(n));
                add(format!("mcp {n}"), in_profile, changed_server(n), in_tool);
            }
            // Disabled for this tool in the profile, but still in it.
            for n in profile
                .mcp_servers
                .iter()
                .filter(|(_, s)| !s.applies_to(id))
                .map(|(n, _)| n)
            {
                let Some(tool_entry) = present.as_ref().and_then(|p| p.mcp_servers.get(n)) else {
                    continue;
                };
                // Already off with the tool's own flag or list: nothing to do.
                if tool_entry.disabled {
                    disabled_rows.push(StatusRow {
                        target: id.to_string(),
                        item: format!("mcp {n}"),
                        path: None,
                        state: State::InSync,
                        detail: Some(
                            "disabled for this tool in the profile; turned off in the tool"
                                .to_string(),
                        ),
                    });
                    continue;
                }
                let turns_off = plan
                    .turned_off_servers
                    .iter()
                    .any(|s| s.name == *n && s.targets.iter().any(|t| t == id));
                if turns_off {
                    disabled_rows.push(StatusRow {
                        target: id.to_string(),
                        item: format!("mcp {n}"),
                        path: None,
                        state: State::Changed,
                        detail: Some(
                            "disabled for this tool in the profile; apply turns it off".to_string(),
                        ),
                    });
                    continue;
                }
                let removes = plan
                    .removed_servers
                    .iter()
                    .any(|s| s.name == *n && s.targets.iter().any(|t| t == id));
                let (state, detail) = if removes {
                    (
                        State::Changed,
                        "disabled for this tool in the profile; apply removes it",
                    )
                } else {
                    (
                        State::Removed,
                        "disabled for this tool in the profile; apply leaves it ('kitbag diff' says why)",
                    )
                };
                disabled_rows.push(StatusRow {
                    target: id.to_string(),
                    item: format!("mcp {n}"),
                    path: None,
                    state,
                    detail: Some(detail.to_string()),
                });
            }
        }
        for (feature, kind, wanted, have) in [
            (
                Feature::Commands,
                "command",
                profile
                    .commands
                    .iter()
                    .map(|c| c.name.clone())
                    .collect::<Vec<_>>(),
                present.as_ref().map(|p| {
                    p.commands
                        .iter()
                        .map(|c| c.name.clone())
                        .collect::<Vec<_>>()
                }),
            ),
            (
                Feature::Skills,
                "skill",
                profile.skills.iter().map(|s| s.name.clone()).collect(),
                present
                    .as_ref()
                    .map(|p| p.skills.iter().map(|s| s.name.clone()).collect()),
            ),
            (
                Feature::Agents,
                "agent",
                profile.agents.iter().map(|s| s.name.clone()).collect(),
                present
                    .as_ref()
                    .map(|p| p.agents.iter().map(|s| s.name.clone()).collect()),
            ),
            (
                Feature::OutputStyles,
                "output-style",
                profile
                    .output_styles
                    .iter()
                    .map(|s| s.name.clone())
                    .collect(),
                present
                    .as_ref()
                    .map(|p| p.output_styles.iter().map(|s| s.name.clone()).collect()),
            ),
        ] {
            if a.supports(env.scope, feature) == adapters::Support::None {
                continue;
            }
            // Items the profile keeps off this tool get a row of their own below.
            let off: Vec<&str> = skipped
                .iter()
                .filter(|s| s.kind == kind)
                .map(|s| s.name.as_str())
                .collect();
            let wanted: Vec<String> = wanted
                .into_iter()
                .filter(|n| !off.contains(&n.as_str()))
                .collect();
            let mut names = wanted.clone();
            if let Some(h) = &have {
                names.extend(
                    h.iter()
                        .filter(|n| !wanted.contains(n) && !off.contains(&n.as_str()))
                        .cloned(),
                );
            }
            for n in names {
                let item = format!("{kind} {n}");
                let in_tool = have.as_ref().map(|h| h.contains(&n));
                add(
                    item.clone(),
                    wanted.contains(&n),
                    changed_item(&item),
                    in_tool,
                );
            }
        }
        // Rules, commands, skills and agents disabled for this tool in the
        // profile, but still in it.
        for s in &skipped {
            let feature = match s.kind {
                "rule" => Feature::Rules,
                "command" => Feature::Commands,
                "skill" => Feature::Skills,
                "output-style" => Feature::OutputStyles,
                _ => Feature::Agents,
            };
            let in_tool = present.as_ref().is_some_and(|p| match s.kind {
                "rule" => p.rules.iter().any(|x| x.name == s.name),
                "command" => p.commands.iter().any(|x| x.name == s.name),
                "skill" => p.skills.iter().any(|x| x.name == s.name),
                "output-style" => p.output_styles.iter().any(|x| x.name == s.name),
                _ => p.agents.iter().any(|x| x.name == s.name),
            });
            if !in_tool || a.supports(env.scope, feature) == adapters::Support::None {
                continue;
            }
            let label = s.label();
            let removes = plan
                .removed_items
                .iter()
                .any(|(t, i)| t == id && *i == label);
            let (state, detail) = if removes {
                (
                    State::Changed,
                    "disabled for this tool in the profile; apply removes it",
                )
            } else {
                (
                    State::Removed,
                    "disabled for this tool in the profile; apply leaves it ('kitbag diff' says why)",
                )
            };
            disabled_rows.push(StatusRow {
                target: id.to_string(),
                item: label,
                path: None,
                state,
                detail: Some(detail.to_string()),
            });
        }
        rows.extend(disabled_rows);
    }
    rows
}

/// `, N hook(s)` for a profile with hooks, else nothing.
fn hook_count(p: &Profile) -> String {
    if p.hooks.is_empty() {
        String::new()
    } else {
        format!(", {} hook(s)", p.hooks.len())
    }
}

/// `, N plugin(s)` for a profile with plugins, else nothing.
fn plugin_count(p: &Profile) -> String {
    let n: usize = p.plugins.values().map(|s| s.enabled.len()).sum();
    if n == 0 {
        String::new()
    } else {
        format!(", {n} plugin(s)")
    }
}

/// `, N agent(s)` for a profile with agents, else nothing.
fn agent_count(p: &Profile) -> String {
    if p.agents.is_empty() {
        String::new()
    } else {
        format!(", {} agent(s)", p.agents.len())
    }
}

/// `, N output style(s)` for a profile with output styles, else nothing.
fn output_style_count(p: &Profile) -> String {
    if p.output_styles.is_empty() {
        String::new()
    } else {
        format!(", {} output style(s)", p.output_styles.len())
    }
}

fn print_status(rows: &[StatusRow], env: &Env, as_json: bool) -> Result<()> {
    if as_json {
        let v: Vec<_> = rows
            .iter()
            .map(|r| {
                json!({
                    "target": r.target, "item": r.item,
                    "path": r.path.as_ref().map(|p| p.display().to_string()),
                    "state": r.state.as_str(), "detail": r.detail,
                })
            })
            .collect();
        println!("{}", serde_json::to_string_pretty(&v)?);
        return Ok(());
    }
    if rows.is_empty() {
        println!("Nothing to compare: no managed instructions found.");
        return Ok(());
    }
    for r in rows {
        let label = format!("{:<16}", r.state.as_str());
        let state = match r.state {
            crate::sync::State::InSync => paint(GREEN, label),
            crate::sync::State::Unsupported => paint(DIM, label),
            s if s.is_conflict() => paint(RED, label),
            _ => paint(YELLOW, label),
        };
        let at = r
            .path
            .as_ref()
            .map(|p| tildify(p, &env.home))
            .unwrap_or_default();
        println!(
            "  {:<14} {:<22} {} {}",
            r.target,
            visible_line(&r.item),
            state,
            paint(DIM, at)
        );
        if let Some(d) = &r.detail {
            println!("  {:<14} {}", "", paint(DIM, visible_line(d)));
        }
    }
    Ok(())
}

fn cmd_status(ctx: &Ctx, src: Option<&str>, s: &StatusArgs) -> Result<i32> {
    let env = &ctx.env;
    let mut rows: Vec<StatusRow> = Vec::new();
    match src {
        Some(src) => {
            let (resolved, profile) = load_source(
                src,
                s.path.as_deref(),
                source::FetchOptions {
                    allow_insecure: s.allow_insecure_source,
                    require_pin: s.require_pin,
                },
            )?;
            let targets = select_targets(s.to.as_deref().unwrap_or("detected"), env, "to")?;
            let loaded = profile.clone();
            let (profile, plan) = plan_apply(
                ctx,
                &resolved,
                profile,
                &targets,
                &ApplyArgs::default(),
                &BTreeSet::new(),
            )?;
            rows.extend(instruction_rows(ctx, &plan)?.into_iter().map(|(r, _)| r));
            rows.extend(
                tool_only_items(ctx, &profile, &plan, &targets)?
                    .into_iter()
                    .map(|t| StatusRow {
                        item: t.item(),
                        detail: Some(t.detail()),
                        target: t.target,
                        path: Some(t.path),
                        state: crate::sync::State::ToolOnly,
                    }),
            );
            rows.extend(inventory_rows(env, &loaded, &plan, &targets));
            rows.extend(hook_rows(ctx, &resolved, &loaded, &targets)?);
            let has_text = profile.instructions.is_some() || !profile.rules.is_empty();
            for a in &targets {
                if has_text
                    && a.supports(env.scope, Feature::Instructions) == adapters::Support::None
                {
                    rows.push(StatusRow {
                        target: a.id().to_string(),
                        item: "instructions".into(),
                        path: None,
                        state: crate::sync::State::Unsupported,
                        detail: Some(unsupported_detail(*a, env)),
                    });
                }
            }
            let applied = match &resolved.commit {
                Some(_) => Store::new(&ctx.state_dir)
                    .applied_commit(&resolved.source)?
                    .map(|g| g.commit),
                None => None,
            };
            if let (Some(now), Some(before)) = (&resolved.commit, &applied)
                && now != before
            {
                rows.insert(
                    0,
                    StatusRow {
                        target: "source".into(),
                        item: "commit".into(),
                        path: None,
                        state: crate::sync::State::ProfileChanged,
                        detail: Some(format!(
                            "moved since the last apply: applied {before}, now {now}"
                        )),
                    },
                );
            }
            if !s.json {
                println!(
                    "{} '{}' from {} ({})",
                    paint(BOLD, "Profile"),
                    visible_line(&profile.name),
                    visible_line(&resolved.origin),
                    where_label(env)
                );
                if let Some(now) = &resolved.commit {
                    println!("  commit   {}", visible_line(now));
                    match &applied {
                        Some(before) => println!("  applied  {}", visible_line(before)),
                        None => println!(
                            "  applied  {}",
                            paint(DIM, "no recorded apply of this source")
                        ),
                    }
                }
                println!();
            }
        }
        None => {
            let only: Option<Vec<&'static str>> = match &s.to {
                Some(to) => Some(
                    select_targets(to, env, "to")?
                        .iter()
                        .map(|a| a.id())
                        .collect(),
                ),
                None => None,
            };
            let mut txs: BTreeSet<String> = BTreeSet::new();
            for r in crate::sync::last_applied(&ctx.state_dir)?.into_values() {
                if !r.matches(env)
                    || only
                        .as_ref()
                        .is_some_and(|o| !o.contains(&r.target.as_str()))
                {
                    continue;
                }
                txs.extend(r.tx.clone());
                let current = crate::sync::item_text(
                    crate::plan_fs::read_opt(&r.path)?.as_deref(),
                    r.block.as_deref(),
                );
                rows.push(StatusRow {
                    state: crate::sync::classify_since_apply(current.as_deref(), r.hash.as_deref()),
                    target: r.target,
                    item: r.item,
                    path: Some(r.path),
                    detail: None,
                });
            }
            if !s.json {
                println!(
                    "{} since the last apply ({})",
                    paint(BOLD, "Changes"),
                    where_label(env)
                );
                // The git sources those applies came from, at the commit applied.
                let mut from: Vec<crate::txn::GitCommit> = Vec::new();
                for t in Store::new(&ctx.state_dir).list()? {
                    if let Some(g) = t.git.filter(|_| txs.contains(&t.id))
                        && !from.contains(&g)
                    {
                        from.push(g);
                    }
                }
                for g in from {
                    println!(
                        "  from {} @ {}",
                        visible_line(&g.source),
                        visible_line(&g.commit)
                    );
                }
                println!();
            }
        }
    }
    let drift = rows
        .iter()
        .any(|r| r.state.is_drift() || (s.strict && r.state.is_tool_only()));
    print_status(&rows, env, s.json)?;
    if !s.json && rows.iter().any(|r| r.state == crate::sync::State::ToolOnly) {
        println!(
            "\n{}",
            paint(
                DIM,
                "Tool only: a rule file, or text outside kitbag's block, that the profile doesn't have. 'kitbag sync PROFILE --from <target> --adopt' pulls it into the profile; '--exit-code --strict' counts it as drift."
            )
        );
    }
    if !s.json
        && rows.iter().any(|r| {
            matches!(
                r.state,
                crate::sync::State::Added
                    | crate::sync::State::Changed
                    | crate::sync::State::Removed
            )
        })
    {
        println!(
            "\n{}",
            paint(
                DIM,
                "MCP servers, commands, skills, agents: added = in the profile, not in the tool; changed = differs from the profile; removed = in the tool, not in the profile (sync leaves it)."
            )
        );
    }
    if !s.json && rows.iter().any(|r| r.state.is_conflict()) {
        println!(
            "\n{}",
            paint(
                DIM,
                "Edited in the tool: 'kitbag sync PROFILE --from <target>' pulls the edit into the profile, '--overwrite' replaces it (backed up)."
            )
        );
    }
    Ok(i32::from(s.exit_code && drift))
}

fn cmd_sync(ctx: &Ctx, src: &str, t: &TargetArgs, a: &ApplyArgs, s: &SyncArgs) -> Result<i32> {
    let env = &ctx.env;
    let (resolved, loaded) = load_source(src, t.path.as_deref(), t.fetch())?;
    if resolved.remote && s.from.is_some() {
        bail!(
            "--from can't be used with a git source: kitbag can't write edits back into a repository it cloned. Clone it, sync the local directory with --from, then commit and push"
        );
    }
    if s.from.is_some() && profile_io::is_plugin(&resolved.dir) {
        bail!(
            "--from can't be used with a plugin: kitbag only writes edits back into a profile (a directory with {}). Edit the plugin's rules/ by hand",
            profile_io::MANIFEST
        );
    }
    let targets = select_targets(&t.to, env, "to")?;
    let none = BTreeSet::new();
    let (mut profile, mut plan) = plan_apply(ctx, &resolved, loaded.clone(), &targets, a, &none)?;
    let mut profile_changes = Vec::new();
    let mut pulled_notes = Vec::new();
    // Rule files just adopted from the tool: the profile's copy replaces them
    // there (same rule, as kitbag writes it), so they aren't held back.
    let mut adopted: Vec<PathBuf> = Vec::new();
    if let Some(from) = &s.from {
        let Some(source_target) = targets
            .iter()
            .copied()
            .find(|x| x.id() == adapters::canonical_id(from))
        else {
            bail!("--from {from} must be one of the targets being synced (see --to)");
        };
        let from = source_target.id().to_string();
        let rows = instruction_rows(ctx, &plan)?;
        let edited: Vec<&engine::Item> = rows
            .iter()
            .filter(|(r, _)| r.target == source_target.id() && r.state.is_conflict())
            .map(|(_, i)| i)
            .collect();
        let (tool_only, taken_out): (Vec<crate::sync::ToolOnly>, Vec<_>) =
            tool_only_items(ctx, &profile, &plan, &[source_target])?
                .into_iter()
                .partition(|t| {
                    !matches!(
                        t.what,
                        crate::sync::ToolOnlyKind::Rule {
                            was_applied: true,
                            ..
                        }
                    )
                });
        // A rule in a sub-directory (`team/x.md`) would be written back flat
        // (`team-x.md`), leaving the tool with two files of the same name.
        let rules_dir: Option<PathBuf> = source_target
            .locations(env)
            .into_iter()
            .find(|(k, _)| *k == "rules")
            .map(|(_, d)| d);
        let (tool_only, nested): (Vec<crate::sync::ToolOnly>, Vec<_>) =
            tool_only.into_iter().partition(|t| match &t.what {
                crate::sync::ToolOnlyKind::Rule { .. } => {
                    match (t.path.parent(), rules_dir.as_deref()) {
                        (Some(parent), Some(dir)) => crate::sync::same_file(parent, dir),
                        _ => true,
                    }
                }
                crate::sync::ToolOnlyKind::Text { .. } => true,
            });
        if s.adopt {
            for t in &nested {
                pulled_notes.push(adapters::Note {
                    level: adapters::Level::Warn,
                    target: from.clone(),
                    message: format!(
                        "{} ({}) was not adopted: it is in a sub-directory, and kitbag would write it back flat next to it. Move it up into the rules directory first",
                        t.item(),
                        tildify(&t.path, &env.home)
                    ),
                });
            }
            for t in &taken_out {
                pulled_notes.push(adapters::Note {
                    level: adapters::Level::Info,
                    target: from.clone(),
                    message: format!(
                        "{} ({}) was not adopted: an earlier apply wrote it from this profile, and it has been taken out of the profile since. Delete the file, or add the rule back to the profile by hand",
                        t.item(),
                        tildify(&t.path, &env.home)
                    ),
                });
            }
        }
        if s.adopt {
            adopted = tool_only.iter().map(|t| t.path.clone()).collect();
        } else if !tool_only.is_empty() {
            let items: Vec<String> = tool_only
                .iter()
                .map(|t| format!("{} ({})", t.item(), tildify(&t.path, &env.home)))
                .collect();
            pulled_notes.push(adapters::Note {
                level: adapters::Level::Info,
                target: from.clone(),
                message: format!(
                    "not pulled back, as they were never in the profile: {}. Add --adopt to pull them in too",
                    items.join(", ")
                ),
            });
        }
        let adopt: &[crate::sync::ToolOnly] = if s.adopt { &tool_only } else { &[] };
        if edited.is_empty() && adopt.is_empty() {
            pulled_notes.push(adapters::Note {
                level: adapters::Level::Info,
                target: from.clone(),
                message: if s.adopt {
                    "nothing to pull back: its managed instructions match the last apply, and it has nothing the profile doesn't"
                } else {
                    "no edits to pull back: its managed instructions match the last apply"
                }
                .into(),
            });
        } else {
            let pb = crate::sync::pull_back(
                &loaded,
                &resolved.dir,
                &edited,
                adopt,
                &slugify(&loaded.name),
            )?;
            pulled_notes.extend(pb.notes);
            profile_changes = pb.changes;
            (profile, plan) = plan_apply(ctx, &resolved, pb.profile, &targets, a, &none)?;
        }
    }
    // Hold back only the items edited in the tool (a managed block or a rule
    // file); everything else for that tool still applies.
    if !s.overwrite {
        let hold: BTreeSet<(PathBuf, Option<String>)> = instruction_rows(ctx, &plan)?
            .into_iter()
            .filter(|(r, i)| {
                r.state.is_conflict()
                    && !(i.block.is_none()
                        && adopted.iter().any(|p| crate::sync::same_file(p, &i.path)))
            })
            .map(|(_, i)| (i.path, i.block))
            .collect();
        if !hold.is_empty() {
            plan = plan_apply(ctx, &resolved, profile.clone(), &targets, a, &hold)?.1;
        }
    }
    plan.notes.extend(pulled_notes);
    let last = crate::sync::last_applied(&ctx.state_dir)?;
    for i in &plan.held_back {
        let never_applied = !last
            .get(&(i.target.clone(), i.path.clone(), i.block.clone()))
            .is_some_and(|r| r.matches(env));
        // A block's markers show kitbag wrote it, whatever the records say.
        let message = if never_applied && i.block.is_none() && i.current.is_some() {
            format!(
                "{} in {} was not synced: the tool already has its own, different copy there, and kitbag has no record of writing it. Compare the two; pull the tool's copy into the profile with --from {}, or replace it with --overwrite (backed up; 'kitbag undo' restores it)",
                held_label(&i.item),
                tildify(&i.path, &env.home),
                i.target
            )
        } else {
            format!(
                "{} in {} was not synced: it was edited in the tool since the last apply. Pull the edit into the profile with --from {}, or replace it with --overwrite (backed up; 'kitbag undo' restores it)",
                held_label(&i.item),
                tildify(&i.path, &env.home),
                i.target
            )
        };
        plan.notes.push(adapters::Note {
            level: adapters::Level::Warn,
            target: i.target.clone(),
            message,
        });
    }
    // An adopted rule file is written back in kitbag's format; say what that
    // drops (frontmatter keys kitbag doesn't carry, for one).
    for i in plan
        .items
        .iter()
        .filter(|i| i.block.is_none() && adopted.iter().any(|p| crate::sync::same_file(p, &i.path)))
    {
        let (Some(cur), Some(new)) = (i.current.as_deref(), i.planned.as_deref()) else {
            continue;
        };
        if crate::sync::hash(cur) == crate::sync::hash(new) {
            continue;
        }
        let (cur, new) = (
            crate::frontmatter::parse(cur),
            crate::frontmatter::parse(new),
        );
        let mut lost: Vec<String> = cur
            .data
            .keys()
            .filter(|k| !new.data.contains_key(*k))
            .map(|k| format!("'{k}' from the frontmatter"))
            .collect();
        if cur.body.trim() != new.body.trim() {
            lost.push("its text as written (kitbag's copy differs)".to_string());
        }
        plan.notes.push(adapters::Note {
            level: if lost.is_empty() {
                adapters::Level::Info
            } else {
                adapters::Level::Warn
            },
            target: i.target.clone(),
            message: if lost.is_empty() {
                format!(
                    "{} in {} is rewritten in kitbag's format (backed up; 'kitbag undo' restores it)",
                    i.item,
                    tildify(&i.path, &env.home)
                )
            } else {
                format!(
                    "{} in {} is rewritten in kitbag's format, which drops: {} (backed up; 'kitbag undo' restores it)",
                    i.item,
                    tildify(&i.path, &env.home),
                    visible_line(&lost.join(", "))
                )
            },
        });
    }
    let held = !plan.held_back.is_empty();
    plan.changes.extend(profile_changes);
    let code = present(
        ctx,
        &resolved,
        &profile,
        &plan,
        Mode::Apply {
            yes: s.yes,
            dry_run: s.dry_run,
        },
    )?;
    Ok(if code == 0 && held { 1 } else { code })
}

/// How a held-back item is named in notes: `instructions block`, `rule rust`.
fn held_label(item: &str) -> String {
    match item {
        "instructions" | "rules" => format!("{item} block"),
        other => other.to_string(),
    }
}

fn cmd_undo(ctx: &Ctx, id: Option<&str>, force: bool) -> Result<i32> {
    let store = Store::new(&ctx.state_dir);
    let done = store.undo_with_home(id, force, Some(&ctx.env.home))?;
    let tx = &done.tx;
    println!(
        "{} {}{} ({})",
        paint(GREEN, "Undid"),
        if done.interrupted {
            "interrupted apply "
        } else {
            ""
        },
        visible_line(&tx.id),
        visible_line(&source::strip_url_credentials(&tx.description))
    );
    for e in &tx.entries {
        if done.refused.iter().any(|r| r.path == e.path) {
            continue;
        }
        if done.untouched.contains(&e.path) {
            println!(
                "  {}  {} (the apply stopped before writing it)",
                paint(DIM, "unchanged"),
                tildify(&e.path, &ctx.env.home)
            );
            continue;
        }
        let what = if e.existed { "restored" } else { "removed" };
        println!(
            "  {}  {}",
            paint(DIM, what),
            tildify(&e.path, &ctx.env.home)
        );
    }
    for r in &done.refused {
        println!(
            "  {}  {}: {}",
            paint(RED, "left alone"),
            tildify(&r.path, &ctx.env.home),
            visible_line(&r.reason)
        );
        if let Some(b) = &r.backup {
            println!(
                "      the original is backed up in {}",
                tildify(b, &ctx.env.home)
            );
        }
    }
    for t in &done.tampered {
        println!(
            "  {}  {}: restored from a backup that changed after the apply (--force)",
            paint(YELLOW, "warning"),
            tildify(&t.path, &ctx.env.home)
        );
    }
    if done.refused.is_empty() {
        Ok(0)
    } else {
        println!(
            "\n{} {} file(s) are no longer where kitbag wrote them, so undo didn't touch them (--force doesn't change this). Check them by hand.",
            paint(YELLOW, "Note:"),
            done.refused.len()
        );
        Ok(1)
    }
}

fn cmd_history(ctx: &Ctx, as_json: bool) -> Result<i32> {
    let list = Store::new(&ctx.state_dir).list()?;
    if as_json {
        let v: Vec<_> = list
            .iter()
            .map(|t| {
                json!({
                    "id": t.id, "created": t.created_at(), "status": t.status,
                    "description": source::strip_url_credentials(&t.description), "targets": t.targets,
                    "files": t.entries.iter().map(|e| e.path.display().to_string()).collect::<Vec<_>>(),
                })
            })
            .collect();
        println!("{}", serde_json::to_string_pretty(&v)?);
        return Ok(0);
    }
    if list.is_empty() {
        println!(
            "No applies recorded yet (state: {}).",
            ctx.state_dir.display()
        );
        return Ok(0);
    }
    for t in list.iter().rev() {
        let status = match t.status {
            Status::Applied => paint(GREEN, "applied"),
            Status::Undone => paint(DIM, "undone"),
            Status::RolledBack => paint(RED, "rolled-back"),
            Status::Pending => paint(YELLOW, "pending"),
        };
        println!(
            "{}  {}  {:<11}  {} file(s)  {}",
            paint(BOLD, visible_line(&t.id)),
            t.created_at(),
            status,
            t.entries.len(),
            visible_line(&source::strip_url_credentials(&t.description))
        );
    }
    Ok(0)
}

fn check_file(path: &Path) -> Option<String> {
    let text = match read_text_opt(path) {
        Ok(Some(t)) => t,
        Ok(None) => return None,
        Err(e) => return Some(format!("{e:#}")),
    };
    let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("");
    let result = match ext {
        "json" => jsonedit::parse_value(Some(&text), path).map(|_| ()),
        "toml" => tomledit::parse(Some(&text), path).map(|_| ()),
        _ => Ok(()),
    };
    result.err().map(|e| format!("{e:#}"))
}

/// What `doctor --mcp-tokens` was asked to do.
struct McpTokens {
    /// How long each server gets.
    timeout: std::time::Duration,
    /// Which servers of a git source may be started.
    git: engine::GitMcp,
}

fn cmd_doctor(
    ctx: &Ctx,
    src: Option<&str>,
    t: &TargetArgs,
    mcp_tokens: Option<McpTokens>,
) -> Result<i32> {
    let env = &ctx.env;
    let chosen = if t.to == "detected" {
        None
    } else {
        Some(select_targets(&t.to, env, "to")?)
    };
    let mut problems = 0;
    println!(
        "{}",
        paint(BOLD, format!("kitbag {}", env!("CARGO_PKG_VERSION")))
    );
    println!(
        "  home        {}",
        visible_line(&env.home.display().to_string())
    );
    println!(
        "  scope       {}{}",
        env.scope,
        if env.scope == Scope::Project {
            format!(" ({})", visible_line(&env.project.display().to_string()))
        } else {
            String::new()
        }
    );
    println!(
        "  state dir   {}",
        visible_line(&ctx.state_dir.display().to_string())
    );
    // An apply that was killed part-way through leaves its transaction
    // pending, with some files written and some not.
    let store = Store::new(&ctx.state_dir);
    for tx in store.interrupted()? {
        problems += 1;
        println!(
            "  {} apply {} from {} did not finish ({}); {} file(s) may be partly applied",
            paint(RED, "interrupted"),
            visible_line(&tx.id),
            tx.created_at(),
            visible_line(&source::strip_url_credentials(&tx.description)),
            tx.entries.len()
        );
        println!(
            "      unless an apply is still running, put them back with: kitbag undo {}",
            visible_line(&tx.id)
        );
    }
    let git = std::process::Command::new("git")
        .arg("--version")
        .output()
        .ok()
        .filter(|o| o.status.success());
    println!(
        "  git         {}",
        git.map_or_else(
            || paint(YELLOW, "not found (needed only for git URL sources)"),
            |o| visible_line(String::from_utf8_lossy(&o.stdout).trim()).into_owned()
        )
    );

    println!("\n{}", paint(BOLD, "Tools"));
    for a in adapters::all() {
        if let Some(chosen) = &chosen
            && !chosen.iter().any(|c| c.id() == a.id())
        {
            continue;
        }
        if !a.scopes().contains(&env.scope) {
            continue;
        }
        let detected = a.detect(env);
        println!(
            "  {} {}",
            if detected {
                paint(GREEN, "✓")
            } else {
                paint(DIM, "·")
            },
            a.name()
        );
        if !detected {
            continue;
        }
        let mut seen = Vec::new();
        for (_, path) in a.locations(env) {
            if seen.contains(&path) || !path.is_file() {
                continue;
            }
            if let Some(err) = check_file(&path) {
                problems += 1;
                println!("      {} {}", paint(RED, "error"), visible(&err));
            }
            seen.push(path);
        }
    }

    // The profile's servers, for --mcp-tokens: Some(None) when it didn't
    // load. The flag says whether it came from a git source.
    let mut profile_servers: Option<Option<(Profile, bool)>> = None;
    if let Some(src) = src {
        println!("\n{}", paint(BOLD, "Profile"));
        // Not load_source: doctor lists the profile's warnings itself.
        let loaded = source::resolve(src, t.path.as_deref(), t.fetch())
            .and_then(|r| profile_io::load_named(&r.dir, &r.name_hint).map(|p| (r, p)));
        match loaded {
            Err(e) => {
                problems += 1;
                profile_servers = Some(None);
                println!("  {} {}", paint(RED, "error"), visible(&format!("{e:#}")));
            }
            Ok((resolved, p)) => {
                if mcp_tokens.is_some() {
                    profile_servers = Some(Some((p.clone(), resolved.remote)));
                }
                println!(
                    "  '{}' from {}",
                    visible_line(&p.name),
                    visible_line(&resolved.origin)
                );
                println!(
                    "  {} MCP server(s), {} rule(s), {} command(s), {} skill(s){}{}{}{}{}",
                    p.mcp_servers.len(),
                    p.rules.len(),
                    p.commands.len(),
                    p.skills.len(),
                    agent_count(&p),
                    output_style_count(&p),
                    hook_count(&p),
                    plugin_count(&p),
                    if p.instructions.is_some() {
                        ", instructions"
                    } else {
                        ""
                    }
                );
                for w in &p.warnings {
                    println!("  {} {}", paint(YELLOW, "warn"), visible_line(w));
                }
                for leak in profile_io::scan_for_secrets(&p) {
                    problems += 1;
                    println!("  {} {}", paint(RED, "secret"), visible_line(&leak));
                }
                for (var, uses) in profile_io::required_env(&p) {
                    let set = env.vars.get(&var).is_some_and(|v| !v.is_empty());
                    println!(
                        "  {} ${{{}}}  {}",
                        if set {
                            paint(GREEN, "set  ")
                        } else {
                            paint(YELLOW, "unset")
                        },
                        visible_line(&var),
                        paint(DIM, visible_line(&uses.join(", ")))
                    );
                }
                for (target, settings) in &p.settings {
                    let shown_target = visible_line(target);
                    if adapters::get(target).is_none() {
                        println!(
                            "  {} settings.{shown_target}: unknown target",
                            paint(YELLOW, "warn")
                        );
                    }
                    let (allowed, dropped) = policy::filter(target, Some(settings));
                    if !dropped.is_empty() {
                        println!(
                            "  {} settings.{shown_target}: will not apply {}",
                            paint(YELLOW, "warn"),
                            visible_line(&dropped.join(", "))
                        );
                    }
                    for r in policy::risky(target, &allowed) {
                        println!("  {} {}", paint(YELLOW, "risky"), visible_line(&r));
                    }
                }
                // Not a problem: one-off rules and notes are allowed to live
                // in a tool, but they don't reach the other tools.
                let found = select_targets(&t.to, env, "to").and_then(|targets| {
                    let (profile, plan) = plan_apply(
                        ctx,
                        &resolved,
                        p.clone(),
                        &targets,
                        &ApplyArgs::default(),
                        &BTreeSet::new(),
                    )?;
                    tool_only_items(ctx, &profile, &plan, &targets)
                });
                match found {
                    Ok(found) => {
                        let mut targets: Vec<&str> = Vec::new();
                        for f in &found {
                            println!(
                                "  {} {} {} {}",
                                paint(YELLOW, "tool only"),
                                f.target,
                                visible_line(&f.item()),
                                paint(DIM, tildify(&f.path, &env.home))
                            );
                            if !targets.contains(&f.target.as_str()) {
                                targets.push(&f.target);
                            }
                        }
                        for t in targets {
                            println!(
                                "      {}",
                                paint(
                                    DIM,
                                    format!(
                                        "not in the profile, so the other tools don't get {t}'s; 'kitbag sync PROFILE --from {t} --adopt' pulls them in"
                                    )
                                )
                            );
                        }
                    }
                    Err(e) => println!(
                        "  {} could not check for rules and text only in a tool: {}",
                        paint(YELLOW, "warn"),
                        visible(&format!("{e:#}"))
                    ),
                }
            }
        }
    }
    // Hooks kitbag wrote: still what it wrote? Hooks you added yourself are not looked at.
    match crate::hooks::standing(&ctx.state_dir, env) {
        Ok((records, warning)) => {
            if let Some(w) = warning {
                println!("\n  {} {}", paint(YELLOW, "warn"), visible_line(&w));
            }
            if !records.is_empty() {
                println!("\n{}", paint(BOLD, "Hooks kitbag applied"));
                let changed = crate::hooks::drift(&records);
                if changed.is_empty() {
                    println!(
                        "  {} {} hook(s) match what kitbag applied",
                        paint(GREEN, "✓"),
                        records.len()
                    );
                }
                for c in changed {
                    problems += 1;
                    println!("  {} {}", paint(RED, "changed"), visible_line(&c));
                }
            }
        }
        Err(e) => println!(
            "\n  {} could not check the hooks kitbag applied: {}",
            paint(YELLOW, "warn"),
            visible(&format!("{e:#}"))
        ),
    }
    if let Some(opts) = mcp_tokens {
        doctor_mcp_tokens(env, chosen.as_deref(), profile_servers.as_ref(), &opts);
    }
    println!();
    if problems == 0 {
        println!("{}", paint(GREEN, "No problems found."));
        Ok(0)
    } else {
        println!("{}", paint(RED, format!("{problems} problem(s) found.")));
        Ok(1)
    }
}

/// One MCP server's line in `doctor --mcp-tokens`.
enum McpRow {
    Cost(mcp_tokens::Cost),
    Skipped(String),
    Failed(String),
}

/// How to start `s` for listing its tools, or why it isn't started.
fn mcp_launch(s: &McpServer, env: &Env) -> Result<mcp_tokens::Launch, McpRow> {
    if s.transport() != Transport::Stdio {
        return Err(McpRow::Skipped(format!(
            "remote ({}) server, not contacted: its URL and headers may carry credentials",
            s.transport().as_str()
        )));
    }
    let mut state = RenderState::default();
    let r = s.map_strings(|v| render_refs(v, RefStyle::Resolve, &env.vars, &mut state));
    if !state.missing.is_empty() {
        let vars: Vec<String> = state.missing.iter().map(|v| format!("${{{v}}}")).collect();
        return Err(McpRow::Skipped(format!(
            "not started: needs {}, which {} unset",
            vars.join(", "),
            if vars.len() == 1 { "is" } else { "are" }
        )));
    }
    let Some(command) = r.command.filter(|c| !c.trim().is_empty()) else {
        return Err(McpRow::Failed("it has no command".into()));
    };
    if let Some(cwd) = &r.cwd
        && !mcp_tokens::cwd_ok(cwd)
    {
        return Err(McpRow::Failed("its cwd is not a directory".into()));
    }
    Ok(mcp_tokens::Launch {
        command,
        args: r.args,
        env: r.env,
        cwd: r.cwd,
    })
}

/// `doctor --mcp-tokens`: per tool, the estimated tokens each MCP server's
/// tool definitions take up. `profile` is the checked profile and whether it
/// came from a git source (`Some(None)` when it didn't load); without one,
/// the servers in each tool's config.
fn doctor_mcp_tokens(
    env: &Env,
    chosen: Option<&[&'static dyn Adapter]>,
    profile: Option<&Option<(Profile, bool)>>,
    opts: &McpTokens,
) {
    let timeout = opts.timeout;
    println!("\n{}", paint(BOLD, "MCP tool definitions"));
    println!(
        "  {}",
        paint(
            DIM,
            format!(
                "Estimated tokens of each server's tool names, descriptions and input schemas, at about {} characters per token (not an exact tokenizer count). Local servers were started only to list their tools",
                mcp_tokens::CHARS_PER_TOKEN
            )
        )
    );
    let profile = match profile {
        Some(None) => {
            println!(
                "  {} the profile could not be loaded",
                paint(YELLOW, "skipped")
            );
            return;
        }
        Some(Some((p, remote))) => Some((crate::portable::expand_profile(p, env), *remote)),
        None => None,
    };
    // A git source's servers start only when allowed, as for apply.
    let git = match &profile {
        Some((_, true)) => Some(&opts.git),
        _ => None,
    };
    let profile = profile.map(|(p, _)| p);
    let targets: Vec<&'static dyn Adapter> = adapters::all()
        .iter()
        .copied()
        .filter(|a| match chosen {
            Some(c) => c.iter().any(|c| c.id() == a.id()),
            None => a.detect(env),
        })
        .filter(|a| {
            a.scopes().contains(&env.scope)
                && a.supports(env.scope, Feature::Mcp) != adapters::Support::None
        })
        .collect();
    if targets.is_empty() {
        println!("  no tool with MCP servers to check");
        return;
    }
    // The same server set up in several tools is started once.
    let mut seen: BTreeMap<mcp_tokens::Launch, Result<mcp_tokens::Cost, String>> = BTreeMap::new();
    for a in targets {
        println!("  {}", paint(BOLD, a.name()));
        let servers = match &profile {
            Some(p) => p.mcp_servers.clone(),
            None => match a.capture(env) {
                Ok(c) => crate::portable::expand_profile(&c.profile, env).mcp_servers,
                Err(e) => {
                    println!(
                        "    {} could not read its MCP servers: {}",
                        paint(RED, "error"),
                        visible_line(&format!("{e:#}"))
                    );
                    continue;
                }
            },
        };
        if servers.is_empty() {
            println!("    {}", paint(DIM, "no MCP servers"));
            continue;
        }
        let width = servers
            .keys()
            .map(|n| visible_line(n).chars().count())
            .max()
            .unwrap_or(0)
            .max("total".len());
        let (mut total, mut counted) = (0, 0);
        for (name, s) in &servers {
            let skip = match &profile {
                Some(_) => s
                    .skip_reason(a.id())
                    .map(|why| format!("disabled for this tool in the profile ({why})"))
                    .or_else(|| s.disabled.then(|| "disabled in the profile".to_string()))
                    .or_else(|| {
                        git.filter(|g| !g.allows(name)).map(|_| {
                            format!(
                                "from a git source, so not started (pass --allow-git-mcp {} or --trust-source to start it)",
                                visible_line(name)
                            )
                        })
                    }),
                None => s
                    .disabled
                    .then(|| format!("disabled in {}'s config", a.name())),
            };
            let row = match skip {
                Some(why) => McpRow::Skipped(format!("{why}; not counted")),
                None => match mcp_launch(s, env) {
                    Err(row) => row,
                    Ok(launch) => {
                        let got = seen
                            .entry(launch)
                            .or_insert_with_key(|l| {
                                mcp_tokens::list_tools(l, timeout)
                                    .map(|tools| mcp_tokens::estimate(&tools))
                            })
                            .clone();
                        match got {
                            Ok(cost) => McpRow::Cost(cost),
                            Err(e) => McpRow::Failed(e),
                        }
                    }
                },
            };
            let shown = visible_line(name);
            let pad = " ".repeat(width.saturating_sub(shown.chars().count()));
            let line = match row {
                McpRow::Cost(c) => {
                    total += c.tokens;
                    counted += 1;
                    format!(
                        "~{} tokens  {}",
                        mcp_tokens::thousands(c.tokens),
                        paint(DIM, format!("{} tool(s)", c.tools))
                    )
                }
                McpRow::Skipped(why) => {
                    format!("{} {}", paint(DIM, "skipped"), visible_line(&why))
                }
                McpRow::Failed(why) => {
                    format!("{} {}", paint(YELLOW, "failed"), visible_line(&why))
                }
            };
            println!("    {shown}{pad}  {line}");
        }
        println!(
            "    {:<width$}  ~{} tokens from {counted} server(s)",
            "total",
            mcp_tokens::thousands(total)
        );
    }
}

fn cmd_doctor_why(home: Option<&Path>, dir: &Path, to: &str) -> Result<i32> {
    let dir = why::normalize(dir).with_context(|| format!("resolving {}", dir.display()))?;
    if !dir.is_dir() {
        bail!("{} is not a directory", dir.display());
    }
    let place = Place {
        scope: Some(Scope::Project),
        project: Some(dir.clone()),
    };
    let ctx = context(home, Some(&place))?;
    let env = &ctx.env;
    let ids: Vec<&'static str> = if matches!(to.trim(), "" | "detected") {
        // Any tool may be started in a project, installed here or not.
        why::TOOLS.to_vec()
    } else {
        select_targets(to, env, "to")?
            .iter()
            .map(|a| a.id())
            .collect()
    };
    let place = why::Place {
        env,
        dir: dir.clone(),
        sandboxed: home.is_some(),
    };
    println!(
        "{}",
        paint(
            BOLD,
            format!("Instruction files for {}", tildify(&dir, &env.home))
        )
    );
    println!(
        "{}",
        paint(
            DIM,
            "What each tool reads when started here, in load order; nothing is run or changed"
        )
    );
    for id in ids {
        let report = why::explain(id, &place);
        println!("\n{}", paint(BOLD, report.name));
        let mut n = 0;
        for e in &report.entries {
            let label = match e.status {
                why::Status::Loaded => {
                    n += 1;
                    paint(GREEN, format!("{:<10}", format!("load {n}")))
                }
                why::Status::Later => paint(CYAN, format!("{:<10}", e.status.label())),
                why::Status::Skipped | why::Status::Unverified => {
                    paint(YELLOW, format!("{:<10}", e.status.label()))
                }
                why::Status::Trap => paint(RED, format!("{:<10}", e.status.label())),
                why::Status::Note => paint(DIM, format!("{:<10}", e.status.label())),
            };
            let path = e
                .path
                .as_deref()
                .map(|p| format!("{}  ", tildify(p, &env.home)))
                .unwrap_or_default();
            println!("  {label} {path}{}", paint(DIM, visible_line(&e.why)));
        }
    }
    Ok(0)
}

fn cmd_list_targets(ctx: &Ctx, as_json: bool) -> Result<i32> {
    let env = &ctx.env;
    if as_json {
        let v: Vec<_> = adapters::all()
            .iter()
            .map(|a| {
                let support: serde_json::Map<String, serde_json::Value> = Feature::ALL
                    .iter()
                    .map(|f| (f.as_str().to_string(), json!(a.supports(env.scope, *f).as_str())))
                    .collect();
                json!({
                    "id": a.id(), "name": a.name(), "docs": a.docs(),
                    "scopes": a.scopes().iter().map(ToString::to_string).collect::<Vec<_>>(),
                    "detected": a.scopes().contains(&env.scope) && a.detect(env),
                    "support": support,
                    "locations": a.scopes().contains(&env.scope).then(|| a.locations(env).into_iter()
                        .map(|(k, p)| (k.to_string(), json!(p.display().to_string()))).collect::<serde_json::Map<_, _>>()),
                })
            })
            .collect();
        println!("{}", serde_json::to_string_pretty(&v)?);
        return Ok(0);
    }
    println!("{}", paint(BOLD, format!("Targets ({} scope)", env.scope)));
    let header: Vec<&str> = Feature::ALL.iter().map(|f| f.as_str()).collect();
    println!(
        "  {:<16} {:<9} {}",
        "id",
        "detected",
        header
            .iter()
            .map(|h| format!("{h:<14}"))
            .collect::<String>()
    );
    for a in adapters::all() {
        let in_scope = a.scopes().contains(&env.scope);
        let detected = if !in_scope {
            paint(DIM, format!("{:<9}", "n/a"))
        } else if a.detect(env) {
            paint(GREEN, format!("{:<9}", "yes"))
        } else {
            paint(DIM, format!("{:<9}", "no"))
        };
        let cells: String = Feature::ALL
            .iter()
            .map(|f| format!("{:<14}", a.supports(env.scope, *f).as_str()))
            .collect();
        println!(
            "  {:<16} {detected} {cells}",
            paint(CYAN, format!("{:<16}", a.id()))
        );
    }
    println!(
        "\n{}",
        paint(
            DIM,
            "native = first-class support, folded = merged into the instructions file, - = not supported"
        )
    );
    Ok(0)
}

/// Print an error chain the way users expect.
pub fn report_error(e: &anyhow::Error) {
    eprintln!("{} {}", paint(RED, "error:"), visible(&format!("{e:#}")));
}
