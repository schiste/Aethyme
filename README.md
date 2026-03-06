# Aethyme

Aethyme is a monorepo with two packages:

- `packages/aethyme`: the core code graph engine, API, CLI, and repository-improvement logic
- `packages/aethyme-cloud`: the SaaS shell built around that core

## Source Of Truth

Use these files first:

- [docs/project-plan.md](/Users/christophehenner/Downloads/Repositories/Aethyme/docs/project-plan.md)
- [packages/aethyme/README.md](/Users/christophehenner/Downloads/Repositories/Aethyme/packages/aethyme/README.md)
- [packages/aethyme/roadmap.md](/Users/christophehenner/Downloads/Repositories/Aethyme/packages/aethyme/roadmap.md)
- [packages/aethyme-cloud/README.md](/Users/christophehenner/Downloads/Repositories/Aethyme/packages/aethyme-cloud/README.md)
- [packages/aethyme-cloud/status.md](/Users/christophehenner/Downloads/Repositories/Aethyme/packages/aethyme-cloud/status.md)
- [packages/aethyme-cloud/roadmap.md](/Users/christophehenner/Downloads/Repositories/Aethyme/packages/aethyme-cloud/roadmap.md)

Historical planning/status paperwork was pruned. If a claim is not represented in the files above or in code, do not treat it as current.

## Working Goal

Build a stable code-intelligence platform that:

1. indexes repositories into a queryable graph
2. supports search, relationship, and impact queries
3. exposes that graph safely through authenticated APIs
4. layers scorecards, autofixers, and AI workflows on top
5. supports a multi-tenant SaaS UX once the backend contract is stable

## Near-Term Priorities

1. Stabilize the core backend and database model
2. Fix auth, tenant isolation, and API consistency
3. Prove one end-to-end path: register -> index -> query -> scorecard
4. Align the cloud package with the core instead of evolving in parallel
