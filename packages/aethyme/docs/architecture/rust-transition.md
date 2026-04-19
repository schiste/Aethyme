# Rust-Core Aethyme Transition

Last Updated: 2026-04-19

## Mission

Aethyme is moving to a **Rust core** with one engine, one data model, and one
binary.

This change is driven by concrete pain today:

- duplicated data models across Rust and Python
- subprocess hops on interactive paths
- confidence and truncation signals produced by the engine but not consistently surfaced to users
- distribution friction from split runtime surfaces

The goal is to keep Python only where it clearly wins: fast eval iteration and
Python-native SDK ergonomics.

## The Bet

- **One engine**: the Rust engine runs in-process for CLI/API/MCP paths.
- **One data model**: nodes, edges, context packs, confidence, truncation, and
  cap signals are defined once in Rust and exposed everywhere.
- **One binary**: `aethyme` ships as the primary installable runtime.

Python remains for:

- `src/eval/` workflows
- `sdk/python/`

Both consume the same Rust core via PyO3 (`aethyme_py`) instead of subprocess
calls.

## Target Runtime Shape

`aethyme` (single static binary) contains:

- engine kernel (`aethyme-engine` lineage)
- graph store via `sqlx` (compile-time-checked SQL)
- HTTP API via `axum` + `tower`
- CLI via `clap`
- MCP server via `rmcp`, calling engine in-process
- auth via `jsonwebtoken` + `argon2` + `oauth2`
- scorecard and autofixer runtime in Rust with declarative rule authoring
  (YAML/Rhai)

`aethyme_py` ships as a PyO3 wheel that exposes the same in-process engine to
Python eval harnesses and Python SDK consumers.

## Non-Negotiables

1. **One data model**
   - No parallel dataclass model drift.
   - If a field exists in engine output, it must be available at every caller
     boundary.

2. **Single useful IPC boundary**
   - PyO3 for Python eval + SDK.
   - HTTP only for external network callers.
   - No subprocess engine calls.

3. **Always-visible completeness signals**
   - Capped traversals must return `{ truncated, reason }`.
   - Semantic edges must carry confidence.
   - No silent partial responses.

4. **Single-binary distribution must stay intact**
   - `curl | sh`, `cargo install`, and `brew install` paths must continue to
     work.

5. **Determinism is tested, not assumed**
   - Snapshot tests for engine outputs are maintained through migration.

## Migration Rule

No big-bang rewrite.

A component moves only when it has:

- stable interface boundaries
- measurable determinism/performance win
- sufficient test coverage to prove parity

Every migrated component lands behind a capability flag until Rust is at
parity on the full Playground eval suite. Python removals happen only after
parity is proven.

Per eval integrity rules, parity work must improve generic system behavior,
never tune for specific eval metrics.

## Sequencing

### Phase 0 — stop drift (weeks)

- Propagate confidence/truncation/cap signals through existing Python bridges.
- Force Python-side shapes to converge with Rust model outputs.

**Exit criteria:** signal parity achieved at API/CLI/user boundaries.

### Phase 1 — collapse subprocess IPC

- Replace `src/indexing/engine.py` subprocess bridge with PyO3 binding.
- Keep Python CLI/API temporarily, but all engine calls run in-process.

**Exit criteria:** no subprocess hop on engine calls from Python surfaces.

### Phase 2 — MCP in Rust

- Ship MCP server as Rust subcommand in main `aethyme` binary.
- Remove Python from MCP serving path.

**Exit criteria:** first production-ready single-binary assistant integration.

### Phase 3 — CLI migration

- Port `src/cli.py` behavior to `clap` in Rust.
- Keep Python CLI only until full eval parity is demonstrated.

**Exit criteria:** Rust CLI parity on Playground eval workflows.

### Phase 4 — graph store migration

- Move `src/graph/store.py` responsibilities to Rust + `sqlx`.
- Consolidate schema/query ownership into Rust.

**Exit criteria:** Python `GraphStore` removed.

### Phase 5 — HTTP API migration

- Port FastAPI routes to `axum` with contract-stable cutover.
- Preserve OpenAPI behavior for external SDK compatibility.
- Move auth primitives with API migration.

**Exit criteria:** Python API path removed with zero contract regressions.

### Phase 6 — scorecard + autofixer runtime migration

- Port execution runtime to Rust.
- Keep authoring velocity high via YAML/Rhai rule layer.
- Do not migrate until rule DSL is stable.

**Exit criteria:** Rust runtime parity with equal or better authoring velocity.

### Phase 7 — final Python contraction

- Remove `src/` except eval-specific and Python SDK surfaces.
- Eval harness and SDK consume `aethyme_py` only.

**Exit criteria:** no Python-owned core runtime components remain.

## Explicit Non-Goals

- Porting HTTP API first before model/signal parity is closed
- Expanding breadth-first parser fallback at the cost of deterministic depth
- Eliminating Python entirely
- Freezing product progress for a rewrite-only window

## 12-Month Success Criteria

- One `aethyme` binary installable on macOS/Linux/Windows
- `aethyme_py` wheel on PyPI consumed by eval harness + Python SDK
- Zero subprocess calls into engine
- One end-to-end data model with visible confidence and truncation signals
- MCP surface constrained to a compact set of high-leverage tools
- In-process eval execution with iteration speed maintained or improved

## Operating Principle

This is not a rewrite detour.

The Rust engine already exists and already carries richer deterministic signals
than current user-facing bridges preserve. The transition completes that path by
removing boundary seams while keeping Python where it has enduring leverage.
