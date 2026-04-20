---
name: aethyme-eval
description: Run Aethyme navigation benchmarks — set up playgrounds, launch runs,
  score results, and add new scenarios. Read this before touching the assessment system.
---

# Aethyme Assessment System

You are working on the assessment system for Aethyme, a code navigation tool. Assessments measure whether Aethyme's graph-based navigation helps AI agents work more efficiently on real codebases.

## Cardinal Rules

1. **Playground only.** Assessments run against cloned repos in `~/Downloads/Repositories/Playground/`, never against the Aethyme repo itself.

2. **No assessment-driven tool changes.** Never modify the engine, skills, or pipeline to improve assessment scores. Ask: "Would I make this change if the assessment didn't exist?" If no, don't make it. See `docs/guides/eval-protocol.md` for detailed examples.

   The active repository-agnostic tooling roadmap is in `docs/guides/eval-tooling-roadmap.md`. Follow that ordering before touching `task-conditioned`.

3. **Control repo is sacred.** The Control copy of each playground repo must never be modified after initial clone. No `.codex/`, no `.aethyme/`, no `.chau7/`. If contaminated, delete and re-clone.

## Scripts

All scripts are in `scripts/eval/` and auto-detect `AETHYME_ROOT` from their own location.

### Set up a new playground

```bash
./scripts/eval/setup-playground.sh \
  --source https://github.com/wikimedia/mediawiki.git \
  --name mediawiki \
  --commit 8b6613f3996 \
  --dest ~/Downloads/Repositories/Playground/Mediawiki
```

Creates `<Name> - Control` (vanilla) and `<Name> - Aethyme` (with skill + graph index). Sanitizes git history so agents can't find fix commits via `git log --all`.

### Verify a playground

```bash
./scripts/eval/verify-playground.sh --target mediawiki
```

15-point check: no contamination on Control, skill + graph on Aethyme, same commit, engine exists. **Run this before every assessment.**

### Run an assessment

```bash
./scripts/eval/run-eval.sh --eval-type dead-code --target aethyme --model haiku
```

End-to-end: verifies playground, starts server, launches 5 conditions via Chau7 MCP, polls until done, prints the scorecard, and writes a full artifact bundle. Results are stored in SQLite at `packages/aethyme-eval-ui/server/evals.db`, visible at http://localhost:5173, and persisted under `packages/aethyme/eval-runs/<timestamp>-<target>-<type>/`.

### Prepare a target

Before any run, persist a lightweight readiness snapshot:

```bash
curl -X POST http://localhost:8000/api/repositories/prepare \
  -H "Content-Type: application/json" \
  -d '{"target":"mediawiki"}'
```

This checks repo cleanliness, engine presence, index presence, and git state. It must stay lightweight: no prompt generation, no tab launch, no agent work.

## Available Assessment Types

| Type | Target | What it tests |
|---|---|---|
| `bug-fix` | grc | Fix a failing test (remove/restore permission implication) |
| `bug-fix-1` | mediawiki | Diagnose T419918 — watchlist marks all revisions as seen (strict JSON diagnostic output) |
| `explain-repo` | any | Produce structured architecture overview |
| `navigation-ctf` | any | Find manifest, entrypoint, area relationship chain |
| `impact-analysis` | mediawiki | List all callers of `doViewUpdates()` |
| `feature-localization` | mediawiki | Trace Watch button execution from handler to DB write |
| `config-audit` | mediawiki | Find rate limiting config, definition, enforcement, override |
| `dead-code` | mediawiki, aethyme | Target-specific dead-code scan (MediaWiki Watchlist or Aethyme indexing) |
| `migration` | mediawiki | List all files referencing `WatchedItemStore` for rename |

## 5-Condition Design

Every assessment runs the same task across 5 conditions to isolate what helps:

| Condition | Repo | CTO | Aethyme Skill | Task-Specific Pack | What it measures |
|---|---|---|---|---|---|
| `control-cto-off` | Control | forceOff | No | No | Baseline: raw agent, no help |
| `control-cto-on` | Control | default | No | No | Effect of CTO file tree context |
| `explore` | Aethyme | default | Yes | No | Effect of having Aethyme available with no prompt help |
| `leverage` | Aethyme | default | Yes | No | Effect of a generic prompt nudge to use Aethyme tools |
| `task-conditioned` | Aethyme | default | Yes | Yes (engine-generated) | Effect of task-specific Aethyme guidance or context packs |

**CTO** = Context Tree Optimization — Claude Code's file tree injection into context.
**Task-Specific Pack** = Engine-generated prompt or artifact with repo structure, function listings, subsystem detail, or task navigation context.

The task-conditioned prompt is built by: `aethyme-engine-cli prompt --repo <path> --task <task> --focus overview [--subsystem <dir>]`

Current product priority:

1. strengthen `explore`
2. make `leverage` a light generic uplift over `explore`
3. revisit `task-conditioned` only after the generic tooling is stronger

Do not use assessment results as a reason to grow the task-conditioned prompt first.

## End-of-Run Metrics

Every finalized eval must expose these per-condition metrics:

- `quality_score` — the benchmark score from the task-specific (keyword) scorer
- `judge_score`, `judge_stdev`, `judge_reliable` — LLM-judge mean + intra-rater consistency
- `scorer_agreement_gap`, `scorer_agreement_divergent` — `|judge - quality|`; divergent when gap > 10
- `tool_call_count` and `top_tools` — what the agent actually used
- `total_tokens`
- `duration_seconds`
- `cost_usd`
- `score_per_1k_tokens`
- `score_per_minute`
- `global_score` / `recalculated_eval_score`
- `deliverable_status` — "success" | "degraded" | "failed"
- `primary_metric`, `minimum_meaningful_delta` — pre-registration declaration
- `judge_elapsed_seconds` — wall-clock overhead added by the judge
- `batch_id`, `run_index`, `runs_in_batch` — multi-run metadata

Batch-level aggregates (`GET /api/batches/{batch_id}`) add:
- `scenario_discrimination` — `between / within`, labelled strong/usable/low-discrimination
- `comparisons_vs_baseline` — verdict per condition (`A>B` | `B>A` | `inconclusive`)
- `judge_overhead` — total/mean seconds the judge added

`global_score` and `recalculated_eval_score` are the same stored number. It is the control-relative comparison metric used for cross-condition evaluation:

```text
100
+ quality_delta_vs_control
+ 10 * ln(token_ratio_vs_control)
+ 10 * ln(time_ratio_vs_control)
+ 5 * ln(cost_ratio_vs_control)
```

The baseline is a **rolling median** of the last K successful control-cto-off runs matching `(model, eval_type, target, scenario)` — K=10, minimum samples=3, else falls back to co-run. The `comparison` block records `baseline_source` and `baseline_window_size` so the source is always explicit.

Control-relative terms:
- `quality_delta_vs_control = quality_score - baseline_quality_score`
- `token_ratio_vs_control = baseline_total_tokens / total_tokens`
- `time_ratio_vs_control = baseline_duration_seconds / duration_seconds`
- `cost_ratio_vs_control = baseline_cost_usd / cost_usd`

Interpretation:
- `quality_score` answers "who solved the task best?"
- `global_score` answers "who beat the control baseline most convincingly once quality, time, tokens, and cost are all considered?"

Do not replace quality with global score. Report both.

## Multi-Run Protocol

**N ≥ 3 is required for any reported comparison.** While the pipeline is being debugged, `RunRequest.runs` defaults to `1` to keep iteration cheap — that default is a developer convenience, not the protocol. **Explicitly pass `runs: 3` (or higher) for any comparison you plan to publish.** Each repetition gets its own run_dir and stores rows with a shared `batch_id` so the scorecard can aggregate to median + IQR.

Key protocol rules:

1. **No single-run comparisons.** Single-run quality differences routinely fall inside single-run variance. N=1 results cannot be published as evidence that one condition beats another.
2. **Pre-register the outcome.** Set `primaryMetric` and `minimumMeaningfulDelta` at launch. Other metrics can be inspected but are marked exploratory.
3. **Check the discrimination label.** `scenario_discrimination.label == "low-discrimination"` means the scenario didn't separate conditions — flag results as weak evidence regardless of who "wins".
4. **Check judge reliability.** `judge_reliable == false` on any row invalidates its judge score. Use `quality_score` alone for that condition.
5. **Check scorer agreement.** `scorer_agreement_divergent == true` (gap > 10) flags rows where keyword and judge disagree — worth manual review before drawing conclusions.
6. **Calibrate the judge periodically.** `POST /api/judge/calibration-check` scores hand-anchored items and reports drift. If `passes == false`, recent judge scores are suspect until investigated.

See [`docs/guides/eval-protocol.md`](../../docs/guides/eval-protocol.md) "Multi-Run Protocol" for the full specification, thresholds, and rationale.

## Adding a New Assessment Scenario

Five files need entries. Follow existing patterns (e.g., `dead-code` or `bug-fix-1`):

### 1. Task text — `packages/aethyme-eval-ui/server/main.py`
Add to the `EVAL_TASKS` dict (~line 567). This is the prompt the agent receives.

### 2. Reference + schema — `src/eval/schemas.py`
Create three functions:
- `mytype_output_schema()` — JSON schema for structured output
- `mytype_scoring_rubric()` — weights dict (sum to 100) + notes
- `mytype_reference()` — ground truth data (files, keywords, expected answers)

Plus a `MYTYPE_PATH_KEYS` frozenset for path normalization.

### 3. Scoring — `src/eval/scoring.py`
Create `score_mytype(candidate, reference, *, cost_usd, repo_path)` returning dict with `scores`, `weighted_score`, `max_score`.

### 4. Orchestrator — `src/eval/orchestrator.py`
Add to `_EVAL_TYPE_DEFAULTS` dict and the `elif` chain in `_build_prepare_phase()` / `build-inputs` handling.

### 5. CLI — `src/cli.py`
Add the type to the `click.Choice` list in the `--eval-type` option (~line 771).

**Ground truth**: Generate by running commands against the actual repo. Prefer
current Aethyme facts/analyzer commands for candidate collection, then manually
review the resulting JSON against source when setting the benchmark reference.
Example for dead-code:
```bash
cd packages/aethyme
.venv/bin/python -m src.cli facts public-functions --repo /path/to/repo --scope includes/Watchlist --include-methods --json-output
.venv/bin/python -m src.cli analyze dead-code --repo /path/to/repo --scope includes/Watchlist --boundary outside-directory --include-methods --format eval-json --show-observability
.venv/bin/python -m src.cli facts function-usage --repo /path/to/repo --target "<function>" --boundary includes/Watchlist --json-output
```

For non-Python repositories or analyzer ambiguity, use language-specific grep or
AST tools as a second pass. Do not expose eval baselines or prior reports to
agent conditions.

## Known Pitfalls

### Git history leaks
`git log --all` shows commits on remote tracking branches. An agent can find the exact fix commit and skip analysis. **Always** remove the remote and prune after cloning. The `setup-playground.sh` script handles this.

### Control contamination
Chau7 creates `.chau7/snippets/` when opening a tab in any directory. Delete from Control before each run. `verify-playground.sh` catches this.

### Output capture
The agent's final text response is hard to capture programmatically. The session JSONL doesn't include it. The terminal scrollback is too small. PTY log has TUI spinner noise. **Telemetry data** (tokens, cost, tools, duration) is reliable. **Quality scores** are approximate until Chau7 provides a `tab_last_response` API.

### Score inflation from prompt
The task-conditioned prompt can contain navigation context with function/file names. If reference keywords appear in the prompt, the scorer matches them against prompt text, not analysis. The `_score_output()` function strips prompt words before matching — always pass the prompt.

### Stale deployed skills
The Aethyme condition is only meaningful if `.codex/skills/aethyme/SKILL.md`
advertises current commands. `verify-playground.sh` must pass the skill freshness
checks before a run. Treat old `$ENGINE unused --repo ...` guidance as stale;
the current dead-code path starts with `python -m src.cli analyze dead-code
--boundary outside-directory --format eval-json --show-observability`, with
`facts public-functions` / `facts function-usage` reserved for follow-up
verification.

### CTO overhead on large repos
On 12K+ file repos like MediaWiki, CTO can increase cost by injecting the full file tree into every turn. Navigation context from Aethyme tools is more cost-effective than the CTO file tree for large repos.

## Architecture

```
CLI (src/cli.py)
  |__ orchestrator.py: generate_run_plan() -> 8-phase JSON plan
       |__ Phase 1: prepare (check repository readiness contract)
       |__ Phase 2: build-inputs (generate prompts, schemas, reference)
       |__ Phase 3: launch (create Chau7 tabs, start backend, send prompts)
       |__ Phase 4: monitor (poll tab_status until all complete)
       |__ Phase 5: collect (read session JSONL + tab output for tokens/cost/output)
       |__ Phase 6: score (quality + usage metrics + recalculated eval score)
       |__ Phase 7: report (generate markdown + complete-result bundle)
       |__ Phase 8: cleanup (close tabs)

Server (packages/aethyme-eval-ui/server/main.py)
  |__ POST /api/repositories/setup -> rebuild playground pair
  |__ POST /api/repositories/prepare -> persist readiness snapshot
  |__ POST /api/run -> generates plan, executes in background thread
  |__ GET /api/run/status -> poll for completion
  |__ GET /api/results -> query SQLite

Chau7 MCP (server/mcp_client.py)
  |__ tab_create, tab_exec, tab_send_input, tab_submit_prompt
  |__ tab_status, tab_output, tab_close
  |__ Unix socket at ~/.chau7/mcp.sock

Storage
  |__ SQLite: packages/aethyme-eval-ui/server/evals.db
  |__ Run artifacts: packages/aethyme/eval-runs/<timestamp>-<target>-<type>/
  |__ UI: http://localhost:5173 (React + Vite)
```

## Key File Locations

| What | Where |
|---|---|
| Full protocol | `docs/guides/eval-protocol.md` |
| Tooling roadmap | `docs/guides/eval-tooling-roadmap.md` |
| Playground setup guide | `docs/guides/playground-setup.md` |
| Target registry | `src/eval/targets.py` |
| Orchestrator | `src/eval/orchestrator.py` |
| Schemas + references | `src/eval/schemas.py` |
| Scoring functions | `src/eval/scoring.py` |
| Server + task text | `packages/aethyme-eval-ui/server/main.py` |
| MCP client | `packages/aethyme-eval-ui/server/mcp_client.py` |
| Setup script | `scripts/eval/setup-playground.sh` |
| Verify script | `scripts/eval/verify-playground.sh` |
| Run script | `scripts/eval/run-eval.sh` |
| Navigation skill | `skills/aethyme/SKILL.md` |
| Engine CLI | `rust/crates/aethyme-engine/src/bin/aethyme-engine-cli.rs` |
