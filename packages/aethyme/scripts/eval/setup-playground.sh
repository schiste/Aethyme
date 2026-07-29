#!/bin/zsh
# setup-playground.sh — Create a Control + Aethyme playground pair from a source repo.
#
# Usage:
#   ./scripts/eval/setup-playground.sh \
#     --source <git-url-or-local-path> \
#     --name <display-name> \
#     --commit <sha> \
#     --dest <directory> \
#     [--control-dir-name <name>] \
#     [--aethyme-dir-name <name>] \
#     [--force]
#
# Example:
#   ./scripts/eval/setup-playground.sh \
#     --source https://github.com/wikimedia/mediawiki.git \
#     --name mediawiki \
#     --commit 8b6613f3996 \
#     --dest ~/Downloads/Repositories/Playground/Mediawiki

set -euo pipefail

# ── Parse arguments ──────────────────────────────────────────────────

SOURCE="" NAME="" COMMIT="" DEST="" CONTROL_DIR_NAME="" AETHYME_DIR_NAME="" FORCE=false

while [[ $# -gt 0 ]]; do
    case "$1" in
        --source) SOURCE="$2"; shift 2 ;;
        --name)   NAME="$2";   shift 2 ;;
        --commit) COMMIT="$2"; shift 2 ;;
        --dest)   DEST="$2";   shift 2 ;;
        --control-dir-name) CONTROL_DIR_NAME="$2"; shift 2 ;;
        --aethyme-dir-name) AETHYME_DIR_NAME="$2"; shift 2 ;;
        --force)  FORCE=true;  shift ;;
        *) echo "Unknown option: $1"; exit 1 ;;
    esac
done

if [[ -z "$SOURCE" || -z "$NAME" || -z "$COMMIT" || -z "$DEST" ]]; then
    echo "Usage: $0 --source <url> --name <name> --commit <sha> --dest <dir> [--control-dir-name <name>] [--aethyme-dir-name <name>] [--force]"
    exit 1
fi

# ── Derive paths ─────────────────────────────────────────────────────

# Capitalize first letter for display unless explicit directory names are supplied.
# zsh + BSD sed do not support the GNU-style \U escape used previously.
DISPLAY_NAME="${(C)NAME}"
CONTROL_DIR_NAME="${CONTROL_DIR_NAME:-$DISPLAY_NAME - Control}"
AETHYME_DIR_NAME="${AETHYME_DIR_NAME:-$DISPLAY_NAME - Aethyme}"
CONTROL_DIR="$DEST/$CONTROL_DIR_NAME"
AETHYME_DIR="$DEST/$AETHYME_DIR_NAME"

# Find Aethyme root (script is at packages/aethyme/scripts/eval/)
SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
AETHYME_ROOT="$(cd "$SCRIPT_DIR/../.." && pwd)"
ENGINE="$AETHYME_ROOT/rust/target/release/aethyme-engine-cli"
GRAPH_INDEXER="$AETHYME_ROOT/rust/target/release/aethyme-graph-index"
GRAPH_ENGINE_VERSION="${AETHYME_GRAPH_ENGINE_VERSION:-local}"
PLAYGROUND_EXCLUDE_MARKER="# AETHYME_PLAYGROUND_GENERATED_ARTIFACTS"

count_git_refs() {
    git for-each-ref --format='%(refname)' refs/heads refs/remotes 2>/dev/null | wc -l | tr -d ' '
}

write_playground_excludes() {
    local repo="$1"
    local exclude_file="$repo/.git/info/exclude"

    mkdir -p "$(dirname "$exclude_file")"
    if [[ -f "$exclude_file" ]] && grep -q "$PLAYGROUND_EXCLUDE_MARKER" "$exclude_file"; then
        return
    fi

    cat >> "$exclude_file" <<'EOF'

# AETHYME_PLAYGROUND_GENERATED_ARTIFACTS
# Local eval/runtime scaffolding. These files may exist so agent products can
# load Aethyme, but they are not benchmark source and should not appear in
# ordinary Git/ripgrep discovery.
.aethyme/
.chau7/
.claude/
.codex/
AGENTS.md
CLAUDE.md
EOF
}

hide_tracked_generated_artifacts() {
    local repo="$1"
    local generated_path tracked_files

    cd "$repo"
    for generated_path in .codex .aethyme .chau7 .claude AGENTS.md CLAUDE.md; do
        tracked_files=("${(@f)$(git ls-files "$generated_path" 2>/dev/null)}")
        tracked_files=("${(@)tracked_files:#}")
        if (( ${#tracked_files[@]} > 0 )); then
            git update-index --skip-worktree -- "${tracked_files[@]}"
        fi
    done
}

generated_artifacts_are_ignored() {
    local generated_path
    for generated_path in .aethyme/graph_store.redb .codex/skills/aethyme/SKILL.md .claude/skills/aethyme/SKILL.md AGENTS.md CLAUDE.md; do
        git check-ignore -q -- "$generated_path" || return 1
    done
}

echo "=== Playground Setup ==="
echo "  Source:  $SOURCE"
echo "  Name:    $DISPLAY_NAME"
echo "  Commit:  $COMMIT"
echo "  Dest:    $DEST"
echo "  Control: $CONTROL_DIR"
echo "  Aethyme: $AETHYME_DIR"
echo "  Engine:  $ENGINE"
echo "  Graph indexer: $GRAPH_INDEXER"
echo "  Graph version: $GRAPH_ENGINE_VERSION"
echo ""

# ── Preflight checks ────────────────────────────────────────────────

if [[ ! -f "$ENGINE" ]]; then
    echo "ERROR: Engine binary not found at $ENGINE"
    echo "Build it: cd $AETHYME_ROOT/rust && cargo build --release"
    exit 1
fi

if [[ ! -f "$GRAPH_INDEXER" ]]; then
    echo "ERROR: Graph indexer binary not found at $GRAPH_INDEXER"
    echo "Build it: cd $AETHYME_ROOT/rust && cargo build --release --bin aethyme-graph-index"
    exit 1
fi

# enhance deploy/verify are native since the Phase 2 flip (2026-07-29):
# the router binary answers them; python -m src.cli enhance no longer exists.
if [[ ! -f "$AETHYME_ROOT/rust/target/release/aethyme" ]]; then
    echo "ERROR: Router binary not found at $AETHYME_ROOT/rust/target/release/aethyme"
    echo "Build it: cd $AETHYME_ROOT/rust && cargo build --release --bin aethyme"
    exit 1
fi

if [[ -d "$CONTROL_DIR" || -d "$AETHYME_DIR" ]]; then
    if [[ "$FORCE" == true ]]; then
        echo "WARNING: --force specified, deleting existing repos..."
        command rm -rf "$CONTROL_DIR" "$AETHYME_DIR"
    else
        echo "ERROR: Repos already exist. Use --force to delete and recreate."
        [[ -d "$CONTROL_DIR" ]] && echo "  $CONTROL_DIR"
        [[ -d "$AETHYME_DIR" ]] && echo "  $AETHYME_DIR"
        exit 1
    fi
fi

mkdir -p "$DEST"

# ── Step 1: Clone ────────────────────────────────────────────────────

echo ">>> Cloning Control repo..."
git clone "$SOURCE" "$CONTROL_DIR"
cd "$CONTROL_DIR"
git checkout --detach "$COMMIT"
echo "  HEAD: $(git rev-parse --short HEAD)"

echo ">>> Cloning Aethyme repo..."
git clone "$SOURCE" "$AETHYME_DIR"
cd "$AETHYME_DIR"
git checkout --detach "$COMMIT"
echo "  HEAD: $(git rev-parse --short HEAD)"

# ── Step 2: Sanitize git history ─────────────────────────────────────

echo ">>> Sanitizing git history (removing remote + branches)..."

for REPO in "$CONTROL_DIR" "$AETHYME_DIR"; do
    cd "$REPO"
    # Remove remote
    git remote remove origin 2>/dev/null || true
    # Delete all local branches
    for branch in $(git for-each-ref --format='%(refname:short)' refs/heads 2>/dev/null); do
        git branch -D "$branch" 2>/dev/null || true
    done
    # Prune unreachable objects
    git reflog expire --expire=now --all
    git gc --prune=now 2>/dev/null
    write_playground_excludes "$REPO"
    echo "  Cleaned: $(basename "$REPO") — $(count_git_refs) refs, $(git remote | wc -l | tr -d ' ') remotes"
done

# Strip eval/tooling contamination from the Control repo. When the source repo is
# Aethyme itself (or already enhanced), discoverability files would otherwise leak
# into the vanilla Control clone and invalidate the playground isolation contract.
echo ">>> Removing Control-side tooling/runtime contamination..."
hide_tracked_generated_artifacts "$CONTROL_DIR"
cd "$CONTROL_DIR"
/bin/rm -rf .codex .aethyme .chau7 .claude
/bin/rm -f AGENTS.md CLAUDE.md
echo "  Removed: .codex .aethyme .chau7 .claude AGENTS.md CLAUDE.md (if present)"

# ── Step 3: Deploy Aethyme tooling ───────────────────────────────────

echo ">>> Building Aethyme fragment graph..."
cd "$AETHYME_DIR"
"$GRAPH_INDEXER" \
    --repo-root "$AETHYME_DIR" \
    --repo-name "$AETHYME_DIR_NAME" \
    --engine-version "$GRAPH_ENGINE_VERSION"

echo ">>> Materializing Redb graph store..."
"$ENGINE" index --repo . 2>&1 | tail -5

echo ">>> Deploying enhancement files..."
"$AETHYME_ROOT/rust/target/release/aethyme" enhance deploy --repo "$AETHYME_DIR" --force
hide_tracked_generated_artifacts "$AETHYME_DIR"
echo "  Aethyme: generated artifacts hidden from git/ripgrep discovery via .git/info/exclude"

# ── Step 4: Verify ───────────────────────────────────────────────────

echo ""
echo ">>> Verifying..."

ERRORS=0

# Control checks: must NOT have any enhancement files
cd "$CONTROL_DIR"
[[ -d .codex ]]    && { echo "  FAIL: Control has .codex/";    ((ERRORS++)); } || true
[[ -d .aethyme ]]  && { echo "  FAIL: Control has .aethyme/";  ((ERRORS++)); } || true
[[ -d .chau7 ]]    && { echo "  FAIL: Control has .chau7/";    ((ERRORS++)); } || true
[[ -d .claude ]]   && { echo "  FAIL: Control has .claude/";   ((ERRORS++)); } || true
[[ -f AGENTS.md ]] && { echo "  FAIL: Control has AGENTS.md";  ((ERRORS++)); } || true
[[ -f CLAUDE.md ]] && { echo "  FAIL: Control has CLAUDE.md";  ((ERRORS++)); } || true
[[ $ERRORS -eq 0 ]] && echo "  Control: clean (no enhancement files)"

# Aethyme checks: enhancement files via the canonical verifier, plus the redb store
cd "$AETHYME_DIR"
if "$AETHYME_ROOT/rust/target/release/aethyme" enhance verify --repo "$AETHYME_DIR"; then
    echo "  Aethyme: enhancement files OK"
else
    echo "  FAIL: Aethyme enhancement verification failed"
    ((ERRORS++))
fi
[[ -d .aethyme/graph ]] && echo "  Aethyme: fragment graph present" || { echo "  FAIL: Aethyme missing .aethyme/graph"; ((ERRORS++)); }
[[ -f .aethyme/graph_store.redb ]] && echo "  Aethyme: graph_store.redb present" || { echo "  FAIL: Aethyme missing graph_store.redb"; ((ERRORS++)); }
DIRTY=$(git status --porcelain --untracked-files=all 2>/dev/null | wc -l | tr -d ' ')
[[ "$DIRTY" == "0" ]] && echo "  Aethyme: generated artifacts do not appear in git status" || { echo "  FAIL: Aethyme has $DIRTY visible working-tree change(s)"; ((ERRORS++)); }
generated_artifacts_are_ignored \
    && echo "  Aethyme: generated artifacts ignored for ordinary discovery" \
    || { echo "  FAIL: Aethyme generated artifacts are not ignored by .git/info/exclude"; ((ERRORS++)); }

# Same commit
CONTROL_HEAD=$(cd "$CONTROL_DIR" && git rev-parse HEAD)
AETHYME_HEAD=$(cd "$AETHYME_DIR" && git rev-parse HEAD)
if [[ "$CONTROL_HEAD" == "$AETHYME_HEAD" ]]; then
    echo "  Both repos at: ${CONTROL_HEAD:0:12}"
else
    echo "  FAIL: Different commits — Control=$CONTROL_HEAD Aethyme=$AETHYME_HEAD"
    ((ERRORS++))
fi

echo ""
if [[ $ERRORS -eq 0 ]]; then
    echo "=== Playground ready ==="
else
    echo "=== $ERRORS verification failures ==="
    exit 1
fi
