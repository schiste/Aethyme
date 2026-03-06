# Aethyme Auth Boundary

## Principle

`packages/aethyme` enforces access. It does not own customer identity flows.

## Cloud Owns

- login and registration
- password and session management
- SSO and OIDC login UX
- org, tenant, team, and user lifecycle
- API key lifecycle UX

## Core Owns

- bearer token verification
- API key verification
- `org_id`, `tenant_id`, and `scopes` contract
- authorization checks on graph, indexing, and scorecard operations
- database isolation and RLS

## Runtime Contract

Protected Aethyme routes expect a bearer credential that resolves to:

- `sub`
- `tenant_id`
- `org`
- `scopes`

Core trusts the issuer and enforces the claims.

Current bridge rule:

- `packages/aethyme-cloud` does not yet model a separate tenant entity
- cloud-issued access tokens currently map `organization_id` to both `org` and `tenant_id`
- this is a temporary compatibility rule until cloud owns a real `Org > Tenant` model

## What Core Does Not Expose

The core API does not provide:

- `/register`
- `/login`
- end-user password flows

Those are SaaS concerns and belong in `packages/aethyme-cloud`.

## Local Development

For local tests and direct operator workflows, scoped tokens can be minted through internal helpers or the cloud-only `POST /api/auth/dev/token` route for an existing active user. That is a development convenience, not a public product surface.
