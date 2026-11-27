//! Home-relative paths, so a profile captured on one machine applies on another.
//!
//! Capture replaces this machine's home directory with `~` in MCP server
//! commands, arguments, env values and `cwd` (at the start of a value or
//! after `=`, as in `--config=/home/me/x`), and in Claude Code's
//! `permissions`. Apply expands `~` back to the home directory of the machine
//! it runs on, in the native separator, except in Claude Code permission rules,
//! which understand `~/` themselves.

use std::path::Path;

use serde_json::{Map, Value};

use crate::adapters::{Env, Os};
use crate::model::Profile;

/// This machine's home directory as a string prefix, or `None` when it is
/// unusable as one (empty or the root).
fn home_prefix(home: &Path) -> Option<String> {
    let h = home.display().to_string();
    let h = h.trim_end_matches(['/', '\\']).to_string();
    (!h.is_empty() && h != "/").then_some(h)
}

/// Places in `v` where a path may start: the start, and after each `=`.
fn starts(v: &str) -> Vec<usize> {
    std::iter::once(0)
        .chain(v.match_indices('=').map(|(i, _)| i + 1))
        .collect()
}

/// `v` with this machine's home directory written as `~` where a path starts.
pub fn to_home_relative(v: &str, home: &Path, os: Os) -> String {
    let Some(h) = home_prefix(home) else {
        return v.to_string();
    };
    let insensitive = os != Os::Linux;
    let mut out = String::with_capacity(v.len());
    let mut last = 0;
    for at in starts(v) {
        if at < last {
            continue;
        }
        let rest = &v[at..];
        let Some(head) = rest.get(..h.len()) else {
            continue;
        };
        let same = if insensitive {
            head.eq_ignore_ascii_case(&h)
        } else {
            head == h
        };
        let after = &rest[h.len()..];
        if !same || !(after.is_empty() || after.starts_with(['/', '\\'])) {
            continue;
        }
        // The path runs to the end of the value, or to the next `=` for flags.
        let end = after.find(['=', ' ']).map_or(v.len(), |i| at + h.len() + i);
        out.push_str(&v[last..at]);
        out.push('~');
        out.push_str(&v[at + h.len()..end].replace('\\', "/"));
        last = end;
    }
    out.push_str(&v[last..]);
    out
}

/// `v` with a leading `~` (at the start, or after `=`) expanded to `home`.
pub fn expand_home(v: &str, home: &Path, os: Os) -> String {
    let Some(h) = home_prefix(home) else {
        return v.to_string();
    };
    let sep = if os == Os::Windows { '\\' } else { '/' };
    let mut out = String::with_capacity(v.len());
    let mut last = 0;
    for at in starts(v) {
        if at < last {
            continue;
        }
        let rest = &v[at..];
        if !(rest == "~" || rest.starts_with("~/")) {
            continue;
        }
        let end = rest[1..].find(['=', ' ']).map_or(v.len(), |i| at + 1 + i);
        out.push_str(&v[last..at]);
        out.push_str(&h);
        out.push_str(&v[at + 1..end].replace('/', &sep.to_string()));
        last = end;
    }
    out.push_str(&v[last..]);
    out
}

/// Claude Code permission rule specifiers are `//abs/path` for absolute
/// paths and `~/path` for the home directory.
pub fn rule_to_home_relative(rule: &str, home: &Path, os: Os) -> String {
    let Some(h) = home_prefix(home) else {
        return rule.to_string();
    };
    let abs = format!("(/{h}");
    match rule.find(&abs) {
        Some(i)
            if rule[i + abs.len()..].starts_with(['/', ')'])
                && (os == Os::Linux || !h.contains('\\')) =>
        {
            format!("{}(~{}", &rule[..i], &rule[i + abs.len()..])
        }
        _ => rule.to_string(),
    }
}

fn map_list(v: Option<&mut Value>, f: impl Fn(&str) -> String) {
    if let Some(Value::Array(items)) = v {
        for item in items {
            if let Value::String(s) = item {
                *s = f(s);
            }
        }
    }
}

/// Make captured settings for `target` portable.
pub fn capture_settings(target: &str, settings: &mut Map<String, Value>, home: &Path, os: Os) {
    if target != "claude-code" {
        return;
    }
    let Some(Value::Object(perms)) = settings.get_mut("permissions") else {
        return;
    };
    for key in ["allow", "ask", "deny"] {
        map_list(perms.get_mut(key), |r| rule_to_home_relative(r, home, os));
    }
    map_list(perms.get_mut("additionalDirectories"), |d| {
        to_home_relative(d, home, os)
    });
}

/// `profile` with home-relative paths expanded for `env`'s machine.
pub fn expand_profile(profile: &Profile, env: &Env) -> Profile {
    let mut p = profile.clone();
    for s in p.mcp_servers.values_mut() {
        *s = s.map_strings(|v| expand_home(v, &env.home, env.os));
    }
    crate::plugins::expand_paths(&mut p.plugins, &env.home, env.os);
    if let Some(Value::Object(perms)) = p
        .settings
        .get_mut("claude-code")
        .and_then(|s| s.get_mut("permissions"))
    {
        map_list(perms.get_mut("additionalDirectories"), |d| {
            expand_home(d, &env.home, env.os)
        });
    }
    p
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_home_paths() {
        let home = Path::new("/home/me");
        let cases = [
            ("/home/me/bin/x", "~/bin/x"),
            ("/home/me", "~"),
            ("--config=/home/me/.x.toml", "--config=~/.x.toml"),
            ("a=/home/me/a,b=/home/me/b", "a=~/a,b=~/b"),
            ("/home/me/My Docs/x", "~/My Docs/x"),
            ("/home/mine/x", "/home/mine/x"),
            ("/opt/home/me/x", "/opt/home/me/x"),
            ("https://host/home/me", "https://host/home/me"),
        ];
        for (abs, rel) in cases {
            assert_eq!(to_home_relative(abs, home, Os::Linux), rel, "{abs}");
            if abs != rel {
                assert_eq!(expand_home(rel, home, Os::Linux), abs, "{rel}");
            }
        }
        assert_eq!(expand_home("~user/x", home, Os::Linux), "~user/x");
        assert_eq!(expand_home("x~/y", home, Os::Linux), "x~/y");
        assert_eq!(to_home_relative("/x", Path::new("/"), Os::Linux), "/x");
    }

    #[test]
    fn windows_paths_become_portable() {
        let home = Path::new(r"C:\Users\Me");
        assert_eq!(
            to_home_relative(r"c:\users\me\bin\srv.exe", home, Os::Windows),
            "~/bin/srv.exe"
        );
        assert_eq!(
            expand_home("~/bin/srv.exe", home, Os::Windows),
            r"C:\Users\Me\bin\srv.exe"
        );
        assert_eq!(
            expand_home("~/bin/srv", Path::new("/Users/me"), Os::MacOs),
            "/Users/me/bin/srv"
        );
    }

    #[test]
    fn permission_rules_use_claude_codes_home_syntax() {
        let home = Path::new("/home/me");
        assert_eq!(
            rule_to_home_relative("Read(//home/me/notes/**)", home, Os::Linux),
            "Read(~/notes/**)"
        );
        assert_eq!(
            rule_to_home_relative("Read(//home/mine/**)", home, Os::Linux),
            "Read(//home/mine/**)"
        );
        assert_eq!(
            rule_to_home_relative("Bash(git status)", home, Os::Linux),
            "Bash(git status)"
        );
    }
}
