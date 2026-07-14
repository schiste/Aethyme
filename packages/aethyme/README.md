# Aethyme Core

Last Updated: 2026-07-09

Aethyme Core is the deterministic repository tooling product in this repository.

> **Direction note (2026-07):** Aethyme is repositioning toward a **local-first
> agent broker** for high-concurrency AI development. The broker (per-agent
> worktrees, session registry, leases, gate runner, merge simulation) is
> **planned, not implemented** — see
> [`../../docs/aethyme-local-agent-broker.md`](../../docs/aethyme-local-agent-broker.md).
> The graph engine described below remains the supporting repo-intelligence
> service. The Gen-0 PostgreSQL graph, the FastAPI service, and the
> tenant CLI commands were REMOVED on 2026-07-13 (partial execution of the
> cloud-lineage decision); `src/auth`, the SDK, and Postgres deps remain
> for the final #30 sweep.

It owns:

1. repository indexing
2. graph persistence and traversal (Rust fragments + redb)
3. symbol, dependency, and impact queries via the engine
4. scorecard analysis
5. controlled autofix tooling from the CLI
6. deterministic navigation primitives for AI agents
7. agent broker + certification (`rust/crates/aethyme-broker`)

## Public Product Model

Aethyme's public model is:

- `Explore`: deterministic repository orientation, candidate answers, evidence, verification steps
- `Act`: planned layer for task-shaped execution guidance built on Explore outputs
- `Learn`: planned layer for post-task telemetry, ranking feedback, and future improvement

Today, `Explore` is the implemented primary entry point. Lower-level graph,
query, facts, and task commands remain available as supporting primitives, not
the default operator path.

## Language Direction

Aethyme is moving toward:

- Rust for deterministic engine components
- Python for CLI surfaces, enhance/onboarding, and scorecard orchestration

See [`docs/architecture/rust-transition.md`](docs/architecture/rust-transition.md) and [`rust/README.md`](rust/README.md).

## Active Surface

### Core Logic
- `src/indexing` (engine adapter, onboarding, skills)
- `src/scorecard`
- `src/autofixers`
- `rust` (engine + broker crates)

### Delivery
- `src/cli.py` (Python surfaces) and the Rust `aethyme` router (explore,
  certify, broker)

### Verification
- `tests/local` (CI lane), `tests/indexing`, `tests/scorecard`,
  `tests/autofixers`, `tests/contracts`, `tests/docs`, Rust workspace tests

## Local-First Workflow

For the first product proof, Aethyme can run against one local repository without any SaaS layer.

Primary commands:

- `aethyme explore --repo /path/to/repo --request "<task>" --format answer-json`
  (single Rust entrypoint since 2026-07-14 (#31): install with
  `cargo install --path rust/crates/aethyme-engine`; the pip console script
  was removed so nothing shadows the router. Explore runs in-process and
  auto-starts the engine daemon. Delegated Python commands resolve the
  package via `aethyme root show` — env var, pointer file, or upward walk.)
- `aethyme enhance deploy --repo /path/to/repo`
- `aethyme enhance verify --repo /path/to/repo`
- `aethyme repo compile-skills /path/to/repo`
- `aethyme repo init-onboarding-overrides /path/to/repo`
- `aethyme repo validate-onboarding-overrides /path/to/repo`
- `aethyme repo init-agents-overrides /path/to/repo`
- `aethyme repo validate-agents-overrides /path/to/repo`
- `aethyme repo experience-telemetry /path/to/repo --check`
- `aethyme repo experience-status /path/to/repo`
- `aethyme repo commit-message-template --type fix --scope watchlist`
- `aethyme repo lint-commit-message .git/COMMIT_EDITMSG`

Supporting commands:

- `aethyme repo ingest /path/to/repo`
- `aethyme repo inspect /path/to/repo --json-output`
- `aethyme repo clear-cache /path/to/repo`
- `aethyme repo warm /path/to/repo`
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
- `aethyme task context --repo /path/to/repo --task "Update validate_token flow" --json-output`
The evaluation harness was removed on 2026-07-13 — design knowledge preserved
at [`docs/architecture/eval-mining-notes.md`](docs/architecture/eval-mining-notes.md).

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

Runtime notes:

- the Python layer now executes a built Rust binary rather than `cargo run` for every call
- local repo artifacts are cached by snapshot key under `AETHYME_CACHE_DIR` or `/tmp/aethyme-cache`
- Git repositories use commit plus dirty-state metadata for cache keys instead of a full recursive fingerprint on every call
- `aethyme enhance deploy --repo <path>` is the primary real-repository enhancement path; it writes cross-product discoverability files plus generated repo-onboarding artifacts
- `AGENTS.md` and `CLAUDE.md` are generated artifacts owned by Aethyme; customize them through `.aethyme/overrides/agents.json`, not by editing the root files directly
- generated root instructions include compact repo routing such as skill paths, fast test, app entrypoint, experience status, and commit hygiene policy
- legacy block-managed `AGENTS.md` files are migration-only now; deploy extracts legacy maintainer text into `.aethyme/overrides/agents.json` before rewriting the root file
- `aethyme repo deploy-skills` remains a compatibility path for the static runtime skill and benchmark-oriented consumers
- generated onboarding lives at `.aethyme/generated/onboarding.json` and renders to `.codex/skills/repo-onboarding/SKILL.md` and `.claude/skills/repo-onboarding/SKILL.md`
- generated Act starter lives at `.aethyme/generated/act-starter.json` and renders to `.codex/skills/repo-act/SKILL.md` and `.claude/skills/repo-act/SKILL.md`
- repo-local onboarding overrides live at `.aethyme/overrides/onboarding.json`; this side owns summon policy, overrides, compact rendering, and generation telemetry, while graph quality stays below the contract boundary
- stable experience-layer lifecycle telemetry is written to `.aethyme/generated/experience-telemetry.jsonl`
- `aethyme repo experience-telemetry --check` now exits nonzero on attention signals such as invalid overrides, no wrapper usage after enhancement, or override/artifact freshness drift
- generated operator status artifacts now live at `.aethyme/generated/experience-status.json` and `.aethyme/generated/experience-status.md`
- `aethyme repo commit-message-template` and `aethyme repo lint-commit-message` define and validate the typed commit contract Aethyme will later use for repo-memory extraction; substantive commit types (`fix`, `feat`, `refactor`, `perf`) require `Problem`, `Decision`, `Rationale`, and `Validation` sections
- `explore --request ...` defaults to `task_localization_query`, a bounded general-purpose answer path that returns ranked candidate files/symbols/areas, compact evidence, verification steps, confidence, next actions, and observability; on large repos it returns degraded `needs_verification` output instead of blocking or claiming answer safety
- `explore --intent usage_boundary_query` now uses a scope-first PHP analyzer path that returns answer/excluded/confidence/observability without building the full repository graph
- reports include an Aethyme Usage section so availability is not confused with actual `src.cli` invocation

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
