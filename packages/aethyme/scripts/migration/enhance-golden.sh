#!/bin/bash
# enhance-golden.sh — artifact parity harness for retirement Phase 2.
#
# Phase 0's golden-diff.sh freezes CLI stdout; for the enhance/skills
# port the contract is the DEPLOYED BYTES. This harness runs
# `enhance deploy` (+ verify) against a fresh fixture repo through the
# aethyme router and snapshots every produced artifact, byte-for-byte.
#
#   ./scripts/migration/enhance-golden.sh capture <golden-dir>
#   ./scripts/migration/enhance-golden.sh compare <golden-dir>
#
# Two fixture conditions: bare repo, and repo with agents overrides
# (.aethyme/overrides/agents.json) — the override corpus requirement
# from python-retirement-plan Phase 2.
#
# HARD-FAIL POLICY: missing binaries or failing commands abort; the
# harness never skips (gate blind-spot lesson, 2026-07-17).

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
[[ -x "$PYTHON" ]] || { echo "FATAL: $PYTHON missing" >&2; exit 1; }

echo ">>> Building router binary..."
cargo build --quiet --manifest-path "$PACKAGE_ROOT/rust/Cargo.toml" --bin aethyme \
    || { echo "FATAL: cargo build failed" >&2; exit 1; }
TARGET_DIR="$PACKAGE_ROOT/rust/target"
AETHYME_BIN="$TARGET_DIR/release/aethyme"
[[ -x "$AETHYME_BIN" ]] || AETHYME_BIN="$TARGET_DIR/debug/aethyme"
[[ -x "$AETHYME_BIN" ]] || { echo "FATAL: aethyme binary not found" >&2; exit 1; }

WORK_DIR="$(mktemp -d)"
trap 'rm -rf "$WORK_DIR"' EXIT

make_repo() {
    local repo="$1" with_overrides="$2"
    mkdir -p "$repo/src"
    printf '# Fixture Repo\n\nEnhance target.\n' > "$repo/README.md"
    printf 'def entry():\n    return 1\n' > "$repo/src/main.py"
    git -C "$repo" init -q 2>/dev/null || true
    if [[ "$with_overrides" == "yes" ]]; then
        mkdir -p "$repo/.aethyme/overrides"
        cat > "$repo/.aethyme/overrides/agents.json" <<'EOF'
{
  "project_context": "Fixture project for enhance parity.",
  "conventions": ["Use tabs never", "Prefer small commits"],
  "commands": {"test": "python -m pytest -q"},
  "warnings": ["Do not touch generated files"]
}
EOF
    fi
}

FAILED=0
for condition in bare overrides; do
    REPO="$WORK_DIR/repo-$condition"
    with="no"; [[ "$condition" == "overrides" ]] && with="yes"
    make_repo "$REPO" "$with"

    echo ">>> enhance deploy ($condition)..."
    # AETHYME_ENHANCE_NATIVE passes through explicitly: with it set to 1
    # in the harness environment, the router answers natively (the Phase 2
    # port); unset/empty keeps the Python implementation answering.
    AETHYME_ROOT="$PACKAGE_ROOT" AETHYME_ENHANCE_NATIVE="${AETHYME_ENHANCE_NATIVE:-}" \
        "$AETHYME_BIN" enhance deploy --repo "$REPO" --force \
        > "$WORK_DIR/deploy-$condition.out" 2>&1 \
        || { echo "FATAL: enhance deploy failed ($condition)"; cat "$WORK_DIR/deploy-$condition.out"; exit 1; }
    AETHYME_ROOT="$PACKAGE_ROOT" AETHYME_ENHANCE_NATIVE="${AETHYME_ENHANCE_NATIVE:-}" \
        "$AETHYME_BIN" enhance verify --repo "$REPO" \
        > "$WORK_DIR/verify-$condition.out" 2>&1 \
        || { echo "FATAL: enhance verify failed ($condition)"; cat "$WORK_DIR/verify-$condition.out"; exit 1; }

    # Snapshot every deployed artifact, normalizing the machine-local root.
    DEST="$GOLDEN_DIR/$condition"
    if [[ "$MODE" == "capture" ]]; then
        rm -rf "$DEST"; mkdir -p "$DEST"
    fi
    while IFS= read -r -d '' file; do
        rel="${file#"$REPO"/}"
        case "$rel" in
            .git/*|src/*|README.md) continue ;;
            # Runtime telemetry — timestamped by design, not part of the
            # deterministic template contract (ports in Phase 3).
            .aethyme/generated/experience-*) continue ;;
        esac
        norm="$WORK_DIR/norm"
        # Timestamp scrub: Python's deploy embeds generated_at wall-clock
        # stamps (onboarding.json, repo-onboarding SKILL.md) — a known
        # determinism gap vs the Phase 2 byte-determinism goal; the port
        # resolves it deliberately (see python-retirement-plan).
        sed -E -e "s|$PACKAGE_ROOT|{AETHYME_ROOT}|g" \
            -e "s|/private$REPO|{REPO}|g" \
            -e "s|$REPO|{REPO}|g" \
            -e "s/[0-9]{4}-[0-9]{2}-[0-9]{2}T[0-9]{2}:[0-9]{2}:[0-9]{2}\+00:00/{TIMESTAMP}/g" \
            "$file" > "$norm"
        if [[ "$MODE" == "capture" ]]; then
            mkdir -p "$DEST/$(dirname "$rel")"
            cp "$norm" "$DEST/$rel"
            echo "  captured $condition/$rel"
        else
            if [[ ! -f "$DEST/$rel" ]]; then
                echo "FAIL [$condition/$rel]: artifact not in goldens" >&2
                FAILED=$((FAILED + 1))
            elif ! cmp -s "$DEST/$rel" "$norm"; then
                echo "FAIL [$condition/$rel]: bytes drifted" >&2
                diff "$DEST/$rel" "$norm" | head -6 >&2
                FAILED=$((FAILED + 1))
            else
                echo "  parity  $condition/$rel"
            fi
        fi
    done < <(find "$REPO" -type f -print0)

    # Stdout parity: deploy/verify command output is part of the frozen
    # surface too. Same volatile normalization as the artifacts.
    STDOUT_DEST="$GOLDEN_DIR/stdout"
    mkdir -p "$STDOUT_DEST"
    for stream in deploy verify; do
        norm="$WORK_DIR/norm-stdout"
        sed -E -e "s|$PACKAGE_ROOT|{AETHYME_ROOT}|g" \
            -e "s|/private$REPO|{REPO}|g" \
            -e "s|$REPO|{REPO}|g" \
            -e "s/[0-9]{4}-[0-9]{2}-[0-9]{2}T[0-9]{2}:[0-9]{2}:[0-9]{2}\+00:00/{TIMESTAMP}/g" \
            "$WORK_DIR/$stream-$condition.out" > "$norm"
        if [[ "$MODE" == "capture" ]]; then
            cp "$norm" "$STDOUT_DEST/$condition-$stream.out"
            echo "  captured stdout/$condition-$stream.out"
        elif [[ ! -f "$STDOUT_DEST/$condition-$stream.out" ]]; then
            echo "FAIL [stdout/$condition-$stream.out]: stdout not in goldens" >&2
            FAILED=$((FAILED + 1))
        elif ! cmp -s "$STDOUT_DEST/$condition-$stream.out" "$norm"; then
            echo "FAIL [stdout/$condition-$stream.out]: stdout drifted" >&2
            diff "$STDOUT_DEST/$condition-$stream.out" "$norm" | head -10 >&2
            FAILED=$((FAILED + 1))
        else
            echo "  parity  stdout/$condition-$stream.out"
        fi
    done

    # In compare mode, also catch artifacts that STOPPED being deployed.
    if [[ "$MODE" == "compare" && -d "$DEST" ]]; then
        while IFS= read -r -d '' golden; do
            rel="${golden#"$DEST"/}"
            if [[ ! -f "$REPO/$rel" ]]; then
                echo "FAIL [$condition/$rel]: golden artifact no longer deployed" >&2
                FAILED=$((FAILED + 1))
            fi
        done < <(find "$DEST" -type f -print0)
    fi
done

echo ""
if [[ $FAILED -gt 0 ]]; then
    echo "=== $MODE: $FAILED artifact(s) FAILED ==="
    exit 1
fi
echo "=== $MODE: all artifacts OK ==="
