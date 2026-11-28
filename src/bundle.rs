//! The opt-in encrypted secrets bundle (`secrets.age`).
//!
//! A bare capture redacts secrets to `${VAR}` and never writes their values.
//! `capture --include secrets` additionally writes the values it just
//! redacted, as a map of `VAR` -> value, to one file next to `kitbag.toml`,
//! encrypted with [age](https://age-encryption.org) (a passphrase, which uses
//! age's scrypt recipient, or public keys: `age1...` and SSH keys). Nothing
//! else in this crate writes a secret value anywhere.
//!
//! Values are held in memory only while a bundle is being sealed or opened:
//! [`Secret`] zeroes its bytes when dropped and prints as `<redacted>` in
//! every format, so a stray `{:?}` can't leak one. The plaintext of the
//! bundle is built in a zeroizing buffer and handed straight to age; there is
//! no plaintext file, not even a temporary one.
//!
//! Besides the `${VAR}` values, a bundle can hold MCP OAuth token stores
//! ([`TokenStore`]): a tool's documented token file, kept as opaque bytes and
//! tagged with the tool it belongs to (see [`crate::oauth_tokens`]). A bundle
//! with token stores is format 2; one without is written as format 1, as
//! before, and both are read.

use std::collections::BTreeMap;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::str::FromStr;

use age::secrecy::SecretString;
use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize, Serializer};
use zeroize::Zeroizing;

/// The bundle's file name, next to `kitbag.toml`.
pub const FILE: &str = "secrets.age";

/// A bundle bigger than this is not read: it is not one kitbag wrote.
const MAX_BUNDLE_BYTES: u64 = 1024 * 1024;
/// The most a passphrase or key file may hold.
const MAX_KEY_FILE_BYTES: u64 = 64 * 1024;
/// scrypt work factors above this (log2 of N, 2^20 is about 1 GiB of memory)
/// are refused when opening, so a hostile file can't make kitbag use more.
const MAX_WORK_FACTOR: u8 = 20;

/// One secret value. Zeroed when dropped; never printed.
#[derive(Clone, PartialEq, Eq)]
pub struct Secret(Zeroizing<String>);

impl Secret {
    pub fn new(value: &str) -> Self {
        Secret(Zeroizing::new(value.to_string()))
    }

    /// Take `value` as it is, without copying it.
    pub fn from_string(value: String) -> Self {
        Secret(Zeroizing::new(value))
    }

    /// The value, for sealing it. Keep the result out of messages and files.
    pub fn expose(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Debug for Secret {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Secret(<redacted>)")
    }
}

impl Serialize for Secret {
    fn serialize<S: Serializer>(&self, s: S) -> std::result::Result<S::Ok, S::Error> {
        s.serialize_str(&self.0)
    }
}

impl<'de> Deserialize<'de> for Secret {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> std::result::Result<Self, D::Error> {
        Ok(Secret(Zeroizing::new(String::deserialize(d)?)))
    }
}

/// One variable of a bundle.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Entry {
    pub value: Secret,
    /// Where capture found it (`mcp_servers.github.env.GITHUB_TOKEN`); a
    /// location in a tool's config, not part of the secret.
    #[serde(default)]
    pub at: String,
}

/// One tool's MCP OAuth token store: the file's bytes, opaque (kitbag never
/// parses them), with what kitbag knew from the tool's config at capture.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TokenStore {
    /// The tool it belongs to (`gemini-cli`); it is only ever written back to
    /// that tool.
    pub tool: String,
    /// Which documented file it is ([`crate::oauth_tokens::GEMINI_KIND`]).
    pub kind: String,
    /// The tool's remote MCP servers at capture, name -> URL (from the
    /// tool's config, not from the token file).
    #[serde(default)]
    pub servers: BTreeMap<String, String>,
    /// The file's content. Never printed.
    pub content: Secret,
}

/// The values of `${VAR}` names, and any token stores, in memory.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Bundle {
    entries: BTreeMap<String, Entry>,
    tokens: Vec<TokenStore>,
}

#[derive(Serialize, Deserialize)]
struct Wire {
    kitbag_secrets: u32,
    secrets: BTreeMap<String, Entry>,
    #[serde(default)]
    token_stores: Vec<TokenStore>,
}

/// The bundle formats this kitbag reads.
const FORMATS: [u32; 2] = [1, 2];

/// Is `s` a tool id or store kind a bundle may hold (`gemini-cli`)?
fn valid_label(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 64
        && s.chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
}

/// Is `name` an environment variable name a bundle may hold?
pub fn valid_name(name: &str) -> bool {
    let mut chars = name.chars();
    chars
        .next()
        .is_some_and(|c| c == '_' || c.is_ascii_alphabetic())
        && chars.all(|c| c == '_' || c.is_ascii_alphanumeric())
        && name.len() <= 256
}

impl Bundle {
    /// No variable and no token store.
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty() && self.tokens.is_empty()
    }

    /// The number of variables (token stores not counted).
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Add a tool's token store. A second store for the same tool and kind
    /// replaces the first.
    pub fn add_token_store(&mut self, store: TokenStore) {
        self.tokens
            .retain(|t| !(t.tool == store.tool && t.kind == store.kind));
        self.tokens.push(store);
    }

    /// The token stores.
    pub fn token_stores(&self) -> &[TokenStore] {
        &self.tokens
    }

    /// Take the token stores out of the bundle.
    pub fn take_token_stores(&mut self) -> Vec<TokenStore> {
        std::mem::take(&mut self.tokens)
    }

    /// Add `name`. The same name with another value is an error: two secrets
    /// must never share a variable.
    pub fn insert(&mut self, name: &str, value: Secret, at: &str) -> Result<()> {
        if !valid_name(name) {
            bail!("'{name}' is not a usable variable name");
        }
        match self.entries.get(name) {
            Some(prev) if prev.value != value => {
                bail!("two different secrets would be stored as ${{{name}}}")
            }
            Some(_) => Ok(()),
            None => {
                self.entries.insert(
                    name.to_string(),
                    Entry {
                        value,
                        at: at.to_string(),
                    },
                );
                Ok(())
            }
        }
    }

    /// Add `name`, or give it a new value if it is there already (keeping
    /// where it was captured). Returns whether a value was replaced.
    pub fn set(&mut self, name: &str, value: Secret, at: &str) -> Result<bool> {
        if !valid_name(name) {
            bail!("'{name}' is not a usable variable name");
        }
        if let Some(e) = self.entries.get_mut(name) {
            e.value = value;
            return Ok(true);
        }
        self.entries.insert(
            name.to_string(),
            Entry {
                value,
                at: at.to_string(),
            },
        );
        Ok(false)
    }

    /// The value of `name`.
    pub fn get(&self, name: &str) -> Option<&Secret> {
        self.entries.get(name).map(|e| &e.value)
    }

    /// The variable names and where each was captured: no values.
    pub fn names(&self) -> Vec<(String, String)> {
        self.entries
            .iter()
            .map(|(k, e)| (k.clone(), e.at.clone()))
            .collect()
    }
}

/// How a bundle is encrypted: to one passphrase, or to public keys. age does
/// not mix the two.
pub enum Seal {
    Passphrase(SecretString),
    Keys(Vec<Box<dyn age::Recipient + Send>>),
}

impl std::fmt::Debug for Seal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Seal::Passphrase(_) => f.write_str("Seal::Passphrase(<redacted>)"),
            Seal::Keys(k) => write!(f, "Seal::Keys({} recipient(s))", k.len()),
        }
    }
}

/// How a bundle is opened.
pub struct Unlock {
    pub passphrase: Option<SecretString>,
    pub identities: Vec<Box<dyn age::Identity + Send + Sync>>,
}

impl std::fmt::Debug for Unlock {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "Unlock {{ passphrase: {}, identities: {} }}",
            if self.passphrase.is_some() {
                "<redacted>"
            } else {
                "none"
            },
            self.identities.len()
        )
    }
}

/// An age recipient given as `--secrets-recipient`: an `age1...` key or an SSH
/// public key line, or a file holding them one per line (`#` comments).
pub fn parse_recipients(arg: &str) -> Result<Vec<Box<dyn age::Recipient + Send>>> {
    let one = |line: &str| -> Option<Box<dyn age::Recipient + Send>> {
        if let Ok(r) = age::x25519::Recipient::from_str(line) {
            return Some(Box::new(r));
        }
        if let Ok(r) = age::ssh::Recipient::from_str(line) {
            return Some(Box::new(r));
        }
        None
    };
    if let Some(r) = one(arg.trim()) {
        return Ok(vec![r]);
    }
    let path = Path::new(arg);
    if path.is_file() {
        let text = read_small(path, "recipients file")?;
        let text = String::from_utf8(text.to_vec())
            .map_err(|_| anyhow::anyhow!("{} is not text", path.display()))?;
        let mut out = Vec::new();
        for (i, line) in text.lines().enumerate() {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            match one(line) {
                Some(r) => out.push(r),
                None => bail!(
                    "{}: line {} is not an age (age1...) or SSH public key",
                    path.display(),
                    i + 1
                ),
            }
        }
        if out.is_empty() {
            bail!("{} names no recipient", path.display());
        }
        return Ok(out);
    }
    bail!(
        "--secrets-recipient takes an age public key (age1...), an SSH public key (ssh-ed25519 ..., ssh-rsa ...) or a file of them; that is neither"
    )
}

/// Read a small key or passphrase file, without following it past a size cap.
fn read_small(path: &Path, what: &str) -> Result<Zeroizing<Vec<u8>>> {
    let file = std::fs::File::open(path)
        .with_context(|| format!("reading the {what} {}", path.display()))?;
    let mut buf = Zeroizing::new(Vec::new());
    file.take(MAX_KEY_FILE_BYTES + 1)
        .read_to_end(&mut buf)
        .with_context(|| format!("reading the {what} {}", path.display()))?;
    if buf.len() as u64 > MAX_KEY_FILE_BYTES {
        bail!("the {what} {} is too large", path.display());
    }
    Ok(buf)
}

/// The passphrase in `path`: its first line, without the line ending.
pub fn passphrase_from_file(path: &Path) -> Result<SecretString> {
    let raw = read_small(path, "passphrase file")?;
    let text = std::str::from_utf8(&raw)
        .map_err(|_| anyhow::anyhow!("the passphrase file {} is not UTF-8 text", path.display()))?;
    let line = text.split(['\n', '\r']).next().unwrap_or("");
    if line.is_empty() {
        bail!("the passphrase file {} is empty", path.display());
    }
    Ok(SecretString::from(line.to_string()))
}

/// The identities (`AGE-SECRET-KEY-...` lines, or an unencrypted SSH private
/// key) in `path`.
pub fn identities_from_file(path: &Path) -> Result<Vec<Box<dyn age::Identity + Send + Sync>>> {
    let raw = read_small(path, "identity file")?;
    if let Ok(file) = age::IdentityFile::from_buffer(&raw[..]) {
        let ids = file
            .into_identities()
            .map_err(|e| anyhow::anyhow!("{}: {e}", path.display()))?;
        if !ids.is_empty() {
            return Ok(ids);
        }
    }
    match age::ssh::Identity::from_buffer(&raw[..], Some(path.display().to_string())) {
        Ok(age::ssh::Identity::Unencrypted(k)) => Ok(vec![Box::new(age::ssh::Identity::from(k))]),
        Ok(age::ssh::Identity::Encrypted(_)) => bail!(
            "{} is a passphrase-protected SSH key; kitbag can't unlock those yet. Use an age identity file, an unprotected key, or the passphrase",
            path.display()
        ),
        Ok(age::ssh::Identity::Unsupported(_)) => {
            bail!("{} is an SSH key age can't use", path.display())
        }
        Err(_) => bail!(
            "{} is neither an age identity file (AGE-SECRET-KEY-...) nor an unencrypted SSH private key",
            path.display()
        ),
    }
}

/// The bundle, encrypted. The plaintext lives in memory only and is zeroed.
pub fn seal(bundle: &Bundle, how: &Seal) -> Result<Vec<u8>> {
    // Sized up front, so growing the buffer never leaves a copy behind that
    // the zeroing on drop wouldn't reach.
    let room: usize = bundle
        .entries
        .iter()
        .map(|(k, e)| (k.len() + e.value.expose().len() + e.at.len()) * 2 + 64)
        .sum::<usize>()
        // A JSON string escape is at most six bytes per input byte.
        + bundle
            .tokens
            .iter()
            .map(|t| {
                t.content.expose().len() * 6
                    + (t.tool.len() + t.kind.len()) * 2
                    + t.servers
                        .iter()
                        .map(|(k, v)| (k.len() + v.len()) * 6 + 8)
                        .sum::<usize>()
                    + 128
            })
            .sum::<usize>()
        + 256;
    let mut plain = Zeroizing::new(Vec::with_capacity(room));
    serde_json::to_writer(
        &mut *plain,
        &WireRef {
            kitbag_secrets: if bundle.tokens.is_empty() { 1 } else { 2 },
            secrets: &bundle.entries,
            token_stores: &bundle.tokens,
        },
    )?;
    let encryptor = match how {
        Seal::Passphrase(p) => age::Encryptor::with_user_passphrase(p.clone()),
        Seal::Keys(keys) => {
            age::Encryptor::with_recipients(keys.iter().map(|k| k.as_ref() as &dyn age::Recipient))
                .map_err(|e| anyhow::anyhow!("could not encrypt to those recipients: {e}"))?
        }
    };
    let mut out = Vec::new();
    let mut w = encryptor.wrap_output(&mut out)?;
    w.write_all(&plain)?;
    w.finish()?;
    Ok(out)
}

#[derive(Serialize)]
struct WireRef<'a> {
    kitbag_secrets: u32,
    secrets: &'a BTreeMap<String, Entry>,
    #[serde(skip_serializing_if = "<[_]>::is_empty")]
    token_stores: &'a [TokenStore],
}

/// Encrypt arbitrary bytes the way a bundle is (age: a passphrase or keys).
/// Used by `kitbag sessions export --encrypt`.
pub fn seal_bytes(plain: &[u8], how: &Seal) -> Result<Vec<u8>> {
    let encryptor = match how {
        Seal::Passphrase(p) => age::Encryptor::with_user_passphrase(p.clone()),
        Seal::Keys(keys) => {
            age::Encryptor::with_recipients(keys.iter().map(|k| k.as_ref() as &dyn age::Recipient))
                .map_err(|e| anyhow::anyhow!("could not encrypt to those recipients: {e}"))?
        }
    };
    let mut out = Vec::new();
    let mut w = encryptor.wrap_output(&mut out)?;
    w.write_all(plain)?;
    w.finish()?;
    Ok(out)
}

/// Decrypt age bytes in memory, at most `limit` bytes of plaintext. The
/// result is zeroed when dropped.
pub fn open_bytes(bytes: &[u8], how: &Unlock, limit: u64) -> Result<Zeroizing<Vec<u8>>> {
    if !looks_like_age(bytes) {
        bail!("not an age-encrypted file");
    }
    let decryptor =
        age::Decryptor::new(bytes).map_err(|e| anyhow::anyhow!("not a readable age file: {e}"))?;
    let mut scrypt;
    let mut ids: Vec<&dyn age::Identity> = Vec::new();
    if decryptor.is_scrypt() {
        let Some(p) = &how.passphrase else {
            bail!(
                "this archive is encrypted to a passphrase: give --secrets-passphrase-file, or run in a terminal to be asked"
            );
        };
        scrypt = age::scrypt::Identity::new(p.clone());
        scrypt.set_max_work_factor(MAX_WORK_FACTOR);
        ids.push(&scrypt);
    } else {
        if how.identities.is_empty() {
            bail!(
                "this archive is encrypted to keys: give --secrets-identity with the matching age identity file or SSH key"
            );
        }
        ids.extend(
            how.identities
                .iter()
                .map(|i| i.as_ref() as &dyn age::Identity),
        );
    }
    let reader = decryptor
        .decrypt(ids.into_iter())
        .map_err(|e| anyhow::anyhow!("could not decrypt the archive: {e}"))?;
    let mut plain = Zeroizing::new(Vec::new());
    reader
        .take(limit + 1)
        .read_to_end(&mut plain)
        .map_err(|e| anyhow::anyhow!("could not decrypt the archive: {e}"))?;
    if plain.len() as u64 > limit {
        bail!("the decrypted archive is larger than kitbag writes");
    }
    Ok(plain)
}

/// Does `bytes` start like an age file (binary or armored)?
pub fn looks_like_age(bytes: &[u8]) -> bool {
    bytes.starts_with(b"age-encryption.org/v1\n")
        || bytes.starts_with(b"-----BEGIN AGE ENCRYPTED FILE-----")
}

/// Is the file encrypted to a passphrase (so it needs one to open)?
pub fn needs_passphrase(bytes: &[u8]) -> bool {
    age::Decryptor::new(bytes).is_ok_and(|d| d.is_scrypt())
}

/// Decrypt a bundle in memory.
pub fn open(bytes: &[u8], how: &Unlock) -> Result<Bundle> {
    if !looks_like_age(bytes) {
        bail!("not an age-encrypted file");
    }
    let decryptor =
        age::Decryptor::new(bytes).map_err(|e| anyhow::anyhow!("not a readable age file: {e}"))?;
    let mut scrypt;
    let mut ids: Vec<&dyn age::Identity> = Vec::new();
    if decryptor.is_scrypt() {
        let Some(p) = &how.passphrase else {
            bail!(
                "this bundle is encrypted to a passphrase: give --secrets-passphrase-file, or run in a terminal to be asked"
            );
        };
        scrypt = age::scrypt::Identity::new(p.clone());
        scrypt.set_max_work_factor(MAX_WORK_FACTOR);
        ids.push(&scrypt);
    } else {
        if how.identities.is_empty() {
            bail!(
                "this bundle is encrypted to keys: give --secrets-identity with the matching age identity file or SSH key"
            );
        }
        ids.extend(
            how.identities
                .iter()
                .map(|i| i.as_ref() as &dyn age::Identity),
        );
    }
    let reader = decryptor.decrypt(ids.into_iter()).map_err(|e| {
        // age's own message, which names no secret.
        anyhow::anyhow!("could not decrypt the bundle: {e}")
    })?;
    let mut plain = Zeroizing::new(Vec::new());
    reader
        .take(MAX_BUNDLE_BYTES + 1)
        .read_to_end(&mut plain)
        .map_err(|e| anyhow::anyhow!("could not decrypt the bundle: {e}"))?;
    if plain.len() as u64 > MAX_BUNDLE_BYTES {
        bail!("the bundle is larger than kitbag writes");
    }
    // serde's own error text can quote input: say only that it is malformed.
    let wire: Wire = serde_json::from_slice(&plain)
        .map_err(|_| anyhow::anyhow!("the decrypted bundle is not one kitbag wrote"))?;
    if !FORMATS.contains(&wire.kitbag_secrets) {
        bail!(
            "the bundle has format {}; this kitbag reads formats 1 and 2",
            wire.kitbag_secrets
        );
    }
    if wire.kitbag_secrets == 1 && !wire.token_stores.is_empty() {
        bail!("the decrypted bundle is not one kitbag wrote");
    }
    let mut bundle = Bundle::default();
    for (name, e) in wire.secrets {
        if !valid_name(&name) {
            bail!("the bundle names a variable that is not a valid name; refusing it");
        }
        bundle.entries.insert(name, e);
    }
    for t in wire.token_stores {
        if !valid_label(&t.tool) || !valid_label(&t.kind) {
            bail!("the bundle has a token store with an unusable tool name; refusing it");
        }
        bundle.add_token_store(t);
    }
    Ok(bundle)
}

/// Read and decrypt the bundle at `path` (a local file), in memory.
pub fn open_file(path: &Path, how: &Unlock) -> Result<Bundle> {
    let bytes = read_bundle(path)?;
    open(&bytes, how).with_context(|| format!("opening {}", path.display()))
}

/// The raw bytes of the bundle at `path`.
pub fn read_bundle(path: &Path) -> Result<Vec<u8>> {
    let meta = std::fs::metadata(path).with_context(|| format!("reading {}", path.display()))?;
    if !meta.is_file() {
        bail!("{} is not a file", path.display());
    }
    if meta.len() > MAX_BUNDLE_BYTES * 2 {
        bail!(
            "{} is too large to be a kitbag secrets bundle",
            path.display()
        );
    }
    std::fs::read(path).with_context(|| format!("reading {}", path.display()))
}

/// Write `bytes` (an encrypted bundle) as `dir/secrets.age`: atomically,
/// private to the user on Unix (0600; on Windows the file keeps the access of
/// the folder it is in). A link in its place is replaced, never written through.
pub fn write(dir: &Path, bytes: &[u8]) -> Result<PathBuf> {
    let path = dir.join(FILE);
    write_to(&path, bytes)?;
    Ok(path)
}

/// Write `bytes` (an encrypted bundle) to `path`, as [`write()`] does: through
/// a temporary file in the same directory renamed into place, so a failure
/// leaves the old file as it was.
pub fn write_to(path: &Path, bytes: &[u8]) -> Result<()> {
    if !looks_like_age(bytes) {
        bail!("refusing to write a secrets bundle that is not age-encrypted");
    }
    crate::txn::write_atomic(path, bytes, Some(0o600))
}

/// Files in a profile directory that hold secrets in plain text, which a
/// profile must not have: a decrypted bundle, `.env` files.
pub fn plaintext_secret_files(dir: &Path) -> Vec<String> {
    let Ok(rd) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut out: Vec<String> = rd
        .flatten()
        .filter(|e| e.file_type().is_ok_and(|t| !t.is_dir()))
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|n| plaintext_secret_name(n))
        .collect();
    out.sort();
    out
}

/// Is a file called `name`, at the top of a profile, a plaintext secrets file?
pub fn plaintext_secret_files_named(name: &str) -> bool {
    plaintext_secret_name(name)
}

fn plaintext_secret_name(name: &str) -> bool {
    let n = name.to_ascii_lowercase();
    if n == FILE {
        return false;
    }
    if n == ".env"
        || (n.starts_with(".env.")
            && !n.ends_with(".example")
            && !n.ends_with(".sample")
            && !n.ends_with(".template"))
    {
        return true;
    }
    (n.starts_with("secrets") && !n.ends_with(".md"))
        || n.ends_with(".decrypted")
        || n.ends_with(".dec")
}

/// Is `.gitignore` text (of the profile directory) keeping `secrets.age` out of git?
pub fn gitignore_covers(text: &str) -> bool {
    text.lines().map(str::trim).any(|l| {
        matches!(
            l,
            "secrets.age" | "/secrets.age" | "*.age" | "/*.age" | "**/secrets.age"
        )
    })
}

/// Make sure the profile's `.gitignore` lists `secrets.age`: created if there
/// is none, otherwise a line is added. Returns whether the file changed.
pub fn ensure_gitignored(dir: &Path) -> Result<bool> {
    let path = dir.join(".gitignore");
    let mut text = match std::fs::read_to_string(&path) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(e) => return Err(e).with_context(|| format!("reading {}", path.display())),
    };
    if gitignore_covers(&text) {
        return Ok(false);
    }
    if !text.is_empty() && !text.ends_with('\n') {
        text.push('\n');
    }
    text.push_str("# kitbag: the encrypted secrets bundle stays out of git\n");
    text.push_str(FILE);
    text.push('\n');
    crate::txn::write_atomic(&path, text.as_bytes(), None)?;
    Ok(true)
}

/// Does a `.gitignore` line name exactly the bundle (not a wider pattern)?
fn is_bundle_rule(line: &str) -> bool {
    matches!(
        line.trim(),
        "secrets.age" | "/secrets.age" | "**/secrets.age"
    )
}

/// `.gitignore` text without the lines that list exactly `secrets.age`, and
/// the comment kitbag puts above its own line. Every other line stays.
pub fn without_bundle_rule(text: &str) -> String {
    let lines: Vec<&str> = text.lines().collect();
    let mut out = String::new();
    for (i, l) in lines.iter().enumerate() {
        if is_bundle_rule(l) {
            continue;
        }
        let kitbag_note = l.trim() == "# kitbag: the encrypted secrets bundle stays out of git"
            && lines.get(i + 1).is_some_and(|n| is_bundle_rule(n));
        if kitbag_note {
            continue;
        }
        out.push_str(l);
        out.push('\n');
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const FAKE_VALUE: &str = "fake-local-token-value-1234";
    const FAKE_PASS: &str = "fake-passphrase-for-tests";

    fn sample() -> Bundle {
        let mut b = Bundle::default();
        b.insert(
            "GITHUB_TOKEN",
            Secret::new(FAKE_VALUE),
            "mcp_servers.gh.env.GITHUB_TOKEN",
        )
        .unwrap();
        b
    }

    #[test]
    fn debug_and_errors_never_show_a_value() {
        let b = sample();
        assert!(!format!("{b:?}").contains(FAKE_VALUE));
        assert!(!format!("{:?}", Secret::new(FAKE_VALUE)).contains(FAKE_VALUE));
        let seal = Seal::Passphrase(SecretString::from(FAKE_PASS.to_string()));
        assert!(!format!("{seal:?}").contains(FAKE_PASS));
        let un = Unlock {
            passphrase: Some(SecretString::from(FAKE_PASS.to_string())),
            identities: vec![],
        };
        assert!(!format!("{un:?}").contains(FAKE_PASS));
        let e = b
            .clone()
            .insert("GITHUB_TOKEN", Secret::new("fake-other-value-9999"), "x");
        let msg = format!("{:#}", e.unwrap_err());
        assert!(!msg.contains("fake-other-value-9999") && !msg.contains(FAKE_VALUE));
    }

    #[test]
    fn a_key_encrypted_bundle_round_trips_and_holds_no_plaintext() {
        let id = age::x25519::Identity::generate();
        let recipient = id.to_public().to_string();
        let sealed = seal(
            &sample(),
            &Seal::Keys(parse_recipients(&recipient).unwrap()),
        )
        .unwrap();
        assert!(looks_like_age(&sealed));
        assert!(
            !sealed
                .windows(FAKE_VALUE.len())
                .any(|w| w == FAKE_VALUE.as_bytes())
        );
        assert!(!needs_passphrase(&sealed));
        let open = open(
            &sealed,
            &Unlock {
                passphrase: None,
                identities: vec![Box::new(id)],
            },
        )
        .unwrap();
        assert_eq!(open, sample());
        // Without the identity it stays closed.
        assert!(
            super::open(
                &sealed,
                &Unlock {
                    passphrase: None,
                    identities: vec![]
                }
            )
            .is_err()
        );
    }

    #[test]
    fn a_passphrase_bundle_opens_only_with_the_passphrase() {
        let sealed = seal(
            &sample(),
            &Seal::Passphrase(SecretString::from(FAKE_PASS.to_string())),
        )
        .unwrap();
        assert!(needs_passphrase(&sealed));
        let with = |p: &str| Unlock {
            passphrase: Some(SecretString::from(p.to_string())),
            identities: vec![],
        };
        assert_eq!(open(&sealed, &with(FAKE_PASS)).unwrap(), sample());
        let err = format!(
            "{:#}",
            open(&sealed, &with("fake-wrong-passphrase")).unwrap_err()
        );
        assert!(!err.contains("fake-wrong-passphrase") && !err.contains(FAKE_VALUE));
    }

    #[test]
    fn plaintext_and_garbage_are_not_bundles() {
        assert!(
            open(
                b"{\"secrets\":{}}",
                &Unlock {
                    passphrase: None,
                    identities: vec![]
                }
            )
            .is_err()
        );
        let dir = tempfile::tempdir().unwrap();
        assert!(write(dir.path(), b"plain").is_err());
        assert!(parse_recipients("not-a-key").is_err());
    }

    #[test]
    fn set_adds_or_replaces_and_keeps_the_capture_location() {
        let mut b = Bundle::default();
        b.insert("A", Secret::new("fake-one"), "mcp_servers.a.env.A")
            .unwrap();
        assert!(b.set("A", Secret::new("fake-two"), "secrets set").unwrap());
        assert!(
            !b.set("B", Secret::new("fake-three"), "secrets set")
                .unwrap()
        );
        assert_eq!(b.get("A").unwrap().expose(), "fake-two");
        assert_eq!(
            b.names(),
            [
                ("A".to_string(), "mcp_servers.a.env.A".to_string()),
                ("B".to_string(), "secrets set".to_string())
            ]
        );
        assert!(b.set("1A", Secret::new("x"), "secrets set").is_err());
    }

    #[test]
    fn names_are_checked() {
        assert!(valid_name("GITHUB_TOKEN") && valid_name("_X1"));
        assert!(!valid_name("1X") && !valid_name("A-B") && !valid_name("") && !valid_name("A B"));
        let mut b = Bundle::default();
        assert!(b.insert("A=B", Secret::new("x"), "").is_err());
    }

    #[test]
    fn plaintext_secret_files_are_recognised() {
        for n in [
            "secrets.json",
            "secrets.toml",
            "secrets.decrypted",
            "Secrets.txt",
            "secrets",
            "secrets.age.dec",
            ".env",
            ".env.local",
            "x.decrypted",
        ] {
            assert!(plaintext_secret_name(n), "{n}");
        }
        for n in [
            "secrets.age",
            "kitbag.toml",
            ".env.example",
            "README.md",
            "instructions.md",
            "secrets.md",
            ".gitignore",
        ] {
            assert!(!plaintext_secret_name(n), "{n}");
        }
    }

    #[test]
    fn gitignore_is_created_and_extended_once() {
        let dir = tempfile::tempdir().unwrap();
        assert!(ensure_gitignored(dir.path()).unwrap());
        assert!(!ensure_gitignored(dir.path()).unwrap());
        let text = std::fs::read_to_string(dir.path().join(".gitignore")).unwrap();
        assert!(gitignore_covers(&text));
        std::fs::write(dir.path().join(".gitignore"), "target").unwrap();
        assert!(ensure_gitignored(dir.path()).unwrap());
        let text = std::fs::read_to_string(dir.path().join(".gitignore")).unwrap();
        assert!(text.starts_with("target\n") && gitignore_covers(&text));
    }

    const FAKE_TOKEN: &str = "ya29.FAKE-gemini-access-unit-test";

    fn store() -> TokenStore {
        TokenStore {
            tool: "gemini-cli".into(),
            kind: crate::oauth_tokens::GEMINI_KIND.into(),
            servers: [(
                "linear".to_string(),
                "https://mcp.example.com/mcp".to_string(),
            )]
            .into(),
            content: Secret::new(&format!("{{\"t\":\"{FAKE_TOKEN}\"}}\n")),
        }
    }

    fn pass() -> Unlock {
        Unlock {
            passphrase: Some(SecretString::from(FAKE_PASS.to_string())),
            identities: vec![],
        }
    }

    fn sealed_plain(plain: &str) -> Vec<u8> {
        let enc = age::Encryptor::with_user_passphrase(SecretString::from(FAKE_PASS.to_string()));
        let mut out = Vec::new();
        let mut w = enc.wrap_output(&mut out).unwrap();
        w.write_all(plain.as_bytes()).unwrap();
        w.finish().unwrap();
        out
    }

    #[test]
    fn token_stores_round_trip_as_format_2_and_stay_encrypted() {
        let mut b = sample();
        b.add_token_store(store());
        assert!(!format!("{b:?}").contains(FAKE_TOKEN));
        let sealed = seal(
            &b,
            &Seal::Passphrase(SecretString::from(FAKE_PASS.to_string())),
        )
        .unwrap();
        assert!(
            !sealed
                .windows(FAKE_TOKEN.len())
                .any(|w| w == FAKE_TOKEN.as_bytes())
        );
        let back = open(&sealed, &pass()).unwrap();
        assert_eq!(back, b);
        assert_eq!(back.token_stores()[0].content, store().content);
        // A second store for the same tool replaces the first.
        b.add_token_store(store());
        assert_eq!(b.token_stores().len(), 1);
    }

    #[test]
    fn a_format_1_bundle_still_opens_and_bad_formats_are_refused() {
        let old = sealed_plain(&format!(
            "{{\"kitbag_secrets\":1,\"secrets\":{{\"GITHUB_TOKEN\":{{\"value\":\"{FAKE_VALUE}\",\"at\":\"mcp_servers.gh.env.GITHUB_TOKEN\"}}}}}}"
        ));
        let b = open(&old, &pass()).unwrap();
        assert_eq!(b, sample());
        assert!(b.token_stores().is_empty());
        // Format 1 carries no token stores, and a later format is refused.
        let bad = sealed_plain(
            "{\"kitbag_secrets\":1,\"secrets\":{},\"token_stores\":[{\"tool\":\"gemini-cli\",\"kind\":\"k\",\"content\":\"x\"}]}",
        );
        assert!(open(&bad, &pass()).is_err());
        let future = sealed_plain("{\"kitbag_secrets\":3,\"secrets\":{}}");
        assert!(open(&future, &pass()).is_err());
        let odd = sealed_plain(
            "{\"kitbag_secrets\":2,\"secrets\":{},\"token_stores\":[{\"tool\":\"../x\",\"kind\":\"k\",\"content\":\"x\"}]}",
        );
        assert!(open(&odd, &pass()).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn the_bundle_file_is_private() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let sealed = seal(
            &sample(),
            &Seal::Passphrase(SecretString::from(FAKE_PASS.to_string())),
        )
        .unwrap();
        let path = write(dir.path(), &sealed).unwrap();
        let mode = std::fs::metadata(path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);
    }

    #[test]
    fn the_bundle_rule_is_removed_and_other_lines_stay() {
        let text =
            "target\n# kitbag: the encrypted secrets bundle stays out of git\nsecrets.age\n*.log\n";
        let out = without_bundle_rule(text);
        assert_eq!(out, "target\n*.log\n");
        assert!(!gitignore_covers(&out));
        assert!(gitignore_covers(&without_bundle_rule(
            "*.age\nsecrets.age\n"
        )));
    }
}
