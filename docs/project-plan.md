# Aethyme Core Plan

Last Updated: 2026-03-06

> **Repositioning note (2026-07-09):** Aethyme's product direction is now a
> **local-first agent broker** for high-concurrency AI development, with the
> graph engine as a supporting repo-intelligence service. The tenant/API
> "Verified Flow" below describes the earlier cloud lineage and is no longer
> the priority path. See
> [`aethyme-local-agent-broker.md`](aethyme-local-agent-broker.md) for the
> current direction. The "First Local Proof" section below remains accurate
> for the graph engine.

## Goal

Keep `packages/aethyme` as a small, verified backend for:

1. repository indexing
2. graph search and traversal
3. scorecard analysis
4. controlled CLI autofix tooling
5. deterministic agent navigation primitives

## Product Boundary

- `packages/aethyme` owns core logic and scoped enforcement
- `packages/aethyme-cloud` owns login, registration, sessions, and SaaS lifecycle

## Canonical Model

`Platform > Org > Tenant > Repository > Graph`

Working rules:

- `tenant` is the runtime isolation boundary
- `org` owns one or more tenants
- repositories, nodes, edges, indexing jobs, and scorecards are tenant-scoped
- core accepts trusted credentials carrying `org`, `tenant_id`, and `scopes`

## Language Direction

Aethyme Core is moving toward a split architecture:

- Rust for deterministic engine components
- Python for delivery, orchestration, and current product-facing flows

### Rust First Targets
- indexing kernels
- graph expansion kernels
- context-pack assembly
- risk and scope classification
- later, policy evaluation

### Python Retained Targets
- API
- CLI
- auth enforcement
- scorecard orchestration
- SDKs
- migration and product experimentation layers

## Core Surface

### Business Logic
- `packages/aethyme/src/indexer`
- `packages/aethyme/src/indexing`
- `packages/aethyme/src/graph`
- `packages/aethyme/src/models`
- `packages/aethyme/src/scorecard`
- `packages/aethyme/src/autofixers`
- `packages/aethyme/rust`

### Delivery
- `packages/aethyme/src/api`
- `packages/aethyme/src/cli.py`
- `packages/aethyme/sdk/python`

### Verification
- `packages/aethyme/tests/api`
- `packages/aethyme/tests/queries`
- `packages/aethyme/tests/scorecard`
- `packages/aethyme/tests/autofixers`
- `packages/aethyme/tests/indexing`

## Verified Flow

1. present a trusted bearer token or API key
2. register and index a repository through the shared indexing service
3. run search
4. run ego graph
5. run impact analysis
6. run scorecard
7. use CLI autofix where appropriate

## First Local Proof

Before any SaaS or multi-repo concerns, the first honest proof path is local:

1. ingest one repository from a filesystem path
2. build a deterministic repository map
3. query symbol and dependency neighborhoods locally
4. emit a deterministic task-context pack for `Explain this repo`
5. generate the local explain-repo evaluation artifacts

Core commands for that path:

- `aethyme repo ingest /path/to/repo`
- `aethyme repo inspect /path/to/repo --json-output`
- `aethyme query symbol /path/to/repo main`
- `aethyme query deps /path/to/repo src/main.py`
- `aethyme task pack --repo /path/to/repo --task "Explain this repo" --json-output`
- `aethyme task explain --repo /path/to/repo`
- `aethyme eval explain-repo --repo /path/to/repo --json-output`

## Current Priorities

1. define and stabilize the Rust engine boundary
2. harden the local repo mapping and discoverability path
3. build deterministic task-context packs
4. add honest evaluation against a control condition
5. keep API and CLI on the same contracts
6. keep autofixers narrow until explicitly productized

## Not In Core

Do not document or market these as active core behavior:

- customer login or registration
- telemetry platform claims
- guardrails platform claims
- agent-enablement platform claims
- duplicate query stacks
- speculative ops maturity claims
