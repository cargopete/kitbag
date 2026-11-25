//! Secret handling.
//!
//! Model: a profile never contains secret values. Secrets are represented as
//! `${ENV_VAR}` references (optionally `${ENV_VAR:-default}` for non-secret
//! defaults). On capture, literal values that look like secrets are replaced
//! with references. On apply, references are written in the target's native
//! interpolation syntax so the secret never touches disk, or, only for
//! targets without interpolation support, resolved from the environment.

use std::collections::{BTreeSet, HashMap};
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

/// How references are written for a target.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RefStyle {
    /// `${VAR}` and `${VAR:-default}` kept as-is (Claude Code).
    Dollar,
    /// `${VAR}` without default support; defaults are resolved now (Gemini CLI).
    DollarPlain,
    /// `${env:VAR}`; defaults are resolved now (Cursor, VS Code).
    EnvColon,
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
            match style {
                RefStyle::EnvColon | RefStyle::DollarPlain => match (fallback, set) {
                    (Some(fb), None) => fb.to_string(),
                    _ if style == RefStyle::EnvColon => format!("${{env:{name}}}"),
                    _ => reference(name),
                },
                _ => match (set, fallback) {
                    (Some(v), _) => {
                        if v.len() >= 4 {
                            state.resolved.insert(v.clone());
                        }
                        v.clone()
                    }
                    (None, Some(fb)) => fb.to_string(),
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
        .find(|p| p.re.find_iter(value).any(|m| !has_ref(m.as_str())))
        .map(|p| p.kind)
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
pub fn classify(name: Option<&str>, value: &str) -> Option<&'static str> {
    if value.is_empty() || sole_ref(value).is_some() {
        return None;
    }
    if let Some(k) = strong_secret_kind(value) {
        return Some(k);
    }
    // `Bearer <token>` / `Basic <creds>` is a credential whatever it is called.
    if let Some(c) = BEARER.captures(value)
        && !has_ref(&c[2])
        && c[2].len() >= 6
    {
        return Some("authorization-header");
    }
    // Under a secret-sounding name any value counts, however short (`rootE`).
    if name.is_some_and(is_secret_name) && !has_ref(value) && !obviously_not_secret(value) {
        return Some("secret-named-field");
    }
    if looks_random_token(value) {
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

/// Replace secret-looking values in an MCP server definition with env references.
pub fn redact_server(server_name: &str, input: &McpServer) -> (McpServer, Vec<Redaction>) {
    let mut s = input.clone();
    let mut out = Vec::new();
    let loc = format!("mcp_servers.{server_name}");

    // `${API_KEY:-literal}`: the literal is written wherever API_KEY is unset.
    let locations: Vec<String> = s.strings().into_iter().map(|(l, _)| l).collect();
    for l in locations {
        s.map_string_at(&l, |v| {
            REF_RE
                .replace_all(v, |c: &Captures| match c.get(2) {
                    Some(fb) if classify(Some(&c[1]), fb.as_str()).is_some() => {
                        out.push(Redaction {
                            location: format!("{loc}.{l}"),
                            env_var: c[1].to_string(),
                            reason: "secret-fallback",
                            fingerprint: None,
                        });
                        reference(&c[1])
                    }
                    _ => c[0].to_string(),
                })
                .into_owned()
        });
    }

    for (k, v) in s.env.iter_mut() {
        if let Some(reason) = classify(Some(k), v) {
            let env_var = env_var_name(&[k]);
            *v = reference(&env_var);
            out.push(Redaction {
                location: format!("{loc}.env.{k}"),
                env_var,
                reason,
                fingerprint: None,
            });
        } else if URL_LIKE.is_match(v) {
            // `API_BASE = "https://x/v1?token=..."`: keep the URL, lose the token.
            let (value, found) = redact_url(server_name, v);
            *v = value;
            for (env_var, reason) in found {
                out.push(Redaction {
                    location: format!("{loc}.env.{k}"),
                    env_var,
                    reason,
                    fingerprint: None,
                });
            }
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
        let reason = classify(Some(header_key(k)), &raw)
            .or_else(|| (scheme.is_some() && !has_ref(&raw)).then_some("authorization-header"));
        if let Some(reason) = reason {
            let part = if k.eq_ignore_ascii_case("authorization") {
                "token"
            } else {
                k.as_str()
            };
            let env_var = env_var_name(&[server_name, part]);
            *v = match scheme {
                Some(sc) => format!("{sc} {}", reference(&env_var)),
                None => reference(&env_var),
            };
            out.push(Redaction {
                location: format!("{loc}.headers.{k}"),
                env_var,
                reason,
                fingerprint: None,
            });
        }
    }

    let mut n = 0;
    for i in 0..s.args.len() {
        let a = s.args[i].clone();
        if let Some(c) = FLAG_EQ.captures(&a) {
            // `--header=Authorization: Bearer ...`
            match redact_header_arg(server_name, &c[2]) {
                HeaderArg::Redacted(new, env_var, reason) => {
                    s.args[i] = format!("{}={new}", &c[1]);
                    out.push(Redaction {
                        location: format!("{loc}.args[{i}]"),
                        env_var,
                        reason,
                        fingerprint: None,
                    });
                    continue;
                }
                HeaderArg::Referenced => continue,
                HeaderArg::NotHeader => {}
            }
            let flag = c[1].trim_start_matches('-').to_string();
            if URL_LIKE.is_match(&c[2]) && !is_secret_name(&flag) {
                // `--endpoint=https://host/sse?api_key=...`
                let (value, found) = redact_url(server_name, &c[2]);
                s.args[i] = format!("{}={value}", &c[1]);
                for (env_var, reason) in found {
                    out.push(Redaction {
                        location: format!("{loc}.args[{i}]"),
                        env_var,
                        reason,
                        fingerprint: None,
                    });
                }
                continue;
            }
            if let Some(reason) = classify(Some(&flag), &c[2]) {
                let env_var = env_var_name(&[server_name, &flag]);
                s.args[i] = format!("{}={}", &c[1], reference(&env_var));
                out.push(Redaction {
                    location: format!("{loc}.args[{i}]"),
                    env_var,
                    reason,
                    fingerprint: None,
                });
            }
            continue;
        }
        if a.starts_with('-') {
            continue;
        }
        // `NAME=value` (docker -e, env-style wrappers).
        if let Some(c) = ASSIGN.captures(&a) {
            if let Some(reason) = classify(Some(&c[1]), &c[2]) {
                let env_var = env_var_name(&[&c[1]]);
                s.args[i] = format!("{}={}", &c[1], reference(&env_var));
                out.push(Redaction {
                    location: format!("{loc}.args[{i}]"),
                    env_var,
                    reason,
                    fingerprint: None,
                });
            } else if URL_LIKE.is_match(&c[2]) {
                let (value, found) = redact_url(server_name, &c[2]);
                s.args[i] = format!("{}={value}", &c[1]);
                for (env_var, reason) in found {
                    out.push(Redaction {
                        location: format!("{loc}.args[{i}]"),
                        env_var,
                        reason,
                        fingerprint: None,
                    });
                }
            }
            continue;
        }
        // `Header-Name: value` (mcp-remote --header, curl-style wrappers).
        if !URL_LIKE.is_match(&a) {
            match redact_header_arg(server_name, &a) {
                HeaderArg::Redacted(new, env_var, reason) => {
                    s.args[i] = new;
                    out.push(Redaction {
                        location: format!("{loc}.args[{i}]"),
                        env_var,
                        reason,
                        fingerprint: None,
                    });
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
            let (value, found) = redact_url(server_name, &a);
            if !found.is_empty() {
                s.args[i] = value;
                for (env_var, reason) in found {
                    out.push(Redaction {
                        location: format!("{loc}.args[{i}]"),
                        env_var,
                        reason,
                        fingerprint: None,
                    });
                }
                continue;
            }
        }
        let prev_flag = (i > 0)
            .then(|| FLAG.captures(&s.args[i - 1]).map(|c| c[1].to_string()))
            .flatten();
        if let Some(reason) = classify(prev_flag.as_deref(), &a) {
            let env_var = match &prev_flag {
                Some(f) => env_var_name(&[server_name, f]),
                None => {
                    n += 1;
                    env_var_name(&[server_name, &format!("secret_{n}")])
                }
            };
            s.args[i] = reference(&env_var);
            out.push(Redaction {
                location: format!("{loc}.args[{i}]"),
                env_var,
                reason,
                fingerprint: None,
            });
        }
    }

    if let Some(url) = s.url.clone() {
        let (value, found) = redact_url(server_name, &url);
        if !found.is_empty() {
            s.url = Some(value);
            for (env_var, reason) in found {
                out.push(Redaction {
                    location: format!("{loc}.url"),
                    env_var,
                    reason,
                    fingerprint: None,
                });
            }
        }
    }

    if let Some(cmd) = s.command.clone()
        && let Some(kind) = strong_secret_kind(&cmd)
    {
        let env_var = env_var_name(&[server_name, "command_secret"]);
        let p = STRONG.iter().find(|p| p.kind == kind).expect("kind exists");
        s.command = Some(p.re.replace_all(&cmd, reference(&env_var)).into_owned());
        out.push(Redaction {
            location: format!("{loc}.command"),
            env_var,
            reason: kind,
            fingerprint: None,
        });
    }

    (s, out)
}

/// What [`redact_header_arg`] made of an argument.
enum HeaderArg {
    /// Not `Header-Name: value`, or a header whose value isn't a secret.
    NotHeader,
    /// A header whose value already uses an env reference.
    Referenced,
    /// (`Name: ${VAR}`, VAR, reason).
    Redacted(String, String, &'static str),
}

/// `Name: value` with a secret value -> `Name: ${VAR}`.
fn redact_header_arg(server_name: &str, arg: &str) -> HeaderArg {
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
    if has_ref(&raw) {
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
    let env_var = env_var_name(&[server_name, part]);
    let v = match scheme {
        Some(sc) => format!("{sc} {}", reference(&env_var)),
        None => reference(&env_var),
    };
    // Keep the original separator: mcp-remote documents `Name:value` without a space.
    HeaderArg::Redacted(format!("{name}{sep}{v}"), env_var, reason)
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
        if known || strong_secret_kind(seg).is_some() || random {
            out.push(i);
        }
    }
    out
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

fn redact_url(server_name: &str, url: &str) -> (String, Vec<(String, &'static str)>) {
    let mut found = Vec::new();
    let value = URL_USERINFO
        .replace(url, |c: &Captures| {
            if has_ref(&c[3]) {
                return c[0].to_string();
            }
            let env_var = env_var_name(&[server_name, "password"]);
            let r = format!("{}{}:{}@", &c[1], &c[2], reference(&env_var));
            found.push((env_var, "url-credentials"));
            r
        })
        .into_owned();
    let value = URL_TOKEN_USER
        .replace(&value, |c: &Captures| {
            if !token_username(&c[2]) {
                return c[0].to_string();
            }
            let env_var = env_var_name(&[server_name, "url_user_token"]);
            let r = format!("{}{}@", &c[1], reference(&env_var));
            found.push((env_var, "url-username-token"));
            r
        })
        .into_owned();
    let value = match split_url(&value) {
        Some((head, path, rest)) => {
            let host = head.split("://").nth(1).unwrap_or("");
            let mut segs: Vec<String> = path.split('/').skip(1).map(str::to_string).collect();
            let view: Vec<&str> = segs.iter().map(String::as_str).collect();
            let hits = secret_path_segments(host, &view, true);
            for (n, i) in hits.iter().enumerate() {
                let suffix = if n == 0 {
                    String::new()
                } else {
                    format!("_{}", n + 1)
                };
                let env_var = env_var_name(&[server_name, &format!("url_token{suffix}")]);
                segs[*i] = reference(&env_var);
                found.push((env_var, "url-path-token"));
            }
            if hits.is_empty() {
                value.clone()
            } else {
                let path: String = segs.iter().map(|s| format!("/{s}")).collect();
                format!("{head}{path}{rest}")
            }
        }
        None => value,
    };
    let value = URL_QUERY
        .replace_all(&value, |c: &Captures| {
            let decoded = percent_decode(&c[3]);
            if has_ref(&decoded) {
                return c[0].to_string();
            }
            match classify(Some(&c[2]), &decoded) {
                Some(reason) => {
                    let env_var = env_var_name(&[server_name, &c[2]]);
                    let r = format!("{}{}={}", &c[1], &c[2], reference(&env_var));
                    found.push((env_var, reason));
                    r
                }
                None => c[0].to_string(),
            }
        })
        .into_owned();
    (value, found)
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
fn redact_text_urls(text: &str) -> (String, bool) {
    let mut hit = false;
    let out = TEXT_URL
        .replace_all(text, |c: &Captures| {
            let url = &c[0];
            let Some((head, path, rest)) = split_url(url) else {
                return url.to_string();
            };
            let host = head.split("://").nth(1).unwrap_or("");
            let mut segs: Vec<String> = path.split('/').skip(1).map(str::to_string).collect();
            let view: Vec<&str> = segs.iter().map(String::as_str).collect();
            for i in secret_path_segments(host, &view, false) {
                segs[i] = REDACTED_PASSWORD.to_string();
                hit = true;
            }
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
                    format!("{}{}={REDACTED_PASSWORD}", &q[1], &q[2])
                } else {
                    q[0].to_string()
                }
            });
            format!("{head}{path}{rest}")
        })
        .into_owned();
    (out, hit)
}

/// What a URL password is replaced with in free text.
pub const REDACTED_PASSWORD: &str = "[REDACTED]";

/// Passwords in documentation examples (`postgres://user:pass@host/db`,
/// `https://u:<password>@x`, `redis://:${REDIS_PASSWORD}@cache`) are not secrets.
fn placeholder_password(pw: &str) -> bool {
    let p = pw.trim();
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
    let mut kinds = Vec::new();
    let mut out = text.to_string();
    for p in STRONG.iter().filter(|p| p.kind != "url-credentials") {
        let mut hit = false;
        let replaced =
            p.re.replace_all(&out, |c: &Captures| {
                if has_ref(&c[0]) {
                    c[0].to_string()
                } else {
                    hit = true;
                    format!("[REDACTED:{}]", p.kind)
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
                format!("{}{REDACTED_PASSWORD}@", &c[1])
            }
        })
        .into_owned();
    if hit {
        kinds.push("url-credentials");
        out = replaced;
    }
    // Tokens in URL queries and webhook paths.
    let (replaced, hit) = redact_text_urls(&out);
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
                format!("{} {REDACTED_PASSWORD}", &c[1])
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
            out.replace_range(a..b, REDACTED_PASSWORD);
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

fn json_has_secret(v: &serde_json::Value, key: Option<&str>) -> bool {
    match v {
        serde_json::Value::String(s) => named_secret(key, s),
        serde_json::Value::Array(a) => a.iter().any(|x| json_has_secret(x, key)),
        serde_json::Value::Object(m) => m.iter().any(|(k, x)| json_has_secret(x, Some(k))),
        _ => false,
    }
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
    let ext = name.rsplit_once('.').map(|(_, e)| e).unwrap_or("");
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
    if ext == "json"
        && let Ok(v) = serde_json::from_str::<serde_json::Value>(&text)
        && json_has_secret(&v, None)
    {
        return Some("contains secret values".to_string());
    }
    if matches!(
        ext,
        "toml" | "yaml" | "yml" | "ini" | "cfg" | "conf" | "properties"
    ) {
        for line in text.lines() {
            if let Some((k, v)) = line.split_once(['=', ':']) {
                let v = v.trim().trim_matches(['"', '\'', ',']);
                if named_secret(Some(k.trim().trim_matches('"')), v) {
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
    Regex::new(r#"(^|[\s{,])([A-Za-z0-9_-]+|"[^"]+")(\s*=\s*)"((?:[^"\\]|\\.)*)""#)
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

/// For display: names and known formats only (no entropy guessing, which would
/// hide ordinary ids and make diffs unreviewable).
fn shown_secret(key: &str, value: &str) -> bool {
    if value.contains("••••••••") {
        return false; // already masked
    }
    let unescaped = value.replace("\\\"", "\"").replace("\\\\", "\\");
    named_secret(Some(key), &unescaped)
        || BEARER
            .captures(&unescaped)
            .is_some_and(|c| !has_ref(&c[2]) && c[2].len() >= 6)
}

/// Mask anything in one line of config text that looks like a credential:
/// known token formats anywhere, and secret-looking values of JSON / TOML keys.
/// Used for every line kitbag displays, so existing secrets in your files never
/// show up in a diff.
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
            if shown_secret(&c[1], &c[3]) {
                format!("\"{}\"{}\"••••••••\"", &c[1], &c[2])
            } else {
                c[0].to_string()
            }
        })
        .into_owned();
    // `"--password", "value"` in args arrays, and `"--token=value"`.
    out = FLAG_PAIR
        .replace_all(&out, |c: &Captures| {
            if shown_secret(c[1].trim_start_matches('-'), &c[3]) {
                format!("\"{}\"{}\"••••••••\"", &c[1], &c[2])
            } else {
                c[0].to_string()
            }
        })
        .into_owned();
    out = FLAG_ASSIGN
        .replace_all(&out, |c: &Captures| {
            if shown_secret(c[1].trim_start_matches('-'), &c[2]) {
                format!("\"{}=••••••••\"", &c[1])
            } else {
                c[0].to_string()
            }
        })
        .into_owned();
    TOML_PAIR
        .replace_all(&out, |c: &Captures| {
            if shown_secret(c[2].trim_matches('"'), &c[4]) {
                format!("{}{}{}\"••••••••\"", &c[1], &c[2], &c[3])
            } else {
                c[0].to_string()
            }
        })
        .into_owned()
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
        assert_eq!(r.args[8], "--header=Authorization: Bearer ${REMOTE_TOKEN}");
        assert_eq!(r.args[9], "postgresql://admin:${REMOTE_PASSWORD}@db/app");
        assert_eq!(red.len(), 6);
        let flat = r.args.join(" ");
        for leak in ["a8f3kd93", "hunter2", "s3cr3t", "abcabc", "lowentropy"] {
            assert!(!flat.contains(leak), "{leak} in {flat}");
        }
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
            r#""PUBLIC_ID": "q8Zr2LkP0vXa7MnB4cTy9WdE""#,
            r#""command": "npx","#,
            r#"env = { API_KEY = "${API_KEY}" }"#,
            r#""LOG_LEVEL": "debug""#,
        ] {
            assert_eq!(mask_line(keep), keep);
        }
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
