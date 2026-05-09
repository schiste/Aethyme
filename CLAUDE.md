# Aethyme Development Guide

## Repository Structure

Monorepo with primary package at `packages/aethyme/`.

- **Python** (`src/`): CLI, API, auth, graph store, indexing, scorecard, eval harnesses
- **Rust** (`rust/crates/aethyme-engine/`): deterministic repo mapping, graph navigation, context-pack assembly
- Python shells out to the Rust engine binary via `src/indexing/engine.py`

## Cardinal Rules

1. **All evaluations run against Playground repositories, never against Aethyme itself.**
2. **Never modify tools, engine, pipeline, or skills to improve eval scores.** Evals are diagnostics, not targets. If an eval reveals a weakness, fix the generic system — never add task-type-specific accommodations, special-case heuristics targeting known eval scenarios, or output formatting that matches scorer expectations. Ask: "Would I make this change if the eval didn't exist?" If no, do not make it.
3. **Audit cross-process consumers before any CLI rename or delete.** Static analysis does not see shell wrappers, deployed skill scripts, hooks, or CI invocations. Before deleting/renaming any Python `cli.py` command or Rust binary subcommand, grep [`packages/aethyme/docs/architecture/cross-process-consumers.md`](packages/aethyme/docs/architecture/cross-process-consumers.md) for callers and update each in the same commit (or accept the breakage with explicit reasoning). If you discover an unlisted consumer mid-migration, add it to the registry — that's how it stays complete.

See [`packages/aethyme/docs/guides/eval-protocol.md`](packages/aethyme/docs/guides/eval-protocol.md) for the full eval protocol including detailed examples of forbidden vs allowed changes.
