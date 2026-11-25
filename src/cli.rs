//! Command-line interface.

use std::collections::HashMap;
use std::io::{BufRead, IsTerminal, Write};
use std::path::{Path, PathBuf};

use anstream::{eprintln, println};
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
use crate::ui::{self, BOLD, CYAN, DIM, GREEN, RED, YELLOW, paint, tildify};

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
    /// Targets to write: ids (comma-separated), 'all' or 'detected'
    #[arg(long, value_name = "IDS", default_value = "detected")]
    to: String,
    #[command(flatten)]
    place: Place,
    /// Profile directory inside a git repository
    #[arg(long, value_name = "SUBDIR")]
    path: Option<String>,
}

#[derive(Args, Debug, Clone)]
struct ApplyArgs {
    /// Keep existing same-named entries instead of replacing them
    #[arg(long)]
    no_overwrite: bool,
    /// For tools that can't read env vars: resolve ${VAR} now, or never write secret values
    #[arg(long, value_enum, default_value = "resolve")]
    secrets: SecretsMode,
}

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
    let project = project.canonicalize().unwrap_or(project);
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
# only = ["claude-code", "cursor"]      # or: except = ["codex"]

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
    for (rel, content) in &files {
        let p = crate::plan_fs::join_rel(dir, rel);
        if let Some(parent) = p.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(&p, content).with_context(|| format!("writing {}", p.display()))?;
    }
    println!(
        "{} profile '{}' in {}",
        paint(GREEN, "Created"),
        name,
        dir.display()
    );
    for (rel, _) in &files {
        println!("  {rel}");
    }
    println!(
        "\nNext: edit the files, then preview with {}",
        paint(BOLD, format!("kitbag diff {} --to all", dir.display()))
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
    let r = engine::capture(&targets, env, &name)?;
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
                p.settings.keys().cloned().collect::<Vec<_>>().join(", ")
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
                red.location,
                paint(BOLD, &red.env_var),
                paint(DIM, format!("({})", red.reason))
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
    println!("\n{} {}", paint(GREEN, "Wrote profile to"), out.display());
    let req = profile_io::required_env(p);
    if !req.is_empty() {
        println!(
            "  Env vars this profile expects: {}",
            req.keys().cloned().collect::<Vec<_>>().join(", ")
        );
    }
    println!(
        "  Review it, commit it to git, then apply it anywhere with {}",
        paint(BOLD, format!("kitbag apply {} --to <tool>", out.display()))
    );
    Ok(0)
}

enum Mode {
    Apply { yes: bool, dry_run: bool },
    Diff { exit_code: bool },
}

fn load_source(src: &str, path: Option<&str>) -> Result<(source::Resolved, Profile)> {
    let resolved = source::resolve(src, path)?;
    let profile = profile_io::load(&resolved.dir)?;
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
            paint(YELLOW, "Settings that let agents act without asking:")
        );
        for r in &plan.risky {
            println!("  - {r}");
        }
    }
    if remote && !plan.commands.is_empty() {
        println!(
            "\n{}",
            paint(
                YELLOW,
                "This profile comes from a remote repository. Your tools will run these commands:"
            )
        );
        for (name, cmd) in &plan.commands {
            println!("  {name}: {}", paint(BOLD, cmd));
        }
    }
    if !plan.missing_env.is_empty() {
        println!(
            "\n{} {}",
            paint(
                YELLOW,
                "Unset env vars (needed by tools that can't read the environment):"
            ),
            plan.missing_env
                .iter()
                .cloned()
                .collect::<Vec<_>>()
                .join(", ")
        );
    }
}

fn cmd_apply(ctx: &Ctx, src: &str, t: &TargetArgs, a: &ApplyArgs, mode: Mode) -> Result<i32> {
    let env = &ctx.env;
    let (resolved, profile) = load_source(src, t.path.as_deref())?;
    let targets = select_targets(&t.to, env, "to")?;
    let opts = ApplyOptions {
        overwrite: !a.no_overwrite,
        secrets: a.secrets,
        block_id: slugify(&profile.name),
    };
    let plan = engine::plan(&profile, &targets, env, &opts)?;
    let where_ = match env.scope {
        Scope::User => "user scope".to_string(),
        Scope::Project => format!("project {}", tildify(&env.project, &env.home)),
    };
    println!(
        "{} '{}' from {}\n  -> {} ({where_})\n",
        paint(BOLD, "Profile"),
        profile.name,
        resolved.origin,
        plan.targets.join(", ")
    );
    let show_diff = matches!(mode, Mode::Diff { .. } | Mode::Apply { dry_run: true, .. });
    if plan.changes.is_empty() {
        println!(
            "{} every target already matches the profile.",
            paint(GREEN, "Up to date:")
        );
        print_plan_extras(&plan, resolved.remote);
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
    print_plan_summary(&plan, env);
    print_plan_extras(&plan, resolved.remote);
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
            match engine::commit(&plan, &desc, &ctx.state_dir)? {
                Some(tx) => println!(
                    "\n{} {} file(s). Undo with {}",
                    paint(GREEN, "Wrote"),
                    tx.entries.len(),
                    paint(BOLD, format!("kitbag undo {}", tx.id))
                ),
                None => println!("Nothing to write."),
            }
            Ok(0)
        }
    }
}

fn cmd_undo(ctx: &Ctx, id: Option<&str>, force: bool) -> Result<i32> {
    let store = Store::new(&ctx.state_dir);
    let tx = store.undo(id, force)?;
    println!("{} {} ({})", paint(GREEN, "Undid"), tx.id, tx.description);
    for e in &tx.entries {
        let what = if e.existed { "restored" } else { "removed" };
        println!(
            "  {}  {}",
            paint(DIM, what),
            tildify(&e.path, &ctx.env.home)
        );
    }
    Ok(0)
}

fn cmd_history(ctx: &Ctx, as_json: bool) -> Result<i32> {
    let list = Store::new(&ctx.state_dir).list()?;
    if as_json {
        let v: Vec<_> = list
            .iter()
            .map(|t| {
                json!({
                    "id": t.id, "created": t.created_at(), "status": t.status,
                    "description": t.description, "targets": t.targets,
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
            paint(BOLD, &t.id),
            t.created_at(),
            status,
            t.entries.len(),
            t.description
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
    let mut problems = 0;
    println!(
        "{}",
        paint(BOLD, format!("kitbag {}", env!("CARGO_PKG_VERSION")))
    );
    println!("  home        {}", env.home.display());
    println!(
        "  scope       {}{}",
        env.scope,
        if env.scope == Scope::Project {
            format!(" ({})", env.project.display())
        } else {
            String::new()
        }
    );
    println!("  state dir   {}", ctx.state_dir.display());
    let git = std::process::Command::new("git")
        .arg("--version")
        .output()
        .ok()
        .filter(|o| o.status.success());
    println!(
        "  git         {}",
        git.map_or_else(
            || paint(YELLOW, "not found (needed only for git URL sources)"),
            |o| String::from_utf8_lossy(&o.stdout).trim().to_string()
        )
    );

    println!("\n{}", paint(BOLD, "Tools"));
    for a in adapters::all() {
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
                println!("      {} {}", paint(RED, "error"), err);
            }
            seen.push(path);
        }
    }

    if let Some(src) = src {
        println!("\n{}", paint(BOLD, "Profile"));
        match load_source(src, t.path.as_deref()) {
            Err(e) => {
                problems += 1;
                println!("  {} {e:#}", paint(RED, "error"));
            }
            Ok((resolved, p)) => {
                println!("  '{}' from {}", p.name, resolved.origin);
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
                for leak in profile_io::scan_for_secrets(&p) {
                    problems += 1;
                    println!("  {} {leak}", paint(RED, "secret"));
                }
                for (var, uses) in profile_io::required_env(&p) {
                    let set = env.vars.get(&var).is_some_and(|v| !v.is_empty());
                    println!(
                        "  {} ${{{var}}}  {}",
                        if set {
                            paint(GREEN, "set  ")
                        } else {
                            paint(YELLOW, "unset")
                        },
                        paint(DIM, uses.join(", "))
                    );
                }
                for (target, settings) in &p.settings {
                    if adapters::get(target).is_none() {
                        println!(
                            "  {} settings.{target}: unknown target",
                            paint(YELLOW, "warn")
                        );
                    }
                    let (allowed, dropped) = policy::filter(target, Some(settings));
                    if !dropped.is_empty() {
                        println!(
                            "  {} settings.{target}: will not apply {}",
                            paint(YELLOW, "warn"),
                            dropped.join(", ")
                        );
                    }
                    for r in policy::risky(target, &allowed) {
                        println!("  {} {r}", paint(YELLOW, "risky"));
                    }
                }
                for (n, s) in &p.mcp_servers {
                    for id in s.only.iter().chain(&s.except) {
                        if adapters::get(id).is_none() {
                            println!(
                                "  {} mcp_servers.{n}: unknown target '{id}' in only/except",
                                paint(YELLOW, "warn")
                            );
                        }
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
    eprintln!("{} {e:#}", paint(RED, "error:"));
}
