# Query Tests

These tests validate the core graph query loop against a seeded PostgreSQL dataset.

## Source of Truth

- schema rebuild: `tests/conftest.py`
- graph seed dataset: `tests/support/db_seed.py`

The seed creates two orgs and two tenants:

- primary tenant: the main e-commerce style graph used by most tests
- secondary tenant: a small isolated blog graph used for tenant-isolation checks

## Covered Operations

- exact, fuzzy, and hybrid search
- ego graph traversal
- reverse impact analysis
- tenant isolation
- basic performance expectations

## Run

```bash
cd packages/aethyme
python -m pytest tests/queries -v
```

## Extending The Dataset

If a query scenario needs more graph structure, add it to `tests/support/db_seed.py`.
Do not add checked-in SQL fixture files.
