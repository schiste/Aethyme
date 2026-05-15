# Graph indexer performance baseline (Phase 4.2)

Last Updated: 2026-05-15

First real-world performance measurement of the Phase 3 indexer
crate against three representative repos. Establishes a baseline
for future optimization work (incremental indexing, per-language
parser caching) to measure against.

## How these numbers were captured

Release binary: `cargo build --release --bin aethyme-graph-index`.
Invoked via:

```bash
aethyme-graph-index \
    --repo-root <path> \
    --repo-name <name> \
    --engine-version 0.1.0 \
    --json
```

Wall-clock time is reported by the binary itself
(`elapsed_ms` in the JSON summary), measured from
`Instant::now()` immediately before `index_repo_to_disk` to the
return of the same function. Includes: filesystem walk,
per-file parse, fragment build, fragment write to disk
(atomic-rename), and per-module index shard write.

Machine: Apple Silicon (Darwin 24.6.0), 8+ cores, release-mode
binary.

## Baseline numbers

### Aethyme self-index (Python + Rust)

```
total_files:       3079
total_skipped:     86 (binaries, unknown extensions)
elapsed_ms:        27499  (~27.5 s)
fragments_written: 3079
shards_written:    3078
per-file avg:      8.9 ms/file
```

Symbol counts:

| Kind | Count |
|---|---|
| file | 389 (code files) |
| non_code_file | 2690 (Markdown, YAML, JSON) |
| function | 2373 |
| method | 675 |
| class | 258 |
| enum | 77 |
| interface | 17 |
| struct | 176 |
| trait | 2 |
| type_alias | 10 |
| global_variable | 481 |

The high `non_code_file` count is the Aethyme docs/reports tree;
the 389 code files split across Python (`src/`), Rust
(`packages/aethyme/rust/`), and TypeScript (the small cloud
front-end).

### MediaWiki - Aethyme (PHP, deep OO)

```
total_files:       10578
total_skipped:     1940 (locale JSON, vendored assets)
elapsed_ms:        70149  (~70.1 s)
fragments_written: 10578
shards_written:    10519
per-file avg:      6.6 ms/file
```

Symbol counts:

| Kind | Count |
|---|---|
| file | 6379 |
| non_code_file | 4199 |
| function | 95 |
| method | 39374 |
| class | 4084 |
| interface | 782 |
| trait | 79 |
| global_variable | 16 |

MediaWiki's shape jumps out: **39,374 methods vs 95 top-level
functions**. That's a 400:1 method-to-function ratio, consistent
with MediaWiki being heavily OO PHP. 4084 classes is the
canonical class count — every MediaWiki feature lives in one or
more of those.

### Mockup - Aethyme (TypeScript-heavy)

```
total_files:       6801
total_skipped:     173 (binary assets, unrecognized)
elapsed_ms:        48050  (~48.0 s)
fragments_written: 6801
shards_written:    6782
per-file avg:      7.1 ms/file
```

Symbol counts:

| Kind | Count |
|---|---|
| file | 5518 |
| non_code_file | 1283 |
| function | 7911 |
| method | 7865 |
| class | 3221 |
| interface | 872 |
| type_alias | 1700 |
| global_variable | 5687 |

Mockup is functional-leaning TS: nearly 1:1 function-to-method
ratio with many `type_alias` declarations and a lot of
`global_variable` entries (mostly `const` exports). The shape
contrasts cleanly with MediaWiki's class-dominated tree.

## Per-file averages

| Repo | Files | Wall-clock | ms/file |
|---|---|---|---|
| Aethyme self | 3,079 | 27.5 s | 8.9 |
| MediaWiki | 10,578 | 70.1 s | 6.6 |
| Mockup | 6,801 | 48.0 s | 7.1 |

~7 ms/file averaged across the three. On an 8-core machine with
rayon-parallel indexing, the per-thread effective parse + emit
cost is ~56 ms-equivalent. The actual per-file parse cost
(measured against the parser's own benchmarks) is sub-millisecond
for most files, so the wall-clock is dominated by filesystem I/O
(atomic-rename tempfile per fragment, plus the BLAKE3 hash work
in NodeId construction) rather than parser CPU.

## Comparison to prior baselines (from `aethyme/memory/`)

| Operation | Old engine | New indexer | Notes |
|---|---|---|---|
| MediaWiki cold index | ~85 s (Phase 1+2 notes, 2026-05-04) | 70 s | -18%, single run. The old engine's cold index hits SurrealDB-write overhead the new indexer skips. |

The new indexer is slightly faster than the old engine's parse
cache build but not dramatically so. The win is concentrated in
the parser-quality dimension (pure-Rust parsers, proper
AST-level extraction) and in the per-file fragment layout
(merge-friendly, incremental-update-ready), not raw cold-index
speed. Incremental re-indexing (deferred to a future phase)
will dwarf both numbers.

## Real-world bugs surfaced during this measurement

The first run against Aethyme self-index failed with two
`Fragment: duplicate node id` errors:

```
duplicate node id "method:aethyme-self:upxtwlvzjnlpqurq6dsbi2lqje"
duplicate node id "function:aethyme-self:7cbqudyenhjvcal6udr3xzm3em"
```

Two root causes:

1. **Method NodeId collisions across receivers.** The
   pre-fix `Method::new` hashed NodeIds over
   `(repo, file, name, kind)` — without the receiver_type. Two
   `__init__` methods on different classes in the same file
   produced identical NodeIds. **Correctness bug**; fixed by
   folding `receiver_type.hash_suffix()` into the symbol-name
   input to `NodeId::new`. Distinct methods on distinct
   receivers now produce distinct NodeIds.
2. **Function NodeId collisions on same-name patterns.** Python
   `@typing.overload` produces multiple `def foo` at the same
   scope with the same name; conditional `def` in
   `if sys.version_info: ... else: ...` blocks does too. These
   ARE the same logical callable (the overloads are typing
   helpers for one runtime function), so silently keeping the
   first one in canonical sort order is the right call.
   **Handling bug**; fixed by changing `Fragment::new` to
   dedup-on-id rather than error-on-duplicate.

Both fixes landed in the same commit that produced these
numbers. The fact that they were caught by real-world running
(not unit tests) is the lesson: forever-schema decisions need
production-codebase stress-testing.

## Phase 4.4 deltas (2026-05-15)

The Python indexer now emits `UnresolvedSymbol` placeholders + `Imports` edges
for every `import` / `from … import …` statement (one placeholder per imported
binding name). Wall-clock impact is negligible — the work is two extra match
arms in the top-level `Stmt` walk and a `Vec` push per imported name. Confirmed
on Aethyme self with the release binary: 14.6 s for 2,978 files (`time` wall
clock), with 1,300 new `unresolved_symbol` nodes generated by Python imports.
The new node kind is the only difference in the on-disk shape; binary fragment
size grows linearly with import count (still dominated by File + Function
payloads in practice).

This is stage 1 of cross-file edge resolution. A future linker pass will walk
the global symbol index, look up each `UnresolvedSymbol` by name + import_path,
and rewrite the edge target to a concrete node. That pass touches zero
language-specific code — it operates entirely on the schema-level `Node` /
`Edge` types. Subsequent commits will add stage 1 for TS / Rust / PHP using
the same `UnresolvedSymbol` + `Imports` shape.

### CLI behavior changes

`aethyme-graph-query find-symbol --name <X>` now also returns import-placeholder
hits when a Python file imports a symbol named `X`. On Aethyme self this means
~1,300 new `unresolved_symbol` rows surface across the corpus. The `--kind`
filter (e.g. `--kind function`) excludes placeholders cleanly. A future
ergonomic improvement is to hide `unresolved_symbol` by default behind an
`--include-unresolved` flag — deferred until the linker pass lands, since
post-linking most of these will be replaced by resolved `Function` / `Class` /
`Module` references and won't appear as separate hits.

### Known limitations carried forward

1. **Edge dedup drops shadowed imports.** Fragment::new dedupes `(src, dst,
   kind)` tuples (storage/fragment.rs:89), which means two imports binding the
   same local name from different modules — `from a import x as y; from b
   import x as y;` — collapse to one `Imports` edge after dedup. The
   surviving edge is the first in sort order rather than Python's "last
   wins". This is a rare real-world pattern but it does occur in conditional
   compatibility shims. Fix is to widen the dedup key to include
   `EdgeAttributes` (or fold `import_path` into the placeholder's NodeId
   formula). Deferred to the linker-pass commit so it can be designed with
   resolved-edge semantics in mind.

2. **Only top-level imports are extracted.** The current Python walk iterates
   `module.body`, so imports inside `try:` / `if:` / function / class bodies
   are silently skipped. Top-level imports are the dominant case in practice
   (>99% of Python code). Nested imports will be addressed when the linker
   pass needs them, since the resolution model is identical.

## Next benchmarking work

- **Per-language parser cost breakdown.** A per-file timing
  report split by language tag would show which parsers are
  the bottlenecks and what the parser-CPU vs filesystem-I/O
  split actually is.
- **Old-engine vs new-indexer apples-to-apples.** A test
  harness that runs both against the same repo and compares
  fragment counts, identified symbols, and wall-clock.
- **Incremental indexing measurement.** Once the engine can
  read fragments back, measure the cost of re-indexing after a
  single-file edit vs. a full re-index. The Option C per-file
  fragment design is supposed to make this 100× cheaper.
