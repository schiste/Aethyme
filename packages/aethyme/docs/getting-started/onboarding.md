# Aethyme Onboarding

`packages/aethyme` owns the backend product core.

## Business Loop

1. auth into org and tenant
2. index a repository
3. search or traverse the graph
4. run scorecard analysis
5. apply autofixes from the CLI when needed

## Important Paths

| Path | Purpose |
|------|---------|
| `src/indexer` | raw indexers and graph building |
| `src/indexing` | shared indexing service and freshness helpers |
| `src/graph` | graph persistence and query execution |
| `src/scorecard` | repository assessment logic |
| `src/autofixers` | CLI-driven safe fix tooling |
| `src/api` | FastAPI surface |
| `src/auth` | JWT and API key handling |
| `tests/api` | API proof for the core loop |
| `tests/queries` | seeded graph query coverage |
| `tests/scorecard` | scorecard coverage |
| `tests/autofixers` | fixer and safety coverage |

## First Commands

```bash
cd packages/aethyme
python3 -m venv .venv
. .venv/bin/activate
pip install -e '.[dev]'
docker compose -f docker-compose.dev.yml up -d postgres redis
export DATABASE_URL='postgresql://aethyme:dev_password_change_me@localhost:5432/aethyme_dev'
export TEST_DATABASE_URL='postgresql://aethyme:dev_password_change_me@localhost:5432/aethyme_test'
bash scripts/migrate.sh
pytest tests/api tests/queries tests/scorecard tests/autofixers -q
```

## Rule

If work does not strengthen the supported core loop, challenge whether it belongs in Aethyme Core right now.
