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

# Resolve paths from the playground naming convention:
#   $PLAYGROUND_ROOT/<Name>/<Name> - {Control,Aethyme}
# (Formerly src.eval.targets, removed with the evaluation stack in 07ddf88.)
if [[ -n "$TARGET" && -z "$CONTROL_DIR" ]]; then
    PLAYGROUND_ROOT="${AETHYME_PLAYGROUND_ROOT:-$HOME/Downloads/Repositories/Playground}"
    case "$TARGET" in
        grc)       DISPLAY_NAME="Mockup" ;;      # GRC pair relocated under Mockup/; slug kept for tooling stability
        mediawiki) DISPLAY_NAME="Mediawiki" ;;
        *) echo "ERROR: Unknown target '$TARGET' (known: grc, mediawiki)"; exit 1 ;;
    esac
    CONTROL_DIR="$PLAYGROUND_ROOT/$DISPLAY_NAME/$DISPLAY_NAME - Control"
    AETHYME_DIR="$PLAYGROUND_ROOT/$DISPLAY_NAME/$DISPLAY_NAME - Aethyme"
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
check_ignored_path() {
    local generated_path="$1"
    local label="$2"
    git check-ignore --no-index -q -- "$generated_path" \
        && check_pass "$label ignored for ordinary discovery" \
        || check_fail "$label is not ignored by .git/info/exclude"
}
visible_agent_guidance_files() {
    local mode="${1:-all}"

    if [[ "$mode" == "allow-root" ]]; then
        find . \( -path ./.git -o -path ./.codex -o -path ./.claude -o -path ./.aethyme -o -path ./.chau7 \) -prune \
            -o \( -path ./AGENTS.md -o -path ./CLAUDE.md \) -prune \
            -o \( -name AGENTS.md -o -name CLAUDE.md \) -type f -print
    else
        find . \( -path ./.git -o -path ./.codex -o -path ./.claude -o -path ./.aethyme -o -path ./.chau7 \) -prune \
            -o \( -name AGENTS.md -o -name CLAUDE.md \) -type f -print
    fi
}
check_no_agent_guidance_files() {
    local mode="$1"
    local label="$2"
    local visible

    visible="$(visible_agent_guidance_files "$mode")"
    [[ -z "$visible" ]] \
        && check_pass "$label hidden from working-tree discovery" \
        || check_fail "$label still visible: ${visible//$'\n'/, }"
}
check_root_guidance() {
    local file="$1"
    local label="$2"

    [[ -f "$file" ]] && check_pass "$label present" || { check_fail "Missing $label"; return; }
    grep -q '{{AETHYME_ROOT}}' "$file" && check_fail "$label has unresolved {{AETHYME_ROOT}} placeholder" || check_pass "$label placeholders resolved"
    grep -q '"$AETHYME_ROOT/rust/target/release/aethyme" explore' "$file" \
        && check_pass "$label points Explore at native aethyme binary" \
        || check_fail "$label missing native Explore quick start"
    grep -q 'mktemp -t aethyme-explore' "$file" \
        && check_pass "$label writes full Explore JSON to temp" \
        || check_fail "$label missing temp-file Explore capture"
    grep -q 'top_verification_targets' "$file" \
        && check_pass "$label prints compact verification-target projection" \
        || check_fail "$label missing compact verification-target projection"
    grep -q 'observability.readiness' "$file" \
        && check_pass "$label inspects compact readiness" \
        || check_fail "$label missing readiness-only observability guidance"
    grep -q 'verify-targets' "$file" \
        && check_pass "$label uses bounded verify-targets source spans" \
        || check_fail "$label missing bounded verify-targets source spans"
    grep -q 'navigation_hints\[\]' "$file" \
        && check_fail "$label still tells agents to inspect navigation_hints[]" \
        || check_pass "$label does not inspect navigation_hints[] by default"
    # 2026-07-30 (python-retirement Phase 2): the delegated command groups
    # went native and deployed templates now spell every command `aethyme ...`.
    # ANY executable `-m src.cli` line is stale, not just explore.
    if grep -- '-m src.cli' "$file" | grep -Ev 'Do not run|not a valid command|was removed|retired' >/dev/null; then
        check_fail "$label still contains executable 'python -m src.cli' guidance (templates spell commands 'aethyme ...' since the Phase 2 flip; redeploy)"
    else
        check_pass "$label has no stale executable Python CLI guidance"
    fi
}
check_reference_file() {
    local file="$1"
    local label="$2"

    [[ -f "$file" ]] && check_pass "$label present" || { check_fail "Missing $label"; return; }
    grep -q '{{AETHYME_ROOT}}' "$file" && check_fail "$label has unresolved {{AETHYME_ROOT}} placeholder" || check_pass "$label placeholders resolved"
    # Same Phase 2 staleness rule as AGENTS/SKILL files: reference
    # workflows spell commands `aethyme ...`, never `python -m src.cli`.
    if grep -- '-m src.cli' "$file" | grep -Ev 'Do not run|not a valid command|was removed|retired' >/dev/null; then
        check_fail "$label still contains executable 'python -m src.cli' guidance (redeploy with current template)"
    else
        check_pass "$label has no stale executable Python CLI guidance"
    fi
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

    check_no_agent_guidance_files "all" "Control agent guidance files"
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
    check_root_guidance "AGENTS.md" "AGENTS.md"
    check_root_guidance "CLAUDE.md" "CLAUDE.md"
    check_no_agent_guidance_files "allow-root" "Source-side agent guidance files"

    # Skill has no unresolved placeholders and keeps auto-load concise. Detailed
    # command workflows live in references/ and are checked below.
    if [[ -f .codex/skills/aethyme/SKILL.md ]]; then
        SKILL_FILE=".codex/skills/aethyme/SKILL.md"
        grep -q '{{AETHYME_ROOT}}' "$SKILL_FILE" && check_fail "Skill has unresolved {{AETHYME_ROOT}} placeholder" || check_pass "Skill placeholders resolved"
        grep -q 'one bounded Explore call' "$SKILL_FILE" && check_pass "Skill states one bounded Explore-call contract" || check_fail "Skill missing one-call contract"
        grep -q 'safe_to_use_as_answer' "$SKILL_FILE" && check_pass "Skill tells agents to inspect trust fields" || check_fail "Skill missing trust-field guidance"
        grep -q 'mktemp -t aethyme-explore' "$SKILL_FILE" && check_pass "Skill writes full Explore JSON to temp" || check_fail "Skill missing temp-file Explore capture"
        grep -q 'top_verification_targets' "$SKILL_FILE" && check_pass "Skill prints compact verification-target projection" || check_fail "Skill missing compact verification-target projection"
        grep -q 'observability.readiness' "$SKILL_FILE" && check_pass "Skill inspects compact readiness" || check_fail "Skill missing readiness-only observability guidance"
        grep -q 'verify-targets' "$SKILL_FILE" && check_pass "Skill uses bounded verify-targets source spans" || check_fail "Skill missing bounded verify-targets source spans"
        grep -q '80-120' "$SKILL_FILE" && check_pass "Skill caps narrow source reads" || check_fail "Skill missing narrow source-read cap"
        grep -q 'broad `rg`' "$SKILL_FILE" && check_pass "Skill blocks broad rg until top targets fail" || check_fail "Skill missing broad-rg guard"
        grep -q 'navigation_hints\[\]' "$SKILL_FILE" && check_fail "Skill still tells agents to inspect navigation_hints[]" || check_pass "Skill does not inspect navigation_hints[] by default"
        (grep -q '"$AETHYME_BIN" explore' "$SKILL_FILE" || grep -q 'aethyme explore' "$SKILL_FILE") \
            && check_pass "Skill includes current native-explore guidance" \
            || check_fail "Skill missing native explore guidance"
        # The post-2026-05-08 SKILL.md should NOT mention `src.cli explore`,
        # and since the Phase 2 flip (2026-07-30) no deployed markdown should
        # carry ANY executable `-m src.cli` line — commands spell `aethyme ...`.
        if grep -- '-m src.cli' "$SKILL_FILE" | grep -Ev 'Do not run|not a valid command|was removed|retired' >/dev/null; then
            check_fail "Skill still contains executable 'python -m src.cli' guidance (deleted/delegated Python entry points; redeploy with current template)"
        else
            check_pass "Skill has no stale executable Python CLI guidance"
        fi
        grep -q 'references/explore.md' "$SKILL_FILE" && check_pass "Skill links Explore reference" || check_fail "Skill missing Explore reference link"
        grep -q 'references/graph-task.md' "$SKILL_FILE" && check_pass "Skill links graph/task reference" || check_fail "Skill missing graph/task reference link"
        grep -q 'references/dead-code.md' "$SKILL_FILE" && check_pass "Skill links dead-code reference" || check_fail "Skill missing dead-code reference link"
        grep -q '\$ENGINE unused' "$SKILL_FILE" && check_fail "Skill still advertises stale \$ENGINE unused command" || check_pass "Skill has no stale unused command"
    fi

    EXPLORE_REF=".codex/skills/aethyme/references/explore.md"
    GRAPH_REF=".codex/skills/aethyme/references/graph-task.md"
    DEAD_CODE_REF=".codex/skills/aethyme/references/dead-code.md"
    check_reference_file "$EXPLORE_REF" "Explore reference"
    check_reference_file "$GRAPH_REF" "Graph/task reference"
    check_reference_file "$DEAD_CODE_REF" "Dead-code reference"
    [[ -f .claude/skills/aethyme/references/explore.md ]] && check_pass "Claude Explore reference present" || check_fail "Missing Claude Explore reference"
    [[ -f .claude/skills/aethyme/references/graph-task.md ]] && check_pass "Claude graph/task reference present" || check_fail "Missing Claude graph/task reference"
    [[ -f .claude/skills/aethyme/references/dead-code.md ]] && check_pass "Claude dead-code reference present" || check_fail "Missing Claude dead-code reference"
    if [[ -f "$EXPLORE_REF" ]]; then
        grep -q 'aethyme intents' "$EXPLORE_REF" && check_pass "Explore reference includes current intent catalog guidance" || check_fail "Explore reference missing intents guidance"
    fi
    if [[ -f "$GRAPH_REF" ]]; then
        grep -q 'graph callers' "$GRAPH_REF" && check_pass "Graph/task reference includes caller guidance" || check_fail "Graph/task reference missing caller guidance"
    fi
    if [[ -f "$DEAD_CODE_REF" ]]; then
        grep -q 'analyze dead-code' "$DEAD_CODE_REF" && check_pass "Dead-code reference includes current analyzer" || check_fail "Dead-code reference missing analyze dead-code guidance"
        grep -q 'facts function-usage' "$DEAD_CODE_REF" && check_pass "Dead-code reference includes usage facts command" || check_fail "Dead-code reference missing facts function-usage guidance"
    fi

    # Wrapper/hook shells (Phase 3 hook flip, 2026-07-30): telemetry
    # calls spell `aethyme repo record-wrapper-invocation`, guarded by
    # `command -v aethyme`. Any executable `-m src.cli` line in a
    # deployed shell is stale — redeploy with the current template.
    for wrapper in ".codex/skills/aethyme/aethyme-explore" ".claude/hooks/aethyme-load-context.sh"; do
        [[ -f "$wrapper" ]] || { check_fail "Missing deployed wrapper $wrapper"; continue; }
        grep -q '{{AETHYME_ROOT}}' "$wrapper" && check_fail "$wrapper has unresolved {{AETHYME_ROOT}} placeholder" || check_pass "$wrapper placeholders resolved"
        grep -q 'aethyme repo record-wrapper-invocation' "$wrapper" \
            && check_pass "$wrapper records telemetry via native router" \
            || check_fail "$wrapper missing native record-wrapper-invocation call (redeploy with current template)"
        # Ignore comment lines: the current templates carry provenance
        # comments mentioning the retired Python spelling.
        if grep -- '-m src.cli' "$wrapper" | grep -v '^[[:space:]]*#' >/dev/null; then
            check_fail "$wrapper still invokes 'python -m src.cli' (Phase 3 hook flip; redeploy with current template)"
        else
            check_pass "$wrapper has no stale Python CLI invocation"
        fi
    done

    # Fragment graph + Redb graph store. Since 4.7.12 the engine
    # materializes the Redb store from committed fragments; the old
    # pass/parser fallback is gone.
    [[ -d .aethyme/graph ]] && check_pass "Fragment graph present" || check_fail "Missing .aethyme/graph — run aethyme-graph-index before $ENGINE index --repo ."
    [[ -f .aethyme/graph_store.redb ]] && check_pass "Redb graph store present" || check_fail "Missing .aethyme/graph_store.redb — run: $ENGINE index --repo ."
    check_ignored_path ".aethyme/graph_store.redb" ".aethyme graph store"
    check_ignored_path ".aethyme/graph" ".aethyme fragment graph"
    check_ignored_path ".codex/skills/aethyme/SKILL.md" "Codex Aethyme skill"
    check_ignored_path ".claude/skills/aethyme/SKILL.md" "Claude Aethyme skill"
    check_ignored_path "AGENTS.md" "Generated AGENTS.md"
    check_ignored_path "CLAUDE.md" "Generated CLAUDE.md"
    check_ignored_path "docs/AGENTS.md" "Nested AGENTS.md"
    check_ignored_path "docs/CLAUDE.md" "Nested CLAUDE.md"

    DIRTY=$(git status --porcelain --untracked-files=all 2>/dev/null | wc -l | tr -d ' ')
    [[ "$DIRTY" == "0" ]] && check_pass "Generated artifacts hidden from git status" || check_fail "$DIRTY visible working-tree change(s)"
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
