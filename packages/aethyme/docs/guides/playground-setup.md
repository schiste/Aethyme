# Playground Setup Guide

Last Updated: 2026-07-28

How to create an eval playground — a pair of repos (Control + Aethyme) from any source repository.

## Prerequisites

- Git
- Engine binary compiled: `cd packages/aethyme/rust && cargo build --release`
- Router on PATH: `cargo install --path packages/aethyme/rust/crates/aethyme-cli`
  (no Python: the product path has needed none since 2026-08-01)
- Python venv ONLY for the eval runner scripts under `scripts/eval/`:
  `cd packages/aethyme && python3 -m venv .venv`
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

### 3. Hide playground-generated artifacts from ordinary discovery

Playground artifacts are local scaffolding, not benchmark source. Add local
exclude rules to both clones so Git and ripgrep ignore generated Aethyme,
Chau7, Claude, and Codex artifacts during ordinary discovery. These rules live
under `.git/info/exclude`, so they do not modify the source repository.

```bash
for repo in "$DEST/Mediawiki - Control" "$DEST/Mediawiki - Aethyme"; do
  cat >> "$repo/.git/info/exclude" <<'EOF'

# AETHYME_PLAYGROUND_GENERATED_ARTIFACTS
.aethyme/
.chau7/
.claude/
.codex/
AGENTS.md
CLAUDE.md
EOF
done
```

If the source repository was already enhanced and any of those files were
tracked, mark them `skip-worktree` before removing Control-side contamination
or deploying the Aethyme condition. The setup script handles this for you.

### 4. Deploy Aethyme tooling (Aethyme repo only)

```bash
AETHYME_ROOT=/path/to/Aethyme/packages/aethyme
ENGINE="$AETHYME_ROOT/rust/target/release/aethyme-engine-cli"
GRAPH_INDEXER="$AETHYME_ROOT/rust/target/release/aethyme-graph-index"

cd "$DEST/Mediawiki - Aethyme"

# Build committed fragments, then materialize the local Redb graph store
$GRAPH_INDEXER \
  --repo-root "$DEST/Mediawiki - Aethyme" \
  --repo-name "Mediawiki - Aethyme" \
  --engine-version local
$ENGINE index --repo .

# Deploy generated root guidance and per-product skills.
"$AETHYME_ROOT/rust/target/release/aethyme" enhance deploy --repo "$PWD" --force
```

The generated `AGENTS.md` and `CLAUDE.md` quick start must point Explore at
the native binary:

```bash
"$AETHYME_ROOT/rust/target/release/aethyme" explore \
  --repo "$PWD" --request "<your task>" --format answer-json
```

They may warn that `python -m src.cli explore` was removed, but they must not
present it as executable guidance. Since 2026-08-01 the Python CLI does not
exist at all — the old spelling fails with `No module named src`.

### 5. Verify

```bash
./scripts/eval/verify-playground.sh --target mediawiki
```

Or manually:

```bash
# Control: must be clean
ls "$DEST/Mediawiki - Control/.codex" 2>/dev/null && echo "FAIL: .codex exists" || echo "OK"
ls "$DEST/Mediawiki - Control/.aethyme" 2>/dev/null && echo "FAIL: .aethyme exists" || echo "OK"
ls "$DEST/Mediawiki - Control/.chau7" 2>/dev/null && echo "FAIL: .chau7 exists" || echo "OK"

# Aethyme: must have skill, references, fragments, and local Redb store
ls "$DEST/Mediawiki - Aethyme/.codex/skills/aethyme/SKILL.md" || echo "FAIL: no skill"
ls "$DEST/Mediawiki - Aethyme/.codex/skills/aethyme/references/explore.md" || echo "FAIL: no Explore reference"
ls "$DEST/Mediawiki - Aethyme/.aethyme/graph" || echo "FAIL: no fragment graph"
ls "$DEST/Mediawiki - Aethyme/.aethyme/graph_store.redb" || echo "FAIL: no Redb graph store"
cd "$DEST/Mediawiki - Aethyme"
for path in .aethyme/graph_store.redb .codex/skills/aethyme/SKILL.md AGENTS.md CLAUDE.md; do
  git check-ignore -q "$path" \
    && echo "OK: ignored $path" \
    || echo "FAIL: generated artifact visible: $path"
done
git status --porcelain --untracked-files=all

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

## Clean Codex A/B Runs

Use the bundled Codex wrapper only after `verify-playground.sh` passes for the
pair. The wrapper requires an explicit arm so the Control condition cannot
accidentally inherit the Aethyme tool surface:

```bash
COMMON_ENV=(
  AETHYME_EVAL_PROMPT="Inspect the auth/token flow without modifying files."
  AETHYME_EVAL_FIXTURE_ID=edge_proxy_backend_auth
  AETHYME_EVAL_TASK_CLASS=auth_token_behavior
  AETHYME_EVAL_OUTPUT_SCHEMA_FILE=/path/to/schema.json
)

env "${COMMON_ENV[@]}" \
  AETHYME_EVAL_ARM=control \
  AETHYME_EVAL_REPO="$DEST/Mediawiki - Control" \
  AETHYME_EVAL_ARTIFACT_DIR=/tmp/aethyme-ab/control \
  "$AETHYME_ROOT/.venv/bin/python" "$AETHYME_ROOT/scripts/eval/run_codex_eval.py"

env "${COMMON_ENV[@]}" \
  AETHYME_EVAL_ARM=aethyme \
  AETHYME_EVAL_REPO="$DEST/Mediawiki - Aethyme" \
  AETHYME_EVAL_TOOL_REPO="$AETHYME_ROOT" \
  AETHYME_EVAL_ARTIFACT_DIR=/tmp/aethyme-ab/aethyme \
  "$AETHYME_ROOT/.venv/bin/python" "$AETHYME_ROOT/scripts/eval/run_codex_eval.py"
```

The wrapper runs both arms with `codex exec --ignore-user-config --json`,
preserves `events.jsonl`, `stderr.log`, `last-message.json`, `command.json`,
`contract.json`, and `leakage.json`, and emits wall time, input/output tokens,
command-output chars, event-log chars, and stderr chars. The Control arm strips
`AETHYME*`/`AETHYMEBENCH*` environment variables and does not add the tool repo
to Codex. The Aethyme arm adds only `AETHYME_EVAL_TOOL_REPO` as the tool
surface. Any generated artifact path (`.aethyme/`, `.chau7/`, `.codex/`,
`.claude/`, generated `AGENTS.md`, generated `CLAUDE.md`, or
`graph_store.redb`) in selected files, snippets, command output, or the final
answer fails the run before the result can be interpreted.

The runner result JSON is written to stdout and now includes a normalized
`regression_metrics` block. Redirect each arm's stdout to a result file when
preparing an A/B pair for the metric gate.

To capture the result files used below:

```bash
mkdir -p /tmp/aethyme-ab/control /tmp/aethyme-ab/aethyme

env "${COMMON_ENV[@]}" \
  AETHYME_EVAL_ARM=control \
  AETHYME_EVAL_REPO="$DEST/Mediawiki - Control" \
  AETHYME_EVAL_ARTIFACT_DIR=/tmp/aethyme-ab/control \
  "$AETHYME_ROOT/.venv/bin/python" "$AETHYME_ROOT/scripts/eval/run_codex_eval.py" \
  > /tmp/aethyme-ab/control/result.json

env "${COMMON_ENV[@]}" \
  AETHYME_EVAL_ARM=aethyme \
  AETHYME_EVAL_REPO="$DEST/Mediawiki - Aethyme" \
  AETHYME_EVAL_TOOL_REPO="$AETHYME_ROOT" \
  AETHYME_EVAL_ARTIFACT_DIR=/tmp/aethyme-ab/aethyme \
  "$AETHYME_ROOT/.venv/bin/python" "$AETHYME_ROOT/scripts/eval/run_codex_eval.py" \
  > /tmp/aethyme-ab/aethyme/result.json
```

Compare a completed pair with the metric gate:

```bash
"$AETHYME_ROOT/.venv/bin/python" "$AETHYME_ROOT/scripts/eval/check_regression_gate.py" \
  --control /tmp/aethyme-ab/control/result.json \
  --control-repeat /tmp/aethyme-ab/control-repeat/result.json \
  --aethyme /tmp/aethyme-ab/aethyme/result.json \
  --aethyme-repeat /tmp/aethyme-ab/aethyme-repeat/result.json \
  --fixture edge_proxy_backend_auth \
  --expected-missing-coverage edge_proxy \
  --control-quality 4 \
  --aethyme-quality 4
```

The gate checks uncached-plus-output budget delta, selected file count delta,
snippet count delta, command-output char delta plus total/per-command
command-output caps, Explore output size, cumulative replay-token estimate,
generated-artifact leakage, Aethyme invocation, first Aethyme call before broad
repo search, deterministic repeat output, Surface/Flow coverage reporting,
auth/token Surface/Flow lane presence, and reviewer-rubric quality. It also
reports total input/output, cached input, and total token estimate as
context-pressure telemetry; cached input does not count as marginal budget
spend. It intentionally does not require selected-file identity equality. A
broad `rg` after successful Explore is reported as a warning because it usually
explains overspend, but it does not by itself fail an otherwise interpretable
A/B result. Missing reviewer scores fail the gate unless
`--allow-missing-quality` is passed for an exploratory dry run. Missing repeat
results, missing Surface/Flow coverage, missing event sequencing, or missing
auth/token subsystem lanes fail the strict gate; use the corresponding
`--allow-missing-*` option only for dry runs that will not be interpreted as V2
evidence. Missing or malformed budget/hygiene metrics fail rather than being
treated as zero. Aethyme invocation is recognized from command metadata, not
from stdout or prose mentions of commands.

For the full V2 suite, use a manifest so the fixture-family coverage is checked
as part of the same gate:

```json
{
  "runs": [
    {
      "fixture_id": "edge_proxy_backend_auth",
      "control": "edge-proxy/control/result.json",
      "control_repeat": "edge-proxy/control-repeat/result.json",
      "aethyme": "edge-proxy/aethyme/result.json",
      "aethyme_repeat": "edge-proxy/aethyme-repeat/result.json",
      "expected_missing_coverage": ["edge_proxy"],
      "control_quality": 4,
      "aethyme_quality": 4
    }
  ]
}
```

The manifest is valid only when it includes all required fixture ids from
`docs/guides/eval-protocol.md` in cadence order: edge proxy + backend auth,
Django backend auth, OIDC + session auth, webhook secret auth, config-owned
middleware behavior, frontend-to-backend route behavior, and queue/job
behavior. The regression gate fails manifests that drift from this order.

## Adding a New Eval Target

The `src/eval/targets.py` registry and orchestrator files were removed
with the evaluation stack (2026-07-13). Targets now resolve by naming
convention: place the pair at
`$AETHYME_PLAYGROUND_ROOT/<Name>/<Name> - {Control,Aethyme}` (default
root: `~/Downloads/Repositories/Playground`) and add the target slug to
the `case` block in `scripts/eval/verify-playground.sh`.

1. Clone and set up the playground (steps above)
2. Add the slug → display-name mapping in `verify-playground.sh`
3. Validate: `./scripts/eval/verify-playground.sh --target <slug>`

Aethyme itself must not be registered as a benchmark target. The repository
contains eval references, historical reports, and tooling artifacts that can
leak answer keys into the target under assessment. Use external repositories as
benchmark targets and keep Aethyme self-runs as unscored diagnostics only.

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
"$AETHYME_ROOT/rust/target/release/aethyme" enhance deploy --repo "$PWD" --force
```

**"Generated artifacts appear in git status or rg output"** — The local
exclude block is missing from `.git/info/exclude`. Re-run
`setup-playground.sh --force`, or add the
`AETHYME_PLAYGROUND_GENERATED_ARTIFACTS` block from step 3 and rerun
`verify-playground.sh`.
