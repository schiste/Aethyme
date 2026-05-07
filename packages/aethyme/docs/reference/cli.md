# CLI Reference

Last Updated: 2026-04-22

## Global Options

- `--tenant-id`
- `--json`
- `--verbose`

## Core Commands

### Indexing
- `aethyme index PATH --name NAME --languages python,typescript --use-fallback`
- `aethyme stats`

### Local Repo Intake
- `aethyme repo ingest /path/to/repo`
- `aethyme repo inspect /path/to/repo --json-output`
- `aethyme repo clear-cache /path/to/repo`
- `aethyme repo deploy-skills /path/to/repo --force`

`repo deploy-skills` deploys only target-safe runtime navigation skills by
default. It must not deploy internal eval workflow skills into benchmark
playground repositories.

### Local Discoverability
- `aethyme query symbol /path/to/repo main`
- `aethyme query deps /path/to/repo src/main.py`
- `aethyme query impact /path/to/repo src/main.py`

### High-Level Intent Surface

> **Note (2026-05-07):** `aethyme explore` is now served by the native Rust binary (`aethyme-engine-cli explore`). The Python implementation at `python -m src.cli explore` is deprecated as a fallback only and prints a stderr warning when invoked directly. All examples below route through native; no behavior change for callers.

- `aethyme explore --repo /path/to/repo --request "Find public functions with no outside callers" --format answer-json`
- `aethyme intents --request "Find public functions with no outside callers" --format compact-json`
- `aethyme explore --repo /path/to/repo --intent behavior_localization_query --request "Find the files responsible for this behavior" --format answer-json --show-observability`
- `aethyme explore --repo /path/to/repo --intent usage_boundary_query --request "Find public functions with no outside callers" --params '{"scope":"src/pkg","symbol_kind":"public_top_level_function","boundary":{"type":"outside_directory","path":"src/pkg"},"search_roots":["src","tests"],"budget_ms":10000,"max_evidence_per_symbol":5}' --format answer-json --show-observability`

`intents` returns the finite mode/intent catalog. Current modes are `explore`,
`act`, and `learn`; `explore` implements the default
`task_localization_query` intent plus specialized intents such as
`behavior_localization_query` and `usage_boundary_query`.

`explore --request ...` without `--intent` runs the default
`task_localization_query` intent. It composes one bounded `task-localize` graph
call, bounded deterministic symbol search, source-text ranking, source
call-site expansion, filename fallback, and compact `task-expand` output into:
- `answer[]`: ranked graph/symbol/source-backed candidate files, symbols, areas, call-site files, and next-step targets
- `navigation_hints[]`: low-confidence investigation hints, including filename-only fallback candidates and suggested searches
- `excluded[]`: out-of-scope areas or candidates
- `ambiguous[]`: low-confidence or missing-anchor cases
- `output_adapters.task_localization_json`: compact candidate file/symbol lists and expansion commands
- `confidence`: answer-only, excluded-only, and analyzed confidence summaries
- `safe_to_use_as_answer` / `trust_policy`: whether `answer[]` is authoritative enough to guide an answer, or only safe as navigation
- `observability`: command, repo path, index freshness, internal analyzers, graph/fact counts, output size, confidence summary, evidence level, trust policy, and degraded/failure reasons

Default `task_localization_query` responsiveness behavior:
- `graph_query_timeout_ms`: default `1000`
- `symbol_query_timeout_ms`: default `1000`
- `skip_symbols_after_graph_timeout`: default `false`; if graph localization times out, Aethyme still attempts bounded symbol and source-text recovery unless the caller opts out.
- The command returns degraded partial output with `degraded_reasons` instead of blocking indefinitely.
- If source-text/call-site evidence is strong enough, degraded output may still set `safe_to_use_as_answer=true`; inspect `observability.degradation_guidance`, `answer[].evidence.line_refs`, and `evidence.callsite_expansions` before trusting it.
- Filename-only evidence is low confidence and cannot set `safe_to_use_as_answer=true`.
- For very large repos where first response speed matters more than graph coverage, callers can lower `graph_query_timeout_ms` to `500`.

`explore --intent behavior_localization_query` is the preferred generic path for
debugging, feature localization, and "which files implement this behavior?"
questions. It uses the same answer schema as the default Explore path but gives
source-text ranking and call-site expansion a larger budget. It is still
repository-agnostic and does not inject benchmark-specific candidates.

Use `intents` or explicit `--intent` when the caller/LLM can select a more
precise deterministic analyzer from the catalog. Aethyme still does not perform
rich free-form routing; the default path is the general repository localization
intent, not a hidden task-specific guess.

`explore --intent usage_boundary_query` is the preferred task-ready entry point
for public-symbol usage boundary questions, including dead-code checks. The LLM
chooses the intent and supplies structured params; Aethyme performs deterministic
analysis and returns:
- `answer[]`: primary task result
- `excluded[]`: candidates rejected by evidence
- `output_adapters.dead_code_eval_json`: compatibility shape for the dead-code eval
- `confidence`: answer-only, excluded-only, and analyzed confidence summaries
- `observability`: command, repo path, index freshness, graph/fact counts, output size, confidence summary, and degraded/failure reasons

The current `usage_boundary_query` implementation uses the scope-first
`analyze-usage-boundary` engine path for PHP public methods/functions. That path
does not build the full repository graph. For non-PHP scopes, or when
`degraded_reasons` includes language/support gaps, use the graph-backed
`analyze dead-code` / `facts function-usage` commands as the fallback.

Optional params:
- `budget_ms`: time budget for the scope-first analyzer, default `10000`
- `max_evidence_per_symbol`: maximum evidence strings retained per symbol, default `5`

### Graph Navigation
- `aethyme graph node /path/to/repo <target> --json-output`
- `aethyme graph children /path/to/repo <target> --json-output`
- `aethyme graph parents /path/to/repo <target> --json-output`
- `aethyme graph callers /path/to/repo <target> --json-output`
- `aethyme graph callees /path/to/repo <target> --json-output`
- `aethyme graph docs /path/to/repo <target> --json-output`
- `aethyme graph configs /path/to/repo <target> --json-output`
- `aethyme graph overview /path/to/repo --json-output`

### Derived Facts
- `aethyme facts public-functions --repo /path/to/repo --scope src/pkg --json-output`
- `aethyme facts function-usage --repo /path/to/repo --target my_function --boundary src/pkg --json-output`
- Add `--roots src,tests` to `function-usage` when the repository is large and the relevant search roots are known.

### Deterministic Analyzers
- Prefer `aethyme explore --intent usage_boundary_query` for task-ready boundary usage answers.
- `aethyme analyze dead-code --repo /path/to/repo --scope src/pkg --boundary outside-directory --format eval-json --show-observability`
- `aethyme analyze dead-code --repo /path/to/repo --scope src/pkg --format full-json`
- Add `--include-methods` when class/object methods are in scope.
- Add `--roots src,tests` to narrow caller evidence collection on large repositories.
- `--format eval-json` emits `unused_functions[]` items with `function_name`,
  `defined_in`, `status`, `external_callers`, `internal_callers`, `evidence`,
  `confidence`, and `reason`, plus `excluded_functions[]`.
- `--show-observability` adds command name, repository path, index freshness,
  graph/fact counts, output size, confidence summary, and degraded reasons.
- `--json-output` remains supported as a compatibility alias for
  `--format full-json`.

### Local Task Packs
- `aethyme task pack --repo /path/to/repo --task "Explain this repo" --json-output`
- `aethyme task explain --repo /path/to/repo`
- `aethyme task anchors --repo /path/to/repo --task "..." --json-output`
- `aethyme task scope --repo /path/to/repo --task "..." --json-output`
- `aethyme task next --repo /path/to/repo --task "..." --json-output`
- `aethyme task expand --repo /path/to/repo --node <target> --json-output`
- `aethyme task context --repo /path/to/repo --task "..." --json-output`

### Local Evaluation
- `aethyme eval explain-repo --repo /path/to/repo --json-output`
- `aethyme eval explain-repo --repo /path/to/repo --baseline-cmd "<cmd>" --aethyme-cmd "<cmd>"`
- `aethyme eval navigation-ctf --repo /path/to/repo --json-output`
- Example Codex wrapper command: `packages/aethyme/.venv/bin/python packages/aethyme/scripts/eval/run_codex_eval.py`

Current behavior:
- with no commands, this builds the control artifacts and comparison report only
- with `--baseline-cmd` and `--aethyme-cmd`, it executes real runs through the evaluation runner contract
- external runners receive the prompt, navigation context, output schema, and Aethyme tool paths through `AETHYME_EVAL_*` env vars
- the bundled Codex wrapper deletes its temporary prompt/event/schema files automatically; if you orchestrate runs through Chau7 MCP, close the tab after the report path is captured and the tab returns to idle
- every run writes a markdown report under `packages/aethyme/docs/reports/evals/`
- JSON output includes the generated `report_path`
- evaluation JSON now includes `output_schema`, `scoring_rubric`, and `reference_output`

## Local Runtime Notes

- the Python layer builds and executes the Rust engine binary directly
- local artifacts are cached by repository snapshot under `AETHYME_CACHE_DIR` or `/tmp/aethyme-cache`
- Git repositories use commit plus dirty-state metadata for cache keys
- `repo clear-cache` clears the current snapshot cache
- the Aethyme-assisted evaluation prompt uses a compact rendered pack rather than the full raw JSON payload

### Graph Queries
- `aethyme search TERM --limit 20 --type hybrid`
- `aethyme ego SYMBOL --depth 2`
- `aethyme impact SYMBOL --max-depth 10`

### Scorecard
- `aethyme ai-ready --repo PATH --format md`

### Autofix
- `aethyme autofix PATH --dry-run`
- `aethyme autofix PATH --apply`
- `aethyme autofix PATH --pr`

## Rule

CLI commands should keep using the same indexing and graph contracts as the API.
