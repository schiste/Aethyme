# Aethyme Quick Start

## 1. Install

```bash
cd packages/aethyme
python3 -m venv .venv
. .venv/bin/activate
pip install -e '.[dev]'
```

## 2. Start Local Services

```bash
docker compose -f docker-compose.dev.yml up -d postgres redis
export DATABASE_URL='postgresql://aethyme:dev_password_change_me@localhost:5432/aethyme_dev'
export REDIS_URL='redis://localhost:6379/0'
```

## 3. Migrate

```bash
bash scripts/migrate.sh
```

## 4. Run The API

```bash
bash scripts/start-api.sh
```

Docs: `http://localhost:8001/docs`

## 5. Provide A Trusted Credential

Core does not expose login or registration routes.

- in normal operation, use a bearer token issued by `packages/aethyme-cloud`
- for machine access, use a scoped API key
- for direct local verification, use the test helpers under [`tests/support/auth_db.py`](/Users/christophehenner/Downloads/Repositories/Aethyme/packages/aethyme/tests/support/auth_db.py) or run the API proof test

```bash
export TOKEN="<trusted-bearer-token>"
```

## 6. Index A Repository Through The API

```bash
curl -s -X POST http://localhost:8001/api/v1/index/repositories \
  -H "Authorization: Bearer $TOKEN" \
  -H 'Content-Type: application/json' \
  -d '{
    "repository_path": "/absolute/path/to/repo",
    "repository_name": "example-repo",
    "languages": ["python"],
    "use_fallback": true,
    "clear_existing": true
  }'
```

## 7. Query The Graph

```bash
curl -s -X POST http://localhost:8001/api/v1/search/ \
  -H "Authorization: Bearer $TOKEN" \
  -H 'Content-Type: application/json' \
  -d '{"query": "GraphStore", "limit": 10, "search_type": "hybrid"}'
```

## 8. Run Scorecard

```bash
curl -s -X POST http://localhost:8001/api/v1/scorecard/scan \
  -H "Authorization: Bearer $TOKEN" \
  -H 'Content-Type: application/json' \
  -d '{"repository_id": "<repo-id>"}'
```

## 9. Run The Verified Test Slice

```bash
export TEST_DATABASE_URL='postgresql://aethyme:dev_password_change_me@localhost:5432/aethyme_test'
pytest tests/api tests/queries tests/scorecard tests/autofixers -q
```
