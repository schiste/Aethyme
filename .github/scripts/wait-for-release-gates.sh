#!/usr/bin/env bash
# Wait for an Aethyme Gates run on $SHA that passed the whole workspace suite.
# A push to main runs only the changed crates unless the commit changes the
# workspace version; such a run passes but records no `full-suite` artifact,
# and it does not qualify a release.
#
# A gates run whose suite was covered by a dispatched OSS CI run on the same
# tree also uploads `tested-by-run-<id>`. That run is verified again here
# (verify-tested-tree-run.sh) against $SHA before the record qualifies: the
# release does not take the gates run's word for it.
set -euo pipefail
verify="$(dirname "$0")/verify-tested-tree-run.sh"
: "${GITHUB_REPOSITORY:?}" "${SHA:?}"
wait_seconds="${AETHYME_GATE_WAIT_SECONDS:-1800}"
interval="${AETHYME_GATE_POLL_SECONDS:-15}"
[[ "$wait_seconds" =~ ^[0-9]+$ && "$interval" =~ ^[1-9][0-9]*$ ]] || { echo 'invalid gate polling duration' >&2; exit 2; }
deadline=$((SECONDS + wait_seconds))
while :; do
  # API errors are transient; they never count as a passing verdict.
  if runs="$(gh api "repos/$GITHUB_REPOSITORY/actions/workflows/aethyme-gates.yml/runs?head_sha=$SHA&per_page=100")"; then
    lookup_failed=0
    refused=0
    for id in $(jq -r '.workflow_runs[] | select(.status == "completed" and .conclusion == "success") | .id' <<< "$runs"); do
      if ! artifacts="$(gh api "repos/$GITHUB_REPOSITORY/actions/runs/$id/artifacts?per_page=100")"; then
        lookup_failed=1
        continue
      fi
      if jq -e '[.artifacts[] | select(.expired == false and .name == "full-suite")] | length > 0' <<< "$artifacts" >/dev/null; then
        # A re-run attempt can upload the same name again, so names are
        # deduplicated and only unexpired ones are cited. An expired one alone
        # still means the record relied on another run, which can no longer
        # be named: that gates run does not qualify.
        tested_by="$(jq -r '[.artifacts[] | select(.expired == false and (.name | startswith("tested-by-run-"))) | .name | ltrimstr("tested-by-run-")] | unique | join(" ")' <<< "$artifacts")"
        if [ -z "$tested_by" ]; then
          if jq -e 'any(.artifacts[]; .name | startswith("tested-by-run-"))' <<< "$artifacts" >/dev/null; then
            echo "Aethyme Gates run $id relied on a suite run whose record expired; trying the next gates run" >&2
            refused=1
            continue
          fi
          echo "Aethyme Gates passed the full suite on $SHA (run $id)"
          exit 0
        fi
        if [[ ! "$tested_by" =~ ^[0-9]+$ ]]; then
          echo "Aethyme Gates run $id names more than one tested-by run: $tested_by; trying the next gates run" >&2
          refused=1
          continue
        fi
        if bash "$verify" "$tested_by" "$SHA"; then
          echo "Aethyme Gates passed the full suite on $SHA (run $id, suite in run $tested_by)"
          exit 0
        else
          status=$?
          # A refusal disqualifies this gates run only; an API error is
          # retried like any other.
          if [ "$status" -eq 1 ]; then
            refused=1
          else
            lookup_failed=1
          fi
        fi
      fi
    done
    if [ "$lookup_failed" -eq 0 ] && jq -e '(.workflow_runs | length > 0) and ([.workflow_runs[] | select(.status != "completed")] | length == 0)' <<< "$runs" >/dev/null; then
      if [ "$refused" -eq 1 ]; then
        echo "Aethyme Gates recorded the full suite on $SHA only through suite runs that failed verification; dispatch Aethyme Gates while $SHA is main's head" >&2
      elif jq -e '.workflow_runs | any(.conclusion == "success")' <<< "$runs" >/dev/null; then
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
