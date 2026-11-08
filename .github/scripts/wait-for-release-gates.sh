#!/usr/bin/env bash
# Wait for an Aethyme Gates run on $SHA that passed the whole workspace suite.
# A push to main runs only the changed crates unless the commit changes the
# workspace version; such a run passes but records no `full-suite` artifact,
# and it does not qualify a release.
set -euo pipefail
: "${GITHUB_REPOSITORY:?}" "${SHA:?}"
wait_seconds="${AETHYME_GATE_WAIT_SECONDS:-1800}"
interval="${AETHYME_GATE_POLL_SECONDS:-15}"
[[ "$wait_seconds" =~ ^[0-9]+$ && "$interval" =~ ^[1-9][0-9]*$ ]] || { echo 'invalid gate polling duration' >&2; exit 2; }
deadline=$((SECONDS + wait_seconds))
while :; do
  # API errors are transient; they never count as a passing verdict.
  if runs="$(gh api "repos/$GITHUB_REPOSITORY/actions/workflows/aethyme-gates.yml/runs?head_sha=$SHA&per_page=100")"; then
    lookup_failed=0
    for id in $(jq -r '.workflow_runs[] | select(.status == "completed" and .conclusion == "success") | .id' <<< "$runs"); do
      if ! artifacts="$(gh api "repos/$GITHUB_REPOSITORY/actions/runs/$id/artifacts?name=full-suite")"; then
        lookup_failed=1
        continue
      fi
      if jq -e '[.artifacts[] | select(.expired == false)] | length > 0' <<< "$artifacts" >/dev/null; then
        echo "Aethyme Gates passed the full suite on $SHA (run $id)"
        exit 0
      fi
    done
    if [ "$lookup_failed" -eq 0 ] && jq -e '(.workflow_runs | length > 0) and ([.workflow_runs[] | select(.status != "completed")] | length == 0)' <<< "$runs" >/dev/null; then
      if jq -e '.workflow_runs | any(.conclusion == "success")' <<< "$runs" >/dev/null; then
        echo "Aethyme Gates passed on $SHA without the full suite; tag a commit that changes the workspace version, or dispatch Aethyme Gates while $SHA is main's head" >&2
      else
        echo "Aethyme Gates completed without success on $SHA" >&2
      fi
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
