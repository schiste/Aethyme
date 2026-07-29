# CLI Reference

Last Updated: 2026-07-29

## Global Options

- `--tenant-id`
- `--json`
- `--verbose`

## Product-Surface Tiers

The public product map is
[`../../../../docs/product-surface.md`](../../../../docs/product-surface.md).
This reference includes stable front-door commands, advanced public tools, and
internal or historical commands; do not treat every listed command as equal
product surface.

### Stable Front Door

- `aethyme init`
- `aethyme certify`
- `aethyme broker status`
- `aethyme broker start`
- `aethyme broker adopt`
- `aethyme broker exec`
- `aethyme broker submit`
- `aethyme broker repair`
- `aethyme broker finish`
- `aethyme broker integration status`
- `aethyme broker quick-test`
- `aethyme broker verify-loop`
- `aethyme explore`

### Advanced Public Tools

- `aethyme broker gates ...`
- `aethyme broker events`
- `aethyme broker metrics`
- `aethyme broker doctor`
- `aethyme broker leases ...`
- `aethyme broker cleanup`
- `aethyme graph ...`
- `aethyme facts ...`
- `aethyme task ...`
- `aethyme analyze dead-code`
- `aethyme enhance deploy`
- `aethyme enhance verify`
- `aethyme repo experience-*`

### Internal Or Historical

Local eval harnesses, benchmark/report generators, storage implementation
details, SaaS-era material, and compatibility-only commands may be documented
for maintainers but should not lead user onboarding.

## Broker Commands

The broker is the stable product front door for multi-agent coordination:

- `aethyme init`
- `aethyme certify`
- `aethyme broker status [--json]`
- `aethyme broker start --task "..." [--json]`
- `aethyme broker adopt [<path>] --task "..." [--reuse] [--json]`
- `aethyme broker exec --session <id> -- <command> [--json]`
- `aethyme broker submit --session <id> [--json]`
- `aethyme broker repair --session <id> [--json]`
- `aethyme broker finish --session <id> [--json]`
- `aethyme broker integration status [--json]`
- `aethyme broker quick-test [--with-gate] [--json]`
- `aethyme broker verify-loop [--json]`

`quick-test` is the disposable install smoke. `verify-loop` is the stronger
operator E2E: it reports the integration commit tested and flags movement during
the run, so callers know whether the result proves the current integration tip.

Frozen broker JSON contracts are limited to the commands listed in
[`../../../../docs/json-contracts.md`](../../../../docs/json-contracts.md).
Other `--json` outputs are useful but provisional.

## Core Commands

### Indexing
- `aethyme index PATH --name NAME --languages python,typescript --use-fallback`
- `aethyme stats`

### Local Repo Intake
- `aethyme repo ingest /path/to/repo`
- `aethyme repo inspect /path/to/repo --json-output`
- `aethyme repo clear-cache /path/to/repo`
- `aethyme repo warm /path/to/repo`
- `aethyme repo compile-skills /path/to/repo`
- `aethyme repo init-onboarding-overrides /path/to/repo`
- `aethyme repo validate-onboarding-overrides /path/to/repo`
- `aethyme repo init-agents-overrides /path/to/repo`
- `aethyme repo validate-agents-overrides /path/to/repo`
- `aethyme repo experience-telemetry /path/to/repo`
- `aethyme repo experience-telemetry /path/to/repo --check`
- `aethyme repo experience-status /path/to/repo`
- `aethyme repo commit-message-template --type fix --scope watchlist`
- `aethyme repo lint-commit-message .git/COMMIT_EDITMSG`
- `aethyme repo lint-commit-message --message "fix(scope): summary\n\nProblem:\n..."`
- `aethyme repo deploy-skills /path/to/repo --force`
- `aethyme repo engine-info --json-output`
- `aethyme repo engine-info --check`

`repo compile-skills` generates repo-specific skills, currently
`repo-onboarding`, into `.aethyme/generated/` plus per-product skill paths.
It also records summon policy and generation telemetry inside the generated
artifact. Maintainers can override selected sections with
`.aethyme/overrides/onboarding.json`.
It also generates a deterministic `repo-act` starter artifact and skill for
debugging and validation planning.

Example override:

```json
{
  "commands": [
    {
      "kind": "test",
      "command": "./scripts/test-fast.sh",
      "source": "manual-override",
      "confidence": "high"
    }
  ],
  "notes": [
    "Use sandbox credentials from 1Password.",
    "Do not edit src/gen directly; run pnpm codegen."
  ]
}
```

`notes[]` are rendered into the visible `repo-onboarding` skill under
`Maintainer Notes`; humans contribute by editing the override file and
regenerating onboarding, not by editing generated skill files directly.

`repo init-onboarding-overrides` writes a starter override file.
`repo validate-onboarding-overrides` checks that the override file is valid JSON
and that key fields use the expected shapes.

`repo init-agents-overrides` writes a starter `.aethyme/overrides/agents.json`
file. Use it for repo-specific root instruction customization such as:
- repo summary
- hard constraints
- validation rules
- commit hygiene notes
- summon policy notes
- migrated maintainer markdown

`repo validate-agents-overrides` checks that the agents override file is valid
JSON and that those fields use the expected shapes.

`repo deploy-skills` is now a compatibility path that deploys only the static
runtime navigation skill. For real repositories, prefer
`aethyme enhance deploy --repo /path/to/repo`.

`repo commit-message-template` prints the typed commit message skeleton Aethyme
expects for durable commit hygiene. `repo lint-commit-message` validates a real
message against that contract and emits structured JSON suitable for future
memory extraction.

Commit hygiene contract:
- subject: `type(scope): short summary` or `type: short summary`
- allowed types: `fix`, `feat`, `refactor`, `perf`, `test`, `docs`, `build`, `chore`, `revert`
- substantive types `fix`, `feat`, `refactor`, and `perf` require structured
  body sections: `Problem`, `Decision`, `Rationale`, `Validation`
- optional sections: `Alternatives considered`, `Risks`, `Follow-up`, `Memory`

Example:

```text
fix(watchlist): mark only viewed revision as seen

Problem:
Viewing a diff marked every revision as seen.

Decision:
Use the viewed revision id for seen-marking.

Rationale:
Seen state is revision-scoped.

Validation:
- Added regression coverage.
- Ran watchlist tests.

Memory:
Watchlist seen-marking must remain revision-scoped.
```

### Local Discoverability
- `aethyme enhance deploy --repo /path/to/repo`
- `aethyme enhance verify --repo /path/to/repo`
- `aethyme query symbol /path/to/repo main`
- `aethyme query deps /path/to/repo src/main.py`
- `aethyme query impact /path/to/repo src/main.py`

`enhance deploy` is the primary repo-facing discoverability path. It writes:
- fully generated `AGENTS.md`
- `CLAUDE.md`
- `.claude/skills/aethyme/SKILL.md`
- `.codex/skills/aethyme/SKILL.md`
- `.claude/skills/aethyme/references/*.md`
- `.codex/skills/aethyme/references/*.md`
- `.claude/hooks/aethyme-load-context.sh`
- `.aethyme/generated/onboarding.json`
- `.aethyme/generated/act-starter.json`
- `.claude/skills/repo-onboarding/SKILL.md`
- `.codex/skills/repo-onboarding/SKILL.md`
- `.claude/skills/repo-act/SKILL.md`
- `.codex/skills/repo-act/SKILL.md`

`AGENTS.md` and `CLAUDE.md` are now generated artifacts owned by Aethyme.
Customize them through `.aethyme/overrides/agents.json`, not by editing the
root files directly. The generated root instructions include:
- native Explore guidance
- repo-onboarding and repo-act routing
- experience status path
- primary fast test when detected
- primary app entrypoint when detected
- commit hygiene policy and commands

Legacy block-managed `AGENTS.md` files are migration-only now. On deploy,
Aethyme extracts maintainer-authored legacy content into
`.aethyme/overrides/agents.json` and then rewrites the root file as a fully
generated artifact.

`onboarding.json` is the canonical artifact. It includes:
- repo identity
- inferred commands, areas, entrypoints, caution zones
- summon rules for when the onboarding skill should be loaded
- freshness metadata
- generation telemetry and override status

`act-starter.json` is the deterministic execution companion artifact. It includes:
- debugging and validation starter checklists
- likely fast test/lint/build commands
- likely entrypoints and caution zones

`enhance verify` also prints a compact summary: recommended skill/mode,
onboarding counts, override presence, override freshness, and Act starter
readiness. Direct edits to `AGENTS.md` or `CLAUDE.md` are now verification
failures; use `.aethyme/overrides/agents.json` instead.

Stable experience-layer telemetry is written to:
- `.aethyme/generated/experience-telemetry.jsonl`

Generated experience status artifacts are written to:
- `.aethyme/generated/experience-status.json`
- `.aethyme/generated/experience-status.md`

Inspect it with:
- `aethyme repo experience-telemetry /path/to/repo`
- `aethyme repo experience-telemetry /path/to/repo --json-output`
- `aethyme repo experience-telemetry /path/to/repo --check`
- `aethyme repo experience-status /path/to/repo`
- `aethyme repo experience-status /path/to/repo --json-output`

The report now derives simple experience-layer KPIs, for example:
- enhancement installed but no wrapper usage recorded yet
- invalid onboarding override present
- onboarding exists but no fast test command detected
- onboarding overrides changed after generated artifacts and need regeneration

`--check` exits nonzero when attention signals are present, so it can be used in
CI or local verification gates without parsing the full report.

`repo experience-status` writes a compact operator artifact with:
- enhancement installed/verified state
- onboarding/Act presence
- override freshness
- KPI signals and suggestions
- recommended next command

It also emits concrete suggestions tied to those signals, for example:
- load onboarding and use the Aethyme wrapper on the next broad task
- fix or reinitialize an invalid override
- add a fast test command through onboarding overrides

This ledger records deterministic lifecycle events only, such as:
- `enhance.deploy`
- `enhance.verify`
- `repo.compile-skills`
- `repo.init-onboarding-overrides`
- `repo.validate-onboarding-overrides`

Wrapper-level signals are also recorded when Aethyme-provided entry points are
actually invoked:
- `wrapper.invocation` with `wrapper_name=aethyme-explore`
- `wrapper.invocation` with `wrapper_name=aethyme-sessionstart-hook`

It does not yet claim actual agent adoption or downstream answer quality.

### High-Level Intent Surface

> **Note (2026-05-12):** `aethyme explore` is served by the native Rust binary. The removed Python module form for Explore now prints a targeted recovery error if invoked. All examples below route through native.

- `aethyme explore --repo /path/to/repo --request "Find public functions with no outside callers" --format answer-json`
- `aethyme intents --request "Find public functions with no outside callers" --format compact-json`
- `aethyme explore --repo /path/to/repo --intent behavior_localization_query --request "Find the files responsible for this behavior" --format answer-json --show-observability`
- `aethyme explore --repo /path/to/repo --intent behavior_localization_query --request "Find the files responsible for this behavior" --format answer-json --show-observability --detail full`
- `aethyme explore --repo /path/to/repo --intent usage_boundary_query --request "Find public functions with no outside callers" --params '{"scope":"src/pkg","symbol_kind":"public_top_level_function","boundary":{"type":"outside_directory","path":"src/pkg"},"search_roots":["src","tests"],"budget_ms":10000,"max_evidence_per_symbol":5}' --format answer-json --show-observability`

`intents` returns the finite mode/intent catalog. The public product model is
`explore / act / learn`; `explore` is the implemented primary mode today and
ships the default `task_localization_query` intent plus specialized intents
such as `behavior_localization_query` and `usage_boundary_query`. `act` and
`learn` are product-direction modes, not equivalent top-level CLI groups yet.

`explore --request ...` without `--intent` runs the default
`task_localization_query` intent. It composes one bounded `task-localize` graph
call, bounded deterministic symbol search, source-text ranking, source
call-site expansion, filename fallback, and compact `task-expand` output into:
- `answer[]`: ranked graph/symbol/source-backed candidate files, symbols, areas, call-site files, and next-step targets
- `navigation_hints[]`: low-confidence investigation hints, including filename-only fallback candidates and suggested searches
- `excluded[]`: out-of-scope areas or candidates
- `ambiguous[]`: low-confidence or missing-anchor cases
- `subsystems[]`: ranked subsystem lanes for ambiguous Surface/Flow tasks, including role, confidence, concrete `token_subsystems`, top verification targets, paths, signals, and missing-coverage warnings; broad auth/token requests use this to separate ingress/proxy, backend validation, and provider/OIDC/audit-style token systems before trusting a flat file ranking
- `output_chars_estimate` / `truncated`: command-output budget metadata for agent loops
- `output_adapters.task_localization_json`: compact candidate file/symbol lists and expansion commands, emitted only with `--detail full`
- `confidence`: answer-only, excluded-only, and analyzed confidence summaries
- `safe_to_use_as_answer` / `trust_policy`: whether `answer[]` is authoritative enough to guide an answer, or only safe as navigation
- `observability`: with `--show-observability`, compact graph-store freshness, Surface/Flow coverage, missing expected surfaces, ranking explainability, answer-safety mode, and readiness fields. Freshness alone is not enough: agents should require the graph to be fresh, complete enough for the request, and explainable before treating `answer[]` as answer-safe. Use `--detail full --show-observability` only when debugging the full observability envelope.

For task/behavior localization, Explore observability includes:
- `graph_freshness`: redb backend status, `fresh`, `stale`, fragment/store timestamps, and path-free artifact role labels (`source_of_truth=graph_fragments`, `derived_query_artifact=redb_graph_store`)
- `surface_flow_graph.coverage`: per-surface coverage for backend, edge/proxy, routes, middleware, webhooks, CLIs, jobs/queues, credential flows, and live behavior tests
- `indexed_languages` / `indexed_frameworks`: language/framework signals inferred from indexed graph fragments, not from source files alone
- `surface_flow_graph.missing_expected_surfaces`: source-present surfaces that graph fragments did not fully index
- `ranking_explainability`: `degraded_ranking_reasons`, `top_signals_used`, `top_signals_absent`, and whether subsystem ambiguity was detected
- `answer_safety`: evidence-only safety, observability-adjusted safety, navigation-only mode, trust policy, and reason
- `readiness`: booleans for `fresh_enough`, `complete_enough`, `surface_flow_complete`, `explainable`, `answer_safe_after_observability`, and `navigation_only_after_observability`

Compact agent-mode observability is the default for `--show-observability`.
It caps `answer[]`, `navigation_hints[]`, subsystem targets/signals, ranking
signals, evidence arrays, and Surface/Flow path hints so an initial Explore
call stays in the 12k-20k character budget. Indexed language/framework detail
and full path-hint coverage remain available through
`--detail full --show-observability`.

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
- `observability`: the same enterprise envelope used by the default Explore path, plus `usage_boundary_analyzer` graph/fact counts, confidence summary, and analyzer degraded reasons. Usage-boundary remains a hybrid contract: redb discovers seeds/candidate files, while source text supplies caller/docs/config evidence.

The current `usage_boundary_query` implementation uses the scope-first
`analyze-usage-boundary` engine path for PHP public methods/functions. That path
opens `.aethyme/graph_store.redb` read-only to discover public symbols and
candidate files, then scans source/docs/config text for evidence. It does not
build `RepositoryMap` or mutate the store; run `aethyme-engine-cli index --repo
<repo>` first if the redb artifact is missing. For non-PHP scopes, or when
`degraded_reasons` includes language/support gaps, use the graph-backed
`analyze dead-code` / `facts function-usage` commands as the fallback.

Phase 5 decision: usage-boundary is intentionally hybrid V2, not fully
redb-native. redb owns seed discovery; query-time source/docs/config scanning
owns evidence strings so caller lines and docs/config references reflect the
current checkout. A fully redb-native analyzer would need persisted evidence
rows plus freshness/invalidation rules before replacing this source-text pass.

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

`node`, relation, and expansion graph commands read
`.aethyme/graph_store.redb` through read-only engine APIs. `graph overview`
still uses the in-memory graph overview path until it receives its own redb
adapter.

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

`task anchors`, `task scope`, `task next`, `task expand`, `task pack`,
`task explain`, and `task context` read the redb graph store. Source text is
still read from the filesystem when context packs need snippets/content, but
candidate selection and graph navigation come from `.aethyme/graph_store.redb`.

### Local Evaluation
- `aethyme eval explain-repo --repo /path/to/repo --json-output`
- `aethyme eval explain-repo --repo /path/to/repo --control-cmd "<cmd>" --explore-cmd "<cmd>" --leverage-cmd "<cmd>"`
- `aethyme eval navigation-ctf --repo /path/to/repo --json-output`
- Example Codex wrapper command: `packages/aethyme/.venv/bin/python packages/aethyme/scripts/eval/run_codex_eval.py`
- Example regression gate command: `packages/aethyme/.venv/bin/python packages/aethyme/scripts/eval/check_regression_gate.py --suite /path/to/suite.json`

Current behavior:
- with no commands, this builds the control artifacts and comparison report only
- with `--control-cmd`, `--explore-cmd`, and `--leverage-cmd`, it executes real runs through the evaluation runner contract
- `--baseline-cmd` and `--aethyme-cmd` remain accepted as legacy aliases for compatibility
- external runners receive the prompt, navigation context, output schema, and Aethyme tool paths through `AETHYME_EVAL_*` env vars
- the bundled Codex wrapper requires `AETHYME_EVAL_ARM=control|aethyme`, runs `codex exec --ignore-user-config --json`, preserves `events.jsonl` / `stderr.log` / `last-message.json` / `leakage.json`, reports wall time, token usage, command-output chars, event-log chars, stderr chars, fixture metadata, and output fingerprints, and fails the run if generated Aethyme artifacts leak into selected files, snippets, command output, or the final answer
- the strict regression gate rejects Aethyme self-evals, incomplete required fixture suites, generated-artifact leakage, missing repeat-output determinism, unbounded command output, token-budget regressions, worse reviewer quality, and hidden Surface/Flow coverage gaps
- every run writes a local markdown report under `packages/aethyme/docs/reports/evals/`
- the repository tracks only a curated subset of eval reports there; the rest are generated local artifacts
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
