# Dead-Code Baseline

Last Updated: 2026-05-08

The `dead-code` eval currently supports reviewed target-specific baselines:

- MediaWiki: `includes/Watchlist/`

Aethyme itself is intentionally not a supported benchmark target. It contains
evaluation references and historical reports that can contaminate self-runs.

The baseline is intentionally split into two views:

1. `literal_external_only`
This is the benchmark view. A method is included when it has zero non-test,
non-vendor call sites outside `includes/Watchlist/`.

2. `engineering_review`
This is the maintainability view. It distinguishes likely real dead code from:
- internal-only public wrappers
- interface or contract surface
- deprecated hook interfaces

Why this split exists:
- the eval prompt asks for methods “never called from outside that directory”
- that is not the same thing as “safe to remove dead code”

Source of truth:
- `mediawiki_dead_code_watchlist.json` (removed 2026-07-13 with the eval harness; recover at git ref `16cfa5e:packages/aethyme/src/eval/baselines/`)

Preferred analyzer path for collecting candidates:
```bash
# `aethyme explore` is the canonical entry point; routes natively via Rust.
# Requires `.aethyme/graph_store.redb` from `aethyme-engine-cli index --repo`.
# `python -m src.cli intents` is still Python (intent catalog discovery).
cd packages/aethyme
.venv/bin/python -m src.cli intents --format compact-json
aethyme explore --repo /path/to/repo --intent usage_boundary_query --request "Find public symbols in <scope> with no callers outside <scope>" --scope "<scope>" --search-root src --search-root tests --format answer-json --show-observability
```

Fallback low-level path:
```bash
.venv/bin/python -m src.cli facts public-functions --repo /path/to/repo --scope <scope> --json-output
.venv/bin/python -m src.cli analyze dead-code --repo /path/to/repo --scope <scope> --boundary outside-directory --format eval-json --show-observability
.venv/bin/python -m src.cli facts function-usage --repo /path/to/repo --target <function> --boundary <scope> --json-output
```

Use `--include-methods` when the target language expresses the public API as
methods. Use `--roots <dir1>,<dir2>` when the relevant search roots are known.
Manual language-specific checks are still required before editing benchmark
baselines.

The high-level `explore` answer is `answer[]`, with rejected candidates in
`excluded[]` and the legacy eval shape at `output_adapters.dead_code_eval_json`.
The usage-boundary analyzer is intentionally hybrid V2: redb discovers public
symbols and candidate files, while source/docs/config text is scanned at query
time for evidence. Do not treat `.aethyme/graph_store.redb` as the authority
for evidence strings unless a future fully redb-native evidence table also
defines freshness/invalidation rules.

The direct analyzer answer is `unused_functions[]`. Each item contains
`function_name`, `defined_in`, `status`, `external_callers`, `internal_callers`,
`evidence`, `confidence`, and `reason`. It also includes
`excluded_functions[]`. With `--show-observability`, the payload also records
command name, repository path, index freshness, graph/fact counts, confidence
summary, output size, and degraded reasons.

Current practical interpretation:
- score benchmark answers against `literal_external_only`
- use `engineering_review` when assessing whether an answer shows sound software judgment

Recommended reporting for this eval:
- `quality_score` against `literal_external_only`
- qualitative review against `engineering_review`
- `tool_call_count`, `top_tools`, `total_tokens`, and `duration_seconds`
- `global_score` / `recalculated_eval_score` for value relative to the `control-cto-off` baseline

That keeps benchmark fit and engineering judgment separate while still exposing runtime efficiency.
