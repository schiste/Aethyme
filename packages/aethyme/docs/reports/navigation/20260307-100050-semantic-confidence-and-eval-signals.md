# Semantic Confidence And Eval Signals Pass

Last Updated: 2026-03-07

## Purpose
1. improve semantic edge confidence in the repograph
2. expose the repo signal block directly in benchmark reports

## Implemented
- Tightened semantic edge generation in the Rust code pass:
  - local function calls now emit high-confidence call edges
  - imported-file calls now emit stronger confidence edges
  - broad cross-file reference edges are only kept when backed by local or imported structure
  - low-value ambiguous reference edges were removed
- Added the `signals` block to:
  - explain-repo eval results
  - navigation-ctf eval results
  - markdown eval reports

## Files Changed
- `packages/aethyme/rust/crates/aethyme-engine/src/passes/code.rs`
- `packages/aethyme/src/eval/explain_repo.py`
- `packages/aethyme/src/eval/navigation_ctf.py`
- `packages/aethyme/src/eval/report.py`
- `packages/aethyme/tests/local/test_local_workflow.py`

## Validation
- `cargo test`: `18 passed`
- `pytest tests/local tests/docs -q`: `23 passed`
- `.venv/bin/ruff check src tests`: passed

## ADD Signal Delta
Before this pass, `hidden_coupling` on `ADD` was:
- score: `20`
- high-confidence semantic edges: `0/7367`

After this pass, `hidden_coupling` on `ADD` is:
- score: `21`
- high-confidence semantic edges: `49/7367`
- low-confidence semantic edges: `7283/7367`

This is a real improvement, but still a small one.

## Interpretation
The signal block now confirms the next real engine problem:
- semantic extraction still relies overwhelmingly on low-confidence inferred edges
- the graph is usable, but still not semantically trustworthy enough for stronger scope and impact claims

The next high-value move is not more ranking polish.
It is better language-aware semantic relation resolution.
