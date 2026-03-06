# Testing Guide

Last Updated: 2026-03-06

## Test Tiers

### Unit
Does not require PostgreSQL.

```bash
cd packages/aethyme
. .venv/bin/activate
make test-unit
```

### Integration
Requires PostgreSQL and uses `TEST_DATABASE_URL`.

```bash
cd packages/aethyme
. .venv/bin/activate
export TEST_DATABASE_URL='postgresql://aethyme:dev_password_change_me@localhost:5432/aethyme_test'
make test-integration
```

### Full
Runs unit and integration coverage together.

```bash
cd packages/aethyme
. .venv/bin/activate
export TEST_DATABASE_URL='postgresql://aethyme:dev_password_change_me@localhost:5432/aethyme_test'
make test-full
```

## What The Suite Proves

The active test model covers:

- auth enforcement
- indexing service behavior
- search
- ego graph
- impact analysis
- scorecard
- local autofixer behavior

## Test Support

- seeded DB support: [`../../tests/support/db_seed.py`](../../tests/support/db_seed.py)
- token helpers: [`../../tests/support/auth_db.py`](../../tests/support/auth_db.py)
- generated repository builders: [`../../tests/support/repo_builders.py`](../../tests/support/repo_builders.py)

## Documentation Rule

If a command, endpoint, or flow changes, update the docs in this directory and keep the docs tests green.
