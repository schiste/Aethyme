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

4. **No answer leakage.** Eval prompts and launch plans must not expose
   reference answers, scoring rubrics, prior eval reports, or benchmark
   implementation files as evidence. The leverage condition uses a narrow
   `.codex/skills/aethyme/aethyme-explore` wrapper in the Aethyme playground so
   the prompt can call Explore without disclosing the Aethyme source root.

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

Creates `<Name> - Control` (vanilla) and `<Name> - Aethyme` (with generated
root guidance, per-product skills, fragment graph, and redb graph store).
Sanitizes git history so agents can't find fix commits via `git log --all`.
Installs local `.git/info/exclude` rules so `.aethyme/`, `.chau7/`, `.claude/`,
`.codex/`, `AGENTS.md`, and `CLAUDE.md` do not appear as ordinary repo
evidence.

### Verify a playground

```bash
./scripts/eval/verify-playground.sh --target mediawiki
```

Readiness check: no contamination on Control; current native-Explore guidance,
skill, graph fragments, redb store, and generated-artifact excludes on Aethyme;
same commit; engine exists. **Run this before every assessment.**

### Run an assessment

The server-based end-to-end runner (`run-eval.sh`, the eval-ui server,
and the `src/eval/` orchestrator) was removed with the evaluation stack
(2026-07-13; stragglers cleaned 2026-07-17). Assessments are currently
run manually against a verified playground; regression tracking lives in
the `aethyme-eval` package (`summarize-history`, `compare`,
`baseline-info`, `playground-command`).

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
| `dead-code` | mediawiki | Target-specific dead-code scan (MediaWiki Watchlist) |
| `migration` | mediawiki | List all files referencing `WatchedItemStore` for rename |

## 5-Condition Design

Every assessment runs the same task across 5 conditions to isolate what helps:

| Condition | Repo | CTO | Aethyme Skill | Task-Specific Pack | What it measures |
|---|---|---|---|---|---|
| `control-cto-off` | Control | forceOff | No | No | Baseline: raw agent, no help |
| `control-cto-on` | Control | default | No | No | Effect of CTO file tree context |
| `explore` | Aethyme | default | Yes | No | Effect of having Aethyme available with the basic prompt only |
| `leverage` | Aethyme | default + generic Aethyme usage card | Yes | No | Effect of a generic hint that exposes the Explore contract |
| `task-conditioned` | Aethyme | default + full context pack | Yes | Yes (engine-generated) | Effect of task-specific Aethyme context-pack mode |

**CTO** = Context Tree Optimization — Claude Code's file tree injection into context.
**Task-Specific Pack** = Engine-generated prompt or artifact with repo structure, function listings, subsystem detail, or task navigation context.

The task-conditioned prompt is built by: `aethyme-engine-cli prompt --repo <path> --task <task> --focus overview [--subsystem <dir>]`

Current product priority:

1. strengthen `explore`
2. make `leverage` a light generic uplift over `explore`
3. revisit `task-conditioned` only after the generic tooling is stronger

Do not use assessment results as a reason to grow the task-conditioned prompt first.

## Leakage and Observability

Every run should record whether Aethyme was actually used:

- `aethyme_used`
- `aethyme_command_count`
- `aethyme_commands`
- `manual_shell_after_aethyme_count`
- `manual_search_after_aethyme_count`

Every run should also stamp the cold-probe leakage fields when the probe is
available:

- `leakage_score_cold`
- `leakage_is_clean`
- `leakage_raw_judge`
- `leakage_probe_version`
- `leakage_error`

Single-run and batch launches both use the same leakage computation path. If
these fields are absent, treat the run as incomplete for analysis rather than
assuming it is clean.

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

The schema/scoring/orchestrator files this section used to reference
(`src/eval/schemas.py`, `src/eval/scoring.py`, `src/eval/orchestrator.py`,
and the eval-ui server) were removed with the evaluation stack
(2026-07-13). New scenarios currently mean: a task prompt, a manually
curated ground-truth reference, and manual scoring against the rubric
patterns preserved in `docs/reports/evals/`.

**Ground truth**: Generate by running commands against the actual repo. Prefer
current Aethyme facts/analyzer commands for candidate collection, then manually
review the resulting JSON against source when setting the benchmark reference.
Example for dead-code:
```bash
cd packages/aethyme
rust/target/release/aethyme explore --repo /path/to/repo --request "Find public methods in includes/Watchlist with no outside callers" --format answer-json
rust/target/release/aethyme explore --repo /path/to/repo --intent usage_boundary_query --request "Find public methods in includes/Watchlist with no outside callers" --params '{"scope":"includes/Watchlist","symbol_kind":"public_method","boundary":{"type":"outside_directory","path":"includes/Watchlist"},"search_roots":[],"budget_ms":10000,"max_evidence_per_symbol":5}' --format answer-json --show-observability
.venv/bin/python -m src.cli facts public-functions --repo /path/to/repo --scope includes/Watchlist --include-methods --json-output
.venv/bin/python -m src.cli analyze dead-code --repo /path/to/repo --scope includes/Watchlist --boundary outside-directory --include-methods --format eval-json --show-observability
.venv/bin/python -m src.cli facts function-usage --repo /path/to/repo --target "<function>" --boundary includes/Watchlist --json-output
```

For `explore --request`, inspect `degraded_reasons`. The default path is
bounded for responsiveness: if graph localization times out, Aethyme may skip
symbol search. Filename-only fallback must remain `navigation_hints[]`, not
authoritative `answer[]`. The default detail is compact and should include
`verification_steps[]`; use `--detail standard` or `--detail full` only when the
analysis needs more evidence payload. Treat `safe_to_use_as_answer=false` and
`trust_policy.trust_policy=needs_verification` as safe degradation: the agent
should verify the ranked candidates before trusting them.

For non-Python repositories or analyzer ambiguity, use language-specific grep or
AST tools as a second pass. Do not expose eval baselines or prior reports to
agent conditions.

## Known Pitfalls

### Git history leaks
`git log --all` shows commits on remote tracking branches. An agent can find the exact fix commit and skip analysis. **Always** remove the remote and prune after cloning. The `setup-playground.sh` script handles this.

### Control contamination
Chau7 creates `.chau7/snippets/` when opening a tab in any directory. Delete from Control before each run. `verify-playground.sh` catches this.

### Aethyme artifact contamination
The Aethyme clone needs `.aethyme/`, `.codex/`, `.claude/`, `AGENTS.md`, and
`CLAUDE.md` so enhanced agents can run, but those files are not benchmark
source. `setup-playground.sh` writes local `.git/info/exclude` rules so Git and
ripgrep ignore them during ordinary discovery; `verify-playground.sh` fails if
those generated artifacts are visible in `git status` or not ignored.

### Output capture
The agent's final text response is hard to capture programmatically. The session JSONL doesn't include it. The terminal scrollback is too small. PTY log has TUI spinner noise. **Telemetry data** (tokens, cost, tools, duration) is reliable. **Quality scores** are approximate until Chau7 provides a `tab_last_response` API.

### Score inflation from prompt
The task-conditioned prompt can contain navigation context with function/file names. If reference keywords appear in the prompt, the scorer matches them against prompt text, not analysis. The `_score_output()` function strips prompt words before matching — always pass the prompt.

### Stale deployed skills
The Aethyme condition is only meaningful if `.codex/skills/aethyme/SKILL.md`
advertises current commands. `verify-playground.sh` must pass the skill freshness
checks before a run. Treat old `$ENGINE unused --repo ...` guidance as stale.
Also treat executable `python -m src.cli explore ...` guidance as stale:
Explore is native now and should start with
`$AETHYME_ROOT/rust/target/release/aethyme explore --repo ... --request ...
--format answer-json`. The Python CLI remains valid for non-Explore surfaces
such as `analyze dead-code`, `facts function-usage`, and `intents`.

### Aethyme availability vs Aethyme usage
An Aethyme-enabled repository does not prove that the agent used Aethyme. Every
run report should inspect the `Aethyme Usage` section:
- `explore` should usually show whether ambient availability alone was enough.
- `leverage` should show whether the generic usage card caused real
  native `aethyme explore` / Python `src.cli intents` calls.
- `task-conditioned` should be interpreted as context-pack value, not as proof
  that high-level Explore commands were used.

### CTO overhead on large repos
On 12K+ file repos like MediaWiki, CTO can increase cost by injecting the full file tree into every turn. Navigation context from Aethyme tools is more cost-effective than the CTO file tree for large repos.

## Architecture

The 8-phase orchestrator, eval-ui server, and Chau7 MCP launch pipeline
were removed with the evaluation stack (2026-07-13). What remains:

```
Playground lifecycle
  |__ scripts/eval/setup-playground.sh   -> build a Control/Aethyme pair
  |__ scripts/eval/verify-playground.sh  -> 15-point readiness check

Regression sentinel (packages/aethyme-eval)
  |__ aethyme-eval summarize-history -> build a baseline from run history
  |__ aethyme-eval compare           -> compare run results to a baseline
  |__ Historical run artifacts: packages/aethyme/eval-runs/<timestamp>-...
```

## Key File Locations

| What | Where |
|---|---|
| Full protocol | `docs/guides/eval-protocol.md` |
| Tooling roadmap | `docs/guides/eval-tooling-roadmap.md` |
| Playground setup guide | `docs/guides/playground-setup.md` |
| Regression sentinel | `packages/aethyme-eval/src/aethyme_eval/` |
| Setup script | `scripts/eval/setup-playground.sh` |
| Verify script | `scripts/eval/verify-playground.sh` |
| Navigation skill | `skills/aethyme/SKILL.md` |
| Engine CLI | `rust/crates/aethyme-engine/src/bin/aethyme-engine-cli.rs` |
