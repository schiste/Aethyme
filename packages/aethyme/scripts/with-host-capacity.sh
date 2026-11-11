#!/usr/bin/env bash
# Run a heavy local command under a host-wide Aethyme capacity grant, so
# concurrent agents on one machine queue for CPU instead of stacking builds.
#
# Usage:
#
#     packages/aethyme/scripts/with-host-capacity.sh [--units N] [--wait DURATION] -- <command> ...
#         # e.g. ... -- cargo build --locked --manifest-path packages/aethyme/rust/Cargo.toml
#
# The command runs under `aethyme broker advanced resources run`, which holds
# N units of the `aethyme-host-build` pool (limit 6) for exactly the command's
# lifetime, renews the lease while it runs, and releases it when the command
# exits. A build of the workspace costs 3 units (the cargo-test gate's cost),
# so at most two run at once on the host; use --units 2 for a single crate.
# --wait (default 30m) bounds the queueing; on expiry the command never starts
# and the exit status is non-zero. The command's own exit status is returned.
#
# Every caller must declare the same limit for a pool, or the broker refuses
# the request with capacity_policy_mismatch, so the limit is a constant here.
#
# The command runs directly, without a grant, in CI (CI or GITHUB_ACTIONS
# set), inside a broker gate (AETHYME_GATE_WORKER_ID set), inside an existing
# grant (AETHYME_RESOURCE_LEASE_ID set), or when AETHYME_HOST_CAPACITY=off.
# Otherwise a missing `aethyme` binary is an error, not a silent bypass.
#
# See who holds or waits for capacity:
#     aethyme broker advanced resources list --json
#     aethyme broker advanced resources pressure --json
set -euo pipefail

pool=aethyme-host-build
limit=6
units=3
wait=30m
ttl_seconds=300

usage() {
    echo "usage: $0 [--units 1-$limit] [--wait DURATION] -- <command> ..." >&2
    exit 2
}

while [ $# -gt 0 ]; do
    case "$1" in
        --units) units="${2:-}"; shift 2 || usage ;;
        --wait) wait="${2:-}"; shift 2 || usage ;;
        --) shift; break ;;
        *) usage ;;
    esac
done
[ $# -gt 0 ] || usage
case "$units" in
    [1-9]) [ "$units" -le "$limit" ] || usage ;;
    *) usage ;;
esac
[ -n "$wait" ] || usage

if [ -n "${CI:-}" ] || [ -n "${GITHUB_ACTIONS:-}" ] \
    || [ -n "${AETHYME_GATE_WORKER_ID:-}" ] || [ -n "${AETHYME_RESOURCE_LEASE_ID:-}" ] \
    || [ "${AETHYME_HOST_CAPACITY:-}" = off ]; then
    exec "$@"
fi

if ! command -v aethyme >/dev/null 2>&1; then
    echo "error: aethyme is not on PATH, so host capacity cannot be reserved." >&2
    echo "Install it (see CLAUDE.md), or set AETHYME_HOST_CAPACITY=off to run unreserved." >&2
    exit 69
fi

# Opaque identities: the broker records them, never paths.
top="$(git rev-parse --show-toplevel 2>/dev/null || pwd -P)"
fingerprint="$(printf '%s' "$top" | git hash-object --stdin)"
label="$(printf '%s' "${1##*/}" | LC_ALL=C tr -c 'A-Za-z0-9._-' '-' | cut -c1-32)"
run_id="aethyme-build-${label}-$$-$(date +%s)-$(od -An -N4 -tx4 /dev/urandom | tr -d ' ')"

request="$(mktemp "${TMPDIR:-/tmp}/aethyme-capacity.XXXXXX")"
trap 'rm -f "$request"' EXIT
cat >"$request" <<JSON
{"schema_version":1,"request_id":"$run_id","repository":"aethyme","worktree_fingerprint":"$fingerprint","run_id":"$run_id","ttl_seconds":$ttl_seconds,"holder_pid":$$,"resources":[{"key":"host_build","kind":"capacity","pool":"$pool","units":$units,"limit":$limit}]}
JSON

# Not exec: the shell stays as holder_pid and removes the request on exit.
status=0
aethyme broker advanced resources run "$request" --wait "$wait" -- "$@" || status=$?
exit "$status"
