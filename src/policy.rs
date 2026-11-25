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
        "gemini-cli" => &["model", "context", "general", "ui"],
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

fn broad_claude_rule(rule: &str) -> bool {
    let bare = ["Bash", "Write", "Edit", "WebFetch"];
    bare.iter()
        .any(|t| rule == *t || rule == format!("{t}(*)") || rule == format!("{t}(**)"))
        || (rule.starts_with("Bash(")
            && rule.ends_with("*)")
            && !rule.ends_with(":*)")
            && !rule.contains(' '))
}

/// Human-readable warnings for settings that widen what an agent may do unattended.
pub fn risky(target: &str, settings: &Map<String, Value>) -> Vec<String> {
    let mut out = Vec::new();
    match target {
        "claude-code" => {
            if let Some(Value::Object(perms)) = settings.get("permissions") {
                if let Some(Value::String(mode)) = perms.get("defaultMode")
                    && matches!(
                        mode.as_str(),
                        "bypassPermissions" | "acceptEdits" | "dontAsk" | "auto"
                    )
                {
                    out.push(format!("claude-code: permissions.defaultMode = {mode}"));
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
    }
}
