//! Estimate how much context an MCP server's tool definitions take up.
//!
//! Used by `kitbag doctor --mcp-tokens`. A local (stdio) server is started only
//! long enough to answer `initialize` and `tools/list`, with a deadline, and is
//! then killed. Its stderr is discarded and its stdout is only parsed for those
//! answers: nothing a server prints, and none of its tools' text, is shown,
//! logged or stored. Remote servers are never contacted here.
//!
//! The estimate is about four characters per token over each tool's name,
//! description and input schema as compact JSON. It is not any tokenizer's
//! exact count; real numbers differ by model and by how a tool wraps them.

use std::collections::BTreeMap;
use std::io::{BufRead, BufReader, Read, Write};
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use serde_json::{Value, json};

/// Characters per token in the estimate.
pub const CHARS_PER_TOKEN: usize = 4;

/// Most bytes read from a server's stdout before giving up on it.
const MAX_OUTPUT: usize = 32 << 20;

/// Most `tools/list` pages followed.
const MAX_PAGES: usize = 100;

/// MCP protocol version kitbag asks for; servers answer with the one they speak.
const PROTOCOL_VERSION: &str = "2025-06-18";

/// What a server's tool list costs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Cost {
    pub tools: usize,
    pub tokens: usize,
}

/// The part of one tool definition a client hands the model, as compact JSON.
fn definition_text(tool: &Value) -> String {
    let mut def = serde_json::Map::new();
    for key in ["name", "description", "inputSchema"] {
        if let Some(v) = tool.get(key) {
            def.insert(key.to_string(), v.clone());
        }
    }
    Value::Object(def).to_string()
}

/// Estimated tokens for `tools` (entries of a `tools/list` result): the
/// characters of each tool's name, description and input schema as compact
/// JSON, divided by [`CHARS_PER_TOKEN`] and rounded up.
pub fn estimate(tools: &[Value]) -> Cost {
    let chars: usize = tools
        .iter()
        .map(|t| definition_text(t).chars().count())
        .sum();
    Cost {
        tools: tools.len(),
        tokens: chars.div_ceil(CHARS_PER_TOKEN),
    }
}

/// A local server to start, with every reference already resolved.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct Launch {
    pub command: String,
    pub args: Vec<String>,
    pub env: BTreeMap<String, String>,
    pub cwd: Option<String>,
}

/// Kills the server however [`list_tools`] returns.
struct Running(Child);

impl Drop for Running {
    fn drop(&mut self) {
        drop(self.0.stdin.take());
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

/// Start `launch`, ask it for its tools and stop it, within `timeout` in all.
/// The error says what went wrong in kitbag's words; it never carries text
/// from the server.
pub fn list_tools(launch: &Launch, timeout: Duration) -> Result<Vec<Value>, String> {
    let deadline = Instant::now() + timeout;
    let mut cmd = Command::new(&launch.command);
    cmd.args(&launch.args)
        .envs(&launch.env)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    if let Some(cwd) = &launch.cwd {
        cmd.current_dir(cwd);
    }
    let mut child = Running(
        cmd.spawn()
            .map_err(|e| format!("could not start it: {e}"))?,
    );
    let stdout = child.0.stdout.take().ok_or("no stdout")?;
    let mut stdin = child.0.stdin.take().ok_or("no stdin")?;
    let lines = read_lines(stdout);
    let mut session = Session {
        lines,
        deadline,
        timeout,
        child: &mut child,
    };

    let init = json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "initialize",
        "params": {
            "protocolVersion": PROTOCOL_VERSION,
            "capabilities": {},
            "clientInfo": {"name": "kitbag", "version": env!("CARGO_PKG_VERSION")},
        },
    });
    send(&mut stdin, &init)?;
    let result = session.answer(1, "initialize")?;
    if result.pointer("/capabilities/tools").is_none() {
        // A server without tools has nothing for the model.
        return Ok(Vec::new());
    }
    send(
        &mut stdin,
        &json!({"jsonrpc": "2.0", "method": "notifications/initialized"}),
    )?;

    let mut tools = Vec::new();
    let mut cursor: Option<String> = None;
    for page in 0..MAX_PAGES {
        let id = 2 + page as u64;
        let params = match &cursor {
            Some(c) => json!({"cursor": c}),
            None => json!({}),
        };
        send(
            &mut stdin,
            &json!({"jsonrpc": "2.0", "id": id, "method": "tools/list", "params": params}),
        )?;
        let mut result = session.answer(id, "tools/list")?;
        match result.get_mut("tools").map(Value::take) {
            Some(Value::Array(list)) => tools.extend(list),
            _ => return Err("its tools/list answer has no tool list".into()),
        }
        cursor = result
            .get("nextCursor")
            .and_then(Value::as_str)
            .filter(|c| !c.is_empty())
            .map(str::to_string);
        if cursor.is_none() {
            return Ok(tools);
        }
    }
    Err(format!("it returned more than {MAX_PAGES} pages of tools"))
}

fn send(stdin: &mut impl Write, msg: &Value) -> Result<(), String> {
    let mut line = msg.to_string();
    line.push('\n');
    stdin
        .write_all(line.as_bytes())
        .and_then(|()| stdin.flush())
        .map_err(|_| "it exited before listing its tools".to_string())
}

/// Lines from the server's stdout, read on another thread so the deadline
/// holds even when the server goes quiet. Ends at end of output, a read
/// error or [`MAX_OUTPUT`] bytes.
fn read_lines(stdout: impl Read + Send + 'static) -> mpsc::Receiver<Vec<u8>> {
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let mut reader = BufReader::new(stdout.take(MAX_OUTPUT as u64));
        loop {
            let mut line = Vec::new();
            match reader.read_until(b'\n', &mut line) {
                Ok(0) | Err(_) => break,
                Ok(_) => {
                    if tx.send(line).is_err() {
                        break;
                    }
                }
            }
        }
    });
    rx
}

struct Session<'a> {
    lines: mpsc::Receiver<Vec<u8>>,
    deadline: Instant,
    timeout: Duration,
    child: &'a mut Running,
}

impl Session<'_> {
    /// The result of request `id`. Lines that aren't JSON-RPC (some servers
    /// log to stdout), notifications and the server's own requests are skipped.
    fn answer(&mut self, id: u64, method: &str) -> Result<Value, String> {
        loop {
            let left = self.deadline.saturating_duration_since(Instant::now());
            let line = match self.lines.recv_timeout(left) {
                Ok(line) => line,
                Err(mpsc::RecvTimeoutError::Timeout) => {
                    return Err(format!("timed out after {}s", self.timeout.as_secs_f64()));
                }
                Err(mpsc::RecvTimeoutError::Disconnected) => {
                    // Give it a moment to be reaped so the status is known.
                    let status = (0..20).find_map(|_| match self.child.0.try_wait() {
                        Ok(Some(s)) => Some(Some(s)),
                        Ok(None) => {
                            std::thread::sleep(Duration::from_millis(10));
                            None
                        }
                        Err(_) => Some(None),
                    });
                    return Err(match status.flatten() {
                        Some(s) => format!("it exited before listing its tools ({s})"),
                        None => "it closed its output before listing its tools".into(),
                    });
                }
            };
            let Ok(msg) = serde_json::from_slice::<Value>(&line) else {
                continue;
            };
            if msg.get("id").and_then(Value::as_u64) != Some(id) || msg.get("method").is_some() {
                continue;
            }
            if let Some(err) = msg.get("error") {
                let code = err.get("code").and_then(Value::as_i64);
                return Err(match code {
                    Some(c) => format!("it answered {method} with an error (code {c})"),
                    None => format!("it answered {method} with an error"),
                });
            }
            return match msg.get("result") {
                Some(r) if r.is_object() => Ok(r.clone()),
                _ => Err(format!("its {method} answer has no result")),
            };
        }
    }
}

/// `1234567` as `1,234,567`.
pub fn thousands(n: usize) -> String {
    let digits = n.to_string();
    let mut out = String::new();
    for (i, ch) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(ch);
    }
    out
}

/// Is `path` usable as a working directory?
pub fn cwd_ok(path: &str) -> bool {
    Path::new(path).is_dir()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn estimate_counts_name_description_and_schema_only() {
        let tool = json!({
            "name": "echo",
            "description": "Echo the input",
            "inputSchema": {"type": "object"},
            "annotations": {"title": "a long title that a model never sees in the tool list"},
        });
        let text =
            r#"{"name":"echo","description":"Echo the input","inputSchema":{"type":"object"}}"#;
        assert_eq!(definition_text(&tool), text);
        let cost = estimate(&[tool.clone(), tool]);
        assert_eq!(cost.tools, 2);
        assert_eq!(cost.tokens, (2 * text.len()).div_ceil(4));
    }

    #[test]
    fn estimate_rounds_up_and_counts_characters_not_bytes() {
        // {"name":"é"} is 12 characters and 13 bytes.
        let cost = estimate(&[json!({"name": "é"})]);
        assert_eq!(cost.tokens, 3);
        assert_eq!(
            estimate(&[]),
            Cost {
                tools: 0,
                tokens: 0
            }
        );
    }

    #[test]
    fn thousands_groups_digits() {
        assert_eq!(thousands(0), "0");
        assert_eq!(thousands(999), "999");
        assert_eq!(thousands(1000), "1,000");
        assert_eq!(thousands(1234567), "1,234,567");
    }

    #[test]
    fn a_missing_command_is_reported() {
        let launch = Launch {
            command: "kitbag-no-such-mcp-server-for-tests".into(),
            args: vec![],
            env: BTreeMap::new(),
            cwd: None,
        };
        let err = list_tools(&launch, Duration::from_secs(5)).unwrap_err();
        assert!(err.starts_with("could not start it"), "{err}");
    }
}
