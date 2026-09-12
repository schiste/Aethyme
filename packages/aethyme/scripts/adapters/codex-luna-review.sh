#!/usr/bin/env bash
# Perform this repository's routed reviews in Codex Luna shells.
#
# `aethyme broker review tick` decides which reviews a pull request is owed and
# which workspace each gets; chau7-review-adapter.py performs the Chau7 half.
# Neither of them names a model, and that is deliberate -- the broker builds
# with no Chau7 present and the adapter works for any agent that takes a prompt
# as an argument. This script is the one place that says "Codex Luna", so
# changing reviewer is an edit here and nothing else.
#
# Run it from a checkout of this repository:
#
#     packages/aethyme/scripts/adapters/codex-luna-review.sh --session <id>
#
# Anything after `--` is forwarded to the adapter, so `--dry-run` and
# `--limit` work as documented there.
set -uo pipefail

REPO_SLUG="${AETHYME_REVIEW_REPO:-schiste/Aethyme}"
BROKER="${AETHYME_BROKER_BIN:-aethyme}"
ADAPTER="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)/chau7-review-adapter.py"

SESSION=""
while [ $# -gt 0 ]; do
    case "$1" in
        --session) SESSION="${2:?--session needs a broker session id}"; shift 2 ;;
        --repo)    REPO_SLUG="${2:?--repo needs owner/name}"; shift 2 ;;
        --)        shift; break ;;
        *)         break ;;
    esac
done
[ -n "$SESSION" ] || { echo "usage: ${0##*/} --session <broker session id> [-- <adapter args>]" >&2; exit 2; }

# The reviewer inherits this shell's PATH, and on a machine carrying a `git`
# wrapper it would read a decorated diff and review bytes nobody wrote. The
# broker resolves its own honest git (#176, #178); a reviewer running `git`
# by hand has no such protection, so strip the wrappers before handing the
# environment on. Harmless where they are absent.
CLEAN_PATH="$(printf '%s' "$PATH" | tr ':' '\n' \
    | grep -v 'chau7/cto_bin' | grep -v 'smartoverlay' | paste -sd: -)"

# Those wrappers sometimes shadow `codex` itself, so resolving it is a check
# and not a formality. Failing here costs one message; failing in the tab
# costs a ledger row closed `running` against a shell that printed
# "command not found" and exited, and the slot is held until the staleness
# window reclaims it.
if ! PATH="$CLEAN_PATH" command -v codex >/dev/null 2>&1; then
    echo "codex is not on the PATH once the git wrappers are stripped;" >&2
    echo "install it outside them or the reviewer tabs will open empty" >&2
    exit 1
fi

# `--approve-for-me` because nobody is sitting at the tab: an unattended
# reviewer blocked on an approval prompt holds its concurrency slot until
# `stale_after_minutes` reclaims it, which reads as "the review never ran".
#
# It also *is* the sandbox choice -- it routes approvals through automatic
# review and selects workspace-write itself -- so naming `--sandbox` beside it
# is not redundant but an error codex refuses to start on. The policy it picks
# is the one wanted anyway: the reviewer must reach `gh` and the broker, and
# its checkout is a detached throwaway, but the rest of the filesystem is not
# part of reviewing a pull request. Only the network override is ours to set.
AGENT="env PATH=$(printf '%q' "$CLEAN_PATH") codex \
--model gpt-5.6-luna \
-c sandbox_workspace_write.network_access=true \
--approve-for-me"

exec python3 "$ADAPTER" \
    --session "$SESSION" \
    --repo "$REPO_SLUG" \
    --repo-path "$(cd "$(dirname "${BASH_SOURCE[0]}")/../../../.." && pwd)" \
    --broker "$BROKER" \
    --agent "$AGENT" \
    "$@"
