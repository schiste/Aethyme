# Discover Parallel Line Counting and Qualified Call Pass

Date: 2026-03-07
Last Updated: 2026-03-07
Repo under test: `/Users/christophehenner/Downloads/Repositories/Aethyme Playground`

## Scope
This pass continued work on the remaining dominant stages:
- `discover_repo`
- `code_resolve_calls`
- `code_resolve_references`
- `docs_link_symbols`

Changes made:
- moved line counting out of recursive discovery and into a parallel post-pass over discovered files
- removed serial inline file reads from the directory walk
- precomputed qualified call names so `.foo(` and `::foo(` checks no longer scan bodies in the inner loop
- trimmed the per-function analysis record further after the call rewrite

## Validation
- `cargo test`: passed (`24 passed`)
- `packages/aethyme/.venv/bin/pytest packages/aethyme/tests/local packages/aethyme/tests/docs -q`: passed (`23 passed`)
- `packages/aethyme/.venv/bin/ruff check packages/aethyme/src packages/aethyme/tests`: passed

## Fresh Cold Profile
Fresh cache was cleared before the run.

Current run:
- `discover_repo`: `2796 ms`
- `structure`: `2322 ms`
- `code_parse_files`: `171 ms`
- `code_normalize_symbols`: `14 ms`
- `code_resolve_imports`: `43 ms`
- `code_resolve_calls`: `3178 ms`
- `code_resolve_references`: `3563 ms`
- `configs_read`: `29 ms`
- `configs_link`: `21 ms`
- `docs_read`: `216 ms`
- `docs_link_areas`: `76 ms`
- `docs_link_files`: `348 ms`
- `docs_link_configs`: `66 ms`
- `docs_link_symbols`: `316 ms`
- `edge_normalization`: `132 ms`
- `overlays`: `301 ms`
- `graph_nodes`: `62 ms`
- `graph_annotations`: `70 ms`
- `graph_sort`: `39 ms`
- total cold build: `16100 ms`

Counts:
- repo files: `106096`
- source files: `4008`
- doc files: `1085`
- config files: `79`
- classes: `3255`
- functions: `12763`
- graph nodes: `125082`
- graph edges: `504338`
- graph annotations: `2146`

## Comparison Against Previous Stable Baseline
Previous baseline from the last pass:
- `discover_repo`: `3629 ms`
- `code_resolve_calls`: `3132 ms`
- `code_resolve_references`: `3215 ms`
- `docs_link_symbols`: `305 ms`
- total cold build: `16624 ms`

Delta:
- `discover_repo`: `3629 ms` -> `2796 ms` (`-833 ms`)
- `code_resolve_calls`: `3132 ms` -> `3178 ms` (`+46 ms`, flat)
- `code_resolve_references`: `3215 ms` -> `3563 ms` (`+348 ms`, worse on this run)
- `docs_link_symbols`: `305 ms` -> `316 ms` (`+11 ms`, flat)
- total cold build: `16624 ms` -> `16100 ms` (`-524 ms`)

## Interpretation
This pass clearly improved repository discovery.

Most important outcomes:
- `discover_repo` is now below `2.8s` on this fresh Playground run
- total cold build improved again to about `16.1s`
- the semantic stages remain the dominant work after discovery and structure

The semantic stage movement in this run is mixed:
- `code_resolve_calls` is effectively unchanged
- `code_resolve_references` regressed versus the immediately previous baseline
- `docs_link_symbols` is flat

So the discovery optimization is solid, but the semantic hot path is still the main optimization target.

## Current Bottleneck Ranking
On this latest Playground run, the dominant stages are now:
1. `code_resolve_references` (`3563 ms`)
2. `code_resolve_calls` (`3178 ms`)
3. `discover_repo` (`2796 ms`)
4. `structure` (`2322 ms`)
5. `docs_link_files` (`348 ms`)

## Conclusion
This pass was still worthwhile because it pushed the total cold build lower and made `discover_repo` materially faster.

The next best work is now unambiguous:
1. `code_resolve_references`
2. `code_resolve_calls`
3. `structure`
4. `discover_repo` only after those if needed

The graph generator is now down to about `16.1s` cold-start on `Aethyme Playground`, and the next major gains must come from the semantic relation stages, not discovery.
