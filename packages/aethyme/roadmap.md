# Aethyme Core Roadmap

Last Updated: 2026-03-06

## Goal

Keep Aethyme Core small, verified, and technically honest while moving its
engine layers toward Rust.

## Current Focus

1. establish the Rust engine boundary
2. prove the first local-repo path end to end
3. improve fallback indexing quality
4. improve graph edge quality for ego and impact
5. build deterministic context-pack assembly
6. keep scorecard useful without overstating it

## Execution Order

### 1. Rust Engine First
Move these first:
- repository mapping
- discoverability kernels
- context-pack data structures and assembly
- scope and risk types
- indexing kernels
- graph expansion kernels

Keep these in Python for now:
- API
- CLI
- auth enforcement
- scorecard orchestration
- SDKs

### 2. Defend The Core Loop
- verified local flow for repo ingest -> inspect -> query -> task pack -> explain-repo eval
- shared indexing contract for API and CLI
- graph store as the canonical query runtime until Rust kernels replace hot paths
- verified API flow for index -> search -> ego -> impact -> scorecard

### 3. Improve Graph Correctness
- better import and symbol resolution in fallback mode
- better cross-file edge construction
- more indexing and graph-semantics tests

### 4. Tighten Secondary Features
- keep scorecard tied to real repo signals
- keep autofixers as local tooling unless explicitly promoted

## Done Means

1. docs match code
2. one runtime stack per job
3. test suite proves the active flow
4. Rust owns the deterministic engine layers that benefit from it
5. Python remains a thin delivery and orchestration layer
