# Security Overview

Last Updated: 2026-03-06

## Core Security Model

Aethyme Core relies on:

- trusted bearer tokens or scoped API keys
- tenant-scoped authorization checks
- row-level security in PostgreSQL
- narrow API surface around the active core loop

## Boundary Rule

Cloud owns identity lifecycle. Core owns access enforcement.

## Operational Rule

Do not add new privileged routes or alternate auth paths without updating:

- [`../architecture/auth-boundary.md`](../architecture/auth-boundary.md)
- [`../reference/api.md`](../reference/api.md)
- the relevant auth and API tests
