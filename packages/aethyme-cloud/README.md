# Aethyme Cloud

> **Status (2026-07-09): frozen / out of scope.** Aethyme's active direction
> is a local-first agent broker (see
> [`docs/aethyme-local-agent-broker.md`](../../docs/aethyme-local-agent-broker.md)).
> This package is retained as an earlier cloud-oriented scaffold. It is not
> maintained as part of the current product path, is not required by any local
> workflow, and no SaaS/cloud execution is part of broker v0.

Aethyme Cloud is the SaaS-oriented package in this repository.

It contains:

- a FastAPI application
- a Next.js web app
- supporting models, migrations, and worker code

## Role In The Repository

This package should be treated as the cloud consumer layer around the core code-intelligence platform.

It is not the source of truth for overall product status.

## Current State

The package has meaningful implementation, but the historical documentation overstated its maturity.

The practical position is:

- structure exists
- some features are implemented or scaffolded
- the package still needs alignment with the core backend contract
- status claims should stay tied to verified working flows

Current auth position:

- cloud owns user login, registration, and refresh flows
- cloud-issued access tokens are the normal bearer credentials for `packages/aethyme`
- until cloud has a separate tenant model, `organization_id` is mapped to both `org` and `tenant_id` in core-facing access tokens
- local development can mint a cloud-issued token for an existing active user via `POST /api/auth/dev/token`

## Primary Documents

- [../../docs/project-plan.md](../../docs/project-plan.md)
- [status.md](status.md)
- [roadmap.md](roadmap.md)
- [docs/README.md](docs/README.md)

## Immediate Goal

Get the cloud package onto a factual, minimal status line and align it with one working backend-backed flow instead of maintaining a parallel product narrative.
