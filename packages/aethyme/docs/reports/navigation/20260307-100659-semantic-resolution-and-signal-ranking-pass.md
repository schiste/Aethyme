# Semantic Resolution And Signal-Aware Ranking Pass

Date: 2026-03-07
Last Updated: 2026-03-07
Repo under test: `/Users/christophehenner/Downloads/Repositories/ADD`

## Scope
This pass implemented two engine-level improvements:

1. Better language-aware semantic relation resolution for Python and Rust.
2. Conservative signal-aware ranking and scope sizing in navigation.

## Files Changed
- `packages/aethyme/rust/crates/aethyme-engine/src/passes/code.rs`
- `packages/aethyme/rust/crates/aethyme-engine/src/overview.rs`
- `packages/aethyme/rust/crates/aethyme-engine/src/navigation.rs`
- `packages/aethyme/src/eval/explain_repo.py`

## Implementation Details

### 1. Python/Rust imported-symbol resolution
`passes/code.rs` now:
- extracts imported symbol names from Python `from ... import ...` and `import ... as ...`
- extracts imported symbol names from Rust `use ...` and `use ...::{...}`
- uses those imported names to raise confidence for:
  - cross-file `calls`
  - cross-file `references`
- recognizes qualified-call forms more explicitly:
  - Python `.name(`
  - Rust `::name(`

A Rust regression test was added for cross-file imported call resolution.

### 2. Signal-aware overview ranking
`overview.rs` now uses repo signals to size overview output conservatively:
- weaker boundary clarity -> fewer code areas
- weaker parser visibility -> allow more reference areas
- stronger entrypoint clarity -> more entrypoints
- stronger config hygiene -> more key configs

### 3. Signal-aware change-task scope sizing
`navigation.rs` now uses repo signals to avoid over-expansion:
- weak hidden coupling on change tasks -> tighter file cap
- `task next` is truncated more aggressively when graph confidence is weak
- change-task symbol scope is trimmed to the retained in-scope files

### 4. Explain-repo reference output consistency fix
`explain_repo.py` no longer rebuilds key configs from raw inspect data.
It now uses the graph-derived overview key-config slice directly.

## Validation
- `cargo test`: passed (`19 passed`)
- `pytest tests/local tests/docs -q`: passed (`23 passed`)
- `ruff check src tests`: passed

## Live Results On ADD

### Graph Overview
- `code_areas`: `tools`, `GameEngine`, `godot`
- `reference_areas`: `lore`, `documentation`
- `key_configs`: `GameEngine/rust/addgame/Cargo.toml`, `GameEngine/godot/project.godot`

### Explain Repo Eval
- control prompt chars: `161`
- aethyme prompt chars: `111`
- report: `packages/aethyme/docs/reports/evals/20260307-090829-add-explain-repo.md`

### Change-Task Scope
Task: `Update osm_to_hex conversion flow`
- `in_scope_files`:
  - `godot/tools/osm_to_hex.py`
  - `tools/osm_to_hexmap.py`
- `in_scope_areas`:
  - `godot`
  - `tools`

### Signals
- `boundary_clarity`: `71` (`mixed`)
- `entrypoint_clarity`: `58` (`mixed`)
- `config_hygiene`: `61` (`mixed`)
- `hidden_coupling`: `21` (`weak`)
- `parser_visibility`: `82` (`strong`)

## Assessment

### What improved materially
- Cross-file semantic resolution is stricter and more justified.
- Navigation output now reacts to graph quality instead of assuming confidence.
- Explain-repo reference output is now consistent with the graph overview path.

### What did not materially improve on ADD
- Aggregate `hidden_coupling` stayed effectively flat.
- The number of high-confidence semantic edges on this repo did not move enough to change the overall repo-level signal meaningfully.

This means the implementation is stronger and more general, but `ADD` is not the repo that best exposes the gain. That is useful information, not a failure.

## Next Recommended Work
1. Improve language-native semantic resolution further for the dominant repo languages actually present in target repos.
2. Validate the same pass on multiple repo archetypes before tuning ranking again.
