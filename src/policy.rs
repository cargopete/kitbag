//! Which tool settings kitbag carries, and which ones deserve a warning.
//!
//! Profiles may come from someone else's git repository, so only settings with
//! a bounded effect are applied: no hooks, no env blocks, no helper commands,
//! no provider endpoints. Anything else is dropped with a note.

use serde_json::{Map, Value};

/// Allow-listed top-level settings keys per target.
pub fn allowed_keys(target: &str) -> &'static [&'static str] {
    match target {
        "claude-code" => &[
            "model",
            "effortLevel",
            "permissions",
            "outputStyle",
            "alwaysThinkingEnabled",
            "includeCoAuthoredBy",
            "cleanupPeriodDays",
        ],
        "codex" => &[
            "model",
            "model_reasoning_effort",
            "model_reasoning_summary",
            "model_verbosity",
            "approval_policy",
            "sandbox_mode",
            "web_search",
            "file_opener",
        ],
        "gemini-cli" => &["model", "ui"],
        _ => &[],
    }
}

/// Split `settings` into the allow-listed part and the names of dropped keys.
pub fn filter(
    target: &str,
    settings: Option<&Map<String, Value>>,
) -> (Map<String, Value>, Vec<String>) {
    let keys = allowed_keys(target);
    let mut allowed = Map::new();
    let mut dropped = Vec::new();
    for (k, v) in settings.into_iter().flatten() {
        if keys.contains(&k.as_str()) {
            allowed.insert(k.clone(), v.clone());
        } else {
            dropped.push(k.clone());
        }
    }
    (allowed, dropped)
}

/// Commands that can do anything once their arguments are open: delete, escalate,
/// download and run, or run arbitrary code.
const DANGEROUS_COMMANDS: &[&str] = &[
    "rm", "sudo", "su", "doas", "sh", "bash", "zsh", "fish", "eval", "exec", "xargs", "env",
    "curl", "wget", "dd", "mkfs", "chmod", "chown", "ssh", "scp", "nc", "python", "python3",
    "node", "perl", "ruby",
];

fn broad_claude_rule(rule: &str) -> bool {
    let bare = ["Bash", "Write", "Edit", "WebFetch"];
    if bare
        .iter()
        .any(|t| rule == *t || rule == format!("{t}(*)") || rule == format!("{t}(**)"))
    {
        return true;
    }
    let Some(spec) = rule.strip_prefix("Bash(").and_then(|r| r.strip_suffix(')')) else {
        return false;
    };
    // `Bash(npm*)`: any command starting with those letters.
    if spec.ends_with('*') && !spec.ends_with(":*") && !spec.contains(' ') {
        return true;
    }
    // `Bash(rm -rf *)`, `Bash(curl:*)`, `Bash(sudo *)`: a dangerous command with open arguments.
    let command = spec
        .split([' ', ':'])
        .next()
        .unwrap_or("")
        .rsplit('/')
        .next()
        .unwrap_or("");
    spec.contains('*') && DANGEROUS_COMMANDS.contains(&command)
}

fn risky_default_mode(mode: &str) -> bool {
    matches!(
        mode,
        "bypassPermissions" | "acceptEdits" | "dontAsk" | "auto"
    )
}

/// Human-readable warnings for settings that widen what an agent may do unattended.
pub fn risky(target: &str, settings: &Map<String, Value>) -> Vec<String> {
    let mut out = Vec::new();
    match target {
        "claude-code" => {
            if let Some(Value::Object(perms)) = settings.get("permissions") {
                if let Some(Value::String(mode)) = perms.get("defaultMode")
                    && risky_default_mode(mode)
                {
                    out.push(format!("claude-code: permissions.defaultMode = {mode}"));
                }
                if let Some(Value::Array(dirs)) = perms.get("additionalDirectories")
                    && !dirs.is_empty()
                {
                    out.push(format!(
                        "claude-code: permissions.additionalDirectories = {}",
                        dirs.iter()
                            .map(|d| d.as_str().map_or_else(|| d.to_string(), str::to_string))
                            .collect::<Vec<_>>()
                            .join(", ")
                    ));
                }
                if let Some(Value::Array(allow)) = perms.get("allow")
                    && !allow.is_empty()
                {
                    let broad: Vec<&str> = allow
                        .iter()
                        .filter_map(Value::as_str)
                        .filter(|r| broad_claude_rule(r))
                        .collect();
                    let mut msg = format!(
                        "claude-code: allows {} tool permission rule(s) without prompting",
                        allow.len()
                    );
                    if !broad.is_empty() {
                        msg.push_str(&format!(", including broad ones: {}", broad.join(", ")));
                    }
                    out.push(msg);
                }
            }
        }
        "codex" => {
            if settings.get("approval_policy").and_then(Value::as_str) == Some("never") {
                out.push("codex: approval_policy = never".into());
            }
            if settings.get("sandbox_mode").and_then(Value::as_str) == Some("danger-full-access") {
                out.push("codex: sandbox_mode = danger-full-access".into());
            }
        }
        _ => {}
    }
    out
}

/// Remove the riskiest settings, the ones that let agents act without asking
/// or reach outside the project: permissive `defaultMode`s,
/// `additionalDirectories`, broad allow rules, Codex's `approval_policy = never`
/// and `sandbox_mode = danger-full-access`. Used for profiles from remote
/// sources. Returns a description of each removed setting.
pub fn drop_risky(target: &str, settings: &mut Map<String, Value>) -> Vec<String> {
    let mut out = Vec::new();
    match target {
        "claude-code" => {
            let Some(Value::Object(perms)) = settings.get_mut("permissions") else {
                return out;
            };
            if let Some(Value::String(mode)) = perms.get("defaultMode")
                && risky_default_mode(mode)
            {
                out.push(format!("permissions.defaultMode = {mode}"));
                perms.remove("defaultMode");
            }
            if let Some(dirs) = perms.remove("additionalDirectories") {
                out.push(format!("permissions.additionalDirectories = {dirs}"));
            }
            if let Some(Value::Array(allow)) = perms.get_mut("allow") {
                allow.retain(|r| {
                    let broad = r.as_str().is_some_and(broad_claude_rule);
                    if broad {
                        out.push(format!("permissions.allow {r}"));
                    }
                    !broad
                });
                if allow.is_empty() {
                    perms.remove("allow");
                }
            }
            if perms.is_empty() {
                settings.remove("permissions");
            }
        }
        "codex" => {
            for (key, bad) in [
                ("approval_policy", "never"),
                ("sandbox_mode", "danger-full-access"),
            ] {
                if settings.get(key).and_then(Value::as_str) == Some(bad) {
                    settings.remove(key);
                    out.push(format!("{key} = {bad}"));
                }
            }
        }
        _ => {}
    }
    out
}

/// Commands and skills that pre-approve tools or run shell commands when used.
pub fn risky_content(profile: &crate::model::Profile) -> Vec<String> {
    let mut out = Vec::new();
    let tools = |v: &Value| match v {
        Value::String(s) => s.clone(),
        Value::Array(a) => a
            .iter()
            .filter_map(Value::as_str)
            .collect::<Vec<_>>()
            .join(", "),
        other => other.to_string(),
    };
    for c in &profile.commands {
        if let Some(v) = c.extra.get("allowed-tools") {
            out.push(format!(
                "command '{}' pre-approves tools: {}",
                c.name,
                tools(v)
            ));
        }
        if c.body.contains("!`") {
            out.push(format!(
                "command '{}' runs shell commands when invoked (!`...`)",
                c.name
            ));
        }
    }
    for s in &profile.skills {
        if let Some(main) = s.main_file() {
            let text = String::from_utf8_lossy(&main.content);
            if let Some(v) = crate::frontmatter::parse(&text).data.get("allowed-tools") {
                out.push(format!(
                    "skill '{}' pre-approves tools: {}",
                    s.name,
                    tools(v)
                ));
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn filters_unsafe_keys() {
        let s = json!({"model": "opus", "hooks": {}, "apiKeyHelper": "cat key", "env": {}});
        let (ok, dropped) = filter("claude-code", s.as_object());
        assert_eq!(Value::Object(ok), json!({"model": "opus"}));
        assert_eq!(dropped, vec!["hooks", "apiKeyHelper", "env"]);
        let (ok, dropped) = filter("unknown-target", s.as_object());
        assert!(ok.is_empty());
        assert_eq!(dropped.len(), 4);
    }

    #[test]
    fn flags_risky_settings() {
        let s = json!({"permissions": {"defaultMode": "bypassPermissions", "allow": ["Bash", "Bash(npm test:*)"]}});
        assert_eq!(
            risky("claude-code", s.as_object().unwrap()),
            vec![
                "claude-code: permissions.defaultMode = bypassPermissions",
                "claude-code: allows 2 tool permission rule(s) without prompting, including broad ones: Bash",
            ]
        );
        let s = json!({"approval_policy": "never", "sandbox_mode": "danger-full-access"});
        assert_eq!(risky("codex", s.as_object().unwrap()).len(), 2);
        let s = json!({"permissions": {"additionalDirectories": ["/"], "allow": ["Bash(rm -rf *)", "Bash(curl:*)", "Bash(cargo test:*)"]}});
        assert_eq!(
            risky("claude-code", s.as_object().unwrap()),
            vec![
                "claude-code: permissions.additionalDirectories = /",
                "claude-code: allows 3 tool permission rule(s) without prompting, including broad ones: Bash(rm -rf *), Bash(curl:*)",
            ]
        );
    }

    #[test]
    fn broad_rules() {
        for r in [
            "Bash",
            "Bash(*)",
            "Bash(npm*)",
            "Bash(rm -rf *)",
            "Bash(sudo *)",
            "Bash(/usr/bin/curl *)",
            "Bash(python3:*)",
            "Write(**)",
        ] {
            assert!(broad_claude_rule(r), "{r}");
        }
        for r in [
            "Bash(npm test:*)",
            "Bash(cargo build *)",
            "Bash(rm target/tmp.txt)",
            "Read(./src/**)",
            "WebFetch(domain:docs.rs)",
        ] {
            assert!(!broad_claude_rule(r), "{r}");
        }
    }

    #[test]
    fn drops_risky_settings() {
        let mut s = json!({"model": "opus", "permissions": {
            "defaultMode": "bypassPermissions",
            "additionalDirectories": ["/"],
            "allow": ["Bash(*)", "Bash(cargo test:*)"],
            "deny": ["Read(./.env)"],
        }})
        .as_object()
        .cloned()
        .unwrap();
        assert_eq!(
            drop_risky("claude-code", &mut s),
            vec![
                "permissions.defaultMode = bypassPermissions",
                r#"permissions.additionalDirectories = ["/"]"#,
                r#"permissions.allow "Bash(*)""#,
            ]
        );
        assert_eq!(
            Value::Object(s),
            json!({"model": "opus", "permissions": {"allow": ["Bash(cargo test:*)"], "deny": ["Read(./.env)"]}})
        );
        let mut s = json!({"permissions": {"allow": ["Bash"]}})
            .as_object()
            .cloned()
            .unwrap();
        drop_risky("claude-code", &mut s);
        assert!(s.is_empty());
        let mut s = json!({"approval_policy": "never", "sandbox_mode": "danger-full-access", "model": "o3"})
            .as_object()
            .cloned()
            .unwrap();
        assert_eq!(drop_risky("codex", &mut s).len(), 2);
        assert_eq!(Value::Object(s), json!({"model": "o3"}));
    }
}
