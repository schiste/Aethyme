# Rust Transition

Last Updated: 2026-03-06

## Decision

Aethyme Core will move as much of the deterministic engine layer to Rust as is
reasonable, while keeping API, CLI, auth enforcement, scorecard orchestration,
and SDKs in Python for now.

This is an explicit architectural decision.

## Why

The core vision requires:

- deterministic behavior
- minimal context by default
- explicit scope and risk handling
- strong graph and retrieval primitives
- later, permission-aware execution

Those engine properties fit Rust well.

## What Moves First

Phase 1 Rust targets:

1. repository mapping
2. discoverability kernels
3. context-pack types
4. context-pack assembly pipeline
5. scope and risk types
6. indexing kernels
7. graph expansion kernels

## Current First Proof

The first practical runtime shape is local and repo-first:

1. Python CLI accepts a local repository path and task
2. Python invokes the Rust engine binary for mapping, search, neighborhood, and pack assembly
3. Python renders the result and builds local evaluation artifacts

That path currently covers:

- `repo ingest`
- `repo inspect`
- `query symbol`
- `query deps`
- `query impact`
- `task pack`
- `task explain`
- `eval explain-repo`

This is the correct first proof because it validates the deterministic engine boundary without waiting for any SaaS surface.

## What Stays In Python

For now, Python continues to own:

- API routes and FastAPI app
- CLI commands
- auth and tenant enforcement
- scorecard orchestration
- SDKs
- migration tooling

## Migration Rule

Do not do a full rewrite first.

Move engine components only when they have:

- clear boundaries
- stable inputs and outputs
- measurable value for determinism or performance

## Current Workspace

The Rust workspace lives under:

- `packages/aethyme/rust`

The first crate is:

- `crates/aethyme-engine`

That crate is the starting point for deterministic engine structures.
