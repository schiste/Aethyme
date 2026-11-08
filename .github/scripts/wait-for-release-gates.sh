#!/usr/bin/env bash
set -euo pipefail
: "${GITHUB_REPOSITORY:?}" "${SHA:?}"
wait_seconds="${AETHYME_GATE_WAIT_SECONDS:-1800}"
interval="${AETHYME_GATE_POLL_SECONDS:-15}"
[[ "$wait_seconds" =~ ^[0-9]+$ && "$interval" =~ ^[1-9][0-9]*$ ]] || { echo 'invalid gate polling duration' >&2; exit 2; }
deadline=$((SECONDS + wait_seconds))
while :; do
  # API errors are transient; they never count as a passing verdict.
  if runs="$(gh api "repos/$GITHUB_REPOSITORY/actions/workflows/aethyme-gates.yml/runs?head_sha=$SHA&per_page=100")"; then
    if jq -e '.workflow_runs | any(.status == "completed" and .conclusion == "success")' <<< "$runs" >/dev/null; then
      echo "Aethyme Gates passed on $SHA"
      exit 0
    fi
    if jq -e '(.workflow_runs | length > 0) and ([.workflow_runs[] | select(.status != "completed")] | length == 0)' <<< "$runs" >/dev/null; then
      echo "Aethyme Gates completed without success on $SHA" >&2
      exit 1
    fi
  else
    echo 'Warning: could not read gate runs; retrying' >&2
  fi
  if (( SECONDS >= deadline )); then
    echo "Timed out waiting for Aethyme Gates on $SHA" >&2
    exit 1
  fi
  sleep "$interval"
done
