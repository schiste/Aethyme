# Testing Guide

Last Updated: 2026-05-09

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

## Static Analysis

Three layers, run independently:

```bash
# Style + import hygiene (local, per-file)
.venv/bin/python -m ruff check src/ tests/

# Type-checking
.venv/bin/python -m pyright src/

# Whole-program reachability — catches "deprecated but callable" code
# that ruff F401 can't see (because the imports are USED by helpers
# that are themselves only called by the deprecated entry point).
# Configured at 80%+ confidence so decorator-driven handlers (FastAPI,
# Click) don't false-positive. Config in `pyproject.toml [tool.vulture]`.
.venv/bin/vulture
```

`vulture` was added 2026-05-09 after the Python explore hard-delete
showed that ruff cleared while a 2,500-line subgraph was unreachable.
See `pyproject.toml [tool.vulture]` for the threshold rationale and
the `ignore_names` patterns covering Click commands and FastAPI
handlers.

## Documentation Rule

If a command, endpoint, or flow changes, update the docs in this directory and keep the docs tests green.
