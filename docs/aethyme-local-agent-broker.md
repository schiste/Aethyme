# Aethyme Local Agent Broker — Direction (v0)

Status: **V1 active; dogfood stage exited** (as of 2026-07-17)
Owner: Aethyme core
Related: [`project-plan.md`](project-plan.md), [`../packages/aethyme/docs/vision.md`](../packages/aethyme/docs/vision.md) (historical)

## What Aethyme is becoming

Aethyme is repositioning from a graph-based agent performance optimizer to a
**local-first agent broker for high-concurrency AI development**: a local
concurrency-control layer that lets many AI coding agents work on the same
repository without conflicting, duplicating work, wasting local CI, or
blindly stepping on each other.

Aethyme is explicitly **not**:

- a generic AI agent orchestrator
- a cloud-first SaaS
- a dashboard-first product
- a graph demo
- a Claude/Codex-specific wrapper
- a Chau7-dependent feature

The long-term architecture is portable, local-first, and high-performance.
SaaS is dropped from planning entirely (decision 2026-07-09): the focus is
the core engine and its capabilities, delivered through a CLI (at best a
TUI later) — but everything runs through APIs with clear, versioned
contracts, so any future delivery surface is a client, not a rewrite.

## Product theses (decided 2026-07-09)

1. **Neutral substrate — multi-vendor is the point.** Vendors will each
   coordinate their own agents; none will coordinate each other's. Aethyme
   is agent-agnostic by construction: a session is a worktree producing
   diffs, regardless of whether Claude Code, Codex, Aider, or a shell
   script is driving it. Multi-vendor operation is a v0 requirement, not a
   later adapter story.
2. **Worktree-first, attach-capable.** Session identity is the
   (worktree, branch) pair. The normal entry point is now `broker start`,
   which creates an isolated worktree + branch and registers the session;
   `broker adopt` remains the attach path for a worktree the user or agent
   tool already created. `broker start-agent` is a convenience spawner
   layered on the same model. PID is optional metadata captured when the
   broker did the spawning; liveness derives from diff/file activity, with
   process state as a bonus signal. No design may assume the broker owns the
   agent process.
3. **API-first.** The broker core is a Rust library crate with a typed
   public API; the CLI is a thin client of it, every command has a
   `--json` form, and the append-only events schema is a versioned
   contract from day one. A TUI, editor integrations, or anything else
   consume the same surface.

## Scope decisions (2026-07-10)

- **Single entrypoint, Rust-focused:** one user-facing `aethyme` command —
  the Rust router binary — with broker commands under `aethyme broker ...`;
  the Python CLI is delegated or ported over time (issue #31).
- **Conflict handling messages the agent:** on a rejected submit the broker
  writes actionable instructions into the session's worktree (vendor-neutral
  file drop; the generated AGENTS.md points agents at it) rather than only
  reporting to the human.
- **Design ceiling: 15 concurrent sessions** (stress-tested at 20), CLI +
  SQLite coordination, still no daemon in v0.
- **Platforms: macOS and Linux.** Windows is a non-goal for v0.
- **Vendor artifacts may be read for liveness** — opt-in per vendor,
  read-only, metadata/mtime only, never content.
- **Whole-repo worktrees only.** Package-awareness comes from gate globs
  and path-scoped leases; cross-repo coordination is v2+.
- **Everything is built as if public** (contracts, docs, history), even
  while the repository is private.
- **Kill criterion for the dogfood:** stop if costs increase too much AND
  no time is saved; the friction log includes cost/time accounting.
- **Promotion lands on a local integration branch only.** `broker submit`
  never pushes and never opens PRs. Explicitly authorized Git and GitHub
  mutations use `broker git` / `broker gh`: fixed executables, repository-wide
  write serialization, a durable redacted journal, and fail-closed recovery
  when a crashed operation has an unknown external outcome.
- **The broker manages conflicts; quality is repo-owned and optional**
  (clarified 2026-07-13). `gates.toml` is repo policy, not broker policy —
  the broker is pure mechanism (selection, ordering, caching,
  cancellation) and runs the repo's checks on the merged tree because
  that is the only place *semantic* conflicts are detectable. A repo with
  no `gates.toml` is a valid conflict-only deployment: textual simulation
  and promotion on clean merges, zero verification, recorded as such.
- **Promotion trigger is configurable — auto by default** (amended
  2026-07-13 after the first dogfood run: verified means verified, and a
  human promote step makes the human the bottleneck). Gates passing
  promotes immediately; `[promote] mode = "manual"` restores the explicit
  `broker promote` step.

## Current state (be precise about this)

**Everything in the "v0 scope" below is built** (Phases 1–6, 2026-07-10 →
2026-07-13): the `aethyme-broker` crate implements sessions, leases, the
affected gate runner, the merge queue with auto-promotion, and the event
log, exposed as `aethyme broker ...` in the Rust router. The deterministic
v0 scenario test covers the three-agent story end to end, and the first
live dogfood loop ran on this repository on 2026-07-13 (see
`docs/dogfood-friction.md`). The dogfood week completed and **#33 closed on
2026-07-17**; V1 is now active.

The **deterministic Rust graph engine** (repo indexing, redb store,
committed graph fragments, navigation and impact-frontier queries) remains
a separate, supporting repo-intelligence service. The broker now consults it
read-only for bounded caller-frontier gate hints; those hints are reported
separately and never alter path-triggered gate enforcement.

## Core architectural split

Two stores, two responsibilities. Do not mix them.

```
Graph engine                       Broker
────────────                       ──────
code intelligence                  sessions
impact hints                       worktrees
repo structure                     leases
redb + committed fragments         events
                                   gates + results
                                   merge queue + promotions
                                   SQLite (.aethyme/broker.db)
```

Broker state is **never** forced into the graph schema. The graph schema is a
deliberately closed, append-ordered bincode contract for code entities; it is
the wrong home for operational state. The broker consults the graph
(read-only) for advisory signals — "these two diffs touch the same module",
"this change's impact frontier includes X" — and must degrade gracefully when
the graph is cold or stale.

Semantic gate selection follows that same split. Path triggers in
`.aethyme/gates.toml` remain the enforced selection surface for session submit
and `broker gates run`; CI still runs the full gates definition via
`broker gates run --all`. Caller-edge or impact-frontier data may only appear
as advisory operator guidance. The `broker gates semantic --session <id>`
command is the intentionally separate read surface: it walks a deterministic,
strictly bounded incoming `Calls` frontier and reports explainable
changed-file → caller-file → suggested-gate chains. Missing, stale, corrupted,
empty, or truncated graph results remain successful advisory reports and do
not affect `broker gates run` or submit-time merged-tree gates.

## v0 scope

Aethyme v0 local broker should eventually provide:

- one isolated git worktree per agent/task
- an agent session registry keyed on (worktree, branch): `broker start`
  creates the default isolated worktree; `broker adopt` registers an
  existing worktree; `broker start-agent` creates worktree + spawns a
  command template as a convenience; task, logs, status, and exit state are
  recorded where known
- changed-file tracking per session
- explicit path leases and guarded command execution for write ownership
- overlapping-edit detection across live sessions
- a simple gate runner (configured gates, run on changed files, results
  cached)
- merge simulation against an integration branch before promotion
- a promotion flow (submit → simulate → gate → promote/reject)
- an append-only event log with a versioned schema (the integration
  contract for any future surface)
- CLI status commands (`status`, `agents`, `leases`, `events`, ...), each
  with a `--json` form backed by the library API

**v0 success criterion (decided 2026-07-09): dogfooding, not a staged
demo.** v0 is done when the broker is used for real multi-agent development
on this repository's own day-to-day work — with at least two different
agent vendors (e.g. Claude Code and Codex) attached concurrently — and has
produced a written friction log of blockers, pros, and cons. A
deterministic scripted-agent integration test exists alongside for CI, but
passing it is not the bar.

## Explicitly out of scope for v0

- SaaS, cloud execution, or SaaS-oriented design work of any kind
- remote multi-machine coordination
- rich per-agent adapters (multi-vendor works because sessions are
  vendor-agnostic worktrees, not because of adapters)
- a dashboard; a TUI is a possible later client of the same API, not v0
- graph-based affected **test selection** (the graph does not populate
  test-coverage edges today; v0 gate selection is config/glob-driven, with
  graph impact hints as an optional advisory layer only)
- cloud auth
- team policy sync
- an advanced lease policy engine (v0 warns on overlap; it does not arbitrate)

## Implementation choice (decided, not yet implemented)

- **Rust broker core as a library crate.** Product-grade broker components
  are written in Rust, living alongside the existing engine crates in
  `packages/aethyme/rust/crates/`. The crate's typed public API is the
  product surface; delivery layers are clients of it.
- **CLI as a thin client.** Broker commands are exposed through a Rust
  binary (and/or the existing click CLI) that only calls the library API.
  Every command has a `--json` output form; those JSON shapes and the
  events schema are versioned contracts, registered in
  `packages/aethyme/docs/architecture/cross-process-consumers.md` once
  external callers exist.
- **SQLite for broker operational state** at `.aethyme/broker.db` (WAL mode).
  redb remains appropriate for the graph/cache-style storage where it is
  already used; operational, multi-writer, queryable state belongs in SQLite.
- **Git-native operations first**, shelling out to `git` (worktree, diff,
  merge-tree) behind a clean internal Git service layer — no libgit2/gix
  dependency until a measured need appears.
- **External mutations are journaled, not falsely transactional.** A repository
  write lock serializes `git` and `gh` subprocesses. Prepared/running/terminal
  states are durable; a process death after start becomes `outcome_unknown`
  and blocks overlapping writes until operator reconciliation.
- **No daemon in v0** unless unavoidable; CLI invocations coordinate through
  SQLite. A future daemon, if needed, follows the existing local
  unix-socket + line-delimited JSON pattern used by the engine daemon.
- **Local config** in `.aethyme/config.toml`; gate definitions in
  `.aethyme/gates.toml`.

### Why not Python for the broker core

Python is acceptable for quick experiments and throwaway prototypes of broker
behavior, but it is not the long-term product-grade foundation:

1. **Concurrency correctness.** The broker's whole job is coordinating many
   concurrent processes; Rust's ownership model and `std::process`/file-lock
   primitives make race-prone code visible at compile time.
2. **Startup latency.** Broker commands run constantly in agent inner loops.
   The repository already grew a Python daemon *solely* to amortize Python
   interpreter startup — and its dispatch is now dead code. A native binary
   avoids the problem instead of caching around it.
3. **Distribution.** A single static binary per platform is the credible
   install story for a local-first tool; the Python package drags a venv and
   an interpreter version constraint.
4. **Precedent in this repo.** The deterministic, performance-sensitive parts
   of Aethyme already migrated to Rust deliberately (see
   `packages/aethyme/docs/architecture/rust-transition.md`); the broker is
   exactly that class of component.

Python experiments toward the broker, if any, live outside the product path
and are labeled as experiments.

## Sequencing after Phase 0

1. **Phase 1 — local state model**: SQLite schema (sessions, leases, gates,
   gate_results, merge_queue, events) behind a typed store layer.
2. **Phase 2 — worktree & session broker**: `start` (broker-created
   worktree), `adopt` (registration on existing worktrees), `start-agent`
   (worktree + spawn convenience), `agents`, `cleanup`; worktree lifecycle;
   activity-based liveness with PID as optional metadata.
3. **Phase 3 — leases & conflict detection**: diff-derived implicit leases,
   explicit path claims, guarded exec, overlap warnings, TTL expiry.
4. **Phase 4 — affected gate runner**: `gates.toml`, glob-triggered
   selection, cheap-first ordering, tree-hash result cache.
5. **Phase 5 — merge simulation & promotion queue**: `git merge-tree`
   simulation, gates on the merged tree, serialized promotions.
6. **Phase 6 — event log & status UX**: every mutation emits an event;
   `status` and `events --follow`.

Each phase lands only with tests, and none of it modifies the graph engine's
contracts.
