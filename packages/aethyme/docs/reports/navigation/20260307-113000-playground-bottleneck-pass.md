# Playground Bottleneck Pass

- Last Updated: 2026-03-07
- Timestamp: 2026-03-07
- Repo: `/Users/christophehenner/Downloads/Repositories/Aethyme Playground`
- Mode: fresh cold build after cache clear

## Goal

Target the measured bottlenecks directly:
- `structure`
- `code_resolve_calls`
- `code_resolve_references`
- `docs_link_areas`
- `graph_annotations`

Also upgrade eval tests so the current report and signal behavior is covered by the Python suite.

## Changes

### 1. Structure pass

File:
- [packages/aethyme/rust/crates/aethyme-engine/src/passes/structure.rs](/Users/christophehenner/Downloads/Repositories/Aethyme/packages/aethyme/rust/crates/aethyme-engine/src/passes/structure.rs)

Changes:
- classify files once at the start of the pass
- replace quadratic inferred-subarea scoring with one-pass aggregated counts
- tighten config-role classification so only operational config/manifests enter the config pipeline

Effect:
- less repeated file-role classification
- less repeated scanning over the full file set
- less config inflation downstream

### 2. Code pass

File:
- [packages/aethyme/rust/crates/aethyme-engine/src/passes/code.rs](/Users/christophehenner/Downloads/Repositories/Aethyme/packages/aethyme/rust/crates/aethyme-engine/src/passes/code.rs)

Changes:
- reuse per-function body analysis across call/reference resolution
- stop re-extracting body tokens for both passes
- restore file-level imported-symbol analysis for correct cross-file relation confidence

Effect:
- preserves semantic correctness
- reduces repeated hot-path work

### 3. Docs pass

File:
- [packages/aethyme/rust/crates/aethyme-engine/src/passes/docs.rs](/Users/christophehenner/Downloads/Repositories/Aethyme/packages/aethyme/rust/crates/aethyme-engine/src/passes/docs.rs)

Changes:
- replace content-wide area-name scanning with token-index-based area linking

Effect:
- `docs_link_areas` no longer scales with `docs * areas * body_length`

### 4. Annotation generation

File:
- [packages/aethyme/rust/crates/aethyme-engine/src/passes/overlays.rs](/Users/christophehenner/Downloads/Repositories/Aethyme/packages/aethyme/rust/crates/aethyme-engine/src/passes/overlays.rs)

Changes:
- add direct path -> file-id map for risk annotation generation
- remove per-risk linear scan over all files

Effect:
- annotation generation becomes indexed instead of repeated linear lookup

### 5. Eval test upgrades

File:
- [packages/aethyme/tests/local/test_engine_cache_and_eval.py](/Users/christophehenner/Downloads/Repositories/Aethyme/packages/aethyme/tests/local/test_engine_cache_and_eval.py)

Changes:
- assert explain-repo eval propagates repo signals
- assert markdown report contains:
  - `## Control`
  - `## Aethyme`
  - `## Repo Signals`
- assert final output messages and structured outputs remain present

## Validation

- `cargo test`: `24 passed`
- `packages/aethyme/.venv/bin/pytest packages/aethyme/tests/local packages/aethyme/tests/docs -q`: `23 passed`
- `packages/aethyme/.venv/bin/ruff check packages/aethyme/src packages/aethyme/tests`: passed

## Fresh Playground Profile

```text
stage=discover_repo duration_ms=3856
stage=structure duration_ms=2327
stage=code_parse_files duration_ms=137
stage=code_normalize_symbols duration_ms=18
stage=code_resolve_imports duration_ms=39
stage=code_resolve_calls duration_ms=3382
stage=code_resolve_references duration_ms=4240
stage=configs_read duration_ms=31
stage=configs_link duration_ms=23
stage=docs_read duration_ms=243
stage=docs_link_areas duration_ms=74
stage=docs_link_files duration_ms=350
stage=docs_link_configs duration_ms=66
stage=docs_link_symbols duration_ms=774
stage=edge_normalization duration_ms=240
stage=overlays duration_ms=290
stage=graph_nodes duration_ms=65
stage=graph_annotations duration_ms=73
stage=graph_sort duration_ms=68
```

Counts:
- `repo_files`: `106096`
- `source_files`: `4008`
- `doc_files`: `1085`
- `config_files`: `79`
- `areas`: `164`
- `directories`: `1639`
- `classes`: `3255`
- `functions`: `12763`
- `docs`: `1085`
- `configs`: `79`
- `graph_nodes`: `125082`
- `graph_edges`: `876805`
- `graph_annotations`: `2146`

Total cold build:
- `19140 ms`

## Before / After

| Metric | Earlier full profile | After this pass |
|---|---:|---:|
| Total cold build | `25253 ms` | `19140 ms` |
| `discover_repo` | `3254 ms` | `3856 ms` |
| `structure` | `7346 ms` | `2327 ms` |
| `code_resolve_calls` | `3321 ms` | `3382 ms` |
| `code_resolve_references` | `3542 ms` | `4240 ms` |
| `docs_link_areas` | `1506 ms` | `74 ms` |
| `graph_annotations` | `1258 ms` | `73 ms` |
| `configs_read` | `30 ms` | `31 ms` |
| Total `code_*` | `7077 ms` | `7816 ms` |

## Interpretation

### What improved materially

- `structure`: `7346 ms` -> `2327 ms`
- `docs_link_areas`: `1506 ms` -> `74 ms`
- `graph_annotations`: `1258 ms` -> `73 ms`

These changes are real and significant.

### What did not improve

The dominant remaining cost is now even more clearly:
- `code_resolve_calls`
- `code_resolve_references`

Those two stages now account for the majority of the remaining semantic build cost.

### What this means

The recent bottleneck work successfully removed the non-semantic waste.

What remains is actual semantic relation resolution cost.

That is the right place for the next performance push.

## Conclusion

This pass made the Playground cold build materially faster and cleaner:
- total cold build is now under `20s`
- non-semantic downstream waste was sharply reduced
- eval/report behavior is better covered by tests

The next performance target is now unambiguous:
1. reduce `code_resolve_calls`
2. reduce `code_resolve_references`

That is where the generator still spends most of its cold-build time.
