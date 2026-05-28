# Phase 4.7 — Fragments-Only Cutover Plan (Variant C1)

Last Updated: 2026-05-28

## Forcing function

At the start of Phase 4.7, two pre-computed graph stores were dragging
behind the live one:

1. `map_cache` — a bincode dump of the entire `RepositoryMap` under
   `.aethyme/cache/`, sized-capped at 1 GiB, gitignored. **Deleted in
   4.7.10.**
2. `ParseStore` — the redb parse cache under `.aethyme/parse_store.redb`
   that backed the per-file parser fast-path (Phase 1+2). **Deleted in
   4.7.11.**

Both predate `aethyme-graph-storage` (the per-file `.bin` fragments +
`_index/*.ndjson` shards under `.aethyme/graph/`, committed to git, byte-pinned
by `§5.7` CI gate). Now that fragments exist, the three stores diverge whenever
producers and consumers race. The user's chosen direction is the most
aggressive convergence we have available — **Variant C1**:

- The on-disk graph is the **only** pre-computed surface.
- The daemon's in-memory `RepositoryMap` is populated **from fragments**, not
  by re-parsing.
- The engine does **no parsing and no decoration** at indexing time. Parsing
  lives in producers; decoration (structure, configs, docs, risks) lives in
  producers that emit non-per-file *overlay fragments*.

Phase 4.7 lands that convergence in 12 mechanically-reviewable commits. After
the last commit, `map_cache.rs`, the `ParseStore` surface, and the engine's
`passes/` + `overlays/` modules are deleted.

## Context inherited from earlier phases

| What we proved | What it means for Phase 4.7 |
|---|---|
| `aethyme-graph-storage` produces byte-deterministic per-file `.bin` + `_index/*.ndjson` | Same property must hold for overlay fragments (test pin: `overlay_bytes_on_disk_byte_identical_across_writes`) |
| `Fragment::new` enforces canonical sort + dedup of nodes/edges | `OverlayFragment::new` lets payloads carry that responsibility — producers must sort/dedup before construction |
| Atomic-rename writes (`write to <path>.tmp.<pid>` → `fs::rename`) survive crashes | Reused unchanged in `write_overlay` |
| `§5.7` CI gate byte-compares regenerated fragments against committed ones | Same gate covers `_overlays/*.bin` once 4.7.7 lands |
| Producers are already file-scoped (Stage 1: TS/Rust/PHP imports) | The hard part of 4.7 is non-per-file producers (structure/configs/docs/risks) — the overlay envelope is built for them |
| `RepositoryMap::build_internal` (`map.rs:207`) is the single cache-check chokepoint | Only one site needs the `populate_from_fragments` branch (4.7.7) |
| The engine binary has 7+ `RepositoryMap::build*` call sites | They reach the same `build_internal` chokepoint — no per-call-site changes needed |

## Current surface (the work)

**Code that will disappear:**

| File | Lines | Disappears in commit |
|---|---|---|
| `engine/src/map_cache.rs` | deleted | 4.7.10 (done 2026-05-28) |
| `engine/src/cache.rs` (parse-cache value type + hash helper) | deleted | 4.7.11 (done 2026-05-28) |
| `engine/src/store/redb/parse_store.rs` | deleted | 4.7.11 (done 2026-05-28) |
| `engine/src/store/redb/mod.rs` (`parse_store` re-export) | deleted | 4.7.11 (done 2026-05-28) |
| `engine/src/passes/code.rs` | ~55K | 4.7.12 |
| `engine/src/passes/structure.rs` | ~13K | 4.7.12 |
| `engine/src/passes/configs.rs` | ~16K | 4.7.12 |
| `engine/src/passes/docs.rs` | ~17K | 4.7.12 |
| `engine/src/passes/overlays.rs` | ~7K | 4.7.12 |
| `engine/src/passes/mod.rs` | trivial | 4.7.12 |

**Code that gains an `OverlayProducer` impl** (new in producers crate):

| Producer | Current home | Overlay kind |
|---|---|---|
| Repository structure | `passes/structure.rs` | `structure` |
| Configs | `passes/configs.rs` | `configs` |
| Docs | `passes/docs.rs` | `docs` |
| Risks (incl. `shared_core_risks`, `destructive_risks`) | `passes/overlays.rs` | `risks` |
| Areas (cluster summaries) | inline in `map.rs` | `areas` (deferred — landed only if blocked by 4.7.7) |

**NOT in scope for Phase 4.7:**

- The Python `src/graph/store.py` API layer (separate concern).
- The redb `GraphStore` (Phase 3 work) — that's the cloud-package store, not
  the daemon's in-memory map. C1 leaves it alone.
- New producer features. Phase 4.7 is **migration only**; every
  fragment/overlay produced after the cutover must match what `passes/*` would
  have produced before, byte-for-byte where the `§5.7` gate covers it.

## Target architecture

```text
              ┌──────────────────────────────────────────────┐
              │ producers crate (already exists; gains       │
              │ structure, configs, docs, risks)             │
              └────────────────────┬─────────────────────────┘
                                   │  writes
                                   ▼
              ┌──────────────────────────────────────────────┐
              │  .aethyme/graph/                              │
              │    <source>.bin            (per-file Fragment)│
              │    _index/<module>.ndjson  (symbol shards)    │
              │    _overlays/<kind>.bin    (OverlayFragment<P>)│
              └────────────────────┬─────────────────────────┘
                                   │  read by
                                   ▼
              ┌──────────────────────────────────────────────┐
              │  engine: RepositoryMap::populate_from_fragments│
              │  (parses **nothing**; folds fragments +       │
              │   overlays into the in-memory map)            │
              └──────────────────────────────────────────────┘
```

Two version axes carried by every overlay payload:

- `schema_version: u16` — wire envelope version, currently `1`. A bump is a
  forever-format-change event (§5.4 / §5.7).
- `producer_version: &'static str` — producer logic identity, free-form
  (e.g. `"structure-producer/0.1.0"`). Producers bump this freely; the engine
  uses it for blame/cache-keying, never for compatibility gating.

## Migration sequence (granular commits)

| # | Commit | Touches | Risk | Rollback |
|---|---|---|---|---|
| **4.7.1** | `feat(graph-storage): overlay fragment schema + layout` | `aethyme-graph-storage`: `overlay.rs`, `disk.rs`, `layout.rs`, `lib.rs`, `store.rs`, `tests/disk.rs` (+7 tests), `docs/architecture/graph-schema.md` (§5.1, new §5.4, renumber), this plan doc | Tiny | `git revert` — no consumer yet |
| **4.7.2** | `feat(producers): OverlayProducer trait + harness` | `aethyme-producers`: new `OverlayProducer` trait (`produce(ctx) -> OverlayFragment<P>`); test harness verifying determinism (run twice, compare bytes); no concrete producers yet | Tiny | `git revert` |
| **4.7.3** | `feat(producers): port structure → OverlayProducer` | New `producers/structure.rs`; engine still owns `passes/structure.rs` (duplicate, intentional). Producer's output `.bin` byte-compared against engine's existing struct in a parity test. | Small | `git revert` — engine path still live |
| **4.7.4** | `feat(producers): port configs → OverlayProducer` | Same shape as 4.7.3 for `configs` | Small | `git revert` |
| **4.7.5** | `feat(producers): port docs → OverlayProducer` | Same shape for `docs` | Small | `git revert` |
| **4.7.6** | `feat(producers): port risks → OverlayProducer` | Same shape for `risks`. Carries `shared_core_risks` + `destructive_risks` thresholds verbatim from `passes/overlays.rs` (`SHARED_CORE_HIGH_THRESHOLD = 5`, `SHARED_CORE_LOW_THRESHOLD = 3`, `DESTRUCTIVE_PATTERNS = [delete, drop, destroy, remove, truncate, reset, purge, wipe]`) | Small | `git revert` |
| **4.7.7** | `feat(engine): populate_from_fragments cutover (gated)` | `engine/src/map.rs`: new `populate_from_fragments(root) -> RepositoryMap`; `build_internal` (line 207) gains an `if fragments_dir_exists { populate_from_fragments } else { old path }` guard. New `--from-fragments` CLI flag for explicit invocation. Tests assert: build via fragments == build via passes (byte-equal for fields covered by `§5.7`; structural-equal otherwise). | **Medium-High** | `git revert` — guarded, old path still default |
| **4.7.8** | `refactor(engine): daemon defaults to fragments` | `engine/src/daemon.rs:165-168` switches to fragment-preferred `RepositoryMap` builds. CLI flag flips to opt-*out* (`--no-fragments`), while legacy `--from-fragments` remains the explicit cache-bypassing diagnostic path. Bench: build time on Mockup + MediaWiki recorded in commit body. | Medium | `git revert` — flag flip only |
| **4.7.9** | `chore(engine): cross-process consumer audit` | Grep `docs/architecture/cross-process-consumers.md` for callers of map-cache or parse-store paths; update each; record outstanding items inline in the consumer registry. (Per Cardinal Rule 3 — this must land *before* the deletes.) | Low | `git revert` — doc-only |
| **4.7.10** | `chore(engine): delete map_cache.rs` | Delete `map_cache.rs`; remove `mod map_cache;` from `lib.rs`; drop the two call sites in `map.rs` (`try_load_cached_map` and `save_cached_map`). Keep the engine's direct `bincode` dependency because `ParseStore` and `GraphStore` still use it at this point. | Medium | `git revert` works but daemon will re-parse from scratch until restart |
| **4.7.11** | `chore(engine): delete ParseStore` | Delete `store/redb/parse_store.rs` and `cache.rs`; trim `store/redb/mod.rs`; remove `ParseStore` parameters/call sites from `map.rs` and `passes::code`. Keep `redb` and `bincode` because `GraphStore` still uses both. | Medium | `git revert` — parse-store DB on disk becomes orphan |
| **4.7.12** | `chore(engine): delete passes/ + engine indexer` | Delete `passes/{code,structure,configs,docs,overlays,mod}.rs`, the legacy `indexer/` module, and parity tests that compared against them. `RepositoryMap` is now fragments-only; `--from-fragments` is compatibility spelling and `--no-fragments` hard-errors. | Medium | `git revert` — but this is the "no going back" line; once shipped, fragments/producers own the parsing surface |
| **4.7.13** | (no commit) Validate: full Rust test suite + Mockup + MediaWiki build-profile + cross-process audit (Cardinal Rule 1 — evals on Playground only) | — | — | — |

12 implementation commits + 1 validation step. Each lands with green tests. The
cutover (4.7.7) is the only commit with elevated risk — it changes the
data-flow, but both paths remain functional behind a flag.

## Decisions already locked

Captured here so a future reader doesn't relitigate them:

- **Variant C1**: producers own parsing; engine reads fragments only. (User
  selected, see `MEMORY.md` and prior conversation transcript.)
- **Per-kind overlay file** (`_overlays/<kind>.bin`) rather than one
  combined `_overlays.bin`: blast-radius isolation (a bad `risks` producer
  doesn't corrupt `structure`), and per-kind bincode discriminant spaces stay
  independent. (Captured in `graph-schema.md §5.4`.)
- **`OverlayFragment<P>` generic** rather than one giant `enum
  OverlayKind { Structure(...), Configs(...), ... }`: keeps the storage crate
  payload-agnostic. New kinds land in producers without touching
  `aethyme-graph-storage`.
- **Belt-and-braces kind check**: `write_overlay(repo, kind, &overlay)` takes
  `kind` as a parameter *and* the bytes embed `overlay.kind()`; read-side
  verifies they match (`OverlayDecodeError::KindMismatch`). Catches both
  caller mistakes and filesystem rename surprises.
- **No runtime registry mapping kinds → payload types**. Callers supply the
  expected `P`. A `KindMismatch` is the load-bearing signal that you asked
  for the wrong type.
- **Historical parity gates, not "trust me" cutover**: every `passes/*` →
  producer port (4.7.3 through 4.7.6) landed with a test that byte-compared
  producer output against engine output on a fixture repo. Those parity tests
  were removed with the legacy engine path in 4.7.12; determinism tests remain
  on the producer side.

## Risks

- **Fragment-era compatibility drift**: after 4.7.12, there is no engine
  fallback to the old `passes/` pipeline. Compatibility now means the
  fragments/producers-backed `RepositoryMap` preserves the public model
  surfaces existing engine consumers use: readable IDs, file/config/doc
  roles, area overlays, and fragment edges remapped into model IDs.
- **Cross-process consumer breakage** (Cardinal Rule 3): `map_cache.rs` and
  `ParseStore` were internal in the 4.7.9 audit; no Python, shell, eval
  manifest, skill, or CI consumer was found before deletion.
- **Daemon restart cost spikes during cutover**: after 4.7.10,
  cold-start no longer has `map_cache` to fall back on. Mitigation:
  4.7.7→4.7.8 landed first; 4.7.12 now uses the same fragments-only
  path for CLI and daemon builds.
- **Eval interference** (Cardinal Rule 2): tempting to tune
  `populate_from_fragments` performance based on eval-run timings.
  Don't — measure against the build-profile harness, not eval token counts.
- **Producer determinism**: bincode is deterministic only if input is
  canonical. The `OverlayFragment` envelope cannot enforce this; producers
  must sort + dedup their payloads before calling `OverlayFragment::new`.
  4.7.2's test harness pins this with double-run byte-equality, but only
  on the harness's fixture — a real producer could regress if the author
  forgets. The harness pattern is the only durable defense.
- **bincode dep removal**: do not drop the engine's direct `bincode`
  dependency with `map_cache.rs` or `ParseStore`; `GraphStore` still encodes
  Redb values with it. `aethyme-graph-storage` also depends on bincode for
  fragments, but that does not satisfy the engine crate's direct uses.

## Estimated effort

12 implementation commits × ~30–90 minutes each = ~10–18 hours across two
or three fresh sessions. The high end assumes one of the producer ports
(probably `risks`, because of the threshold semantics) needs a split commit
for the parity test fixture.

## Out of scope for Phase 4.7

- **Eval reruns**: post-cutover eval rerun for confidence is a separate
  workstream. Phase 4.7 ships when tests + build-profile are green;
  eval-time validation is on Playground per Cardinal Rule 1.
- **`GraphStore` (cloud) consolidation**: the Phase-3 redb `GraphStore` is
  the cloud-package's surface. Whether it should *also* read from
  fragments is a separate (probably larger) workstream and deliberately
  out of C1's scope.
- **`areas` overlay**: cluster summaries currently live inline in `map.rs`.
  If 4.7.7 needs them to be persisted (it shouldn't — they're computed),
  add an `areas` overlay then. Otherwise skip.
- **Removing the `_index/*.ndjson` shards**: they're an independent
  optimization. Don't conflate.
- **New parsers / new languages**: Phase 4.7 is migration only. A new
  language goes through the producers crate after 4.7 lands.

## Pre-flight check before Phase 4.7 starts

1. `aethyme-graph-storage` test suite green (71 tests, incl. 17 disk +
   7 overlay tests). ← already true as of 4.7.1's WIP.
2. `docs/architecture/graph-schema.md §5.4` describes the overlay envelope
   and §5.1's tree shows `_overlays/`. ← already true.
3. `cross-process-consumers.md` is up-to-date with current consumers of
   `map_cache` and `parse_store`. ← verified in 4.7.9.
4. A scratch branch (off `main`) is open for the 12-commit sequence; the
   parity tests added in 4.7.3–6 are visible on it before 4.7.7 lands.

When all four are green, start commit 4.7.1.
