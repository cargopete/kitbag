//! Gemini CLI.
//!
//! * MCP + settings: `~/.gemini/settings.json` / `.gemini/settings.json`
//!   (`mcpServers`; `httpUrl` for streamable HTTP, `url` for SSE). Gemini CLI
//!   expands `$VAR` / `${VAR}` in settings, without `:-default` support.
//! * Instructions: `~/.gemini/GEMINI.md` / `./GEMINI.md`; rules are folded in.
//! * Commands: `.gemini/commands/<name>.toml` with `prompt` and `{{args}}`.
//! * Skills: `.gemini/skills/<name>/SKILL.md`.
//! * Subagents: `~/.gemini/agents/<name>.md` and `.gemini/agents/<name>.md`,
//!   markdown with YAML frontmatter (`name` and `description` required, then
//!   `kind`, `tools`, `mcpServers`, `model`, `temperature`, `max_turns`,
//!   `timeout_mins`) and the system prompt as the body.
//!   Docs: <https://geminicli.com/docs/core/subagents/>.

use std::path::PathBuf;

use anyhow::Result;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};

use super::common::{
    AgentName, add_command, capture_agent_dir, capture_instructions, capture_skills_dir,
    claim_name, documented_agent, import_server, merge_json_servers, merge_json_settings,
    name_from_rel, note_agent_keys, pick_settings, read_text_item, render_server, safe_settings,
    server_entries, server_from_json, servers_for, skill_with_required_frontmatter, str_of,
    string_list, write_agent, write_instructions, write_skill,
};
use super::{Adapter, ApplyOptions, Captured, Env, Feature, Scope, Support, Work};
use crate::edit::json::parse_value;
use crate::model::{Agent, Command, Profile, Transport, is_valid_name};
use crate::plan_fs::{join_rel, walk_files};
use crate::secrets::RefStyle;

const ID: &str = "gemini-cli";
const MANAGED_KEYS: &[&str] = &["command", "args", "env", "cwd", "url", "httpUrl", "headers"];

pub struct GeminiCli;

struct Paths {
    settings: PathBuf,
    instructions: PathBuf,
    commands: PathBuf,
    skills: PathBuf,
    agents: PathBuf,
}

fn paths(env: &Env) -> Paths {
    let (base, instructions) = match env.scope {
        Scope::User => {
            let b = env.home.join(".gemini");
            let i = b.join("GEMINI.md");
            (b, i)
        }
        Scope::Project => (env.project.join(".gemini"), env.project.join("GEMINI.md")),
    };
    Paths {
        settings: base.join("settings.json"),
        instructions,
        commands: base.join("commands"),
        skills: base.join("skills"),
        agents: base.join("agents"),
    }
}

/// Frontmatter keys Gemini CLI's subagent docs list besides `name` and `description`.
const AGENT_KEYS: &[&str] = &[
    "kind",
    "tools",
    "mcpServers",
    "model",
    "temperature",
    "max_turns",
    "timeout_mins",
];

/// A `name` Gemini CLI accepts: "only lowercase letters, numbers, hyphens,
/// and underscores".
fn slug(name: &str) -> bool {
    !name.is_empty()
        && name
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-' || c == '_')
}

/// A subagent as a Gemini CLI file: the documented keys only, each with the
/// type the docs give it (`tools` a list, `mcpServers` a map, `kind` `local`,
/// the numbers numbers). A `name` that isn't a slug is replaced by the
/// profile's name.
pub fn gemini_agent(a: &Agent) -> (String, Vec<String>) {
    let mut a = a.clone();
    if a.extra
        .get("name")
        .is_some_and(|n| !n.as_str().is_some_and(slug))
    {
        a.extra.remove("name");
    }
    documented_agent(&a, AgentName::Required, AGENT_KEYS, |k, v| match k {
        "kind" => v.as_str() == Some("local"),
        "tools" => v.is_array(),
        "mcpServers" => v.is_object(),
        "model" => v.is_string(),
        _ => v.is_number(),
    })
}

fn apply_agent(
    work: &mut Work,
    dir: &std::path::Path,
    a: &Agent,
    opts: &ApplyOptions,
) -> Result<()> {
    if a.description.is_none() {
        work.warn(
            ID,
            format!(
                "agent '{}' skipped: Gemini CLI requires a description, and kitbag does not make one up",
                a.name
            ),
        );
        return Ok(());
    }
    if !slug(&a.name) {
        work.warn(
            ID,
            format!(
                "agent '{}' skipped: Gemini CLI names an agent with lowercase letters, numbers, hyphens and underscores only",
                a.name
            ),
        );
        return Ok(());
    }
    let (text, dropped) = gemini_agent(a);
    if a.extra
        .get("name")
        .is_some_and(|n| !n.as_str().is_some_and(slug))
    {
        work.info(
            ID,
            format!(
                "agent '{}': its name is not a Gemini CLI slug, so the profile name is used",
                a.name
            ),
        );
    }
    note_agent_keys(work, ID, "Gemini CLI", a, &dropped);
    let path = dir.join(format!("{}.md", a.name));
    write_agent(work, &path, text.as_bytes(), opts, ID, &a.name)
}

#[derive(Serialize, Deserialize)]
struct CommandToml {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    description: Option<String>,
    prompt: String,
}

/// Profile command (markdown, `$ARGUMENTS`) -> Gemini CLI command (TOML, `{{args}}`).
pub fn command_toml(c: &Command) -> String {
    let doc = CommandToml {
        description: c.description.clone(),
        prompt: format!("{}\n", c.body.trim().replace("$ARGUMENTS", "{{args}}")),
    };
    toml::to_string_pretty(&doc).expect("serializable")
}

pub fn command_from_toml(name: &str, text: &str) -> Option<Command> {
    let doc: CommandToml = toml::from_str(text).ok()?;
    Some(Command {
        name: name.to_string(),
        description: doc.description.filter(|d| !d.trim().is_empty()),
        body: doc
            .prompt
            .replace("{{args}}", "$ARGUMENTS")
            .trim()
            .to_string(),
        ..Default::default()
    })
}

impl Adapter for GeminiCli {
    fn id(&self) -> &'static str {
        ID
    }
    fn name(&self) -> &'static str {
        "Gemini CLI"
    }
    fn docs(&self) -> &'static str {
        "https://geminicli.com/docs/tools/mcp-server/"
    }
    fn scopes(&self) -> &'static [Scope] {
        &[Scope::User, Scope::Project]
    }
    fn support(&self, _scope: Scope) -> [(Feature, Support); 8] {
        [
            (Feature::Mcp, Support::Native),
            (Feature::Instructions, Support::Native),
            (Feature::Rules, Support::Folded),
            (Feature::Commands, Support::Native),
            (Feature::Skills, Support::Native),
            (Feature::Agents, Support::Native),
            (Feature::OutputStyles, Support::None),
            (Feature::Settings, Support::Native),
        ]
    }

    fn locations(&self, env: &Env) -> Vec<(&'static str, PathBuf)> {
        let p = paths(env);
        vec![
            ("mcp", p.settings.clone()),
            ("instructions", p.instructions.clone()),
            ("rules", p.instructions),
            ("commands", p.commands),
            ("skills", p.skills),
            ("agents", p.agents),
            ("settings", p.settings),
        ]
    }

    fn detect(&self, env: &Env) -> bool {
        paths(env).settings.parent().is_some_and(|d| d.exists())
    }

    fn capture(&self, env: &Env) -> Result<Captured> {
        let mut cap = Captured::for_env(env);
        let p = paths(env);
        if let Some(text) = cap.read_config(&p.settings, ID)? {
            cap.sources.push(p.settings.clone());
            let doc = parse_value(Some(&text), &p.settings)?;
            {
                for (name, def) in server_entries(
                    &mut cap,
                    ID,
                    &p.settings,
                    "mcpServers",
                    doc.get("mcpServers"),
                ) {
                    let mut s = server_from_json(def);
                    s.oauth = crate::oauth::read(ID, def);
                    if let Some(http) = str_of(def.get("httpUrl")) {
                        s.url = Some(http);
                    } else if s.url.is_some() {
                        s.transport = Some(Transport::Sse);
                    }
                    import_server(&mut cap, ID, name, s, true);
                }
            }
            // `mcp.excluded` lists servers Gemini CLI won't connect to.
            let excluded = string_list(doc.get("mcp").and_then(|m| m.get("excluded")));
            for n in excluded {
                if let Some(s) = cap.profile.mcp_servers.get_mut(&n) {
                    s.disabled = true;
                }
            }
            if let Value::Object(doc) = &doc {
                pick_settings(&mut cap, ID, doc);
                if let Some(f) = doc.get("context").and_then(|c| c.get("fileName"))
                    && f != "GEMINI.md"
                {
                    cap.info(
                        ID,
                        format!("context.fileName is {f}; only GEMINI.md was captured"),
                    );
                }
            }
        }
        capture_instructions(&mut cap, &p.instructions, ID)?;
        let mut seen = std::collections::BTreeMap::new();
        for rel in walk_files(&p.commands, 500)? {
            if !rel.ends_with(".toml") {
                continue;
            }
            let path = join_rel(&p.commands, &rel);
            // Gemini namespaces nested commands with ':'; profile names use '-'.
            let name = name_from_rel(&rel, ".toml");
            claim_name(&mut seen, &p.commands, &rel, &name)?;
            let Some(text) = read_text_item(&mut cap, &path, ID) else {
                continue;
            };
            match command_from_toml(&name, &text) {
                Some(c) if is_valid_name(&name) => {
                    cap.sources.push(path);
                    add_command(&mut cap, c, ID);
                }
                _ => cap.warn(
                    ID,
                    format!("skipped {}: not a valid Gemini CLI command", path.display()),
                ),
            }
        }
        capture_skills_dir(&mut cap, &p.skills, ID)?;
        capture_agent_dir(&mut cap, &p.agents, None, ID, "a subagent")?;
        super::hook_files::capture_for(&mut cap, env, &super::hook_files::GEMINI)?;
        Ok(cap)
    }

    fn apply(
        &self,
        profile: &Profile,
        env: &Env,
        opts: &ApplyOptions,
        work: &mut Work,
    ) -> Result<()> {
        let p = paths(env);
        let mut servers = Vec::new();
        for (name, s) in servers_for(profile, ID, work, false) {
            let original = s;
            let s = render_server(s, RefStyle::DollarPlain, env, work);
            let mut def = Map::new();
            match s.transport() {
                Transport::Stdio => {
                    def.insert("command".into(), json!(s.command));
                    if !s.args.is_empty() {
                        def.insert("args".into(), json!(s.args));
                    }
                    if !s.env.is_empty() {
                        def.insert("env".into(), json!(s.env));
                    }
                    if let Some(cwd) = &s.cwd {
                        def.insert("cwd".into(), json!(cwd));
                    }
                }
                t => {
                    let key = if t == Transport::Http {
                        "httpUrl"
                    } else {
                        "url"
                    };
                    def.insert(key.into(), json!(s.url));
                    if !s.headers.is_empty() {
                        def.insert("headers".into(), json!(s.headers));
                    }
                }
            }
            super::common::add_oauth(work, ID, name, (original, &s), &mut def);
            servers.push((name.clone(), Value::Object(def)));
        }
        // Gemini CLI documents no flag on a server entry. It does document
        // `mcp.excluded` in settings.json, a list of servers it won't connect
        // to, so at user scope a server the profile disables is listed there
        // and its entry stays. A project's settings.json is the file shared
        // with a team, so there the entry is removed as before.
        let off_names =
            (env.scope == Scope::User).then(|| super::common::take_skipped_servers(work, ID));
        merge_json_servers(
            work,
            &p.settings,
            &["mcpServers"],
            &servers,
            MANAGED_KEYS,
            opts,
            ID,
        )?;
        let settings = safe_settings(profile, ID, work);
        merge_json_settings(work, &p.settings, &settings, opts, ID)?;
        if let Some(skipped) = off_names {
            let enabled: Vec<String> = servers.iter().map(|(n, _)| n.clone()).collect();
            super::common::sync_off_list(
                work,
                opts,
                ID,
                (&p.settings, &["mcpServers"]),
                &super::common::OffList {
                    path: &p.settings,
                    key: &["mcp", "excluded"],
                    label: "mcp.excluded",
                },
                &skipped,
                &enabled,
            )?;
        }

        write_instructions(work, &p.instructions, profile, opts, true, ID)?;
        if profile.rules.iter().any(|r| !r.globs.is_empty()) {
            work.info(
                ID,
                "Gemini CLI has no file-scoped rules; rules were folded into GEMINI.md",
            );
        }
        for c in &profile.commands {
            if (1..=9).any(|n| c.body.contains(&format!("${n}"))) {
                work.warn(ID, format!("command '{}' uses positional $N placeholders; Gemini CLI only supports {{{{args}}}}", c.name));
            }
            let path = p.commands.join(format!("{}.toml", c.name));
            super::common::write_command(
                work,
                &path,
                command_toml(c).as_bytes(),
                opts,
                ID,
                &c.name,
            )?;
        }
        for s in &profile.skills {
            let s = skill_with_required_frontmatter(s);
            write_skill(work, &p.skills.join(&s.name), &s, opts, ID)?;
        }
        for a in &profile.agents {
            apply_agent(work, &p.agents, a, opts)?;
        }
        super::hook_files::apply_for(profile, env, opts, work, &super::hook_files::GEMINI)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn command_round_trip() {
        let c = Command {
            name: "changelog".into(),
            description: Some("Draft a changelog entry".into()),
            body: "Write a changelog entry for: $ARGUMENTS\n\nKeep it short.".into(),
            ..Default::default()
        };
        let t = command_toml(&c);
        assert!(t.contains("{{args}}"), "{t}");
        assert_eq!(command_from_toml("changelog", &t).unwrap(), c);
    }
}
