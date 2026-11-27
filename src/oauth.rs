//! MCP OAuth client configuration, and the sign-in checklist.
//!
//! kitbag carries the *configuration* a remote server's OAuth sign-in needs:
//! the client id, scopes, callback port and the like, in the keys each tool's
//! own docs name. It never reads, writes or prints an access token, a refresh
//! token or a token store, never starts a sign-in, and never writes a literal
//! client secret: a secret is carried only as an env reference, and only to a
//! tool whose docs say it can read one there.
//!
//! What each tool documents (read 2026-11-27):
//!
//! - Claude Code, <https://code.claude.com/docs/en/mcp>: `oauth.clientId`,
//!   `oauth.callbackPort`, `oauth.scopes` (one space-separated string),
//!   `oauth.authServerMetadataUrl`. The client secret is kept in the system
//!   keychain or a credentials file, "not in your config". Sign in: `/mcp`, or
//!   `claude mcp login <name>`.
//! - Gemini CLI, <https://geminicli.com/docs/tools/mcp-server/>: `oauth.enabled`,
//!   `clientId`, `clientSecret`, `authorizationUrl`, `issuer`, `tokenUrl`,
//!   `scopes` (array), `redirectUri`, and `tokenParamName`, `audiences`,
//!   `authorizationResponseIssParameterSupported`, which kitbag leaves alone.
//!   Sign in: `/mcp auth <name>`.
//! - opencode, <https://opencode.ai/docs/mcp-servers/>: `oauth: false`, or an
//!   `oauth` object with `client_id`, `client_secret`, `scope`, `callback_port`,
//!   `redirect_uri`, `auth_server_metadata_url`; `{env:NAME}` references. A
//!   remote server uses OAuth unless `oauth` is `false`. Sign in:
//!   `opencode mcp auth <name>`.
//! - Codex, <https://learn.chatgpt.com/docs/config-file/config-reference>:
//!   `mcp_servers.<id>.oauth.client_id`, `oauth.callback_port`, `scopes`,
//!   `oauth_resource`. `oauth.client_secret` is "stored in configuration" as
//!   written, and no env reference is documented there, so it is not carried.
//!   Sign in: `codex mcp login <name>`.
//! - Cursor, <https://cursor.com/docs/context/mcp>: `auth.CLIENT_ID`,
//!   `auth.CLIENT_SECRET` (`${env:NAME}`), and `auth.scopes`, whose type the
//!   page doesn't say, so scopes are not carried. No sign-in command is named.
//! - Hermes Agent: `auth: oauth` and an `oauth` block with `client_id`,
//!   `client_secret` (`${VAR}` as its page shows), `redirect_uri`; sign in
//!   `hermes mcp login <name>`. Its `redirect_host`, `redirect_port`,
//!   `client_name`, `timeout` and `flow` are left as they are.
//!
//! Dated skips: GitHub Copilot's docs name none of the `oauth*` keys kitbag
//! leaves out of its entries; Zed documents an in-app prompt and no keys; Amp's
//! `amp.mcpServers` has no `oauth` keys (only `amp mcp oauth login` flags);
//! Claude Desktop's docs name none.

use serde_json::{Map, Value, json};

use crate::model::{McpOauth, McpServer, Profile, Transport};

/// A server that still needs a sign-in on a tool, with the tool's own command.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SignIn {
    pub target: String,
    pub server: String,
    pub command: String,
}

/// The tools that read OAuth configuration from a server entry.
pub fn carries(target: &str) -> bool {
    matches!(
        target,
        "claude-code" | "gemini-cli" | "opencode" | "codex" | "cursor" | "hermes"
    )
}

/// The documented command that signs in to server `name` on `target`.
pub fn signin_command(target: &str, name: &str) -> Option<String> {
    Some(match target {
        "claude-code" => format!("claude mcp login {name}"),
        "gemini-cli" => format!("/mcp auth {name}"),
        "opencode" => format!("opencode mcp auth {name}"),
        "codex" => format!("codex mcp login {name}"),
        "hermes" => format!("hermes mcp login {name}"),
        _ => return None,
    })
}

/// Servers of `profile` that still need a sign-in on `target`: those the
/// profile gives OAuth configuration (or marks as signing in with OAuth), and,
/// on opencode, remote servers without header credentials, which its docs say
/// use OAuth unless `oauth` is `false`. Never a local server, and not every
/// remote one.
pub fn checklist(profile: &Profile, target: &str) -> Vec<SignIn> {
    let mut out = Vec::new();
    for (name, s) in &profile.mcp_servers {
        if !s.applies_to(target) || s.disabled || s.transport() == Transport::Stdio {
            continue;
        }
        let flagged = match &s.oauth {
            Some(o) => o.is_on(),
            None => target == "opencode" && s.headers.is_empty(),
        };
        if !flagged {
            continue;
        }
        if let Some(command) = signin_command(target, name) {
            out.push(SignIn {
                target: target.to_string(),
                server: name.clone(),
                command,
            });
        }
    }
    out
}

/// What [`write()`] made of a server's OAuth configuration for one tool.
#[derive(Debug, Default)]
pub struct Written {
    /// The keys to put in the server entry (`oauth`, or `auth`, `scopes`...).
    pub keys: Map<String, Value>,
    /// Settings the tool has no documented key for, by their profile names.
    pub unsupported: Vec<&'static str>,
    /// `client_secret` was in the profile but isn't written to this tool.
    pub secret_not_written: bool,
}

fn put(m: &mut Map<String, Value>, k: &str, v: &Option<String>) {
    if let Some(v) = v {
        m.insert(k.into(), json!(v));
    }
}

fn is_set(o: &McpOauth, field: &str) -> bool {
    match field {
        "enabled" => o.enabled == Some(false),
        "client_id" => o.client_id.is_some(),
        "client_secret" => o.client_secret.is_some(),
        "scopes" => !o.scopes.is_empty(),
        "callback_port" => o.callback_port.is_some(),
        "redirect_uri" => o.redirect_uri.is_some(),
        "auth_server_metadata_url" => o.auth_server_metadata_url.is_some(),
        "authorization_url" => o.authorization_url.is_some(),
        "token_url" => o.token_url.is_some(),
        "issuer" => o.issuer.is_some(),
        "resource" => o.resource.is_some(),
        _ => false,
    }
}

const FIELDS: &[&str] = &[
    "enabled",
    "client_id",
    "client_secret",
    "scopes",
    "callback_port",
    "redirect_uri",
    "auth_server_metadata_url",
    "authorization_url",
    "token_url",
    "issuer",
    "resource",
];

/// Is the profile's client secret a lone `${VAR}` without a fallback? Only
/// that is ever written to a tool.
pub fn secret_is_ref(o: &McpOauth) -> bool {
    safe_secret(o).is_some()
}

/// The client secret as an env reference the tool may keep, or `None`: only a
/// single `${VAR}` without a fallback is ever written, whatever the profile says.
fn safe_secret(o: &McpOauth) -> Option<&String> {
    o.client_secret
        .as_ref()
        .filter(|s| crate::secrets::sole_ref(s).is_some_and(|r| r.fallback.is_none()))
}

/// `o` in `target`'s own keys. `o`'s strings are already rendered in the
/// tool's reference style (see `render_server`), which is how `${VAR}` reaches
/// a tool as `{env:VAR}` or `${env:VAR}`. `secret_is_ref` says the profile's
/// client secret is a lone env reference (see [`secret_is_ref`]); without it
/// no secret is written, whatever `o` holds.
pub fn write(target: &str, o: &McpOauth, secret_is_ref: bool) -> Written {
    let mut w = Written::default();
    // The secret goes only where it is documented to live in the entry, and
    // only as a reference.
    let secret_ok = secret_is_ref && o.client_secret.is_some();
    let mut handled: Vec<&str> = Vec::new();
    match target {
        "claude-code" => {
            let mut m = Map::new();
            put(&mut m, "clientId", &o.client_id);
            if let Some(p) = o.callback_port {
                m.insert("callbackPort".into(), json!(p));
            }
            if !o.scopes.is_empty() {
                m.insert("scopes".into(), json!(o.scopes.join(" ")));
            }
            put(&mut m, "authServerMetadataUrl", &o.auth_server_metadata_url);
            handled.extend([
                "client_id",
                "callback_port",
                "scopes",
                "auth_server_metadata_url",
            ]);
            if !m.is_empty() {
                w.keys.insert("oauth".into(), Value::Object(m));
            }
        }
        "gemini-cli" => {
            let mut m = Map::new();
            if let Some(e) = o.enabled {
                m.insert("enabled".into(), json!(e));
            }
            put(&mut m, "clientId", &o.client_id);
            if secret_ok {
                put(&mut m, "clientSecret", &o.client_secret);
            }
            put(&mut m, "authorizationUrl", &o.authorization_url);
            put(&mut m, "issuer", &o.issuer);
            put(&mut m, "tokenUrl", &o.token_url);
            if !o.scopes.is_empty() {
                m.insert("scopes".into(), json!(o.scopes));
            }
            put(&mut m, "redirectUri", &o.redirect_uri);
            handled.extend([
                "enabled",
                "client_id",
                "client_secret",
                "authorization_url",
                "issuer",
                "token_url",
                "scopes",
                "redirect_uri",
            ]);
            if !m.is_empty() {
                w.keys.insert("oauth".into(), Value::Object(m));
            }
        }
        "opencode" => {
            handled.extend([
                "enabled",
                "client_id",
                "client_secret",
                "scopes",
                "callback_port",
                "redirect_uri",
                "auth_server_metadata_url",
            ]);
            let mut m = Map::new();
            put(&mut m, "client_id", &o.client_id);
            if secret_ok {
                put(&mut m, "client_secret", &o.client_secret);
            }
            if !o.scopes.is_empty() {
                m.insert("scope".into(), json!(o.scopes.join(" ")));
            }
            if let Some(p) = o.callback_port {
                m.insert("callback_port".into(), json!(p));
            }
            put(&mut m, "redirect_uri", &o.redirect_uri);
            put(
                &mut m,
                "auth_server_metadata_url",
                &o.auth_server_metadata_url,
            );
            if o.enabled == Some(false) {
                w.keys.insert("oauth".into(), json!(false));
            } else if !m.is_empty() {
                w.keys.insert("oauth".into(), Value::Object(m));
            }
        }
        "codex" => {
            handled.extend(["client_id", "callback_port", "scopes", "resource"]);
            let mut m = Map::new();
            put(&mut m, "client_id", &o.client_id);
            if let Some(p) = o.callback_port {
                m.insert("callback_port".into(), json!(p));
            }
            if !m.is_empty() {
                w.keys.insert("oauth".into(), Value::Object(m));
            }
            if !o.scopes.is_empty() {
                w.keys.insert("scopes".into(), json!(o.scopes));
            }
            put(&mut w.keys, "oauth_resource", &o.resource);
        }
        "hermes" => {
            handled.extend(["enabled", "client_id", "client_secret", "redirect_uri"]);
            let mut m = Map::new();
            put(&mut m, "client_id", &o.client_id);
            if secret_ok {
                put(&mut m, "client_secret", &o.client_secret);
            }
            put(&mut m, "redirect_uri", &o.redirect_uri);
            if o.enabled != Some(false) {
                w.keys.insert("auth".into(), json!("oauth"));
            }
            if !m.is_empty() && o.enabled != Some(false) {
                w.keys.insert("oauth".into(), Value::Object(m));
            }
        }
        "cursor" => {
            handled.extend(["client_id", "client_secret"]);
            let mut m = Map::new();
            put(&mut m, "CLIENT_ID", &o.client_id);
            if secret_ok {
                put(&mut m, "CLIENT_SECRET", &o.client_secret);
            }
            if !m.is_empty() {
                w.keys.insert("auth".into(), Value::Object(m));
            }
        }
        _ => {}
    }
    // Only `enabled = false` is a setting worth naming; `true` is the default.
    w.unsupported = FIELDS
        .iter()
        .copied()
        .filter(|f| *f != "client_secret" && is_set(o, f) && !handled.contains(f))
        .collect();
    w.secret_not_written =
        o.client_secret.is_some() && !(secret_ok && handled.contains(&"client_secret"));
    w
}

/// The names of the keys a tool keeps its OAuth configuration under (so the
/// merge knows which part of an entry is OAuth).
pub const KEYS: &[&str] = &["oauth", "auth", "scopes", "oauth_resource"];

fn s(m: &Map<String, Value>, k: &str) -> Option<String> {
    match m.get(k) {
        Some(Value::String(v)) if !v.is_empty() => Some(v.clone()),
        _ => None,
    }
}

fn port(m: &Map<String, Value>, k: &str) -> Option<u16> {
    m.get(k)
        .and_then(Value::as_u64)
        .and_then(|p| u16::try_from(p).ok())
        .filter(|p| *p != 0)
}

fn scopes_of(v: Option<&Value>) -> Vec<String> {
    match v {
        Some(Value::String(s)) => s.split_whitespace().map(str::to_string).collect(),
        Some(Value::Array(a)) => a
            .iter()
            .filter_map(Value::as_str)
            .map(str::to_string)
            .collect(),
        _ => Vec::new(),
    }
}

/// The OAuth configuration in a tool's server entry, in the profile's terms.
/// `client_secret` is returned as written; the capture refuses a literal.
pub fn read(target: &str, def: &Map<String, Value>) -> Option<McpOauth> {
    let mut o = McpOauth::default();
    match target {
        "claude-code" => {
            let m = def.get("oauth")?.as_object()?;
            o.client_id = s(m, "clientId");
            o.callback_port = port(m, "callbackPort");
            o.scopes = scopes_of(m.get("scopes"));
            o.auth_server_metadata_url = s(m, "authServerMetadataUrl");
        }
        "gemini-cli" => {
            let m = def.get("oauth")?.as_object()?;
            o.enabled = m.get("enabled").and_then(Value::as_bool);
            o.client_id = s(m, "clientId");
            o.client_secret = s(m, "clientSecret");
            o.authorization_url = s(m, "authorizationUrl");
            o.issuer = s(m, "issuer");
            o.token_url = s(m, "tokenUrl");
            o.scopes = scopes_of(m.get("scopes"));
            o.redirect_uri = s(m, "redirectUri");
        }
        "opencode" => match def.get("oauth")? {
            Value::Bool(false) => o.enabled = Some(false),
            Value::Object(m) => {
                o.client_id = s(m, "client_id");
                o.client_secret = s(m, "client_secret");
                o.scopes = scopes_of(m.get("scope"));
                o.callback_port = port(m, "callback_port");
                o.redirect_uri = s(m, "redirect_uri");
                o.auth_server_metadata_url = s(m, "auth_server_metadata_url");
            }
            _ => return None,
        },
        "codex" => {
            if let Some(Value::Object(m)) = def.get("oauth") {
                o.client_id = s(m, "client_id");
                o.client_secret = s(m, "client_secret");
                o.callback_port = port(m, "callback_port");
            }
            o.scopes = scopes_of(def.get("scopes"));
            o.resource = s(def, "oauth_resource");
        }
        "hermes" => {
            let on = def
                .get("auth")
                .and_then(Value::as_str)
                .is_some_and(|a| a.eq_ignore_ascii_case("oauth"));
            if let Some(Value::Object(m)) = def.get("oauth") {
                o.client_id = s(m, "client_id");
                o.client_secret = s(m, "client_secret");
                o.redirect_uri = s(m, "redirect_uri");
            }
            if on {
                o.enabled = Some(true);
            }
        }
        "cursor" => {
            let m = def.get("auth")?.as_object()?;
            o.client_id = s(m, "CLIENT_ID");
            o.client_secret = s(m, "CLIENT_SECRET");
        }
        _ => return None,
    }
    (o != McpOauth::default()).then_some(o)
}

/// Drop what must not enter a profile from a captured server: a client secret
/// that isn't a lone env reference. Returns `true` when one was left behind.
pub fn drop_literal_secret(server: &mut McpServer) -> bool {
    let Some(o) = &mut server.oauth else {
        return false;
    };
    if o.client_secret.is_some() && safe_secret(o).is_none() {
        o.client_secret = None;
        if *o == McpOauth::default() {
            server.oauth = None;
        }
        return true;
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    fn full() -> McpOauth {
        McpOauth {
            client_id: Some("pub-client".into()),
            client_secret: Some("${MY_SECRET}".into()),
            scopes: vec!["read".into(), "write".into()],
            callback_port: Some(8080),
            ..Default::default()
        }
    }

    #[test]
    fn claude_code_gets_its_own_names_and_no_secret() {
        let w = write("claude-code", &full(), true);
        assert_eq!(
            w.keys["oauth"],
            json!({"clientId": "pub-client", "callbackPort": 8080, "scopes": "read write"})
        );
        assert!(w.secret_not_written);
    }

    #[test]
    fn opencode_gets_snake_case_and_a_brace_ref() {
        let mut o = full();
        o.client_secret = Some("{env:MY_SECRET}".into());
        // The caller renders refs; a rendered `{env:..}` is still a reference.
        let w = write("opencode", &o, true);
        assert_eq!(w.keys["oauth"]["client_id"], "pub-client");
        assert_eq!(w.keys["oauth"]["scope"], "read write");
        assert_eq!(w.keys["oauth"]["callback_port"], 8080);
    }

    #[test]
    fn a_literal_secret_is_never_written() {
        let mut o = full();
        o.client_secret = Some("fake-literal-secret".into());
        for t in ["gemini-cli", "opencode", "cursor", "codex", "claude-code"] {
            let w = write(t, &o, secret_is_ref(&o));
            assert!(
                !Value::Object(w.keys).to_string().contains("fake-literal"),
                "{t}"
            );
            assert!(w.secret_not_written);
        }
    }

    #[test]
    fn read_round_trips_claude_code_and_gemini() {
        let w = write("claude-code", &full(), true);
        let r = read("claude-code", &w.keys).unwrap();
        assert_eq!(r.client_id.as_deref(), Some("pub-client"));
        assert_eq!(r.scopes, ["read", "write"]);
        assert_eq!(r.callback_port, Some(8080));
        let w = write("gemini-cli", &full(), true);
        let r = read("gemini-cli", &w.keys).unwrap();
        assert_eq!(r.scopes, ["read", "write"]);
        assert_eq!(r.client_secret.as_deref(), Some("${MY_SECRET}"));
    }

    #[test]
    fn checklist_names_documented_commands_only() {
        let mut p = Profile::default();
        p.mcp_servers.insert(
            "linear".into(),
            McpServer {
                url: Some("https://mcp.example.com/mcp".into()),
                oauth: Some(full()),
                ..Default::default()
            },
        );
        p.mcp_servers.insert(
            "plain".into(),
            McpServer {
                url: Some("https://plain.example.com/mcp".into()),
                headers: [("X-Key".to_string(), "${K}".to_string())].into(),
                ..Default::default()
            },
        );
        p.mcp_servers.insert(
            "bare".into(),
            McpServer {
                url: Some("https://bare.example.com/mcp".into()),
                ..Default::default()
            },
        );
        p.mcp_servers.insert(
            "local".into(),
            McpServer {
                command: Some("npx".into()),
                ..Default::default()
            },
        );
        let c = checklist(&p, "claude-code");
        assert_eq!(c.len(), 1);
        assert_eq!(c[0].command, "claude mcp login linear");
        assert_eq!(checklist(&p, "gemini-cli")[0].command, "/mcp auth linear");
        assert_eq!(checklist(&p, "codex")[0].command, "codex mcp login linear");
        // opencode: OAuth by default for a remote server without header credentials.
        let names: Vec<_> = checklist(&p, "opencode")
            .into_iter()
            .map(|c| c.server)
            .collect();
        assert_eq!(names, ["bare", "linear"]);
        assert!(checklist(&p, "cursor").is_empty());
        assert!(checklist(&p, "amp").is_empty());
    }
}
