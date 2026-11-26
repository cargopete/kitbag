//! High-level operations: capture from several tools, plan an apply, commit it.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use anyhow::Result;

use crate::adapters::{Adapter, ApplyOptions, Env, Feature, Level, Note, Scope, Support, Work};
use crate::model::Profile;
use crate::plan_fs::{FileChange, PlanFs};
use crate::policy;
use crate::secrets::{Redaction, env_var_name, reference};
use crate::txn::{Store, Transaction, WriteOptions};

/// The merged result of capturing one or more targets.
#[derive(Debug, Default)]
pub struct CaptureResult {
    pub profile: Profile,
    pub notes: Vec<Note>,
    pub redactions: Vec<Redaction>,
    pub sources: Vec<PathBuf>,
}

fn note(level: Level, target: &str, message: String) -> Note {
    Note {
        level,
        target: target.to_string(),
        message,
    }
}

/// Capture from `targets` and merge. Earlier targets win on name conflicts.
///
/// With several targets, one whose config can't be read is reported and the
/// rest are still captured; it is an error only if none could be read. A
/// single target fails loudly.
pub fn capture(targets: &[&dyn Adapter], env: &Env, name: &str) -> Result<CaptureResult> {
    let mut out = CaptureResult {
        profile: Profile::new(name),
        ..Default::default()
    };
    let tolerant = targets.len() > 1;
    let mut failures = Vec::new();
    let mut read = 0;
    for a in targets {
        if !a.scopes().contains(&env.scope) {
            out.notes.push(note(
                Level::Info,
                a.id(),
                format!("{} has no {} scope; skipped", a.name(), env.scope),
            ));
            continue;
        }
        let c = match a.capture(env) {
            Ok(c) => c,
            Err(e) if tolerant => {
                out.notes.push(note(
                    Level::Warn,
                    a.id(),
                    format!("skipped {}: {e:#}", a.name()),
                ));
                failures.push(format!("{}: {e:#}", a.id()));
                continue;
            }
            Err(e) => return Err(e),
        };
        read += 1;
        out.notes.extend(c.notes);
        for s in c.sources {
            if !out.sources.contains(&s) {
                out.sources.push(s);
            }
        }
        let merged = &mut out.profile;
        for (n, s) in c.profile.mcp_servers {
            match merged.mcp_servers.get(&n) {
                None => {
                    out.redactions.extend(c.redactions.iter().filter(|r| r.server.as_ref() == Some(&n)).cloned());
                    merged.mcp_servers.insert(n, s);
                }
                Some(prev) if *prev != s => out.notes.push(note(
                    Level::Warn,
                    a.id(),
                    format!("MCP server '{n}' differs from the one already captured from another tool; kept the first"),
                )),
                // Alike once redacted, but holding other secrets: the profile's
                // variables stand for the first tool's values.
                Some(_) if {
                    let fps = |list: &[Redaction]| -> Vec<(String, Option<String>)> {
                        list.iter()
                            .filter(|r| r.server.as_ref() == Some(&n))
                            .map(|r| (r.location.clone(), r.fingerprint.clone()))
                            .collect()
                    };
                    fps(&c.redactions) != fps(&out.redactions)
                } => out.notes.push(note(
                    Level::Warn,
                    a.id(),
                    format!("MCP server '{n}' holds different secret values than the one already captured from another tool; kept the first, so its variables stand for the first tool's values"),
                )),
                Some(_) => {}
            }
        }
        if let Some(text) = c.profile.instructions {
            merged.instructions = Some(match merged.instructions.take() {
                None => text,
                Some(cur) if cur.contains(text.trim()) => cur,
                Some(cur) if text.contains(cur.trim()) => text,
                Some(cur) => format!("{cur}\n\n{text}"),
            });
        }
        macro_rules! merge_named {
            ($field:ident, $kind:literal) => {
                for item in c.profile.$field {
                    match merged.$field.iter().find(|x| x.name == item.name) {
                        None => merged.$field.push(item),
                        Some(prev) if *prev != item => out.notes.push(note(
                            Level::Info,
                            a.id(),
                            format!(
                                concat!(
                                    $kind,
                                    " '{}' also exists in an earlier target; kept the first"
                                ),
                                item.name
                            ),
                        )),
                        Some(_) => {}
                    }
                }
            };
        }
        merge_named!(rules, "rule");
        merge_named!(commands, "command");
        merge_named!(skills, "skill");
        for (t, s) in c.profile.settings {
            merged.settings.entry(t).or_insert(s);
        }
    }
    if read == 0 && !failures.is_empty() {
        anyhow::bail!(
            "none of the selected tools could be read:{}",
            failures
                .iter()
                .map(|f| format!("\n  - {f}"))
                .collect::<String>()
        );
    }
    dedupe_env_vars(&mut out);
    out.profile.sort();
    Ok(out)
}

/// Redaction names env vars after the key they came from (`API_KEY`), so two
/// servers with different secrets under the same key would end up sharing one
/// variable. Give later servers their own, prefixed variable instead.
fn dedupe_env_vars(out: &mut CaptureResult) {
    let redacted: BTreeSet<&str> = out.redactions.iter().map(|r| r.location.as_str()).collect();
    // Variables the configs already referenced themselves: (server, no fingerprint).
    let mut owner: BTreeMap<String, (String, Option<String>)> = BTreeMap::new();
    for (n, s) in &out.profile.mcp_servers {
        for (loc, v) in s.strings() {
            if redacted.contains(format!("mcp_servers.{n}.{loc}").as_str()) {
                continue;
            }
            for var in crate::secrets::refs_in(v) {
                owner.entry(var).or_insert_with(|| (n.clone(), None));
            }
        }
    }
    for r in &mut out.redactions {
        let Some(server) = r.server.clone() else {
            continue;
        };
        match owner.get(&r.env_var) {
            None => {
                owner.insert(r.env_var.clone(), (server, r.fingerprint.clone()));
            }
            Some((o, _)) if *o == server => {}
            // The same secret value in two servers can share one variable.
            Some((_, Some(fp))) if r.fingerprint.as_ref() == Some(fp) => {}
            Some(_) => {
                // The prefixed name may be taken too (server `a` with `B_C`
                // and server `a_b` with `C` both make `A_B_C`).
                let base = env_var_name(&[&server, &r.env_var]);
                let mut new = base.clone();
                let mut n = 1;
                while owner
                    .get(&new)
                    .is_some_and(|(o, fp)| *o != server || fp.is_none() || *fp != r.fingerprint)
                {
                    n += 1;
                    new = format!("{base}_{n}");
                }
                let field = r.location[format!("mcp_servers.{server}.").len()..].to_string();
                let (old_ref, new_ref) = (reference(&r.env_var), reference(&new));
                if let Some(s) = out.profile.mcp_servers.get_mut(&server) {
                    s.map_string_at(&field, |v| v.replace(&old_ref, &new_ref));
                }
                owner.insert(new.clone(), (server, r.fingerprint.clone()));
                r.env_var = new;
            }
        }
    }
}

/// An MCP server the plan adds or changes, summarised for review: what it runs
/// or where it connects, and which env vars and headers it sets (names only).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServerReview {
    pub name: String,
    /// Targets whose config gets this server.
    pub targets: Vec<String>,
    /// `command arg1 arg2 ...` for local servers, the URL for remote ones,
    /// as written in the profile (`${VAR}` references, not their values).
    pub endpoint: String,
    pub env: Vec<String>,
    pub headers: Vec<String>,
}

/// Everything `apply` would do, computed without touching disk.
#[derive(Debug, Default)]
pub struct Plan {
    pub changes: Vec<FileChange>,
    pub notes: Vec<Note>,
    pub targets: Vec<String>,
    /// Targets left out of the apply (no such scope, edited by hand ...); the notes say why.
    pub skipped: Vec<String>,
    /// Env vars referenced but not set (only matters where values had to be resolved).
    pub missing_env: BTreeSet<String>,
    /// Secret values that will be written to disk; masked in any displayed diff.
    pub sensitive: BTreeSet<String>,
    /// MCP servers added or changed, for review before writing.
    pub servers: Vec<ServerReview>,
    /// MCP servers from a git source that would be added or changed but were
    /// held back because the user didn't opt in (see [`plan_untrusted`]).
    pub withheld: Vec<ServerReview>,
    /// Env vars left as placeholders because secrets were not resolved.
    pub kept_env: BTreeSet<String>,
    /// Settings that widen what agents may do without asking.
    pub risky: Vec<String>,
    /// Files that will hold secret values (from `sensitive`): written with
    /// mode 0600 on Unix, existing files included.
    pub private: BTreeSet<PathBuf>,
    /// Files to write with the executable bit (skill scripts), Unix only.
    pub executable: BTreeSet<PathBuf>,
    /// The instruction blocks and rule files kitbag manages, as they are now
    /// and as the plan leaves them (for `status` and `sync`).
    pub items: Vec<Item>,
    /// (target, `command <name>` / `skill <name>`) of commands and skills the plan writes.
    pub changed_items: Vec<(String, String)>,
}

/// One managed instructions item (see [`crate::adapters::Tracked`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Item {
    pub target: String,
    /// `instructions`, `rules` or `rule <name>`.
    pub item: String,
    /// The real file.
    pub path: PathBuf,
    pub block: Option<String>,
    /// Its text on disk now (`None`: missing).
    pub current: Option<String>,
    /// Its text after this plan (`None`: the profile doesn't write it).
    pub planned: Option<String>,
}

pub fn plan(
    profile: &Profile,
    targets: &[&dyn Adapter],
    env: &Env,
    opts: &ApplyOptions,
) -> Result<Plan> {
    // `~/...` paths in the profile point into this machine's home directory.
    let expanded = crate::portable::expand_profile(profile, env);
    let profile = &expanded;
    // Project configs may come from someone else's repository: never let a
    // symlink in it redirect a write outside the project.
    let mut work = Work {
        fs: match env.scope {
            Scope::Project => PlanFs::rooted(&env.project, true),
            Scope::User => PlanFs::rooted(&env.home, false),
        },
        ..Work::default()
    };
    let mut applied = Vec::new();
    let mut skipped = Vec::new();
    for a in targets {
        if !a.scopes().contains(&env.scope) {
            work.info(
                a.id(),
                format!("{} has no {} scope; skipped", a.name(), env.scope),
            );
            skipped.push(a.id().to_string());
            continue;
        }
        let mut unsupported = Vec::new();
        let items = [
            (
                Feature::Instructions,
                usize::from(profile.instructions.is_some()),
            ),
            (Feature::Rules, profile.rules.len()),
            (Feature::Commands, profile.commands.len()),
            (Feature::Skills, profile.skills.len()),
        ];
        for (f, n) in items {
            if n > 0 && a.supports(env.scope, f) == Support::None {
                unsupported.push(match f {
                    Feature::Instructions => "instructions".to_string(),
                    _ => format!("{n} {}", f.as_str()),
                });
            }
        }
        if !unsupported.is_empty() {
            work.info(
                a.id(),
                format!(
                    "not supported by {} at {} scope, skipped: {}",
                    a.name(),
                    env.scope,
                    unsupported.join(", ")
                ),
            );
        }
        a.apply(profile, env, opts, &mut work)?;
        applied.push(a.id().to_string());
    }
    let mut servers: Vec<ServerReview> = Vec::new();
    for (target, name) in &work.changed_servers {
        if let Some(r) = servers.iter_mut().find(|r| r.name == *name) {
            if !r.targets.contains(target) {
                r.targets.push(target.clone());
            }
            continue;
        }
        let Some(s) = profile.mcp_servers.get(name) else {
            continue;
        };
        let endpoint = match &s.command {
            Some(c) if s.url.is_none() || s.transport() == crate::model::Transport::Stdio => {
                std::iter::once(c)
                    .chain(&s.args)
                    .cloned()
                    .collect::<Vec<_>>()
                    .join(" ")
            }
            _ => s.url.clone().unwrap_or_default(),
        };
        servers.push(ServerReview {
            name: name.clone(),
            targets: vec![target.clone()],
            endpoint,
            env: s.env.keys().cloned().collect(),
            headers: s.headers.keys().cloned().collect(),
        });
    }
    // A fallback is written to disk wherever its variable is unset.
    for (n, s) in &profile.mcp_servers {
        if !applied.iter().any(|t| s.applies_to(t)) {
            continue;
        }
        for (loc, v) in s.strings() {
            for (var, _) in crate::secrets::secret_fallbacks(v) {
                work.notes.push(note(
                    Level::Warn,
                    "profile",
                    format!(
                        "mcp_servers.{n}.{loc}: the ${{{var}:-…}} fallback looks like a secret; it is written to disk wherever {var} is unset. Use ${{{var}}} and set {var} instead"
                    ),
                ));
            }
        }
    }
    let mut risky: Vec<String> = applied
        .iter()
        .filter_map(|t| profile.settings.get(t).map(|s| (t, s)))
        .flat_map(|(t, s)| {
            let (allowed, _) = policy::filter(t, Some(s));
            policy::risky(t, &allowed)
        })
        .collect();
    if !applied.is_empty() {
        risky.extend(policy::risky_content(profile));
    }
    let mut items: Vec<Item> = Vec::new();
    for t in &work.tracked {
        let path = work
            .fs
            .real_path(&t.path)
            .unwrap_or_else(|_| t.path.clone());
        if items
            .iter()
            .any(|i| i.target == t.target && i.path == path && i.block == t.block)
        {
            continue;
        }
        let block = t.block.as_deref();
        let planned = crate::sync::item_text(work.fs.read_bytes(&t.path)?.as_deref(), block);
        let current = crate::sync::item_text(crate::plan_fs::read_opt(&path)?.as_deref(), block);
        if planned.is_none() && current.is_none() {
            continue;
        }
        items.push(Item {
            target: t.target.clone(),
            item: t.item.clone(),
            path,
            block: t.block.clone(),
            current,
            planned,
        });
    }
    let changes = work.fs.changes();
    let private: BTreeSet<PathBuf> = changes
        .iter()
        .filter(|c| {
            c.after.as_ref().is_some_and(|a| {
                crate::secrets::contains_any(&String::from_utf8_lossy(a), &work.render.resolved)
            })
        })
        .map(|c| c.path.clone())
        .collect();
    for path in &private {
        if let Some(mode) = crate::txn::file_mode(path).filter(|m| m & 0o077 != 0) {
            work.notes.push(note(
                Level::Warn,
                "apply",
                format!(
                    "{} will hold secret values; its mode is tightened from {mode:04o} to 0600",
                    path.display()
                ),
            ));
        }
    }
    let mut notes: Vec<Note> = Vec::new();
    for n in work.notes {
        if !notes.contains(&n) {
            notes.push(n);
        }
    }
    let executable = work.fs.executables();
    Ok(Plan {
        changes,
        private,
        executable,
        notes,
        targets: applied,
        skipped,
        missing_env: work.render.missing,
        sensitive: work.render.resolved,
        kept_env: work.render.kept,
        servers,
        withheld: Vec::new(),
        risky,
        items,
        changed_items: work.changed_items,
    })
}

/// Which MCP servers from a git source may be added or changed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GitMcp {
    /// Every server (`--allow-git-mcp` without names, or `--trust-source`).
    All,
    /// Only these (`--allow-git-mcp a,b`); none when empty.
    Only(BTreeSet<String>),
}

impl GitMcp {
    pub fn allows(&self, name: &str) -> bool {
        match self {
            GitMcp::All => true,
            GitMcp::Only(names) => names.contains(name),
        }
    }
}

/// Plan an apply of a profile from a git source: MCP servers that would be
/// added or changed are left out unless `allow` lets them in, and listed in
/// [`Plan::withheld`]. Servers already in place, and everything that isn't
/// an MCP server, apply as usual.
pub fn plan_untrusted(
    profile: &mut Profile,
    targets: &[&dyn Adapter],
    env: &Env,
    opts: &ApplyOptions,
    allow: &GitMcp,
) -> Result<Plan> {
    let first = plan(profile, targets, env, opts)?;
    let withheld: Vec<ServerReview> = first
        .servers
        .iter()
        .filter(|s| !allow.allows(&s.name))
        .cloned()
        .collect();
    if withheld.is_empty() {
        return Ok(first);
    }
    for s in &withheld {
        profile.mcp_servers.remove(&s.name);
    }
    let mut out = plan(profile, targets, env, opts)?;
    out.withheld = withheld;
    Ok(out)
}

/// Write a plan to disk as an undoable transaction. Returns `None` when there is nothing to do.
pub fn commit(plan: &Plan, description: &str, state_dir: &Path) -> Result<Option<Transaction>> {
    if plan.changes.is_empty() {
        return Ok(None);
    }
    let store = Store::new(state_dir);
    let private = plan.private.clone();
    store
        .commit(
            &plan.changes,
            description,
            &plan.targets,
            &WriteOptions {
                private,
                executable: plan.executable.clone(),
            },
        )
        .map(Some)
}
