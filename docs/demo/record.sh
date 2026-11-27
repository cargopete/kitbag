#!/bin/sh
# Record docs/demo.gif from docs/demo/demo.tape.
#
# Needs: a built `kitbag` on PATH (or CARGO_TARGET_DIR/target/release/kitbag
# after `cargo build --locked --release` in the repo root), vhs
# (https://github.com/charmbracelet/vhs), ttyd, ffmpeg, and optionally
# gifsicle. The tape uses DejaVu Sans Mono.
#
# The session runs in a sandbox home (default /home/demo). It writes a small
# fake Claude Code setup there, including an obviously fake GitHub token that
# kitbag redacts on capture. Nothing is read from your real home.
#
# Usage, from the repository root:
#   docs/demo/record.sh

set -eu

ROOT=$(CDPATH= cd -- "$(dirname "$0")/../.." && pwd)
DEMO_HOME=${DEMO_HOME:-/home/demo}
TAPE="$ROOT/docs/demo/demo.tape"
GIF="$ROOT/docs/demo.gif"

if ! command -v kitbag >/dev/null 2>&1; then
  if [ -x "$ROOT/target/release/kitbag" ]; then
    PATH="$ROOT/target/release:$PATH"
    export PATH
  else
    echo "record.sh: kitbag not on PATH; build it with: cargo build --locked --release" >&2
    exit 1
  fi
fi
command -v vhs >/dev/null 2>&1 || { echo "record.sh: vhs is not installed" >&2; exit 1; }
command -v ttyd >/dev/null 2>&1 || { echo "record.sh: ttyd is not installed (required by vhs)" >&2; exit 1; }
command -v ffmpeg >/dev/null 2>&1 || { echo "record.sh: ffmpeg is not installed (required by vhs)" >&2; exit 1; }

# The sandbox directory must be writable. /home/demo is short enough that
# kitbag's review line still fits the GIF width.
if [ ! -d "$DEMO_HOME" ]; then
  mkdir -p "$DEMO_HOME" 2>/dev/null || {
    echo "record.sh: create a writable $DEMO_HOME (or set DEMO_HOME) and re-run" >&2
    exit 1
  }
fi

# Fresh sandbox: only the fake Claude Code setup, no leftover profile or state.
rm -rf "$DEMO_HOME/.claude" "$DEMO_HOME/.claude.json" "$DEMO_HOME/dotagents" \
  "$DEMO_HOME/.cursor" "$DEMO_HOME/.local" "$DEMO_HOME/bin"
mkdir -p "$DEMO_HOME/.claude/commands" "$DEMO_HOME/.claude/skills/release-notes" \
  "$DEMO_HOME/bin"

cat > "$DEMO_HOME/.claude.json" <<'JSON'
{
  "mcpServers": {
    "github": {
      "type": "stdio",
      "command": "npx",
      "args": ["-y", "@modelcontextprotocol/server-github"],
      "env": {
        "GITHUB_PERSONAL_ACCESS_TOKEN": "ghp_FAKE0123456789abcdefghijklmnopqrstuv"
      }
    },
    "linear": {
      "type": "http",
      "url": "https://mcp.linear.app/mcp",
      "headers": {
        "Authorization": "Bearer ${LINEAR_API_KEY}"
      }
    }
  }
}
JSON

cat > "$DEMO_HOME/.claude/commands/review.md" <<'MD'
Review this diff for bugs, missing tests and secrets. Be concise.
MD

cat > "$DEMO_HOME/.claude/skills/release-notes/SKILL.md" <<'MD'
---
name: release-notes
description: Draft a changelog from recent commits
---
Summarise commits since the last tag as Keep a Changelog bullets.
MD

KITBAG_BIN=$(command -v kitbag)
KITBAG_DIR=$(CDPATH= cd -- "$(dirname "$KITBAG_BIN")" && pwd)

cat > "$DEMO_HOME/bin/setup.sh" <<SH
export HOME=$DEMO_HOME
export KITBAG_HOME=$DEMO_HOME
export PATH="$KITBAG_DIR:/usr/bin:/bin"
export PS1='\$ '
export TERM=xterm-256color
unset PROMPT_COMMAND
export HISTFILE=/dev/null
cd $DEMO_HOME || exit 1
hash -r
SH

# The tape sources /home/demo/bin/setup.sh. If DEMO_HOME differs, rewrite a
# throwaway tape that sources the setup we just wrote.
WORK=$(mktemp -d)
trap 'rm -rf "$WORK"' EXIT
if [ "$DEMO_HOME" = /home/demo ]; then
  TAPE_RUN=$TAPE
else
  # tape paths are fixed at /home/demo; refuse a mismatch rather than guess
  echo "record.sh: this tape sources /home/demo/bin/setup.sh; use DEMO_HOME=/home/demo" >&2
  exit 1
fi

cp "$TAPE" "$WORK/demo.tape"
(
  cd "$WORK"
  # vhs Require kitbag looks at PATH
  PATH="$KITBAG_DIR:$PATH"
  export PATH
  vhs demo.tape
)
if command -v gifsicle >/dev/null 2>&1; then
  gifsicle -O3 --colors 256 "$WORK/demo.gif" -o "$GIF"
else
  cp "$WORK/demo.gif" "$GIF"
fi
ls -lh "$GIF"
