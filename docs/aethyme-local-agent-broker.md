# Aethyme Local Agent Broker — Direction (v0)

Status: **planned, not implemented** (as of 2026-07-09)
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

The long-term architecture is portable, local-first, and high-performance,
able to become a SaaS-connected system later without ever requiring cloud
execution.

## Current state (be precise about this)

Nothing described in the "v0 scope" below exists yet. As of the Phase 0 truth
audit (2026-07):

- there is **no** worktree management code in the repository
- there is **no** agent session registry
- there is **no** lease/lock/conflict system
- there is **no** merge simulation
- there is **no** event bus
- there is **no** affected-gate runner

What does exist is the **deterministic Rust graph engine** (repo indexing,
redb store, committed graph fragments, navigation and impact-frontier
queries, a warm engine daemon) and the Python CLI and eval harness around
it. The engine remains a supporting repo-intelligence service for the broker;
the broker itself is a **new local subsystem**.

## Core architectural split

Two stores, two responsibilities. Do not mix them.

```
Graph engine (exists)              Broker (planned, new)
─────────────────────              ─────────────────────
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

## v0 scope

Aethyme v0 local broker should eventually provide:

- one isolated git worktree per agent/task
- an agent session registry (task, branch, worktree path, command, logs,
  status, exit state)
- changed-file tracking per session
- overlapping-edit detection across live sessions
- a simple gate runner (configured gates, run on changed files, results
  cached)
- merge simulation against an integration branch before promotion
- a promotion flow (submit → simulate → gate → promote/reject)
- an append-only event log
- CLI status commands (`status`, `agents`, `leases`, `events`, ...)

## Explicitly out of scope for v0

- SaaS or any cloud execution
- remote multi-machine coordination
- rich per-agent adapters (an agent is a command template in v0)
- a dashboard or TUI
- graph-based affected **test selection** (the graph does not populate
  test-coverage edges today; v0 gate selection is config/glob-driven, with
  graph impact hints as an optional advisory layer only)
- cloud auth
- team policy sync
- an advanced lease policy engine (v0 warns on overlap; it does not arbitrate)

## Implementation choice (decided, not yet implemented)

- **Rust broker core.** Product-grade broker components are written in Rust,
  living alongside the existing engine crates in
  `packages/aethyme/rust/crates/`.
- **CLI wrapper.** Broker commands are exposed through a Rust binary and/or
  the existing click CLI as thin delivery, matching the current
  Python-delivery / Rust-engine split.
- **SQLite for broker operational state** at `.aethyme/broker.db` (WAL mode).
  redb remains appropriate for the graph/cache-style storage where it is
  already used; operational, multi-writer, queryable state belongs in SQLite.
- **Git-native operations first**, shelling out to `git` (worktree, diff,
  merge-tree) behind a clean internal Git service layer — no libgit2/gix
  dependency until a measured need appears.
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
2. **Phase 2 — worktree & session broker**: `start-agent`, `agents`,
   `cleanup`; worktree lifecycle; subprocess spawn with log capture.
3. **Phase 3 — leases & conflict detection**: diff-derived implicit leases,
   overlap warnings, TTL expiry.
4. **Phase 4 — affected gate runner**: `gates.toml`, glob-triggered
   selection, cheap-first ordering, tree-hash result cache.
5. **Phase 5 — merge simulation & promotion queue**: `git merge-tree`
   simulation, gates on the merged tree, serialized promotions.
6. **Phase 6 — event log & status UX**: every mutation emits an event;
   `status` and `events --follow`.

Each phase lands only with tests, and none of it modifies the graph engine's
contracts.
