//! Gemini CLI.
//!
//! * MCP + settings: `~/.gemini/settings.json` / `.gemini/settings.json`
//!   (`mcpServers`; `httpUrl` for streamable HTTP, `url` for SSE). Gemini CLI
//!   expands `$VAR` / `${VAR}` in settings, without `:-default` support.
//! * Instructions: `~/.gemini/GEMINI.md` / `./GEMINI.md`; rules are folded in.
//! * Commands: `.gemini/commands/<name>.toml` with `prompt` and `{{args}}`.
//! * Skills: `.gemini/skills/<name>/SKILL.md`.

use std::path::PathBuf;

use anyhow::Result;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};

use super::common::{
    add_command, capture_instructions, capture_skills_dir, import_server, merge_json_servers,
    merge_json_settings, name_from_rel, pick_settings, render_server, safe_settings,
    server_from_json, servers_for, skill_with_required_frontmatter, str_of, write_instructions,
    write_owned, write_skill,
};
use super::{Adapter, ApplyOptions, Captured, Env, Feature, Scope, Support, Work};
use crate::edit::json::parse_value;
use crate::model::{Command, Profile, Transport, is_valid_name};
use crate::plan_fs::{join_rel, read_text_opt, walk_files};
use crate::secrets::RefStyle;

const ID: &str = "gemini-cli";
const MANAGED_KEYS: &[&str] = &["command", "args", "env", "cwd", "url", "httpUrl", "headers"];

pub struct GeminiCli;

struct Paths {
    settings: PathBuf,
    instructions: PathBuf,
    commands: PathBuf,
    skills: PathBuf,
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
    }
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
    fn support(&self, _scope: Scope) -> [(Feature, Support); 6] {
        [
            (Feature::Mcp, Support::Native),
            (Feature::Instructions, Support::Native),
            (Feature::Rules, Support::Folded),
            (Feature::Commands, Support::Native),
            (Feature::Skills, Support::Native),
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
            ("settings", p.settings),
        ]
    }

    fn detect(&self, env: &Env) -> bool {
        paths(env).settings.parent().is_some_and(|d| d.exists())
    }

    fn capture(&self, env: &Env) -> Result<Captured> {
        let mut cap = Captured::default();
        let p = paths(env);
        if let Some(text) = read_text_opt(&p.settings)? {
            cap.sources.push(p.settings.clone());
            let doc = parse_value(Some(&text), &p.settings)?;
            if let Some(Value::Object(servers)) = doc.get("mcpServers") {
                for (name, def) in servers {
                    let Value::Object(def) = def else { continue };
                    let mut s = server_from_json(def);
                    if let Some(http) = str_of(def.get("httpUrl")) {
                        s.url = Some(http);
                    } else if s.url.is_some() {
                        s.transport = Some(Transport::Sse);
                    }
                    import_server(&mut cap, ID, name, s, true);
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
        for rel in walk_files(&p.commands, 500)? {
            if !rel.ends_with(".toml") {
                continue;
            }
            let path = join_rel(&p.commands, &rel);
            // Gemini namespaces nested commands with ':'; profile names use '-'.
            let name = name_from_rel(&rel, ".toml");
            let text = read_text_opt(&path)?.unwrap_or_default();
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
            servers.push((name.clone(), Value::Object(def)));
        }
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

        write_instructions(work, &p.instructions, profile, opts, true)?;
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
            write_owned(
                work,
                &path,
                command_toml(c).as_bytes(),
                opts,
                ID,
                &format!("command '{}'", c.name),
            )?;
        }
        for s in &profile.skills {
            let s = skill_with_required_frontmatter(s);
            write_skill(work, &p.skills.join(&s.name), &s, opts, ID)?;
        }
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
