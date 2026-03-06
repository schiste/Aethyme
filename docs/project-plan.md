# Aethyme Project Plan

## Goal

Make Aethyme Core the verified backend for repository indexing, graph traversal, scorecard analysis, and controlled autofix tooling.

## Product Boundary

- `packages/aethyme` owns the backend product logic
- `packages/aethyme-cloud` consumes that backend and handles SaaS concerns

## Canonical Model

`Platform > Org > Tenant > Repository > Graph`

Working rule:
- `tenant` is the runtime isolation boundary
- `org` owns one or more tenants
- repositories, indexing jobs, graph data, and scorecards live inside tenant scope

## Current Core Surface

### Business Logic
- `packages/aethyme/src/indexer`
- `packages/aethyme/src/indexing`
- `packages/aethyme/src/graph`
- `packages/aethyme/src/models`
- `packages/aethyme/src/scorecard`
- `packages/aethyme/src/autofixers`

### Delivery
- `packages/aethyme/src/api`
- `packages/aethyme/src/cli.py`
- `packages/aethyme/tests/api`
- `packages/aethyme/tests/queries`
- `packages/aethyme/tests/scorecard`
- `packages/aethyme/tests/autofixers`

### Support
- `packages/aethyme/src/auth`
- `packages/aethyme/src/database`
- `packages/aethyme/src/cache`
- `packages/aethyme/src/middleware`
- `packages/aethyme/src/config.py`

## Quarantined Or Removed From Core

The following areas are not part of the active core contract:
- duplicate query stack under `src/queries`
- agent enablement
- guardrails
- telemetry
- efficiency
- old unmounted API surfaces
- old duplicate CLI surfaces

## Verified Product Flow

1. register or login
2. index a repository through the shared indexing service
3. run search
4. run ego graph
5. run impact analysis
6. run scorecard
7. use CLI autofix where needed

## Exit Criteria

The core is in a good state when:
1. docs match the code
2. auth and scope enforcement are explicit
3. indexing, search, ego, impact, and scorecard are verified through real tests
4. there is one runtime stack per responsibility
5. non-core modules are not advertised as active product behavior
