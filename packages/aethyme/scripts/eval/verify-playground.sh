#!/bin/zsh
# verify-playground.sh — Validate that a playground pair is correctly set up.
#
# Usage:
#   ./scripts/eval/verify-playground.sh --target mediawiki
#   ./scripts/eval/verify-playground.sh --control <path> --aethyme <path>

set -uo pipefail

# ── Parse arguments ──────────────────────────────────────────────────

TARGET="" CONTROL_DIR="" AETHYME_DIR=""

while [[ $# -gt 0 ]]; do
    case "$1" in
        --target)  TARGET="$2";      shift 2 ;;
        --control) CONTROL_DIR="$2"; shift 2 ;;
        --aethyme) AETHYME_DIR="$2"; shift 2 ;;
        *) echo "Unknown option: $1"; exit 1 ;;
    esac
done

# Resolve paths from target registry
if [[ -n "$TARGET" && -z "$CONTROL_DIR" ]]; then
    SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
    AETHYME_ROOT="$(cd "$SCRIPT_DIR/../.." && pwd)"
    PATHS=$(cd "$AETHYME_ROOT" && "$AETHYME_ROOT/.venv/bin/python" -c "
from src.eval.targets import get_target
t = get_target('$TARGET')
print(t.control_path)
print(t.aethyme_path)
" 2>/dev/null) || { echo "ERROR: Unknown target '$TARGET'"; exit 1; }
    CONTROL_DIR=$(echo "$PATHS" | head -1)
    AETHYME_DIR=$(echo "$PATHS" | tail -1)
fi

if [[ -z "$CONTROL_DIR" || -z "$AETHYME_DIR" ]]; then
    echo "Usage: $0 --target <name>  OR  --control <path> --aethyme <path>"
    exit 1
fi

# Find engine binary
SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
AETHYME_ROOT="$(cd "$SCRIPT_DIR/../.." && pwd)"
ENGINE="$AETHYME_ROOT/rust/target/release/aethyme-engine-cli"

echo "=== Verifying Playground ==="
echo "  Control: $CONTROL_DIR"
echo "  Aethyme: $AETHYME_DIR"
echo ""

ERRORS=0
WARNINGS=0

check_pass() { echo "  [PASS] $1"; }
check_fail() { echo "  [FAIL] $1"; ((ERRORS++)); }
check_warn() { echo "  [WARN] $1"; ((WARNINGS++)); }
count_git_refs() {
    git for-each-ref --format='%(refname)' refs/heads refs/remotes 2>/dev/null | wc -l | tr -d ' '
}

# ── Control Repo ─────────────────────────────────────────────────────

echo "--- Control Repo ---"

# Exists and is git repo
[[ -d "$CONTROL_DIR" ]]      && check_pass "Exists" || { check_fail "Directory missing: $CONTROL_DIR"; }
[[ -d "$CONTROL_DIR/.git" ]] && check_pass "Is git repo" || check_fail "Not a git repo"

if [[ -d "$CONTROL_DIR/.git" ]]; then
    cd "$CONTROL_DIR"

    # Detached HEAD
    git symbolic-ref HEAD &>/dev/null && check_fail "Not detached HEAD (on a branch)" || check_pass "Detached HEAD"

    # No remote
    REMOTES=$(git remote | wc -l | tr -d ' ')
    [[ "$REMOTES" == "0" ]] && check_pass "No remotes" || check_fail "$REMOTES remote(s) found — agents can access fix commits"

    # No local branches
    BRANCHES=$(count_git_refs)
    [[ "$BRANCHES" == "0" ]] && check_pass "No local branches" || check_fail "$BRANCHES branch(es) found — agents can access via git log --all"

    # No contamination
    [[ -d .codex ]]   && check_fail "Has .codex/ (skill contamination)" || check_pass "No .codex/"
    [[ -d .aethyme ]] && check_fail "Has .aethyme/ (engine contamination)" || check_pass "No .aethyme/"
    if [[ -d .chau7 ]]; then
        check_warn "Has .chau7/ (Chau7 created this — delete it)"
    else
        check_pass "No .chau7/"
    fi
    if [[ -d .claude ]]; then
        check_warn "Has .claude/ (Claude Code created this)"
    else
        check_pass "No .claude/"
    fi

    # No uncommitted changes
    DIRTY=$(git status --porcelain 2>/dev/null | wc -l | tr -d ' ')
    [[ "$DIRTY" == "0" ]] && check_pass "Clean working tree" || check_warn "$DIRTY uncommitted change(s)"
fi

echo ""

# ── Aethyme Repo ─────────────────────────────────────────────────────

echo "--- Aethyme Repo ---"

[[ -d "$AETHYME_DIR" ]]      && check_pass "Exists" || { check_fail "Directory missing: $AETHYME_DIR"; }
[[ -d "$AETHYME_DIR/.git" ]] && check_pass "Is git repo" || check_fail "Not a git repo"

if [[ -d "$AETHYME_DIR/.git" ]]; then
    cd "$AETHYME_DIR"

    # Detached HEAD
    git symbolic-ref HEAD &>/dev/null && check_fail "Not detached HEAD" || check_pass "Detached HEAD"

    # No remote
    REMOTES=$(git remote | wc -l | tr -d ' ')
    [[ "$REMOTES" == "0" ]] && check_pass "No remotes" || check_fail "$REMOTES remote(s) found"

    # No local branches
    BRANCHES=$(count_git_refs)
    [[ "$BRANCHES" == "0" ]] && check_pass "No local branches" || check_fail "$BRANCHES branch(es) found"

    # Skill deployed
    [[ -f .codex/skills/aethyme/SKILL.md ]] && check_pass "Skill deployed" || check_fail "Missing .codex/skills/aethyme/SKILL.md"
    [[ -d .codex/skills/eval ]] && check_fail "Internal eval skill leaked into playground" || check_pass "No internal eval skill deployed"

    # Skill has no unresolved placeholders
    if [[ -f .codex/skills/aethyme/SKILL.md ]]; then
        SKILL_FILE=".codex/skills/aethyme/SKILL.md"
        grep -q '{{AETHYME_ROOT}}' "$SKILL_FILE" && check_fail "Skill has unresolved {{AETHYME_ROOT}} placeholder" || check_pass "Skill placeholders resolved"
        # 2026-05-08: Python explore_command was hard-deleted. The skill's
        # explore guidance now references the native Rust binary
        # (`$AETHYME_BIN explore`); the `intents` command stays in Python.
        grep -q 'src.cli intents' "$SKILL_FILE" && check_pass "Skill includes current intent catalog guidance" || check_fail "Skill missing intents guidance"
        (grep -q '"$AETHYME_BIN" explore' "$SKILL_FILE" || grep -q 'aethyme explore' "$SKILL_FILE") \
            && check_pass "Skill includes current native-explore guidance" \
            || check_fail "Skill missing native explore guidance"
        # The post-2026-05-08 SKILL.md should NOT mention `src.cli explore` —
        # that's the deleted Python entry point. Flip the check: if it's
        # there, the skill is stale.
        grep -q 'src.cli explore' "$SKILL_FILE" \
            && check_fail "Skill still references deleted 'src.cli explore' (Python explore_command was hard-deleted 2026-05-08; redeploy with current template)" \
            || check_pass "Skill has no stale 'src.cli explore' references"
        grep -q 'analyze dead-code' "$SKILL_FILE" && check_pass "Skill includes current dead-code analyzer" || check_fail "Skill missing analyze dead-code guidance"
        grep -q 'facts function-usage' "$SKILL_FILE" && check_pass "Skill includes current usage facts command" || check_fail "Skill missing facts function-usage guidance"
        grep -q '\$ENGINE unused' "$SKILL_FILE" && check_fail "Skill still advertises stale \$ENGINE unused command" || check_pass "Skill has no stale unused command"
    fi

    # Graph index
    [[ -d .aethyme/graph.db ]] && check_pass "Graph index present" || check_fail "Missing .aethyme/graph.db — run: $ENGINE index --repo ."
fi

echo ""

# ── Cross-checks ─────────────────────────────────────────────────────

echo "--- Cross-checks ---"

if [[ -d "$CONTROL_DIR/.git" && -d "$AETHYME_DIR/.git" ]]; then
    CONTROL_HEAD=$(cd "$CONTROL_DIR" && git rev-parse HEAD)
    AETHYME_HEAD=$(cd "$AETHYME_DIR" && git rev-parse HEAD)
    if [[ "$CONTROL_HEAD" == "$AETHYME_HEAD" ]]; then
        check_pass "Same HEAD commit: ${CONTROL_HEAD:0:12}"
    else
        check_fail "Different HEAD commits — Control: ${CONTROL_HEAD:0:12}, Aethyme: ${AETHYME_HEAD:0:12}"
    fi
fi

# Engine binary
[[ -f "$ENGINE" ]] && check_pass "Engine binary exists" || check_fail "Engine binary missing: $ENGINE"

echo ""

# ── Summary ──────────────────────────────────────────────────────────

if [[ $ERRORS -eq 0 && $WARNINGS -eq 0 ]]; then
    echo "=== All checks passed ==="
    exit 0
elif [[ $ERRORS -eq 0 ]]; then
    echo "=== Passed with $WARNINGS warning(s) ==="
    exit 0
else
    echo "=== $ERRORS failure(s), $WARNINGS warning(s) ==="
    exit 1
fi
