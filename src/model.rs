//! The in-memory profile model.
//!
//! On disk a profile is a directory (see [`crate::profile_io`]):
//!
//! ```text
//! kitbag.toml              manifest: MCP servers + per-tool settings
//! instructions.md          always-on instructions (-> CLAUDE.md / AGENTS.md / GEMINI.md ...)
//! rules/<name>.md          scoped rules with optional `globs` frontmatter
//! commands/<name>.md       reusable prompts / slash commands
//! agents/<name>.md         custom agents (Copilot `.agent.md`; chat modes)
//! output-styles/<name>.md  Claude Code output styles (response role, tone and format)
//! skills/<name>/SKILL.md   agent skills, plus any supporting files
//! hooks/<path>             scripts the `[[hooks]]` of the manifest run (optional)
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

/// Why a thing with this `targets` allow-list and `disabled_on` deny-list is
/// not applied to `target`, or `None` when it is.
pub fn targeting_skip_reason(
    targets: &Option<Vec<String>>,
    disabled_on: &[String],
    target: &str,
) -> Option<&'static str> {
    if let Some(only) = targets
        && !only.iter().any(|t| t == target)
    {
        return Some("not in its 'targets'");
    }
    disabled_on
        .iter()
        .any(|t| t == target)
        .then_some("listed in 'disabled_on'")
}

/// Move the older `only` / `except` names into `targets` / `disabled_on`.
/// Returns (errors, warnings); `noun` is what is being targeted ("server").
pub fn take_legacy_targeting(
    targets: &mut Option<Vec<String>>,
    disabled_on: &mut Vec<String>,
    only: Option<Vec<String>>,
    except: Option<Vec<String>>,
    noun: &str,
) -> (Vec<String>, Vec<String>) {
    let (mut errors, mut warnings) = (Vec::new(), Vec::new());
    if let Some(only) = only {
        if targets.is_some() {
            errors.push(
                "both 'only' and 'targets' are set; 'only' is the older name of 'targets', keep one"
                    .into(),
            );
        } else if only.is_empty() {
            warnings.push(format!(
                "'only' is empty, which means no restriction: the {noun} applies to every tool; remove it, or list tools in 'targets'"
            ));
        } else {
            *targets = Some(only);
        }
    }
    if let Some(except) = except {
        if !disabled_on.is_empty() {
            errors.push(
                "both 'except' and 'disabled_on' are set; 'except' is the older name of 'disabled_on', keep one"
                    .into(),
            );
        } else {
            *disabled_on = except;
        }
    }
    (errors, warnings)
}

/// Problems with `targets` / `disabled_on`, as (errors, warnings); see
/// [`McpServer::targeting_issues`].
pub fn targeting_issues(
    targets: &Option<Vec<String>>,
    disabled_on: &[String],
    valid: &[&str],
    noun: &str,
) -> (Vec<String>, Vec<String>) {
    let (mut errors, mut warnings) = (Vec::new(), Vec::new());
    if targets.is_some() && !disabled_on.is_empty() {
        errors.push(
            "set either 'targets' (only these tools) or 'disabled_on' (all but these), not both ('only' / 'except' are their older names)"
                .into(),
        );
    }
    if targets.as_ref().is_some_and(Vec::is_empty) {
        errors.push(format!(
            "'targets' is empty, so the {noun} would apply nowhere; list at least one target, or remove the {noun}"
        ));
    }
    let lists = [
        ("targets", targets.as_deref().unwrap_or_default()),
        ("disabled_on", disabled_on),
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

/// Per-tool targeting of a rule, command, skill or agent: the same
/// `targets` / `disabled_on` as an MCP server has, kept in the item's
/// frontmatter.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Targeting {
    /// Only apply to these target ids (an allow-list).
    pub targets: Option<Vec<String>>,
    /// Never apply to these target ids (a deny-list).
    pub disabled_on: Vec<String>,
}

/// The frontmatter keys that hold [`Targeting`], newest names first.
pub const TARGETING_KEYS: [&str; 4] = ["targets", "disabled_on", "only", "except"];

impl Targeting {
    pub fn is_empty(&self) -> bool {
        self.targets.is_none() && self.disabled_on.is_empty()
    }

    /// Why this item is not applied to `target`, or `None` when it is.
    pub fn skip_reason(&self, target: &str) -> Option<&'static str> {
        targeting_skip_reason(&self.targets, &self.disabled_on, target)
    }

    pub fn applies_to(&self, target: &str) -> bool {
        self.skip_reason(target).is_none()
    }

    /// Take the targeting keys out of frontmatter `data`. `targets` and
    /// `disabled_on` are always kitbag's; the older `only` / `except` only
    /// when they hold a list of names (another tool's key of that name stays).
    /// Returns the targeting and (errors, warnings), with aliases and
    /// duplicates dealt with as for MCP servers.
    pub fn take_from(
        data: &mut Map<String, Value>,
        valid: &[&str],
        canonical: impl Fn(&str) -> String,
        noun: &str,
    ) -> (Targeting, Vec<String>, Vec<String>) {
        let mut errors = Vec::new();
        let mut list = |key: &str, only_if_list: bool| -> Option<Vec<String>> {
            let v = data.get(key)?;
            let names = match v {
                Value::String(s) => Some(
                    s.split(',')
                        .map(|n| n.trim().to_string())
                        .filter(|n| !n.is_empty())
                        .collect::<Vec<_>>(),
                ),
                Value::Array(a) if a.iter().all(Value::is_string) => Some(
                    a.iter()
                        .filter_map(|n| n.as_str().map(|n| n.trim().to_string()))
                        .filter(|n| !n.is_empty())
                        .collect(),
                ),
                _ => None,
            };
            if names.is_none() && !only_if_list {
                errors.push(format!("'{key}' must be a list of tool names"));
            }
            if names.is_some() || !only_if_list {
                data.remove(key);
            }
            names
        };
        let mut targets = list("targets", false);
        let mut disabled_on = list("disabled_on", false).unwrap_or_default();
        let only = list("only", true);
        let except = list("except", true);
        let (e, mut warnings) =
            take_legacy_targeting(&mut targets, &mut disabled_on, only, except, noun);
        errors.extend(e);
        for ids in targets.iter_mut().chain(std::iter::once(&mut disabled_on)) {
            let mut seen = std::collections::BTreeSet::new();
            ids.retain_mut(|id| {
                *id = canonical(id);
                seen.insert(id.clone())
            });
        }
        let t = Targeting {
            targets,
            disabled_on,
        };
        let (e, w) = targeting_issues(&t.targets, &t.disabled_on, valid, noun);
        errors.extend(e);
        warnings.extend(w);
        (t, errors, warnings)
    }

    /// The frontmatter entries that spell this targeting (none when empty).
    pub fn entries(&self) -> Vec<(&'static str, Value)> {
        let mut out = Vec::new();
        if let Some(t) = &self.targets {
            out.push(("targets", Value::from(t.clone())));
        }
        if !self.disabled_on.is_empty() {
            out.push(("disabled_on", Value::from(self.disabled_on.clone())));
        }
        out
    }
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
        targeting_skip_reason(&self.targets, &self.disabled_on, target)
    }

    /// Move the older `only` / `except` fields into `targets` / `disabled_on`.
    /// Returns (errors, warnings). An empty `only` keeps its old meaning, no
    /// restriction, with a warning; setting a field under both names is an error.
    pub fn take_legacy_targeting(&mut self) -> (Vec<String>, Vec<String>) {
        take_legacy_targeting(
            &mut self.targets,
            &mut self.disabled_on,
            self.only.take(),
            self.except.take(),
            "server",
        )
    }

    /// Problems with `targets` / `disabled_on`, given the valid target ids, as
    /// (errors, warnings). An unknown target is only a warning, so a profile
    /// naming a tool from a newer kitbag still loads; the name matches no
    /// target here, so it is in effect ignored.
    pub fn targeting_issues(&self, valid: &[&str]) -> (Vec<String>, Vec<String>) {
        targeting_issues(&self.targets, &self.disabled_on, valid, "server")
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
    pub targeting: Targeting,
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
    pub targeting: Targeting,
}

/// A custom agent (Copilot's `.agent.md`, once called a chat mode): a
/// persona with its own instructions, tools and model.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Agent {
    pub name: String,
    pub description: Option<String>,
    /// Other frontmatter keys (`tools`, `model`, `handoffs`, `mcp-servers` ...),
    /// kept verbatim.
    pub extra: Map<String, Value>,
    /// Markdown instructions for the agent.
    pub body: String,
    pub targeting: Targeting,
}

/// A Claude Code output style: a Markdown file that sets Claude's role, tone
/// and response format for a whole session. Its text is a prompt the tool
/// sends with every request, so remote profiles show it in the plan.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct OutputStyle {
    pub name: String,
    pub description: Option<String>,
    /// Other frontmatter keys (`name`, `keep-coding-instructions`,
    /// `force-for-plugin` ...), kept verbatim.
    pub extra: Map<String, Value>,
    /// Markdown instructions for the style.
    pub body: String,
    pub targeting: Targeting,
}

/// A hook: a shell command a tool runs on an event (before a tool call, when
/// a session starts ...). Hooks run automatically, so kitbag carries them only
/// when asked (see [`crate::hooks`]).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Hook {
    /// A neutral event name (`pre-tool`, `session-start` ...), or `<target>:Name` (`claude-code:Name`,
    /// `cursor:Name` ...) for an event only that tool has. See [`crate::hooks::EVENTS`].
    pub event: String,
    /// What the event is narrowed to (a tool name for `pre-tool`), as the tool
    /// reads it. Absent: every time.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub matcher: Option<String>,
    /// The shell command. A script in the profile's `hooks/` directory is
    /// written as `{{hooks_dir}}/<path>`.
    pub command: String,
    /// Seconds the tool waits for it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timeout: Option<u64>,
    /// Other keys of the tool's hook entry (`args`, `shell`, `async`, `if`,
    /// `statusMessage` ...), kept verbatim.
    #[serde(default, skip_serializing_if = "Map::is_empty")]
    pub extra: Map<String, Value>,
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
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Skill {
    pub name: String,
    pub description: Option<String>,
    pub files: Vec<SkillFile>,
    /// Read from `SKILL.md`'s frontmatter when the profile is loaded, and
    /// taken out of the file's content here; written back by [`crate::profile_io`].
    pub targeting: Targeting,
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
    pub agents: Vec<Agent>,
    pub output_styles: Vec<OutputStyle>,
    /// Hooks, in the order they were captured. Optional: most profiles have none.
    pub hooks: Vec<Hook>,
    /// The scripts the hooks run (`hooks/<path>` in the profile; `path` is
    /// relative to `hooks/`).
    pub hook_files: Vec<SkillFile>,
    /// Plugins and the marketplaces they come from, by reference, per target
    /// (see [`crate::plugins`]).
    pub plugins: BTreeMap<String, crate::plugins::PluginSet>,
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
            && self.agents.is_empty()
            && self.output_styles.is_empty()
            && self.hooks.is_empty()
            && self
                .plugins
                .values()
                .all(crate::plugins::PluginSet::is_empty)
    }

    pub fn sort(&mut self) {
        self.rules.sort_by(|a, b| a.name.cmp(&b.name));
        self.commands.sort_by(|a, b| a.name.cmp(&b.name));
        self.skills.sort_by(|a, b| a.name.cmp(&b.name));
        self.agents.sort_by(|a, b| a.name.cmp(&b.name));
        self.output_styles.sort_by(|a, b| a.name.cmp(&b.name));
        self.hook_files.sort_by(|a, b| a.path.cmp(&b.path));
    }
}

/// One rule, command, skill, agent or output style a profile keeps off a target.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SkippedItem {
    /// `rule`, `command`, `skill`, `agent` or `output-style`.
    pub kind: &'static str,
    pub name: String,
    pub why: &'static str,
}

impl SkippedItem {
    /// `rule <name>`, as items are named in plans and records.
    pub fn label(&self) -> String {
        format!("{} {}", self.kind, self.name)
    }
}

impl Profile {
    /// The rules, commands, skills, agents and output styles not applied to `target`.
    pub fn skipped_items(&self, target: &str) -> Vec<SkippedItem> {
        let mut out = Vec::new();
        let mut add = |kind: &'static str, name: &str, t: &Targeting| {
            if let Some(why) = t.skip_reason(target) {
                out.push(SkippedItem {
                    kind,
                    name: name.to_string(),
                    why,
                });
            }
        };
        for r in &self.rules {
            add("rule", &r.name, &r.targeting);
        }
        for c in &self.commands {
            add("command", &c.name, &c.targeting);
        }
        for s in &self.skills {
            add("skill", &s.name, &s.targeting);
        }
        for a in &self.agents {
            add("agent", &a.name, &a.targeting);
        }
        for o in &self.output_styles {
            add("output-style", &o.name, &o.targeting);
        }
        out
    }

    /// This profile without the items not applied to `target`; `None` when
    /// there are none, so the caller can keep using the profile itself.
    pub fn for_target(&self, target: &str) -> Option<Profile> {
        if self.skipped_items(target).is_empty() {
            return None;
        }
        let mut p = self.clone();
        p.rules.retain(|r| r.targeting.applies_to(target));
        p.commands.retain(|c| c.targeting.applies_to(target));
        p.skills.retain(|s| s.targeting.applies_to(target));
        p.agents.retain(|a| a.targeting.applies_to(target));
        p.output_styles.retain(|o| o.targeting.applies_to(target));
        Some(p)
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
    check(
        "agents/",
        p.agents.iter().map(|a| a.name.clone()).collect(),
        &mut issues,
    );
    check(
        "output-styles/",
        p.output_styles.iter().map(|o| o.name.clone()).collect(),
        &mut issues,
    );
    let mut hook_paths: Vec<String> = Vec::new();
    for f in &p.hook_files {
        let parts: Vec<&str> = f.path.split('/').collect();
        for i in 1..parts.len() {
            hook_paths.push(parts[..i].join("/"));
        }
        hook_paths.push(f.path.clone());
    }
    hook_paths.sort();
    hook_paths.dedup();
    check("hooks/", hook_paths, &mut issues);
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
    fn item_targeting_reuses_the_server_rules() {
        let t = Targeting {
            targets: Some(vec!["codex".into()]),
            ..Default::default()
        };
        assert_eq!(t.skip_reason("cursor"), Some("not in its 'targets'"));
        assert!(t.applies_to("codex"));
        let t = Targeting {
            disabled_on: vec!["codex".into()],
            ..Default::default()
        };
        assert_eq!(t.skip_reason("codex"), Some("listed in 'disabled_on'"));
        assert!(Targeting::default().applies_to("codex"));
        assert_eq!(
            t.entries(),
            vec![("disabled_on", serde_json::json!(["codex"]))]
        );
        assert!(Targeting::default().entries().is_empty());

        let mut p = Profile::new("x");
        p.rules.push(Rule {
            name: "r".into(),
            targeting: t.clone(),
            ..Default::default()
        });
        p.commands.push(Command {
            name: "c".into(),
            ..Default::default()
        });
        assert!(p.for_target("cursor").is_none());
        let skipped = p.skipped_items("codex");
        assert_eq!(skipped.len(), 1);
        assert_eq!(skipped[0].label(), "rule r");
        let only = p.for_target("codex").unwrap();
        assert!(only.rules.is_empty() && only.commands.len() == 1);
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
            ..Default::default()
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
