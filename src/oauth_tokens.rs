//! MCP OAuth token stores carried in the encrypted secrets bundle (#18, the
//! second phase).
//!
//! Only a store whose path a tool's *official* docs name is carried. As of
//! 2026-11-27 that is Gemini CLI alone: its MCP docs say tokens are "stored
//! securely in `~/.gemini/mcp-oauth-tokens.json`", refreshed when expired and
//! validated before each connection. The docs don't describe the file's
//! layout, so kitbag keeps it as opaque bytes: it never parses, merges,
//! validates or refreshes it. Codex, Claude Code, opencode, Amp, Cursor,
//! Hermes, Zed, Copilot and Claude Desktop name no token file in their docs
//! (or say not to edit it), so nothing of theirs is read; their servers stay
//! on the sign-in checklist.
//!
//! The rules on apply:
//! - a store is written only back to its own tool, and only when that tool is
//!   a target;
//! - when a server URL recorded with the store now differs, in the profile
//!   or in the tool's config, nothing is written and the user signs in again;
//! - an existing non-empty token file is never overwritten (there is no
//!   documented way to merge one);
//! - the file is written outside the undo transaction, so no token reaches
//!   the state directory, and `kitbag undo` doesn't remove it.
//!
//! No token value is printed: messages name the tool and the path only.

use std::collections::BTreeMap;
use std::io::Read;
use std::path::{Path, PathBuf};

use anyhow::{Result, bail};

use crate::adapters::{Env, Level, Note, Scope};
use crate::bundle::{Secret, TokenStore};
use crate::model::Profile;
use crate::oauth::SignIn;

/// Gemini CLI's tool id.
pub const GEMINI: &str = "gemini-cli";
/// The kind of Gemini CLI's documented token file.
pub const GEMINI_KIND: &str = "gemini-mcp-oauth-tokens";
/// The documented location, as shown to the user.
pub const GEMINI_SHOWN: &str = "~/.gemini/mcp-oauth-tokens.json";

/// A token file bigger than this is not carried.
const MAX_STORE_BYTES: u64 = 256 * 1024;

/// Where Gemini CLI keeps its MCP OAuth tokens, under `home`.
pub fn gemini_path(home: &Path) -> PathBuf {
    home.join(".gemini").join("mcp-oauth-tokens.json")
}

/// How a store is named to the user: tool and documented path, no content.
pub fn describe(store: &TokenStore) -> String {
    if store.tool == GEMINI && store.kind == GEMINI_KIND {
        format!("{GEMINI} MCP OAuth tokens ({GEMINI_SHOWN})")
    } else {
        format!("{} token store ({})", store.tool, store.kind)
    }
}

fn note(level: Level, message: String) -> Note {
    Note {
        level,
        target: "secrets".to_string(),
        message,
    }
}

/// Read up to the cap, without following a link. `Ok(None)` when there is
/// nothing to carry, with a note saying why when that is worth saying.
fn read_store(path: &Path, notes: &mut Vec<Note>) -> Result<Option<String>> {
    let meta = match std::fs::symlink_metadata(path) {
        Ok(m) => m,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => {
            notes.push(note(
                Level::Warn,
                format!("{GEMINI_SHOWN} could not be read ({e}); it is not in the bundle"),
            ));
            return Ok(None);
        }
    };
    if !meta.is_file() {
        notes.push(note(
            Level::Warn,
            format!("{GEMINI_SHOWN} is not a regular file (a link?); it is not read or carried"),
        ));
        return Ok(None);
    }
    if meta.len() == 0 {
        return Ok(None);
    }
    if meta.len() > MAX_STORE_BYTES {
        notes.push(note(
            Level::Warn,
            format!(
                "{GEMINI_SHOWN} is larger than {} KiB; it is not carried, sign in again on the other machine",
                MAX_STORE_BYTES / 1024
            ),
        ));
        return Ok(None);
    }
    // Sized up front, so the buffer never moves and leaves a copy behind.
    let mut buf = Vec::with_capacity(MAX_STORE_BYTES as usize + 1);
    let file = std::fs::File::open(path);
    let read = file.and_then(|f| f.take(MAX_STORE_BYTES + 1).read_to_end(&mut buf));
    let bytes = zeroize::Zeroizing::new(buf);
    if let Err(e) = read {
        notes.push(note(
            Level::Warn,
            format!("{GEMINI_SHOWN} could not be read ({e}); it is not in the bundle"),
        ));
        return Ok(None);
    }
    if bytes.len() as u64 > MAX_STORE_BYTES {
        notes.push(note(
            Level::Warn,
            format!("{GEMINI_SHOWN} grew while it was read; it is not carried"),
        ));
        return Ok(None);
    }
    match std::str::from_utf8(&bytes) {
        Ok(text) => Ok(Some(text.to_string())),
        Err(_) => {
            notes.push(note(
                Level::Warn,
                format!("{GEMINI_SHOWN} is not UTF-8 text; it is not carried"),
            ));
            Ok(None)
        }
    }
}

/// The remote servers `profile` has for `tool`, name -> URL.
fn servers_for(profile: &Profile, tool: &str) -> BTreeMap<String, String> {
    profile
        .mcp_servers
        .iter()
        .filter(|(_, s)| s.applies_to(tool))
        .filter_map(|(n, s)| s.url.clone().map(|u| (n.clone(), u)))
        .collect()
}

/// `capture --include secrets`: the token stores of the tools captured from
/// (`ids`), read from `env.home`. `profile` is the captured profile, whose
/// server URLs are recorded with each store.
pub fn capture(env: &Env, ids: &[&str], profile: &Profile) -> Result<(Vec<TokenStore>, Vec<Note>)> {
    let mut notes = Vec::new();
    let mut out = Vec::new();
    if ids.contains(&GEMINI)
        && let Some(text) = read_store(&gemini_path(&env.home), &mut notes)?
    {
        out.push(TokenStore {
            tool: GEMINI.to_string(),
            kind: GEMINI_KIND.to_string(),
            servers: servers_for(profile, GEMINI),
            content: Secret::from_string(text),
        });
    }
    Ok((out, notes))
}

/// A token store `apply` will write after the plan is confirmed.
#[derive(Debug)]
pub struct Pending {
    pub path: PathBuf,
    pub store: TokenStore,
}

impl Pending {
    pub fn describe(&self) -> String {
        describe(&self.store)
    }
}

/// What `apply --secrets-from` does with a bundle's token stores.
#[derive(Debug, Default)]
pub struct Planned {
    pub pending: Vec<Pending>,
    pub notes: Vec<Note>,
    pub signins: Vec<SignIn>,
}

/// The URL of server `name` in Gemini CLI's `settings.json` at `path`.
fn configured_url(path: &Path, name: &str) -> Option<String> {
    let text = std::fs::read_to_string(path).ok()?;
    let doc = crate::edit::json::parse_value(Some(&text), path).ok()?;
    let def = doc.get("mcpServers")?.get(name)?;
    def.get("httpUrl")
        .or_else(|| def.get("url"))
        .and_then(|v| v.as_str())
        .map(str::to_string)
}

/// The recorded servers whose URL now differs, in the profile or in Gemini
/// CLI's own config (user settings, and the project's at project scope).
fn moved_servers(store: &TokenStore, profile: &Profile, env: &Env) -> Vec<String> {
    let mut configs = vec![env.home.join(".gemini").join("settings.json")];
    if env.scope == Scope::Project {
        configs.push(env.project.join(".gemini").join("settings.json"));
    }
    store
        .servers
        .iter()
        .filter(|(name, url)| {
            let in_profile = profile
                .mcp_servers
                .get(*name)
                .is_some_and(|s| s.url.as_deref() != Some(url.as_str()));
            in_profile
                || configs
                    .iter()
                    .any(|c| configured_url(c, name).is_some_and(|u| &u != *url))
        })
        .map(|(n, _)| n.clone())
        .collect()
}

/// Is there nothing at `path` that writing would destroy: missing, or an
/// empty regular file? A link or a directory is never written over.
fn free_to_write(path: &Path) -> std::result::Result<(), String> {
    match std::fs::symlink_metadata(path) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(format!("it could not be checked ({e})")),
        Ok(m) if m.is_file() && m.len() == 0 => Ok(()),
        Ok(m) if m.is_file() => Err(
            "it already exists and is not empty; its layout isn't documented, so it is not merged or overwritten"
                .to_string(),
        ),
        Ok(_) => Err("something other than a regular file is there".to_string()),
    }
}

fn signins_for(store: &TokenStore, names: &[String]) -> Vec<SignIn> {
    names
        .iter()
        .filter_map(|n| {
            crate::oauth::signin_command(&store.tool, n).map(|command| SignIn {
                target: store.tool.clone(),
                server: n.clone(),
                command,
            })
        })
        .collect()
}

/// Decide, for each token store of a bundle, whether `apply` writes it.
/// `targets` are the ids applied to. Nothing is written here.
pub fn plan(stores: Vec<TokenStore>, profile: &Profile, targets: &[&str], env: &Env) -> Planned {
    let mut out = Planned::default();
    for store in stores {
        let what = describe(&store);
        if store.tool != GEMINI || store.kind != GEMINI_KIND {
            out.notes.push(note(
                Level::Info,
                format!("the bundle holds a {what} that this kitbag doesn't write; skipped"),
            ));
            continue;
        }
        if !targets.contains(&GEMINI) {
            out.notes.push(note(
                Level::Info,
                format!(
                    "the bundle's {what} is only ever written to {GEMINI}, which isn't a target; nothing is copied to another tool (sign in there)"
                ),
            ));
            continue;
        }
        let all: Vec<String> = store.servers.keys().cloned().collect();
        let moved = moved_servers(&store, profile, env);
        if !moved.is_empty() {
            out.notes.push(note(
                Level::Warn,
                format!(
                    "not writing the bundle's {what}: the URL of MCP server(s) {} changed since capture, so its tokens may be for another server. Sign in again",
                    moved.join(", ")
                ),
            ));
            out.signins.extend(signins_for(&store, &all));
            continue;
        }
        let path = gemini_path(&env.home);
        if let Err(why) = free_to_write(&path) {
            out.notes.push(note(
                Level::Info,
                format!("not writing the bundle's {what}: {why}. Sign in to any server it lacks"),
            ));
            out.signins.extend(signins_for(&store, &all));
            continue;
        }
        out.notes.push(note(
            Level::Info,
            format!(
                "the bundle's {what} is written from memory after the other files (private to you; not in the undo record, so 'kitbag undo' leaves it: sign out in {GEMINI}, or delete the file)"
            ),
        ));
        out.pending.push(Pending { path, store });
    }
    out
}

/// Write a planned store, checking again that nothing is in the way.
pub fn write(p: &Pending) -> Result<()> {
    if let Err(why) = free_to_write(&p.path) {
        bail!("not writing {}: {why}", p.describe());
    }
    if let Some(dir) = p.path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    crate::txn::write_atomic(&p.path, p.store.content.expose().as_bytes(), Some(0o600))
}

#[cfg(test)]
mod tests {
    use super::*;

    const FAKE: &str = "ya29.FAKE-gemini-access-unit";

    fn store() -> TokenStore {
        TokenStore {
            tool: GEMINI.into(),
            kind: GEMINI_KIND.into(),
            servers: BTreeMap::new(),
            content: Secret::new(FAKE),
        }
    }

    #[test]
    fn nothing_here_prints_a_token() {
        let s = store();
        assert!(!format!("{s:?}").contains(FAKE));
        assert!(!describe(&s).contains(FAKE));
        let p = Pending {
            path: PathBuf::from("x"),
            store: s,
        };
        assert!(!format!("{p:?}").contains(FAKE));
    }

    #[test]
    fn only_a_missing_or_empty_file_is_free() {
        let dir = tempfile::tempdir().unwrap();
        let f = dir.path().join("t.json");
        assert!(free_to_write(&f).is_ok());
        std::fs::write(&f, "").unwrap();
        assert!(free_to_write(&f).is_ok());
        std::fs::write(&f, "{}").unwrap();
        assert!(free_to_write(&f).is_err());
        assert!(free_to_write(dir.path()).is_err());
    }
}
