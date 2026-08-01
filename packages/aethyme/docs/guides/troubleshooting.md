# Troubleshooting

Last Updated: 2026-03-06

## API Will Not Start

### Symptoms
- startup exits early
- `/health/ready` is unavailable

### Checks

```bash
cd packages/aethyme
bash scripts/start-api.sh
```

Verify:

- `DATABASE_URL` points to PostgreSQL
- migrations have been applied
- PostgreSQL is reachable

## Indexing Fails

### Checks

```bash
cd packages/aethyme
aethyme repo ingest .
```

If fallback works and SCIP mode fails, the issue is in the language indexer toolchain rather than the shared indexing contract.

## Search Returns No Results

Check that the repository was indexed successfully:

```bash
curl -s http://localhost:8001/api/v1/index/freshness \
  -H "Authorization: Bearer $TOKEN"
```

## Scorecard Fails

Verify that the repository path still exists and that the repository was indexed under the expected tenant.

## First Debug Command

```bash
cd packages/aethyme
make test-full
```
