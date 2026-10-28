#!/usr/bin/env bash
# Offline controls for the exact non-Rust performance path classifier.
set -euo pipefail

# Git selectors from the caller must not redirect disposable fixture setup or
# the classifier's local repository reads.
for git_variable in "${!GIT_@}"; do
  unset "$git_variable"
done

script_dir=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
classifier="$script_dir/performance-fast-lane.sh"
fixtures=$(mktemp -d "${TMPDIR:-/tmp}/aethyme-performance-fast-lane-tests.XXXXXX")
trap 'rm -rf "$fixtures"' EXIT

case_number=0
repo=''
base=''
init_case() {
  case_number=$((case_number + 1))
  repo="$fixtures/$case_number-$1"
  mkdir -p "$repo"
  git -C "$repo" init -q
  git -C "$repo" checkout -q -b main
  git -C "$repo" config user.name 'Performance fast-lane tests'
  git -C "$repo" config user.email 'performance-fast-lane@example.invalid'
  git -C "$repo" config core.filemode true
  printf 'fixture\n' >"$repo/seed"
  git -C "$repo" add -- seed
  git -C "$repo" commit -qm 'base fixture'
  base=$(git -C "$repo" rev-parse HEAD)
}

commit_changes() {
  git -C "$repo" commit -qm 'fixture change'
}

expect_result() {
  local label=$1 expected=$2 head actual stderr_file
  head=$(git -C "$repo" rev-parse HEAD)
  stderr_file="$fixtures/classifier-$case_number.stderr"
  if ! actual=$(cd "$repo" && "$classifier" "$base" "$head" 2>"$stderr_file"); then
    printf 'FAIL: %s classifier exited nonzero\n' "$label" >&2
    cat "$stderr_file" >&2
    exit 1
  fi
  if [[ $actual != "$expected" ]]; then
    printf 'FAIL: %s expected %s, got %s\n' "$label" "$expected" "$actual" >&2
    cat "$stderr_file" >&2
    exit 1
  fi
  printf 'PASS: %s => %s\n' "$label" "$actual"
}

expect_exact_performance_path() {
  local label=$1 path=$2
  init_case "$label"
  mkdir -p "$repo/${path%/*}"
  printf 'performance input\n' >"$repo/$path"
  git -C "$repo" add -- "$path"
  commit_changes
  expect_result "$label" true
}

expect_exact_performance_path 'performance workflow' '.github/workflows/product-performance.yml'
expect_exact_performance_path 'performance budget policy' 'packages/aethyme-eval/benchmarks/performance/broker-budgets.json'
expect_exact_performance_path 'action matrix budget contract' 'packages/aethyme-eval/benchmarks/performance/broker-action-matrix-v2.json'
expect_exact_performance_path 'action matrix scenario contract' 'packages/aethyme-eval/benchmarks/performance/broker-action-scenarios-v1.json'
expect_exact_performance_path 'action matrix harness' 'packages/aethyme-eval/benchmarks/performance/broker_action_matrix.py'
expect_exact_performance_path 'concurrent broker benchmark' 'packages/aethyme-eval/benchmarks/performance/broker_concurrent_submit.py'
expect_exact_performance_path 'concurrent broker benchmark tests' 'packages/aethyme-eval/benchmarks/performance/test_broker_concurrent_submit.py'
expect_exact_performance_path 'standalone Python benchmark' 'packages/aethyme-eval/benchmarks/performance/broker_latency.py'
expect_exact_performance_path 'Python benchmark tests' 'packages/aethyme-eval/benchmarks/performance/test_broker_latency.py'
expect_exact_performance_path 'performance report guide' 'packages/aethyme/docs/guides/broker-performance.md'
expect_exact_performance_path 'Git config experiment' 'packages/aethyme-eval/benchmarks/performance/git_worktree_config.py'
expect_exact_performance_path 'Git config controls' 'packages/aethyme-eval/benchmarks/performance/test_git_worktree_config.py'
expect_exact_performance_path 'Git config assessment' 'packages/aethyme/docs/architecture/git-worktree-config-assessment.md'

init_case 'modified performance budget policy'
mkdir -p "$repo/packages/aethyme-eval/benchmarks/performance"
printf 'before\n' >"$repo/packages/aethyme-eval/benchmarks/performance/broker-budgets.json"
git -C "$repo" add -- packages/aethyme-eval/benchmarks/performance/broker-budgets.json
commit_changes
base=$(git -C "$repo" rev-parse HEAD)
printf 'after\n' >"$repo/packages/aethyme-eval/benchmarks/performance/broker-budgets.json"
git -C "$repo" add -- packages/aethyme-eval/benchmarks/performance/broker-budgets.json
commit_changes
expect_result 'modified performance budget policy' true

init_case 'performance plus mirrored skills'
mkdir -p "$repo/.claude/skills/repo-act" "$repo/.codex/skills/repo-act" \
  "$repo/packages/aethyme-eval/benchmarks/performance"
printf 'guidance\n' >"$repo/.claude/skills/repo-act/SKILL.md"
printf 'guidance\n' >"$repo/.codex/skills/repo-act/SKILL.md"
printf 'policy\n' >"$repo/packages/aethyme-eval/benchmarks/performance/broker-budgets.json"
git -C "$repo" add -- .claude/skills/repo-act/SKILL.md .codex/skills/repo-act/SKILL.md \
  packages/aethyme-eval/benchmarks/performance/broker-budgets.json
commit_changes
expect_result 'performance and mirrored skill changes run both checks' true

init_case 'skills only'
mkdir -p "$repo/.claude/skills/repo-act"
printf 'guidance\n' >"$repo/.claude/skills/repo-act/SKILL.md"
git -C "$repo" add -- .claude/skills/repo-act/SKILL.md
commit_changes
expect_result 'skill-only diff remains on docs classifier' false

expect_full_path() {
  local label=$1 path=$2
  init_case "$label"
  mkdir -p "$repo/${path%/*}"
  printf 'other input\n' >"$repo/$path"
  git -C "$repo" add -- "$path"
  commit_changes
  expect_result "$label" false
}

expect_full_path 'OSS CI workflow' '.github/workflows/oss-ci.yml'
expect_full_path 'cross-process workflow' '.github/workflows/cross-process-contract.yml'
expect_full_path 'gate workflow' '.github/workflows/aethyme-gates.yml'
expect_full_path 'lookalike workflow' '.github/workflows/product-performance-extra.yml'
expect_full_path 'Rust source' 'packages/aethyme/rust/crates/example/src/lib.rs'
expect_full_path 'workspace manifest' 'packages/aethyme/rust/Cargo.toml'
expect_full_path 'Cargo lockfile' 'packages/aethyme/rust/Cargo.lock'
expect_full_path 'Cargo build profile' '.cargo/config.toml'
expect_full_path 'gate policy' '.aethyme/gates.toml'
expect_full_path 'Rust performance fixture' 'packages/aethyme-eval/benchmarks/performance/fixture/src/rust/src/lib.rs'
expect_full_path 'unlisted benchmark file' 'packages/aethyme-eval/benchmarks/performance/other.py'
expect_full_path 'shared Git benchmark dependency' 'packages/aethyme-eval/benchmarks/performance/git_worktree_options.py'
expect_full_path 'Git config lookalike' 'packages/aethyme-eval/benchmarks/performance/git_worktree_config_extra.py'
expect_full_path 'classifier itself' '.github/scripts/performance-fast-lane.sh'
expect_full_path 'validator itself' '.github/scripts/validate-performance-fast-lane.sh'

init_case 'performance mixed with Rust'
mkdir -p "$repo/packages/aethyme-eval/benchmarks/performance" \
  "$repo/packages/aethyme/rust/crates/example/src"
printf 'policy\n' >"$repo/packages/aethyme-eval/benchmarks/performance/broker-budgets.json"
printf 'fn main() {}\n' >"$repo/packages/aethyme/rust/crates/example/src/main.rs"
git -C "$repo" add -- packages/aethyme-eval/benchmarks/performance/broker-budgets.json \
  packages/aethyme/rust/crates/example/src/main.rs
commit_changes
expect_result 'performance mixed with Rust selects full suite' false

init_case 'performance symlink'
mkdir -p "$repo/packages/aethyme-eval/benchmarks/performance"
ln -s target.json "$repo/packages/aethyme-eval/benchmarks/performance/broker-budgets.json"
git -C "$repo" add -- packages/aethyme-eval/benchmarks/performance/broker-budgets.json
commit_changes
expect_result 'symlink at an allowed path selects full suite' false

init_case 'regular performance file replaced by a symlink'
mkdir -p "$repo/packages/aethyme-eval/benchmarks/performance"
printf 'policy\n' >"$repo/packages/aethyme-eval/benchmarks/performance/broker-budgets.json"
git -C "$repo" add -- packages/aethyme-eval/benchmarks/performance/broker-budgets.json
commit_changes
base=$(git -C "$repo" rev-parse HEAD)
rm "$repo/packages/aethyme-eval/benchmarks/performance/broker-budgets.json"
ln -s target.json "$repo/packages/aethyme-eval/benchmarks/performance/broker-budgets.json"
git -C "$repo" add -- packages/aethyme-eval/benchmarks/performance/broker-budgets.json
commit_changes
expect_result 'regular performance file replaced by a symlink' false

init_case 'modified performance symlink'
mkdir -p "$repo/packages/aethyme-eval/benchmarks/performance"
ln -s first.json "$repo/packages/aethyme-eval/benchmarks/performance/broker-budgets.json"
git -C "$repo" add -- packages/aethyme-eval/benchmarks/performance/broker-budgets.json
commit_changes
base=$(git -C "$repo" rev-parse HEAD)
rm "$repo/packages/aethyme-eval/benchmarks/performance/broker-budgets.json"
ln -s second.json "$repo/packages/aethyme-eval/benchmarks/performance/broker-budgets.json"
git -C "$repo" add -- packages/aethyme-eval/benchmarks/performance/broker-budgets.json
commit_changes
expect_result 'modified performance symlink' false

init_case 'performance deletion'
mkdir -p "$repo/packages/aethyme-eval/benchmarks/performance"
printf 'policy\n' >"$repo/packages/aethyme-eval/benchmarks/performance/broker-budgets.json"
git -C "$repo" add -- packages/aethyme-eval/benchmarks/performance/broker-budgets.json
commit_changes
base=$(git -C "$repo" rev-parse HEAD)
rm "$repo/packages/aethyme-eval/benchmarks/performance/broker-budgets.json"
git -C "$repo" add -u -- packages/aethyme-eval/benchmarks/performance/broker-budgets.json
commit_changes
expect_result 'deletion at an allowed path selects full suite' false

init_case 'performance mode change'
mkdir -p "$repo/packages/aethyme-eval/benchmarks/performance"
printf 'policy\n' >"$repo/packages/aethyme-eval/benchmarks/performance/broker-budgets.json"
git -C "$repo" add -- packages/aethyme-eval/benchmarks/performance/broker-budgets.json
commit_changes
base=$(git -C "$repo" rev-parse HEAD)
chmod +x "$repo/packages/aethyme-eval/benchmarks/performance/broker-budgets.json"
git -C "$repo" add -- packages/aethyme-eval/benchmarks/performance/broker-budgets.json
commit_changes
expect_result 'executable mode change selects full suite' false

init_case 'empty diff'
expect_result 'empty diff selects full suite' false

init_case 'invalid revision'
if ! actual=$(cd "$repo" && "$classifier" "$base" 'not-a-commit' 2>"$fixtures/invalid.stderr"); then
  printf 'FAIL: invalid revision classifier exited nonzero\n' >&2
  cat "$fixtures/invalid.stderr" >&2
  exit 1
fi
[[ $actual == false ]] || { printf 'FAIL: invalid revision expected false, got %s\n' "$actual" >&2; exit 1; }
printf 'PASS: invalid revision => false\n'

init_case 'non-ancestor base'
git -C "$repo" checkout -q --orphan disconnected
git -C "$repo" rm -rfq -- .
mkdir -p "$repo/packages/aethyme-eval/benchmarks/performance"
printf 'policy\n' >"$repo/packages/aethyme-eval/benchmarks/performance/broker-budgets.json"
git -C "$repo" add -- packages/aethyme-eval/benchmarks/performance/broker-budgets.json
commit_changes
expect_result 'non-ancestor base selects full suite' false

printf 'All performance-fast-lane classifier controls passed.\n'
