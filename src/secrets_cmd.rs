//! The command-line side of the encrypted secrets bundle: the flags of
//! `capture --include secrets`, `apply --secrets-from` and `kitbag secrets`.
//! The crypto and file handling are in [`crate::bundle`].
//!
//! Passphrases come from a file or a terminal prompt, never from the command
//! line, and nothing here prints a value: only variable names.

use std::io::IsTerminal;
use std::path::{Path, PathBuf};

use age::secrecy::SecretString;
use anstream::println;
use anyhow::{Result, bail};
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
    /// list its variable names; no value is written into any tool config or
    /// kept in the undo record
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
}

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
    let bytes = bundle::read_bundle(file)?;
    let u = unlock(a, &bytes)?;
    let b = bundle::open(&bytes, &u).map_err(|e| anyhow::anyhow!("{}: {e:#}", file.display()))?;
    Ok(b.names())
}

/// `kitbag secrets list`.
pub fn list(file: Option<&Path>, a: &UnlockArgs) -> Result<i32> {
    let given = file.unwrap_or_else(|| Path::new("."));
    if let Some(s) = given.to_str()
        && looks_remote(s)
    {
        bail!("a secrets bundle is read from a file on this machine, never from a git source");
    }
    let path = if given.is_dir() {
        given.join(bundle::FILE)
    } else {
        given.to_path_buf()
    };
    let names = names_of(&path, a)?;
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
/// which of the profile's variables it covers. Names only.
pub fn apply_notes(from: &FromArgs, needed: &[String], git_source: bool) -> Result<Vec<Note>> {
    let Some(path) = &from.secrets_from else {
        return Ok(Vec::new());
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
    let held = names_of(path, &from.unlock_args())?;
    let held: Vec<String> = held.into_iter().map(|(n, _)| n).collect();
    let covered: Vec<&String> = needed.iter().filter(|n| held.contains(n)).collect();
    let lacking: Vec<&String> = needed.iter().filter(|n| !held.contains(n)).collect();
    let list = |v: &[&String]| v.iter().map(|s| s.as_str()).collect::<Vec<_>>().join(", ");
    let mut notes = vec![Note {
        level: Level::Info,
        target: "secrets".to_string(),
        message: format!(
            "{} holds {} variable(s); it was decrypted in memory to check it, and no value was written into any tool or kept in the undo record. Tools get ${{VAR}} references as before: set the variables in your environment{}",
            path.display(),
            held.len(),
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
    Ok(notes)
}
