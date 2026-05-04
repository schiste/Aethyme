# Phase 3 — SurrealDB Graph Store → redb Migration Plan

Last updated: 2026-05-04

## Forcing function

SurrealDB's BSL 1.1 license restricts commercial / SaaS use. Aethyme's
cloud package needs a graph store that can be deployed without BSL
encumbrance. This is non-negotiable; redb (Apache-2.0, pure Rust) is the
chosen replacement. Phases 1 and 2 already validated redb against the
parse-cache workload (`store/redb/parse_store.rs`).

## Context inherited from Phase 1 + 2

| What we proved | What it means for Phase 3 |
|---|---|
| redb works embedded, single-file, MVCC, ACID | No fundamental rewrite — extend the existing `store/redb/` module |
| Variant B (`BuildSession` with batched commits) works at scale | Reuse the same pattern for graph indexing |
| `InternedStr` makes string-heavy entities cheap | Use it as the natural key type — 16-byte fat pointer instead of 30+ byte String, atomic clone |
| `edges_by_target` inverted index is essential for graph queries | Phase 3 schema must include both `edges_out` and `edges_in` `MultimapTable`s from day one |
| bincode boundaries keep the storage layer clean | Use bincode as the wire format; entity types serialize transparently |
| Per-snapshot files give free GC and immutable freezing | One redb DB per `(repo, snapshot)`; old snapshots GC by `unlink` |
| Test suite (57 tests) catches regressions | Run after each migration commit |

## Current SurrealDB surface (the work)

**Code that uses SurrealDB directly** (8 files, ~2,500 lines):

| File | What it does |
|---|---|
| `src/store/mod.rs` | `GraphStore::open` — opens SurrealDB at `<repo>/.aethyme/graph.db` |
| `src/store/schema.rs` | Schema definition: nodes, edges, areas |
| `src/store/write.rs` | Insert APIs: `insert_area`, `insert_file`, `insert_edge`, `insert_risk`, `delete_file_data` |
| `src/store/read.rs` | Read APIs: `list_areas`, `list_files`, `list_edges_outgoing`, `list_edges_incoming`, `subgraph`, `overview` |
| `src/store/snippets.rs` | Snippet retrieval helpers |
| `src/store/prompt.rs` | Prompt generation using graph queries |

**Consumers of `GraphStore`** (7+ call sites in `src/bin/aethyme-engine-cli.rs`):

CLI dispatch for: `store-build`, `store-overview`, `store-subgraph`,
`store-snippets`, `store-prompt`, plus the indexer pipeline. Each opens
a `GraphStore`, runs queries, formats output. All async because Surreal is
async.

**NOT in scope for Phase 3:**
- The Python `src/graph/store.py` `GraphStore` — that's a separate layer
  (the API/cloud consumer) and doesn't import the Rust SurrealDB code.
- The parse cache (`store/redb/parse_store.rs`) — already on redb.
- The in-memory `RepositoryMap` — that's a separate query-time
  representation; keep as-is.

## Schema

Per-snapshot file: `<repo>/.aethyme/graph_store.redb` (or
`<repo>/.aethyme/graphs/<snapshot_id>.redb` if we go per-commit).

```rust
// ── Identity / metadata ────────────────────────────────────────────────
const META: TableDefinition<&str, &[u8]> = TableDefinition::new("meta");
// "schema_version", "snapshot_id", "built_at_unix", "repo_root", ...

// ── Node records (one table per kind, separate tablespaces) ────────────
const FILES:     TableDefinition<&str, &[u8]> = TableDefinition::new("files");
const AREAS:     TableDefinition<&str, &[u8]> = TableDefinition::new("areas");
const FUNCTIONS: TableDefinition<&str, &[u8]> = TableDefinition::new("functions");
const CLASSES:   TableDefinition<&str, &[u8]> = TableDefinition::new("classes");
const DOCS:      TableDefinition<&str, &[u8]> = TableDefinition::new("docs");
const CONFIGS:   TableDefinition<&str, &[u8]> = TableDefinition::new("configs");
// Key: node_id (raw &str so prefix-range scans work for scope queries).
// Value: bincode-encoded entity record. Entity fields are InternedStr; bincode
// roundtrips them via the same serde path used in parse_store.

// ── Adjacency (the wedge for ego/impact/dead-code queries) ─────────────
const EDGES_OUT: MultimapTableDefinition<&str, &[u8]> = MultimapTableDefinition::new("edges_out");
const EDGES_IN:  MultimapTableDefinition<&str, &[u8]> = MultimapTableDefinition::new("edges_in");
// Key: src node id (out) / dst node id (in). Value: bincoded EdgeRecord
// (kind: u8, other_node_id: InternedStr).

// ── Scope-bounded lookups (raw paths give free prefix range reads) ─────
const FUNCTIONS_BY_PATH: MultimapTableDefinition<&str, &str> =
    MultimapTableDefinition::new("fn_by_path");
const NODES_BY_PATH: MultimapTableDefinition<&str, &str> =
    MultimapTableDefinition::new("nodes_by_path");
// Key: file_path. Value: node id. Range scan from "includes/" to
// "includes/\xff" = scope query.

// ── Symbol search ──────────────────────────────────────────────────────
const SYMBOL_BY_NAME: MultimapTableDefinition<&str, &str> =
    MultimapTableDefinition::new("sym_by_name");
// Key: lowercased name. Value: node id.

// ── Risk overlays ──────────────────────────────────────────────────────
const RISK_FLAGS: MultimapTableDefinition<&str, &[u8]> =
    MultimapTableDefinition::new("risk_flags");
```

This deliberately matches the schema sketched mid-Phase-1 with one update:
`edges_in` is now first-class (informed by the `edges_by_target`
algorithmic fix). On MediaWiki this is the difference between O(E) and
O(in_degree) for caller queries.

## Migration sequence (granular commits)

Following the Phase 1 + 2 pattern of small, mechanically-reviewable
commits each landing with green tests.

| # | Commit | Touches | Risk |
|---|---|---|---|
| **3.1** | `feat(engine): add redb GraphStore skeleton + schema` | `store/redb/graph_store.rs` (new): `GraphStore::open`, schema tables, basic write/read primitives, tests | Tiny |
| **3.2** | `feat(engine): add GraphStore::index method (Variant B build session)` | `graph_store.rs`: `IndexSession` borrowing `WriteTransaction`, `insert_node`, `insert_edge`, batched commit policy reusing the rotate pattern from `parse_store` | Small |
| **3.3** | `feat(engine): port write APIs to redb (insert_area, insert_file, insert_edge, ...)` | `graph_store.rs`: parity with `store/write.rs`. SYNC api (no async needed for redb). | Medium |
| **3.4** | `feat(engine): port read APIs to redb (list_areas, list_edges_*, subgraph, overview)` | `graph_store.rs`: parity with `store/read.rs`. Use `MultimapTable` for adjacency, range scans for scope queries. | Medium |
| **3.5** | `feat(engine): redb GraphStore parity — snippets + prompt` | New module mirrors `store/snippets.rs` + `store/prompt.rs` against the redb backend | Medium |
| **3.6** | `refactor(engine): swap CLI to redb GraphStore` | `bin/aethyme-engine-cli.rs`: 7 call sites updated to use `store::redb::graph_store::GraphStore` instead of `store::GraphStore`. Async→sync at boundaries. | Medium-High |
| **3.7** | `chore(engine): delete SurrealDB store, drop dependency` | Delete `store/{mod,read,write,schema,snippets,prompt}.rs`. Remove `surrealdb` and `tokio` from Cargo.toml. | Small |
| **3.8** | (no commit) Validate: full test suite + MediaWiki build-profile + dead-code + GRC if available | `dhat` re-run optional | – |

Estimated 7–10 commits, depending on whether 3.4 needs to split (read APIs are denser than write APIs).

## Decisions to lock before starting

These are real choices where I'd want your input before sinking days
into implementation:

### Q1 — File layout: per-snapshot vs single-file-with-snapshot-prefix?

- **Per-snapshot file** (`<repo>/.aethyme/graphs/<snapshot_id>.redb`):
  - ✅ Free GC (just `unlink`)
  - ✅ Read-only freezing trivial
  - ✅ One writer per file = no contention
  - ❌ Many small files at scale (could be 100s on an active repo)
- **Single file, snapshot prefix in keys** (`<repo>/.aethyme/graph_store.redb`):
  - ✅ One file to manage
  - ❌ GC requires range deletes (more work, fragmentation)
  - ❌ Single writer for the whole file

**My lean: per-snapshot file**. Matches the parse-store pattern; cloud
multi-tenancy story is cleaner.

### Q2 — Async-to-sync transition

The current `GraphStore` is async because SurrealDB is. redb is sync.
Two paths:

- **Sync API throughout**: drop `async` from all signatures. Cleaner.
  CLI dispatch loses Tokio runtime. Caller code (Python via FFI) sees
  no difference because it shells out to a binary anyway.
- **Keep async signatures** wrapping sync redb calls: lets caller code
  stay async-compatible. But every async fn is just `Ok(sync_call())` —
  wasteful indirection.

**My lean: sync API**. The async-ness was driven by SurrealDB; nothing
else in the engine needs it.

### Q3 — Migration path: parallel vs swap

- **Parallel build** (3.1 → 3.5 land redb store alongside SurrealDB,
  then 3.6 swaps): safer, easier rollback. Bigger Cargo.lock churn
  during the transition.
- **In-place swap** (each layer migrated and consumers updated in the
  same commit): smaller PRs but higher per-commit risk; if something
  breaks mid-migration, partial state.

**My lean: parallel build then swap**. Mirrors what we did for
`parse_store` (lived alongside `cache.rs` until Phase 1 deleted it).

### Q4 — `RepositoryMap` interaction

Today `RepositoryMap::build` produces an in-memory map; the SurrealDB
store is populated separately by `store-build` CLI. Should Phase 3:

- **Preserve current architecture**: redb store is populated via
  `store-build` separately. `RepositoryMap` remains in-memory.
- **Unify**: build the redb store *during* `RepositoryMap::build` so
  there's only one indexing pipeline.

**My lean: preserve current architecture** for Phase 3. Unification is
a separate (larger) workstream — it forces decisions about whether
in-memory map should still exist at all.

## Risks

- **Tokio drop**: removing `tokio` is a meaningful Cargo.lock change.
  Need to confirm nothing else in the engine uses `tokio` (the dhat
  search work suggested it's only via SurrealDB). 5-min audit.
- **SurrealDB schema differences**: `RELATE` semantics in Surreal are
  graph-DB-native (record references with table prefixes); redb uses
  bare keys. Some queries may need re-thinking, especially `subgraph`
  expansion.
- **API consumers**: the `bin/aethyme-engine-cli.rs` async calls need
  the right error mapping. surrealdb::Error is async-typed; redb's
  errors are sync. The error-conversion layer needs care (parse_store
  has a clean example with `From<redb::*Error>` impls).

## Estimated effort

7–10 commits × ~30–60 minutes of focused work each = 4–8 hours total
across one or two fresh sessions. Each commit lands with green tests
and is independently reviewable.

## Out of scope for Phase 3

These are real but separate:

- **Python `GraphStore` API layer**: lives in `src/graph/store.py`,
  separate concern.
- **`RepositoryMap` redb-fication**: as Q4 notes, unification is
  larger than Phase 3.
- **Parse-time cross-thread interning** ("Phase 2c"): would drop the
  remaining ~8 GB of `Symbol::new` allocations, but invasive (rayon +
  thread-safe interner). Schedule separately if memory pressure stays
  uncomfortable.
- **Replacement for SurrealDB's `RELATE` query language ergonomics**:
  redb is bare KV; we hand-roll the queries we need. No drop-in
  alternative.

## Pre-flight check before Phase 3 starts

1. Confirm redb v2 still works with the entity types after Phase 2
   (it does, parse_store's serde path uses InternedStr).
2. Audit `tokio` usage outside SurrealDB (5-min grep).
3. Decide Q1, Q2, Q3, Q4 with the user.
4. Open a Phase 3 branch (separate from main).

When all four are green, start commit 3.1.
