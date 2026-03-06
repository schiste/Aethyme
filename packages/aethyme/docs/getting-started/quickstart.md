# Aethyme Quick Start

Last Updated: 2026-03-06

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

## 3. Apply Migrations

```bash
bash scripts/migrate.sh
```

## 4. Run The API

```bash
bash scripts/start-api.sh
```

Docs are served at `http://localhost:8001/docs`.

## 5. Provide A Trusted Credential

Core does not expose login or registration routes.

Use one of these:

- a bearer token issued by `packages/aethyme-cloud`
- a scoped API key
- the local test helpers in [`../../tests/support/auth_db.py`](../../tests/support/auth_db.py)

```bash
export TOKEN='<trusted-bearer-token>'
```

## 6. Index A Repository

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

## 7. Search The Graph

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

## 9. Run The Verified Test Slices

```bash
. .venv/bin/activate
make test-unit
TEST_DATABASE_URL='postgresql://aethyme:dev_password_change_me@localhost:5432/aethyme_test' make test-integration
```

## 10. Run The Local-First Proof Path

If you want to test Aethyme on one repository without any SaaS layer, use the CLI path directly.

```bash
. .venv/bin/activate

aethyme repo ingest /absolute/path/to/repo
aethyme repo inspect /absolute/path/to/repo --json-output
aethyme repo clear-cache /absolute/path/to/repo
aethyme query symbol /absolute/path/to/repo main
aethyme task pack --repo /absolute/path/to/repo --task "Explain this repo" --json-output
aethyme task explain --repo /absolute/path/to/repo
aethyme eval explain-repo --repo /absolute/path/to/repo --json-output
```

This local-first path is the current shortest route to proving:

1. deterministic repository mapping
2. deterministic discoverability
3. deterministic task-context packs
4. explain-repo evaluation artifacts

At this stage:

- the Rust engine is executed as a built binary
- local artifacts are cached by repository snapshot
- Git repositories use commit plus dirty-state metadata for cache keys
- `eval explain-repo` produces the control prompts, pack, explanation, and comparison report by default
- it can execute real runs when `--baseline-cmd` and `--aethyme-cmd` are provided
- the Aethyme-assisted prompt uses a compact rendered pack rather than the full raw pack payload
