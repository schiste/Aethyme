# Aethyme Core

Last Updated: 2026-08-23

Aethyme Core is the deterministic repository tooling product in this repository.

> **Direction note (2026-08):** Aethyme is a **local-first agent broker** for
> high-concurrency AI development. Per-agent worktrees, sessions, leases,
> gates, normalized submission, integration shipping, handoffs, and redacted
> reports are implemented — see
> [`../../docs/aethyme-local-agent-broker.md`](../../docs/aethyme-local-agent-broker.md).
> The graph engine remains the supporting repository-intelligence service.
> The former Python, FastAPI, PostgreSQL, SDK, and tenant-command product paths
> have all been removed.

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

- `Coordinate`: isolated sessions, leases, gates, submission, shipping, and
  durable handoffs through the broker
- `Explore`: deterministic repository orientation, candidate answers,
  evidence, and verification steps through the graph engine
- `Improve`: explicit local telemetry, scorecards, and controlled autofix
  tooling without a cloud control plane

The broker is the stable operator front door. Explore and lower-level graph,
query, facts, and task commands remain available as supporting repository
intelligence rather than a separate service.

## Language Direction

Arrived:

- Rust for every shipped command: the engine, the router, enhance,
  and the quality domain (scorecard + autofix)
- **No Python at all.** The product path went Python-free in the Phase 6
  sweep (2026-08-01, `src/` deleted) and the dev test stack followed in
  Phase 7 (2026-08-06, `tests/` and `pyproject.toml` deleted). This
  package is 100% Rust: Homebrew or the paired release installer is the
  recommended install story, `cargo install` remains the contributor/source
  fallback, installer-managed binaries expose explicit digest-confirmed
  `aethyme update` commands, and
  `cargo test --workspace` is the whole test story. `packages/aethyme-eval`
  is a separate package and stays Python by design.

See [`docs/architecture/rust-transition.md`](docs/architecture/rust-transition.md) and [`rust/README.md`](rust/README.md).

## Active Surface

### Core Logic
- `rust` (engine, router, broker, enhance, and quality crates —
  quality holds both the AI-readiness scorecard and the autofixers).
  This is the entire product: `src/` was deleted 2026-08-01.

### Delivery
- the Rust `aethyme` router — every command is native, and there is no
  delegation path: an unknown subcommand is an error. `python -m src.cli`
  is **gone** (Phase 6, 2026-08-01) with no shim; it now fails with
  `No module named src`. Run `aethyme --help`.

### Verification
- `cargo test --workspace`: per-crate unit tests, the
  implementation-blind CLI suites in `rust/crates/aethyme-cli/tests/`
  (they drive the built binary as a subprocess), and the repo-hygiene
  suites in `rust/crates/aethyme-testkit/tests/` (docs, PR template,
  grammar provenance). The pytest tree they were ported from is gone
  (Phase 7, 2026-08-06)

## Local-First Workflow

Aethyme runs against local repositories without a SaaS layer.

Primary commands:

- `aethyme deploy --repo /path/to/repo`
- `aethyme deploy verify --repo /path/to/repo`
- `aethyme deploy bridge --repo /path/to/repo`
- `aethyme deploy --local-only --repo /path/to/repo`
- `aethyme upgrade plan --repo /path/to/repo`
- `aethyme upgrade apply --repo /path/to/repo --confirm <plan-sha256>`
- `aethyme explore --repo /path/to/repo --request "<task>" --format answer-json`
  (native Rust entrypoint; the paired `aethyme-engine-cli` binary serves the
  daemon protocol and is installed from the same release archive)
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

### Local workflow test lane

- `cargo test -p aethyme-cli --test local_workflow` indexes a fixture repo
  the same way `scripts/eval/setup-playground.sh` does and drives the
  router over it.
- There is no skip path and no `AETHYME_REQUIRE_LOCAL_ENGINE` opt-in any
  more: `aethyme_testkit::bins` asserts when a binary fails to build, so
  what used to be the strict lane is now the only lane (Phase 7,
  2026-08-06). An environment-dependent skip reports green while
  verifying nothing.
- CI runs it in `.github/workflows/aethyme-local-tests.yml`.

This local path is the shortest route to proving:

1. repository mapping
2. discoverability
3. graph-mediated navigation
4. deterministic task-context packs

Runtime notes:

- the Python layer now executes a built Rust binary rather than `cargo run` for every call
- local repo artifacts are cached by snapshot key under `AETHYME_CACHE_DIR` or `/tmp/aethyme-cache`
- Git repositories use commit plus dirty-state metadata for cache keys instead of a full recursive fingerprint on every call
- `aethyme deploy --repo <path>` is the canonical mandatory repository-enrollment path; it composes broker scaffold, gate drafting, agent-policy deployment, verification, and certification
- `aethyme deploy verify --repo <path>` is its read-only local and CI contract; `aethyme enhance deploy/verify` remain lower-level discoverability operations
- binary installation and repository migration are separate: after updating the paired binaries, `aethyme upgrade plan` reviews embedded repository-policy migrations one repository at a time, and `upgrade apply` requires the plan digest before changing files
- canonical deployments track `.aethyme/repository.json`; local-only deployments keep the equivalent marker ignored, and enrolled repositories fail closed on broker use when their policy schema is stale or newer than the installed binary
- `aethyme deploy bridge` installs a small committed AGENTS/CLAUDE rendezvous point; `deploy --local-only` keeps the full policy, skills, broker configuration, gates, and state clone-local behind `.aethyme/local/enabled`
- an inactive bridge performs no PATH probe, process spawn, installation, warning, or hook work; a fresh clone remains inactive until its developer opts in
- `AGENTS.md` and `CLAUDE.md` are generated artifacts owned by Aethyme; customize them through `.aethyme/overrides/agents.json`, not by editing the root files directly
- generated root instructions include compact repo routing such as skill paths, fast test, app entrypoint, experience status, and commit hygiene policy
- legacy block-managed `AGENTS.md` files are migration-only now; deploy extracts legacy maintainer text into `.aethyme/overrides/agents.json` before rewriting the root file
- `aethyme repo deploy-skills` remains a compatibility path for the static runtime skill and benchmark-oriented consumers
- portable generated onboarding lives at `.aethyme/generated/onboarding.json` and renders to `.codex/skills/repo-onboarding/SKILL.md` and `.claude/skills/repo-onboarding/SKILL.md`; both the canonical input and rendered skills are committed
- portable generated Act starter lives at `.aethyme/generated/act-starter.json` and renders to `.codex/skills/repo-act/SKILL.md` and `.claude/skills/repo-act/SKILL.md`; both are committed
- repo-local onboarding overrides live at `.aethyme/overrides/onboarding.json`; this side owns summon policy, overrides, compact rendering, and generation telemetry, while graph quality stays below the contract boundary
- machine-local experience-layer lifecycle telemetry is written to the ignored `.aethyme/generated/experience-telemetry.jsonl`
- `aethyme repo experience-telemetry --check` now exits nonzero on attention signals such as invalid overrides, no wrapper usage after enhancement, or override/artifact freshness drift
- generated operator status artifacts are machine-local and ignored at `.aethyme/generated/experience-status.json` and `.aethyme/generated/experience-status.md`
- `aethyme repo commit-message-template` and `aethyme repo lint-commit-message` define and validate the typed commit contract Aethyme will later use for repo-memory extraction; substantive commit types (`fix`, `feat`, `refactor`, `perf`) require `Problem`, `Decision`, `Rationale`, and `Validation` sections, while non-substantive types (`test`, `docs`, `build`, `chore`, `revert`) may be subject-only; structured section content can begin on the header line (`Problem: text`) or the following line
- `explore --request ...` defaults to `task_localization_query`, a bounded general-purpose answer path that returns ranked candidate files/symbols/areas, compact evidence, verification steps, confidence, next actions, compact agent observability, `output_chars_estimate`, and `truncated`; on large repos it returns degraded `needs_verification` output instead of blocking or claiming answer safety
- `explore --intent usage_boundary_query` now uses a scope-first PHP analyzer path that returns answer/excluded/confidence/observability without building the full repository graph
- reports include an Aethyme Usage section so availability is not confused with actual `aethyme` invocation

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
