# Testing Guide

Last Updated: 2026-07-29

## Test Tiers

### Unit
Does not require PostgreSQL.

```bash
cd packages/aethyme
. .venv/bin/activate
python -m pytest tests/local tests/scorecard -q
```

### Integration
Requires PostgreSQL and uses `TEST_DATABASE_URL`.

```bash
cd packages/aethyme
. .venv/bin/activate
WORKER_ID="${AETHYME_TEST_DB_SUFFIX:-${AETHYME_GATE_WORKER_ID:-${USER:-local}_$$}}"
export TEST_DATABASE_URL="postgresql://aethyme:dev_password_change_me@localhost:5432/aethyme_test_${WORKER_ID}"
make test-integration
```

In broker gates, prefer deriving the database name from
`AETHYME_TEST_DB_SUFFIX`; the gate runner sets it to a session/process-scoped
value before launching the command.

### Full
Runs unit and integration coverage together.

```bash
cd packages/aethyme
. .venv/bin/activate
WORKER_ID="${AETHYME_TEST_DB_SUFFIX:-${AETHYME_GATE_WORKER_ID:-${USER:-local}_$$}}"
export TEST_DATABASE_URL="postgresql://aethyme:dev_password_change_me@localhost:5432/aethyme_test_${WORKER_ID}"
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

- generated repository builders: [`../../tests/support/repo_builders.py`](../../tests/support/repo_builders.py)
  (the seeded-DB and token helpers were removed 2026-07-13 with the
  Gen-0 PostgreSQL lineage)

## Static Analysis

Three layers, run independently:

```bash
# Style + import hygiene (local, per-file)
python -m ruff check src/ tests/

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
