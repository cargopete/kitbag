//! kitbag: a portable, versioned agent profile for AI coding tools.
//!
//! Capture MCP servers, instructions, rules, commands, skills and settings from
//! one tool (Claude Code, Cursor, Codex CLI, Gemini CLI, Claude Desktop), store
//! them as a secret-free, git-friendly profile directory, and apply that profile
//! to any other tool, with diffs, merges, backups and undo.
//!
//! The command-line interface lives in [`cli`]; the pieces are usable as a library:
//! [`profile_io::load`] a profile, [`engine::plan`] an apply against some
//! [`adapters`], and [`engine::commit`] it as an undoable transaction.

pub mod adapters;
pub mod cli;
pub mod edit;
pub mod engine;
pub mod frontmatter;
pub mod hooks;
pub mod mcp_tokens;
pub mod model;
pub mod plan_fs;
pub mod policy;
pub mod portable;
pub mod profile_io;
pub mod secrets;
pub mod source;
pub mod sync;
pub mod txn;
pub mod ui;
pub mod why;
