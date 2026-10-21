#!/usr/bin/env bash
set -euo pipefail

repo_root="$(git rev-parse --show-toplevel)"
workflow="${1:-$repo_root/.github/workflows/oss-ci.yml}"
if [[ ! -f "$workflow" ]]; then
  printf 'FAIL: workflow file not found: %s\n' "$workflow" >&2
  exit 1
fi

awk '
  function indent(line, prefix) {
    prefix = line
    sub(/[^ ].*$/, "", prefix)
    return length(prefix)
  }

  function finish_checkout() {
    if (active && !credentials_disabled) {
      printf "FAIL: checkout step at line %d must set with.persist-credentials: false\n", checkout_line > "/dev/stderr"
      failures = 1
    }
  }

  {
    line_indent = indent($0)
    if (active && NF > 0 && line_indent <= step_indent) {
      finish_checkout()
      active = 0
      with_seen = 0
      credentials_disabled = 0
    }

    if ($0 ~ /^[[:space:]]*-[[:space:]]*uses:[[:space:]]*actions\/checkout@/) {
      step_indent = line_indent
      checkout_line = NR
      checkout_count++
      active = 1
      with_seen = 0
      credentials_disabled = 0
      next
    }
    if ($0 ~ /^[[:space:]]*uses:[[:space:]]*actions\/checkout@/) {
      step_indent = line_indent - 2
      checkout_line = NR
      checkout_count++
      active = 1
      with_seen = 0
      credentials_disabled = 0
      next
    }

    if (!active) {
      next
    }
    if (line_indent == step_indent + 2 && $0 ~ /^[[:space:]]*with:[[:space:]]*$/) {
      with_seen = 1
      next
    }
    if (with_seen && line_indent == step_indent + 4 &&
        $0 ~ /^[[:space:]]*persist-credentials:[[:space:]]*false[[:space:]]*(#.*)?$/) {
      credentials_disabled = 1
      next
    }
  }

  END {
    finish_checkout()
    if (checkout_count == 0) {
      print "FAIL: no actions/checkout steps found" > "/dev/stderr"
      failures = 1
    }
    if (!failures) {
      printf "PASS: all %d actions/checkout steps disable persisted credentials\n", checkout_count
    }
    exit failures
  }
' "$workflow"

if include_keys="$(git config --local --no-includes --name-only --get-regexp '^include.*\.path$' 2>&1)"; then
  printf 'FAIL: local Git config contains include paths that can alter build identity: %s\n' "$include_keys" >&2
  exit 1
else
  git_status=$?
  if [[ "$git_status" -ne 1 ]]; then
    printf 'FAIL: could not inspect local Git config (git config exited %d): %s\n' "$git_status" "$include_keys" >&2
    exit "$git_status"
  fi
fi

printf 'PASS: local Git config has no include paths\n'
