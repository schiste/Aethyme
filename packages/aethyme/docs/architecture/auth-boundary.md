# Auth Boundary

Last Updated: 2026-03-06

## Principle

`packages/aethyme` enforces access. It does not own customer identity flows.

## Cloud Owns

- login and registration
- password and session management
- org, tenant, team, and user lifecycle
- API key lifecycle UX

## Core Owns

- bearer token verification
- API key verification
- `org`, `tenant_id`, and `scopes` enforcement
- authorization checks on indexing, graph queries, and scorecard operations
- tenant-scoped database isolation

## Required Claims

Protected core routes expect a credential that resolves to:

- `sub`
- `org`
- `tenant_id`
- `scopes`

## Local Development

For development only, cloud may mint a token for an existing active user through `POST /api/auth/dev/token`.

That route is a convenience bridge. It does not change the core boundary.
