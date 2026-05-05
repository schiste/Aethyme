#!/usr/bin/env bash
# Aethyme SessionStart hook: load AGENTS.md / CLAUDE.md content into
# `additionalContext` so headless Claude Code sessions (e.g. evaluation
# runs launched via `claude --dangerously-skip-permissions`) discover the
# in-repo agent instructions even when the harness skips the standard
# CWD-CLAUDE.md auto-load.
#
# This script is deployed by `aethyme enhance deploy --repo <path>` and
# wired into the project's .claude/settings.local.json under
# hooks.SessionStart.
#
# Output protocol: the script must emit a single JSON object on stdout
# of the shape
#   {"hookSpecificOutput": {"hookEventName": "SessionStart",
#                            "additionalContext": "<text>"}}
# Empty stdout (or non-zero exit) = no context injected.

set -uo pipefail

cwd="${CLAUDE_PROJECT_DIR:-${PWD}}"

agents="$cwd/AGENTS.md"
claude_md="$cwd/CLAUDE.md"

context=""

if [[ -f "$agents" ]]; then
    context="$(cat -- "$agents")"
fi

# Append CLAUDE.md only if it exists AND differs from AGENTS.md
# (typical setup deploys both with identical content).
if [[ -f "$claude_md" ]]; then
    if [[ -z "$context" ]] || ! diff -q "$agents" "$claude_md" >/dev/null 2>&1; then
        if [[ -n "$context" ]]; then
            context+=$'\n\n---\n\n'
        fi
        context+="$(cat -- "$claude_md")"
    fi
fi

if [[ -z "$context" ]]; then
    exit 0
fi

# Emit the JSON envelope. Use python for safe JSON escaping rather than
# hand-rolling string substitution.
python3 - "$context" <<'PY'
import json, sys
ctx = sys.argv[1]
print(json.dumps({
    "hookSpecificOutput": {
        "hookEventName": "SessionStart",
        "additionalContext": ctx,
    }
}))
PY
