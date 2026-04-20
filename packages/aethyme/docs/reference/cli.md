# CLI Reference

Last Updated: 2026-04-20

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
- `aethyme analyze dead-code --repo /path/to/repo --scope src/pkg --boundary outside-directory --format eval-json --show-observability`
- `aethyme analyze dead-code --repo /path/to/repo --scope src/pkg --format full-json`
- Add `--include-methods` when class/object methods are in scope.
- Add `--roots src,tests` to narrow caller evidence collection on large repositories.
- `--format eval-json` emits `unused_functions[]` items with `function_name`,
  `defined_in`, `status`, `external_callers`, `internal_callers`, `evidence`,
  `confidence`, and `reason`.
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
