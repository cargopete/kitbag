//! Secret handling.
//!
//! Model: a profile never contains secret values. Secrets are represented as
//! `${ENV_VAR}` references (optionally `${ENV_VAR:-default}` for non-secret
//! defaults). On capture, literal values that look like secrets are replaced
//! with references. On apply, references are written in the target's native
//! interpolation syntax so the secret never touches disk, or, only for
//! targets without interpolation support, resolved from the environment.

use std::collections::{BTreeSet, HashMap};
use std::path::Path;
use std::sync::LazyLock;

use regex::{Captures, Regex};

use crate::model::McpServer;

static REF_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"\$\{([A-Za-z_][A-Za-z0-9_]*)(?::-([^}]*))?\}").expect("valid regex")
});
static SOLE_REF_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^\$\{([A-Za-z_][A-Za-z0-9_]*)(?::-([^}]*))?\}$").expect("valid regex")
});
static ENV_COLON_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\$\{env:([A-Za-z_][A-Za-z0-9_]*)\}").expect("valid regex"));
static ENV_BRACE_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(\$?)\{env:([A-Za-z_][A-Za-z0-9_]*)\}").expect("valid regex"));

/// A `${NAME}` / `${NAME:-fallback}` reference.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EnvRef {
    pub name: String,
    pub fallback: Option<String>,
}

/// Env var names referenced in a string, in order of appearance.
pub fn refs_in(value: &str) -> Vec<String> {
    REF_RE
        .captures_iter(value)
        .map(|c| c[1].to_string())
        .collect()
}

pub fn has_ref(value: &str) -> bool {
    REF_RE.is_match(value)
}

/// `value` without its `${...}` references: the literal text that still has
/// to be checked for secrets.
pub fn strip_refs(value: &str) -> String {
    REF_RE.replace_all(value, "").into_owned()
}

/// A literal part long enough to matter next to a reference
/// (`hunter2-${X}`, not `${A}-${B}` or `key=${K}`).
fn substantial_literal(literal: &str) -> bool {
    literal.chars().filter(char::is_ascii_alphanumeric).count() >= 6
}

/// If the whole string is exactly one reference, return it.
pub fn sole_ref(value: &str) -> Option<EnvRef> {
    SOLE_REF_RE.captures(value).map(|c| EnvRef {
        name: c[1].to_string(),
        fallback: c.get(2).map(|m| m.as_str().to_string()),
    })
}

pub fn reference(name: &str) -> String {
    format!("${{{name}}}")
}

/// Convert tool-specific reference syntaxes into canonical `${VAR}`.
///
/// `${env:VAR}` (Cursor, VS Code) is always converted; bare `$VAR` (Gemini CLI)
/// only when `bare_dollar` is set.
pub fn normalize_refs(value: &str, bare_dollar: bool) -> String {
    let v = ENV_COLON_RE.replace_all(value, "$${$1}").into_owned();
    if !bare_dollar {
        return v;
    }
    let mut out = String::with_capacity(v.len());
    let chars: Vec<char> = v.chars().collect();
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        let next_is_start = chars
            .get(i + 1)
            .is_some_and(|n| n.is_ascii_alphabetic() || *n == '_');
        let escaped = i > 0 && (chars[i - 1] == '\\' || chars[i - 1] == '$');
        if c == '$' && next_is_start && !escaped {
            let mut j = i + 1;
            while j < chars.len() && (chars[j].is_ascii_alphanumeric() || chars[j] == '_') {
                j += 1;
            }
            let name: String = chars[i + 1..j].iter().collect();
            out.push_str(&reference(&name));
            i = j;
            continue;
        }
        out.push(c);
        i += 1;
    }
    out
}

/// Convert opencode's `{env:VAR}` references into canonical `${VAR}`.
/// `${env:VAR}` is left for [`normalize_refs`].
pub fn normalize_brace_refs(value: &str) -> String {
    ENV_BRACE_RE
        .replace_all(value, |c: &Captures| {
            if c[1].is_empty() {
                reference(&c[2])
            } else {
                c[0].to_string()
            }
        })
        .into_owned()
}

/// How references are written for a target.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RefStyle {
    /// `${VAR}` and `${VAR:-default}` kept as-is (Claude Code).
    Dollar,
    /// `${VAR}` without default support; defaults are resolved now (Gemini CLI).
    DollarPlain,
    /// `${env:VAR}`; defaults are resolved now (Cursor, VS Code).
    EnvColon,
    /// `{env:VAR}`; defaults are resolved now (opencode).
    EnvBrace,
    /// Substitute values from the environment now.
    Resolve,
}

/// Collects what happened while rendering references.
#[derive(Debug, Default, Clone)]
pub struct RenderState {
    /// Var names that were needed but not set.
    pub missing: BTreeSet<String>,
    /// Secret values that were written to disk literally; masked in any displayed output.
    pub resolved: BTreeSet<String>,
    /// Var names left as `${VAR}` placeholders because resolving was off (`--secrets never`).
    pub kept: BTreeSet<String>,
    /// Var names that were unset, so their `${VAR:-fallback}` was written instead.
    pub fallbacks: BTreeSet<String>,
}

/// Render `value` in `style`, recording missing / resolved values into `state`.
pub fn render_refs(
    value: &str,
    style: RefStyle,
    env: &HashMap<String, String>,
    state: &mut RenderState,
) -> String {
    if style == RefStyle::Dollar {
        return value.to_string();
    }
    REF_RE
        .replace_all(value, |c: &Captures| {
            let name = &c[1];
            let fallback = c.get(2).map(|m| m.as_str());
            let set = env.get(name);
            // A fallback written to disk is as sensitive as a resolved value
            // when it looks like a secret.
            let mut use_fallback = |fb: &str| {
                state.fallbacks.insert(name.to_string());
                if classify(Some(name), fb).is_some() {
                    state.resolved.insert(fb.to_string());
                }
                fb.to_string()
            };
            match style {
                RefStyle::EnvColon | RefStyle::EnvBrace | RefStyle::DollarPlain => {
                    match (fallback, set) {
                        (Some(fb), None) => use_fallback(fb),
                        _ if style == RefStyle::EnvColon => format!("${{env:{name}}}"),
                        _ if style == RefStyle::EnvBrace => format!("{{env:{name}}}"),
                        _ => reference(name),
                    }
                }
                _ => match (set, fallback) {
                    (Some(v), _) => {
                        if v.len() >= 4 {
                            state.resolved.insert(v.clone());
                        }
                        v.clone()
                    }
                    (None, Some(fb)) => use_fallback(fb),
                    (None, None) => {
                        state.missing.insert(name.to_string());
                        c[0].to_string()
                    }
                },
            }
        })
        .into_owned()
}

// ---------------------------------------------------------------------------
// Detection

const SECRET_WORDS: &[&str] = &[
    "token",
    "secret",
    "password",
    "passwd",
    "pwd",
    "pass",
    "apikey",
    "pat",
    "authorization",
    "credential",
    "credentials",
    "cookie",
    "bearer",
    "privatekey",
    "signature",
    "dsn",
    "passphrase",
    "auth",
];

/// Words that are secret-sounding even as the end of a longer word
/// (`PGPASSWORD`, `APITOKEN`, `CLIENTSECRET`).
const SECRET_SUFFIXES: &[&str] = &["password", "passwd", "passphrase", "token", "secret"];

const QUALIFIERS: &[&str] = &[
    "ttl",
    "timeout",
    "expiry",
    "expires",
    "expiration",
    "length",
    "len",
    "size",
    "count",
    "limit",
    "max",
    "min",
    "port",
    "interval",
    "seconds",
    "secs",
    "ms",
    "url",
    "uri",
    "endpoint",
    "host",
    "file",
    "path",
    "dir",
    "name",
    "type",
    "header",
    "prefix",
    "id",
    "mode",
    "method",
    "provider",
    "scheme",
    "strategy",
    "enabled",
    "required",
    "version",
    "domain",
    "region",
    "scope",
    "scopes",
    "user",
    "username",
    "issuer",
    "audience",
    "realm",
    "algorithm",
    "level",
    "env",
    "var",
];

static CAMEL_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"([a-z0-9])([A-Z])").expect("valid regex"));
static JOINED_SECRET_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"apikey|accesskey|secretkey|privatekey|authtoken|accesstoken|refreshtoken|clientsecret|connectionstring|connstring|connstr|signingkey")
        .expect("valid regex")
});

fn words(name: &str) -> Vec<String> {
    CAMEL_RE
        .replace_all(name, "${1}_${2}")
        .to_lowercase()
        .split(|c: char| !c.is_ascii_alphanumeric())
        .filter(|w| !w.is_empty())
        .map(str::to_string)
        .collect()
}

/// Does this env var / header / flag name suggest a secret?
pub fn is_secret_name(name: &str) -> bool {
    let w = words(name);
    if w.is_empty() {
        return false;
    }
    if JOINED_SECRET_RE.is_match(&w.concat()) {
        return true;
    }
    // `KEY` counts as the last word (API_KEY, STRIPE_KEY) but not as a prefix
    // (KEY_PREFIX, KEY_ID) or after PUBLIC.
    let last = w.len() - 1;
    // VAULT_SECRET_ID, CLIENT_SECRET_ID: the id half of a credential pair.
    if last > 0 && w[last] == "id" && w[last - 1] == "secret" {
        return true;
    }
    // TOKEN_TTL, PASSWORD_MIN_LENGTH, AUTH_PORT: settings *about* a secret.
    if QUALIFIERS.contains(&w[last].as_str()) {
        return false;
    }
    w.iter().enumerate().any(|(i, word)| {
        SECRET_WORDS.contains(&word.as_str())
            || SECRET_SUFFIXES.iter().any(|s| word.ends_with(s))
            || (word == "key" && i == last && (i == 0 || w[i - 1] != "public"))
    })
}

/// Does this HTTP header name suggest a credential? Custom headers are named
/// freely (`X-Auth`, `X-Session`, `X-Goog-Api-Key`), so any mention counts.
pub fn is_secret_header_name(name: &str) -> bool {
    let n = name.to_ascii_lowercase();
    is_secret_name(name)
        || ["auth", "key", "token", "secret", "session", "cookie"]
            .iter()
            .any(|w| n.contains(w))
}

/// The name to classify a header value under: any secret-sounding header
/// counts as a secret name.
fn header_key(name: &str) -> &str {
    if is_secret_header_name(name) {
        "secret"
    } else {
        name
    }
}

struct Pattern {
    kind: &'static str,
    re: Regex,
}

/// High-confidence token formats. Used everywhere, including free text.
static STRONG: LazyLock<Vec<Pattern>> =
    LazyLock::new(|| {
        [
        (
            "private-key",
            r"-----BEGIN [A-Z ]*PRIVATE KEY-----(?s:.*?)(?:-----END [A-Z ]*PRIVATE KEY-----|\z)",
        ),
        // Also covers admin keys (`sk-ant-admin01-...`).
        ("anthropic-key", r"\bsk-ant-[A-Za-z0-9_-]{20,}"),
        // Before openai-key, which would also match it.
        ("openrouter-key", r"\bsk-or-v1-[a-f0-9]{64}"),
        ("openai-key", r"\bsk-(?:proj-|svcacct-|admin-)?[A-Za-z0-9_-]{20,}"),
        ("xai-key", r"\bxai-[A-Za-z0-9]{40,}"),
        ("groq-key", r"\bgsk_[A-Za-z0-9]{40,}"),
        ("perplexity-key", r"\bpplx-[A-Za-z0-9]{40,}"),
        ("tavily-key", r"\btvly-[A-Za-z0-9_-]{20,}"),
        ("supabase-token", r"\bsbp_[a-f0-9]{40}\b"),
        ("replicate-token", r"\br8_[A-Za-z0-9]{30,}"),
        ("firecrawl-key", r"\bfc-[a-f0-9]{32}\b"),
        ("stripe-key", r"\b(?:sk|rk)_(?:live|test)_[A-Za-z0-9]{16,}"),
        (
            "github-token",
            r"\b(?:gh[pousr]_[A-Za-z0-9]{30,}|github_pat_[A-Za-z0-9_]{40,})",
        ),
        ("gitlab-token", r"\bglpat-[A-Za-z0-9_-]{20,}"),
        ("slack-token", r"\bxox[abposr]-[A-Za-z0-9-]{10,}"),
        ("aws-access-key", r"\b(?:AKIA|ASIA)[0-9A-Z]{16}\b"),
        ("google-api-key", r"\bAIza[0-9A-Za-z_-]{35}"),
        ("npm-token", r"\bnpm_[A-Za-z0-9]{36}"),
        ("pypi-token", r"\bpypi-[A-Za-z0-9_-]{50,}"),
        ("huggingface-token", r"\bhf_[A-Za-z0-9]{30,}"),
        ("linear-key", r"\blin_api_[A-Za-z0-9]{30,}"),
        ("notion-token", r"\b(?:ntn_|secret_)[A-Za-z0-9]{40,}"),
        ("sentry-token", r"\bsntry[su]?_[A-Za-z0-9_+/=-]{30,}"),
        ("google-oauth-secret", r"\bGOCSPX-[A-Za-z0-9_-]{20,}"),
        ("google-refresh-token", r"\b1//0[A-Za-z0-9_-]{30,}"),
        ("digitalocean-token", r"\bdo[opr]_v1_[a-f0-9]{64}"),
        ("sendgrid-key", r"\bSG\.[A-Za-z0-9_-]{16,}\.[A-Za-z0-9_-]{16,}"),
        ("shopify-token", r"\bshp(?:at|ca|pa|ss)_[a-fA-F0-9]{32}"),
        (
            "jwt",
            r"\beyJ[A-Za-z0-9_-]{8,}\.eyJ[A-Za-z0-9_-]{8,}\.[A-Za-z0-9_-]{8,}",
        ),
        (
            "url-credentials",
            r"(?i)\b[a-z][a-z0-9+.-]*://[^\s:/@]*:[^\s@/]{3,}@\S+",
        ),
    ]
    .into_iter()
    .map(|(kind, re)| Pattern {
        kind,
        re: Regex::new(re).expect("valid regex"),
    })
    .collect()
    });

/// The kind of credential `value` contains, if it matches a well-known format.
pub fn strong_secret_kind(value: &str) -> Option<&'static str> {
    STRONG
        .iter()
        .find(|p| {
            p.re.find_iter(value)
                .any(|m| literal_match(&p.re, m.as_str()))
        })
        .map(|p| p.kind)
}

/// A pattern match is a secret unless it only matched thanks to `${...}`
/// references in it (`postgres://u:${PW}@db`).
fn literal_match(re: &Regex, m: &str) -> bool {
    !has_ref(m) || re.is_match(&strip_refs(m))
}

fn shannon(s: &str) -> f64 {
    let mut freq: HashMap<char, usize> = HashMap::new();
    let mut n = 0usize;
    for ch in s.chars() {
        *freq.entry(ch).or_default() += 1;
        n += 1;
    }
    let len = n as f64;
    freq.values()
        .map(|&c| {
            let p = c as f64 / len;
            -p * p.log2()
        })
        .sum()
}

static URL_LIKE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)^[a-z][a-z0-9+.-]*://").expect("valid regex"));
static PATH_LIKE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^(?:[~.]?[\\/]|[A-Za-z]:[\\/])").expect("valid regex"));
static PKG_LIKE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)^@?[a-z0-9-]+/[a-z0-9._-]+(?:@[\w.^~-]+)?$").expect("valid regex")
});
static HEX40: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)^[0-9a-f]{40}$").expect("valid regex"));
static UUID: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)^[0-9a-f-]{36}$").expect("valid regex"));
static SRI_HASH: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^sha(?:1|256|384|512)-[A-Za-z0-9+/=]+$").expect("valid regex"));
static BEARER: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)^(Bearer|Basic|Token)\s+(.+)$").expect("valid regex"));

/// Heuristic for opaque random-looking tokens (structured values only, never prose).
pub fn looks_random_token(value: &str) -> bool {
    let v = BEARER
        .captures(value)
        .map_or(value, |c| c.get(2).map_or(value, |m| m.as_str()));
    let len = v.chars().count();
    if !(20..=4096).contains(&len) || v.chars().any(char::is_whitespace) {
        return false;
    }
    if URL_LIKE.is_match(v)
        || PATH_LIKE.is_match(v)
        || SRI_HASH.is_match(v)
        || PKG_LIKE.is_match(v)
        || HEX40.is_match(v)
        || UUID.is_match(v)
    {
        return false;
    }
    let classes = [
        v.chars().any(|c| c.is_ascii_lowercase()),
        v.chars().any(|c| c.is_ascii_uppercase()),
        v.chars().any(|c| c.is_ascii_digit()),
    ]
    .iter()
    .filter(|b| **b)
    .count();
    classes >= 2 && shannon(v) >= 3.5
}

/// Numbers, booleans, plain URLs and paths under a secret-sounding name
/// (`SESSION_TIMEOUT=3600`, `AUTH_URL=https://...`) are configuration, not secrets.
fn obviously_not_secret(value: &str) -> bool {
    let v = value.trim();
    v.is_empty()
        || matches!(
            v.to_ascii_lowercase().as_str(),
            "true" | "false" | "yes" | "no" | "on" | "off" | "none" | "null"
        )
        // Ports, counts, durations.
        || (v.len() <= 5 && v.chars().all(|c| c.is_ascii_digit()))
        || (URL_LIKE.is_match(v) && strong_secret_kind(v).is_none() && !URL_QUERY.is_match(v))
        || clearly_a_path(v)
}

/// `/run/secrets/db`, `~/.ssh/id_rsa`, `C:\keys\a.pem`: more than one segment or
/// a file extension. A lone `/q8Zr2LkP0...` is a slash-prefixed token, not a path.
fn clearly_a_path(v: &str) -> bool {
    if !PATH_LIKE.is_match(v) || v.contains(char::is_whitespace) {
        return false;
    }
    let rest = v.trim_start_matches(['~', '.', '/', '\\']);
    let rest = rest
        .strip_prefix(|c: char| c.is_ascii_alphabetic())
        .and_then(|r| r.strip_prefix(':'))
        .map_or(rest, |r| r.trim_start_matches(['/', '\\']));
    rest.contains(['/', '\\'])
        || rest
            .rsplit_once('.')
            .is_some_and(|(_, ext)| (1..=5).contains(&ext.len()))
        || !looks_random_token(rest)
}

/// Why `value` (stored under `name`) should be treated as a secret, if it should.
///
/// Only `${...}` spans are exempt: the literal parts of a value that also holds
/// a reference (`hunter2-${SUFFIX}`, `Bearer tok ${X}`) are still checked.
pub fn classify(name: Option<&str>, value: &str) -> Option<&'static str> {
    if value.is_empty() || sole_ref(value).is_some() {
        return None;
    }
    if let Some(k) = strong_secret_kind(value) {
        return Some(k);
    }
    let mixed = has_ref(value);
    let literal = strip_refs(value);
    let enough = |l: &str| !mixed || substantial_literal(l);
    // `Bearer <token>` / `Basic <creds>` is a credential whatever it is called.
    if let Some(c) = BEARER.captures(value) {
        let token = strip_refs(&c[2]);
        if token.trim().len() >= 6 && enough(&token) {
            return Some("authorization-header");
        }
        if mixed {
            return None;
        }
    }
    // Under a secret-sounding name any value counts, however short (`rootE`).
    if name.is_some_and(is_secret_name) && enough(&literal) && !obviously_not_secret(&literal) {
        return Some("secret-named-field");
    }
    if looks_random_token(&literal) {
        return Some("high-entropy-value");
    }
    None
}

/// `${VAR:-fallback}` references in `value` whose literal fallback looks like
/// a secret, as (VAR, reason). A fallback is written to disk wherever the
/// variable is unset, so it must not be a credential.
pub fn secret_fallbacks(value: &str) -> Vec<(String, &'static str)> {
    REF_RE
        .captures_iter(value)
        .filter_map(|c| {
            let fallback = c.get(2)?.as_str();
            classify(Some(&c[1]), fallback).map(|reason| (c[1].to_string(), reason))
        })
        .collect()
}

// ---------------------------------------------------------------------------
// Redaction

/// One value that was replaced by an env reference during capture.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Redaction {
    /// Human-readable location, e.g. `mcp_servers.github.env.GITHUB_TOKEN`.
    pub location: String,
    /// Env var the value must now be supplied through.
    pub env_var: String,
    pub reason: &'static str,
    /// SHA-256 of the redacted value (set during capture), so identical secrets
    /// can share one variable while different ones never do.
    pub fingerprint: Option<String>,
    /// The MCP server the value came from. Server names may contain dots
    /// (`a.b`), so the server can't be read back from `location` reliably.
    pub server: Option<String>,
}

/// `ENV_VAR_NAME` built from parts (`"github", "apiKey"` -> `GITHUB_API_KEY`).
pub fn env_var_name(parts: &[&str]) -> String {
    let joined = parts.join("_");
    let snake = CAMEL_RE.replace_all(&joined, "${1}_${2}");
    let mut out = String::new();
    for ch in snake.chars() {
        if ch.is_ascii_alphanumeric() {
            out.push(ch.to_ascii_uppercase());
        } else if !out.ends_with('_') {
            out.push('_');
        }
    }
    let out = out.trim_matches('_').to_string();
    if out.starts_with(|c: char| c.is_ascii_digit()) || out.is_empty() {
        format!("_{out}")
    } else {
        out
    }
}

static FLAG_EQ: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^(--?[A-Za-z0-9][A-Za-z0-9_-]*)=(.+)$").expect("valid regex"));
static FLAG: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^--?([A-Za-z0-9][A-Za-z0-9_-]*)$").expect("valid regex"));
static ASSIGN: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^([A-Za-z_][A-Za-z0-9_]*)=(.+)$").expect("valid regex"));
static HEADER_ARG: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^([A-Za-z][A-Za-z0-9-]*)(:\s*)([^\s/].*)$").expect("valid regex")
});
static URL_USERINFO: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)^([a-z][a-z0-9+.-]*://)([^\s:/@]*):([^\s@/]+)@").expect("valid regex")
});
static URL_QUERY: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"([?&])([^=&#\s]+)=([^&#\s]*)").expect("valid regex"));

/// SHA-256 of a redacted secret, so identical secrets can share one variable
/// while different ones never do.
pub fn fingerprint(secret: &str) -> String {
    crate::txn::sha256(Some(secret.as_bytes())).unwrap_or_default()
}

/// The redactions of one MCP server, and the env var names given out so far.
/// The same secret keeps its name; a different secret that would get a name
/// already taken gets a numbered one (`API_KEY_2`), so two secrets never end
/// up sharing a variable.
#[derive(Default)]
struct Found {
    redactions: Vec<Redaction>,
    /// Env var name -> fingerprint of the secret it stands for.
    names: HashMap<String, String>,
}

impl Found {
    /// Record that the secret at `location` is replaced by a reference to a
    /// variable named like `want`; returns the variable's name.
    fn add(&mut self, location: &str, want: String, reason: &'static str, secret: &str) -> String {
        let fp = fingerprint(secret);
        let mut name = want.clone();
        let mut n = 1;
        while let Some(taken) = self.names.get(&name) {
            if *taken == fp {
                break;
            }
            n += 1;
            name = format!("{want}_{n}");
        }
        self.names.entry(name.clone()).or_insert_with(|| fp.clone());
        self.redactions.push(Redaction {
            location: location.to_string(),
            env_var: name.clone(),
            reason,
            fingerprint: Some(fp),
            server: None,
        });
        name
    }
}

/// Replace secret-looking values in an MCP server definition with env references.
pub fn redact_server(server_name: &str, input: &McpServer) -> (McpServer, Vec<Redaction>) {
    let mut s = input.clone();
    let mut found = Found::default();
    let loc = format!("mcp_servers.{server_name}");

    // `${API_KEY:-literal}`: the literal is written wherever API_KEY is unset.
    // The variable keeps its name: it is the user's own.
    let locations: Vec<String> = s.strings().into_iter().map(|(l, _)| l).collect();
    for l in locations {
        s.map_string_at(&l, |v| {
            REF_RE
                .replace_all(v, |c: &Captures| match c.get(2) {
                    Some(fb) if classify(Some(&c[1]), fb.as_str()).is_some() => {
                        let fp = fingerprint(fb.as_str());
                        found.names.entry(c[1].to_string()).or_insert(fp.clone());
                        found.redactions.push(Redaction {
                            location: format!("{loc}.{l}"),
                            env_var: c[1].to_string(),
                            reason: "secret-fallback",
                            fingerprint: Some(fp),
                            server: None,
                        });
                        reference(&c[1])
                    }
                    _ => c[0].to_string(),
                })
                .into_owned()
        });
    }

    for (k, v) in s.env.iter_mut() {
        let at = format!("{loc}.env.{k}");
        if let Some(reason) = classify(Some(k), v) {
            let env_var = found.add(&at, env_var_name(&[k]), reason, v);
            *v = reference(&env_var);
        } else if URL_LIKE.is_match(v) {
            // `API_BASE = "https://x/v1?token=..."`: keep the URL, lose the token.
            *v = redact_url_into(server_name, v, &at, &mut found);
        }
    }

    for (k, v) in s.headers.iter_mut() {
        let (scheme, raw) = match BEARER.captures(v) {
            Some(c) => (Some(c[1].to_string()), c[2].to_string()),
            None => (None, v.clone()),
        };
        if sole_ref(&raw).is_some() {
            continue;
        }
        let reason = classify(Some(header_key(k)), &raw).or_else(|| {
            (scheme.is_some() && !strip_refs(&raw).trim().is_empty())
                .then_some("authorization-header")
        });
        if let Some(reason) = reason {
            let part = if k.eq_ignore_ascii_case("authorization") {
                "token"
            } else {
                k.as_str()
            };
            let env_var = found.add(
                &format!("{loc}.headers.{k}"),
                env_var_name(&[server_name, part]),
                reason,
                &raw,
            );
            *v = match scheme {
                Some(sc) => format!("{sc} {}", reference(&env_var)),
                None => reference(&env_var),
            };
        }
    }

    let mut n = 0;
    for i in 0..s.args.len() {
        let a = s.args[i].clone();
        let at = format!("{loc}.args[{i}]");
        if let Some(c) = FLAG_EQ.captures(&a) {
            // `--header=Authorization: Bearer ...`
            match redact_header_arg(server_name, &c[2], &at, &mut found) {
                HeaderArg::Redacted(new) => {
                    s.args[i] = format!("{}={new}", &c[1]);
                    continue;
                }
                HeaderArg::Referenced => continue,
                HeaderArg::NotHeader => {}
            }
            let flag = c[1].trim_start_matches('-').to_string();
            if URL_LIKE.is_match(&c[2]) && !is_secret_name(&flag) {
                // `--endpoint=https://host/sse?api_key=...`
                let value = redact_url_into(server_name, &c[2], &at, &mut found);
                s.args[i] = format!("{}={value}", &c[1]);
                continue;
            }
            if let Some(reason) = classify(Some(&flag), &c[2]) {
                let env_var = found.add(&at, env_var_name(&[server_name, &flag]), reason, &c[2]);
                s.args[i] = format!("{}={}", &c[1], reference(&env_var));
            }
            continue;
        }
        if a.starts_with('-') {
            continue;
        }
        // `NAME=value` (docker -e, env-style wrappers).
        if let Some(c) = ASSIGN.captures(&a) {
            if let Some(reason) = classify(Some(&c[1]), &c[2]) {
                let env_var = found.add(&at, env_var_name(&[&c[1]]), reason, &c[2]);
                s.args[i] = format!("{}={}", &c[1], reference(&env_var));
            } else if URL_LIKE.is_match(&c[2]) {
                let value = redact_url_into(server_name, &c[2], &at, &mut found);
                s.args[i] = format!("{}={value}", &c[1]);
            }
            continue;
        }
        // `Header-Name: value` (mcp-remote --header, curl-style wrappers).
        if !URL_LIKE.is_match(&a) {
            match redact_header_arg(server_name, &a, &at, &mut found) {
                HeaderArg::Redacted(new) => {
                    s.args[i] = new;
                    continue;
                }
                // `X-Api-Key:${KEY}` is already safe; the entropy check below
                // would misread the name and reference as one opaque token.
                HeaderArg::Referenced => continue,
                HeaderArg::NotHeader => {}
            }
        }
        // URLs: credentials in userinfo or query parameters.
        if URL_LIKE.is_match(&a) {
            let before = found.redactions.len();
            let value = redact_url_into(server_name, &a, &at, &mut found);
            if found.redactions.len() > before {
                s.args[i] = value;
                continue;
            }
        }
        let prev_flag = (i > 0)
            .then(|| FLAG.captures(&s.args[i - 1]).map(|c| c[1].to_string()))
            .flatten();
        if let Some(reason) = classify(prev_flag.as_deref(), &a) {
            let want = match &prev_flag {
                Some(f) => env_var_name(&[server_name, f]),
                None => {
                    n += 1;
                    env_var_name(&[server_name, &format!("secret_{n}")])
                }
            };
            let env_var = found.add(&at, want, reason, &a);
            s.args[i] = reference(&env_var);
        }
    }

    if let Some(url) = s.url.clone() {
        let before = found.redactions.len();
        let value = redact_url_into(server_name, &url, &format!("{loc}.url"), &mut found);
        if found.redactions.len() > before {
            s.url = Some(value);
        }
    }

    if let Some(cmd) = s.command.clone()
        && let Some(kind) = strong_secret_kind(&cmd)
    {
        let p = STRONG.iter().find(|p| p.kind == kind).expect("kind exists");
        let at = format!("{loc}.command");
        let want = env_var_name(&[server_name, "command_secret"]);
        let replaced =
            p.re.replace_all(&cmd, |c: &Captures| {
                reference(&found.add(&at, want.clone(), kind, &c[0]))
            })
            .into_owned();
        s.command = Some(replaced);
    }

    for r in &mut found.redactions {
        r.server = Some(server_name.to_string());
    }
    (s, found.redactions)
}

/// What [`redact_header_arg`] made of an argument.
enum HeaderArg {
    /// Not `Header-Name: value`, or a header whose value isn't a secret.
    NotHeader,
    /// A header whose value already uses an env reference.
    Referenced,
    /// `Name: ${VAR}`, recorded in the [`Found`] passed in.
    Redacted(String),
}

/// `Name: value` with a secret value -> `Name: ${VAR}`.
fn redact_header_arg(server_name: &str, arg: &str, at: &str, found: &mut Found) -> HeaderArg {
    let Some(c) = HEADER_ARG.captures(arg) else {
        return HeaderArg::NotHeader;
    };
    let (name, sep, value) = (c[1].to_string(), c[2].to_string(), c[3].trim().to_string());
    // Only HTTP-header-shaped names (X-Api-Key, Authorization, Cookie) or secret
    // names; `user:secret` and similar pairs are left to the flag-based checks.
    let header_like = name.contains('-')
        || ["authorization", "cookie"].contains(&name.to_ascii_lowercase().as_str())
        || is_secret_header_name(&name);
    if !header_like {
        return HeaderArg::NotHeader;
    }
    let (scheme, raw) = match BEARER.captures(&value) {
        Some(b) => (Some(b[1].to_string()), b[2].to_string()),
        None => (None, value.clone()),
    };
    if has_ref(&raw) && !substantial_literal(&strip_refs(&raw)) {
        return HeaderArg::Referenced;
    }
    let Some(reason) =
        classify(Some(header_key(&name)), &raw).or(scheme.as_ref().map(|_| "authorization-header"))
    else {
        return HeaderArg::NotHeader;
    };
    let part = if name.eq_ignore_ascii_case("authorization") {
        "token"
    } else {
        name.as_str()
    };
    let env_var = found.add(at, env_var_name(&[server_name, part]), reason, &raw);
    let v = match scheme {
        Some(sc) => format!("{sc} {}", reference(&env_var)),
        None => reference(&env_var),
    };
    // Keep the original separator: mcp-remote documents `Name:value` without a space.
    HeaderArg::Redacted(format!("{name}{sep}{v}"))
}

fn percent_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%'
            && let Some(h) = s.get(i + 1..i + 3)
            && let Ok(b) = u8::from_str_radix(h, 16)
        {
            out.push(b);
            i += 3;
            continue;
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// `https://<token>@host`: a username that is itself the credential. Only for
/// HTTP(S) and WebSocket URLs, and only when it looks like a token, so
/// `ssh://git@host` and `https://bob@host` stay.
static URL_TOKEN_USER: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)^((?:https?|wss?)://)([^\s:/@]+)@").expect("valid regex"));

fn token_username(user: &str) -> bool {
    !has_ref(user)
        && !user.contains('.')
        && (strong_secret_kind(user).is_some()
            || looks_random_token(user)
            || user.chars().count() >= 12)
}

/// Indexes of the path segments of `url` (split on `/`, after the authority)
/// that carry a credential: known secret-path shapes (Slack and Discord
/// webhooks, Zapier-style `/mcp/s/<token>/`), known token formats and, if
/// `entropy`, random-looking segments.
fn secret_path_segments(host: &str, segs: &[&str], entropy: bool) -> Vec<usize> {
    let host = host.rsplit('@').next().unwrap_or(host).to_ascii_lowercase();
    let mut out = Vec::new();
    for (i, seg) in segs.iter().enumerate() {
        if seg.is_empty() || has_ref(seg) || has_redaction_marker(seg) {
            continue;
        }
        let prev = |n: usize| i.checked_sub(n).map(|j| segs[j]);
        let known = (host == "hooks.slack.com" && segs.first() == Some(&"services") && i > 0)
            || ((host.ends_with("discord.com") || host.ends_with("discordapp.com"))
                && prev(3) == Some("api")
                && prev(2) == Some("webhooks"))
            || (prev(1) == Some("s") && prev(2) == Some("mcp"));
        // Paths hold many ids and slugs (`gpt-4o-mini-2024-07-18`), so a
        // segment must also mix upper case, lower case and digits.
        let random = entropy
            && looks_random_token(seg)
            && seg.chars().any(|c| c.is_ascii_uppercase())
            && seg.chars().any(|c| c.is_ascii_lowercase())
            && seg.chars().any(|c| c.is_ascii_digit());
        let hex = entropy && hex_token(seg) && !names_a_commit(&host, prev(1), seg);
        if known || strong_secret_kind(seg).is_some() || random || hex {
            out.push(i);
        }
    }
    out
}

/// A hex-only token (`/mcp/3f9a...`, a classic 40-digit GitHub token), which
/// the mixed-case test above can't catch. 32 hex digits is 128 bits, the
/// usual size of a random API token, while numeric ids, short git SHAs (7 to
/// 12 digits) and most other hex ids in paths are shorter. A long run of
/// digits alone is a numeric id, so a letter and a digit are both required.
fn hex_token(seg: &str) -> bool {
    seg.len() >= 32
        && seg.chars().all(|c| c.is_ascii_hexdigit())
        && seg.chars().any(|c| c.is_ascii_digit())
        && seg.chars().any(|c| c.is_ascii_alphabetic())
}

/// Full commit SHAs (40 hex digits, 64 for SHA-256 repositories) appear in
/// paths where they name a commit, not a credential: after `commit`, `blob`,
/// `tree`, `raw` and the like, and on hosts that serve files by commit
/// (`raw.githubusercontent.com/<owner>/<repo>/<sha>/<file>`). Any other
/// length can't be a full SHA, so it is redacted wherever it appears.
fn names_a_commit(host: &str, prev: Option<&str>, seg: &str) -> bool {
    const WORDS: &[&str] = &[
        "commit", "commits", "tree", "blob", "raw", "blame", "src", "archive", "compare", "rev",
        "revision", "sha", "ref", "refs",
    ];
    const HOSTS: &[&str] = &[
        "raw.githubusercontent.com",
        "gist.githubusercontent.com",
        "codeload.github.com",
    ];
    if seg.len() != 40 && seg.len() != 64 {
        return false;
    }
    prev.is_some_and(|p| WORDS.contains(&p.to_ascii_lowercase().as_str()))
        || HOSTS.contains(&host.split(':').next().unwrap_or(host))
}

/// Replace the secret path segments `hits` of `segs` with `with(n, secret)`.
/// A Slack webhook's `T…/B…/<secret>` tail is one credential, so it becomes
/// one value.
fn replace_segments(
    host: &str,
    segs: &mut Vec<String>,
    hits: &[usize],
    mut with: impl FnMut(usize, &str) -> String,
) {
    let slack = host.eq_ignore_ascii_case("hooks.slack.com") && hits.len() > 1;
    if slack && hits.iter().enumerate().all(|(n, i)| *i == n + 1) {
        let tail = segs[1..].join("/");
        segs.truncate(1);
        segs.push(with(0, &tail));
        return;
    }
    for (n, i) in hits.iter().enumerate() {
        segs[*i] = with(n, &segs[*i].clone());
    }
}

/// Split a URL into `scheme://authority`, the path segments after it, and the
/// rest (`?query#fragment`).
fn split_url(url: &str) -> Option<(&str, &str, &str)> {
    let scheme_end = url.find("://")? + 3;
    let after = &url[scheme_end..];
    let auth_len = after.find(['/', '?', '#']).unwrap_or(after.len());
    let path_start = scheme_end + auth_len;
    let path_len = url[path_start..]
        .find(['?', '#'])
        .unwrap_or(url.len() - path_start);
    Some((
        &url[..path_start],
        &url[path_start..path_start + path_len],
        &url[path_start + path_len..],
    ))
}

/// `url` with its credentials replaced by env references, as (new URL,
/// [(variable, reason)]). Used on its own in tests.
#[cfg(test)]
fn redact_url(server_name: &str, url: &str) -> (String, Vec<(String, &'static str)>) {
    let mut found = Found::default();
    let value = redact_url_into(server_name, url, "url", &mut found);
    let list = found
        .redactions
        .into_iter()
        .map(|r| (r.env_var, r.reason))
        .collect();
    (value, list)
}

/// `url` with its credentials replaced by env references, each recorded in
/// `found` at location `at`.
fn redact_url_into(server_name: &str, url: &str, at: &str, found: &mut Found) -> String {
    let value = URL_USERINFO
        .replace(url, |c: &Captures| {
            if strip_refs(&c[3]).is_empty() {
                return c[0].to_string();
            }
            let env_var = found.add(
                at,
                env_var_name(&[server_name, "password"]),
                "url-credentials",
                &c[3],
            );
            format!("{}{}:{}@", &c[1], &c[2], reference(&env_var))
        })
        .into_owned();
    let value = URL_TOKEN_USER
        .replace(&value, |c: &Captures| {
            if !token_username(&c[2]) {
                return c[0].to_string();
            }
            let env_var = found.add(
                at,
                env_var_name(&[server_name, "url_user_token"]),
                "url-username-token",
                &c[2],
            );
            format!("{}{}@", &c[1], reference(&env_var))
        })
        .into_owned();
    let value = match split_url(&value) {
        Some((head, path, rest)) => {
            let host = head.split("://").nth(1).unwrap_or("");
            let mut segs: Vec<String> = path.split('/').skip(1).map(str::to_string).collect();
            let view: Vec<&str> = segs.iter().map(String::as_str).collect();
            let hits = secret_path_segments(host, &view, true);
            replace_segments(host, &mut segs, &hits, |n, secret| {
                let suffix = if n == 0 {
                    String::new()
                } else {
                    format!("_{}", n + 1)
                };
                let want = env_var_name(&[server_name, &format!("url_token{suffix}")]);
                reference(&found.add(at, want, "url-path-token", secret))
            });
            if hits.is_empty() {
                value.clone()
            } else {
                let path: String = segs.iter().map(|s| format!("/{s}")).collect();
                format!("{head}{path}{rest}")
            }
        }
        None => value,
    };
    URL_QUERY
        .replace_all(&value, |c: &Captures| {
            let decoded = percent_decode(&c[3]);
            match classify(Some(&c[2]), &decoded) {
                Some(reason) => {
                    let env_var =
                        found.add(at, env_var_name(&[server_name, &c[2]]), reason, &decoded);
                    format!("{}{}={}", &c[1], &c[2], reference(&env_var))
                }
                None => c[0].to_string(),
            }
        })
        .into_owned()
}

static TEXT_URL: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r#"(?i)\b(?:https?|wss?)://[^\s"'`<>()\[\]{}]+"#).expect("valid regex")
});

/// `YOUR_KEY`, `<token>`, `xxx`: documentation placeholders, not secrets.
fn placeholder_value(v: &str) -> bool {
    placeholder_password(v)
        || v.to_ascii_lowercase().contains("your")
        || v.chars().all(|c| c.is_ascii_uppercase() || c == '_')
}

/// In free text, URLs lose secret-named query values and secret path segments
/// (known shapes only: prose has too many ids for an entropy guess).
fn redact_text_urls(text: &str, marker: &str) -> (String, bool) {
    let mut hit = false;
    let out = TEXT_URL
        .replace_all(text, |c: &Captures| {
            // Sentence punctuation after a URL isn't part of it.
            let url = c[0].trim_end_matches(['.', ',', ';', ':', '!', '?']);
            let tail = &c[0][url.len()..];
            let Some((head, path, rest)) = split_url(url) else {
                return c[0].to_string();
            };
            let host = head.split("://").nth(1).unwrap_or("");
            let mut segs: Vec<String> = path.split('/').skip(1).map(str::to_string).collect();
            let view: Vec<&str> = segs.iter().map(String::as_str).collect();
            let hits = secret_path_segments(host, &view, false);
            hit |= !hits.is_empty();
            replace_segments(host, &mut segs, &hits, |_, _| marker.to_string());
            let path: String = segs.iter().map(|s| format!("/{s}")).collect();
            let rest = URL_QUERY.replace_all(rest, |q: &Captures| {
                let v = percent_decode(&q[3]);
                if is_secret_name(&q[2])
                    && !has_ref(&v)
                    && !placeholder_value(&v)
                    && !has_redaction_marker(&v)
                    && classify(Some(&q[2]), &v).is_some()
                {
                    hit = true;
                    format!("{}{}={marker}", &q[1], &q[2])
                } else {
                    q[0].to_string()
                }
            });
            format!("{head}{path}{rest}{tail}")
        })
        .into_owned();
    (out, hit)
}

/// What a URL password is replaced with in free text.
pub const REDACTED_PASSWORD: &str = "[REDACTED]";

/// Passwords in documentation examples (`postgres://user:pass@host/db`,
/// `https://u:<password>@x`, `redis://:${REDIS_PASSWORD}@cache`) are not secrets.
fn placeholder_password(pw: &str) -> bool {
    let stripped = strip_refs(pw.trim());
    let p = stripped.as_str();
    if p.is_empty() {
        return true;
    }
    let wrapped = |open: char, close: char| {
        p.chars().count() >= 2 && p.starts_with(open) && p.ends_with(close)
    };
    if p.starts_with('$')
        || has_ref(p)
        || wrapped('<', '>')
        || wrapped('{', '}')
        || wrapped('[', ']')
        || wrapped('%', '%')
    {
        return true;
    }
    if !p.is_empty()
        && p.chars()
            .all(|c| matches!(c, 'x' | 'X' | '*' | '.' | '•' | '#' | '_' | '-'))
    {
        return true;
    }
    let norm: String = p
        .chars()
        .filter(char::is_ascii_alphanumeric)
        .collect::<String>()
        .to_ascii_lowercase();
    matches!(
        norm.as_str(),
        "pass"
            | "password"
            | "passwd"
            | "pwd"
            | "secret"
            | "changeme"
            | "changeit"
            | "yourpassword"
            | "mypassword"
            | "userpassword"
            | "dbpassword"
            | "example"
            | "placeholder"
            | "redacted"
    )
}

/// The kind of credential in free text (instructions, rules, commands, skills,
/// frontmatter, permission rules): anything [`redact_text`] would replace.
pub fn text_secret_kind(text: &str) -> Option<&'static str> {
    redact_text(text).1.first().copied()
}

static TEXT_NAME: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"[A-Za-z_][A-Za-z0-9_-]*").expect("valid regex"));
static TEXT_BEARER: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)\b(Bearer|Basic)\s+([A-Za-z0-9._~+/=-]{8,})").expect("valid regex")
});

/// A value in free text that is a credential rather than prose, code or a
/// placeholder: a single token of at least 6 characters mixing letters and
/// digits (or random-looking), and not a reference, path, URL or `<example>`.
fn secret_text_value(v: &str) -> bool {
    let token = v
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || "._~+/=!@#$%^&*-".contains(c));
    token
        && v.chars().count() >= 6
        && !v.starts_with('$')
        && !has_ref(v)
        && !placeholder_password(v)
        && !obviously_not_secret(v)
        && ((v.chars().any(|c| c.is_ascii_digit()) && v.chars().any(|c| c.is_ascii_alphabetic()))
            || looks_random_token(v))
}

/// Byte ranges of values assigned to secret-sounding names in free text:
/// `NAME=value` (shell, `docker -e`), `Name: value` (headers, YAML),
/// `--flag=value` and `--flag value`.
fn assignment_spans(text: &str) -> Vec<(usize, usize)> {
    let mut out = Vec::new();
    for m in TEXT_NAME.find_iter(text) {
        if !is_secret_name(m.as_str()) {
            continue;
        }
        let flag = text[..m.start()].ends_with('-');
        let rest = &text[m.end()..];
        let after_space = rest.trim_start_matches([' ', '\t']);
        let spaced = after_space.len() < rest.len();
        let value_from = match after_space.chars().next() {
            Some(c @ ('=' | ':')) => after_space[c.len_utf8()..].trim_start_matches([' ', '\t']),
            Some(_) if flag && spaced => after_space,
            _ => continue,
        };
        let quoted = value_from.starts_with(['"', '\'']);
        let value = if quoted { &value_from[1..] } else { value_from };
        let start = text.len() - value.len();
        let len = value
            .find(|c: char| c.is_whitespace() || "\"'`<>,;()[]{}&".contains(c))
            .unwrap_or(value.len());
        let raw = &value[..len];
        let trimmed = raw.trim_end_matches(['.', ':', '!']);
        if secret_text_value(trimmed) {
            out.push((start, start + trimmed.len()));
        }
    }
    out
}

/// Redact high-confidence secrets from free text (instructions, rules, commands, skills).
/// Returns the new text and the kinds of secret found.
///
/// Tokens become `[REDACTED:<kind>]`. In URLs only the password is replaced
/// (`postgres://user:[REDACTED]@host/db`), so the surrounding Markdown survives.
pub fn redact_text(text: &str) -> (String, Vec<&'static str>) {
    redact_text_with(text, &|kind| match kind {
        Some(k) => format!("[REDACTED:{k}]"),
        None => REDACTED_PASSWORD.to_string(),
    })
}

/// [`redact_text`], with `mark(Some(kind))` replacing a known token format
/// and `mark(None)` any other secret.
fn redact_text_with(
    text: &str,
    mark: &dyn Fn(Option<&'static str>) -> String,
) -> (String, Vec<&'static str>) {
    let plain = mark(None);
    let mut kinds = Vec::new();
    let mut out = text.to_string();
    for p in STRONG.iter().filter(|p| p.kind != "url-credentials") {
        let mut hit = false;
        let replaced =
            p.re.replace_all(&out, |c: &Captures| {
                if literal_match(&p.re, &c[0]) {
                    hit = true;
                    mark(Some(p.kind))
                } else {
                    c[0].to_string()
                }
            })
            .into_owned();
        if hit {
            kinds.push(p.kind);
            out = replaced;
        }
    }
    // Last, so a token used as a password has already become a marker.
    let mut hit = false;
    let replaced = URL_PASSWORD
        .replace_all(&out, |c: &Captures| {
            if placeholder_password(&c[2]) {
                c[0].to_string()
            } else {
                hit = true;
                format!("{}{plain}@", &c[1])
            }
        })
        .into_owned();
    if hit {
        kinds.push("url-credentials");
        out = replaced;
    }
    // Tokens in URL queries and webhook paths.
    let (replaced, hit) = redact_text_urls(&out, &plain);
    if hit {
        kinds.push("url-token");
        out = replaced;
    }
    // `Authorization: Bearer <token>` (curl -H, docs).
    let mut hit = false;
    let replaced = TEXT_BEARER
        .replace_all(&out, |c: &Captures| {
            if secret_text_value(&c[2]) {
                hit = true;
                format!("{} {plain}", &c[1])
            } else {
                c[0].to_string()
            }
        })
        .into_owned();
    if hit {
        kinds.push("authorization-header");
        out = replaced;
    }
    // `export DB_PASSWORD=...`, `X-Api-Key: ...`, `--token ...`.
    let spans = assignment_spans(&out);
    if !spans.is_empty() {
        for (a, b) in spans.into_iter().rev() {
            out.replace_range(a..b, &plain);
        }
        kinds.push("secret-named-value");
    }
    (out, kinds)
}

/// [`redact_text`] applied to every string inside a JSON value (frontmatter,
/// settings). Returns the kinds of secret found.
pub fn redact_json_strings(v: &mut serde_json::Value) -> Vec<&'static str> {
    let mut kinds = Vec::new();
    match v {
        serde_json::Value::String(s) => {
            let (red, k) = redact_text(s);
            if !k.is_empty() {
                *s = red;
                kinds.extend(k);
            }
        }
        serde_json::Value::Array(a) => a
            .iter_mut()
            .for_each(|x| kinds.extend(redact_json_strings(x))),
        serde_json::Value::Object(m) => m
            .values_mut()
            .for_each(|x| kinds.extend(redact_json_strings(x))),
        _ => {}
    }
    kinds.dedup();
    kinds
}

/// Every string inside a JSON value.
pub fn json_strings(v: &serde_json::Value) -> Vec<&str> {
    match v {
        serde_json::Value::String(s) => vec![s.as_str()],
        serde_json::Value::Array(a) => a.iter().flat_map(json_strings).collect(),
        serde_json::Value::Object(m) => m.values().flat_map(json_strings).collect(),
        _ => Vec::new(),
    }
}

static MARKER: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\[REDACTED(?::[a-z0-9-]+)?\]").expect("valid regex"));

/// Does `text` contain a marker left by [`redact_text`]?
pub fn has_redaction_marker(text: &str) -> bool {
    MARKER.is_match(text)
}

/// Is `redacted` what [`redact_text`] makes of `original`, i.e. equal to it
/// except that each marker stands for one or more characters? Used so that
/// applying a profile to the tool it was captured from keeps the real values.
pub fn is_redacted_form_of(redacted: &str, original: &str) -> bool {
    if !has_redaction_marker(redacted) {
        return redacted == original;
    }
    let parts: Vec<&str> = MARKER.split(redacted).collect();
    let (first, last) = (parts[0], parts[parts.len() - 1]);
    let Some(rest) = original.strip_prefix(first) else {
        return false;
    };
    // Leftmost matching of each literal piece leaves the most room for the rest.
    let mut pos = 0;
    for piece in &parts[1..parts.len() - 1] {
        let from = pos + rest[pos..].chars().next().map_or(1, char::len_utf8);
        let Some(found) = rest.get(from..).and_then(|r| r.find(piece)) else {
            return false;
        };
        pos = from + found + piece.len();
    }
    rest.len() > pos + last.len() && rest.ends_with(last)
}

const SENSITIVE_FILE_NAMES: &[&str] = &[
    "credentials.json",
    "credential.json",
    "token.json",
    "tokens.json",
    "id_rsa",
    "id_dsa",
    "id_ecdsa",
    "id_ed25519",
    ".netrc",
    ".npmrc",
    ".pypirc",
];
const SENSITIVE_EXTENSIONS: &[&str] = &["pem", "key", "p12", "pfx", "jks", "keystore", "ppk"];

/// Text files read as prose (like instructions and rules), not as config or code.
const PROSE_EXTENSIONS: &[&str] = &[
    "md", "markdown", "mdx", "txt", "text", "rst", "adoc", "html", "htm",
];
/// Config formats checked line by line for `name = value` and `name: value`.
const CONFIG_EXTENSIONS: &[&str] = &[
    "toml",
    "yaml",
    "yml",
    "ini",
    "cfg",
    "conf",
    "cnf",
    "config",
    "properties",
    "env",
    "hcl",
    "tf",
    "tfvars",
];

fn extension(path: &str) -> String {
    let name = path.rsplit('/').next().unwrap_or(path).to_ascii_lowercase();
    name.rsplit_once('.')
        .map(|(_, e)| e.to_string())
        .unwrap_or_default()
}

fn prose_file(path: &str) -> bool {
    PROSE_EXTENSIONS.contains(&extension(path).as_str())
}

fn json_has_secret(v: &serde_json::Value, key: Option<&str>) -> bool {
    match v {
        serde_json::Value::String(s) => named_secret(key, s),
        serde_json::Value::Array(a) => a.iter().any(|x| json_has_secret(x, key)),
        serde_json::Value::Object(m) => m.iter().any(|(k, x)| json_has_secret(x, Some(k))),
        _ => false,
    }
}

/// Where in `v` a string sits under a secret-sounding key (at any depth) or
/// has a known token format: `ui.apiToken`, `hooks[0].env.PASSWORD`. Values
/// capture already redacted (`[REDACTED]`) don't count.
pub fn json_secret_fields(v: &serde_json::Value) -> Vec<String> {
    fn walk(v: &serde_json::Value, key: Option<&str>, at: &str, out: &mut Vec<String>) {
        match v {
            serde_json::Value::String(s) => {
                if named_secret(key, s) && !has_redaction_marker(s) {
                    out.push(at.to_string());
                }
            }
            serde_json::Value::Array(a) => {
                for (i, x) in a.iter().enumerate() {
                    walk(x, key, &format!("{at}[{i}]"), out);
                }
            }
            serde_json::Value::Object(m) => {
                for (k, x) in m {
                    let at = if at.is_empty() {
                        k.clone()
                    } else {
                        format!("{at}.{k}")
                    };
                    walk(x, Some(k), &at, out);
                }
            }
            _ => {}
        }
    }
    let mut out = Vec::new();
    walk(v, None, "", &mut out);
    out
}

/// [`json_secret_fields`], replaced by `[REDACTED]` markers. `key` is the
/// name `v` is stored under. Returns whether anything was replaced.
pub fn redact_secret_fields(v: &mut serde_json::Value, key: Option<&str>) -> bool {
    match v {
        serde_json::Value::String(s) => {
            if named_secret(key, s) && !has_redaction_marker(s) {
                *s = "[REDACTED:secret-named-field]".to_string();
                true
            } else {
                false
            }
        }
        serde_json::Value::Array(a) => a
            .iter_mut()
            .fold(false, |hit, x| redact_secret_fields(x, key) | hit),
        serde_json::Value::Object(m) => m
            .iter_mut()
            .fold(false, |hit, (k, x)| redact_secret_fields(x, Some(k)) | hit),
        _ => false,
    }
}

/// `API_KEY = "..."`, `password: '...'`, `"token": "..."`, `export SECRET="..."`:
/// a quoted value assigned to a name, in a script or config file of any kind.
static QUOTED_ASSIGN: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r#"(?:^|[^A-Za-z0-9_.-])["']?([A-Za-z_][A-Za-z0-9_.-]*)["']?[ \t]*(?::=|=>|=|:)[ \t]*(?:"((?:[^"\\\n]|\\.)*)"|'([^'\n]*)')"#,
    )
    .expect("valid regex")
});

/// Byte ranges of quoted values assigned to secret-sounding names. Shell
/// expansions (`"$(cat f)"`, `"$TOKEN"`), placeholders (`"<token>"`,
/// `"YOUR_KEY"`) and values with spaces (prompts, messages) don't count.
fn quoted_assignment_spans(text: &str) -> Vec<(usize, usize)> {
    let mut out = Vec::new();
    for c in QUOTED_ASSIGN.captures_iter(text) {
        let Some(value) = c.get(2).or_else(|| c.get(3)) else {
            continue;
        };
        let v = value.as_str();
        if named_secret(Some(&c[1]), v)
            && !v.starts_with('$')
            && !v.contains(char::is_whitespace)
            && !placeholder_value(v)
            && !has_redaction_marker(v)
        {
            out.push((value.start(), value.end()));
        }
    }
    out
}

/// Redact a text file inside a skill: like [`redact_text`], and for scripts
/// and config files (anything but prose such as `.md`, whatever the
/// extension) also quoted values assigned to secret-sounding names, which
/// free text only catches when they mix letters and digits.
pub fn redact_support_file(path: &str, text: &str) -> (String, Vec<&'static str>) {
    let (mut out, mut kinds) = redact_text(text);
    if !prose_file(path) {
        let spans = quoted_assignment_spans(&out);
        if !spans.is_empty() {
            for (a, b) in spans.into_iter().rev() {
                out.replace_range(a..b, REDACTED_PASSWORD);
            }
            if !kinds.contains(&"secret-named-value") {
                kinds.push("secret-named-value");
            }
        }
    }
    (out, kinds)
}

/// The kind of credential [`redact_support_file`] would replace, if any.
pub fn support_file_secret_kind(path: &str, text: &str) -> Option<&'static str> {
    redact_support_file(path, text).1.first().copied()
}

/// In structured files only names and known formats count; entropy alone
/// would flag lockfile hashes and the like.
fn named_secret(key: Option<&str>, value: &str) -> bool {
    strong_secret_kind(value).is_some()
        || (key.is_some_and(is_secret_name)
            && value.chars().count() >= 6
            && !has_ref(value)
            && !obviously_not_secret(value))
}

/// Why a file (e.g. inside a skill) must not be copied into a profile at all:
/// key and credential files, structured files holding secret values, or
/// binary files containing a known token format.
pub fn sensitive_file(path: &str, content: &[u8]) -> Option<String> {
    let name = path.rsplit('/').next().unwrap_or(path).to_ascii_lowercase();
    let ext = extension(path);
    let ext = ext.as_str();
    if SENSITIVE_FILE_NAMES.contains(&name.as_str())
        || SENSITIVE_EXTENSIONS.contains(&ext)
        || name.starts_with(".env")
        || name.starts_with("client_secret")
        || name.starts_with("service-account")
        || name.starts_with("service_account")
    {
        return Some("looks like a credential file".to_string());
    }
    let text = String::from_utf8_lossy(content);
    if std::str::from_utf8(content).is_err() {
        if let Some(k) = strong_secret_kind(&text) {
            return Some(format!("binary file contains a {k}"));
        }
        return None;
    }
    // JSON whatever the extension (`.jsonc`, `.json5`, `settings.local`, none),
    // with comments and trailing commas allowed.
    let looks_json = matches!(ext, "json" | "jsonc" | "json5")
        || (!prose_file(path) && text.trim_start().starts_with(['{', '[']));
    if looks_json
        && let Some(v) = serde_json::from_str::<serde_json::Value>(&text)
            .ok()
            .or_else(|| crate::edit::json::parse_value(Some(&text), Path::new(path)).ok())
        && json_has_secret(&v, None)
    {
        return Some("contains secret values".to_string());
    }
    if CONFIG_EXTENSIONS.contains(&ext) {
        for line in text.lines() {
            if let Some((k, v)) = line.split_once(['=', ':']) {
                let v = v.trim().trim_matches(['"', '\'', ',']);
                if named_secret(Some(k.trim().trim_matches(['"', '\''])), v) {
                    return Some("contains secret values".to_string());
                }
            }
        }
    }
    None
}

static JSON_PAIR: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r#""([^"\\]+)"(\s*:\s*)"((?:[^"\\]|\\.)*)""#).expect("valid regex")
});
static TOML_PAIR: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r#"(^|[\s{,])([A-Za-z0-9_-]+|"[^"]+"|'[^']+')(\s*=\s*)"((?:[^"\\]|\\.)*)""#)
        .expect("valid regex")
});
/// `key = 'value'`: a TOML literal string, which has no escapes.
static TOML_LITERAL_PAIR: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r#"(^|[\s{,])([A-Za-z0-9_-]+|"[^"]+"|'[^']+')(\s*=\s*)'([^'\n]*)'"#)
        .expect("valid regex")
});

static URL_PASSWORD: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r#"(?i)\b([a-z][a-z0-9+.-]*://[^\s:/@"]*:)([^\s@/"]{3,})@"#).expect("valid regex")
});
static FLAG_PAIR: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r#""(--?[A-Za-z0-9][A-Za-z0-9_-]*)"(\s*,\s*)"((?:[^"\\]|\\.)*)""#)
        .expect("valid regex")
});
static FLAG_ASSIGN: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r#""(--?[A-Za-z0-9][A-Za-z0-9_-]*)=((?:[^"\\]|\\.)*)""#).expect("valid regex")
});
/// `'--token', 'value'` in a TOML array of literal strings.
static FLAG_LITERAL_PAIR: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r#"'(--?[A-Za-z0-9][A-Za-z0-9_-]*)'(\s*,\s*)'([^'\n]*)'"#).expect("valid regex")
});
static FLAG_LITERAL_ASSIGN: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r#"'(--?[A-Za-z0-9][A-Za-z0-9_-]*)=([^'\n]*)'"#).expect("valid regex")
});
/// Any string on a line, double-quoted (JSON, TOML) or single-quoted (TOML).
static QUOTED: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r#""((?:[^"\\]|\\.)*)"|'([^'\n]*)'"#).expect("valid regex"));
/// A line ending in `"--token",`: a pretty-printed array whose next line
/// holds the value.
static TRAILING_FLAG: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r#"(?:"(--?[A-Za-z0-9][A-Za-z0-9_-]*)"|'(--?[A-Za-z0-9][A-Za-z0-9_-]*)')\s*,\s*$"#)
        .expect("valid regex")
});
/// The string a line starts with.
static LEADING_STRING: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r#"^(\s*)(?:"((?:[^"\\]|\\.)*)"|'([^'\n]*)')"#).expect("valid regex")
});

/// A JSON or TOML basic string's content, with `\"` and `\\` unescaped.
fn unescape(value: &str) -> String {
    value.replace("\\\"", "\"").replace("\\\\", "\\")
}

/// For display: a value capture would flag as random-looking (a high-entropy
/// value), and that also has a run of 16 or more letters and digits mixing
/// both, as tokens do. The second test keeps ordinary ids such as model names
/// (`claude-sonnet-4-5-20250929`) and camel-case tool names visible.
fn random_value(value: &str) -> bool {
    if value.contains(MASK) {
        return false;
    }
    let literal = strip_refs(value);
    looks_random_token(&literal)
        && literal
            .split(|c: char| !c.is_ascii_alphanumeric())
            .any(|run| {
                run.len() >= 16
                    && run.chars().any(|c| c.is_ascii_digit())
                    && run.chars().any(|c| c.is_ascii_alphabetic())
            })
}

/// For display: known formats, values under a secret-sounding name, and
/// random-looking values (see [`random_value`]). `value` is already unescaped.
fn shown_secret(key: &str, value: &str) -> bool {
    if value.contains(MASK) {
        return false; // already masked
    }
    // Like capture: under a secret-sounding name any literal counts, however
    // short, except configuration such as numbers, booleans, URLs and paths.
    let literal = strip_refs(value);
    let named = is_secret_name(key)
        && sole_ref(value).is_none()
        && (!has_ref(value) || substantial_literal(&literal))
        && !obviously_not_secret(&literal);
    strong_secret_kind(value).is_some()
        || named
        || BEARER
            .captures(value)
            .is_some_and(|c| !has_ref(&c[2]) && c[2].len() >= 6)
        || random_value(value)
}

/// The value given to a flag (`"--token", "value"`), as capture judges it:
/// by the flag's name, and never a value that is itself a flag.
fn shown_flag_value(flag: &str, value: &str) -> bool {
    !value.starts_with('-') && shown_secret(flag.trim_start_matches('-'), value)
}

/// For each line, the flag left waiting for its value by the line before it
/// (skipping blank lines), as in a pretty-printed `args` array with
/// `"--token",` on one line and the value on the next. Pass the result to
/// [`mask_line_after`].
pub fn pending_flags<'a>(lines: impl IntoIterator<Item = &'a str>) -> Vec<Option<String>> {
    let mut out = Vec::new();
    let mut pending: Option<String> = None;
    for line in lines {
        out.push(pending.clone());
        if line.trim().is_empty() {
            continue;
        }
        pending = TRAILING_FLAG.captures(line).map(|c| {
            c.get(1)
                .or_else(|| c.get(2))
                .map_or_else(String::new, |m| m.as_str().to_string())
        });
    }
    out
}

/// [`mask_line`] for a line that may hold the value of `flag`, given on the
/// line before (see [`pending_flags`]).
pub fn mask_line_after(line: &str, flag: Option<&str>) -> String {
    if let Some(flag) = flag
        && let Some(c) = LEADING_STRING.captures(line)
    {
        let (value, quote) = match (c.get(2), c.get(3)) {
            (Some(v), _) => (unescape(v.as_str()), '"'),
            (_, Some(v)) => (v.as_str().to_string(), '\''),
            _ => (String::new(), '"'),
        };
        if shown_flag_value(flag, &value) {
            let rest = &line[c.get(0).map_or(0, |m| m.end())..];
            return mask_line(&format!("{}{quote}{MASK}{quote}{rest}", &c[1]));
        }
    }
    mask_line(line)
}

/// Mask anything in one line of config text that looks like a credential:
/// known token formats anywhere, secret-looking values of JSON / TOML keys and
/// flags, and random-looking strings. Used for every line kitbag displays, so
/// existing secrets in your files never show up in a diff.
pub fn mask_line(line: &str) -> String {
    let mut out = URL_PASSWORD
        .replace_all(line, |c: &Captures| {
            if has_ref(&c[2]) {
                c[0].to_string()
            } else {
                format!("{}••••••••@", &c[1])
            }
        })
        .into_owned();
    for p in STRONG.iter().filter(|p| p.kind != "url-credentials") {
        out =
            p.re.replace_all(&out, |c: &Captures| {
                if has_ref(&c[0]) {
                    c[0].to_string()
                } else {
                    "••••••••".into()
                }
            })
            .into_owned();
    }
    out = JSON_PAIR
        .replace_all(&out, |c: &Captures| {
            if shown_secret(&c[1], &unescape(&c[3])) {
                format!("\"{}\"{}\"••••••••\"", &c[1], &c[2])
            } else {
                c[0].to_string()
            }
        })
        .into_owned();
    // `"--password", "value"` in args arrays, and `"--token=value"`; in TOML
    // also with single quotes.
    out = FLAG_PAIR
        .replace_all(&out, |c: &Captures| {
            if shown_flag_value(&c[1], &unescape(&c[3])) {
                format!("\"{}\"{}\"••••••••\"", &c[1], &c[2])
            } else {
                c[0].to_string()
            }
        })
        .into_owned();
    out = FLAG_LITERAL_PAIR
        .replace_all(&out, |c: &Captures| {
            if shown_flag_value(&c[1], &c[3]) {
                format!("'{}'{}'••••••••'", &c[1], &c[2])
            } else {
                c[0].to_string()
            }
        })
        .into_owned();
    out = FLAG_ASSIGN
        .replace_all(&out, |c: &Captures| {
            if shown_secret(c[1].trim_start_matches('-'), &unescape(&c[2])) {
                format!("\"{}=••••••••\"", &c[1])
            } else {
                c[0].to_string()
            }
        })
        .into_owned();
    out = FLAG_LITERAL_ASSIGN
        .replace_all(&out, |c: &Captures| {
            if shown_secret(c[1].trim_start_matches('-'), &c[2]) {
                format!("'{}=••••••••'", &c[1])
            } else {
                c[0].to_string()
            }
        })
        .into_owned();
    out = TOML_PAIR
        .replace_all(&out, |c: &Captures| {
            if shown_secret(c[2].trim_matches(['"', '\'']), &unescape(&c[4])) {
                format!("{}{}{}\"••••••••\"", &c[1], &c[2], &c[3])
            } else {
                c[0].to_string()
            }
        })
        .into_owned();
    out = TOML_LITERAL_PAIR
        .replace_all(&out, |c: &Captures| {
            if shown_secret(c[2].trim_matches(['"', '\'']), &c[4]) {
                format!("{}{}{}'••••••••'", &c[1], &c[2], &c[3])
            } else {
                c[0].to_string()
            }
        })
        .into_owned();
    // Random-looking strings anywhere else, such as a token on its own in an
    // `args` array, as capture redacts them from MCP servers.
    out = QUOTED
        .replace_all(&out, |c: &Captures| match (c.get(1), c.get(2)) {
            (Some(v), _) if random_value(&unescape(v.as_str())) => "\"••••••••\"".to_string(),
            (_, Some(v)) if random_value(v.as_str()) => "'••••••••'".to_string(),
            _ => c[0].to_string(),
        })
        .into_owned();
    // Whatever capture would redact from free text (instructions, rules):
    // `export DB_PASSWORD=...`, `Bearer ...`, webhook and query tokens.
    redact_text_with(&out, &|_| MASK.to_string()).0
}

/// What a secret is shown as.
const MASK: &str = "••••••••";

static KEY_BEGIN: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"-----BEGIN [A-Z ]*PRIVATE KEY-----").expect("valid regex"));
static KEY_END: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"-----END [A-Z ]*PRIVATE KEY-----").expect("valid regex"));
static BASE64_LINE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^[A-Za-z0-9+/=]{16,}$").expect("valid regex"));
static PEM_HEADER: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^[A-Za-z-]+: \S").expect("valid regex"));

/// Mask private keys (PEM `-----BEGIN ... PRIVATE KEY-----` blocks) across
/// lines, before a text is split into lines for display. Each block becomes
/// one line, `•••••••• (private key, sha256:1a2b3c4d)`: the short hash can't
/// be turned back into the key, but shows whether a key changed.
///
/// A block without an `END` line runs as far as it looks like one: base64
/// lines, PEM headers (`Proc-Type: ...`) and blank lines between them.
pub fn mask_key_blocks(text: &str) -> String {
    let mut out = String::new();
    let mut rest = text;
    while let Some(m) = KEY_BEGIN.find(rest) {
        out.push_str(&rest[..m.start()]);
        let block = &rest[m.start()..];
        let len = match KEY_END.find(block) {
            Some(e) => e.end(),
            None => unterminated_key_len(block),
        };
        let hash = crate::txn::sha256(Some(&block.as_bytes()[..len])).unwrap_or_default();
        out.push_str(&format!("{MASK} (private key, sha256:{})", &hash[..8]));
        rest = &block[len..];
    }
    out.push_str(rest);
    out
}

/// Length of a key block that has no `END` line: its first line plus the
/// key-like lines after it, without the line break after the last one.
fn unterminated_key_len(block: &str) -> usize {
    let mut lines = block.split_inclusive('\n');
    let first = lines.next().unwrap_or_default();
    let mut end = first.trim_end_matches(['\n', '\r']).len();
    let mut pos = first.len();
    for line in lines {
        let content = line.trim_end_matches(['\n', '\r']);
        let t = content.trim();
        if BASE64_LINE.is_match(t) || PEM_HEADER.is_match(t) {
            end = pos + content.len();
        } else if !t.is_empty() {
            break;
        }
        pos += line.len();
    }
    end
}

/// The forms a secret can take on disk: raw, and escaped inside a JSON / TOML string.
pub fn variants(secret: &str) -> Vec<String> {
    let mut out = vec![secret.to_string()];
    let json = serde_json::to_string(secret).expect("strings serialize");
    let inner = json[1..json.len() - 1].to_string();
    if inner != secret {
        out.push(inner);
    }
    out
}

/// Does `text` contain any of `secrets`, in any on-disk form?
pub fn contains_any(text: &str, secrets: &BTreeSet<String>) -> bool {
    secrets
        .iter()
        .flat_map(|s| variants(s))
        .any(|v| v.len() >= 4 && text.contains(&v))
}

/// Mask known secret values in output shown to humans (diffs, logs).
pub fn mask(text: &str, secrets: &BTreeSet<String>) -> String {
    let mut list: Vec<String> = secrets
        .iter()
        .flat_map(|s| variants(s))
        .filter(|s| s.len() >= 4)
        .collect();
    list.sort_by_key(|s| std::cmp::Reverse(s.len()));
    let mut out = text.to_string();
    for s in list {
        out = out.replace(s.as_str(), "••••••••");
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn env(pairs: &[(&str, &str)]) -> HashMap<String, String> {
        pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect()
    }

    #[test]
    fn reference_parsing() {
        assert_eq!(refs_in("a ${A} b ${B:-x}"), vec!["A", "B"]);
        assert_eq!(
            sole_ref("${TOKEN:-dev}"),
            Some(EnvRef {
                name: "TOKEN".into(),
                fallback: Some("dev".into())
            })
        );
        assert_eq!(sole_ref("Bearer ${TOKEN}"), None);
    }

    #[test]
    fn normalisation() {
        assert_eq!(normalize_refs("${env:API_KEY}", false), "${API_KEY}");
        assert_eq!(normalize_refs("$API_KEY", false), "$API_KEY");
        assert_eq!(normalize_refs("$A-$B ${C}", true), "${A}-${B} ${C}");
        assert_eq!(normalize_refs("cost: \\$5", true), "cost: \\$5");
        assert_eq!(
            normalize_brace_refs("Bearer {env:API_KEY} {env:B}"),
            "Bearer ${API_KEY} ${B}"
        );
        assert_eq!(
            normalize_brace_refs("${env:A} {file:~/k}"),
            "${env:A} {file:~/k}"
        );
    }

    #[test]
    fn rendering_styles() {
        let e = env(&[("SET", "the-value")]);
        let mut st = RenderState::default();
        let v = "${SET} ${UNSET:-fb} ${MISSING}";
        assert_eq!(render_refs(v, RefStyle::Dollar, &e, &mut st), v);
        assert_eq!(
            render_refs(v, RefStyle::EnvColon, &e, &mut st),
            "${env:SET} fb ${env:MISSING}"
        );
        assert_eq!(
            render_refs(v, RefStyle::DollarPlain, &e, &mut st),
            "${SET} fb ${MISSING}"
        );
        assert_eq!(
            render_refs(v, RefStyle::EnvBrace, &e, &mut st),
            "{env:SET} fb {env:MISSING}"
        );
        assert!(st.missing.is_empty() && st.resolved.is_empty());
        assert_eq!(
            render_refs(v, RefStyle::Resolve, &e, &mut st),
            "the-value fb ${MISSING}"
        );
        assert!(st.missing.contains("MISSING"));
        assert!(st.resolved.contains("the-value"));
    }

    #[test]
    fn secret_names() {
        for n in [
            "GITHUB_PERSONAL_ACCESS_TOKEN",
            "apiKey",
            "STRIPE_KEY",
            "X-Api-Key",
            "password",
            "clientSecret",
            "PGPASSWORD",
            "SSH_PASSPHRASE",
            "VAULT_SECRET_ID",
            "APITOKEN",
            "BASIC_AUTH",
            "PRIVATE_KEY",
            "DB_CREDENTIAL",
        ] {
            assert!(is_secret_name(n), "{n}");
        }
        for h in ["X-Auth", "X-Session", "X-Goog-Api-Key", "Cookie", "X-Token"] {
            assert!(is_secret_header_name(h), "{h}");
        }
        for h in ["Accept", "Content-Type", "X-Request-Id"] {
            assert!(!is_secret_header_name(h), "{h}");
        }
        for (n, v) in [
            ("DB_PASSWORD", "rootE"),
            ("API_KEY", "x1"),
            (
                "AWS_SECRET_ACCESS_KEY",
                "/wJalrXUtnFEMIK7MDENGbPxRfiCYEXAMPLEKEYG",
            ),
        ] {
            assert_eq!(classify(Some(n), v), Some("secret-named-field"), "{n}={v}");
        }
        for (n, v) in [
            ("DB_PASSWORD", ""),
            ("API_KEY", "${API_KEY}"),
            ("SECRET_DAYS", "30"),
            ("ENABLE_AUTH", "true"),
            ("TOKEN_PATH", "/run/secrets/token"),
            ("PASSWORD", "/run/secrets/db_password"),
            ("PASSWORD", "C:\\keys\\db.txt"),
        ] {
            assert_eq!(classify(Some(n), v), None, "{n}={v}");
        }
        for n in [
            "LOG_LEVEL",
            "PUBLIC_KEY",
            "KEY_ID",
            "KEY_PREFIX",
            "KEYBOARD",
            "MODE",
            "PATH",
            "AUTH_SERVER_URL",
            "SESSION_TIMEOUT",
            "MAX_TOKENS",
            "AUTH_MODE",
            "TOKENIZER",
            "PASSWORD_MIN_LENGTH",
        ] {
            assert!(!is_secret_name(n), "{n}");
        }
    }

    #[test]
    fn strong_patterns() {
        assert_eq!(
            strong_secret_kind("ghp_FAKE0123456789abcdefghijklmnopqrstuv"),
            Some("github-token")
        );
        assert_eq!(
            strong_secret_kind("postgresql://admin:hunter2secret@db:5432/app"),
            Some("url-credentials")
        );
        assert_eq!(strong_secret_kind("postgresql://admin:${PW}@db/app"), None);
        assert_eq!(strong_secret_kind("just some words"), None);
    }

    #[test]
    fn provider_key_formats() {
        let alnum = "Zk3jQ9vLm2Xp8RtY4wNb7HcF1sDg6KaE";
        let hex64 = "0123456789abcdef".repeat(4);
        let cases = [
            (format!("xai-{alnum}{alnum}{alnum}"), "xai-key"),
            (format!("gsk_{alnum}{alnum}"), "groq-key"),
            (format!("pplx-{alnum}{alnum}"), "perplexity-key"),
            (format!("tvly-dev-{alnum}"), "tavily-key"),
            (format!("sbp_{}", &hex64[..40]), "supabase-token"),
            (format!("r8_{alnum}"), "replicate-token"),
            (format!("sk-or-v1-{hex64}"), "openrouter-key"),
            (format!("fc-{}", &hex64[..32]), "firecrawl-key"),
            (format!("sk-ant-admin01-{alnum}"), "anthropic-key"),
        ];
        for (token, kind) in &cases {
            assert_eq!(strong_secret_kind(token), Some(*kind), "{token}");
            let (t, kinds) = redact_text(&format!("Use key {token} when calling it."));
            assert_eq!(t, format!("Use key [REDACTED:{kind}] when calling it."));
            assert_eq!(kinds, vec![*kind]);
            // Values in MCP env blocks use the same table.
            assert_eq!(classify(Some("MODE"), token), Some(*kind));
        }
        for prose in [
            "xai-sdk and gsk_ prefixes, pplx-api docs, tvly-dev keys",
            "the fc-barcelona fan club, r8_ratio, sbp_short",
            "xai-grok-2-latest is the model; sk-or-v1-abc is not a key",
        ] {
            assert_eq!(strong_secret_kind(prose), None, "{prose}");
        }
    }

    #[test]
    fn entropy_heuristic_skips_common_non_secrets() {
        assert!(looks_random_token("q8Zr2LkP0vXa7MnB4cTy9WdE"));
        assert!(!looks_random_token("@modelcontextprotocol/server-github"));
        assert!(!looks_random_token("/usr/local/bin/some-server-binary"));
        assert!(!looks_random_token("https://mcp.linear.app/mcp"));
        assert!(!looks_random_token(
            "0123456789abcdef0123456789abcdef01234567"
        ));
    }

    #[test]
    fn redacts_server() {
        let s = McpServer {
            command: Some("uvx".into()),
            args: vec![
                "mcp-server-postgres".into(),
                "--connection-string".into(),
                "postgresql://admin:hunter2secret@db.internal:5432/app".into(),
                "--api-key=q8Zr2LkP0vXa7MnB4cTy9WdE".into(),
            ],
            env: [
                (
                    "GITHUB_TOKEN".to_string(),
                    "ghp_FAKE0123456789abcdefghijklmnopqrstuv".to_string(),
                ),
                ("LOG_LEVEL".to_string(), "debug".to_string()),
            ]
            .into_iter()
            .collect(),
            ..Default::default()
        };
        let (r, red) = redact_server("pg", &s);
        assert_eq!(r.env["GITHUB_TOKEN"], "${GITHUB_TOKEN}");
        assert_eq!(r.env["LOG_LEVEL"], "debug");
        assert_eq!(
            r.args[2],
            "postgresql://admin:${PG_PASSWORD}@db.internal:5432/app"
        );
        assert_eq!(r.args[3], "--api-key=${PG_API_KEY}");
        assert_eq!(red.len(), 3);
    }

    #[test]
    fn redacts_headers_and_urls() {
        let s = McpServer {
            url: Some("https://x.dev/mcp?api_key=q8Zr2LkP0vXa7MnB4cTy9WdE&region=eu".into()),
            headers: [
                (
                    "Authorization".to_string(),
                    "Bearer abc123def456".to_string(),
                ),
                ("X-Keep".to_string(), "Bearer ${ALREADY}".to_string()),
            ]
            .into_iter()
            .collect(),
            ..Default::default()
        };
        let (r, red) = redact_server("svc", &s);
        assert_eq!(r.headers["Authorization"], "Bearer ${SVC_TOKEN}");
        assert_eq!(r.headers["X-Keep"], "Bearer ${ALREADY}");
        assert_eq!(
            r.url.as_deref(),
            Some("https://x.dev/mcp?api_key=${SVC_API_KEY}&region=eu")
        );
        assert_eq!(red.len(), 2);
    }

    #[test]
    fn secret_names_with_config_values_are_left_alone() {
        assert_eq!(
            classify(Some("AUTH_TOKEN_URL"), "https://auth.example.com/token"),
            None
        );
        assert_eq!(classify(Some("TOKEN_TTL"), "3600000"), None);
        assert_eq!(
            classify(Some("DB_PASSWORD"), "84629137"),
            Some("secret-named-field")
        );
        assert_eq!(
            classify(Some("AUTH_HEADER"), "Bearer abc123def456"),
            Some("authorization-header")
        );
        assert_eq!(
            sensitive_file(
                "package-lock.json",
                br#"{"integrity": "sha512-q8Zr2LkP0vXa7MnB4cTy9WdEq8Zr2LkP0vXa7MnB4c=="}"#
            ),
            None
        );
        assert_eq!(
            classify(Some("API_KEY"), "a-real-looking-key"),
            Some("secret-named-field")
        );
        assert_eq!(
            classify(
                Some("MDB_MCP_CONNECTION_STRING"),
                "mongodb+srv://admin:Pa$$w0rd99@cluster0.x.net/db"
            ),
            Some("url-credentials")
        );
        assert_eq!(
            classify(Some("DB_CONNECTION_STRING"), "Server=db;Password=x1y2z3"),
            Some("secret-named-field")
        );
    }

    #[test]
    fn structured_checks_cover_any_extension_and_quoted_keys() {
        let secret = |path: &str, text: &str| sensitive_file(path, text.as_bytes());
        // JSON with comments, or under an extension that isn't `.json`.
        assert!(
            secret(
                "settings.jsonc",
                "{\n  // mine\n  \"apiToken\": \"correcthorse\",\n}\n"
            )
            .is_some()
        );
        assert!(secret("config.json5", r#"{"password": "correcthorse"}"#).is_some());
        assert!(secret("settings.local", r#"{"ui": {"authToken": "correcthorse"}}"#).is_some());
        // More config formats, and single-quoted keys.
        assert!(secret("my.cnf", "[client]\npassword=correcthorse\n").is_some());
        assert!(secret("prod.env", "DB_PASSWORD=correcthorse\n").is_some());
        assert!(secret("vars.tfvars", "api_key = \"correcthorse\"\n").is_some());
        assert!(secret("c.yaml", "'api_key': 'correcthorse'\n").is_some());
        // Prose is read as free text, not parsed as config.
        assert_eq!(secret("README.md", r#"{"password": "string"}"#), None);
        assert_eq!(secret("settings.local", r#"{"theme": "dark"}"#), None);
    }

    #[test]
    fn quoted_assignments_in_scripts_are_redacted() {
        let (red, kinds) = redact_support_file(
            "scripts/deploy.sh",
            "export DB_PASSWORD=\"correcthorse\"\nclient_secret = 'battery'\n",
        );
        assert!(
            !red.contains("correcthorse") && !red.contains("battery"),
            "{red}"
        );
        assert!(red.contains("DB_PASSWORD=\"[REDACTED]\""), "{red}");
        assert_eq!(kinds, ["secret-named-value"]);
        assert_eq!(
            support_file_secret_kind("x.py", "API_TOKEN = \"staple\""),
            Some("secret-named-value")
        );
        assert_eq!(
            support_file_secret_kind("x", "\"token\": \"staple\""),
            Some("secret-named-value")
        );
        // Shell expansions, placeholders, prompts and settings about a secret.
        for kept in [
            "TOKEN=\"$(cat ~/.token)\"",
            "API_KEY=\"${API_KEY}\"",
            "API_KEY = \"YOUR_API_KEY\"",
            "api_key: \"<your key>\"",
            "password = \"Enter your password\"",
            "TOKEN_TTL = \"3600s\"",
            "PASSWORD = \"[REDACTED]\"",
            "auth_url = \"https://auth.example.com/token\"",
        ] {
            assert_eq!(support_file_secret_kind("run.sh", kept), None, "{kept}");
        }
        // Prose keeps the free-text rules: letters-only values aren't secrets.
        assert_eq!(
            support_file_secret_kind("SKILL.md", "export DB_PASSWORD=\"correcthorse\""),
            None
        );
    }

    #[test]
    fn secret_fields_are_found_at_any_depth() {
        let v = serde_json::json!({
            "ui": {"theme": "dark", "apiToken": "correcthorse"},
            "hooks": [{"env": {"PASSWORD": "staple", "TOKEN": "${TOKEN}"}}],
            "done": {"secret": "[REDACTED:secret-named-field]"},
        });
        assert_eq!(
            json_secret_fields(&v),
            ["ui.apiToken", "hooks[0].env.PASSWORD"]
        );
        let mut red = v.clone();
        assert!(redact_secret_fields(&mut red, None));
        assert!(json_secret_fields(&red).is_empty());
        assert_eq!(red["ui"]["theme"], "dark");
        assert_eq!(red["hooks"][0]["env"]["TOKEN"], "${TOKEN}");
        assert!(!red.to_string().contains("correcthorse"));
    }

    #[test]
    fn redacts_secrets_hidden_in_wrapper_args() {
        let s = McpServer {
            command: Some("npx".into()),
            args: vec![
                "mcp-remote".into(),
                "https://x.dev/sse?api_key=a8f3kd93jd83hd73hs".into(),
                "--header".into(),
                "Authorization: Bearer a8f3kd93jd83hd73hs".into(),
                "-e".into(),
                "DB_PASSWORD=hunter2".into(),
                "--header".into(),
                "X-Api-Key:abcabc123123".into(),
                "--header=Authorization: Bearer lowentropytoken1".into(),
                "postgresql://admin:s3cr3t$pw@db/app".into(),
            ],
            ..Default::default()
        };
        let (r, red) = redact_server("remote", &s);
        assert_eq!(r.args[1], "https://x.dev/sse?api_key=${REMOTE_API_KEY}");
        assert_eq!(r.args[3], "Authorization: Bearer ${REMOTE_TOKEN}");
        assert_eq!(r.args[5], "DB_PASSWORD=${DB_PASSWORD}");
        assert_eq!(r.args[7], "X-Api-Key:${REMOTE_X_API_KEY}");
        // A different token than args[3], so a variable of its own.
        assert_eq!(
            r.args[8],
            "--header=Authorization: Bearer ${REMOTE_TOKEN_2}"
        );
        assert_eq!(r.args[9], "postgresql://admin:${REMOTE_PASSWORD}@db/app");
        assert_eq!(red.len(), 6);
        let flat = r.args.join(" ");
        for leak in ["a8f3kd93", "hunter2", "s3cr3t", "abcabc", "lowentropy"] {
            assert!(!flat.contains(leak), "{leak} in {flat}");
        }
    }

    #[test]
    fn different_secrets_in_one_server_get_different_variables() {
        let s = McpServer {
            command: Some("docker".into()),
            args: vec![
                "-e".into(),
                "API_KEY=FakeFirstSecret111".into(),
                "postgres://u:FakePwOne1@db/a".into(),
                "postgres://u:FakePwTwo2@db/b?password=FakePwThree3".into(),
                "https://x.example/sse?token=FakeTokA1b2&token=FakeTokC3d4".into(),
                // The same secret again shares its variable.
                "--header=Authorization: Bearer FakeBearer123".into(),
                "Authorization: Bearer FakeBearer123".into(),
            ],
            env: [("API_KEY".to_string(), "FakeSecondSecret22".to_string())]
                .into_iter()
                .collect(),
            ..Default::default()
        };
        let (r, red) = redact_server("svc", &s);
        assert_eq!(r.env["API_KEY"], "${API_KEY}");
        assert_eq!(r.args[1], "API_KEY=${API_KEY_2}");
        assert_eq!(r.args[2], "postgres://u:${SVC_PASSWORD}@db/a");
        assert_eq!(
            r.args[3],
            "postgres://u:${SVC_PASSWORD_2}@db/b?password=${SVC_PASSWORD_3}"
        );
        assert_eq!(
            r.args[4],
            "https://x.example/sse?token=${SVC_TOKEN}&token=${SVC_TOKEN_2}"
        );
        assert_eq!(r.args[5], "--header=Authorization: Bearer ${SVC_TOKEN_3}");
        assert_eq!(r.args[6], "Authorization: Bearer ${SVC_TOKEN_3}");
        // One variable per secret, one fingerprint per variable.
        let mut by_var: HashMap<&str, &Option<String>> = HashMap::new();
        for x in &red {
            assert!(x.fingerprint.is_some(), "{x:?}");
            let prev = by_var.insert(&x.env_var, &x.fingerprint);
            assert!(prev.is_none_or(|p| p == &x.fingerprint), "{x:?}");
        }
        assert_eq!(by_var.len(), 8);
        // Two tokens of the same kind in the command.
        let s = McpServer {
            command: Some(format!("run ghp_{} ghp_{}", "A".repeat(36), "B".repeat(36))),
            ..Default::default()
        };
        let (r, _) = redact_server("svc", &s);
        assert_eq!(
            r.command.as_deref(),
            Some("run ${SVC_COMMAND_SECRET} ${SVC_COMMAND_SECRET_2}")
        );
    }

    #[test]
    fn colon_pairs_that_are_not_headers_keep_their_format() {
        let s = McpServer {
            command: Some("x".into()),
            args: vec!["--api-key".into(), "abc:XyZ123SecretValue987Qw".into()],
            ..Default::default()
        };
        let (r, _) = redact_server("svc", &s);
        assert_eq!(r.args[1], "${SVC_API_KEY}");
    }

    #[test]
    fn masks_escaped_forms() {
        let secret = "p@ss\"w0rd\\x";
        let secrets: BTreeSet<String> = [secret.to_string()].into_iter().collect();
        let json = serde_json::to_string(secret).unwrap();
        assert!(contains_any(&json, &secrets));
        assert!(!mask(&json, &secrets).contains("w0rd"));
    }

    #[test]
    fn redacts_free_text() {
        let (t, kinds) = redact_text("use ghp_FAKE0123456789abcdefghijklmnopqrstuv please");
        assert_eq!(t, "use [REDACTED:github-token] please");
        assert_eq!(kinds, vec!["github-token"]);
    }

    #[test]
    fn free_text_url_passwords_keep_the_markdown_around_them() {
        let (t, kinds) = redact_text(
            "connect with `postgres://admin:Xk93jdLq0@db.internal:5432/app`. Then run it.",
        );
        assert_eq!(
            t,
            "connect with `postgres://admin:[REDACTED]@db.internal:5432/app`. Then run it."
        );
        assert_eq!(kinds, vec!["url-credentials"]);
        assert_eq!(text_secret_kind(&t), None, "redacted text passes the scan");
        assert_eq!(
            text_secret_kind("see https://u:Xk93jdLq0@x.dev/"),
            Some("url-credentials")
        );
        // A token used as the password is named after the token.
        let (t, kinds) =
            redact_text("https://x:ghp_FAKE0123456789abcdefghijklmnopqrstuv@github.com/o/r");
        assert_eq!(t, "https://x:[REDACTED:github-token]@github.com/o/r");
        assert_eq!(kinds, vec!["github-token"]);
    }

    #[test]
    fn free_text_assignments_to_secret_names_are_redacted() {
        for (text, want) in [
            (
                "export AWS_SECRET_ACCESS_KEY=wJalrXUtnFEMIK7MDENGbPxRfiCYEXAMPLEKEYU and password: hunter2hunterV.",
                "export AWS_SECRET_ACCESS_KEY=[REDACTED] and password: [REDACTED].",
            ),
            (
                "Bash(curl -H 'X-Api-Key: abcd1234efgh5678Q' https://api.x.example/*)",
                "Bash(curl -H 'X-Api-Key: [REDACTED]' https://api.x.example/*)",
            ),
            (
                "run with --token abc123def456",
                "run with --token [REDACTED]",
            ),
            (
                "Authorization: Bearer lowentropy42tok",
                "Authorization: Bearer [REDACTED]",
            ),
            (
                "DB_PASSWORD=\"s3cr3t-pw\" psql",
                "DB_PASSWORD=\"[REDACTED]\" psql",
            ),
        ] {
            let (t, kinds) = redact_text(text);
            assert_eq!(t, want, "{text}");
            assert!(!kinds.is_empty());
        }
        for keep in [
            "password = os.environ[\"DB_PASSWORD\"]",
            "token = get_token(42)",
            "api_key: ${API_KEY}",
            "API_KEY=$API_KEY",
            "set API_KEY=<your-key-1> first",
            "Set the token: it expires after 3600 seconds.",
            "password: changeme",
            "TOKEN_TTL=3600",
            "the token is stored in ~/.config/x/token1",
            "Use the Bearer scheme.",
        ] {
            assert_eq!(redact_text(keep), (keep.to_string(), vec![]), "{keep}");
        }
    }

    #[test]
    fn hex_only_path_tokens_are_redacted() {
        let tok32 = "3f9a8b7c6d5e4f3a2b1c0d9e8f7a6b5c";
        let tok40 = "0123456789abcdef0123456789abcdef01234567";
        for url in [
            format!("https://mcp.example.com/u/{tok32}/sse"),
            format!("https://mcp.example.com/{}/sse", tok32.to_ascii_uppercase()),
            // A 40-digit token outside a commit position.
            format!("https://hooks.example.com/notify/{tok40}"),
            // A 32-digit token can't be a full SHA, even after a commit word
            // or on a host that serves files by commit.
            format!("https://mcp.example.com/src/{tok32}/sse"),
            format!("https://mcp.example.com/raw/{tok32}/sse"),
            format!("https://raw.githubusercontent.com/o/r/{tok32}/server.js"),
        ] {
            let (v, found) = redact_url("s", &url);
            assert!(v.contains("${S_URL_TOKEN}"), "{url} -> {v}");
            assert!(!v.contains("3f9a8b") && !v.contains("3F9A8B") && !v.contains(tok40));
            assert_eq!(found, vec![("S_URL_TOKEN".to_string(), "url-path-token")]);
        }
        for url in [
            // One digit short of the threshold.
            format!("https://x.example/u/{}/sse", &tok32[..31]),
            // Short ids and short SHAs.
            "https://x.example/items/1a2b3c4d/v/deadbeef0123".to_string(),
            // A long numeric id.
            "https://x.example/n/12345678901234567890123456789012".to_string(),
            // Full commit SHAs where they name a commit.
            format!("https://gitlab.com/o/r/-/blob/{tok40}/server.js"),
            format!("https://github.com/o/r/tree/{tok40}/src"),
            format!("https://raw.githubusercontent.com/o/r/{tok40}/server.js"),
            format!("https://x.example/o/r/archive/{tok40}"),
            format!("https://x.example/o/r/commit/{}", &tok40.repeat(2)[..64]),
        ] {
            assert_eq!(redact_url("s", &url), (url.clone(), vec![]), "{url}");
        }
        // Free text keeps to known shapes: prose has too many ids for a guess.
        let prose = format!("See https://x.example/u/{tok32}/sse for details.");
        assert_eq!(redact_text(&prose).0, prose);
    }

    #[test]
    fn ordinary_url_paths_and_usernames_are_kept() {
        for url in [
            "https://api.example/v1/models/gpt-4o-mini-2024-07-18",
            "https://x.example/items/123e4567-e89b-12d3-a456-426614174000",
            "https://github.com/o/r/commit/0123456789abcdef0123456789abcdef01234567",
            "https://bob@example.com/repo.git",
            "wss://user@stream.example/feed",
            "https://hooks.slack.com/",
            "https://x.example/mcp?region=eu&page=2",
        ] {
            assert_eq!(redact_url("s", url), (url.to_string(), vec![]), "{url}");
        }
        let (v, found) = redact_url(
            "s",
            "https://discord.com/api/webhooks/123456789012345678/FAKEdiscordFAKEdiscordFAKEdiscordW",
        );
        assert_eq!(
            v,
            "https://discord.com/api/webhooks/123456789012345678/${S_URL_TOKEN}"
        );
        assert_eq!(found.len(), 1);
        let (v, found) = redact_url(
            "s",
            "https://hooks.slack.com/services/T0000FAKE/B0000FAKE/FAKE-FAKE-FAKE-FAKEFAKEFAKP",
        );
        assert_eq!(v, "https://hooks.slack.com/services/${S_URL_TOKEN}");
        assert_eq!(found.len(), 1);
        let (t, _) = redact_text("see https://hooks.slack.com/services/T0/B0/FAKEFAKE.");
        assert_eq!(t, "see https://hooks.slack.com/services/[REDACTED].");
    }

    #[test]
    fn recognises_the_redacted_form_of_a_text() {
        let original = "a ghp_FAKE0123456789abcdefghijklmnopqrstuv b https://u:Xk93jdLq0@h c";
        let (red, _) = redact_text(original);
        assert!(has_redaction_marker(&red));
        assert!(is_redacted_form_of(&red, original));
        assert!(!is_redacted_form_of(&red, "a b https://u:@h c"));
        assert!(!is_redacted_form_of(&red, &format!("{original} and more")));
        assert!(!is_redacted_form_of(&red, &original.replace(" b ", " B ")));
        assert!(is_redacted_form_of("same", "same"));
        assert!(!is_redacted_form_of("[REDACTED]", ""));
        assert!(is_redacted_form_of("[REDACTED:x][REDACTED]", "ab"));
        assert!(!is_redacted_form_of("[REDACTED:x][REDACTED]", "a"));
        assert!(is_redacted_form_of("é[REDACTED]ü", "éßü"));
    }

    #[test]
    fn documentation_placeholder_passwords_are_not_secrets() {
        for doc in [
            "e.g. `postgres://user:pass@host/db`. This is desired state, ...",
            "mysql://root:password@localhost/app",
            "redis://default:secret@cache:6379",
            "amqp://guest:changeme@mq",
            "https://user:xxx@example.com",
            "https://user:***@example.com",
            "postgres://u:<password>@h/db",
            "postgres://u:${DB_PASSWORD}@h/db",
            "postgres://u:$DB_PASSWORD@h/db",
            "postgres://u:{password}@h/db",
            "postgres://u:YOUR_PASSWORD@h/db",
        ] {
            assert_eq!(redact_text(doc), (doc.to_string(), vec![]), "{doc}");
            assert_eq!(text_secret_kind(doc), None, "{doc}");
        }
    }

    #[test]
    fn masks_secret_looking_config_lines() {
        assert_eq!(
            mask_line(r#"      "GITHUB_TOKEN": "ghp_FAKE0123456789abcdefghijklmnopqrstuv","#),
            r#"      "GITHUB_TOKEN": "••••••••","#
        );
        assert_eq!(
            mask_line(r#"  "DB_PASSWORD": "p@ss\"w0rd","#),
            r#"  "DB_PASSWORD": "••••••••","#
        );
        assert_eq!(
            mask_line(r#"env = { API_KEY = "abcdef123456" }"#),
            r#"env = { API_KEY = "••••••••" }"#
        );
        assert_eq!(
            mask_line(r#""args": ["--password", "OldSecretValue1", "--x=1"]"#),
            r#""args": ["--password", "••••••••", "--x=1"]"#
        );
        assert_eq!(
            mask_line(r#""args": ["--token=abcdef123"]"#),
            r#""args": ["--token=••••••••"]"#
        );
        assert_eq!(
            mask_line(r#""url": "postgres://u:hunter22@db/app", "x": 1"#),
            r#""url": "postgres://u:••••••••@db/app", "x": 1"#
        );
        for keep in [
            r#""command": "npx","#,
            r#"env = { API_KEY = "${API_KEY}" }"#,
            r#""LOG_LEVEL": "debug""#,
        ] {
            assert_eq!(mask_line(keep), keep);
        }
    }

    /// Anything capture would redact from text is masked in a diff too.
    #[test]
    fn masks_what_capture_would_redact() {
        for (line, gone) in [
            ("export DB_PASSWORD=hunter22x", "hunter22x"),
            (
                "curl -H 'Authorization: Bearer abcdef123456'",
                "abcdef123456",
            ),
            (
                "see https://hooks.slack.com/services/T0FAKE/B0FAKE/FakeFakeFake123",
                "FakeFakeFake123",
            ),
            (
                "https://api.example.com/v1?api_key=fake123abc456",
                "fake123abc456",
            ),
            // Under a secret name, however short, as capture redacts it.
            (r#"  "PASSWORD": "rootE","#, "rootE"),
            (r#"token = "a1b2c""#, "a1b2c"),
        ] {
            let masked = mask_line(line);
            assert!(!masked.contains(gone), "{line} -> {masked}");
            assert!(masked.contains("••••••••"), "{line} -> {masked}");
        }
        for keep in [
            "Run `cargo test` before you push.",
            "export LOG_LEVEL=debug",
            r#"  "PASSWORD": "${DB_PASSWORD}","#,
            r#"  "AUTH_TIMEOUT": "30","#,
            r#"  "PASSWORD_FILE": "/run/secrets/db","#,
            "token: [REDACTED]",
        ] {
            assert_eq!(mask_line(keep), keep);
        }
    }

    /// A random-looking value is masked whatever it is called, as capture
    /// redacts it from an MCP server; ordinary ids stay readable.
    #[test]
    fn masks_random_looking_values_under_ordinary_names() {
        for (line, gone) in [
            (
                r#""PUBLIC_ID": "q8Zr2LkP0vXa7MnB4cTy9WdE""#,
                "q8Zr2LkP0vXa7MnB4cTy9WdE",
            ),
            (
                r#"CLIENT_REF = "q8Zr2LkP0vXa7MnB4cTy9WdE""#,
                "q8Zr2LkP0vXa7MnB4cTy9WdE",
            ),
            (
                r#""args": ["-y", "svc", "Xk29fLq0Zp8RwT4vNm7Bc1Yd"]"#,
                "Xk29fLq0Zp8RwT4vNm7Bc1Yd",
            ),
            (
                r#""args": ["--client-ref=Xk29fLq0Zp8RwT4vNm7Bc1Yd"]"#,
                "Xk29fLq0Zp8RwT4vNm7Bc1Yd",
            ),
            (
                r#"      "Xk29fLq0Zp8RwT4vNm7Bc1Yd""#,
                "Xk29fLq0Zp8RwT4vNm7Bc1Yd",
            ),
        ] {
            let masked = mask_line(line);
            assert!(!masked.contains(gone), "{line} -> {masked}");
            assert!(masked.contains("••••••••"), "{line} -> {masked}");
        }
        assert_eq!(
            mask_line(r#""args": ["--client-ref=Xk29fLq0Zp8RwT4vNm7Bc1Yd"]"#),
            r#""args": ["--client-ref=••••••••"]"#
        );
        // Ordinary values, including long ids and names, are shown as they are.
        for keep in [
            r#""model": "claude-sonnet-4-5-20250929","#,
            r#""allow": ["mcp__github__createPullRequest"],"#,
            r#""args": ["-y", "@modelcontextprotocol/server-github"],"#,
            r#""id": "123e4567-e89b-12d3-a456-426614174000","#,
            r#""rev": "0123456789abcdef0123456789abcdef01234567","#,
            r#""cwd": "/home/me/src/kitbag_2025_release_branch","#,
            r#"description = 'Search the docs, then answer'"#,
            "token-like words in prose such as Xk29fLq0Zp8RwT4vNm7Bc1Yd are left",
        ] {
            assert_eq!(mask_line(keep), keep);
        }
    }

    /// TOML literal strings (`'...'`) are checked like basic strings.
    #[test]
    fn masks_single_quoted_toml_values() {
        assert_eq!(
            mask_line("API_KEY = 'abcdef123456'"),
            "API_KEY = '••••••••'"
        );
        assert_eq!(
            mask_line("env = { DB_PASSWORD = 'rootE', LOG_LEVEL = 'debug' }"),
            "env = { DB_PASSWORD = '••••••••', LOG_LEVEL = 'debug' }"
        );
        assert_eq!(
            mask_line("'API_TOKEN' = 'p\\w0rd1'"),
            "'API_TOKEN' = '••••••••'"
        );
        assert_eq!(
            mask_line("args = ['--token', 'hunter22x', '--port', '8080']"),
            "args = ['--token', '••••••••', '--port', '8080']"
        );
        assert_eq!(
            mask_line("args = ['--password=hunter22x']"),
            "args = ['--password=••••••••']"
        );
        assert_eq!(
            mask_line("bearer_token = 'ghp_FAKE0123456789abcdefghijklmnopqrstuv'"),
            "bearer_token = '••••••••'"
        );
        for keep in [
            "API_KEY = '${API_KEY}'",
            "SESSION_TIMEOUT = '3600'",
            "command = 'npx'",
            "args = ['--token', '--verbose']",
        ] {
            assert_eq!(mask_line(keep), keep);
        }
    }

    /// `"--token",` at the end of one line and its value on the next, as in a
    /// pretty-printed `args` array.
    #[test]
    fn masks_flag_values_on_the_next_line() {
        let text = "{\n  \"args\": [\n    \"--api-key\",\n    \"hunter22x\",\n\n    '--password',\n    'rootE'\n    \"--port\",\n    \"8080\",\n    \"--token\",\n    \"--verbose\"\n  ]\n}\n";
        let lines: Vec<&str> = text.split_inclusive('\n').collect();
        let flags = pending_flags(lines.iter().copied());
        assert_eq!(flags.len(), lines.len());
        let shown: String = lines
            .iter()
            .zip(&flags)
            .map(|(l, f)| mask_line_after(l, f.as_deref()))
            .collect();
        assert!(!shown.contains("hunter22x"), "{shown}");
        assert!(!shown.contains("rootE"), "{shown}");
        assert!(shown.contains("    \"••••••••\",\n"), "{shown}");
        assert!(shown.contains("    '••••••••'\n"), "{shown}");
        // Not a secret flag, and a flag is never taken for a value.
        assert!(shown.contains("\"8080\""), "{shown}");
        assert!(shown.contains("\"--verbose\""), "{shown}");
        assert_eq!(flags[3].as_deref(), Some("--api-key"));
        assert_eq!(flags[6].as_deref(), Some("--password"), "{flags:?}");
        assert_eq!(flags[1], None);
        // CRLF line endings.
        let crlf = "[\r\n  \"--token\",\r\n  \"hunter22x\"\r\n]\r\n";
        let lines: Vec<&str> = crlf.split_inclusive('\n').collect();
        let flags = pending_flags(lines.iter().copied());
        assert_eq!(
            mask_line_after(lines[2], flags[2].as_deref()),
            "  \"••••••••\"\r\n"
        );
        // Without a flag before it, a short value is shown.
        assert_eq!(
            mask_line_after("    \"hunter22x\",", None),
            "    \"hunter22x\","
        );
    }

    #[test]
    fn masks_private_key_blocks_across_lines() {
        let key = "-----BEGIN OPENSSH PRIVATE KEY-----\nFAKEb3BlbnNzaC1rZXktdjEAAAAA\nFAKEAAAAAAAAAAEAAAAzAAAAC3Nz\n-----END OPENSSH PRIVATE KEY-----";
        let text = format!("before\n{key}\nafter\n");
        let masked = mask_key_blocks(&text);
        assert!(!masked.contains("FAKE"), "{masked}");
        assert!(masked.starts_with("before\n•••••••• (private key, sha256:"));
        assert!(masked.ends_with(")\nafter\n"), "{masked}");
        assert_eq!(masked.lines().count(), 3);
        // A different key gets a different tag, so the diff shows the change.
        let other = mask_key_blocks(&text.replace("FAKEb3", "FAKEc4"));
        assert_ne!(masked, other);
        // Escaped on one line, as in JSON.
        let json = format!("\"key\": \"{}\",", key.replace('\n', "\\n"));
        let masked = mask_key_blocks(&json);
        assert!(
            !masked.contains("FAKE") && masked.ends_with("\","),
            "{masked}"
        );
        // Without an END line: the key-like lines after BEGIN, not the prose.
        let cut = "-----BEGIN RSA PRIVATE KEY-----\r\nProc-Type: 4,ENCRYPTED\r\n\r\nFAKEMIIEpAIBAAKCAQEA0Z3VS5JJcds3xf\r\n  FAKEn/ygWyF8PbnGy0AHB7MhgHcTz6sE2I2y\r\n\r\nThe end.\r\n";
        let masked = mask_key_blocks(cut);
        assert!(!masked.contains("FAKE"), "{masked}");
        assert!(masked.ends_with(")\r\n\r\nThe end.\r\n"), "{masked:?}");
        let plain = "No keys here: -----BEGIN PUBLIC KEY----- is fine.\n";
        assert_eq!(mask_key_blocks(plain), plain);
    }

    #[test]
    fn masking() {
        let secrets: BTreeSet<String> = ["s3cr3t-value".to_string()].into_iter().collect();
        assert_eq!(mask("k = s3cr3t-value", &secrets), "k = ••••••••");
    }

    #[test]
    fn env_names() {
        assert_eq!(env_var_name(&["github", "apiKey"]), "GITHUB_API_KEY");
        assert_eq!(env_var_name(&["my-svc", "X-Api-Key"]), "MY_SVC_X_API_KEY");
        assert_eq!(env_var_name(&["9lives"]), "_9LIVES");
    }
}
