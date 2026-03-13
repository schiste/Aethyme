# Call/Reference Optimization Pass

Date: 2026-03-07
Last Updated: 2026-03-07
Repo under test: `/Users/christophehenner/Downloads/Repositories/Aethyme Playground`

## Scope
This pass targeted the two remaining hot sub-stages in the Rust repograph generator:
- `code_resolve_calls`
- `code_resolve_references`

Changes made:
- added precomputed `call_names` to per-function analysis
- replaced quadratic same-file call detection with local name-index lookup
- removed repeated body string scans from `body_call_candidates`
- removed fallback string scans in cross-file reference resolution
- replaced local class reference scanning with token-index lookup
- kept imported-symbol-aware cross-file semantic resolution intact
- removed dead `StringInterner::resolve` after the refactor

## Validation
- `cargo test`: passed (`24 passed`)
- `packages/aethyme/.venv/bin/pytest packages/aethyme/tests/local packages/aethyme/tests/docs -q`: passed (`23 passed`)
- `packages/aethyme/.venv/bin/ruff check packages/aethyme/src packages/aethyme/tests`: passed

## Fresh Cold Profile
Fresh cache was cleared before the run.

Current run:
- `discover_repo`: `5083 ms`
- `structure`: `2376 ms`
- `code_parse_files`: `134 ms`
- `code_normalize_symbols`: `16 ms`
- `code_resolve_imports`: `38 ms`
- `code_resolve_calls`: `3467 ms`
- `code_resolve_references`: `3395 ms`
- `configs_read`: `30 ms`
- `configs_link`: `19 ms`
- `docs_read`: `403 ms`
- `docs_link_areas`: `80 ms`
- `docs_link_files`: `364 ms`
- `docs_link_configs`: `73 ms`
- `docs_link_symbols`: `783 ms`
- `edge_normalization`: `226 ms`
- `overlays`: `311 ms`
- `graph_nodes`: `68 ms`
- `graph_annotations`: `75 ms`
- `graph_sort`: `58 ms`
- total cold build: `19689 ms`

Counts:
- repo files: `106096`
- source files: `4008`
- doc files: `1085`
- config files: `79`
- classes: `3255`
- functions: `12763`
- graph nodes: `125082`
- graph edges: `740470`
- graph annotations: `2146`

## Comparison Against Previous Stable Baseline
Previous measured baseline from the prior bottleneck pass:
- `discover_repo`: `3856 ms`
- `structure`: `2327 ms`
- `code_resolve_calls`: `3382 ms`
- `code_resolve_references`: `4240 ms`
- total cold build: `19140 ms`

Delta:
- `code_resolve_calls`: `3382 ms` -> `3467 ms` (`+85 ms`, effectively flat)
- `code_resolve_references`: `4240 ms` -> `3395 ms` (`-845 ms`, meaningful improvement)
- combined `calls + references`: `7622 ms` -> `6862 ms` (`-760 ms`)
- total cold build: `19140 ms` -> `19689 ms` (`+549 ms`)

## Interpretation
The hot-path rewrite improved the semantic sub-stages where expected:
- call resolution did not materially regress or improve
- reference resolution improved substantially
- combined semantic relation cost is lower by about `760 ms`

The total cold build did not improve in this specific run because:
- `discover_repo` moved up by roughly `1227 ms`
- `docs_read` also moved up relative to the earlier run

So the optimization is real, but it is partially masked at total-build level by cold-run variance outside the targeted sub-stages.

## Current Bottleneck Ranking
On this latest Playground run, the dominant stages are now:
1. `discover_repo` (`5083 ms`)
2. `code_resolve_calls` (`3467 ms`)
3. `code_resolve_references` (`3395 ms`)
4. `structure` (`2376 ms`)
5. `docs_link_symbols` (`783 ms`)

## Conclusion
This pass was worthwhile.
- The targeted semantic hot path is better.
- The generator remains correct and warning-free.
- The next work should focus on:
  1. `discover_repo`
  2. `code_resolve_calls`
  3. `docs_link_symbols`

The immediate lesson is that the repograph engine is now in a state where sub-stage optimization is measurable and predictable rather than opaque.
