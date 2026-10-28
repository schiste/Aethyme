#!/usr/bin/env bash
# Print true only for a nonempty PR diff containing the exact non-Rust
# performance-reporting paths, optionally mixed with mirrored skill Markdown.
# Any uncertainty selects the full validation suite.
set -euo pipefail

select_full_suite() {
  printf 'performance-fast-lane: %s; selecting full suite\n' "$1" >&2
  printf 'false\n'
  exit 0
}

if [[ $# -ne 2 ]]; then
  select_full_suite 'expected base and head commit ids'
fi
base=$1
head=$2

if [[ ! $base =~ ^[0-9a-f]{40}$ || ! $head =~ ^[0-9a-f]{40}$ ]]; then
  select_full_suite 'commit id is not a full SHA-1'
fi
if ! repo_root=$(git rev-parse --show-toplevel 2>/dev/null); then
  select_full_suite 'not inside a Git worktree'
fi
if ! git -C "$repo_root" cat-file -e "$base^{commit}" 2>/dev/null \
  || ! git -C "$repo_root" cat-file -e "$head^{commit}" 2>/dev/null; then
  select_full_suite 'base or head commit is unavailable'
fi
if ! git -C "$repo_root" merge-base --is-ancestor "$base" "$head" 2>/dev/null; then
  select_full_suite 'base is not an ancestor of head'
fi

if ! diff_file=$(mktemp "${TMPDIR:-/tmp}/aethyme-performance-fast-lane.XXXXXX"); then
  select_full_suite 'could not create a temporary diff file'
fi
trap 'rm -f "$diff_file"' EXIT
if ! git -C "$repo_root" diff --raw --no-renames --no-ext-diff --no-textconv -z \
  "$base" "$head" >"$diff_file"; then
  select_full_suite 'Git could not read the requested diff'
fi

records=0
performance_paths=0
while :; do
  raw=''
  if ! IFS= read -r -d '' raw; then
    [[ -z $raw ]] || select_full_suite 'diff record is not NUL terminated'
    break
  fi
  path=''
  if ! IFS= read -r -d '' path; then
    select_full_suite 'diff path is missing or not NUL terminated'
  fi
  [[ $raw == :* ]] || select_full_suite 'diff record has an unexpected format'
  read -r old_mode new_mode old_oid new_oid status extra <<<"${raw#:}"
  [[ -n ${status:-} && -z ${extra:-} ]] || select_full_suite 'diff metadata is incomplete'

  case "$status" in
    A)
      [[ $old_mode == 000000 && $new_mode == 100644 ]] \
        || select_full_suite 'added path is not a regular non-executable file'
      ;;
    M)
      [[ $old_mode == 100644 && $new_mode == 100644 ]] \
        || select_full_suite 'modified path changed file type or mode'
      ;;
    *)
      select_full_suite 'diff contains a deletion, rename, type change, or other status'
      ;;
  esac

  case "$path" in
    .claude/skills/*.md|.codex/skills/*.md)
      ;;
    .github/workflows/product-performance.yml|\
    packages/aethyme-eval/benchmarks/performance/broker-budgets.json|\
    packages/aethyme-eval/benchmarks/performance/broker-action-matrix-v2.json|\
    packages/aethyme-eval/benchmarks/performance/broker-action-scenarios-v1.json|\
    packages/aethyme-eval/benchmarks/performance/broker_action_matrix.py|\
    packages/aethyme-eval/benchmarks/performance/broker_concurrent_submit.py|\
    packages/aethyme-eval/benchmarks/performance/test_broker_concurrent_submit.py|\
    packages/aethyme-eval/benchmarks/performance/broker_latency.py|\
    packages/aethyme-eval/benchmarks/performance/test_broker_latency.py|\
    packages/aethyme-eval/benchmarks/performance/git_worktree_config.py|\
    packages/aethyme-eval/benchmarks/performance/test_git_worktree_config.py|\
    packages/aethyme/docs/architecture/git-worktree-config-assessment.md|\
    packages/aethyme/docs/guides/broker-performance.md)
      performance_paths=$((performance_paths + 1))
      ;;
    *)
      select_full_suite 'changed path is outside the exact performance and mirrored skill allowlist'
      ;;
  esac
  records=$((records + 1))
done <"$diff_file"

if (( records == 0 || performance_paths == 0 )); then
  select_full_suite 'diff has no exact performance-reporting path'
fi
printf 'true\n'
