#!/usr/bin/env bash
# Aethyme hook shim.
#
# Installed globally by the Codex/Claude plugin, so it fires in EVERY repository
# on this machine, including repositories that have never heard of Aethyme. It
# must therefore be silent and successful by default, and only speak when it is
# both in an Aethyme-enhanced repository and talking to a CLI that understands
# the event.
#
# Contract with the agent surface:
#   - stdout is parsed as a hook envelope, so anything printed on a path that is
#     not a deliberate envelope corrupts the session. Print nothing otherwise.
#   - a nonzero exit is a hook failure. Always exit 0.
#
# The plugin ships files; the CLI ships logic. Keeping this shim a thin pipe
# over one stable entry point (`aethyme hook <event>`) means an installed plugin
# never has to match the installed CLI version.

set -u

event="${1:-}"
[[ -n "$event" ]] || exit 0

# Read the event JSON before any early exit: the caller writes it to our stdin
# and a closed pipe can surface as a broken-pipe error on their side.
payload="$(cat 2>/dev/null || true)"

command -v aethyme >/dev/null 2>&1 || exit 0
command -v git >/dev/null 2>&1 || exit 0

# Self-gate on the repository, not on the machine. `git rev-parse` resolves the
# real root when the agent is running from a subdirectory or a broker worktree.
root="$(git rev-parse --show-toplevel 2>/dev/null || true)"
[[ -n "$root" && -d "$root/.aethyme" ]] || exit 0

# One call. If this CLI has no `hook` subcommand (or the event is one it does
# not handle), it exits nonzero and we stay silent rather than leaking an
# error message into the envelope slot.
out="$(printf '%s' "$payload" | aethyme hook "$event" --repo "$root" 2>/dev/null)"
status=$?

if [[ $status -eq 0 && -n "$out" ]]; then
    printf '%s' "$out"
    exit 0
fi

# Fallback: record that the surface fired so the broker can still see liveness,
# and say nothing to the agent. Two very different situations land here -- a CLI
# that predates `aethyme hook` (exit 2, unknown subcommand) and a current one
# that deliberately had nothing to say (exit 0, empty) -- and from the outside
# both look like a plugin that does nothing. Carrying the status is what lets
# `aethyme plugin status` be corroborated after the fact instead of guessed at.
aethyme repo record-wrapper-invocation "$root" \
    --wrapper "aethyme-plugin-hook" \
    --detail "event=$event status=$status" \
    >/dev/null 2>&1 || true

exit 0
