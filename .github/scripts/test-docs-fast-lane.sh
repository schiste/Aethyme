#!/usr/bin/env bash
# Small, offline controls for the docs-only path classifier.
set -euo pipefail

# Git selectors from the caller must not redirect disposable fixture setup or
# the classifier's local repository reads.
for git_variable in "${!GIT_@}"; do
  unset "$git_variable"
done

script_dir=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
classifier="$script_dir/docs-fast-lane.sh"
fixtures=$(mktemp -d "${TMPDIR:-/tmp}/aethyme-docs-fast-lane-tests.XXXXXX")
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
  git -C "$repo" config user.name 'Docs fast-lane tests'
  git -C "$repo" config user.email 'docs-fast-lane@example.invalid'
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
  local label=$1 expected=$2 head actual
  head=$(git -C "$repo" rev-parse HEAD)
  actual=$(cd "$repo" && "$classifier" "$base" "$head" 2>/dev/null)
  if [[ $actual != "$expected" ]]; then
    printf 'FAIL: %s expected %s, got %s\n' "$label" "$expected" "$actual" >&2
    exit 1
  fi
  printf 'PASS: %s => %s\n' "$label" "$actual"
}
expect_bad_ref() {
  local label=$1 expected=$2 actual
  actual=$(cd "$repo" && "$classifier" "$base" 'not-a-commit' 2>/dev/null)
  if [[ $actual != "$expected" ]]; then
    printf 'FAIL: %s expected %s, got %s\n' "$label" "$expected" "$actual" >&2
    exit 1
  fi
  printf 'PASS: %s => %s\n' "$label" "$actual"
}
expect_unavailable_ref() {
  local label=$1 expected=$2 actual unavailable
  printf -v unavailable '%040d' 0
  actual=$(cd "$repo" && "$classifier" "$base" "$unavailable" 2>/dev/null)
  if [[ $actual != "$expected" ]]; then
    printf 'FAIL: %s expected %s, got %s\n' "$label" "$expected" "$actual" >&2
    exit 1
  fi
  printf 'PASS: %s => %s\n' "$label" "$actual"
}

init_case add-paired-markdown
mkdir -p "$repo/.claude/skills/aethyme" "$repo/.codex/skills/aethyme"
printf 'paired guidance\n' >"$repo/.claude/skills/aethyme/SKILL.md"
printf 'paired guidance\n' >"$repo/.codex/skills/aethyme/SKILL.md"
git -C "$repo" add -- .claude/skills/aethyme/SKILL.md .codex/skills/aethyme/SKILL.md
commit_changes
expect_result 'added mirrored skill Markdown' true

init_case modify-paired-markdown
mkdir -p "$repo/.claude/skills/aethyme" "$repo/.codex/skills/aethyme"
printf 'before\n' >"$repo/.claude/skills/aethyme/SKILL.md"
printf 'before\n' >"$repo/.codex/skills/aethyme/SKILL.md"
git -C "$repo" add -- .claude/skills/aethyme/SKILL.md .codex/skills/aethyme/SKILL.md
commit_changes
base=$(git -C "$repo" rev-parse HEAD)
printf 'after\n' >"$repo/.claude/skills/aethyme/SKILL.md"
printf 'after\n' >"$repo/.codex/skills/aethyme/SKILL.md"
git -C "$repo" add -- .claude/skills/aethyme/SKILL.md .codex/skills/aethyme/SKILL.md
commit_changes
expect_result 'modified mirrored skill Markdown' true

init_case nul-safe-path
path=$'.codex/skills/aethyme/name with\nnewline.md'
mkdir -p "$repo/${path%/*}"
printf 'guidance\n' >"$repo/$path"
git -C "$repo" add -- "$path"
commit_changes
expect_result 'Markdown path containing whitespace and newline' true

# Each report is a committed synthetic blob; no report content is executed.
report_case() {
  local label=$1 expected=$2 content=$3
  init_case "$label"
  mkdir -p "$repo/packages/aethyme/docs/reports"
  printf '%s' "$content" >"$repo/packages/aethyme/docs/reports/example.md"
  git -C "$repo" add -- packages/aethyme/docs/reports/example.md
  commit_changes
  expect_result "$label" "$expected"
}
report_case prose-report true $'# Diagnostic report\n\nMeasured wall time: 12 ms.\n'
report_case backtick-code false $'# Report\n```rust\nfn main() {}\n```\n'
report_case tilde-code false $'# Report\n~~~json\n{}\n~~~\n'
report_case indented-code false $'# Report\n  ```bash\ntrue\n  ```\n'
report_case legacy-blob-link false '[source](https://github.com/aeptus/aethyme/blob/main/file.rs)'
report_case invalid-utf8 false $'Invalid: \377\n'

init_case modified-prose-report
mkdir -p "$repo/packages/aethyme/docs/reports" "$repo/.codex/skills/aethyme"
printf 'before\n' >"$repo/packages/aethyme/docs/reports/example.md"
git -C "$repo" add -- packages/aethyme/docs/reports/example.md
commit_changes
base=$(git -C "$repo" rev-parse HEAD)
printf 'after\n' >"$repo/packages/aethyme/docs/reports/example.md"
printf 'skill\n' >"$repo/.codex/skills/aethyme/SKILL.md"
git -C "$repo" add -- packages/aethyme/docs/reports/example.md .codex/skills/aethyme/SKILL.md
commit_changes
expect_result 'modified prose report mixed with skill' true

init_case binary-report
mkdir -p "$repo/packages/aethyme/docs/reports"
printf 'prose\0hidden' >"$repo/packages/aethyme/docs/reports/example.md"
git -C "$repo" add -- packages/aethyme/docs/reports/example.md
commit_changes
expect_result 'NUL report' false

init_case oversized-report
mkdir -p "$repo/packages/aethyme/docs/reports"
# Bound the test fixture too: exactly one byte over the accepted limit.
python3 - "$repo/packages/aethyme/docs/reports/example.md" <<'OVERSIZE'
import pathlib
import sys
pathlib.Path(sys.argv[1]).write_bytes(b"a" * 1048577)
OVERSIZE
git -C "$repo" add -- packages/aethyme/docs/reports/example.md
commit_changes
expect_result 'oversized report' false

init_case report-with-rust
mkdir -p "$repo/packages/aethyme/docs/reports"
printf 'prose\n' >"$repo/packages/aethyme/docs/reports/example.md"
printf 'fn main() {}\n' >"$repo/main.rs"
git -C "$repo" add -- packages/aethyme/docs/reports/example.md main.rs
commit_changes
expect_result 'report mixed with Rust' false

init_case mixed-rust-path
mkdir -p "$repo/.claude/skills/aethyme" "$repo/packages/aethyme/rust/crates/example/src"
printf 'guidance\n' >"$repo/.claude/skills/aethyme/SKILL.md"
printf 'fn main() {}\n' >"$repo/packages/aethyme/rust/crates/example/src/main.rs"
git -C "$repo" add -- .claude/skills/aethyme/SKILL.md packages/aethyme/rust/crates/example/src/main.rs
commit_changes
expect_result 'Markdown mixed with Rust source' false

init_case outside-markdown
mkdir -p "$repo/packages/aethyme/docs/guides"
printf 'documentation\n' >"$repo/packages/aethyme/docs/guides/example.md"
git -C "$repo" add -- packages/aethyme/docs/guides/example.md
commit_changes
expect_result 'Markdown outside mirrored skill trees' false

init_case package-skill-template
mkdir -p "$repo/packages/aethyme/skills/aethyme"
printf 'template guidance\n' >"$repo/packages/aethyme/skills/aethyme/SKILL.md"
git -C "$repo" add -- packages/aethyme/skills/aethyme/SKILL.md
commit_changes
expect_result 'package skill template outside mirrored trees' false

init_case hook-script
mkdir -p "$repo/.claude/hooks"
printf '#!/bin/sh\n' >"$repo/.claude/hooks/example.sh"
git -C "$repo" add -- .claude/hooks/example.sh
commit_changes
expect_result 'hook script under a mirrored tool root' false

init_case workflow
mkdir -p "$repo/.github/workflows"
printf 'name: changed\n' >"$repo/.github/workflows/oss-ci.yml"
git -C "$repo" add -- .github/workflows/oss-ci.yml
commit_changes
expect_result 'workflow change' false

init_case policy
mkdir -p "$repo/.aethyme"
printf 'policy = true\n' >"$repo/.aethyme/gates.toml"
git -C "$repo" add -- .aethyme/gates.toml
commit_changes
expect_result 'broker policy change' false

init_case cargo-lock
mkdir -p "$repo/packages/aethyme/rust"
printf 'version = 4\n' >"$repo/packages/aethyme/rust/Cargo.lock"
git -C "$repo" add -- packages/aethyme/rust/Cargo.lock
commit_changes
expect_result 'Cargo lockfile change' false

init_case deletion
mkdir -p "$repo/.claude/skills/aethyme"
printf 'old guidance\n' >"$repo/.claude/skills/aethyme/removed.md"
git -C "$repo" add -- .claude/skills/aethyme/removed.md
commit_changes
base=$(git -C "$repo" rev-parse HEAD)
rm "$repo/.claude/skills/aethyme/removed.md"
git -C "$repo" add -A -- .claude/skills/aethyme/removed.md
commit_changes
expect_result 'Markdown deletion' false

init_case rename
mkdir -p "$repo/.codex/skills/aethyme"
printf 'guidance\n' >"$repo/.codex/skills/aethyme/old.md"
git -C "$repo" add -- .codex/skills/aethyme/old.md
commit_changes
base=$(git -C "$repo" rev-parse HEAD)
git -C "$repo" mv -- .codex/skills/aethyme/old.md .codex/skills/aethyme/new.md
commit_changes
expect_result 'Markdown rename' false

init_case symlink-mode
mkdir -p "$repo/.claude/skills/aethyme"
ln -s target.md "$repo/.claude/skills/aethyme/link.md"
git -C "$repo" add -- .claude/skills/aethyme/link.md
commit_changes
expect_result 'symlink ending in Markdown suffix' false

init_case regular-to-symlink
mkdir -p "$repo/.codex/skills/aethyme"
printf 'guidance\n' >"$repo/.codex/skills/aethyme/link.md"
git -C "$repo" add -- .codex/skills/aethyme/link.md
commit_changes
base=$(git -C "$repo" rev-parse HEAD)
rm "$repo/.codex/skills/aethyme/link.md"
ln -s target.md "$repo/.codex/skills/aethyme/link.md"
git -C "$repo" add -A -- .codex/skills/aethyme/link.md
commit_changes
expect_result 'regular Markdown replaced by a symlink' false

init_case modified-symlink
mkdir -p "$repo/.claude/skills/aethyme"
ln -s first-target.md "$repo/.claude/skills/aethyme/link.md"
git -C "$repo" add -- .claude/skills/aethyme/link.md
commit_changes
base=$(git -C "$repo" rev-parse HEAD)
rm "$repo/.claude/skills/aethyme/link.md"
ln -s second-target.md "$repo/.claude/skills/aethyme/link.md"
git -C "$repo" add -A -- .claude/skills/aethyme/link.md
commit_changes
expect_result 'modified symlink ending in Markdown suffix' false

init_case executable-mode
mkdir -p "$repo/.codex/skills/aethyme"
printf 'guidance\n' >"$repo/.codex/skills/aethyme/SKILL.md"
chmod +x "$repo/.codex/skills/aethyme/SKILL.md"
git -C "$repo" add -- .codex/skills/aethyme/SKILL.md
commit_changes
expect_result 'executable Markdown add' false

init_case mode-change
mkdir -p "$repo/.claude/skills/aethyme"
printf 'guidance\n' >"$repo/.claude/skills/aethyme/SKILL.md"
git -C "$repo" add -- .claude/skills/aethyme/SKILL.md
commit_changes
base=$(git -C "$repo" rev-parse HEAD)
chmod +x "$repo/.claude/skills/aethyme/SKILL.md"
git -C "$repo" add -- .claude/skills/aethyme/SKILL.md
commit_changes
expect_result 'Markdown mode change' false

init_case empty-diff
expect_result 'empty diff' false
expect_bad_ref 'invalid revision' false
expect_unavailable_ref 'unavailable full-length revision' false

init_case non-ancestor
git -C "$repo" checkout -q --orphan disconnected
git -C "$repo" rm -rfq -- .
mkdir -p "$repo/.codex/skills/aethyme"
printf 'guidance\n' >"$repo/.codex/skills/aethyme/SKILL.md"
git -C "$repo" add -- .codex/skills/aethyme/SKILL.md
commit_changes
expect_result 'valid but non-ancestor base commit' false

printf 'All docs-fast-lane classifier controls passed.\n'
