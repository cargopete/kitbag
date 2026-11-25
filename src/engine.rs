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
                    let prefix = format!("mcp_servers.{n}.");
                    out.redactions.extend(c.redactions.iter().filter(|r| r.location.starts_with(&prefix)).cloned());
                    merged.mcp_servers.insert(n, s);
                }
                Some(prev) if *prev != s => out.notes.push(note(
                    Level::Warn,
                    a.id(),
                    format!("MCP server '{n}' differs from the one already captured from another tool; kept the first"),
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
    let server_of = |loc: &str, names: &[String]| -> Option<String> {
        let rest = loc.strip_prefix("mcp_servers.")?;
        names
            .iter()
            .filter(|n| rest.starts_with(&format!("{n}.")))
            .max_by_key(|n| n.len())
            .cloned()
    };
    let names: Vec<String> = out.profile.mcp_servers.keys().cloned().collect();
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
        let Some(server) = server_of(&r.location, &names) else {
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
                let new = env_var_name(&[&server, &r.env_var]);
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

/// Everything `apply` would do, computed without touching disk.
#[derive(Debug, Default)]
pub struct Plan {
    pub changes: Vec<FileChange>,
    pub notes: Vec<Note>,
    pub targets: Vec<String>,
    /// Env vars referenced but not set (only matters where values had to be resolved).
    pub missing_env: BTreeSet<String>,
    /// Secret values that will be written to disk; masked in any displayed diff.
    pub sensitive: BTreeSet<String>,
    /// stdio commands the configured tools will run.
    pub commands: Vec<(String, String)>,
    /// Settings that widen what agents may do without asking.
    pub risky: Vec<String>,
}

pub fn plan(
    profile: &Profile,
    targets: &[&dyn Adapter],
    env: &Env,
    opts: &ApplyOptions,
) -> Result<Plan> {
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
    for a in targets {
        if !a.scopes().contains(&env.scope) {
            work.info(
                a.id(),
                format!("{} has no {} scope; skipped", a.name(), env.scope),
            );
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
    let commands = profile
        .mcp_servers
        .iter()
        .filter(|(_, s)| !s.disabled && targets.iter().any(|a| s.applies_to(a.id())))
        .filter_map(|(n, s)| {
            s.command.as_ref().map(|c| {
                let mut line = c.clone();
                for a in &s.args {
                    line.push(' ');
                    line.push_str(a);
                }
                (n.clone(), line)
            })
        })
        .collect();
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
    let mut notes: Vec<Note> = Vec::new();
    for n in work.notes {
        if !notes.contains(&n) {
            notes.push(n);
        }
    }
    Ok(Plan {
        changes: work.fs.changes(),
        notes,
        targets: applied,
        missing_env: work.render.missing,
        sensitive: work.render.resolved,
        commands,
        risky,
    })
}

/// Write a plan to disk as an undoable transaction. Returns `None` when there is nothing to do.
pub fn commit(plan: &Plan, description: &str, state_dir: &Path) -> Result<Option<Transaction>> {
    if plan.changes.is_empty() {
        return Ok(None);
    }
    let store = Store::new(state_dir);
    let private = plan
        .changes
        .iter()
        .filter(|c| {
            c.after.as_ref().is_some_and(|a| {
                crate::secrets::contains_any(&String::from_utf8_lossy(a), &plan.sensitive)
            })
        })
        .map(|c| c.path.clone())
        .collect();
    store
        .commit(
            &plan.changes,
            description,
            &plan.targets,
            &WriteOptions { private },
        )
        .map(Some)
}
