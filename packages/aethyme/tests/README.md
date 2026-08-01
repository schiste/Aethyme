# Aethyme Tests

All suites are local-first: no PostgreSQL or external services are required.

## Suites

- `tests/local` — end-to-end local workflow, CLI, engine cache, enhance, hygiene checks (run in CI).
  Includes the implementation-blind `ai-ready` and `autofix` suites that replaced
  `tests/scorecard` and `tests/autofixers` when those commands went native
- `tests/indexing` — language/skill indexing behavior
- `tests/contracts` — cross-process contract and schema stability
- `tests/docs` — documentation link and example validation

Run everything:

```bash
python -m pytest tests/local tests/indexing tests/contracts tests/docs -q
```

## Data Model

- [repo_builders.py](support/repo_builders.py) creates temporary repositories on demand

Do not add checked-in fake repos or SQL fixture dumps back into the active test path.
