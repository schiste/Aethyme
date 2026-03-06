# Aethyme Testing Guide

This guide covers the current Aethyme core verification path only.

## Test Tiers

From `packages/aethyme`:

```bash
make test-unit
make test-integration
make test-full
```

- `test-unit`: no PostgreSQL required
- `test-integration`: requires `TEST_DATABASE_URL`
- `test-full`: runs both

## Local API Checks

Start local dependencies:

```bash
make dev
bash scripts/start-api.sh
```

The API is served on `http://localhost:8001`.

### Health

```bash
curl http://localhost:8001/health/
curl http://localhost:8001/health/ready
curl http://localhost:8001/health/detailed | jq .
```

## Core Product Loop

### 1. Register

```bash
curl -X POST http://localhost:8001/api/v1/auth/register \
  -H "Content-Type: application/json" \
  -d '{
    "email": "test@example.com",
    "password": "test12345",
    "org_name": "Example Org",
    "tenant_name": "default"
  }' | jq .
```

Export the returned token:

```bash
export TOKEN="<access_token>"
```

### 2. Inspect Current User

```bash
curl http://localhost:8001/api/v1/auth/me \
  -H "Authorization: Bearer $TOKEN" | jq .
```

### 3. Register and Index a Repository

```bash
curl -X POST http://localhost:8001/api/v1/index/repositories \
  -H "Authorization: Bearer $TOKEN" \
  -H "Content-Type: application/json" \
  -d '{
    "repository_path": ".",
    "repository_name": "aethyme-core",
    "languages": ["python"],
    "use_fallback": true,
    "clear_existing": true
  }' | jq .
```

Save the returned `repository_id`:

```bash
export REPOSITORY_ID="<repository_id>"
```

### 4. Search

```bash
curl -X POST http://localhost:8001/api/v1/search/ \
  -H "Authorization: Bearer $TOKEN" \
  -H "Content-Type: application/json" \
  -d '{
    "query": "GraphStore",
    "limit": 5,
    "search_type": "hybrid"
  }' | jq .
```

### 5. Ego Graph

```bash
curl -X POST http://localhost:8001/api/v1/ego/ \
  -H "Authorization: Bearer $TOKEN" \
  -H "Content-Type: application/json" \
  -d '{
    "symbol": "graph/store.py:GraphStore",
    "depth": 2,
    "limit": 50
  }' | jq .
```

### 6. Impact Analysis

```bash
curl -X POST http://localhost:8001/api/v1/impact/ \
  -H "Authorization: Bearer $TOKEN" \
  -H "Content-Type: application/json" \
  -d '{
    "symbol": "graph/store.py:GraphStore",
    "max_depth": 3,
    "limit": 100
  }' | jq .
```

### 7. Scorecard

Trigger a scan:

```bash
curl -X POST http://localhost:8001/api/v1/scorecard/scan \
  -H "Authorization: Bearer $TOKEN" \
  -H "Content-Type: application/json" \
  -d "{
    \"repository_id\": \"${REPOSITORY_ID}\"
  }" | jq .
```

Inspect summary:

```bash
curl http://localhost:8001/api/v1/scorecard/summary/${REPOSITORY_ID} \
  -H "Authorization: Bearer $TOKEN" | jq .
```

## Notes

- Integration tests rebuild the test database from migrations.
- Seed data lives in [tests/support/db_seed.py](/Users/christophehenner/Downloads/Repositories/Aethyme/packages/aethyme/tests/support/db_seed.py).
- Temporary repositories for scorecard and CLI tests are created on demand in [tests/support/repo_builders.py](/Users/christophehenner/Downloads/Repositories/Aethyme/packages/aethyme/tests/support/repo_builders.py).
- Do not add checked-in fake repos, SQL fixture dumps, synthetic eval harnesses, or synthetic benchmarks back into the active test path.
