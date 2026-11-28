#!/usr/bin/env bash
# Succeed only for a non-empty, committed documentation-only candidate diff.
# Missing or uninspectable base is not evidence for the narrow path.
set -euo pipefail
base="${AETHYME_GATE_BASE_COMMIT:-}"
[ -n "$base" ] || exit 1
git rev-parse --verify "$base^{commit}" >/dev/null || exit 1
status="$(git status --porcelain --untracked-files=normal)" || exit 1
[ -z "$status" ] || exit 1
git diff --no-renames --name-only -z "$base" HEAD -- | (
    seen=0
    while IFS= read -r -d '' path; do
        seen=1
        case "$path" in
            packages/aethyme/docs/*.md|packages/aethyme/skills/*.md|.claude/skills/*.md|.codex/skills/*.md) ;;
            *) exit 1 ;;
        esac
    done
    [ "$seen" -eq 1 ]
)
