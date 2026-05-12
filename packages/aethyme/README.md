# Aethyme Core

Last Updated: 2026-05-11

Aethyme Core is the deterministic repository tooling product in this repository.

It owns:

1. repository indexing
2. graph persistence and traversal
3. search, ego graph, and impact analysis
4. scorecard analysis
5. controlled autofix tooling from the CLI
6. deterministic navigation primitives for AI agents
7. navigation evaluation benchmarks — see [`docs/guides/eval-protocol.md`](docs/guides/eval-protocol.md)
8. evaluation tooling roadmap — see [`docs/guides/eval-tooling-roadmap.md`](docs/guides/eval-tooling-roadmap.md)

## Public Product Model

Aethyme's public model is:

- `Explore`: deterministic repository orientation, candidate answers, evidence, verification steps
- `Act`: planned layer for task-shaped execution guidance built on Explore outputs
- `Learn`: planned layer for post-task telemetry, ranking feedback, and future improvement

Today, `Explore` is the implemented primary entry point. Lower-level graph,
query, facts, and task commands remain available as supporting primitives, not
the default operator path.

## Canonical Model

`Platform > Org > Tenant > Repository > Graph`

Runtime isolation is tenant-scoped.

## Language Direction

Aethyme is moving toward:

- Rust for deterministic engine components
- Python for API, CLI, auth enforcement, scorecard orchestration, and SDKs

See [`docs/architecture/rust-transition.md`](docs/architecture/rust-transition.md) and [`rust/README.md`](rust/README.md).

## Auth Boundary

- cloud owns login, registration, sessions, and user lifecycle
- core validates bearer credentials and API keys
- core enforces `org`, `tenant_id`, and `scopes`
- local development can mint a cloud-issued token for an existing user via `POST /api/auth/dev/token`

## Active Surface

### Core Logic
- `src/indexer`
- `src/indexing`
- `src/graph`
- `src/models`
- `src/scorecard`
- `src/autofixers`
- `rust`

### Delivery
- `src/api`
- `src/cli.py`
- `sdk/python`

### Verification
- `tests/api`
- `tests/queries`
- `tests/indexing`
- `tests/scorecard`
- `tests/autofixers`

## Current Standard

Only document and defend the verified path:

- trusted bearer token or API key required
- `POST /api/v1/index/repositories`
- `POST /api/v1/search/`
- `POST /api/v1/ego/`
- `POST /api/v1/impact/`
- `POST /api/v1/scorecard/scan`

API and CLI indexing both run through [`src/indexing/service.py`](src/indexing/service.py).

## Local-First Workflow

For the first product proof, Aethyme can run against one local repository without any SaaS layer.

Primary commands:

- `aethyme explore --repo /path/to/repo --request "<task>" --format answer-json`
- `aethyme enhance deploy --repo /path/to/repo`
- `aethyme enhance verify --repo /path/to/repo`
- `aethyme repo compile-skills /path/to/repo`
- `aethyme repo init-onboarding-overrides /path/to/repo`
- `aethyme repo validate-onboarding-overrides /path/to/repo`
- `aethyme repo experience-telemetry /path/to/repo --check`
- `aethyme repo experience-status /path/to/repo`

Supporting commands:

- `aethyme repo ingest /path/to/repo`
- `aethyme repo inspect /path/to/repo --json-output`
- `aethyme repo clear-cache /path/to/repo`
- `aethyme --engine-transport auto repo engine-info`
- `aethyme --engine-transport pyo3 repo engine-info --check`
- `aethyme query symbol /path/to/repo main`
- `aethyme query deps /path/to/repo src/main.py`
- `aethyme graph node /path/to/repo src/main.py --json-output`
- `aethyme graph children /path/to/repo GameEngine --json-output`
- `aethyme graph callers /path/to/repo fn:REPO:path:main --json-output`
- `aethyme graph docs /path/to/repo src/main.py --json-output`
- `aethyme task pack --repo /path/to/repo --task "Explain this repo" --json-output`
- `aethyme task explain --repo /path/to/repo`
- `aethyme task anchors --repo /path/to/repo --task "Update validate_token flow" --json-output`
- `aethyme task scope --repo /path/to/repo --task "Update validate_token flow" --json-output`
- `aethyme task next --repo /path/to/repo --task "Update validate_token flow" --json-output`
- `aethyme task expand --repo /path/to/repo --node src/auth.py --json-output`
- `aethyme eval explain-repo --repo /path/to/repo --json-output`
- `aethyme eval explain-repo --repo /path/to/repo --control-cmd "<cmd>" --explore-cmd "<cmd>" --leverage-cmd "<cmd>"`
- `aethyme eval navigation-ctf --repo /path/to/repo --json-output`

See [`docs/guides/eval-protocol.md`](docs/guides/eval-protocol.md) for the canonical 5-condition playground eval protocol, repository setup flow, and Chau7 execution method.
See [`docs/guides/eval-tooling-roadmap.md`](docs/guides/eval-tooling-roadmap.md) for the current repository-agnostic priority order: strengthen `explore`, then `leverage`, then revisit `task-conditioned`.

### Local workflow test lanes

- Default local test runs skip engine-backed integration tests if the Rust engine cannot be built in the current environment.
- Set `AETHYME_REQUIRE_LOCAL_ENGINE=1` to enforce strict mode (tests fail instead of skip when engine build/runtime is unavailable).
- Strict lane example:
  - `AETHYME_REQUIRE_LOCAL_ENGINE=1 pytest packages/aethyme/tests/local/test_local_workflow.py -q`
- CI runs both lanes in `.github/workflows/aethyme-local-tests.yml`.

This local path is the shortest route to proving:

1. repository mapping
2. discoverability
3. graph-mediated navigation
4. deterministic task-context packs
5. explain-repo evaluation artifacts

Runtime notes:

- the Python layer now executes a built Rust binary rather than `cargo run` for every call
- local repo artifacts are cached by snapshot key under `AETHYME_CACHE_DIR` or `/tmp/aethyme-cache`
- Git repositories use commit plus dirty-state metadata for cache keys instead of a full recursive fingerprint on every call
- `eval explain-repo` can execute local comparison runs when `--control-cmd`, `--explore-cmd`, and `--leverage-cmd` are provided
- external runners receive `AETHYME_EVAL_OUTPUT_SCHEMA_FILE`, `AETHYME_EVAL_TOOL_REPO`, and `AETHYME_EVAL_TOOL_PYTHON` so agent wrappers can enforce structured output and call back into Aethyme
- the canonical playground protocol is Chau7 MCP with 5 conditions: `control-cto-off`, `control-cto-on`, `explore`, `leverage`, `task-conditioned`
- without those commands, it still emits the comparison artifacts only
- the `leverage` condition uses a compact generic Aethyme Explore usage card; `task-conditioned` remains the full context-pack mode
- `aethyme enhance deploy --repo <path>` is the primary real-repository enhancement path; it writes cross-product discoverability files plus generated repo-onboarding artifacts
- Aethyme owns only the generated `AETHYME:BEGIN/END` block in `AGENTS.md`; maintainer-authored instructions outside that block are preserved, and the block includes compact repo routing such as skill paths, fast test, app entrypoint, and experience status
- `aethyme repo deploy-skills` remains a compatibility path for the static runtime skill and benchmark-oriented consumers
- generated onboarding lives at `.aethyme/generated/onboarding.json` and renders to `.codex/skills/repo-onboarding/SKILL.md` and `.claude/skills/repo-onboarding/SKILL.md`
- generated Act starter lives at `.aethyme/generated/act-starter.json` and renders to `.codex/skills/repo-act/SKILL.md` and `.claude/skills/repo-act/SKILL.md`
- repo-local onboarding overrides live at `.aethyme/overrides/onboarding.json`; this side owns summon policy, overrides, compact rendering, and generation telemetry, while graph quality stays below the contract boundary
- stable experience-layer lifecycle telemetry is written to `.aethyme/generated/experience-telemetry.jsonl`
- `aethyme repo experience-telemetry --check` now exits nonzero on attention signals such as invalid overrides, no wrapper usage after enhancement, or override/artifact freshness drift
- generated operator status artifacts now live at `.aethyme/generated/experience-status.json` and `.aethyme/generated/experience-status.md`
- `explore --request ...` defaults to `task_localization_query`, a bounded general-purpose answer path that returns ranked candidate files/symbols/areas, compact evidence, verification steps, confidence, next actions, and observability; on large repos it returns degraded `needs_verification` output instead of blocking or claiming answer safety
- `explore --intent usage_boundary_query` now uses a scope-first PHP analyzer path that returns answer/excluded/confidence/observability without building the full repository graph
- reports include an Aethyme Usage section so availability is not confused with actual `src.cli` invocation
- every `eval explain-repo` run writes a markdown report under `packages/aethyme/docs/reports/evals/`
- the report includes quality score, recalculated eval score vs control baseline, tool usage, tokens, duration, prompts, pack JSON, and verbose run results
- eval outputs now include a structured output schema, scoring rubric, and reference answer

## Start Here

- [`../../docs/project-plan.md`](../../docs/project-plan.md)
- [`docs/vision.md`](docs/vision.md)
- [`docs/agent-navigation-spec.md`](docs/agent-navigation-spec.md)
- [`docs/architecture/research-informed-architecture-memo.md`](docs/architecture/research-informed-architecture-memo.md)
- [`docs/architecture/research-lessons-revised-after-implementation.md`](docs/architecture/research-lessons-revised-after-implementation.md)
- [`docs/architecture/graphability-and-navigability-signals.md`](docs/architecture/graphability-and-navigability-signals.md)
- [`docs/architecture/rust-transition.md`](docs/architecture/rust-transition.md)
- [`rust/README.md`](rust/README.md)
- [`docs/getting-started/quickstart.md`](docs/getting-started/quickstart.md)
- [`roadmap.md`](roadmap.md)
