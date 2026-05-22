# Aethyme Tests

The suite is split into unit and integration paths.

## Unit
- no PostgreSQL required
- run with `make test-unit`
- current examples:
  - `tests/autofixers`
  - `tests/scorecard`
  - `tests/indexing`
  - `tests/docs`

## Integration
- requires `TEST_DATABASE_URL`
- run with `make test-integration`
- current integration directories:
  - `tests/api`
  - `tests/auth`
  - `tests/queries`

## Full
- run everything with `make test-full`

## Data Model
- integration tests rebuild `aethyme_test` from migrations
- [db_seed.py](support/db_seed.py) seeds canonical org, tenant, repository, node, and edge data
- [repo_builders.py](support/repo_builders.py) creates temporary repositories on demand

Do not add checked-in fake repos or SQL fixture dumps back into the active test path.
