#!/bin/sh
# Aethyme pilot metrics export: one redacted, timestamped snapshot per run.
#
# Reads `aethyme broker status --json` and the blocker list
# (`aethyme broker unblock --json` on v0.8.4 and later, `blockers --json` on
# older builds) for one repository, keeps an allowlist of counts, ids,
# statuses and timestamps, and writes it to <out>/<label>-<UTC time>.json.
#
# Nothing else is kept: no file paths, branch names, task text, commit SHAs,
# agent identities, advisory evidence or blocker causes. Raw broker output is
# never written to disk. The script only reads broker state; it changes
# nothing in the repository or the broker.
#
# Usage:
#   export-metrics.sh [--repo <path>] [--out <dir>] [--label <name>]
#
#   --repo   repository to snapshot (default: current directory)
#   --out    output directory (default: $AETHYME_PILOT_OUT or
#            ~/aethyme-pilot-metrics)
#   --label  short name for this repository in the file name, chosen by you
#            (default: repo). It is the only repository identifier kept.
#
# Requires: aethyme, jq (1.6 or later).

set -eu

repo=.
out=${AETHYME_PILOT_OUT:-"$HOME/aethyme-pilot-metrics"}
label=repo

usage() {
    sed -n '2,23p' "$0" | sed 's/^# \{0,1\}//'
}

while [ "$#" -gt 0 ]; do
    case "$1" in
        --repo) [ "$#" -ge 2 ] || { usage >&2; exit 2; }; repo=$2; shift 2 ;;
        --out) [ "$#" -ge 2 ] || { usage >&2; exit 2; }; out=$2; shift 2 ;;
        --label) [ "$#" -ge 2 ] || { usage >&2; exit 2; }; label=$2; shift 2 ;;
        -h|--help) usage; exit 0 ;;
        *) printf 'export-metrics: unknown argument: %s\n' "$1" >&2; usage >&2; exit 2 ;;
    esac
done

case "$label" in
    *[!A-Za-z0-9_-]*|'') printf 'export-metrics: --label may use only letters, digits, - and _\n' >&2; exit 2 ;;
esac

for tool in aethyme jq; do
    command -v "$tool" >/dev/null 2>&1 || {
        printf 'export-metrics: %s not found on PATH\n' "$tool" >&2
        exit 1
    }
done

cd "$repo"
mkdir -p "$out"

captured_at=$(date -u +%Y-%m-%dT%H:%M:%SZ)
stamp=$(date -u +%Y%m%dT%H%M%SZ)
# First two words only: "aethyme 0.8.4". The build suffix is not needed.
version=$(aethyme --version | awk '{print $2}')

# Status is required; a failure here is reported, not papered over.
status_json=$(aethyme broker status --json) || {
    printf 'export-metrics: aethyme broker status --json failed in %s\n' "$repo" >&2
    exit 1
}

# Blockers are optional: try the v0.8.4 spelling, then the older one. If
# neither yields a blocker list, record that it was unavailable rather than
# writing an empty list, which would read as "nothing blocked".
blockers_json=null
if b=$(aethyme broker unblock --json 2>/dev/null) &&
    printf '%s' "$b" | jq -e 'has("blockers")' >/dev/null 2>&1; then
    blockers_json=$b
elif b=$(aethyme broker blockers --json 2>/dev/null) &&
    printf '%s' "$b" | jq -e 'has("blockers")' >/dev/null 2>&1; then
    blockers_json=$b
fi

tmp=$(mktemp "$out/.snapshot.XXXXXX")
trap 'rm -f "$tmp"' EXIT

printf '%s' "$status_json" | jq \
    --arg captured_at "$captured_at" \
    --arg version "$version" \
    --arg label "$label" \
    --argjson blockers "$blockers_json" '
def count_by(f): map(f) | group_by(.) | map({key: (. [0] | tostring), value: length}) | from_entries;
{
  schema: "aethyme-pilot-metrics.v1",
  captured_at: $captured_at,
  aethyme_version: $version,
  label: $label,
  summary: ((.summary // {}) | {
    live_sessions, active_sessions, idle_sessions, stale_sessions,
    dirty_sessions, overlap_count, promoted_conflict_count,
    integration_relation, integration_ahead_main_commits
  }),
  sessions: [(.agents // [])[] | {
    id, status, derived_status, origin, cleanup_state,
    created_at, last_activity_at
  }],
  leases: {
    count: ((.leases // []) | length),
    by_kind: ((.leases // []) | count_by(.kind)),
    sessions: ((.leases // []) | map(.session_id) | unique)
  },
  queue: {
    count: ((.queue // []) | length),
    by_status: ((.queue // []) | count_by(.status)),
    entries: [(.queue // [])[] | {id, session_id, status, created_at, updated_at}]
  },
  queue_history: ((.queue_history.terminal_counts // []) | map({key: .status, value: .count}) | from_entries),
  promoted_conflicts: {
    count: ((.promoted_conflicts // []) | length),
    sessions: ((.promoted_conflicts // []) | map(.session_id) | unique)
  },
  advice: [(.advice // [])[] | {id, severity, session_id}],
  advisories: {
    count: ((.outstanding_advisories // []) | length),
    by_producer: ((.outstanding_advisories // []) | count_by(.producer)),
    by_severity: ((.outstanding_advisories // []) | count_by(.severity))
  },
  storage: ((.cleanup_retention // {}) | {
    broker_owned_worktree_count, eligible_worktree_count,
    estimated_retained_bytes, estimated_reclaimable_bytes, severity
  }),
  blockers: (if $blockers == null then {available: false} else {
    available: true,
    count: ($blockers.blockers | length),
    by_kind: ($blockers.blockers | count_by(.kind)),
    by_scope: ($blockers.blockers | count_by(.scope)),
    safe_to_clear_automatically: ($blockers.blockers | map(select(.safe_to_clear_automatically)) | length),
    sessions: ($blockers.blockers | map(.session_id // empty) | unique),
    unavailable_sources: (($blockers.unavailable // []) | map(.source))
  } end)
}' >"$tmp"

# Defence in depth: refuse to keep a snapshot in which any string looks like
# a path. Every allowlisted field is an id, a status, a count or a timestamp.
# The offending fields are named by position, never by value.
suspect=$(jq -r '[paths(strings) as $p | select(getpath($p) | test("/")) | $p | map(tostring) | join(".")] | join(", ")' "$tmp")
if [ -n "$suspect" ]; then
    printf 'export-metrics: field(s) looked like a path, snapshot discarded: %s\n' "$suspect" >&2
    exit 1
fi

dest="$out/$label-$stamp.json"
mv "$tmp" "$dest"
trap - EXIT
printf '%s\n' "$dest"
