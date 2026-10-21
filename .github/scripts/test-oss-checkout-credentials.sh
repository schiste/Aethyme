#!/usr/bin/env bash
set -euo pipefail

repo_root="$(git rev-parse --show-toplevel)"
workflow="${1:-$repo_root/.github/workflows/oss-ci.yml}"
workflow_only="${2:-}"
script_path="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)/$(basename "${BASH_SOURCE[0]}")"
if [[ $# -gt 2 || ( $# -eq 2 && $workflow_only != '--workflow-only' ) ]]; then
  printf 'FAIL: expected [workflow] [--workflow-only]\n' >&2
  exit 2
fi
if [[ ! -f "$workflow" ]]; then
  printf 'FAIL: workflow file not found: %s\n' "$workflow" >&2
  exit 1
fi

awk '
  function is_checkout_use(line, lowered) {
    lowered = tolower(line)
    return index(lowered, "actions/checkout") > 0 &&
      (lowered ~ /^[[:space:]]*-[[:space:]]*uses:[[:space:]]*/ ||
       lowered ~ /^[[:space:]]*uses:[[:space:]]*/)
  }

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

    if (is_checkout_use($0)) {
      step_indent = line_indent
      if ($0 ~ /^[[:space:]]*uses:[[:space:]]*/) {
        step_indent = line_indent - 2
      }
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
    if (with_seen && line_indent == step_indent + 2 &&
        $0 ~ /^[[:space:]]*[A-Za-z0-9_-]+[[:space:]]*:/ &&
        $0 !~ /^[[:space:]]*with[[:space:]]*:/) {
      with_seen = 0
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

if [[ $workflow_only == '--workflow-only' ]]; then
  exit 0
fi

fixtures="$(mktemp -d "${TMPDIR:-/tmp}/aethyme-checkout-credentials.XXXXXX")"
trap 'rm -rf "$fixtures"' EXIT

expect_workflow_rejected() {
  local label=$1 fixture=$2 output
  if output="$(bash "$script_path" "$fixture" --workflow-only 2>&1)"; then
    printf 'FAIL: %s unexpectedly passed the checkout ratchet\n' "$label" >&2
    exit 1
  fi
  case "$output" in
    *'must set with.persist-credentials: false'*)
      printf 'PASS: %s is rejected\n' "$label"
      ;;
    *)
      printf 'FAIL: %s failed for an unexpected reason\n%s\n' "$label" "$output" >&2
      exit 1
      ;;
  esac
}

# A value under a sibling env: map must not satisfy the with: input check.
awk '
  BEGIN { first_checkout = 0; changed_with = 0; injected_env = 0 }
  !first_checkout && /uses:[[:space:]]*actions\/checkout@/ { first_checkout = 1 }
  first_checkout && !changed_with && /persist-credentials:[[:space:]]*false/ {
    sub(/false/, "true")
    changed_with = 1
  }
  first_checkout && !injected_env && /fetch-depth:[[:space:]]*0/ {
    print
    print "        env:"
    print "          persist-credentials: false"
    injected_env = 1
    first_checkout = 0
    next
  }
  { print }
  END { if (!changed_with || !injected_env) exit 2 }
' "$workflow" >"$fixtures/env-sibling.yml"
expect_workflow_rejected 'persist-credentials under env instead of with' "$fixtures/env-sibling.yml"

# Quoted literal uses values still identify checkout steps; dropping their
# input must fail rather than silently reducing the checkout count.
awk '
  BEGIN { quoted_checkout = 0; removed_input = 0 }
  !quoted_checkout && /uses:[[:space:]]*actions\/checkout@/ {
    sub(/uses: actions\/checkout@/, "uses: \"actions/checkout@")
    sub(/ # v7\.0\.1$/, "\" # v7.0.1")
    quoted_checkout = 1
    print
    next
  }
  quoted_checkout && !removed_input && /persist-credentials:[[:space:]]*false/ {
    removed_input = 1
    next
  }
  { print }
  END { if (!quoted_checkout || !removed_input) exit 2 }
' "$workflow" >"$fixtures/quoted-checkout.yml"
expect_workflow_rejected 'quoted checkout reference without disabled credentials' "$fixtures/quoted-checkout.yml"

printf 'All checkout credential ratchet controls passed.\n'
