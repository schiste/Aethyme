# Aethyme Tests

All suites are local-first: no PostgreSQL or external services are required.

**Status: dev-only, and temporary.** `packages/aethyme` ships no Python
(`src/` was deleted 2026-08-01, python-retirement Phase 6). What lives
here is an implementation-blind harness that drives the built `aethyme`
binary as a subprocess — it caught real breakages through every phase of
the migration, which is why it outlived the code it started as a test of.
It ports to Rust integration tests in a follow-up session, at which point
this directory, `pyproject.toml`, and the last Python in the package go
away together (operator decision 5, 2026-08-01).

## Suites

- `tests/local` — end-to-end local workflow, CLI, engine cache, enhance, hygiene checks (run in CI).
  Includes the implementation-blind `ai-ready` and `autofix` suites that replaced
  `tests/scorecard` and `tests/autofixers` when those commands went native
- `tests/indexing` — language/skill indexing behavior
- `tests/docs` — documentation link and example validation
- `tests/support` — fixtures and the engine build-if-stale helper
  (`engine_binary.py`, formerly `src/indexing/engine.py`)

`tests/contracts` was removed on 2026-08-01 with `src/contracts/`: it
tested Python dataclasses nothing else consumed. Schema stability now
lives with the Rust types that emit the schemas.

Run everything (the venv holds pytest only — there is no package to
install):

```bash
python -m pytest tests/local tests/indexing tests/docs -q
```

## Data Model

- [repo_builders.py](support/repo_builders.py) creates temporary repositories on demand

Do not add checked-in fake repos or SQL fixture dumps back into the active test path.
