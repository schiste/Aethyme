# Aethyme Core

Aethyme Core owns the backend product loop:

1. authenticate into an org and tenant
2. index a repository into the graph
3. search and traverse the graph
4. run scorecard analysis
5. apply controlled autofixes from the CLI

## Canonical Model

`Platform > Org > Tenant > Repository > Graph`

Runtime isolation is tenant-scoped.

## Auth Boundary

- cloud owns login, registration, sessions, and user lifecycle
- core validates bearer credentials and API keys
- core enforces `org`, `tenant_id`, and `scopes`
- local development can mint a cloud-issued token for an existing user via `POST /api/auth/dev/token`

## Active Surface

### Core Logic
- `src/indexer`
- `src/indexing`
- `src/graph`
- `src/models`
- `src/scorecard`
- `src/autofixers`

### Delivery
- `src/api`
- `src/cli.py`
- `tests/api`
- `tests/queries`
- `tests/scorecard`
- `tests/autofixers`

### Support
- `src/auth`
- `src/database`
- `src/config.py`

## Test Commands

From `packages/aethyme`:

- `make test-unit`
- `make test-integration`
- `make test-full`

Integration tests use `TEST_DATABASE_URL` and currently cover:

- `tests/api`
- `tests/auth`
- `tests/queries`

## Not In Core

The repo no longer treats agent-enablement, guardrails, telemetry, efficiency, or the old duplicate query stack as active core.

## Current Standard

Only document and defend the verified path:

- trusted bearer token or API key required
- `POST /api/v1/index/repositories`
- `POST /api/v1/search/`
- `POST /api/v1/ego/`
- `POST /api/v1/impact/`
- `POST /api/v1/scorecard/scan`

Repository indexing is defined once in [`src/indexing/service.py`](/Users/christophehenner/Downloads/Repositories/Aethyme/packages/aethyme/src/indexing/service.py) through `RepositoryIndexRequest` and `run_indexing()`. The API and CLI both consume that same contract.

## Primary Docs

- [../../docs/project-plan.md](/Users/christophehenner/Downloads/Repositories/Aethyme/docs/project-plan.md)
- [roadmap.md](/Users/christophehenner/Downloads/Repositories/Aethyme/packages/aethyme/roadmap.md)
- [docs/README.md](/Users/christophehenner/Downloads/Repositories/Aethyme/packages/aethyme/docs/README.md)
