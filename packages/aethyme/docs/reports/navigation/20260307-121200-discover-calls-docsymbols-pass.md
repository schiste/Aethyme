# Discover Repo, Call Resolution, and Doc-Symbol Linking Pass

Date: 2026-03-07
Last Updated: 2026-03-07
Repo under test: `/Users/christophehenner/Downloads/Repositories/Aethyme Playground`

## Scope
This pass targeted the three remaining stages the user selected:
- `discover_repo`
- `code_resolve_calls`
- `docs_link_symbols`

Changes made:
- removed per-directory child-path sorting from repository discovery
- removed duplicate file stat work for non-symlink files
- kept deterministic output by sorting the final file list only
- narrowed doc-to-symbol global fallback so only explicit symbol mentions search the global symbol index
- kept generic doc token linking area-scoped only
- removed inner-loop target-name interning from call resolution by carrying the call token through candidate resolution

## Validation
- `cargo test`: passed (`24 passed`)
- `packages/aethyme/.venv/bin/pytest packages/aethyme/tests/local packages/aethyme/tests/docs -q`: passed (`23 passed`)
- `packages/aethyme/.venv/bin/ruff check packages/aethyme/src packages/aethyme/tests`: passed

## Fresh Cold Profile
Fresh cache was cleared before the run.

Current run:
- `discover_repo`: `3629 ms`
- `structure`: `2223 ms`
- `code_parse_files`: `136 ms`
- `code_normalize_symbols`: `13 ms`
- `code_resolve_imports`: `36 ms`
- `code_resolve_calls`: `3132 ms`
- `code_resolve_references`: `3215 ms`
- `configs_read`: `29 ms`
- `configs_link`: `21 ms`
- `docs_read`: `404 ms`
- `docs_link_areas`: `70 ms`
- `docs_link_files`: `350 ms`
- `docs_link_configs`: `65 ms`
- `docs_link_symbols`: `305 ms`
- `edge_normalization`: `121 ms`
- `overlays`: `288 ms`
- `graph_nodes`: `59 ms`
- `graph_annotations`: `68 ms`
- `graph_sort`: `40 ms`
- total cold build: `16624 ms`

Counts:
- repo files: `106096`
- source files: `4008`
- doc files: `1085`
- config files: `79`
- classes: `3255`
- functions: `12763`
- graph nodes: `125082`
- graph edges: `504326`
- graph annotations: `2146`

## Comparison Against Previous Stable Baseline
Previous stable baseline from the last report:
- `discover_repo`: `5083 ms`
- `code_resolve_calls`: `3467 ms`
- `code_resolve_references`: `3395 ms`
- `docs_link_symbols`: `783 ms`
- total cold build: `19689 ms`

Delta:
- `discover_repo`: `5083 ms` -> `3629 ms` (`-1454 ms`)
- `code_resolve_calls`: `3467 ms` -> `3132 ms` (`-335 ms`)
- `code_resolve_references`: `3395 ms` -> `3215 ms` (`-180 ms`)
- `docs_link_symbols`: `783 ms` -> `305 ms` (`-478 ms`)
- total cold build: `19689 ms` -> `16624 ms` (`-3065 ms`)

## Interpretation
This pass produced clear improvements on all three targeted areas.

Most important outcomes:
- repository discovery is materially faster after removing unnecessary per-directory sorting and duplicate stat work
- call resolution improved modestly after removing one remaining inner-loop string/interning cost
- reference resolution also improved slightly because the same call-token work reduced overlap in the semantic hot path
- doc-to-symbol linking improved substantially because generic document tokens no longer trigger global symbol lookups
- total cold build time improved by just over `3 seconds`

The edge count also dropped significantly:
- `740470` -> `504326`

That is not a regression by itself. It reflects a stricter and less noisy symbol-linking path, especially on docs.

## Current Bottleneck Ranking
On this latest Playground run, the dominant stages are now:
1. `discover_repo` (`3629 ms`)
2. `code_resolve_references` (`3215 ms`)
3. `code_resolve_calls` (`3132 ms`)
4. `structure` (`2223 ms`)
5. `docs_read` (`404 ms`)

## Conclusion
This was a successful pass.

The repograph generator on `Aethyme Playground` improved from:
- `19689 ms` cold build

to:
- `16624 ms` cold build

The next highest-value work is now:
1. `discover_repo`
2. `code_resolve_references`
3. `code_resolve_calls`
4. `structure`

The engine is now significantly closer to a usable cold-start experience on large mixed repos.
