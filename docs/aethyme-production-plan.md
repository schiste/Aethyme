# Aethyme Production Plan

## Purpose

This is the concise production plan for the repository as it exists today.

For broader project sequencing, use [project-plan.md](project-plan.md).

## Production Goal

Reach a state where the core product can be operated predictably for a small number of tenants and repositories.

## Required Before Any Production Claim

1. The backend starts cleanly in a documented environment
2. The database schema in use is unambiguous
3. Auth and tenant isolation are consistent across request paths
4. One end-to-end path is verified:
   - authenticate
   - index a repository
   - query the graph
   - run scorecard
5. Operational basics exist:
   - migrations
   - logs
   - health checks
   - backup/restore procedure
   - rollback procedure

## Current Gaps

- competing data/auth/tenant models across the repository
- incomplete confidence in tenant isolation and auth behavior
- cloud package status overstated relative to repository reality
- documentation previously mixed aspiration with present state

## Production Work Order

### 1. Stabilize The Core

- pick the authoritative schema path
- remove or quarantine incompatible alternatives
- make the core API and CLI paths reliable

### 2. Harden Security And Isolation

- unify JWT and API key semantics
- ensure tenant context cannot leak across pooled connections
- add request-level tests for isolation-critical endpoints

### 3. Prove Operations

- document startup and migration path
- verify health, readiness, and backup procedures
- define a small production envelope instead of broad readiness claims

### 4. Publish A Narrow Release

Release only what is verified.

Recommended first release scope:

- backend and CLI
- indexing
- search
- ego graph
- impact analysis
- scorecard

Cloud UX should only be included if it is validated against the same backend contract.
