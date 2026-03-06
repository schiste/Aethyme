# Aethyme Core Roadmap

## Goal

Make Aethyme Core a small, honest, verified backend for repository indexing, graph queries, scorecard analysis, and controlled autofix tooling.

## Current Core

The supported product loop is:

1. auth
2. repository indexing
3. search / ego / impact
4. scorecard
5. CLI autofix

## Execution Order

### 1. Keep The Model Stable
- keep `Platform > Org > Tenant > Repository > Graph`
- keep tenant as the runtime isolation boundary
- keep one auth path and one schema direction

### 2. Defend The Core Loop
- keep CLI and API indexing on the same service path
- keep query behavior inside `src/graph/store.py`
- keep API proof coverage for auth -> index -> search -> ego -> impact -> scorecard

### 3. Tighten Secondary Surfaces
- keep autofixers limited to the fixers that are implemented and testable
- do not reintroduce approval, guardrails, telemetry, agent-enablement, or efficiency until they are wired to the real loop

### 4. Expand Only After Proof
After the core loop is stable:
- improve fallback indexing quality
- improve autofix coverage
- improve API ergonomics and SDKs

## Done Means

1. the docs match the code
2. the core loop is verified through the API and tests
3. the codebase has no competing runtime stacks for the same job
