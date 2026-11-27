//! Hooks in the files of the tools that have them, in one place.
//!
//! Each tool is described by a [`Format`]: where its events are, how an entry
//! is laid out, what its events are called. The code that reads a hook file
//! into the profile (`capture --include hooks`) and writes one back
//! (`apply --apply-hooks`) is the same for all of them.
//!
//! Only `command` hooks are carried (`http`, `mcp_tool`, `prompt` and `agent`
//! hooks are reported and left out). Other keys of a command hook are kept as
//! they are and written back only where the target documents them.
//!
//! | target | file | docs |
//! |---|---|---|
//! | claude-code | `settings.json`, `hooks` | <https://code.claude.com/docs/en/hooks> |
//! | cursor | `~/.cursor/hooks.json`, `.cursor/hooks.json` | <https://cursor.com/docs/agent/hooks> |
//! | codex | `$CODEX_HOME/hooks.json`, `.codex/hooks.json` | <https://developers.openai.com/codex/hooks> |
//! | gemini-cli | `settings.json`, `hooks` | <https://geminicli.com/docs/hooks/> |
//! | grok-build | `$GROK_HOME/hooks/*.json`, `.grok/hooks/*.json` | <https://docs.x.ai/build/features/hooks> |
//! | antigravity | `~/.gemini/config/hooks.json`, `.agents/hooks.json` | <https://antigravity.google/docs/hooks> |
//! | copilot | `$COPILOT_HOME/hooks/*.json`, `.github/hooks/*.json` | <https://docs.github.com/copilot/reference/hooks-reference> |
//! | hermes | `$HERMES_HOME/config.yaml`, `hooks` (shell hooks only, user scope) | <https://hermes-agent.nousresearch.com/docs/user-guide/features/hooks> |
//!
//! Scripts a hook runs are copied into the profile's `hooks/` directory and
//! written back to a `hooks/kitbag/` directory next to the hook file, which the
//! hook command reaches through `{{hooks_dir}}`.

use std::collections::BTreeMap;
use std::path::{Component, Path, PathBuf};

use anyhow::Result;
use serde_json::{Map, Value, json};

use super::common::{skill_file_executable, write_owned};
use super::{ApplyOptions, Captured, Env, HookWrite, Scope, Work};
use crate::edit::json::JsonDoc;
use crate::edit::yaml::YamlDoc;
use crate::hooks;
use crate::model::{Hook, SkillFile};
use crate::plan_fs::{join_rel, read_opt};
use crate::profile_io::{MAX_FILE_BYTES, max_file_size};
use crate::secrets::{
    has_redaction_marker, redact_json_strings, redact_support_file, redact_text, sensitive_file,
};

/// How a tool lays an event's handlers out.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum Layout {
    /// `[ { "matcher": ..., "hooks": [ handler ] } ]`
    Grouped,
    /// `[ handler ]`, with the matcher in the handler.
    Flat,
    /// Grouped for the events in the list, a plain handler list for the rest.
    Mixed(&'static [&'static str]),
}

/// Keys of a hook entry a target writes besides its command and timeout.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum Extras {
    /// Every key, as it is.
    All,
    /// Only these; a hook with another key is not written, except cosmetic
    /// ones (`statusMessage`, `name`, `description`), which are left off.
    Only(&'static [&'static str]),
}

/// What kitbag knows of one tool's hook files.
pub(crate) struct Format {
    pub id: &'static str,
    pub name: &'static str,
    /// Neutral event name and the tool's name for it.
    pub events: &'static [(&'static str, &'static str)],
    /// Every event of the tool (written `<id>:<Name>` in a profile). Events
    /// with a neutral name are in `events` as well.
    pub only: &'static [&'static str],
    pub layout: Layout,
    /// Where the events are in the file.
    pub root: &'static str,
    /// Keys that hold the command, the first one present is used.
    pub command_keys: &'static [&'static str],
    /// The file has `"version": 1`.
    pub version: bool,
    pub timeout_key: &'static str,
    pub timeout_ms: bool,
    pub extras: Extras,
    /// Environment variables a project hook command may start a path with.
    pub project_vars: &'static [&'static str],
    /// How a project hook command names the script directory (`None`: the
    /// tool doesn't say what a command runs relative to, so a hook that runs
    /// a script is not written in a project).
    pub project_scripts: Option<&'static str>,
    /// Set this `cwd` on a handler that runs a script in a project.
    pub project_cwd: Option<&'static str>,
    /// Directory (below the home directory) holding the user's `./` paths.
    pub user_relative: bool,
    /// Directory name of the tool in a project (`.claude`), left out of a script's path in the profile.
    pub dot_dir: &'static str,
    /// What the tool does about hooks it hasn't been told to trust.
    pub trust_note: Option<&'static str>,
    pub quirks: Quirks,
}

/// How a tool's hook file differs from the usual JSON file of `command` hooks.
#[derive(Clone, Copy)]
pub(crate) struct Quirks {
    /// The file is YAML (the hooks are one section of it, edited in place).
    pub yaml: bool,
    /// A handler carries `"type": "command"`.
    pub typed: bool,
    /// Directory (below the tool's config directory) scripts are looked for
    /// in, and written to, in a `kitbag/` subdirectory.
    pub script_dir: &'static str,
    /// Keys next to the events that are other hook systems of the tool, with
    /// what they are called: reported, never captured, never changed.
    pub other_keys: &'static [(&'static str, &'static str)],
    /// The tool documents no project hook file.
    pub user_only: bool,
    /// Events that take a matcher; `None` when every event does.
    pub matcher_events: Option<&'static [&'static str]>,
}

impl Quirks {
    const STANDARD: Quirks = Quirks {
        yaml: false,
        typed: true,
        script_dir: "hooks",
        other_keys: &[],
        user_only: false,
        matcher_events: None,
    };
}

pub(crate) const CLAUDE: Format = Format {
    id: "claude-code",
    name: "Claude Code",
    events: hooks::EVENTS,
    only: hooks::CLAUDE_EVENTS,
    layout: Layout::Grouped,
    root: "hooks",
    command_keys: &["command"],
    version: false,
    timeout_key: "timeout",
    timeout_ms: false,
    extras: Extras::All,
    project_vars: &["CLAUDE_PROJECT_DIR"],
    project_scripts: Some("${CLAUDE_PROJECT_DIR}/.claude/hooks/kitbag"),
    project_cwd: None,
    user_relative: false,
    dot_dir: ".claude",
    trust_note: None,
    quirks: Quirks::STANDARD,
};

pub(crate) const CURSOR: Format = Format {
    id: "cursor",
    name: "Cursor",
    events: &[
        ("session-start", "sessionStart"),
        ("session-end", "sessionEnd"),
        ("prompt-submit", "beforeSubmitPrompt"),
        ("pre-tool", "preToolUse"),
        ("post-tool", "postToolUse"),
        ("post-tool-failure", "postToolUseFailure"),
        ("stop", "stop"),
        ("subagent-start", "subagentStart"),
        ("subagent-stop", "subagentStop"),
        ("pre-compact", "preCompact"),
    ],
    only: &[
        "beforeShellExecution",
        "afterShellExecution",
        "beforeMCPExecution",
        "afterMCPExecution",
        "beforeReadFile",
        "afterFileEdit",
        "afterAgentResponse",
        "afterAgentThought",
        "beforeTabFileRead",
        "afterTabFileEdit",
        "workspaceOpen",
    ],
    layout: Layout::Flat,
    root: "hooks",
    command_keys: &["command"],
    version: true,
    timeout_key: "timeout",
    timeout_ms: false,
    extras: Extras::Only(&["failClosed", "loop_limit"]),
    project_vars: &[],
    project_scripts: Some(".cursor/hooks/kitbag"),
    project_cwd: None,
    user_relative: true,
    dot_dir: ".cursor",
    trust_note: None,
    quirks: Quirks::STANDARD,
};

pub(crate) const CODEX: Format = Format {
    id: "codex",
    name: "Codex",
    events: &[
        ("session-start", "SessionStart"),
        ("session-end", "SessionEnd"),
        ("prompt-submit", "UserPromptSubmit"),
        ("pre-tool", "PreToolUse"),
        ("post-tool", "PostToolUse"),
        ("permission-request", "PermissionRequest"),
        ("stop", "Stop"),
        ("subagent-start", "SubagentStart"),
        ("subagent-stop", "SubagentStop"),
        ("pre-compact", "PreCompact"),
        ("post-compact", "PostCompact"),
    ],
    only: &["PreModelRequest", "PostModelRequest", "Interrupt"],
    layout: Layout::Grouped,
    root: "hooks",
    command_keys: &["command"],
    version: false,
    timeout_key: "timeout",
    timeout_ms: false,
    extras: Extras::Only(&[
        "statusMessage",
        "async",
        "commandWindows",
        "additionalContextLimit",
    ]),
    project_vars: &[],
    project_scripts: Some("$(git rev-parse --show-toplevel)/.codex/hooks/kitbag"),
    project_cwd: None,
    user_relative: false,
    dot_dir: ".codex",
    trust_note: Some(
        "Codex skips a new or changed hook until you review and trust it with /hooks; kitbag doesn't bypass that",
    ),
    quirks: Quirks::STANDARD,
};

pub(crate) const GEMINI: Format = Format {
    id: "gemini-cli",
    name: "Gemini CLI",
    events: &[
        ("session-start", "SessionStart"),
        ("session-end", "SessionEnd"),
        ("pre-tool", "BeforeTool"),
        ("post-tool", "AfterTool"),
        ("notification", "Notification"),
        ("pre-compact", "PreCompress"),
    ],
    only: &[
        "BeforeAgent",
        "AfterAgent",
        "BeforeModel",
        "AfterModel",
        "BeforeToolSelection",
    ],
    layout: Layout::Grouped,
    root: "hooks",
    command_keys: &["command"],
    version: false,
    timeout_key: "timeout",
    timeout_ms: true,
    extras: Extras::Only(&["name", "description"]),
    project_vars: &["GEMINI_PROJECT_DIR"],
    project_scripts: Some("${GEMINI_PROJECT_DIR}/.gemini/hooks/kitbag"),
    project_cwd: None,
    user_relative: false,
    dot_dir: ".gemini",
    trust_note: Some(
        "Gemini CLI warns before it runs a project hook it hasn't seen (hooks are fingerprinted by name and command); kitbag doesn't bypass that",
    ),
    quirks: Quirks::STANDARD,
};

pub(crate) const GROK: Format = Format {
    id: "grok-build",
    name: "Grok Build",
    events: &[
        ("session-start", "SessionStart"),
        ("session-end", "SessionEnd"),
        ("prompt-submit", "UserPromptSubmit"),
        ("pre-tool", "PreToolUse"),
        ("post-tool", "PostToolUse"),
        ("post-tool-failure", "PostToolUseFailure"),
        ("notification", "Notification"),
        ("stop", "Stop"),
        ("subagent-start", "SubagentStart"),
        ("subagent-stop", "SubagentStop"),
        ("pre-compact", "PreCompact"),
        ("post-compact", "PostCompact"),
    ],
    only: &["PermissionDenied", "StopFailure"],
    layout: Layout::Grouped,
    root: "hooks",
    command_keys: &["command"],
    version: false,
    timeout_key: "timeout",
    timeout_ms: false,
    extras: Extras::Only(&[]),
    project_vars: &["GROK_WORKSPACE_ROOT"],
    project_scripts: Some("${GROK_WORKSPACE_ROOT}/.grok/hooks/kitbag"),
    project_cwd: None,
    user_relative: false,
    dot_dir: ".grok",
    trust_note: Some(
        "Grok Build runs project hooks only in a folder you have trusted (/hooks-trust or --trust); kitbag doesn't change that",
    ),
    quirks: Quirks::STANDARD,
};

pub(crate) const ANTIGRAVITY: Format = Format {
    id: "antigravity",
    name: "Antigravity",
    events: &[
        ("session-start", "SessionStart"),
        ("pre-tool", "PreToolUse"),
        ("post-tool", "PostToolUse"),
    ],
    only: &["PreInvocation", "PostInvocation", "Stop"],
    layout: Layout::Mixed(&["PreToolUse", "PostToolUse"]),
    root: "kitbag",
    command_keys: &["command"],
    version: false,
    timeout_key: "timeout",
    timeout_ms: false,
    extras: Extras::Only(&[]),
    project_vars: &[],
    project_scripts: None,
    project_cwd: None,
    user_relative: false,
    dot_dir: ".agents",
    trust_note: None,
    quirks: Quirks::STANDARD,
};

pub(crate) const COPILOT: Format = Format {
    id: "copilot",
    name: "GitHub Copilot",
    events: &[
        ("session-start", "sessionStart"),
        ("session-end", "sessionEnd"),
        ("prompt-submit", "userPromptSubmitted"),
        ("pre-tool", "preToolUse"),
        ("post-tool", "postToolUse"),
        ("post-tool-failure", "postToolUseFailure"),
        ("permission-request", "permissionRequest"),
        ("notification", "notification"),
        ("stop", "agentStop"),
        ("subagent-start", "subagentStart"),
        ("subagent-stop", "subagentStop"),
        ("pre-compact", "preCompact"),
    ],
    only: &["errorOccurred", "userPromptTransformed"],
    layout: Layout::Flat,
    root: "hooks",
    command_keys: &["command", "bash"],
    version: true,
    timeout_key: "timeoutSec",
    timeout_ms: false,
    extras: Extras::Only(&["cwd", "env"]),
    project_vars: &[],
    project_scripts: Some(".github/hooks/kitbag"),
    project_cwd: Some("."),
    user_relative: false,
    dot_dir: ".github",
    trust_note: Some(
        "Copilot CLI loads repository hooks only for a workspace you have admitted; kitbag doesn't change that",
    ),
    quirks: Quirks::STANDARD,
};

/// Hermes Agent's shell hooks: the `hooks:` block of `config.yaml`, a list of
/// `{matcher, command, timeout, fail_closed}` entries under each event.
/// `hooks.outbound` (outbound webhooks) sits in the same block and is left
/// alone; gateway hooks (`hooks/<name>/HOOK.yaml`) and plugin hooks are other
/// systems, not carried. Scripts go to `agent-hooks/kitbag/`, the directory the
/// docs suggest for them, not to `hooks/`, where Hermes loads gateway hooks.
pub(crate) const HERMES: Format = Format {
    id: "hermes",
    name: "Hermes Agent",
    events: &[
        ("session-start", "on_session_start"),
        ("prompt-submit", "pre_llm_call"),
        ("pre-tool", "pre_tool_call"),
        ("post-tool", "post_tool_call"),
        ("subagent-start", "subagent_start"),
        ("subagent-stop", "subagent_stop"),
    ],
    only: &[
        "transform_tool_result",
        "transform_terminal_output",
        "pre_transcription",
        "post_llm_call",
        "transform_llm_output",
        "pre_verify",
        "pre_api_request",
        "post_api_request",
        "api_request_error",
        "pre_auxiliary_call",
        "post_auxiliary_call",
        "on_stream_start",
        "on_stream_delta",
        "on_stream_end",
        "on_interim_message",
        "on_session_open",
        "on_session_end",
        "on_session_finalize",
        "post_compaction",
        "on_session_reset",
        "on_session_delete",
        "post_curator_run",
        "agent_loop_stopped",
        "on_skill_lifecycle",
        "pre_gateway_dispatch",
        "post_gateway_admission",
        "gateway_platform_event",
        "pre_command",
        "pre_approval_request",
        "post_approval_response",
        "on_human_input_request",
        "on_human_input_resolved",
        "on_session_prompt",
        "on_session_prompt_settled",
        "on_room_member_activity",
        "kanban_task_claimed",
        "kanban_task_completed",
        "kanban_task_blocked",
        "on_kanban_worker_spawned",
        "on_kanban_worker_exited",
        "on_kanban_worker_stale_claim",
        "on_kanban_task_updated",
        "on_kanban_dispatch_tick",
    ],
    layout: Layout::Flat,
    root: "hooks",
    command_keys: &["command"],
    version: false,
    timeout_key: "timeout",
    timeout_ms: false,
    extras: Extras::Only(&["fail_closed", "failClosed"]),
    project_vars: &[],
    project_scripts: None,
    project_cwd: None,
    user_relative: false,
    dot_dir: ".hermes",
    trust_note: Some(
        "Hermes asks you to approve each new hook (a foreground `hermes chat` prompts; the gateway, cron and CI need --accept-hooks, HERMES_ACCEPT_HOOKS=1 or an entry in shell-hooks-allowlist.json) and kitbag doesn't write that file or set hooks_auto_accept; `hermes hooks list` shows what is waiting",
    ),
    quirks: Quirks {
        yaml: true,
        typed: false,
        script_dir: "agent-hooks",
        other_keys: &[("outbound", "outbound webhooks")],
        user_only: true,
        matcher_events: Some(&["pre_tool_call", "post_tool_call"]),
    },
};

/// Targets that have no hook file kitbag writes, with the reason. Each reason names
/// what the tool's official docs said when last checked (2026-11-27), so a skip is a
/// documented decision, not "not looked at".
const NO_HOOKS: &[(&str, &str)] = &[
    (
        "claude-desktop",
        "Claude Desktop has no hook configuration of its own; its docs say Claude Code's hooks (settings.json) fire in the Desktop app too, which the claude-code target carries (checked 2026-11-27)",
    ),
    (
        "opencode",
        "opencode runs hooks only as JavaScript/TypeScript plugins, which kitbag doesn't write (plugins are #12; docs checked 2026-11-27)",
    ),
    (
        "windsurf",
        "Windsurf's hooks page now redirects to Devin Desktop's docs, which name only .devin/hooks.v1.json and no Windsurf path or event list; kitbag doesn't write .devin files (checked 2026-11-27)",
    ),
    (
        "zed",
        "Zed's docs (zed.dev/docs/ai) describe no hooks for its agent, so there is no hook file to write (checked 2026-11-27)",
    ),
    (
        "amp",
        "Amp's docs describe event handlers only as TypeScript/JavaScript plugins (amp.on(...)), which kitbag doesn't write (plugins are #12; checked 2026-11-27)",
    ),
    (
        "cline",
        "Cline's hooks page now only points to its SDK plugins, which are TypeScript code and not written by kitbag (plugins are #12); it documents no hook file (checked 2026-11-27)",
    ),
    (
        "roo",
        "Roo Code's docs describe no hooks, so there is no hook file to write (checked 2026-11-27)",
    ),
    (
        "continue",
        "Continue's docs and config.yaml reference describe no hooks, so there is no hook file to write (checked 2026-11-27)",
    ),
];

/// The hook format of the target `id`.
pub(crate) fn format_for(id: &str) -> Option<&'static Format> {
    [
        &CLAUDE,
        &CURSOR,
        &CODEX,
        &GEMINI,
        &GROK,
        &ANTIGRAVITY,
        &COPILOT,
        &HERMES,
    ]
    .into_iter()
    .find(|f| f.id == id)
}

/// Why a target can't carry hooks, or `None` when it can (or is unknown).
pub(crate) fn no_hooks_reason(id: &str) -> Option<&'static str> {
    NO_HOOKS.iter().find(|(t, _)| *t == id).map(|(_, r)| *r)
}

impl Format {
    fn prefix(&self) -> String {
        format!("{}:", self.id)
    }

    /// The tool's name for a neutral event, or `None` when it has none.
    pub fn native_event(&self, event: &str) -> Option<&'static str> {
        if let Some(name) = event.strip_prefix(&self.prefix()) {
            return self.only.iter().copied().find(|e| *e == name);
        }
        self.events
            .iter()
            .find(|(n, _)| *n == event)
            .map(|(_, c)| *c)
    }

    /// The profile's name for one of the tool's events.
    pub fn neutral_event(&self, native: &str) -> Option<String> {
        if let Some((n, _)) = self.events.iter().find(|(_, c)| *c == native) {
            return Some((*n).to_string());
        }
        self.only
            .contains(&native)
            .then(|| format!("{}{native}", self.prefix()))
    }

    fn grouped(&self, event: &str) -> bool {
        match self.layout {
            Layout::Grouped => true,
            Layout::Flat => false,
            Layout::Mixed(list) => list.contains(&event),
        }
    }

    /// A hook file of this tool, parsed (`None` when it can't be read).
    pub fn read_doc(&self, path: &Path) -> Option<Value> {
        let text = std::fs::read_to_string(path).ok()?;
        if self.quirks.yaml {
            crate::edit::yaml::parse_map(Some(&text), path)
                .ok()
                .map(Value::Object)
        } else {
            crate::edit::json::parse_value(Some(&text), path).ok()
        }
    }

    /// The events object of a hook file.
    fn events_of<'a>(&self, doc: &'a Value) -> Option<&'a Map<String, Value>> {
        doc.get(self.root)?.as_object()
    }

    /// Handlers of `event` in a hook file that kitbag would have written for
    /// this matcher and command (what `doctor` looks for).
    pub fn find_handlers<'a>(
        &self,
        doc: &'a Value,
        event: &str,
        matcher: Option<&str>,
        command: &str,
    ) -> Vec<&'a Value> {
        let Some(list) = self
            .events_of(doc)
            .and_then(|m| m.get(event))
            .and_then(Value::as_array)
        else {
            return Vec::new();
        };
        let is_cmd = |h: &&Value| h.get("command").and_then(Value::as_str) == Some(command);
        let mut out = Vec::new();
        for item in list {
            match item.get("hooks").and_then(Value::as_array) {
                Some(inner) if self.grouped(event) => {
                    if item.get("matcher").and_then(Value::as_str) == matcher {
                        out.extend(inner.iter().filter(is_cmd));
                    }
                }
                _ => {
                    if item.get("matcher").and_then(Value::as_str) == matcher && is_cmd(&item) {
                        out.push(item);
                    }
                }
            }
        }
        out
    }
}

// ---------------------------------------------------------------------------
/// Where a tool keeps its hooks at a scope.
struct Where {
    /// Hook files to read (a directory of them is expanded to its `*.json`).
    read: Vec<PathBuf>,
    /// The hook file kitbag writes.
    write: PathBuf,
    /// Where scripts are written.
    scripts: PathBuf,
    /// The tool's config directory (user) or the project: scripts are read from below it.
    base: PathBuf,
}

fn locate(fmt: &Format, env: &Env) -> Where {
    let user = env.scope == Scope::User;
    let (dir, base) = match fmt.id {
        "cursor" => {
            let d = if user {
                env.home.join(".cursor")
            } else {
                env.project.join(".cursor")
            };
            (d.clone(), if user { d } else { env.project.clone() })
        }
        "codex" => {
            let d = if user {
                env.location_var("CODEX_HOME")
                    .unwrap_or_else(|| env.home.join(".codex"))
            } else {
                env.project.join(".codex")
            };
            (d.clone(), if user { d } else { env.project.clone() })
        }
        "gemini-cli" => {
            let d = if user {
                env.home.join(".gemini")
            } else {
                env.project.join(".gemini")
            };
            (d.clone(), if user { d } else { env.project.clone() })
        }
        "grok-build" => {
            let d = if user {
                super::grok_build::grok_home(env)
            } else {
                env.project.join(".grok")
            };
            (d.clone(), if user { d } else { env.project.clone() })
        }
        "antigravity" => {
            let d = if user {
                env.home.join(".gemini").join("config")
            } else {
                env.project.join(".agents")
            };
            (d.clone(), if user { d } else { env.project.clone() })
        }
        "hermes" => {
            // User scope only (see `Quirks::user_only`); the project path is never used.
            let d = super::hermes::hermes_home(env);
            (d.clone(), d)
        }
        // Copilot
        _ => {
            if user {
                let d = env
                    .location_var("COPILOT_HOME")
                    .unwrap_or_else(|| env.home.join(".copilot"));
                (d.clone(), d)
            } else {
                (env.project.join(".github"), env.project.clone())
            }
        }
    };
    let scripts = dir.join(fmt.quirks.script_dir).join("kitbag");
    let (read, write) = match fmt.id {
        "hermes" => {
            let f = dir.join("config.yaml");
            (vec![f.clone()], f)
        }
        "cursor" | "codex" | "antigravity" => {
            let f = dir.join("hooks.json");
            (vec![f.clone()], f)
        }
        "gemini-cli" => {
            let f = dir.join("settings.json");
            (vec![f.clone()], f)
        }
        // A directory of hook files: kitbag writes its own.
        _ => {
            let d = dir.join("hooks");
            (vec![d.clone()], d.join("kitbag.json"))
        }
    };
    Where {
        read,
        write,
        scripts,
        base,
    }
}

/// The `*.json` files of a directory, in name order.
fn json_files(cap: &mut Captured, dir: &Path, id: &str) -> Vec<PathBuf> {
    if !dir.is_dir() || cap.escapes(dir, id) {
        return Vec::new();
    }
    let mut out: Vec<PathBuf> = std::fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|e| e == "json") && p.is_file())
        .collect();
    out.sort();
    out
}

/// Capture the hooks of the tool `fmt` describes at `env`'s scope.
pub(super) fn capture_for(cap: &mut Captured, env: &Env, fmt: &Format) -> Result<()> {
    if fmt.quirks.user_only && env.scope == Scope::Project {
        return Ok(());
    }
    let w = locate(fmt, env);
    let mut files = Vec::new();
    for p in &w.read {
        if p.extension().is_some() {
            files.push(p.clone());
        } else {
            files.extend(json_files(cap, p, fmt.id));
        }
    }
    capture_files(cap, env, fmt, &files, &w.base)
}

/// Plan the hooks of `profile` for the tool `fmt` describes at `env`'s scope.
pub(super) fn apply_for(
    profile: &crate::model::Profile,
    env: &Env,
    opts: &ApplyOptions,
    work: &mut Work,
    fmt: &Format,
) -> Result<()> {
    if fmt.quirks.user_only && env.scope == Scope::Project {
        if !profile.hooks.is_empty() {
            work.warn(
                fmt.id,
                format!(
                    "{} hook(s) not written to {}: its docs name no project hook file, so hooks are written at user scope only. They would run:",
                    profile.hooks.len(),
                    fmt.name
                ),
            );
            for h in &profile.hooks {
                work.info(fmt.id, format!("hook {}", hooks::describe(h)));
                work.hook_skipped
                    .push((fmt.id.to_string(), hooks::describe(h)));
            }
        }
        return Ok(());
    }
    let w = locate(fmt, env);
    apply(profile, env, opts, work, fmt, &w.write, &w.scripts)
}

// ---------------------------------------------------------------------------
// Capture

/// The path a hook command word names, if it names one kitbag can follow:
/// `${CLAUDE_PROJECT_DIR}/...`, `~/...`, `$HOME/...`, an absolute path or (in
/// a project) `./...`.
fn resolve(word: &str, env: &Env, fmt: &Format, base: &Path) -> Option<PathBuf> {
    let project = (env.scope == Scope::Project).then(|| env.project.to_string_lossy().into_owned());
    let home = env.home.to_string_lossy().into_owned();
    let mut w = word.to_string();
    let mut prefixes: Vec<(String, Option<String>)> = Vec::new();
    for var in fmt.project_vars {
        prefixes.push((format!("${{{var}}}"), project.clone()));
        prefixes.push((format!("${var}"), project.clone()));
    }
    prefixes.push(("${HOME}".into(), Some(home.clone())));
    prefixes.push(("$HOME".into(), Some(home.clone())));
    for (pre, with) in prefixes {
        if let Some(rest) = w.strip_prefix(pre.as_str()) {
            w = format!("{}{rest}", with?);
            break;
        }
    }
    if let Some(rest) = w.strip_prefix("~/") {
        w = format!("{home}/{rest}");
    } else if let Some(rest) = w.strip_prefix("./") {
        // Project hooks run from the project; Cursor's user hooks from its config directory.
        let from = match &project {
            Some(p) => p.clone(),
            None if fmt.user_relative => base.to_string_lossy().into_owned(),
            None => return None,
        };
        w = format!("{from}/{rest}");
    }
    // A bare relative path (`.cursor/hooks/x.sh`) is relative to the project.
    if let Some(p) = &project
        && w.contains('/')
        && !w.starts_with(['/', '~', '-'])
        && !w.contains(':')
        && !w.starts_with("./")
    {
        w = format!("{p}/{w}");
    }
    if w.contains(['$', '`']) {
        return None;
    }
    let path = PathBuf::from(w);
    (path.is_absolute() && !path.components().any(|c| c == Component::ParentDir)).then_some(path)
}

/// The profile's path for a script at `path` below `base`: relative to it,
/// without a leading `.claude/` (project) and `hooks/`.
fn profile_path(path: &Path, base: &Path, env: &Env, fmt: &Format) -> Option<String> {
    let rel = path.strip_prefix(base).ok()?;
    let parts: Vec<String> = rel
        .components()
        .map(|c| c.as_os_str().to_string_lossy().into_owned())
        .collect();
    let mut parts: &[String] = &parts;
    if env.scope == Scope::Project && parts.first().is_some_and(|p| p == fmt.dot_dir) {
        parts = &parts[1..];
    }
    if parts.len() > 1 && parts[0] == fmt.quirks.script_dir {
        parts = &parts[1..];
        // Where an earlier apply put them.
        if parts.len() > 1 && parts[0] == "kitbag" {
            parts = &parts[1..];
        }
    }
    let rel = parts.join("/");
    hooks::safe_script_path(&rel).then_some(rel)
}

/// Copy the script of `hook` into the profile and point the hook at the copy.
/// Returns the hook as it goes in the profile.
fn take_script(
    cap: &mut Captured,
    env: &Env,
    fmt: &Format,
    base: &Path,
    hook: Hook,
    seen: &mut BTreeMap<String, PathBuf>,
) -> Hook {
    let mut words = vec![hook.command.clone()];
    let exec_form = matches!(hook.extra.get("args"), Some(Value::Array(_)));
    if let Some(Value::Array(a)) = hook.extra.get("args") {
        words.extend(a.iter().filter_map(Value::as_str).map(str::to_string));
    }
    if !exec_form {
        words = hooks::shell_words(&hook.command);
    }
    let Some(word) = hooks::script_word(&words).map(str::to_string) else {
        return hook;
    };
    let Some(path) = resolve(&word, env, fmt, base) else {
        return hook;
    };
    let shown = path.display().to_string();
    if !path.is_file() {
        if word.contains('/') {
            cap.warn(
                fmt.id,
                format!(
                    "hook {}: the script {shown} was not found, so the command is kept as it is",
                    hook.event
                ),
            );
        }
        return hook;
    }
    if !path.starts_with(base) {
        cap.warn(
            fmt.id,
            format!(
                "hook {}: {shown} is outside {}, so it was not copied into the profile; the command keeps a path that only exists on this machine",
                hook.event,
                base.display()
            ),
        );
        return hook;
    }
    if env.scope == Scope::Project && cap.escapes(&path, fmt.id) {
        return hook;
    }
    let skip = |cap: &mut Captured, why: String| {
        cap.warn(
            fmt.id,
            format!(
                "hook {}: script {shown} was not copied ({why}); the command is kept as it is",
                hook.event
            ),
        );
    };
    let Some(rel) = profile_path(&path, base, env, fmt) else {
        skip(
            cap,
            "its path has characters kitbag can't keep portable, or it is not below the tool's config directory".into(),
        );
        return hook;
    };
    match std::fs::metadata(&path) {
        Ok(m) if m.len() > MAX_FILE_BYTES => {
            skip(
                cap,
                format!(
                    "{} KiB is over the {} limit for profile files",
                    m.len() / 1024,
                    max_file_size()
                ),
            );
            return hook;
        }
        Err(e) => {
            skip(cap, e.to_string());
            return hook;
        }
        _ => {}
    }
    let bytes = match read_opt(&path) {
        Ok(Some(b)) => b,
        Ok(None) => return hook,
        Err(e) => {
            skip(cap, format!("{e:#}"));
            return hook;
        }
    };
    if let Some(why) = sensitive_file(&rel, &bytes) {
        skip(cap, why);
        return hook;
    }
    let content = match String::from_utf8(bytes) {
        Ok(text) => {
            let (red, kinds) = redact_support_file(&rel, &text);
            if !kinds.is_empty() {
                cap.warn(
                    fmt.id,
                    format!("redacted {} from hook script hooks/{rel}", kinds.join(", ")),
                );
            }
            red.into_bytes()
        }
        Err(e) => e.into_bytes(),
    };
    if let Some(other) = seen.get(&rel)
        && *other != path
    {
        skip(
            cap,
            format!("{} is already captured as hooks/{rel}", other.display()),
        );
        return hook;
    }
    seen.insert(rel.clone(), path.clone());
    if !cap.profile.hook_files.iter().any(|f| f.path == rel) {
        cap.sources.push(path.clone());
        cap.profile.hook_files.push(SkillFile {
            path: rel.clone(),
            content,
            executable: skill_file_executable(&path),
        });
    }
    hooks::with_script_ref(&hook, &word, &rel)
}

/// The event objects of a hook file: the `hooks` of most tools, or, where the
/// file names its hooks (Antigravity), the events of each hook that isn't
/// switched off.
fn event_maps<'a>(
    cap: &mut Captured,
    fmt: &Format,
    doc: &'a Map<String, Value>,
) -> Vec<&'a Map<String, Value>> {
    if matches!(fmt.layout, Layout::Mixed(_)) {
        let mut out = Vec::new();
        for (name, v) in doc {
            let Value::Object(m) = v else { continue };
            if m.get("enabled") == Some(&Value::Bool(false)) {
                cap.info(
                    fmt.id,
                    format!("hook '{name}' is switched off (enabled: false); not captured"),
                );
                continue;
            }
            out.push(m);
        }
        return out;
    }
    match doc.get(fmt.root) {
        None => Vec::new(),
        Some(Value::Object(m)) => vec![m],
        Some(_) => {
            cap.warn(
                fmt.id,
                format!("'{}' is not an object; no hooks captured", fmt.root),
            );
            Vec::new()
        }
    }
}

/// The handlers in an event's list, each with its matcher: a group's
/// handlers carry the group's, a plain handler its own.
fn handlers_in(list: &[Value]) -> Vec<(Option<String>, &Map<String, Value>, bool)> {
    let mut out = Vec::new();
    for item in list {
        let Value::Object(o) = item else { continue };
        let matcher = o.get("matcher").and_then(Value::as_str).map(str::to_string);
        match o.get("hooks").and_then(Value::as_array) {
            Some(inner) => out.extend(inner.iter().filter_map(|h| match h {
                Value::Object(h) => Some((matcher.clone(), h, false)),
                _ => None,
            })),
            None => out.push((matcher, o, true)),
        }
    }
    out
}

/// Read the hooks of a hook file into the profile (`capture --include
/// hooks`): command hooks only, their scripts copied, everything redacted.
/// `label` names the file in notes.
pub(super) fn capture(
    cap: &mut Captured,
    env: &Env,
    fmt: &Format,
    doc: &Map<String, Value>,
    base: &Path,
    label: &str,
) {
    let mut seen = BTreeMap::new();
    let mut count = 0;
    for events in event_maps(cap, fmt, doc) {
        for (event, groups) in events {
            if let Some((_, what)) = fmt
                .quirks
                .other_keys
                .iter()
                .find(|(k, _)| *k == event.as_str())
            {
                let n = groups.as_array().map_or(0, Vec::len);
                cap.warn(
                    fmt.id,
                    format!(
                        "hooks: the {what} ({}.{event}, {n} entr{}) were not captured: kitbag doesn't carry them, and apply leaves them as they are",
                        fmt.root,
                        if n == 1 { "y" } else { "ies" }
                    ),
                );
                continue;
            }
            let Some(neutral) = fmt.neutral_event(event) else {
                if event != "enabled" {
                    cap.warn(
                        fmt.id,
                        format!(
                            "hooks: skipped event '{event}', which kitbag doesn't know for {}",
                            fmt.name
                        ),
                    );
                }
                continue;
            };
            let Value::Array(groups) = groups else {
                cap.warn(fmt.id, format!("hooks: '{event}' is not a list; skipped"));
                continue;
            };
            for (matcher, h, own_matcher) in handlers_in(groups) {
                let ty = h.get("type").and_then(Value::as_str).unwrap_or("command");
                if ty != "command" {
                    cap.warn(
                        fmt.id,
                        format!("hooks: a {ty} hook on {event} was not captured (kitbag carries command hooks only)"),
                    );
                    continue;
                }
                let command_key = fmt
                    .command_keys
                    .iter()
                    .find(|k| h.get(**k).and_then(Value::as_str).is_some());
                let Some((command_key, command)) =
                    command_key.and_then(|k| h.get(*k).and_then(Value::as_str).map(|c| (*k, c)))
                else {
                    cap.warn(
                        fmt.id,
                        format!("hooks: a hook on {event} has no command (kitbag carries command hooks only); skipped"),
                    );
                    continue;
                };
                let timeout = [fmt.timeout_key, "timeout"]
                    .iter()
                    .find_map(|k| h.get(*k).and_then(Value::as_u64))
                    .filter(|t| *t > 0)
                    .map(|t| if fmt.timeout_ms { t.div_ceil(1000) } else { t });
                let extra: Map<String, Value> = h
                    .iter()
                    .filter(|(k, v)| match k.as_str() {
                        "type" => false,
                        "matcher" => !own_matcher,
                        k if k == command_key => false,
                        k if k == fmt.timeout_key || k == "timeout" => {
                            timeout.is_none() || !v.is_number()
                        }
                        _ => true,
                    })
                    .map(|(k, v)| (k.clone(), v.clone()))
                    .collect();
                let hook = Hook {
                    event: neutral.clone(),
                    matcher: matcher.clone(),
                    command: command.to_string(),
                    timeout,
                    extra,
                };
                let mut hook = take_script(cap, env, fmt, base, hook, &mut seen);
                let home = env.home.to_string_lossy();
                if !home.is_empty() && hook.command.contains(home.as_ref()) {
                    cap.warn(
                        fmt.id,
                        format!(
                            "hook {}: the command mentions {home}, which is specific to this machine; edit it in the profile",
                            hook.event
                        ),
                    );
                }
                let (red, mut kinds) = redact_text(&hook.command);
                hook.command = red;
                for v in hook.extra.values_mut() {
                    kinds.extend(redact_json_strings(v));
                }
                kinds.dedup();
                if !kinds.is_empty() {
                    cap.warn(
                        fmt.id,
                        format!("redacted {} from a hook on {event}", kinds.join(", ")),
                    );
                }
                if !cap.profile.hooks.contains(&hook) {
                    cap.profile.hooks.push(hook);
                    count += 1;
                }
            }
        }
    }
    if count > 0 {
        let scripts = cap.profile.hook_files.len();
        cap.info(
            fmt.id,
            format!(
                "captured {count} hook(s) and {scripts} script(s) from {label}; they run commands by themselves, so applying them needs --apply-hooks"
            ),
        );
    }
}

/// How many hooks a hook file has (for the note when they are not captured).
pub(super) fn count(fmt: &Format, doc: &Map<String, Value>) -> usize {
    let events: Vec<&Map<String, Value>> = if matches!(fmt.layout, Layout::Mixed(_)) {
        doc.values().filter_map(Value::as_object).collect()
    } else {
        doc.get(fmt.root)
            .and_then(Value::as_object)
            .into_iter()
            .collect()
    };
    events
        .into_iter()
        .flat_map(|m| m.iter())
        .filter(|(k, _)| !fmt.quirks.other_keys.iter().any(|(o, _)| o == k))
        .filter_map(|(_, v)| v.as_array())
        .map(|l| handlers_in(l).len())
        .sum()
}

/// Capture the hooks of the hook files at `paths` (each one a JSON file),
/// or, without `--include hooks`, say how many there are.
pub(super) fn capture_files(
    cap: &mut Captured,
    env: &Env,
    fmt: &Format,
    paths: &[PathBuf],
    base: &Path,
) -> Result<()> {
    let mut left_out = 0;
    for path in paths {
        let Some(text) = cap.read_config(path, fmt.id)? else {
            continue;
        };
        let parsed = if fmt.quirks.yaml {
            crate::edit::yaml::parse_map(Some(&text), path).map(Value::Object)
        } else {
            crate::edit::json::parse_value(Some(&text), path)
        };
        let doc = match parsed {
            Ok(Value::Object(m)) => m,
            Ok(_) => {
                cap.warn(
                    fmt.id,
                    format!(
                        "{} is not a {} object; no hooks read from it",
                        path.display(),
                        if fmt.quirks.yaml { "YAML" } else { "JSON" }
                    ),
                );
                continue;
            }
            Err(e) => {
                cap.warn(
                    fmt.id,
                    format!("could not read hooks from {}: {e:#}", path.display()),
                );
                continue;
            }
        };
        if env.include_hooks {
            cap.sources.push(path.clone());
            let label = path.file_name().map_or_else(
                || path.display().to_string(),
                |n| n.to_string_lossy().into_owned(),
            );
            capture(cap, env, fmt, &doc, base, &label);
        } else {
            left_out += count(fmt, &doc);
        }
    }
    if left_out > 0 {
        cap.info(
            fmt.id,
            format!(
                "{left_out} hook(s) were not captured: they run commands by themselves, so capture only reads them with --include hooks"
            ),
        );
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Apply

fn sha(bytes: &[u8]) -> String {
    crate::txn::sha256(Some(bytes)).unwrap_or_default()
}

/// Keys that only label a hook; a target that doesn't have them leaves them off.
const COSMETIC: &[&str] = &["statusMessage", "name", "description"];

/// The handler as it goes into the hook file, or why the target can't have
/// it, with what was left off.
fn handler(
    fmt: &Format,
    h: &Hook,
    flat: bool,
    cwd: Option<&str>,
) -> Result<(Value, Vec<String>), String> {
    let mut m = Map::new();
    if fmt.quirks.typed {
        m.insert("type".into(), json!("command"));
    }
    m.insert("command".into(), json!(h.command));
    if flat && let Some(x) = &h.matcher {
        m.insert("matcher".into(), json!(x));
    }
    if let Some(t) = h.timeout {
        let t = if fmt.timeout_ms {
            t.saturating_mul(1000)
        } else {
            t
        };
        m.insert(fmt.timeout_key.into(), json!(t));
    }
    let mut dropped = Vec::new();
    for (k, v) in &h.extra {
        match fmt.extras {
            Extras::All => {
                m.insert(k.clone(), v.clone());
            }
            Extras::Only(keys) if keys.contains(&k.as_str()) => {
                m.insert(k.clone(), v.clone());
            }
            Extras::Only(_) if COSMETIC.contains(&k.as_str()) => dropped.push(k.clone()),
            Extras::Only(_) => {
                return Err(format!(
                    "its '{k}' setting is not one {} documents, and leaving it off would change what the hook does",
                    fmt.name
                ));
            }
        }
    }
    if let Some(c) = cwd
        && !m.contains_key("cwd")
    {
        m.insert("cwd".into(), json!(c));
    }
    Ok((Value::Object(m), dropped))
}

/// The text standing for the script directory in a command: the variable
/// the tool expands itself in a project, or the directory. `None` when a
/// project hook can't reach scripts.
fn script_dir_text(env: &Env, fmt: &Format, dir: &Path) -> Option<String> {
    match env.scope {
        Scope::Project => fmt.project_scripts.map(str::to_string),
        Scope::User => Some(dir.to_string_lossy().replace('\\', "/")),
    }
}

/// Plan the hooks of `profile` for the tool `fmt` describes: list every one
/// in the plan, and, with `--apply-hooks`, write the scripts into
/// `scripts_dir` and the entries into the hook file `file`.
pub(super) fn apply(
    profile: &crate::model::Profile,
    env: &Env,
    opts: &ApplyOptions,
    work: &mut Work,
    fmt: &Format,
    file: &Path,
    scripts_dir: &Path,
) -> Result<()> {
    if profile.hooks.is_empty() {
        return Ok(());
    }
    let id = fmt.id;
    let dir_text = script_dir_text(env, fmt, scripts_dir);
    let shown_dir = dir_text
        .clone()
        .unwrap_or_else(|| scripts_dir.to_string_lossy().replace('\\', "/"));
    let files: BTreeMap<&str, &SkillFile> = profile
        .hook_files
        .iter()
        .map(|f| (f.path.as_str(), f))
        .collect();
    let describe = |h: &Hook| {
        let scripts: Vec<String> = hooks::referenced_files(h)
            .iter()
            .map(|f| match files.get(f.as_str()) {
                Some(file) => format!(
                    "runs the profile's script hooks/{f} ({} bytes, copied to {})",
                    file.content.len(),
                    join_rel(scripts_dir, f).display()
                ),
                None => format!("runs hooks/{f}, which the profile does not have"),
            })
            .collect();
        // As it would be written, not with the profile's placeholder.
        let shown = hooks::with_script_dir(h, &shown_dir).unwrap_or_else(|| h.clone());
        let mut s = hooks::describe(&shown);
        if !scripts.is_empty() {
            s.push_str(&format!(" [{}]", scripts.join("; ")));
        }
        s
    };
    if !opts.apply_hooks {
        work.warn(
            id,
            format!(
                "{} hook(s) in the profile not applied to {}: hooks run commands by themselves; pass --apply-hooks to apply them (nothing is written for them without it). They would run:",
                profile.hooks.len(),
                fmt.name
            ),
        );
        for h in &profile.hooks {
            work.info(id, format!("hook {}", describe(h)));
        }
        return Ok(());
    }

    // A script capture redacted a secret from would run with a marker where
    // the secret was: neither it nor the hooks that run it are applied.
    let redacted = |bytes: &[u8]| std::str::from_utf8(bytes).is_ok_and(has_redaction_marker);
    let broken_scripts: Vec<&str> = files
        .iter()
        .filter(|(_, f)| redacted(&f.content))
        .map(|(rel, _)| *rel)
        .collect();
    for rel in &broken_scripts {
        work.warn(
            id,
            format!(
                "hook script hooks/{rel} not written: capture removed a secret from it ([REDACTED] marker); edit it in the profile to read the secret from the environment"
            ),
        );
    }
    // The scripts first: the hooks that run them are only added if they can be.
    let mut script_hashes: BTreeMap<String, String> = BTreeMap::new();
    let mut script_changed: Vec<String> = Vec::new();
    if dir_text.is_some() {
        for (rel, f) in files
            .iter()
            .filter(|(rel, _)| !broken_scripts.contains(rel))
        {
            let path = join_rel(scripts_dir, rel);
            if write_owned(
                work,
                &path,
                &f.content,
                opts,
                id,
                &format!("hook script '{rel}'"),
            )? {
                if f.executable {
                    work.fs.mark_executable(&path)?;
                }
                script_changed.push((*rel).to_string());
            }
            script_hashes.insert((*rel).to_string(), sha(&f.content));
        }
    }

    let text = work.fs.read(file)?;
    let mut doc = if fmt.quirks.yaml {
        let doc = YamlDoc::parse(text.as_deref(), file)?;
        if let Err(why) = doc.check_editable(fmt.root) {
            work.warn(
                id,
                format!(
                    "hooks not written to {}: {why}, which kitbag can't edit in place; add them by hand",
                    file.display()
                ),
            );
            work.hook_skipped.extend(
                profile
                    .hooks
                    .iter()
                    .map(|h| (id.to_string(), hooks::describe(h))),
            );
            return Ok(());
        }
        HookDoc::Yaml(doc)
    } else {
        let doc = JsonDoc::parse(text.as_deref(), file)?;
        doc.check_unique_keys(&[], &[fmt.root])?;
        HookDoc::Json(doc)
    };
    let mut all = match doc.events(fmt.root) {
        None => Map::new(),
        Some(Value::Object(m)) => m,
        Some(_) => {
            work.warn(
                id,
                format!(
                    "'{}' in {} is not an object, so no hooks were written",
                    fmt.root,
                    file.display()
                ),
            );
            work.hook_skipped.extend(
                profile
                    .hooks
                    .iter()
                    .map(|h| (id.to_string(), hooks::describe(h))),
            );
            return Ok(());
        }
    };
    let before = all.clone();
    let mut changed = false;
    let mut written = 0;
    let mut landed: Vec<usize> = Vec::new();
    let mut matchers = false;
    let mut writes: Vec<HookWrite> = Vec::new();
    for (n, h) in profile.hooks.iter().enumerate() {
        let what = describe(h);
        let Some(event) = fmt.native_event(&h.event) else {
            work.warn(
                id,
                format!(
                    "hook not applied: '{}' has no equivalent event in {} ({what})",
                    h.event, fmt.name
                ),
            );
            continue;
        };
        let marked = has_redaction_marker(&h.command)
            || h.extra.values().any(|v| {
                crate::secrets::json_strings(v)
                    .into_iter()
                    .any(has_redaction_marker)
            });
        if marked
            || hooks::referenced_files(h)
                .iter()
                .any(|f| broken_scripts.contains(&f.as_str()))
        {
            work.warn(
                id,
                format!(
                    "hook not applied: capture removed a secret from it or its script ([REDACTED] marker); edit it in the profile to read the secret from the environment ({what})"
                ),
            );
            continue;
        }
        let rels = hooks::referenced_files(h);
        let missing: Vec<String> = rels
            .iter()
            .filter(|f| !files.contains_key(f.as_str()))
            .cloned()
            .collect();
        if !missing.is_empty() {
            work.warn(
                id,
                format!(
                    "hook not applied: its script hooks/{} is not in the profile ({what})",
                    missing.join(", hooks/")
                ),
            );
            continue;
        }
        let Some(dir_text) = dir_text.as_deref().or(rels.is_empty().then_some("")) else {
            work.warn(
                id,
                format!(
                    "hook not applied: {}'s docs don't say what a project hook command runs relative to, so it can't reach its script (user scope works) ({what})",
                    fmt.name
                ),
            );
            continue;
        };
        let rendered = match hooks::with_script_dir(h, dir_text) {
            Some(r) => r,
            None => {
                work.warn(
                    id,
                    format!(
                        "hook not applied: the script directory {dir_text} has characters that can't be put in a command safely ({what})"
                    ),
                );
                continue;
            }
        };
        let grouped = fmt.grouped(event);
        let cwd = (env.scope == Scope::Project && !rels.is_empty())
            .then_some(fmt.project_cwd)
            .flatten();
        let takes_matcher = fmt
            .quirks
            .matcher_events
            .is_none_or(|events| events.contains(&event));
        let matcher = if (grouped || matches!(fmt.layout, Layout::Flat)) && takes_matcher {
            h.matcher.clone()
        } else {
            if h.matcher.is_some() {
                work.info(
                    id,
                    format!(
                        "hook matcher left off: {} ignores matchers on {event} ({what})",
                        fmt.name
                    ),
                );
            }
            None
        };
        let rendered = Hook {
            matcher: matcher.clone(),
            ..rendered
        };
        let (entry, dropped) = match handler(fmt, &rendered, !grouped, cwd) {
            Ok(x) => x,
            Err(why) => {
                work.warn(
                    id,
                    format!("hook not applied to {}: {why} ({what})", fmt.name),
                );
                continue;
            }
        };
        if !dropped.is_empty() {
            work.info(
                id,
                format!(
                    "hook written without {} (not keys {} has) ({what})",
                    dropped.join(", "),
                    fmt.name
                ),
            );
        }
        matchers |= matcher.is_some();
        work.info(id, format!("hook {what}"));
        let Value::Array(list) = all
            .entry(event.to_string())
            .or_insert_with(|| Value::Array(Vec::new()))
        else {
            work.warn(
                id,
                format!(
                    "'{}.{event}' in {} is not a list; hook not applied",
                    fmt.root,
                    file.display()
                ),
            );
            continue;
        };
        let mut wrote = false;
        let is_command = |x: &Value| {
            x.get("command").and_then(Value::as_str) == Some(rendered.command.as_str())
                && x.get("type")
                    .and_then(Value::as_str)
                    .is_none_or(|t| t == "command")
        };
        // The list of handlers this one goes in, or none when it is a new group.
        let target: Option<&mut Vec<Value>> = if grouped {
            let at = list
                .iter()
                .position(|g| g.get("matcher").and_then(Value::as_str) == matcher.as_deref());
            match at {
                None => None,
                Some(i) => match list[i].get_mut("hooks") {
                    Some(Value::Array(inner)) => Some(inner),
                    _ => {
                        work.warn(
                            id,
                            format!(
                                "a hooks entry for {event} in {} has no 'hooks' list; hook not applied",
                                file.display()
                            ),
                        );
                        continue;
                    }
                },
            }
        } else {
            Some(list)
        };
        match target {
            None => {
                let mut g = Map::new();
                if let Some(m) = &matcher {
                    g.insert("matcher".into(), json!(m));
                }
                g.insert("hooks".into(), json!([entry.clone()]));
                list.push(Value::Object(g));
                wrote = true;
            }
            Some(handlers) => {
                let same = handlers.iter().position(|x| {
                    is_command(x)
                        && (grouped
                            || x.get("matcher").and_then(Value::as_str) == matcher.as_deref())
                });
                match same {
                    None => {
                        handlers.push(entry.clone());
                        wrote = true;
                    }
                    Some(j) if handlers[j] == entry => {}
                    Some(j) if opts.overwrite => {
                        handlers[j] = entry.clone();
                        wrote = true;
                    }
                    Some(_) => work.warn(
                        id,
                        format!(
                            "kept the existing hook with the same command in {} because --no-overwrite was given ({what})",
                            file.display()
                        ),
                    ),
                }
            }
        }
        written += 1;
        landed.push(n);
        if wrote || rels.iter().any(|r| script_changed.contains(r)) {
            changed |= wrote;
            writes.push(HookWrite {
                target: id.to_string(),
                path: file.to_path_buf(),
                event: event.to_string(),
                matcher,
                command: rendered.command.clone(),
                hash: crate::sync::entry_hash(&entry),
                scripts: rels
                    .iter()
                    .map(|r| (join_rel(scripts_dir, r), script_hashes[r].clone()))
                    .collect(),
            });
        }
    }
    if matchers && fmt.id != CLAUDE.id {
        work.warn(
            id,
            format!(
                "hook matchers are written to {} as they are; tools name their tools differently (Bash, run_command, shell ...), so check that they match {}'s",
                fmt.name, fmt.name
            ),
        );
    }
    if written > 0
        && let Some(note) = fmt.trust_note
    {
        work.info(id, note);
    }
    if written > 0 && changed {
        for (key, what) in fmt.quirks.other_keys {
            if let Some(Value::Array(list)) = all.get(*key) {
                work.info(
                    id,
                    format!(
                        "the {what} ({}.{key}, {} entr{}) in {} were left as they are; kitbag doesn't carry them",
                        fmt.root,
                        list.len(),
                        if list.len() == 1 { "y" } else { "ies" },
                        file.display()
                    ),
                );
            }
        }
    }
    if changed {
        match &mut doc {
            HookDoc::Json(doc) => {
                doc.set(&[fmt.root], &Value::Object(all));
                if fmt.version && doc.get(&["version"]).is_none() {
                    doc.set(&["version"], &json!(1));
                }
                work.fs.write(file, doc.render())?;
            }
            HookDoc::Yaml(doc) => {
                // Only the events that changed are rewritten; the rest of the
                // block (and of the file) stays as it is.
                for (event, list) in &all {
                    if before.get(event) != Some(list) {
                        doc.set(fmt.root, event, list)?;
                    }
                }
                work.fs.write(file, doc.render()?)?;
            }
        }
    }
    work.hook_writes.extend(writes);
    work.hook_skipped.extend(
        profile
            .hooks
            .iter()
            .enumerate()
            .filter(|(n, _)| !landed.contains(n))
            .map(|(_, h)| (id.to_string(), hooks::describe(h))),
    );
    Ok(())
}

/// The hook file being edited.
enum HookDoc {
    Json(JsonDoc),
    Yaml(YamlDoc),
}

impl HookDoc {
    /// The value of the events object, as it is now.
    fn events(&self, root: &str) -> Option<Value> {
        match self {
            HookDoc::Json(d) => d.get(&[root]),
            HookDoc::Yaml(d) => d.get(root).filter(|v| !v.is_null()).cloned(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const ALL: [&Format; 8] = [
        &CLAUDE,
        &CURSOR,
        &CODEX,
        &GEMINI,
        &GROK,
        &ANTIGRAVITY,
        &COPILOT,
        &HERMES,
    ];

    #[test]
    fn events_map_both_ways_for_every_tool() {
        for f in ALL {
            for (neutral, native) in f.events {
                assert!(
                    hooks::EVENTS.iter().any(|(n, _)| n == neutral),
                    "{}: '{neutral}' is not a neutral name",
                    f.id
                );
                assert_eq!(f.native_event(neutral), Some(*native), "{}", f.id);
                assert_eq!(
                    f.neutral_event(native).as_deref(),
                    Some(*neutral),
                    "{}",
                    f.id
                );
            }
            for native in f
                .only
                .iter()
                .filter(|n| !f.events.iter().any(|(_, c)| c == *n))
            {
                let name = format!("{}:{native}", f.id);
                assert_eq!(f.native_event(&name), Some(*native), "{name}");
                assert_eq!(f.neutral_event(native).as_deref(), Some(name.as_str()));
            }
            assert_eq!(f.native_event("on-save"), None);
            assert_eq!(f.neutral_event("MadeUp"), None);
        }
    }

    #[test]
    fn a_tools_own_events_do_not_cross_over_to_the_others() {
        assert_eq!(
            CURSOR.native_event("cursor:beforeShellExecution"),
            Some("beforeShellExecution")
        );
        assert_eq!(CODEX.native_event("cursor:beforeShellExecution"), None);
        assert_eq!(CURSOR.native_event("claude-code:FileChanged"), None);
        assert_eq!(
            CLAUDE.native_event("claude-code:FileChanged"),
            Some("FileChanged")
        );
        // A name without its prefix isn't one of the tool's events either.
        assert_eq!(CURSOR.native_event("beforeShellExecution"), None);
        // The same neutral event, in each tool's words.
        let pre: Vec<_> = ALL.iter().map(|f| f.native_event("pre-tool")).collect();
        assert_eq!(
            pre,
            [
                Some("PreToolUse"),
                Some("preToolUse"),
                Some("PreToolUse"),
                Some("BeforeTool"),
                Some("PreToolUse"),
                Some("PreToolUse"),
                Some("preToolUse"),
                Some("pre_tool_call"),
            ]
        );
        // Events without a documented counterpart are not guessed.
        assert_eq!(GEMINI.native_event("stop"), None);
        assert_eq!(ANTIGRAVITY.native_event("stop"), None);
        assert_eq!(CURSOR.native_event("post-compact"), None);
    }

    #[test]
    fn hermes_events_are_the_documented_equivalents_and_the_rest_are_its_own() {
        assert_eq!(HERMES.native_event("pre-tool"), Some("pre_tool_call"));
        assert_eq!(HERMES.native_event("prompt-submit"), Some("pre_llm_call"));
        // The docs don't tie these two to `session-end` / `post-compact`.
        assert_eq!(HERMES.native_event("session-end"), None);
        assert_eq!(HERMES.native_event("post-compact"), None);
        assert_eq!(
            HERMES.native_event("hermes:on_session_finalize"),
            Some("on_session_finalize")
        );
        assert_eq!(
            HERMES.neutral_event("post_compaction").as_deref(),
            Some("hermes:post_compaction")
        );
        // `on_session_end` fires after every turn, so it is not `session-end`.
        assert_eq!(
            HERMES.native_event("hermes:on_session_end"),
            Some("on_session_end")
        );
        assert_eq!(
            HERMES.neutral_event("on_session_end").as_deref(),
            Some("hermes:on_session_end")
        );
        // No documented counterpart: not guessed.
        for e in [
            "stop",
            "post-tool-failure",
            "notification",
            "pre-compact",
            "permission-request",
            "session-end",
            "post-compact",
        ] {
            assert_eq!(HERMES.native_event(e), None, "{e}");
        }
        // Shell hooks can't register this one.
        assert_eq!(
            HERMES.native_event("hermes:transform_api_error_classification"),
            None
        );
        // Outbound webhooks are not an event.
        assert_eq!(HERMES.neutral_event("outbound"), None);
        assert_eq!(HERMES.quirks.script_dir, "agent-hooks");
    }

    #[test]
    fn hermes_hooks_are_found_in_a_parsed_config() {
        let doc = json!({"hooks": {"pre_tool_call": [
            {"matcher": "terminal", "command": "a", "timeout": 5}]}});
        assert_eq!(
            HERMES
                .find_handlers(&doc, "pre_tool_call", Some("terminal"), "a")
                .len(),
            1
        );
        assert!(
            HERMES
                .find_handlers(&doc, "pre_tool_call", None, "a")
                .is_empty()
        );
        // Outbound webhooks are not shell hooks.
        let doc = yaml_doc(
            "hooks:\n  outbound:\n  - url: https://x\n    events: [a]\n  on_session_start:\n  - command: c\n",
        );
        assert_eq!(count(&HERMES, doc.as_object().unwrap()), 1);
    }

    fn yaml_doc(text: &str) -> Value {
        Value::Object(crate::edit::yaml::parse_map(Some(text), Path::new("c.yaml")).unwrap())
    }

    #[test]
    fn handlers_are_found_in_every_layout() {
        let grouped = json!({"hooks": {"PreToolUse": [
            {"matcher": "Bash", "hooks": [{"type": "command", "command": "a"}]}]}});
        assert_eq!(
            CODEX
                .find_handlers(&grouped, "PreToolUse", Some("Bash"), "a")
                .len(),
            1
        );
        assert!(
            CODEX
                .find_handlers(&grouped, "PreToolUse", None, "a")
                .is_empty()
        );
        let flat = json!({"hooks": {"preToolUse": [{"matcher": "x", "command": "a"}]}});
        assert_eq!(
            CURSOR
                .find_handlers(&flat, "preToolUse", Some("x"), "a")
                .len(),
            1
        );
        assert!(
            CURSOR
                .find_handlers(&flat, "preToolUse", Some("y"), "a")
                .is_empty()
        );
        let named = json!({"kitbag": {
            "PreToolUse": [{"matcher": "m", "hooks": [{"command": "a"}]}],
            "SessionStart": [{"command": "b"}]}});
        assert_eq!(
            ANTIGRAVITY
                .find_handlers(&named, "PreToolUse", Some("m"), "a")
                .len(),
            1
        );
        assert_eq!(
            ANTIGRAVITY
                .find_handlers(&named, "SessionStart", None, "b")
                .len(),
            1
        );
    }

    #[test]
    fn every_skip_reason_is_dated_and_none_is_a_guess() {
        for a in crate::adapters::all() {
            if let Some(r) = no_hooks_reason(a.id()) {
                assert!(r.contains("checked 2026-11-27"), "{}: {r}", a.id());
                assert!(!r.contains("not verified"), "{}: {r}", a.id());
            }
        }
        // Devin's own folders stay unwritten, and the reason says so.
        let w = no_hooks_reason("windsurf").unwrap();
        assert!(w.contains("Devin") && w.contains("doesn't write .devin"));
    }

    #[test]
    fn every_target_is_either_a_hook_format_or_says_why_not() {
        for a in crate::adapters::all() {
            let has = format_for(a.id()).is_some();
            let reason = no_hooks_reason(a.id()).is_some();
            assert!(
                has != reason,
                "{}: needs exactly one of a format or a reason",
                a.id()
            );
        }
    }
}
