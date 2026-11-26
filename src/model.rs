//! The in-memory profile model.
//!
//! On disk a profile is a directory (see [`crate::profile_io`]):
//!
//! ```text
//! kitbag.toml              manifest: MCP servers + per-tool settings
//! instructions.md          always-on instructions (-> CLAUDE.md / AGENTS.md / GEMINI.md ...)
//! rules/<name>.md          scoped rules with optional `globs` frontmatter
//! commands/<name>.md       reusable prompts / slash commands
//! skills/<name>/SKILL.md   agent skills, plus any supporting files
//! ```
//!
//! Everything in a [`Profile`] is secret-free by construction: values that are
//! secrets are stored as `${ENV_VAR}` references and only resolved (if at all)
//! when applying to a tool that cannot expand references itself.

use std::collections::BTreeMap;
use std::sync::LazyLock;

use regex::Regex;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

/// Version of the on-disk profile format. Bumped only for breaking changes.
pub const FORMAT_VERSION: u32 = 1;

/// How an MCP server is reached.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Transport {
    /// A local process speaking MCP over stdin/stdout.
    Stdio,
    /// Streamable HTTP.
    Http,
    /// Legacy HTTP + server-sent events.
    Sse,
}

impl Transport {
    pub fn as_str(self) -> &'static str {
        match self {
            Transport::Stdio => "stdio",
            Transport::Http => "http",
            Transport::Sse => "sse",
        }
    }
}

fn is_false(b: &bool) -> bool {
    !*b
}

/// A portable MCP server definition.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct McpServer {
    /// `stdio`, `http` or `sse`. Inferred from `command` / `url` when omitted.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub transport: Option<Transport>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub command: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub args: Vec<String>,
    /// Environment for the server process. Values may contain `${VAR}` references.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub env: BTreeMap<String, String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    /// HTTP headers for remote servers. Values may contain `${VAR}` references.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub headers: BTreeMap<String, String>,
    /// Only apply this server to these target ids (an allow-list).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub targets: Option<Vec<String>>,
    /// Never apply this server to these target ids (a deny-list).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub disabled_on: Vec<String>,
    /// The older name of `targets`, read but never written. Loading a profile
    /// moves it into `targets` (see [`McpServer::take_legacy_targeting`]).
    #[serde(default, skip_serializing)]
    pub only: Option<Vec<String>>,
    /// The older name of `disabled_on`, read but never written.
    #[serde(default, skip_serializing)]
    pub except: Option<Vec<String>>,
    /// Keep the definition but mark it disabled where the target supports that.
    #[serde(default, skip_serializing_if = "is_false")]
    pub disabled: bool,
}

impl McpServer {
    pub fn transport(&self) -> Transport {
        match self.transport {
            Some(t) => t,
            None if self.url.is_some() && self.command.is_none() => Transport::Http,
            None => Transport::Stdio,
        }
    }

    pub fn applies_to(&self, target: &str) -> bool {
        self.skip_reason(target).is_none()
    }

    /// Why this server is not applied to `target`, or `None` when it is.
    pub fn skip_reason(&self, target: &str) -> Option<&'static str> {
        if let Some(only) = &self.targets
            && !only.iter().any(|t| t == target)
        {
            return Some("not in its 'targets'");
        }
        self.disabled_on
            .iter()
            .any(|t| t == target)
            .then_some("listed in 'disabled_on'")
    }

    /// Move the older `only` / `except` fields into `targets` / `disabled_on`.
    /// Returns (errors, warnings). An empty `only` keeps its old meaning, no
    /// restriction, with a warning; setting a field under both names is an error.
    pub fn take_legacy_targeting(&mut self) -> (Vec<String>, Vec<String>) {
        let (mut errors, mut warnings) = (Vec::new(), Vec::new());
        if let Some(only) = self.only.take() {
            if self.targets.is_some() {
                errors.push(
                    "both 'only' and 'targets' are set; 'only' is the older name of 'targets', keep one"
                        .into(),
                );
            } else if only.is_empty() {
                warnings.push(
                    "'only' is empty, which means no restriction: the server applies to every tool; remove it, or list tools in 'targets'"
                        .into(),
                );
            } else {
                self.targets = Some(only);
            }
        }
        if let Some(except) = self.except.take() {
            if !self.disabled_on.is_empty() {
                errors.push(
                    "both 'except' and 'disabled_on' are set; 'except' is the older name of 'disabled_on', keep one"
                        .into(),
                );
            } else {
                self.disabled_on = except;
            }
        }
        (errors, warnings)
    }

    /// Problems with `targets` / `disabled_on`, given the valid target ids, as
    /// (errors, warnings). An unknown target is only a warning, so a profile
    /// naming a tool from a newer kitbag still loads; the name matches no
    /// target here, so it is in effect ignored.
    pub fn targeting_issues(&self, valid: &[&str]) -> (Vec<String>, Vec<String>) {
        let (mut errors, mut warnings) = (Vec::new(), Vec::new());
        if self.targets.is_some() && !self.disabled_on.is_empty() {
            errors.push(
                "set either 'targets' (only these tools) or 'disabled_on' (all but these), not both ('only' / 'except' are their older names)"
                    .into(),
            );
        }
        if self.targets.as_ref().is_some_and(Vec::is_empty) {
            errors.push(
                "'targets' is empty, so the server would apply nowhere; list at least one target, or remove the server"
                    .into(),
            );
        }
        let lists = [
            ("targets", self.targets.as_deref().unwrap_or_default()),
            ("disabled_on", self.disabled_on.as_slice()),
        ];
        for (field, ids) in lists {
            for id in ids {
                if !valid.contains(&id.as_str()) {
                    warnings.push(format!(
                        "unknown target '{id}' in '{field}' is ignored (from a newer kitbag, or a typo?); valid targets: {}",
                        valid.join(", ")
                    ));
                }
            }
        }
        (errors, warnings)
    }

    /// Every string value in the definition, with a human-readable location.
    pub fn strings(&self) -> Vec<(String, &str)> {
        let mut out = Vec::new();
        if let Some(c) = &self.command {
            out.push(("command".to_string(), c.as_str()));
        }
        for (i, a) in self.args.iter().enumerate() {
            out.push((format!("args[{i}]"), a.as_str()));
        }
        for (k, v) in &self.env {
            out.push((format!("env.{k}"), v.as_str()));
        }
        if let Some(c) = &self.cwd {
            out.push(("cwd".to_string(), c.as_str()));
        }
        if let Some(u) = &self.url {
            out.push(("url".to_string(), u.as_str()));
        }
        for (k, v) in &self.headers {
            out.push((format!("headers.{k}"), v.as_str()));
        }
        out
    }

    /// Apply `f` to every string value (used for reference normalisation and rendering).
    pub fn map_strings(&self, mut f: impl FnMut(&str) -> String) -> McpServer {
        let mut s = self.clone();
        s.command = s.command.as_deref().map(&mut f);
        s.args = s.args.iter().map(|a| f(a)).collect();
        s.env = s.env.iter().map(|(k, v)| (k.clone(), f(v))).collect();
        s.cwd = s.cwd.as_deref().map(&mut f);
        s.url = s.url.as_deref().map(&mut f);
        s.headers = s.headers.iter().map(|(k, v)| (k.clone(), f(v))).collect();
        s
    }

    /// Apply `f` to the one string at `location` (as named by [`McpServer::strings`]).
    pub fn map_string_at(&mut self, location: &str, f: impl FnOnce(&str) -> String) {
        let slot: Option<&mut String> = if location == "command" {
            self.command.as_mut()
        } else if location == "cwd" {
            self.cwd.as_mut()
        } else if location == "url" {
            self.url.as_mut()
        } else if let Some(i) = location
            .strip_prefix("args[")
            .and_then(|r| r.strip_suffix(']'))
        {
            i.parse::<usize>().ok().and_then(|i| self.args.get_mut(i))
        } else if let Some(k) = location.strip_prefix("env.") {
            self.env.get_mut(k)
        } else if let Some(k) = location.strip_prefix("headers.") {
            self.headers.get_mut(k)
        } else {
            None
        };
        if let Some(v) = slot {
            *v = f(v);
        }
    }

    /// Problems that make this definition unusable.
    pub fn validate(&self) -> Vec<String> {
        let mut issues = Vec::new();
        let t = self.transport();
        if t == Transport::Stdio && self.command.as_deref().is_none_or(str::is_empty) {
            issues.push("stdio servers need 'command' (or set 'url' for a remote server)".into());
        }
        if t != Transport::Stdio {
            if self.url.as_deref().is_none_or(str::is_empty) {
                issues.push(format!("{} servers need 'url'", t.as_str()));
            }
            if self.command.is_some() || !self.args.is_empty() {
                issues.push("remote servers can't have 'command' or 'args'".into());
            }
        }
        if t == Transport::Stdio && !self.headers.is_empty() {
            issues.push("'headers' only apply to remote servers".into());
        }
        issues
    }
}

/// A rule: instructions that apply always, to matching files, or on demand.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Rule {
    /// File stem, e.g. `typescript` for `rules/typescript.md`.
    pub name: String,
    pub description: Option<String>,
    /// Glob patterns this rule applies to. Empty = not file-scoped.
    pub globs: Vec<String>,
    /// `Some(true)` forces always-on even with globs; `Some(false)` without globs
    /// means "on demand" (the agent decides based on the description).
    pub always_apply: Option<bool>,
    pub body: String,
}

/// When a rule is loaded.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RuleMode {
    Always,
    Globs,
    OnDemand,
}

impl Rule {
    pub fn mode(&self) -> RuleMode {
        match (self.globs.is_empty(), self.always_apply) {
            (_, Some(true)) => RuleMode::Always,
            (false, _) => RuleMode::Globs,
            (true, Some(false)) => RuleMode::OnDemand,
            (true, None) => RuleMode::Always,
        }
    }
}

/// A reusable prompt / slash command.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Command {
    pub name: String,
    pub description: Option<String>,
    pub argument_hint: Option<String>,
    /// Other frontmatter keys (e.g. Claude Code's `allowed-tools`), kept verbatim
    /// and passed on to targets that understand command frontmatter.
    pub extra: Map<String, Value>,
    /// Markdown prompt. `$ARGUMENTS` is the full argument string, `$1`..`$9` positionals.
    pub body: String,
}

/// One file inside a skill directory.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SkillFile {
    /// Path relative to the skill directory, `/`-separated.
    pub path: String,
    pub content: Vec<u8>,
    /// Had the executable bit set (Unix); kept when the skill is applied.
    pub executable: bool,
}

/// An agent skill: a directory with `SKILL.md` and optional supporting files.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Skill {
    pub name: String,
    pub description: Option<String>,
    pub files: Vec<SkillFile>,
}

impl Skill {
    pub fn main_file(&self) -> Option<&SkillFile> {
        self.files.iter().find(|f| f.path == "SKILL.md")
    }
}

/// Tool-specific settings, keyed by target id, passed through after allow-listing.
pub type Settings = BTreeMap<String, Map<String, Value>>;

/// A complete, secret-free agent setup.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Profile {
    pub name: String,
    pub description: Option<String>,
    pub mcp_servers: BTreeMap<String, McpServer>,
    pub settings: Settings,
    pub instructions: Option<String>,
    pub rules: Vec<Rule>,
    pub commands: Vec<Command>,
    pub skills: Vec<Skill>,
    /// Problems found while loading that don't stop the profile from loading.
    pub warnings: Vec<String>,
}

impl Profile {
    pub fn new(name: impl Into<String>) -> Self {
        Profile {
            name: name.into(),
            ..Profile::default()
        }
    }

    pub fn is_empty(&self) -> bool {
        self.mcp_servers.is_empty()
            && self.settings.is_empty()
            && self.instructions.is_none()
            && self.rules.is_empty()
            && self.commands.is_empty()
            && self.skills.is_empty()
    }

    pub fn sort(&mut self) {
        self.rules.sort_by(|a, b| a.name.cmp(&b.name));
        self.commands.sort_by(|a, b| a.name.cmp(&b.name));
        self.skills.sort_by(|a, b| a.name.cmp(&b.name));
    }
}

static NAME_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^[A-Za-z0-9][A-Za-z0-9._-]{0,127}$").expect("valid regex"));

/// Valid names for rules, commands, skills and servers: portable across file systems and tools.
pub fn is_valid_name(name: &str) -> bool {
    NAME_RE.is_match(name) && name != "." && name != ".." && !name.ends_with('.')
}

/// Is `name` (or the part before its first `.`) a device name Windows
/// reserves, such as `CON`, `nul.md` or `COM1`? Such files can't be created
/// on Windows.
pub fn is_reserved_on_windows(name: &str) -> bool {
    let base = name.split('.').next().unwrap_or("").trim_end_matches(' ');
    let upper = base.to_ascii_uppercase();
    matches!(
        upper.as_str(),
        "CON" | "PRN" | "AUX" | "NUL" | "CONIN$" | "CONOUT$"
    ) || ((upper.starts_with("COM") || upper.starts_with("LPT"))
        && upper.len() == 4
        && upper.as_bytes()[3].is_ascii_digit())
}

/// Names of rules, commands, skills and skill files that would break the
/// profile on another system: Windows device names, and names that differ
/// only in case (one file on Windows and macOS). Profiles move between
/// machines, so this applies on every OS.
pub fn portable_name_issues(p: &Profile) -> Vec<String> {
    let mut issues = Vec::new();
    let reserved = |what: String| {
        format!(
            "{what}: reserved device name on Windows, where the file can't be created; rename it"
        )
    };
    let check = |kind: &str, names: Vec<String>, issues: &mut Vec<String>| {
        let mut seen: BTreeMap<String, String> = BTreeMap::new();
        for name in names {
            if name.rsplit('/').next().is_some_and(is_reserved_on_windows) {
                issues.push(reserved(format!("{kind}{name}")));
            }
            if let Some(first) = seen.insert(name.to_lowercase(), name.clone())
                && first != name
            {
                issues.push(format!(
                    "{kind}{first} and {kind}{name}: names differ only in case, so they are one file on Windows and macOS; rename one"
                ));
            }
        }
    };
    check(
        "rules/",
        p.rules.iter().map(|r| r.name.clone()).collect(),
        &mut issues,
    );
    check(
        "commands/",
        p.commands.iter().map(|c| c.name.clone()).collect(),
        &mut issues,
    );
    check(
        "skills/",
        p.skills.iter().map(|s| s.name.clone()).collect(),
        &mut issues,
    );
    for s in &p.skills {
        // Every directory along a path counts, so `a/x` and `A/y` clash too.
        let mut paths: Vec<String> = Vec::new();
        for f in &s.files {
            let parts: Vec<&str> = f.path.split('/').collect();
            for i in 1..parts.len() {
                paths.push(parts[..i].join("/"));
            }
            paths.push(f.path.clone());
        }
        paths.sort();
        paths.dedup();
        check(&format!("skills/{}/", s.name), paths, &mut issues);
    }
    issues
}

/// Turn arbitrary text into a valid name (`My Setup!` -> `my-setup`).
pub fn slugify(input: &str) -> String {
    let mut out = String::new();
    for ch in input.trim().chars() {
        if ch.is_ascii_alphanumeric() || ch == '.' || ch == '_' {
            out.push(ch.to_ascii_lowercase());
        } else if !out.ends_with('-') {
            out.push('-');
        }
    }
    let trimmed: String = out
        .trim_matches(|c: char| !c.is_ascii_alphanumeric())
        .chars()
        .take(64)
        .collect();
    if trimmed.is_empty() {
        "profile".to_string()
    } else {
        trimmed
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn transport_inference() {
        let s = McpServer {
            url: Some("https://x".into()),
            ..Default::default()
        };
        assert_eq!(s.transport(), Transport::Http);
        let s = McpServer {
            command: Some("npx".into()),
            ..Default::default()
        };
        assert_eq!(s.transport(), Transport::Stdio);
    }

    #[test]
    fn targets_and_disabled_on() {
        let s = McpServer {
            targets: Some(vec!["codex".into()]),
            ..Default::default()
        };
        assert!(s.applies_to("codex"));
        assert!(!s.applies_to("cursor"));
        assert_eq!(s.skip_reason("cursor"), Some("not in its 'targets'"));
        let s = McpServer {
            disabled_on: vec!["codex".into()],
            ..Default::default()
        };
        assert!(!s.applies_to("codex"));
        assert!(s.applies_to("cursor"));
        assert_eq!(s.skip_reason("codex"), Some("listed in 'disabled_on'"));
        assert!(McpServer::default().applies_to("codex"));
    }

    #[test]
    fn targeting_issues() {
        let valid = ["claude-code", "codex"];
        let both = McpServer {
            targets: Some(vec!["codex".into()]),
            disabled_on: vec!["claude-code".into()],
            ..Default::default()
        };
        let (errors, warnings) = both.targeting_issues(&valid);
        assert_eq!(errors.len(), 1, "{errors:?}");
        assert!(errors[0].contains("not both"));
        assert!(warnings.is_empty());
        let unknown = McpServer {
            disabled_on: vec!["vscode".into()],
            ..Default::default()
        };
        assert_eq!(
            unknown.targeting_issues(&valid),
            (
                vec![],
                vec![
                    "unknown target 'vscode' in 'disabled_on' is ignored (from a newer kitbag, or a typo?); valid targets: claude-code, codex"
                        .to_string()
                ]
            )
        );
        // An unknown name matches no target, so it changes nothing.
        assert!(unknown.applies_to("codex"));
        let empty = McpServer {
            targets: Some(Vec::new()),
            ..Default::default()
        };
        assert!(empty.targeting_issues(&valid).0[0].contains("'targets' is empty"));
        assert_eq!(
            McpServer::default().targeting_issues(&valid),
            (vec![], vec![])
        );
    }

    #[test]
    fn legacy_targeting() {
        let mut s = McpServer {
            only: Some(vec!["codex".into()]),
            except: Some(vec!["cursor".into()]),
            ..Default::default()
        };
        assert_eq!(s.take_legacy_targeting(), (vec![], vec![]));
        assert_eq!(s.targets, Some(vec!["codex".to_string()]));
        assert_eq!(s.disabled_on, vec!["cursor".to_string()]);
        assert_eq!((s.only, s.except), (None, None));

        // An empty 'only' still means no restriction.
        let mut s = McpServer {
            only: Some(vec![]),
            ..Default::default()
        };
        let (errors, warnings) = s.take_legacy_targeting();
        assert!(errors.is_empty());
        assert!(warnings[0].contains("'only' is empty"), "{warnings:?}");
        assert_eq!(s.targets, None);
        assert!(s.applies_to("cursor"));

        let mut s = McpServer {
            only: Some(vec!["codex".into()]),
            targets: Some(vec!["cursor".into()]),
            except: Some(vec!["codex".into()]),
            disabled_on: vec!["cursor".into()],
            ..Default::default()
        };
        let (errors, _) = s.take_legacy_targeting();
        assert_eq!(errors.len(), 2, "{errors:?}");
        assert!(errors[0].contains("both 'only' and 'targets'"));
        assert!(errors[1].contains("both 'except' and 'disabled_on'"));
    }

    #[test]
    fn names_and_slugs() {
        assert!(is_valid_name("pdf-tools"));
        assert!(is_valid_name("a.b_c"));
        assert!(!is_valid_name("../etc"));
        assert!(!is_valid_name(".hidden"));
        assert!(!is_valid_name("a/b"));
        for n in [
            "CON",
            "con",
            "nul.md",
            "Com1",
            "LPT9.txt",
            "aux.tar.gz",
            "PRN ",
        ] {
            assert!(is_reserved_on_windows(n), "{n}");
        }
        for n in ["console", "com", "COM10", "nul-ish", "lpt", "a.con"] {
            assert!(!is_reserved_on_windows(n), "{n}");
        }
    }

    #[test]
    fn names_that_break_on_other_systems() {
        let file = |path: &str| SkillFile {
            path: path.into(),
            content: vec![],
            executable: false,
        };
        let mut p = Profile::new("me");
        p.rules.push(Rule {
            name: "nul".into(),
            ..Default::default()
        });
        for name in ["Build", "build", "ok"] {
            p.commands.push(Command {
                name: name.into(),
                ..Default::default()
            });
        }
        p.skills.push(Skill {
            name: "s".into(),
            description: None,
            files: vec![
                file("SKILL.md"),
                file("lib/a.sh"),
                file("Lib/b.sh"),
                file("aux.txt"),
            ],
        });
        let issues = portable_name_issues(&p);
        assert_eq!(issues.len(), 4, "{issues:#?}");
        assert!(issues[0].starts_with("rules/nul: reserved device name"));
        assert!(
            issues[1].starts_with("commands/Build and commands/build: names differ only in case")
        );
        assert!(
            issues
                .iter()
                .any(|i| i.starts_with("skills/s/Lib and skills/s/lib")),
            "{issues:#?}"
        );
        assert!(
            issues
                .iter()
                .any(|i| i.starts_with("skills/s/aux.txt: reserved")),
            "{issues:#?}"
        );
        p.rules.clear();
        p.commands.retain(|c| c.name == "ok");
        p.skills.clear();
        assert!(portable_name_issues(&p).is_empty());
        assert_eq!(slugify("My Setup!"), "my-setup");
        assert_eq!(slugify("  "), "profile");
    }

    #[test]
    fn rule_modes() {
        let mut r = Rule::default();
        assert_eq!(r.mode(), RuleMode::Always);
        r.always_apply = Some(false);
        assert_eq!(r.mode(), RuleMode::OnDemand);
        r.globs = vec!["*.rs".into()];
        assert_eq!(r.mode(), RuleMode::Globs);
        r.always_apply = Some(true);
        assert_eq!(r.mode(), RuleMode::Always);
    }
}
