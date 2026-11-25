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
    /// For tools that can't read env vars: resolve ${VAR} now, or never write secret
    /// values [default: resolve; never for git sources]
    #[arg(long, value_enum)]
    secrets: Option<SecretsMode>,
    /// Trust a git source like a local profile: also apply settings that let
    /// agents act without asking (bypassPermissions, permissions.allow rules,
    /// approval_policy = never ...). Local profiles always apply them, with a warning
    #[arg(long, visible_alias = "trust-source")]
    allow_risky_settings: bool,
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
    let starter = [
        MANIFEST,
        "instructions.md",
        "rules/example.md",
        "commands/review.md",
    ];
    if !force
        && let Some(f) = starter
            .iter()
            .find(|f| crate::plan_fs::join_rel(dir, f).exists())
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
            paint(YELLOW, "Lets agents act without asking, or runs commands:")
        );
        for r in &plan.risky {
            println!("  - {}", visible_line(r));
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
        // Shown as in the profile; anything that still looks like a secret is masked,
        // and control characters can't rewrite the terminal.
        let shown = |s: &str| {
            let masked = crate::secrets::mask(&crate::secrets::mask_line(s), &plan.sensitive);
            visible_line(&masked).into_owned()
        };
        for s in &plan.servers {
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
    let env = &ctx.env;
    let (resolved, mut profile) = load_source(src, t.path.as_deref())?;
    let targets = select_targets(&t.to, env, "to")?;
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
    let opts = ApplyOptions {
        overwrite: !a.no_overwrite,
        secrets,
        block_id: slugify(&profile.name),
    };
    let mut plan = engine::plan(&profile, &targets, env, &opts)?;
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
    let where_ = match env.scope {
        Scope::User => "user scope".to_string(),
        Scope::Project => format!("project {}", tildify(&env.project, &env.home)),
    };
    println!(
        "{} '{}' from {}\n  -> {} ({where_})\n",
        paint(BOLD, "Profile"),
        visible_line(&profile.name),
        visible_line(&resolved.origin),
        visible_line(&plan.targets.join(", "))
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
    println!(
        "{} {} ({})",
        paint(GREEN, "Undid"),
        visible_line(&tx.id),
        visible_line(&tx.description)
    );
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
            paint(BOLD, visible_line(&t.id)),
            t.created_at(),
            status,
            t.entries.len(),
            visible_line(&t.description)
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
        match load_source(src, t.path.as_deref()) {
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
                for (n, s) in &p.mcp_servers {
                    for id in s.only.iter().chain(&s.except) {
                        if adapters::get(id).is_none() {
                            println!(
                                "  {} mcp_servers.{}: unknown target '{}' in only/except",
                                paint(YELLOW, "warn"),
                                visible_line(n),
                                visible_line(id)
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
    eprintln!("{} {}", paint(RED, "error:"), visible(&format!("{e:#}")));
}
