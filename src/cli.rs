//! Command-line interface.

use std::collections::HashMap;
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
use crate::model::{Profile, slugify};
use crate::plan_fs::read_text_opt;
use crate::policy;
use crate::profile_io::{self, MANIFEST};
use crate::source;
use crate::txn::{Status, Store};
use crate::ui::{self, BOLD, CYAN, DIM, GREEN, RED, YELLOW, paint, tildify, visible, visible_line};

const ABOUT: &str = "Pack your AI agent setup once, unpack it into any coding tool.";

const AFTER_HELP: &str = "\
Sources (for apply / diff / doctor) are a profile directory or a git URL:
  ./my-profile   github:you/dotagents   gh:you/dotfiles/agents#main
  https://github.com/you/dotagents.git   git@github.com:you/dotagents.git

Examples:
  kitbag capture --from claude-code,cursor -o ~/dotagents
  kitbag diff ~/dotagents --to codex
  kitbag apply github:you/dotagents --to all
  kitbag apply ~/dotagents --project . --to cursor,codex
  kitbag apply ~/dotagents --to gemini-cli,antigravity
  kitbag status ~/dotagents
  kitbag sync ~/dotagents --from claude-code
  kitbag undo";

#[derive(Parser, Debug)]
#[command(name = "kitbag", version, about = ABOUT, after_help = AFTER_HELP, propagate_version = true)]
pub struct Cli {
    /// Treat DIR as the home directory, and ignore CODEX_HOME / CLAUDE_CONFIG_DIR /
    /// XDG_CONFIG_HOME / APPDATA (for sandboxes and testing)
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
        /// Targets to read: ids (comma-separated), 'all' or 'detected'
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
        /// Show what would be captured without writing anything
        #[arg(long)]
        dry_run: bool,
    },
    /// Apply a profile: shows the plan, then asks before writing anything
    Apply {
        /// Profile directory or git URL
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
        /// Profile directory or git URL
        source: String,
        #[command(flatten)]
        targets: TargetArgs,
        #[command(flatten)]
        apply: ApplyArgs,
        /// Exit with status 1 when there are differences (for CI drift checks)
        #[arg(long)]
        exit_code: bool,
    },
    /// Show where the instructions and rules in each tool differ from the
    /// profile, or (without PROFILE) changed since the last apply. Never writes
    Status {
        /// Profile directory or git URL to compare with
        source: Option<String>,
        #[command(flatten)]
        args: StatusArgs,
    },
    /// Apply a profile without overwriting instructions edited in the tool
    /// since the last apply (see 'kitbag status')
    Sync {
        /// Profile directory or git URL
        source: String,
        #[command(flatten)]
        targets: TargetArgs,
        #[command(flatten)]
        sync: SyncArgs,
        #[command(flatten)]
        apply: ApplyArgs,
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
        /// Profile directory or git URL to check
        source: Option<String>,
        #[command(flatten)]
        targets: TargetArgs,
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
    /// Targets to write: ids (comma-separated: claude-code, cursor, codex, gemini-cli,
    /// claude-desktop, antigravity), 'all' or 'detected'
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
}

#[derive(Args, Debug, Clone)]
struct StatusArgs {
    /// Targets to compare: ids (comma-separated), 'all' or 'detected'
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
    /// Exit with status 1 when anything drifted
    #[arg(long)]
    exit_code: bool,
    /// Machine-readable output
    #[arg(long)]
    json: bool,
}

#[derive(Args, Debug, Clone)]
struct SyncArgs {
    /// Pull this target's edits to the managed instructions and rules back into
    /// the profile (redacted, like capture), then apply them to the others
    #[arg(long, value_name = "ID")]
    from: Option<String>,
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
        if self.allow_risky_settings || self.allow_git_mcp.iter().any(|n| n == "*") {
            engine::GitMcp::All
        } else {
            engine::GitMcp::Only(
                self.allow_git_mcp
                    .iter()
                    .map(|n| n.trim().to_string())
                    .filter(|n| !n.is_empty())
                    .collect(),
            )
        }
    }
}

/// Does this argument look like a profile source (path or git URL) rather
/// than a list of MCP server names? A bare word only counts when it is a
/// directory holding a `kitbag.toml`, so `--allow-git-mcp docs` keeps meaning
/// the server `docs` even when the current directory has a `docs/` folder.
fn looks_like_source(arg: &str) -> bool {
    arg.contains(['/', '\\', ':'])
        || arg.starts_with(['.', '~'])
        || arg.ends_with(".git")
        || std::path::Path::new(arg)
            .join(crate::profile_io::MANIFEST)
            .is_file()
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
            dry_run,
        } => cmd_capture(
            &context(home, Some(&place))?,
            &from,
            &out,
            name,
            force,
            dry_run,
        ),
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
        Cmd::Undo { id, force } => cmd_undo(&context(home, None)?, id.as_deref(), force),
        Cmd::History { json } => cmd_history(&context(home, None)?, json),
        Cmd::Doctor { source, targets } => cmd_doctor(
            &context(home, Some(&targets.place))?,
            source.as_deref(),
            &targets,
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
    dry_run: bool,
) -> Result<i32> {
    let env = &ctx.env;
    let targets = select_targets(from, env, "from")?;
    let name = name.unwrap_or_else(|| {
        let base = std::path::absolute(out)
            .ok()
            .and_then(|d| d.file_name().map(|f| f.to_string_lossy().into_owned()));
        slugify(&base.unwrap_or_else(|| "my-setup".into()))
    });
    let mut r = engine::capture(&targets, env, &name)?;
    // Re-capturing into a profile keeps the per-tool targeting set there.
    let kept = if out.join(profile_io::MANIFEST).is_file() {
        profile_io::load(out)
            .map(|old| profile_io::keep_targeting(&mut r.profile, &old))
            .unwrap_or_default()
    } else {
        Vec::new()
    };
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
    let p = &r.profile;
    println!("{} ({} scope)", paint(BOLD, "Captured"), env.scope);
    for s in &r.sources {
        println!("  {} {}", paint(DIM, "read"), tildify(s, &env.home));
    }
    println!(
        "\n  {} MCP server(s), {} rule(s), {} command(s), {} skill(s){}{}",
        p.mcp_servers.len(),
        p.rules.len(),
        p.commands.len(),
        p.skills.len(),
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
    allow_insecure: bool,
) -> Result<(source::Resolved, Profile)> {
    let resolved = source::resolve(src, path, allow_insecure)?;
    let profile = profile_io::load(&resolved.dir)?;
    for w in &profile.warnings {
        eprintln!("{} {}", paint(YELLOW, "warning:"), visible_line(w));
    }
    Ok((resolved, profile))
}

fn print_plan_summary(plan: &Plan, env: &Env) {
    for c in &plan.changes {
        let stats = ui::line_stats(c)
            .map(|(a, d)| {
                format!(
                    "{} {}",
                    paint(GREEN, format!("+{a}")),
                    paint(RED, format!("-{d}"))
                )
            })
            .unwrap_or_else(|| paint(DIM, "binary"));
        println!(
            "  {}  {}  {}",
            ui::change_label(c),
            tildify(&c.path, &env.home),
            stats
        );
    }
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
    let (resolved, profile) = load_source(src, t.path.as_deref(), t.allow_insecure_source)?;
    let targets = select_targets(&t.to, &ctx.env, "to")?;
    let (profile, plan) = plan_apply(ctx, &resolved, profile, &targets, a)?;
    present(ctx, &resolved, &profile, &plan, mode)
}

/// Plan an apply of `profile`, with the restrictions for git sources: risky
/// settings and content dropped, secrets not resolved, MCP servers opt-in.
/// Returns the profile as applied (after those restrictions) and the plan.
fn plan_apply(
    ctx: &Ctx,
    resolved: &source::Resolved,
    mut profile: Profile,
    targets: &[&'static dyn Adapter],
    a: &ApplyArgs,
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
    let allow = a.git_mcp();
    let opts = ApplyOptions {
        overwrite: !a.no_overwrite,
        secrets,
        block_id: slugify(&profile.name),
        applied_servers,
        source: Some(resolved.source.clone()),
        git_removals: resolved.remote.then(|| allow.clone()),
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
    if let Some(w) = records_warning {
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

fn print_skipped(plan: &Plan) {
    if !plan.skipped.is_empty() {
        println!(
            "{} {} (see the notes below for why)",
            paint(YELLOW, "Skipped:"),
            plan.skipped.join(", ")
        );
    }
}

fn where_label(env: &Env) -> String {
    match env.scope {
        Scope::User => "user scope".to_string(),
        Scope::Project => format!("project {}", tildify(&env.project, &env.home)),
    }
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
        "{} '{}' from {}\n  -> {} ({where_})\n",
        paint(BOLD, "Profile"),
        visible_line(&profile.name),
        visible_line(&resolved.origin),
        visible_line(&plan.targets.join(", "))
    );
    let show_diff = matches!(mode, Mode::Diff { .. } | Mode::Apply { dry_run: true, .. });
    let record = |tx: Option<&str>| -> Result<()> {
        if matches!(mode, Mode::Apply { dry_run: false, .. }) {
            crate::sync::record(&ctx.state_dir, plan, env, &profile.name, tx)?;
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
            } else if !plan.skipped.is_empty() {
                println!(
                    "{} {} already matches the profile.",
                    paint(GREEN, "Up to date:"),
                    plan.targets.join(", ")
                );
                print_skipped(plan);
            } else {
                println!(
                    "{} every target already matches the profile.",
                    paint(GREEN, "Up to date:")
                );
            }
        } else {
            println!("Nothing to write without the MCP servers listed below.");
        }
        print_plan_extras(plan, resolved.remote);
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
            match engine::commit(plan, &desc, &ctx.state_dir)? {
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

/// MCP servers, commands and skills of `profile` (as loaded, before any
/// remote restrictions) compared with each planned target: names only.
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
                if !present
                    .as_ref()
                    .is_some_and(|p| p.mcp_servers.contains_key(n))
                {
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
        ] {
            if a.supports(env.scope, feature) == adapters::Support::None {
                continue;
            }
            let mut names = wanted.clone();
            if let Some(h) = &have {
                names.extend(h.iter().filter(|n| !wanted.contains(n)).cloned());
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
        rows.extend(disabled_rows);
    }
    rows
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
            let (resolved, profile) = load_source(src, s.path.as_deref(), s.allow_insecure_source)?;
            let targets = select_targets(s.to.as_deref().unwrap_or("detected"), env, "to")?;
            let loaded = profile.clone();
            let (profile, plan) =
                plan_apply(ctx, &resolved, profile, &targets, &ApplyArgs::default())?;
            rows.extend(instruction_rows(ctx, &plan)?.into_iter().map(|(r, _)| r));
            rows.extend(inventory_rows(env, &loaded, &plan, &targets));
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
            if !s.json {
                println!(
                    "{} '{}' from {} ({})\n",
                    paint(BOLD, "Profile"),
                    visible_line(&profile.name),
                    visible_line(&resolved.origin),
                    where_label(env)
                );
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
            for r in crate::sync::last_applied(&ctx.state_dir)?.into_values() {
                if !r.matches(env)
                    || only
                        .as_ref()
                        .is_some_and(|o| !o.contains(&r.target.as_str()))
                {
                    continue;
                }
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
                    "{} since the last apply ({})\n",
                    paint(BOLD, "Changes"),
                    where_label(env)
                );
            }
        }
    }
    let drift = rows.iter().any(|r| r.state.is_drift());
    print_status(&rows, env, s.json)?;
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
                "MCP servers, commands, skills: added = in the profile, not in the tool; changed = differs from the profile; removed = in the tool, not in the profile (sync leaves it)."
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
    let (resolved, loaded) = load_source(src, t.path.as_deref(), t.allow_insecure_source)?;
    if resolved.remote && s.from.is_some() {
        bail!(
            "--from can't be used with a git source: kitbag can't write edits back into a repository it cloned. Clone it, sync the local directory with --from, then commit and push"
        );
    }
    let mut targets = select_targets(&t.to, env, "to")?;
    let (mut profile, mut plan) = plan_apply(ctx, &resolved, loaded.clone(), &targets, a)?;
    let mut profile_changes = Vec::new();
    let mut pulled_notes = Vec::new();
    if let Some(from) = &s.from {
        let Some(source_target) = targets.iter().find(|x| x.id() == from.as_str()) else {
            bail!("--from {from} must be one of the targets being synced (see --to)");
        };
        let rows = instruction_rows(ctx, &plan)?;
        let edited: Vec<&engine::Item> = rows
            .iter()
            .filter(|(r, _)| r.target == source_target.id() && r.state.is_conflict())
            .map(|(_, i)| i)
            .collect();
        if edited.is_empty() {
            pulled_notes.push(adapters::Note {
                level: adapters::Level::Info,
                target: from.clone(),
                message: "no edits to pull back: its managed instructions match the last apply"
                    .into(),
            });
        } else {
            let pb =
                crate::sync::pull_back(&loaded, &resolved.dir, &edited, &slugify(&loaded.name))?;
            pulled_notes.extend(pb.notes);
            profile_changes = pb.changes;
            (profile, plan) = plan_apply(ctx, &resolved, pb.profile, &targets, a)?;
        }
    }
    let mut skipped: Vec<String> = Vec::new();
    if !s.overwrite {
        for (r, _) in instruction_rows(ctx, &plan)? {
            if r.state.is_conflict() && !skipped.contains(&r.target) {
                skipped.push(r.target);
            }
        }
        if !skipped.is_empty() {
            targets.retain(|x| !skipped.iter().any(|id| id == x.id()));
            plan = if targets.is_empty() {
                Plan::default()
            } else {
                plan_apply(ctx, &resolved, profile.clone(), &targets, a)?.1
            };
        }
    }
    plan.notes.extend(pulled_notes);
    plan.skipped.extend(skipped.iter().cloned());
    for id in &skipped {
        plan.notes.push(adapters::Note {
            level: adapters::Level::Warn,
            target: id.clone(),
            message: format!(
                "{id} was not synced: its managed instructions were edited in the tool since the last apply. Pull the edit into the profile with --from {id}, or replace it with --overwrite (backed up; 'kitbag undo' restores it)"
            ),
        });
    }
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
    Ok(if code == 0 && !skipped.is_empty() {
        1
    } else {
        code
    })
}

fn cmd_undo(ctx: &Ctx, id: Option<&str>, force: bool) -> Result<i32> {
    let store = Store::new(&ctx.state_dir);
    let done = store.undo_with_home(id, force, Some(&ctx.env.home))?;
    let tx = &done.tx;
    println!(
        "{} {} ({})",
        paint(GREEN, "Undid"),
        visible_line(&tx.id),
        visible_line(&source::strip_url_credentials(&tx.description))
    );
    for e in &tx.entries {
        if done.refused.iter().any(|r| r.path == e.path) {
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

fn cmd_doctor(ctx: &Ctx, src: Option<&str>, t: &TargetArgs) -> Result<i32> {
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

    if let Some(src) = src {
        println!("\n{}", paint(BOLD, "Profile"));
        // Not load_source: doctor lists the profile's warnings itself.
        let loaded = source::resolve(src, t.path.as_deref(), t.allow_insecure_source)
            .and_then(|r| profile_io::load(&r.dir).map(|p| (r, p)));
        match loaded {
            Err(e) => {
                problems += 1;
                println!("  {} {}", paint(RED, "error"), visible(&format!("{e:#}")));
            }
            Ok((resolved, p)) => {
                println!(
                    "  '{}' from {}",
                    visible_line(&p.name),
                    visible_line(&resolved.origin)
                );
                println!(
                    "  {} MCP server(s), {} rule(s), {} command(s), {} skill(s){}",
                    p.mcp_servers.len(),
                    p.rules.len(),
                    p.commands.len(),
                    p.skills.len(),
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
            }
        }
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
            .map(|h| format!("{h:<13}"))
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
            .map(|f| format!("{:<13}", a.supports(env.scope, *f).as_str()))
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
