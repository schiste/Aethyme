# Aethyme Deployment Plan

## Purpose

This is the deployment plan for the current repository. It replaces older long-form deployment narratives.

## Deployment Strategy

Deploy in this order:

1. database and cache services
2. core backend (`packages/aethyme`)
3. optional cloud API/web (`packages/aethyme-cloud`) only after backend contract validation

## Environments

### Local Development

Use for all feature work and contract validation.

Minimum expectations:

- PostgreSQL
- optional Redis
- migrations applied
- documented startup commands

### Staging

Use to validate:

- auth
- tenant isolation
- repository indexing
- query flows
- scorecard flow
- rollback procedure

### Production

Only after staging has a verified end-to-end flow and rollback plan.

## Deployment Checklist

### Core Backend

- migrations apply cleanly
- service starts and health checks pass
- auth works with documented token format
- tenant isolation tests pass
- one sample repository can be indexed and queried

### Cloud Package

- package startup is documented
- API connects to the intended backend and database model
- one minimal user flow works end to end
- status documentation matches observed behavior

## Non-Goals

This plan does not assume:

- full enterprise readiness
- broad compliance posture
- every UI feature is production-ready
- every planning document in the repo is current

## Release Rule

If a feature is not verified in the deployed stack, do not describe it as shipped.
