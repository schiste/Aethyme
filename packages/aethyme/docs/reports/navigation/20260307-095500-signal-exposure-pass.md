# Signal Exposure Pass

Last Updated: 2026-03-07

Date: 2026-03-07

## Purpose
Expose the first five graphability and navigability signals directly in:
- `repo inspect`
- `graph overview`

## Implemented
- Added a Rust signal evaluator in `signals.rs`
- Exposed `signals` in the repository inspect JSON payload
- Exposed `signals` in the graph overview JSON payload
- Added CLI human-readable rendering for the same signal block
- Added local regression coverage for both surfaces

## Signals Exposed
- `boundary_clarity`
- `entrypoint_clarity`
- `config_hygiene`
- `hidden_coupling`
- `parser_visibility`

Each signal now returns:
- `score`
- `level`
- `evidence`

## Validation
- `cargo test`: `18 passed`
- `pytest tests/local tests/docs -q`: `23 passed`
- `.venv/bin/ruff check src tests`: passed

## ADD Result
The new signal block on `graph overview /Users/christophehenner/Downloads/Repositories/ADD --json-output` is:

```json
{
  "boundary_clarity": {
    "score": 71,
    "level": "mixed",
    "evidence": [
      "cross-area semantic edges: 761/8592",
      "source files with area assignment: 5/5",
      "generic source file names: 0"
    ]
  },
  "entrypoint_clarity": {
    "score": 58,
    "level": "mixed",
    "evidence": [
      "direct code entrypoint edges: 1",
      "configs with entrypoints: 1",
      "areas with ambiguous entrypoints: 0"
    ]
  },
  "config_hygiene": {
    "score": 61,
    "level": "mixed",
    "evidence": [
      "operational configs: 3",
      "linked configs: 3/3",
      "duplicate config families: 0"
    ]
  },
  "hidden_coupling": {
    "score": 20,
    "level": "weak",
    "evidence": [
      "low-confidence semantic edges: 7296/7367",
      "high-confidence semantic edges: 0/7367",
      "cross-area semantic edges: 3/7367"
    ]
  },
  "parser_visibility": {
    "score": 82,
    "level": "strong",
    "evidence": [
      "supported source files: 4/5",
      "source files with semantic extraction: 4/5",
      "total extracted functions/classes: 64"
    ]
  }
}
```

## Interpretation
- `boundary_clarity`: usable but not strong yet
- `entrypoint_clarity`: there is one clear direct entrypoint path, but the repo still exposes limited explicit runtime ownership
- `config_hygiene`: operational configs are fairly clean on this repo
- `hidden_coupling`: this is the biggest current weakness; semantic extraction still relies heavily on low-confidence inferred edges
- `parser_visibility`: strong for the core code-bearing parts of the repo

## Immediate implication
The next highest-value graph improvement is not more ranking work.
It is improving semantic edge confidence so hidden coupling drops and task navigation can trust more of the graph.
