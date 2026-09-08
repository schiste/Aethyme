#!/bin/bash
# One scheduled pass of PR monitoring for a single repository.
#
# Two steps, in order, because the second consumes what the first produces:
#   1. `watch pr tick` -- one bounded foreground poll of due watches.
#   2. the Chau7 adapter -- deliver any resulting activity to the owning session.
#
# The broker starts no background poller by design, so something has to invoke
# this; on macOS that is the launchd agent beside this script. Failure here is
# not an error worth retrying hard: nothing is lost, the watches keep their
# cursors, and the next tick catches up. So this never exits nonzero for an
# absent Chau7 or a quiet repository -- a launchd agent that "fails" every
# quiet minute is noise that trains you to ignore it.
set -uo pipefail

REPO="${AETHYME_PR_MONITOR_REPO:?set AETHYME_PR_MONITOR_REPO to the repository path}"
BROKER="${AETHYME_BROKER_BIN:-aethyme}"
WORKER="${AETHYME_PR_MONITOR_WORKER:-launchd-$(hostname -s)}"
LIMIT="${AETHYME_PR_MONITOR_LIMIT:-20}"
ADAPTER="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)/chau7-delivery-adapter.py"

cd "$REPO" || exit 0
printf '[%s] tick\n' "$(date -u +%Y-%m-%dT%H:%M:%SZ)"

# A tick that fails must not stop delivery: previously-polled batches may still
# be sitting in the outbox undelivered.
"$BROKER" broker watch pr tick --limit "$LIMIT" 2>&1 || \
    printf '  tick failed; continuing to delivery\n'

if [ -x "$ADAPTER" ] || [ -f "$ADAPTER" ]; then
    python3 "$ADAPTER" --worker "$WORKER" --repo "$REPO" --broker "$BROKER" 2>&1 || \
        printf '  adapter failed\n'
fi
exit 0
