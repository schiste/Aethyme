#!/bin/bash
# golden-diff.sh — parity harness for the python-retirement migration.
#
# Runs the frozen command list (golden-commands.txt) against a
# deterministically built fixture repo through the `aethyme` router,
# normalizes each output, and either records goldens or diffs against
# previously recorded ones. Byte parity after normalization is the
# migration bar (python-retirement-plan.md, decision #2).
#
# Usage:
#   ./scripts/migration/golden-diff.sh capture <golden-dir>
#   ./scripts/migration/golden-diff.sh compare <golden-dir>
#
# Typical phase flow: `capture` before flipping a command group (Python
# implementation answers), `compare` after the flip (native answers).
#
# HARD-FAIL POLICY: a missing router/engine binary or a failing command
# aborts with a non-zero exit. This harness must never skip — silent
# environment-dependent skips are a known gate blind spot (see
# python-retirement-plan.md, cross-phase risks).

set -uo pipefail

MODE="${1:-}"
GOLDEN_DIR="${2:-}"
if [[ "$MODE" != "capture" && "$MODE" != "compare" ]] || [[ -z "$GOLDEN_DIR" ]]; then
    echo "usage: $0 capture|compare <golden-dir>" >&2
    exit 2
fi

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
PACKAGE_ROOT="$(cd "$SCRIPT_DIR/../.." && pwd)"
PYTHON="$PACKAGE_ROOT/.venv/bin/python"
NORMALIZE="$SCRIPT_DIR/normalize-output.py"
COMMANDS="$SCRIPT_DIR/golden-commands.txt"

[[ -x "$PYTHON" ]] || { echo "FATAL: $PYTHON missing (create the venv first)" >&2; exit 1; }

# Build what we need up front; failures are fatal, never skipped.
echo ">>> Building router + engine binaries..."
cargo build --quiet --manifest-path "$PACKAGE_ROOT/rust/Cargo.toml" \
    --bin aethyme --bin aethyme-engine-cli --bin aethyme-graph-index \
    || { echo "FATAL: cargo build failed — refusing to skip" >&2; exit 1; }

TARGET_DIR="$PACKAGE_ROOT/rust/target"
AETHYME_BIN="$TARGET_DIR/release/aethyme"
[[ -x "$AETHYME_BIN" ]] || AETHYME_BIN="$TARGET_DIR/debug/aethyme"
[[ -x "$AETHYME_BIN" ]] || { echo "FATAL: aethyme router binary not found" >&2; exit 1; }
GRAPH_INDEX="$TARGET_DIR/release/aethyme-graph-index"
[[ -x "$GRAPH_INDEX" ]] || GRAPH_INDEX="$TARGET_DIR/debug/aethyme-graph-index"
ENGINE_CLI="$TARGET_DIR/release/aethyme-engine-cli"
[[ -x "$ENGINE_CLI" ]] || ENGINE_CLI="$TARGET_DIR/debug/aethyme-engine-cli"

# Deterministic fixture in a fresh temp dir (never checked in).
WORK_DIR="$(mktemp -d)"
trap 'rm -rf "$WORK_DIR"' EXIT
echo ">>> Building medium fixture..."
REPO="$(cd "$PACKAGE_ROOT" && "$PYTHON" - "$WORK_DIR" <<'PY'
import sys
from pathlib import Path
sys.path.insert(0, ".")
from tests.support.repo_builders import build_medium_fixture_repo
print(build_medium_fixture_repo(Path(sys.argv[1])))
PY
)"
[[ -d "$REPO" ]] || { echo "FATAL: fixture build failed" >&2; exit 1; }

echo ">>> Indexing fixture (fragments + redb store)..."
"$GRAPH_INDEX" --repo-root "$REPO" --repo-name medium-fixture --engine-version golden >/dev/null \
    || { echo "FATAL: aethyme-graph-index failed" >&2; exit 1; }
"$ENGINE_CLI" index --repo "$REPO" --from-fragments >/dev/null \
    || { echo "FATAL: engine index failed" >&2; exit 1; }

mkdir -p "$GOLDEN_DIR"
FAILED=0
INDEX=0

while IFS= read -r line; do
    [[ -z "$line" || "$line" == \#* ]] && continue
    INDEX=$((INDEX + 1))
    cmd="${line//\{REPO\}/$REPO}"
    slug="$(printf '%02d' "$INDEX")-$(echo "$line" | awk '{print $1"-"$2}' | tr -cd 'a-z0-9-')"

    # shellcheck disable=SC2086 — word-splitting the frozen command line is intended
    output="$(cd "$PACKAGE_ROOT" && AETHYME_ROOT="$PACKAGE_ROOT" "$AETHYME_BIN" $cmd 2>&1)"
    status=$?
    if [[ $status -ne 0 ]]; then
        echo "FAIL [$slug]: exit $status running: $line" >&2
        echo "$output" | head -5 >&2
        FAILED=$((FAILED + 1))
        continue
    fi

    normalized="$(echo "$output" | "$PYTHON" "$NORMALIZE" --repo "$REPO")"

    if [[ "$MODE" == "capture" ]]; then
        echo "$normalized" > "$GOLDEN_DIR/$slug.golden"
        echo "  captured $slug"
    else
        if [[ ! -f "$GOLDEN_DIR/$slug.golden" ]]; then
            echo "FAIL [$slug]: no golden recorded — run capture first" >&2
            FAILED=$((FAILED + 1))
        elif ! diff -u "$GOLDEN_DIR/$slug.golden" <(echo "$normalized") > "$GOLDEN_DIR/$slug.diff" 2>&1; then
            echo "FAIL [$slug]: output drifted (see $GOLDEN_DIR/$slug.diff)" >&2
            FAILED=$((FAILED + 1))
        else
            rm -f "$GOLDEN_DIR/$slug.diff"
            echo "  parity  $slug"
        fi
    fi
done < "$COMMANDS"

echo ""
if [[ $FAILED -gt 0 ]]; then
    echo "=== $MODE: $FAILED command(s) FAILED out of $INDEX ==="
    exit 1
fi
echo "=== $MODE: all $INDEX commands OK ==="
