# Core Architecture

Last Updated: 2026-03-06

## Runtime Shape

Aethyme Core is now a split architecture by intent:

1. Python remains the delivery and orchestration layer
2. Rust becomes the deterministic engine layer

The first implementation target for this split is the local-repo proof path:

1. repo intake
2. repository mapping
3. discoverability
4. task-context pack assembly
5. explain-repo evaluation artifacts

## Python Responsibilities

Python currently owns:

- API delivery
- CLI delivery
- auth and tenant enforcement
- scorecard orchestration
- SDK surface
- migration and operational scripts
- local evaluation harnesses
- Rust engine adapters and rendering

## Rust Responsibilities

Rust is the target home for:

- fallback indexing kernels
- graph expansion and traversal kernels
- context-pack assembly
- scope and risk classification
- repository mapping
- deterministic discoverability
- later, policy evaluation

## First Local Runtime Shape

For the first local-repo proof:

1. Python CLI accepts the repo path and task
2. Python executes a built Rust engine binary for mapping, search, neighborhood, and pack assembly
3. Python caches local artifacts by repository snapshot
4. Python renders results and can orchestrate real evaluation runs through a runner contract

This keeps the deterministic core in Rust while avoiding premature service-boundary complexity.

## Rule

Do not rewrite everything into Rust at once.
Move deterministic engine components first.
Keep user-facing orchestration thin in Python until product semantics stabilize.
