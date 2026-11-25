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
];

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
    // TOKEN_TTL, PASSWORD_MIN_LENGTH, AUTH_PORT: settings *about* a secret.
    if QUALIFIERS.contains(&w[last].as_str()) {
        return false;
    }
    w.iter().enumerate().any(|(i, word)| {
        SECRET_WORDS.contains(&word.as_str())
            || (word == "key" && i == last && (i == 0 || w[i - 1] != "public"))
    })
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
        ("anthropic-key", r"\bsk-ant-[A-Za-z0-9_-]{20,}"),
        ("openai-key", r"\bsk-(?:proj-|svcacct-|admin-)?[A-Za-z0-9_-]{20,}"),
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
            r"(?i)\b[a-z][a-z0-9+.-]*://[^\s:/@]+:[^\s@/]{3,}@\S+",
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
    matches!(
        v.to_ascii_lowercase().as_str(),
        "true" | "false" | "yes" | "no" | "on" | "off"
    ) || (URL_LIKE.is_match(v) && strong_secret_kind(v).is_none() && !URL_QUERY.is_match(v))
        || (PATH_LIKE.is_match(v) && !v.contains(char::is_whitespace))
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
    if name.is_some_and(is_secret_name)
        && value.chars().count() >= 6
        && !has_ref(value)
        && !obviously_not_secret(value)
    {
        return Some("secret-named-field");
    }
    if looks_random_token(value) {
        return Some("high-entropy-value");
    }
    None
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
static HEADER_ARG: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^([A-Za-z][A-Za-z0-9-]*):\s*([^\s/].*)$").expect("valid regex"));
static URL_USERINFO: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)^([a-z][a-z0-9+.-]*://)([^\s:/@]+):([^\s@/]+)@").expect("valid regex")
});
static URL_QUERY: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"([?&])([^=&#\s]+)=([^&#\s]*)").expect("valid regex"));

/// Replace secret-looking values in an MCP server definition with env references.
pub fn redact_server(server_name: &str, input: &McpServer) -> (McpServer, Vec<Redaction>) {
    let mut s = input.clone();
    let mut out = Vec::new();
    let loc = format!("mcp_servers.{server_name}");

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
        let reason = classify(Some(k), &raw)
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
            if let Some((new, env_var, reason)) = redact_header_arg(server_name, &c[2]) {
                s.args[i] = format!("{}={new}", &c[1]);
                out.push(Redaction {
                    location: format!("{loc}.args[{i}]"),
                    env_var,
                    reason,
                    fingerprint: None,
                });
                continue;
            }
            let flag = c[1].trim_start_matches('-').to_string();
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
            }
            continue;
        }
        // `Header-Name: value` (mcp-remote --header, curl-style wrappers).
        if !URL_LIKE.is_match(&a)
            && let Some((new, env_var, reason)) = redact_header_arg(server_name, &a)
        {
            s.args[i] = new;
            out.push(Redaction {
                location: format!("{loc}.args[{i}]"),
                env_var,
                reason,
                fingerprint: None,
            });
            continue;
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

/// `Name: value` with a secret value -> (`Name: ${VAR}`, VAR, reason).
fn redact_header_arg(server_name: &str, arg: &str) -> Option<(String, String, &'static str)> {
    let c = HEADER_ARG.captures(arg)?;
    let (name, value) = (c[1].to_string(), c[2].trim().to_string());
    let (scheme, raw) = match BEARER.captures(&value) {
        Some(b) => (Some(b[1].to_string()), b[2].to_string()),
        None => (None, value.clone()),
    };
    if has_ref(&raw) {
        return None;
    }
    let reason = classify(Some(&name), &raw).or(scheme.as_ref().map(|_| "authorization-header"))?;
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
    Some((format!("{name}: {v}"), env_var, reason))
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

/// Redact high-confidence secrets from free text (instructions, rules, commands, skills).
/// Returns the new text and the kinds of secret found.
pub fn redact_text(text: &str) -> (String, Vec<&'static str>) {
    let mut kinds = Vec::new();
    let mut out = text.to_string();
    for p in STRONG.iter() {
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
    (out, kinds)
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

/// Mask anything in one line of config text that looks like a credential:
/// known token formats anywhere, and secret-looking values of JSON / TOML keys.
/// Used for every line kitbag displays, so existing secrets in your files never
/// show up in a diff.
pub fn mask_line(line: &str) -> String {
    let mut out = line.to_string();
    for p in STRONG.iter() {
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
        .replace_all(&out, |c: &Captures| match classify(Some(&c[1]), &c[3]) {
            Some(_) => format!("\"{}\"{}\"••••••••\"", &c[1], &c[2]),
            None => c[0].to_string(),
        })
        .into_owned();
    TOML_PAIR
        .replace_all(&out, |c: &Captures| {
            match classify(Some(c[2].trim_matches('"')), &c[4]) {
                Some(_) => format!("{}{}{}\"••••••••\"", &c[1], &c[2], &c[3]),
                None => c[0].to_string(),
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
        ] {
            assert!(is_secret_name(n), "{n}");
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
        assert_eq!(r.args[7], "X-Api-Key: ${REMOTE_X_API_KEY}");
        assert_eq!(r.args[8], "--header=Authorization: Bearer ${REMOTE_TOKEN}");
        assert_eq!(r.args[9], "postgresql://admin:${REMOTE_PASSWORD}@db/app");
        assert_eq!(red.len(), 6);
        let flat = r.args.join(" ");
        for leak in ["a8f3kd93", "hunter2", "s3cr3t", "abcabc", "lowentropy"] {
            assert!(!flat.contains(leak), "{leak} in {flat}");
        }
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
        for keep in [
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
