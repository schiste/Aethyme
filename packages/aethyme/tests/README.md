# Aethyme Tests

All suites are local-first: no PostgreSQL or external services are required.

## Suites

- `tests/local` — end-to-end local workflow, CLI, engine cache, enhance, hygiene checks (run in CI)
- `tests/indexing` — language/skill indexing behavior
- `tests/scorecard` — scorecard engine and detectors
- `tests/autofixers` — fixers, safety, and patch handling
- `tests/contracts` — cross-process contract and schema stability
- `tests/docs` — documentation link and example validation

Run everything:

```bash
python -m pytest tests/local tests/indexing tests/scorecard tests/autofixers tests/contracts tests/docs -q
```

## Data Model

- [repo_builders.py](support/repo_builders.py) creates temporary repositories on demand

Do not add checked-in fake repos or SQL fixture dumps back into the active test path.
