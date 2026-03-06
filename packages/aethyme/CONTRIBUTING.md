# Contributing To Aethyme

## Development Standard

Work against the current core flow only:

1. authenticate or seed a tenant
2. index a repository
3. run search, ego, or impact
4. run scorecard
5. keep tests and docs aligned with that flow

Do not add speculative status docs, sprint reports, or checked-in fake fixture repositories.

## Local Setup

```bash
cd packages/aethyme
python3 -m venv .venv
. .venv/bin/activate
pip install -e '.[dev]'
docker compose -f docker-compose.dev.yml up -d postgres redis
export DATABASE_URL='postgresql://aethyme:dev_password_change_me@localhost:5432/aethyme_dev'
export REDIS_URL='redis://localhost:6379/0'
bash scripts/migrate.sh
```

## Run The Core Checks

```bash
. .venv/bin/activate
pytest tests/queries tests/scorecard -q
python -m src.cli index . --name aethyme
```

## Documentation Rule

If behavior changes, update one of these instead of adding a new status file:

- `README.md`
- `roadmap.md`
- `docs/getting-started/quickstart.md`
- `docs/reference/api.md`
- `docs/reference/cli.md`
- `tests/README.md`
