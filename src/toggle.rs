//! `kitbag enable` / `kitbag disable`: change the per-tool targeting
//! (`targets` / `disabled_on`) of one MCP server, rule, command, skill or
//! agent in a profile directory on this machine. Nothing is applied to a
//! tool from here; `apply` does that, as it already does for these fields.

use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use serde_json::Value;
use toml_edit::{Array, Item};

use crate::adapters;
use crate::edit::toml as tomledit;
use crate::frontmatter;
use crate::model::{Profile, TARGETING_KEYS, Targeting};
use crate::profile_io::{self, MANIFEST};
use crate::txn::write_atomic;

/// What kind of profile item a name refers to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Server,
    Rule,
    Command,
    Skill,
    Agent,
}

impl Kind {
    const ALL: [Kind; 5] = [
        Kind::Server,
        Kind::Rule,
        Kind::Command,
        Kind::Skill,
        Kind::Agent,
    ];

    /// The prefix that picks this kind (`skill:foo`).
    pub fn prefix(self) -> &'static str {
        match self {
            Kind::Server => "mcp",
            Kind::Rule => "rule",
            Kind::Command => "command",
            Kind::Skill => "skill",
            Kind::Agent => "agent",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Kind::Server => "MCP server",
            other => other.prefix(),
        }
    }

    fn from_prefix(p: &str) -> Option<Kind> {
        match p {
            "mcp" | "server" => Some(Kind::Server),
            "rule" => Some(Kind::Rule),
            "command" => Some(Kind::Command),
            "skill" => Some(Kind::Skill),
            "agent" => Some(Kind::Agent),
            _ => None,
        }
    }
}

/// What a toggle did, for the caller to print.
#[derive(Debug)]
pub struct Outcome {
    pub kind: Kind,
    pub name: String,
    /// The file that was edited (or would have been, when nothing changed).
    pub file: PathBuf,
    pub changed: bool,
    pub before: Targeting,
    pub after: Targeting,
}

/// Split `skill:foo` into a kind and a name. A name with no known prefix is
/// returned whole, so a server called `a:b` still works unprefixed.
fn parse_spec(spec: &str) -> Result<(Option<Kind>, &str)> {
    let (kind, name) = match spec.split_once(':') {
        Some(("hook" | "hooks", _)) => bail!(
            "hooks can't be enabled or disabled per tool: they have no 'targets' / 'disabled_on' in a profile. \
             Remove the hook from kitbag.toml, or leave it off with 'kitbag apply' (hooks are only written with --apply-hooks)"
        ),
        Some((p, rest)) => match Kind::from_prefix(p) {
            Some(k) => (Some(k), rest),
            None => (None, spec),
        },
        None => (None, spec),
    };
    if name.trim().is_empty() {
        bail!("no item name given; pass the name of an MCP server, rule, command, skill or agent");
    }
    Ok((kind, name))
}

fn targeting_of(p: &Profile, kind: Kind, name: &str) -> Option<Targeting> {
    match kind {
        Kind::Server => p.mcp_servers.get(name).map(|s| Targeting {
            targets: s.targets.clone(),
            disabled_on: s.disabled_on.clone(),
        }),
        Kind::Rule => p
            .rules
            .iter()
            .find(|r| r.name == name)
            .map(|r| r.targeting.clone()),
        Kind::Command => p
            .commands
            .iter()
            .find(|r| r.name == name)
            .map(|r| r.targeting.clone()),
        Kind::Skill => p
            .skills
            .iter()
            .find(|r| r.name == name)
            .map(|r| r.targeting.clone()),
        Kind::Agent => p
            .agents
            .iter()
            .find(|r| r.name == name)
            .map(|r| r.targeting.clone()),
    }
}

fn names_of(p: &Profile) -> Vec<String> {
    let mut out = Vec::new();
    for k in Kind::ALL {
        let names: Vec<&str> = match k {
            Kind::Server => p.mcp_servers.keys().map(String::as_str).collect(),
            Kind::Rule => p.rules.iter().map(|r| r.name.as_str()).collect(),
            Kind::Command => p.commands.iter().map(|r| r.name.as_str()).collect(),
            Kind::Skill => p.skills.iter().map(|r| r.name.as_str()).collect(),
            Kind::Agent => p.agents.iter().map(|r| r.name.as_str()).collect(),
        };
        out.extend(names.into_iter().map(|n| format!("{}:{n}", k.prefix())));
    }
    out
}

/// The kind and name `spec` refers to in `p`: an error when it names nothing,
/// or (without a prefix) more than one thing.
pub fn find(p: &Profile, spec: &str) -> Result<(Kind, String)> {
    let (kind, name) = parse_spec(spec)?;
    let kinds: Vec<Kind> = match kind {
        Some(k) => vec![k],
        None => Kind::ALL.to_vec(),
    };
    let found: Vec<Kind> = kinds
        .into_iter()
        .filter(|k| targeting_of(p, *k, name).is_some())
        .collect();
    match found.as_slice() {
        [k] => Ok((*k, name.to_string())),
        [] => {
            let have = names_of(p);
            let list = if have.is_empty() {
                "it has no MCP servers, rules, commands, skills or agents".to_string()
            } else {
                format!("it has: {}", have.join(", "))
            };
            match kind {
                Some(k) => bail!("the profile has no {} '{name}'; {list}", k.label()),
                None => bail!(
                    "the profile has no MCP server, rule, command, skill or agent '{name}'; {list}"
                ),
            }
        }
        many => bail!(
            "'{name}' is more than one thing in this profile: {}. Say which, e.g. '{}:{name}'",
            many.iter()
                .map(|k| format!("{}:{name}", k.prefix()))
                .collect::<Vec<_>>()
                .join(", "),
            many[0].prefix()
        ),
    }
}

/// Canonical ids for `--on` values, or an error naming a tool kitbag doesn't know.
pub fn check_targets(on: &[String]) -> Result<Vec<String>> {
    let mut out: Vec<String> = Vec::new();
    for id in on {
        let id = id.trim();
        let a = adapters::get(id).with_context(|| {
            format!(
                "unknown target '{id}'; known targets: {}",
                adapters::ids().join(", ")
            )
        })?;
        if !out.iter().any(|x| x == a.id()) {
            out.push(a.id().to_string());
        }
    }
    Ok(out)
}

/// `t` after enabling or disabling on `on` (canonical ids; empty: every tool).
/// An allow-list that would end up empty, or a deny-list of every tool, is
/// spelled the one way that loads: a `disabled_on` naming every target.
pub fn change(t: &Targeting, enable: bool, on: &[String], all: &[&str]) -> Targeting {
    let mut t = t.clone();
    if on.is_empty() {
        return if enable {
            Targeting::default()
        } else {
            Targeting {
                targets: None,
                disabled_on: all.iter().map(|s| (*s).to_string()).collect(),
            }
        };
    }
    for id in on {
        match (&mut t.targets, enable) {
            (Some(list), true) => {
                if !list.contains(id) {
                    list.push(id.clone());
                }
            }
            (Some(list), false) => list.retain(|x| x != id),
            (None, true) => t.disabled_on.retain(|x| x != id),
            (None, false) => {
                if !t.disabled_on.contains(id) {
                    t.disabled_on.push(id.clone());
                }
            }
        }
    }
    if let Some(list) = &t.targets {
        if list.is_empty() {
            t.targets = None;
            t.disabled_on = all.iter().map(|s| (*s).to_string()).collect();
        } else if all.iter().all(|a| list.iter().any(|x| x == a)) {
            t.targets = None;
        }
    }
    t
}

/// A short sentence for the state `t` leaves an item in.
pub fn describe(t: &Targeting, all: &[&str]) -> String {
    if let Some(list) = &t.targets {
        return format!("only on {}", list.join(", "));
    }
    if t.disabled_on.is_empty() {
        return "on every tool".to_string();
    }
    if all.iter().all(|a| t.disabled_on.iter().any(|x| x == a)) {
        return "off on every tool".to_string();
    }
    format!("off on {}", t.disabled_on.join(", "))
}

/// The edited text of `kitbag.toml`, with the server's targeting replaced.
fn server_manifest(text: &str, path: &Path, name: &str, t: &Targeting) -> Result<String> {
    let mut doc = tomledit::parse(Some(text), path)?;
    let entry = doc
        .get_mut("mcp_servers")
        .and_then(Item::as_table_like_mut)
        .and_then(|s| s.get_mut(name))
        .and_then(Item::as_table_like_mut)
        .with_context(|| {
            format!(
                "{}: can't find the table of server '{name}'",
                path.display()
            )
        })?;
    for key in TARGETING_KEYS {
        entry.remove(key);
    }
    let array = |ids: &[String]| {
        let mut a = Array::new();
        for id in ids {
            a.push(id.as_str());
        }
        Item::Value(a.into())
    };
    if let Some(list) = &t.targets {
        entry.insert("targets", array(list));
    }
    if !t.disabled_on.is_empty() {
        entry.insert("disabled_on", array(&t.disabled_on));
    }
    Ok(doc.to_string())
}

/// Frontmatter keys that hold targeting in `text` and are ours to rewrite:
/// `targets` / `disabled_on`, and `only` / `except` when they hold names (as
/// the loader reads them).
fn keys_to_drop(text: &str) -> Vec<&'static str> {
    let data = frontmatter::parse(text).data;
    TARGETING_KEYS
        .into_iter()
        .filter(|k| {
            data.get(*k).is_some_and(|v| match *k {
                "only" | "except" => {
                    v.is_string() || v.as_array().is_some_and(|a| a.iter().all(Value::is_string))
                }
                _ => true,
            })
        })
        .collect()
}

fn markdown_with(text: &str, t: &Targeting) -> String {
    let base =
        frontmatter::remove_keys(text, &keys_to_drop(text)).unwrap_or_else(|| text.to_string());
    frontmatter::add_entries(&base, &t.entries())
}

/// Where the item's markdown lives in the profile.
fn markdown_path(dir: &Path, kind: Kind, name: &str) -> Result<PathBuf> {
    let path = match kind {
        Kind::Rule => {
            let md = dir.join("rules").join(format!("{name}.md"));
            if md.exists() {
                md
            } else {
                dir.join("rules").join(format!("{name}.mdc"))
            }
        }
        Kind::Command => dir.join("commands").join(format!("{name}.md")),
        Kind::Agent => dir.join("agents").join(format!("{name}.md")),
        Kind::Skill => dir.join("skills").join(name).join("SKILL.md"),
        Kind::Server => bail!("an MCP server is in {MANIFEST}"),
    };
    Ok(path)
}

fn same_targeting(a: &Targeting, b: &Targeting) -> bool {
    a == b
}

/// Reads `kind`'s targeting back out of `text` as the loader would, to check
/// the edit says what was meant before it is written.
fn markdown_targeting(text: &str) -> Targeting {
    let mut data = frontmatter::parse(text).data;
    let (t, _, _) = Targeting::take_from(
        &mut data,
        &adapters::ids(),
        |id| adapters::canonical_id(id).to_string(),
        "item",
    );
    t
}

fn file_mode(path: &Path) -> Option<u32> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::metadata(path)
            .ok()
            .map(|m| m.permissions().mode() & 0o7777)
    }
    #[cfg(not(unix))]
    {
        let _ = path;
        None
    }
}

/// Enable or disable `spec` on the targets `on` (none: every one) in the
/// profile directory `dir`. The profile must load, the item must exist, and
/// the edit is checked before one file is replaced atomically.
pub fn toggle(dir: &Path, spec: &str, enable: bool, on: &[String]) -> Result<Outcome> {
    let on = check_targets(on)?;
    let p = profile_io::load(dir)?;
    let (kind, name) = find(&p, spec)?;
    let all = adapters::ids();
    let before = targeting_of(&p, kind, &name).context("item vanished")?;
    let after = change(&before, enable, &on, &all);

    let (file, text, new_text) = if kind == Kind::Server {
        let file = dir.join(MANIFEST);
        let text =
            fs::read_to_string(&file).with_context(|| format!("reading {}", file.display()))?;
        let new_text = server_manifest(&text, &file, &name, &after)?;
        let reread = profile_io::parse_manifest(&new_text, &file)
            .map_err(|e| anyhow::anyhow!("the edited {MANIFEST} would not load: {e}"))?;
        let got = reread.mcp_servers.get(&name).map(|s| Targeting {
            targets: s.targets.clone(),
            disabled_on: s.disabled_on.clone(),
        });
        if !got.is_some_and(|g| same_targeting(&g, &after)) {
            bail!(
                "couldn't edit '{name}' in {MANIFEST} safely; edit its 'targets' / 'disabled_on' by hand"
            );
        }
        (file, text, new_text)
    } else {
        let file = markdown_path(dir, kind, &name)?;
        let text =
            fs::read_to_string(&file).with_context(|| format!("reading {}", file.display()))?;
        let new_text = markdown_with(&text, &after);
        if !same_targeting(&markdown_targeting(&new_text), &after) {
            bail!(
                "couldn't edit {} safely; edit its 'targets' / 'disabled_on' by hand",
                file.display()
            );
        }
        (file, text, new_text)
    };

    let changed = before != after;
    if changed && new_text != text {
        write_atomic(&file, new_text.as_bytes(), file_mode(&file))?;
    }
    Ok(Outcome {
        kind,
        name,
        file,
        changed,
        before,
        after,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const ALL: [&str; 3] = ["claude-code", "codex", "cursor"];

    fn t(targets: Option<&[&str]>, off: &[&str]) -> Targeting {
        Targeting {
            targets: targets.map(|l| l.iter().map(|s| s.to_string()).collect()),
            disabled_on: off.iter().map(|s| s.to_string()).collect(),
        }
    }

    fn on(ids: &[&str]) -> Vec<String> {
        ids.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn disable_and_enable_one_tool() {
        let d = change(&t(None, &[]), false, &on(&["codex"]), &ALL);
        assert_eq!(d, t(None, &["codex"]));
        // Twice changes nothing.
        assert_eq!(change(&d, false, &on(&["codex"]), &ALL), d);
        assert_eq!(change(&d, true, &on(&["codex"]), &ALL), t(None, &[]));
        // Enabling what is already on changes nothing.
        assert_eq!(
            change(&t(None, &[]), true, &on(&["codex"]), &ALL),
            t(None, &[])
        );
    }

    #[test]
    fn allow_lists_grow_and_shrink() {
        let a = t(Some(&["codex", "cursor"]), &[]);
        assert_eq!(
            change(&a, false, &on(&["codex"]), &ALL),
            t(Some(&["cursor"]), &[])
        );
        assert_eq!(
            change(&a, true, &on(&["claude-code"]), &ALL),
            t(None, &[]),
            "an allow-list naming every tool is no restriction"
        );
        // The last one out leaves a deny-list of every tool, not an empty allow-list.
        let last = t(Some(&["codex"]), &[]);
        assert_eq!(change(&last, false, &on(&["codex"]), &ALL), t(None, &ALL));
    }

    #[test]
    fn everywhere() {
        assert_eq!(
            change(&t(Some(&["codex"]), &[]), false, &[], &ALL),
            t(None, &ALL)
        );
        assert_eq!(
            change(&t(Some(&["codex"]), &[]), true, &[], &ALL),
            t(None, &[])
        );
        assert_eq!(change(&t(None, &["codex"]), true, &[], &ALL), t(None, &[]));
    }

    #[test]
    fn describes_states() {
        assert_eq!(describe(&t(None, &[]), &ALL), "on every tool");
        assert_eq!(describe(&t(None, &["codex"]), &ALL), "off on codex");
        assert_eq!(describe(&t(None, &ALL), &ALL), "off on every tool");
        assert_eq!(describe(&t(Some(&["codex"]), &[]), &ALL), "only on codex");
    }

    #[test]
    fn specs() {
        assert_eq!(parse_spec("skill:x").unwrap(), (Some(Kind::Skill), "x"));
        assert_eq!(parse_spec("mcp:x").unwrap(), (Some(Kind::Server), "x"));
        assert_eq!(parse_spec("a:b").unwrap(), (None, "a:b"));
        assert!(parse_spec("hook:x").is_err());
        assert!(parse_spec("skill:").is_err());
    }

    #[test]
    fn markdown_edit_keeps_everything_else() {
        let text = "---\ndescription: D\nonly:\n  - claude\nmodel: x\n---\n\nBody\n";
        let new = markdown_with(text, &t(None, &["codex"]));
        assert_eq!(
            new,
            "---\ndescription: D\nmodel: x\ndisabled_on:\n  - codex\n---\n\nBody\n"
        );
        let cleared = markdown_with(&new, &t(None, &[]));
        assert_eq!(cleared, "---\ndescription: D\nmodel: x\n---\n\nBody\n");
        // Another tool's non-list `only` stays.
        let other = "---\nonly: 3\n---\nB\n";
        assert_eq!(
            markdown_with(other, &t(None, &["codex"])),
            "---\nonly: 3\ndisabled_on:\n  - codex\n---\nB\n"
        );
    }

    #[test]
    fn server_edit_keeps_comments() {
        let text = "kitbag = 1\nname = \"p\"\n\n# docs\n[mcp_servers.docs]\ncommand = \"docs-mcp\" # run it\nonly = [\"codex\"]\n";
        let new = server_manifest(
            text,
            Path::new("kitbag.toml"),
            "docs",
            &t(None, &["cursor"]),
        )
        .unwrap();
        assert!(new.contains("# docs\n"), "{new}");
        assert!(new.contains("command = \"docs-mcp\" # run it"), "{new}");
        assert!(new.contains("disabled_on = [\"cursor\"]"), "{new}");
        assert!(!new.contains("only"), "{new}");
    }
}
