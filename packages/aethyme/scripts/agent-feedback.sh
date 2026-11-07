#!/usr/bin/env bash
# Collect existing local broker signals and agent reports as one JSON document.
set -u

if ! command -v jq >/dev/null; then
  printf '%s\n' 'agent-feedback: jq is required' >&2
  exit 1
fi
work_dir=$(mktemp -d "${TMPDIR:-/tmp}/aethyme-feedback.XXXXXXXX") || exit 1
trap 'rm -rf "$work_dir"' EXIT
binary=${AETHYME_BIN:-aethyme}
failed=0

collect() {
  local name=$1
  shift
  if "$binary" "$@" >"$work_dir/$name.json" && jq -se 'length == 1 and (.[0] | type == "object")' "$work_dir/$name.json" >/dev/null; then
    printf 'true' >"$work_dir/$name.ok"
  else
    printf 'agent-feedback: %s unavailable or invalid\n' "$name" >&2
    printf 'null\n' >"$work_dir/$name.json"
    printf 'false' >"$work_dir/$name.ok"
    failed=1
  fi
}

collect insights broker advanced insights --session-limit 0 --pull-request-limit 0 --json
collect metrics broker advanced metrics --json
collect reports broker advanced report list --json
collect status broker status --compact --json

jq -n \
  --slurpfile insights "$work_dir/insights.json" --slurpfile insights_ok "$work_dir/insights.ok" \
  --slurpfile metrics "$work_dir/metrics.json" --slurpfile metrics_ok "$work_dir/metrics.ok" \
  --slurpfile reports "$work_dir/reports.json" --slurpfile reports_ok "$work_dir/reports.ok" \
  --slurpfile status "$work_dir/status.json" --slurpfile status_ok "$work_dir/status.ok" \
  '{schema_version: 1, sources: {
    insights: {available: $insights_ok[0], data: $insights[0]},
    metrics: {available: $metrics_ok[0], data: $metrics[0]},
    reports: {available: $reports_ok[0], data: $reports[0]},
    status: {available: $status_ok[0], data: $status[0]}
  }}' || exit 1
exit "$failed"
