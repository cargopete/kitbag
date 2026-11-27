//! The command-line side of the encrypted secrets bundle: the flags of
//! `capture --include secrets`, `apply --secrets-from` and `kitbag secrets`
//! (`list`, `set`, `rotate`).
//! The crypto and file handling are in [`crate::bundle`].
//!
//! Passphrases come from a file or a terminal prompt, never from the command
//! line, and nothing here prints a value: only variable names.

use std::io::{IsTerminal, Read};
use std::path::{Path, PathBuf};

use age::secrecy::{ExposeSecret, SecretString};
use anstream::println;
use anyhow::{Context, Result, bail};
use clap::{Args, Subcommand};

use crate::adapters::{Level, Note};
use crate::bundle::{self, Bundle, Seal, Unlock};
use crate::secrets::Redaction;
use crate::ui::{BOLD, DIM, GREEN, paint, visible_line};

/// What `capture --include secrets` encrypts the bundle to.
#[derive(Args, Debug, Clone, Default)]
pub struct SealArgs {
    /// With --include secrets: a file whose first line is the passphrase to
    /// encrypt secrets.age with (age's scrypt). Without this or
    /// --secrets-recipient, a terminal asks for one (twice). Never put a
    /// passphrase on the command line
    #[arg(long, value_name = "PATH")]
    pub secrets_passphrase_file: Option<PathBuf>,
    /// With --include secrets: encrypt secrets.age to this age public key
    /// (age1...) or SSH public key (ssh-ed25519 ..., ssh-rsa ...), or a file
    /// of them, one per line. Repeatable. Can't be combined with a passphrase
    #[arg(long, value_name = "KEY")]
    pub secrets_recipient: Vec<String>,
}

impl SealArgs {
    pub fn is_set(&self) -> bool {
        self.secrets_passphrase_file.is_some() || !self.secrets_recipient.is_empty()
    }
}

/// How to open a bundle.
#[derive(Args, Debug, Clone, Default)]
pub struct UnlockArgs {
    /// A file whose first line is the bundle's passphrase. Without it, a
    /// terminal asks. Never put a passphrase on the command line
    #[arg(long, value_name = "PATH")]
    pub secrets_passphrase_file: Option<PathBuf>,
    /// An age identity file (AGE-SECRET-KEY-...) or an unprotected SSH private
    /// key that the bundle was encrypted to. Repeatable
    #[arg(long, value_name = "PATH")]
    pub secrets_identity: Vec<PathBuf>,
}

/// The `apply --secrets-from` flags.
#[derive(Args, Debug, Clone, Default)]
pub struct FromArgs {
    /// A secrets.age written by 'capture --include secrets': a local file, never a
    /// git URL, and not with --remote. It is decrypted in memory to check it and
    /// list its variable names; no variable's value is written into any tool
    /// config or kept in the undo record. A Gemini CLI MCP OAuth token store in
    /// it is written to Gemini CLI's documented token file only for gemini-cli,
    /// only if that file is missing or empty, and outside the undo record
    #[arg(long, value_name = "PATH")]
    pub secrets_from: Option<PathBuf>,
    /// With --secrets-from: a file whose first line is the bundle's passphrase
    /// (otherwise a terminal asks)
    #[arg(
        long,
        value_name = "PATH",
        requires = "secrets_from",
        conflicts_with = "secrets_identity"
    )]
    pub secrets_passphrase_file: Option<PathBuf>,
    /// With --secrets-from: an age identity file or unprotected SSH private
    /// key the bundle was encrypted to. Repeatable
    #[arg(long, value_name = "PATH", requires = "secrets_from")]
    pub secrets_identity: Vec<PathBuf>,
}

impl FromArgs {
    fn unlock_args(&self) -> UnlockArgs {
        UnlockArgs {
            secrets_passphrase_file: self.secrets_passphrase_file.clone(),
            secrets_identity: self.secrets_identity.clone(),
        }
    }
}

#[derive(Subcommand, Debug)]
pub enum SecretsCmd {
    /// List the variable names in an encrypted secrets bundle (never the values)
    List {
        /// The bundle, or the profile directory holding secrets.age [default: .]
        file: Option<PathBuf>,
        #[command(flatten)]
        unlock: UnlockArgs,
    },
    /// Add or replace one variable in a secrets bundle. The value is read from
    /// --value-file (never the command line) and never shown
    Set {
        /// The variable name (GITHUB_TOKEN)
        name: String,
        /// The bundle, or the profile directory holding secrets.age [default: .].
        /// Created if it doesn't exist
        file: Option<PathBuf>,
        /// A file holding the value ('-' for stdin). One trailing newline is
        /// dropped
        #[arg(long, value_name = "PATH", required = true)]
        value_file: PathBuf,
        /// A file whose first line is the passphrase: the bundle's, to open it
        /// and encrypt it again with the same one, or a new bundle's. Never put
        /// a passphrase on the command line
        #[arg(long, value_name = "PATH")]
        secrets_passphrase_file: Option<PathBuf>,
        /// An age identity file or unprotected SSH private key that opens a
        /// key-encrypted bundle. Repeatable
        #[arg(long, value_name = "PATH")]
        secrets_identity: Vec<PathBuf>,
        /// Encrypt a new or key-encrypted bundle to this age or SSH public key,
        /// or a file of them. Repeatable; needed to rewrite a key-encrypted
        /// bundle. Can't be combined with a passphrase
        #[arg(long, value_name = "KEY")]
        secrets_recipient: Vec<String>,
    },
    /// Encrypt a secrets bundle again to a new passphrase or new keys, with the
    /// same variables and token stores. The values are never shown
    Rotate {
        /// The bundle, or the profile directory holding secrets.age [default: .]
        file: Option<PathBuf>,
        #[command(flatten)]
        unlock: UnlockArgs,
        /// A file whose first line is the new passphrase. Never put a
        /// passphrase on the command line
        #[arg(long, value_name = "PATH")]
        new_secrets_passphrase_file: Option<PathBuf>,
        /// Encrypt to this age or SSH public key instead, or a file of them.
        /// Repeatable. Can't be combined with a new passphrase
        #[arg(long, value_name = "KEY")]
        new_secrets_recipient: Vec<String>,
    },
}

/// Where a capture location is for a variable `secrets set` added.
const SET_AT: &str = "secrets set";
/// The most a value file may hold.
const MAX_VALUE_BYTES: u64 = 64 * 1024;

/// What to encrypt a bundle to, before any prompt.
pub enum SealPlan {
    Passphrase(SecretString),
    Keys(Vec<Box<dyn age::Recipient + Send>>),
    /// Ask on the terminal.
    Ask,
}

/// Check the capture flags early, so a bad one fails before anything is read.
pub fn seal_plan(a: &SealArgs) -> Result<SealPlan> {
    match (&a.secrets_passphrase_file, a.secrets_recipient.is_empty()) {
        (Some(_), false) => bail!(
            "--secrets-passphrase-file and --secrets-recipient can't be combined: age encrypts to a passphrase or to keys, not both"
        ),
        (Some(f), true) => Ok(SealPlan::Passphrase(bundle::passphrase_from_file(f)?)),
        (None, false) => {
            let mut keys = Vec::new();
            for r in &a.secrets_recipient {
                keys.extend(bundle::parse_recipients(r)?);
            }
            Ok(SealPlan::Keys(keys))
        }
        (None, true) => Ok(SealPlan::Ask),
    }
}

/// The encryption, asking for a passphrase on the terminal if needed.
pub fn resolve_seal(plan: SealPlan) -> Result<Seal> {
    Ok(match plan {
        SealPlan::Passphrase(p) => Seal::Passphrase(p),
        SealPlan::Keys(k) => Seal::Keys(k),
        SealPlan::Ask => {
            if !std::io::stdin().is_terminal() {
                bail!(
                    "no way to encrypt secrets.age: give --secrets-passphrase-file PATH or --secrets-recipient KEY (a passphrase is only asked for on a terminal)"
                );
            }
            let first = rpassword::prompt_password("New passphrase for secrets.age: ")?;
            let again = rpassword::prompt_password("Same passphrase again: ")?;
            if first != again {
                bail!("the passphrases differ; nothing was written");
            }
            if first.is_empty() {
                bail!("an empty passphrase is not accepted");
            }
            Seal::Passphrase(SecretString::from(first))
        }
    })
}

/// The values capture kept, as a bundle. An error names the variable only.
pub fn bundle_of(redactions: &[Redaction]) -> Result<Bundle> {
    let mut b = Bundle::default();
    for r in redactions {
        if let Some(v) = &r.value {
            b.insert(&r.env_var, v.clone(), &r.location)?;
        }
    }
    Ok(b)
}

/// Passphrase and identities for opening `bytes`; a terminal is asked for a
/// passphrase the file needs and none was given.
pub fn unlock(a: &UnlockArgs, bytes: &[u8]) -> Result<Unlock> {
    let mut u = Unlock {
        passphrase: None,
        identities: Vec::new(),
    };
    if let Some(f) = &a.secrets_passphrase_file {
        u.passphrase = Some(bundle::passphrase_from_file(f)?);
    }
    for f in &a.secrets_identity {
        u.identities.extend(bundle::identities_from_file(f)?);
    }
    if u.passphrase.is_none()
        && u.identities.is_empty()
        && bundle::needs_passphrase(bytes)
        && std::io::stdin().is_terminal()
    {
        let p = rpassword::prompt_password("Passphrase for secrets.age: ")?;
        u.passphrase = Some(SecretString::from(p));
    }
    Ok(u)
}

/// The variable names of a bundle, with the places they were captured from.
/// The values are decrypted in memory to get here and dropped before this returns.
pub fn names_of(file: &Path, a: &UnlockArgs) -> Result<Vec<(String, String)>> {
    Ok(open_bundle(file, a)?.names())
}

/// The bundle at `file`, decrypted in memory.
fn open_bundle(file: &Path, a: &UnlockArgs) -> Result<Bundle> {
    let bytes = bundle::read_bundle(file)?;
    let u = unlock(a, &bytes)?;
    bundle::open(&bytes, &u).map_err(|e| anyhow::anyhow!("{}: {e:#}", file.display()))
}

/// The bundle file `file` names: itself, or `secrets.age` in it if it is a
/// directory. Never a git source.
fn local_bundle_path(file: Option<&Path>) -> Result<PathBuf> {
    let given = file.unwrap_or_else(|| Path::new("."));
    if let Some(s) = given.to_str()
        && looks_remote(s)
    {
        bail!("a secrets bundle is read from a file on this machine, never from a git source");
    }
    Ok(if given.is_dir() {
        given.join(bundle::FILE)
    } else {
        given.to_path_buf()
    })
}

/// `kitbag secrets list`.
pub fn list(file: Option<&Path>, a: &UnlockArgs) -> Result<i32> {
    let path = local_bundle_path(file)?;
    let b = open_bundle(&path, a)?;
    let names = b.names();
    let stores: Vec<String> = b
        .token_stores()
        .iter()
        .map(crate::oauth_tokens::describe)
        .collect();
    drop(b);
    println!(
        "{} {} variable(s), names only; the values are never shown:",
        paint(BOLD, visible_line(&path.display().to_string())),
        names.len()
    );
    for (name, at) in names {
        println!(
            "  {}  {}",
            paint(GREEN, visible_line(&name)),
            paint(DIM, visible_line(&at))
        );
    }
    if !stores.is_empty() {
        println!(
            "{} token store(s), tool and location only; the tokens are never shown:",
            stores.len()
        );
        for s in stores {
            println!("  {}", paint(GREEN, visible_line(&s)));
        }
    }
    Ok(0)
}

/// The value in `path` (`-` is stdin), without one trailing newline. An
/// error names the file, never its content.
fn read_value(path: &Path) -> Result<bundle::Secret> {
    let mut buf = zeroize::Zeroizing::new(Vec::new());
    let what = if path == Path::new("-") {
        "standard input".to_string()
    } else {
        format!("the value file {}", path.display())
    };
    let got = if path == Path::new("-") {
        std::io::stdin()
            .lock()
            .take(MAX_VALUE_BYTES + 1)
            .read_to_end(&mut buf)
    } else {
        std::fs::File::open(path).and_then(|f| f.take(MAX_VALUE_BYTES + 1).read_to_end(&mut buf))
    };
    got.with_context(|| format!("reading {what}"))?;
    if buf.len() as u64 > MAX_VALUE_BYTES {
        bail!("{what} is larger than a secret value kitbag keeps");
    }
    let end = if buf.ends_with(b"\r\n") {
        buf.len() - 2
    } else if buf.ends_with(b"\n") {
        buf.len() - 1
    } else {
        buf.len()
    };
    let Ok(text) = std::str::from_utf8(&buf[..end]) else {
        bail!("{what} is not UTF-8 text");
    };
    if text.is_empty() {
        bail!("{what} is empty; no value was set");
    }
    Ok(bundle::Secret::new(text))
}

/// The arguments of `kitbag secrets set`.
pub struct SetArgs<'a> {
    pub name: &'a str,
    pub file: Option<&'a Path>,
    pub value_file: &'a Path,
    pub passphrase_file: Option<&'a Path>,
    pub identities: &'a [PathBuf],
    pub recipients: &'a [String],
}

/// `kitbag secrets set`: add or replace one variable, encrypt the bundle
/// again and replace the file in one rename. The value is never shown.
pub fn set(a: &SetArgs) -> Result<i32> {
    if !bundle::valid_name(a.name) {
        bail!(
            "'{}' is not a usable variable name (letters, digits and _, not starting with a digit)",
            a.name
        );
    }
    let path = local_bundle_path(a.file)?;
    if a.passphrase_file.is_some() && !a.recipients.is_empty() {
        bail!(
            "--secrets-passphrase-file and --secrets-recipient can't be combined: age encrypts to a passphrase or to keys, not both"
        );
    }
    let exists = std::fs::symlink_metadata(&path).is_ok();
    // Settle how it will be encrypted before reading anything secret.
    let (mut b, seal) = if exists {
        let bytes = bundle::read_bundle(&path)?;
        if bundle::needs_passphrase(&bytes) {
            if !a.recipients.is_empty() {
                bail!(
                    "{} is encrypted to a passphrase; 'secrets set' keeps it so. To encrypt it to keys, use 'kitbag secrets rotate --new-secrets-recipient'",
                    path.display()
                );
            }
        } else if bundle::looks_like_age(&bytes) {
            if a.passphrase_file.is_some() {
                bail!(
                    "{} is encrypted to keys; 'secrets set' keeps it so. To change it to a passphrase, use 'kitbag secrets rotate --new-secrets-passphrase-file'",
                    path.display()
                );
            }
            if a.recipients.is_empty() {
                bail!(
                    "{} is encrypted to keys: give --secrets-recipient (the keys to encrypt it to again) as well as --secrets-identity to open it",
                    path.display()
                );
            }
        }
        let unlock_args = UnlockArgs {
            secrets_passphrase_file: a.passphrase_file.map(Path::to_path_buf),
            secrets_identity: a.identities.to_vec(),
        };
        let u = unlock(&unlock_args, &bytes)?;
        let b =
            bundle::open(&bytes, &u).map_err(|e| anyhow::anyhow!("{}: {e:#}", path.display()))?;
        let seal = match u.passphrase {
            Some(p) if bundle::needs_passphrase(&bytes) => Seal::Passphrase(p),
            _ => resolve_seal(seal_plan(&SealArgs {
                secrets_passphrase_file: None,
                secrets_recipient: a.recipients.to_vec(),
            })?)?,
        };
        (b, seal)
    } else {
        if !a.identities.is_empty() {
            bail!(
                "{} doesn't exist, so there is nothing to open with --secrets-identity; give --secrets-recipient to encrypt a new bundle",
                path.display()
            );
        }
        let plan = seal_plan(&SealArgs {
            secrets_passphrase_file: a.passphrase_file.map(Path::to_path_buf),
            secrets_recipient: a.recipients.to_vec(),
        })?;
        (Bundle::default(), resolve_seal(plan)?)
    };
    let value = read_value(a.value_file)?;
    let replaced = b.set(a.name, value, SET_AT)?;
    let sealed = bundle::seal(&b, &seal)?;
    let count = b.len();
    drop(b);
    bundle::write_to(&path, &sealed)?;
    let ignored = if exists {
        false
    } else {
        match path.parent() {
            Some(dir) if path.file_name() == Some(bundle::FILE.as_ref()) => {
                let dir = if dir.as_os_str().is_empty() {
                    Path::new(".")
                } else {
                    dir
                };
                dir.join("kitbag.toml").is_file() && bundle::ensure_gitignored(dir)?
            }
            _ => false,
        }
    };
    println!(
        "{} {} in {} ({} variable(s), encrypted to {}); the value is never shown",
        paint(GREEN, if replaced { "Replaced" } else { "Added" }),
        paint(BOLD, visible_line(a.name)),
        visible_line(&path.display().to_string()),
        count,
        describe(&seal)
    );
    if ignored {
        println!("  and listed secrets.age in the profile's .gitignore");
    }
    Ok(0)
}

/// What a bundle is encrypted to, for a message.
fn describe(seal: &Seal) -> String {
    match seal {
        Seal::Passphrase(_) => "a passphrase".to_string(),
        Seal::Keys(k) => format!("{} key(s)", k.len()),
    }
}

/// `kitbag secrets rotate`: open the bundle, encrypt the same variables and
/// token stores to a new passphrase or new keys, and replace the file in one
/// rename. Nothing is written in plain text, and a failure leaves the file
/// as it was.
pub fn rotate(
    file: Option<&Path>,
    current: &UnlockArgs,
    new_passphrase_file: Option<&Path>,
    new_recipients: &[String],
) -> Result<i32> {
    let path = local_bundle_path(file)?;
    let plan = match (new_passphrase_file, new_recipients.is_empty()) {
        (Some(_), false) => bail!(
            "--new-secrets-passphrase-file and --new-secrets-recipient can't be combined: age encrypts to a passphrase or to keys, not both"
        ),
        (None, true) => bail!(
            "give the new encryption: --new-secrets-passphrase-file PATH or --new-secrets-recipient KEY (rotate never keeps the old one)"
        ),
        _ => seal_plan(&SealArgs {
            secrets_passphrase_file: new_passphrase_file.map(Path::to_path_buf),
            secrets_recipient: new_recipients.to_vec(),
        })?,
    };
    let bytes = bundle::read_bundle(&path)?;
    let u = unlock(current, &bytes)?;
    let b = bundle::open(&bytes, &u).map_err(|e| anyhow::anyhow!("{}: {e:#}", path.display()))?;
    if let (SealPlan::Passphrase(new), Some(old)) = (&plan, &u.passphrase)
        && bundle::needs_passphrase(&bytes)
        && new.expose_secret() == old.expose_secret()
    {
        bail!("the new passphrase is the same as the current one; nothing was written");
    }
    drop(u);
    let seal = resolve_seal(plan)?;
    let sealed = bundle::seal(&b, &seal)?;
    let (count, stores) = (b.len(), b.token_stores().len());
    drop(b);
    bundle::write_to(&path, &sealed)?;
    println!(
        "{} {} to {}: {} variable(s) and {} token store(s), the same as before; the values are never shown",
        paint(GREEN, "Encrypted"),
        paint(BOLD, visible_line(&path.display().to_string())),
        describe(&seal),
        count,
        stores
    );
    Ok(0)
}

fn looks_remote(src: &str) -> bool {
    src.contains("://")
        || src.starts_with("github:")
        || src.starts_with("gh:")
        || src.starts_with("git@")
        || src.ends_with(".git")
}

/// `apply --secrets-from`: open the bundle in memory to check it, and say
/// which of the profile's variables it covers (names only). Returns the
/// bundle's MCP OAuth token stores too, for [`crate::oauth_tokens::plan`].
pub fn apply_notes(
    from: &FromArgs,
    needed: &[String],
    git_source: bool,
) -> Result<(Vec<Note>, Vec<bundle::TokenStore>)> {
    let Some(path) = &from.secrets_from else {
        return Ok((Vec::new(), Vec::new()));
    };
    if git_source {
        bail!(
            "--secrets-from isn't used with a git source: a profile you didn't write could name any variable. Clone it, review it, and apply the directory"
        );
    }
    if let Some(s) = path.to_str()
        && looks_remote(s)
    {
        bail!("--secrets-from takes a file on this machine, not a git source");
    }
    let mut b = open_bundle(path, &from.unlock_args())?;
    let stores = b.take_token_stores();
    let held = b.names();
    drop(b);
    let held: Vec<String> = held.into_iter().map(|(n, _)| n).collect();
    let covered: Vec<&String> = needed.iter().filter(|n| held.contains(n)).collect();
    let lacking: Vec<&String> = needed.iter().filter(|n| !held.contains(n)).collect();
    let list = |v: &[&String]| v.iter().map(|s| s.as_str()).collect::<Vec<_>>().join(", ");
    let mut notes = vec![Note {
        level: Level::Info,
        target: "secrets".to_string(),
        message: format!(
            "{} holds {} variable(s); it was decrypted in memory to check it, and no {} was written into any tool or kept in the undo record. Tools get ${{VAR}} references as before: set the variables in your environment{}",
            path.display(),
            held.len(),
            if stores.is_empty() {
                "value"
            } else {
                "variable's value"
            },
            if covered.is_empty() {
                String::new()
            } else {
                format!(". The bundle has: {}", list(&covered))
            }
        ),
    }];
    if !lacking.is_empty() {
        notes.push(Note {
            level: Level::Warn,
            target: "secrets".to_string(),
            message: format!(
                "the bundle has no value for the profile's variable(s): {}",
                list(&lacking)
            ),
        });
    }
    Ok((notes, stores))
}
