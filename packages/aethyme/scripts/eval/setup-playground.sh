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

# Capitalize first letter for display unless explicit directory names are supplied
DISPLAY_NAME="$(echo "$NAME" | sed 's/./\U&/')"
CONTROL_DIR_NAME="${CONTROL_DIR_NAME:-$DISPLAY_NAME - Control}"
AETHYME_DIR_NAME="${AETHYME_DIR_NAME:-$DISPLAY_NAME - Aethyme}"
CONTROL_DIR="$DEST/$CONTROL_DIR_NAME"
AETHYME_DIR="$DEST/$AETHYME_DIR_NAME"

# Find Aethyme root (script is at packages/aethyme/scripts/eval/)
SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
AETHYME_ROOT="$(cd "$SCRIPT_DIR/../.." && pwd)"
ENGINE="$AETHYME_ROOT/rust/target/release/aethyme-engine-cli"

count_git_refs() {
    git for-each-ref --format='%(refname)' refs/heads refs/remotes 2>/dev/null | wc -l | tr -d ' '
}

echo "=== Playground Setup ==="
echo "  Source:  $SOURCE"
echo "  Name:    $DISPLAY_NAME"
echo "  Commit:  $COMMIT"
echo "  Dest:    $DEST"
echo "  Control: $CONTROL_DIR"
echo "  Aethyme: $AETHYME_DIR"
echo "  Engine:  $ENGINE"
echo ""

# ── Preflight checks ────────────────────────────────────────────────

if [[ ! -f "$ENGINE" ]]; then
    echo "ERROR: Engine binary not found at $ENGINE"
    echo "Build it: cd $AETHYME_ROOT/rust && cargo build --release"
    exit 1
fi

if [[ -d "$CONTROL_DIR" || -d "$AETHYME_DIR" ]]; then
    if [[ "$FORCE" == true ]]; then
        echo "WARNING: --force specified, deleting existing repos..."
        rm -rf "$CONTROL_DIR" "$AETHYME_DIR"
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
    echo "  Cleaned: $(basename "$REPO") — $(count_git_refs) refs, $(git remote | wc -l | tr -d ' ') remotes"
done

# ── Step 3: Deploy Aethyme tooling ───────────────────────────────────

echo ">>> Indexing Aethyme repo..."
cd "$AETHYME_DIR"
"$ENGINE" index --repo . 2>&1 | tail -5

echo ">>> Deploying skill..."
mkdir -p .codex/skills/aethyme
cp "$AETHYME_ROOT/skills/aethyme/SKILL.md" .codex/skills/aethyme/SKILL.md
sed -i '' "s|{{AETHYME_ROOT}}|$AETHYME_ROOT|g" .codex/skills/aethyme/SKILL.md
echo "  Skill deployed to .codex/skills/aethyme/"

# ── Step 4: Verify ───────────────────────────────────────────────────

echo ""
echo ">>> Verifying..."

ERRORS=0

# Control checks
cd "$CONTROL_DIR"
[[ -d .codex ]]   && echo "  FAIL: Control has .codex/"   && ((ERRORS++)) || true
[[ -d .aethyme ]] && echo "  FAIL: Control has .aethyme/" && ((ERRORS++)) || true
[[ -d .chau7 ]]   && echo "  FAIL: Control has .chau7/"   && ((ERRORS++)) || true
echo "  Control: clean (no .codex, .aethyme, .chau7)"

# Aethyme checks
cd "$AETHYME_DIR"
[[ -f .codex/skills/aethyme/SKILL.md ]] && echo "  Aethyme: skill present" || { echo "  FAIL: Aethyme missing skill"; ((ERRORS++)); }
[[ -d .aethyme/graph.db ]]              && echo "  Aethyme: graph.db present" || { echo "  FAIL: Aethyme missing graph.db"; ((ERRORS++)); }

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
