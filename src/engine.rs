//! High-level operations: capture from several tools, plan an apply, commit it.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use anyhow::Result;

use crate::adapters::{
    Adapter, ApplyOptions, Env, Feature, FileWrite, HookWrite, Level, Note, Scope, ServerWrite,
    Support, Work,
};
use crate::model::{Profile, SkippedItem};
use crate::plan_fs::{FileChange, PlanFs};
use crate::policy;
use crate::secrets::{Redaction, env_var_name, reference};
use crate::txn::{GitCommit, Store, Transaction, WriteOptions};

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
    // Per server, the tools that have it on and those that keep it off.
    let mut off_state: BTreeMap<String, (Vec<String>, Vec<String>)> = BTreeMap::new();
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
        for (n, mut s) in c.profile.mcp_servers {
            // Whether each tool has the server switched off is kept apart, so
            // tools that disagree don't make the servers "differ".
            let st = off_state.entry(n.clone()).or_default();
            if std::mem::take(&mut s.disabled) {
                st.1.push(a.id().to_string());
            } else {
                st.0.push(a.id().to_string());
            }
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
        merge_named!(agents, "agent");
        merge_named!(output_styles, "output style");
        for h in c.profile.hooks {
            if !merged.hooks.contains(&h) {
                merged.hooks.push(h);
            }
        }
        for f in c.profile.hook_files {
            match merged.hook_files.iter().find(|x| x.path == f.path) {
                None => merged.hook_files.push(f),
                // Two tools' scripts with one name: the hooks that run the
                // second would run the first's.
                Some(x) if x.content != f.content => out.notes.push(note(
                    Level::Warn,
                    a.id(),
                    format!(
                        "hook script hooks/{} differs from the one already captured from another tool; kept the first, so hooks that run it use that one",
                        f.path
                    ),
                )),
                Some(_) => {}
            }
        }
        for (t, s) in c.profile.settings {
            merged.settings.entry(t).or_insert(s);
        }
        for (t, s) in c.profile.plugins {
            merged.plugins.entry(t).or_insert(s);
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
    // Off in every tool that has it: disabled. Off in some only: off for just
    // those tools, which is what `disabled_on` says.
    for (n, (on, off)) in off_state {
        let Some(s) = out.profile.mcp_servers.get_mut(&n) else {
            continue;
        };
        if off.is_empty() {
            continue;
        }
        if on.is_empty() {
            s.disabled = true;
        } else {
            s.disabled_on = off;
        }
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
    /// Managed items left as they are in the tool because of
    /// [`ApplyOptions::hold`] (`sync` holds back hand edits). They are not in
    /// `items`, so the apply doesn't record them and they still show as edited.
    pub held_back: Vec<Item>,
    /// (target, `command <name>` / `skill <name>` / `agent <name>`) of
    /// commands, skills and agents the plan writes.
    pub changed_items: Vec<(String, String)>,
    /// MCP servers the plan removes from a tool, because the profile now
    /// disables them there (`targets` / `disabled_on`).
    pub removed_servers: Vec<ServerReview>,
    /// MCP servers the plan keeps in a tool but turns off there, with the
    /// tool's own flag or list, because the profile now disables them there.
    pub turned_off_servers: Vec<ServerReview>,
    /// MCP servers a profile from a git source would remove from a tool, held
    /// back like additions until `--allow-git-mcp` names them.
    pub withheld_removals: Vec<ServerReview>,
    /// MCP server entries the plan writes or removes, with the real config
    /// file (recorded after a write, see [`crate::sync::record`]).
    pub server_writes: Vec<ServerWrite>,
    /// Hook entries the plan writes, with the real settings and script files
    /// (recorded after a write, see [`crate::hooks::record`]).
    pub hook_writes: Vec<HookWrite>,
    /// (target, hook as [`crate::hooks::describe`] spells it) of the profile's
    /// hooks that a target refused to write; `status` reports them.
    pub hook_skipped: Vec<(String, String)>,
    /// Rule, command, skill and agent files the plan writes or removes, with
    /// the real path (recorded after a write, see [`crate::sync::record`]).
    pub file_writes: Vec<FileWrite>,
    /// (target, `rule <name>` ...) of items the plan removes from a tool,
    /// because the profile now disables them there (`targets` / `disabled_on`).
    pub removed_items: Vec<(String, String)>,
    /// Skill directories the plan empties, taken out once the write is done.
    pub prune_dirs: Vec<PathBuf>,
    /// Where the profile comes from ([`ApplyOptions::source`]).
    pub source: Option<String>,
    /// At project scope, the project directory (as given, and canonical):
    /// every write the plan makes in it must stay inside it. Checked again
    /// when the plan is committed, since a symbolic link in the project can
    /// change while the plan waits for confirmation.
    pub confine_to: Option<(PathBuf, PathBuf)>,
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
    let original = profile;
    // Project configs may come from someone else's repository: never let a
    // symlink in it redirect a write outside the project.
    let mut work = Work {
        fs: plan_fs(env),
        profile: profile.name.clone(),
        home: env.home.clone(),
        targets: targets
            .iter()
            .filter(|a| a.scopes().contains(&env.scope))
            .map(|a| a.id().to_string())
            .collect(),
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
        // Rules, commands, skills and agents the profile keeps off this tool
        // are planned as if they weren't in it, and removed if kitbag put them there.
        let skipped = profile.skipped_items(a.id());
        let for_target = profile.for_target(a.id());
        let profile = for_target.as_ref().unwrap_or(profile);
        for s in &skipped {
            if a.supports(env.scope, item_feature(s)) != Support::None {
                work.info(
                    a.id(),
                    format!(
                        "{} '{}' skipped on {}: disabled in profile ({})",
                        s.kind,
                        s.name,
                        a.id(),
                        s.why
                    ),
                );
            }
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
            (Feature::Agents, profile.agents.len()),
            (Feature::OutputStyles, profile.output_styles.len()),
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
            let message = format!(
                "not supported by {} at {} scope, skipped: {}",
                a.name(),
                env.scope,
                unsupported.join(", ")
            );
            work.info(a.id(), message);
        }
        if !profile.hooks.is_empty() && crate::adapters::hook_files::format_for(a.id()).is_none() {
            // Never drop hooks without saying so: this target has nowhere to put them.
            let why = crate::adapters::hook_files::no_hooks_reason(a.id())
                .unwrap_or("kitbag doesn't know where it keeps hooks");
            work.warn(
                a.id(),
                format!(
                    "{} hook(s) not written to {}: {why}. They would run:",
                    profile.hooks.len(),
                    a.name()
                ),
            );
            for h in &profile.hooks {
                work.info(a.id(), format!("hook {}", crate::hooks::describe(h)));
            }
        }
        a.apply(profile, env, opts, &mut work)?;
        remove_disabled_items(&mut work, *a, env, opts, original, &skipped)?;
        applied.push(a.id().to_string());
    }
    let file_writes: Vec<FileWrite> = work
        .file_writes
        .iter()
        .map(|w| FileWrite {
            path: work
                .fs
                .real_path(&w.path)
                .unwrap_or_else(|_| w.path.clone()),
            ..w.clone()
        })
        .collect();
    let servers = review_servers(profile, &work.changed_servers);
    let removed_servers = review_servers(profile, &work.removed_servers);
    let turned_off_servers = review_servers(profile, &work.turned_off_servers);
    let withheld_removals = review_servers(profile, &work.withheld_removals);
    let server_writes: Vec<ServerWrite> = work
        .server_writes
        .iter()
        .map(|w| ServerWrite {
            path: work
                .fs
                .real_path(&w.path)
                .unwrap_or_else(|_| w.path.clone()),
            ..w.clone()
        })
        .collect();
    let real = |p: &Path| work.fs.real_path(p).unwrap_or_else(|_| p.to_path_buf());
    let hook_writes: Vec<HookWrite> = work
        .hook_writes
        .iter()
        .map(|w| HookWrite {
            path: real(&w.path),
            scripts: w
                .scripts
                .iter()
                .map(|(p, h)| (real(p), h.clone()))
                .collect(),
            ..w.clone()
        })
        .collect();
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
    let mut held_back: Vec<Item> = Vec::new();
    for t in &work.tracked {
        let path = work
            .fs
            .real_path(&t.path)
            .unwrap_or_else(|_| t.path.clone());
        if items
            .iter()
            .chain(&held_back)
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
        let held = opts.hold.contains(&(path.clone(), t.block.clone()));
        let item = Item {
            target: t.target.clone(),
            item: t.item.clone(),
            path,
            block: t.block.clone(),
            current,
            planned,
        };
        if held {
            held_back.push(item);
        } else {
            items.push(item);
        }
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
        held_back,
        changed_items: work.changed_items,
        removed_servers,
        turned_off_servers,
        withheld_removals,
        server_writes,
        hook_writes,
        hook_skipped: work.hook_skipped,
        file_writes,
        removed_items: work.removed_items,
        prune_dirs: work.prune_dirs,
        source: opts.source.clone(),
        confine_to: (env.scope == Scope::Project).then(|| {
            let root = env
                .project
                .canonicalize()
                .unwrap_or_else(|_| env.project.clone());
            (env.project.clone(), root)
        }),
    })
}

fn plan_fs(env: &Env) -> PlanFs {
    match env.scope {
        Scope::Project => PlanFs::rooted(&env.project, true),
        Scope::User => PlanFs::rooted(&env.home, false),
    }
}

fn item_feature(s: &SkippedItem) -> Feature {
    match s.kind {
        "rule" => Feature::Rules,
        "command" => Feature::Commands,
        "skill" => Feature::Skills,
        "output-style" => Feature::OutputStyles,
        _ => Feature::Agents,
    }
}

/// Remove from `a`'s tool the rules, commands, skills and agents the profile
/// disables there, but only those an earlier apply of this same profile (and
/// source) wrote and that are unchanged since. Anything else (added by hand,
/// by another profile, or edited) stays, and the plan says why.
fn remove_disabled_items(
    work: &mut Work,
    a: &dyn Adapter,
    env: &Env,
    opts: &ApplyOptions,
    profile: &Profile,
    skipped: &[SkippedItem],
) -> Result<()> {
    let id = a.id();
    for s in skipped {
        if a.supports(env.scope, item_feature(s)) == Support::None {
            continue;
        }
        let label = s.label();
        let what = format!("{} '{}'", s.kind, s.name);
        let disabled = format!("{what} is disabled for {id} in the profile");
        let records: Vec<&crate::sync::FileRecord> = opts
            .applied_files
            .iter()
            .filter(|r| r.target == id && r.item == label && r.hash.is_some())
            .collect();
        if records.is_empty() {
            if let Some(path) = find_unrecorded(work, a, env, opts, profile, s) {
                let file = crate::ui::tildify(&path, &work.home);
                work.warn(
                    id,
                    format!(
                        "{disabled}, but kitbag has no record of adding it to {file}, so it was left in place; remove it by hand if you don't want it"
                    ),
                );
            }
            continue;
        }
        let remote = opts.git_removals.is_some();
        for r in records {
            // The files of the item that are there now, as (path under r.path, bytes).
            let mut present: Vec<(String, Vec<u8>)> = Vec::new();
            if r.files.is_empty() {
                if let Some(b) = work.fs.read_bytes(&r.path)? {
                    present.push((String::new(), b));
                }
            } else {
                for rel in r.files.keys() {
                    if let Some(b) = work
                        .fs
                        .read_bytes(&crate::plan_fs::join_rel(&r.path, rel))?
                    {
                        present.push((rel.clone(), b));
                    }
                }
            }
            if present.is_empty() {
                continue;
            }
            let file = crate::ui::tildify(&r.path, &work.home);
            let unchanged = if r.files.is_empty() {
                let text = String::from_utf8_lossy(&present[0].1).into_owned();
                r.hash.as_deref() == Some(crate::sync::hash(&text).as_str())
            } else {
                present
                    .iter()
                    .all(|(rel, b)| r.files.get(rel) == Some(&crate::sync::content_hash(b)))
            };
            let keep = if r.profile != work.profile {
                Some(format!(
                    "{disabled}, but profile '{}' added it to {file}, so it was left in place",
                    r.profile
                ))
            } else if r.source.is_none() && remote {
                Some(format!(
                    "{disabled}, but kitbag's record of adding it to {file} doesn't say where the profile came from, and this one is from a git source, so it was left in place; remove it by hand if you don't want it"
                ))
            } else if r.source.is_some() && r.source != opts.source {
                Some(format!(
                    "{disabled}, but a profile '{}' from {} added it to {file}, so it was left in place",
                    r.profile,
                    r.source.as_deref().unwrap_or_default()
                ))
            } else if !unchanged {
                Some(format!(
                    "{disabled}, but it was edited in {file} since kitbag added it, so it was left in place; remove it by hand if you don't want it"
                ))
            } else if !opts.overwrite {
                Some(format!(
                    "kept {what} in {file}, disabled for {id} in the profile, because --no-overwrite was given"
                ))
            } else {
                None
            };
            if let Some(msg) = keep {
                work.warn(id, msg);
                continue;
            }
            for (rel, _) in &present {
                let path = if rel.is_empty() {
                    r.path.clone()
                } else {
                    crate::plan_fs::join_rel(&r.path, rel)
                };
                work.fs.remove(&path)?;
                if !rel.is_empty() {
                    // Directories the skill leaves empty, deepest first.
                    let mut dir = path.parent();
                    while let Some(d) = dir.filter(|d| d.starts_with(&r.path)) {
                        if !work.prune_dirs.contains(&d.to_path_buf()) {
                            work.prune_dirs.push(d.to_path_buf());
                        }
                        dir = d.parent();
                    }
                }
            }
            work.info(
                id,
                format!(
                    "{what} removed from {file}: disabled for {id} in the profile (kitbag added it there before)"
                ),
            );
            work.file_writes.push(FileWrite {
                target: id.to_string(),
                path: r.path.clone(),
                item: label.clone(),
                hash: None,
                files: Default::default(),
            });
            work.removed_items.push((id.to_string(), label.clone()));
        }
    }
    work.prune_dirs
        .sort_by_key(|d| std::cmp::Reverse(d.components().count()));
    Ok(())
}

/// Where `a`'s tool already has a file for the disabled item `s` that kitbag
/// has no record of writing, if the profile's own copy would differ from it
/// (found by planning that one item into a scratch copy of the file system).
fn find_unrecorded(
    work: &Work,
    a: &dyn Adapter,
    env: &Env,
    opts: &ApplyOptions,
    profile: &Profile,
    s: &SkippedItem,
) -> Option<PathBuf> {
    let mut one = Profile::new(profile.name.clone());
    match s.kind {
        "rule" => one
            .rules
            .extend(profile.rules.iter().find(|r| r.name == s.name).cloned()),
        "command" => one
            .commands
            .extend(profile.commands.iter().find(|c| c.name == s.name).cloned()),
        "skill" => one
            .skills
            .extend(profile.skills.iter().find(|k| k.name == s.name).cloned()),
        "output-style" => one.output_styles.extend(
            profile
                .output_styles
                .iter()
                .find(|o| o.name == s.name)
                .cloned(),
        ),
        _ => one
            .agents
            .extend(profile.agents.iter().find(|g| g.name == s.name).cloned()),
    }
    let mut scratch = Work {
        fs: plan_fs(env),
        profile: profile.name.clone(),
        home: env.home.clone(),
        targets: work.targets.clone(),
        ..Work::default()
    };
    a.apply(&one, env, opts, &mut scratch).ok()?;
    let label = s.label();
    if s.kind == "rule"
        && let Some(t) = scratch
            .tracked
            .iter()
            .find(|t| t.block.is_none() && t.item == label)
    {
        let real = scratch.fs.real_path(&t.path).ok()?;
        return real.is_file().then_some(real);
    }
    scratch.fs.changes().into_iter().find_map(|c| {
        if c.before.is_none() || c.after.is_none() {
            return None;
        }
        if s.kind == "skill" {
            c.path
                .ancestors()
                .find(|d| d.file_name().is_some_and(|n| n == s.name.as_str()))
                .map(Path::to_path_buf)
        } else {
            let stem = format!("{}.", s.name);
            c.path
                .file_name()
                .is_some_and(|n| n.to_string_lossy().starts_with(&stem))
                .then_some(c.path)
        }
    })
}

/// The profile servers named in `entries` (target, name), with the targets of each.
fn review_servers(profile: &Profile, entries: &[(String, String)]) -> Vec<ServerReview> {
    let mut servers: Vec<ServerReview> = Vec::new();
    for (target, name) in entries {
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
    servers
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
    commit_from(plan, description, state_dir, None)
}

/// [`commit`], recording the git source and commit the plan came from.
pub fn commit_from(
    plan: &Plan,
    description: &str,
    state_dir: &Path,
    git: Option<GitCommit>,
) -> Result<Option<Transaction>> {
    if plan.changes.is_empty() {
        return Ok(None);
    }
    let store = Store::new(state_dir);
    let private = plan.private.clone();
    let tx = store.commit_from(
        &plan.changes,
        description,
        &plan.targets,
        &WriteOptions {
            private,
            executable: plan.executable.clone(),
            confine_to: plan.confine_to.clone(),
        },
        git,
    )?;
    // Directories a removed skill left empty (undo puts its files back).
    for d in &plan.prune_dirs {
        let _ = std::fs::remove_dir(d);
    }
    Ok(Some(tx))
}
