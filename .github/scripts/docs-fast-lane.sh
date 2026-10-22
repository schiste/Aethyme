#!/usr/bin/env bash
# Print true only for a nonempty PR diff consisting exclusively of regular
# Markdown skills or bounded prose-only diagnostic reports.
# Any uncertainty selects the full validation suite.
set -euo pipefail

select_full_suite() {
  printf 'docs-fast-lane: %s; selecting full suite\n' "$1" >&2
  printf 'false\n'
  exit 0
}

if [[ $# -ne 2 ]]; then
  select_full_suite 'expected base and head commit ids'
fi
base=$1
head=$2

# GitHub supplies SHA-1 commit ids here. Validate before using values in Git
# revisions so malformed or option-like input cannot alter the diff command.
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

if ! diff_file=$(mktemp "${TMPDIR:-/tmp}/aethyme-docs-fast-lane.XXXXXX"); then
  select_full_suite 'could not create a temporary diff file'
fi
content_file=''
trap 'rm -f "$diff_file"; [[ -z $content_file ]] || rm -f "$content_file"' EXIT
if ! git -C "$repo_root" diff --raw --no-renames --no-ext-diff --no-textconv -z \
  "$base" "$head" >"$diff_file"; then
  select_full_suite 'Git could not read the requested diff'
fi

records=0
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
    .claude/skills/*.md|.codex/skills/*.md) ;;
    packages/aethyme/docs/reports/*.md)
      # Reports are exempt from freshness/link/spelling ratchets, but fenced
      # examples and legacy absolute blob links still require Rust doc tests.
      # Read only the committed blob, bounded before copying or decoding it.
      if ! blob_size=$(git -C "$repo_root" cat-file -s "$head:$path"); then
        select_full_suite 'report blob is unavailable'
      fi
      [[ $blob_size =~ ^[0-9]+$ && $blob_size -le 1048576 ]] \
        || select_full_suite 'report exceeds the bounded prose limit'
      if [[ -z $content_file ]]; then
        content_file=$(mktemp "${TMPDIR:-/tmp}/aethyme-report-content.XXXXXX") \
          || select_full_suite 'could not create a report validation file'
      fi
      git -C "$repo_root" cat-file blob "$head:$path" >"$content_file" \
        || select_full_suite 'could not read report blob'
      if ! python3 - "$content_file" <<'REPORT_PROSE'
import pathlib
import re
import sys

try:
    text = pathlib.Path(sys.argv[1]).read_bytes().decode("utf-8", errors="strict")
    assert "\x00" not in text
    assert not re.search(r"(?m)^[ \t]*(?:```|~~~)", text)
    # Match the existing no_absolute_github_links ratchet conservatively.
    assert not ("github.com/aeptus/aethyme" in text and "/blob/" in text)
except (OSError, UnicodeError, AssertionError):
    sys.exit(1)
REPORT_PROSE
      then
        select_full_suite 'report contains executable examples or unvalidated content'
      fi
      ;;
    *) select_full_suite 'changed path is outside supported Markdown paths' ;;
  esac
  records=$((records + 1))
done <"$diff_file"

(( records > 0 )) || select_full_suite 'diff is empty'
printf 'true\n'
