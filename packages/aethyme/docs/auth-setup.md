# Auth Setup

Last Updated: 2026-03-06

Aethyme Core does not provide login or registration routes.

## Core Expectation

Protected routes require a trusted bearer credential or scoped API key.

Expected claims:

- `sub`
- `org`
- `tenant_id`
- `scopes`

## Where Identity Comes From

- normal user identity comes from `packages/aethyme-cloud`
- machine access can use scoped API keys
- local development can use test helpers or the cloud dev token route

See [`architecture/auth-boundary.md`](architecture/auth-boundary.md) for the canonical split.
