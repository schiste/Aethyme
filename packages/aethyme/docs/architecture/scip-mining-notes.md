# SCIP Integration — Mined Knowledge (for #28)

Last Updated: 2026-07-13

Distilled from `src/indexer/` (scip_wrapper.py, graph_builder.py,
fallback_indexer.py) before its removal — the Gen-0 lineage's one
genuinely reusable asset. Recover the full sources at git ref
`759d041~1:packages/aethyme/src/indexer/` if implementation detail is
ever needed.

## Why this matters

Issue #28 (restore cross-file caller relations in the fragment pipeline)
has two implementation options: grow the native Rust parsers until they
resolve cross-file references, or shell out to SCIP indexers the way the
broker shells out to git. SCIP emits exactly the reference data the
Gen-2 schema's unpopulated `Calls`/`References` edges need. This
document is the head start for the SCIP option.

## Operational knowledge (hard-won)

- **Binaries**: `scip-python` (Python), `scip-typescript` (TS/JS/TSX/JSX
  — one binary for all four). Availability probe: `<binary> --version`
  with a 5s timeout; treat non-zero or missing as "fallback mode".
- **Invocation**: `<binary> index --output <tmp>.scip --project-name <name>`
  with `cwd` = repo root. Use a generous timeout (the old wrapper used
  300s; MediaWiki-sized repos need it).
- **TypeScript gotcha**: `scip-typescript` refuses repos without a
  `tsconfig.json`. The wrapper auto-created a minimal one
  (`target: ES2020`, `allowJs: true`) — required for JS-only repos.
- **Output parsing**: the old pipeline consumed a JSONL rendering of the
  SCIP protobuf (one document per line, `documents[].occurrences[]`).
  A Rust implementation should read the protobuf directly
  (`scip` crate exists) rather than round-tripping through JSON.

## The mapping that worked (graph_builder.py)

- **Definition detection**: `occurrence.symbol_roles & 1` (SCIP role bit
  1 = Definition). Everything else is a reference.
- **Node identity**: SCIP symbol strings are globally unique and
  descriptor-suffixed — `(`…`)` ⇒ function/method, `#` ⇒ class-like,
  trailing `.` ⇒ term. The builder kept a `symbol → definition-node`
  map from the definition pass.
- **Edge derivation** (the part #28 wants):
  - reference occurrence whose symbol has a known definition, where the
    referencing site is function-like ⇒ `INVOKE` edge (caller → callee)
    — this maps directly onto Gen-2 `Calls`.
  - file → its definitions ⇒ `CONTAIN` (Gen-2 `Contains`, already have).
  - import-shaped references ⇒ `IMPORT` (already have).
  - Dedup on `(from, to, type)` — SCIP emits an occurrence per call
    *site*; the old builder collapsed to one edge per pair. Gen-2 could
    instead keep per-site occurrences as edge attributes (call counts).
- **What SCIP gives that the native parsers currently don't**: resolved
  cross-file symbol identity. The native fragment linker resolves
  imports; SCIP resolves *usages* (calls, attribute access) through the
  language's own type/binding machinery.

## Fallback indexer (not worth porting)

`fallback_indexer.py` was a regex/AST heuristic layer for when SCIP
binaries were absent. The Rust fragment producers already are that
fallback, and better. Nothing to mine beyond the reminder that SCIP
availability must be optional (agents' machines won't all have the
binaries — same "worktree-independent toolchain" rule as gates).

## Recommendation recorded for #28

Evaluate SCIP-as-subprocess first (mirrors the git service layer
pattern: external binary, one wrapper module, graceful degradation),
against a spike of native tree-sitter call-resolution. Decision criteria:
per-language coverage (SCIP: py/ts free; rust/go need scip-rust/scip-go),
cold index time on MediaWiki-scale playgrounds, and whether per-site
occurrence data (call counts) is worth carrying into edge attributes.
The navigation_ctf / explain_repo evals are the acceptance harness
either way.
