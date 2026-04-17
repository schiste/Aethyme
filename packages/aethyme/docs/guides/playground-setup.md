# Playground Setup Guide

How to create an eval playground — a pair of repos (Control + Aethyme) from any source repository.

## Prerequisites

- Git
- Engine binary compiled: `cd packages/aethyme/rust && cargo build --release`
- Python venv: `cd packages/aethyme && python -m venv .venv && .venv/bin/pip install -e .`
- Disk space: ~2x the source repo size (two clones + graph DB)

## Quick Start

```bash
cd packages/aethyme
./scripts/eval/setup-playground.sh \
  --source https://github.com/wikimedia/mediawiki.git \
  --name mediawiki \
  --commit 8b6613f3996 \
  --dest ~/Downloads/Repositories/Playground/Mediawiki
```

This creates:

```

The setup script also accepts explicit directory names when a target's playground layout does not follow the default `<Name> - Control` / `<Name> - Aethyme` convention:

```bash
./scripts/eval/setup-playground.sh \
  --source <repo> \
  --name grc \
  --commit <sha> \
  --dest ~/Downloads/Repositories/Playground/GRC \
  --control-dir-name "Playground Control" \
  --aethyme-dir-name "Playground Aethyme"
```
~/Downloads/Repositories/Playground/Mediawiki/
├── Mediawiki - Control/    ← vanilla repo, never modified
└── Mediawiki - Aethyme/    ← same commit + Aethyme skill + graph index
```

## Manual Setup (Step by Step)

### 1. Clone the source repo twice

```bash
DEST=~/Downloads/Repositories/Playground/Mediawiki
SOURCE=https://github.com/wikimedia/mediawiki.git
COMMIT=8b6613f3996

git clone "$SOURCE" "$DEST/Mediawiki - Control"
cd "$DEST/Mediawiki - Control"
git checkout --detach "$COMMIT"

git clone "$SOURCE" "$DEST/Mediawiki - Aethyme"
cd "$DEST/Mediawiki - Aethyme"
git checkout --detach "$COMMIT"
```

### 2. Sanitize git history

Agents can find fix commits via `git log --all` if remote tracking branches exist. Remove them:

```bash
# For BOTH repos:
cd "$DEST/Mediawiki - Control"
git remote remove origin
git branch -D master 2>/dev/null   # delete any local branches
git reflog expire --expire=now --all
git gc --prune=now

cd "$DEST/Mediawiki - Aethyme"
git remote remove origin
git branch -D master 2>/dev/null
git reflog expire --expire=now --all
git gc --prune=now
```

After this, `git log --all` only shows commits reachable from the detached HEAD. Fix commits on newer branches are gone.

### 3. Deploy Aethyme tooling (Aethyme repo only)

```bash
AETHYME_ROOT=~/Downloads/Repositories/Aethyme/packages/aethyme
ENGINE="$AETHYME_ROOT/rust/target/release/aethyme-engine-cli"

cd "$DEST/Mediawiki - Aethyme"

# Build the graph index
$ENGINE index --repo .

# Deploy the skill
mkdir -p .codex/skills/aethyme
cp "$AETHYME_ROOT/skills/aethyme/SKILL.md" .codex/skills/aethyme/SKILL.md
# Replace the AETHYME_ROOT placeholder with the actual path
sed -i '' "s|{{AETHYME_ROOT}}|$AETHYME_ROOT|g" .codex/skills/aethyme/SKILL.md
```

### 4. Verify

```bash
./scripts/eval/verify-playground.sh --target mediawiki
```

Or manually:

```bash
# Control: must be clean
ls "$DEST/Mediawiki - Control/.codex" 2>/dev/null && echo "FAIL: .codex exists" || echo "OK"
ls "$DEST/Mediawiki - Control/.aethyme" 2>/dev/null && echo "FAIL: .aethyme exists" || echo "OK"
ls "$DEST/Mediawiki - Control/.chau7" 2>/dev/null && echo "FAIL: .chau7 exists" || echo "OK"

# Aethyme: must have skill + index
ls "$DEST/Mediawiki - Aethyme/.codex/skills/aethyme/SKILL.md" || echo "FAIL: no skill"
ls "$DEST/Mediawiki - Aethyme/.aethyme/graph.db" || echo "FAIL: no graph"

# Both: same commit
CONTROL_HEAD=$(cd "$DEST/Mediawiki - Control" && git rev-parse HEAD)
AETHYME_HEAD=$(cd "$DEST/Mediawiki - Aethyme" && git rev-parse HEAD)
[ "$CONTROL_HEAD" = "$AETHYME_HEAD" ] && echo "OK: same commit" || echo "FAIL: different commits"
```

## Control Repo Rules

The Control repo is the scientific baseline. After initial setup:

1. **Never add files** — no `.codex/`, no `.aethyme/`, no `.chau7/`, no scripts
2. **Never run engine commands** against it — no `index`, no `callers`, no `unused`
3. **Never modify git state** — no commits, no branch creation, no config changes
4. **Check before each eval run** — Chau7 creates `.chau7/snippets/` when tabs are opened in a directory. Delete it if present.

If the Control repo gets contaminated, delete it and re-clone from scratch.

## Adding a New Eval Target

1. Clone and set up the playground (steps above)

2. Register in `src/eval/targets.py`:
```python
TARGETS["myrepo"] = EvalTarget(
    name="myrepo",
    display_name="My Repo",
    control_path=_PLAYGROUND_ROOT / "MyRepo" / "MyRepo - Control",
    aethyme_path=_PLAYGROUND_ROOT / "MyRepo" / "MyRepo - Aethyme",
    description="Language/framework, ~N files",
)
```

Example for this repo itself:
```python
TARGETS["aethyme"] = EvalTarget(
    name="aethyme",
    display_name="Aethyme",
    control_path=_PLAYGROUND_ROOT / "Aethyme" / "Aethyme - Control",
    aethyme_path=_PLAYGROUND_ROOT / "Aethyme" / "Aethyme - Aethyme",
    description="Aethyme monorepo",
    setup_source=str(Path(__file__).resolve().parents[4]),
    setup_commit="<pinned-commit-sha>",
    setup_control_dir_name="Aethyme - Control",
    setup_aethyme_dir_name="Aethyme - Aethyme",
)
```

3. Add eval scenarios in `src/eval/schemas.py` (reference data) and `src/eval/scoring.py` (scoring function)

4. Register in `src/eval/orchestrator.py` (`_EVAL_TYPE_DEFAULTS` dict)

5. Add task text in `packages/aethyme-eval-ui/server/main.py` (`EVAL_TASKS` dict)

6. Validate: `python -m src.cli eval targets`

## Troubleshooting

**"git log --all shows fix commits"** — Local branches or remote tracking branches still exist. Run:
```bash
git remote remove origin
git branch -D <branch-name>
git gc --prune=now
```

**"Engine binary not found"** — Build it: `cd packages/aethyme/rust && cargo build --release`

**"Control repo has .chau7/"** — Chau7 auto-creates this when opening a tab. Delete it:
```bash
rm -rf "$DEST/Control/.chau7"
```

**"Aethyme skill shows {{AETHYME_ROOT}}"** — The sed replacement failed. Check the path and re-run:
```bash
sed -i '' "s|{{AETHYME_ROOT}}|$AETHYME_ROOT|g" .codex/skills/aethyme/SKILL.md
```
