# Aethyme API Reference

Base path: `http://localhost:8001`

## Health
- `GET /health/`
- `GET /health/live`
- `GET /health/ready`
- `GET /health/detailed`

## Authentication
- protected routes require `Authorization: Bearer <token-or-api-key>`
- bearer tokens are issued outside core by a trusted identity layer
- core enforces `tenant_id`, `org`, and `scopes`

## Indexing
- Canonical contract: [`src/indexing/service.py`](/Users/christophehenner/Downloads/Repositories/Aethyme/packages/aethyme/src/indexing/service.py)
- `POST /api/v1/index/repositories`
- `GET /api/v1/index/status/{repo_id}`
- `GET /api/v1/index/freshness`

## Search
- `POST /api/v1/search/`
- `GET /api/v1/search/suggest`
- `POST /api/v1/search/advanced`

## Ego
- `POST /api/v1/ego/`
- `GET /api/v1/ego/definition/{symbol}`

## Impact
- `POST /api/v1/impact/`
- `POST /api/v1/impact/bulk`

## Scorecard
- `POST /api/v1/scorecard/scan`
- `GET /api/v1/scorecard/results/{scan_id}`
- `GET /api/v1/scorecard/summary/{repository_id}`
- `GET /api/v1/scorecard/history/{repository_id}`
- `GET /api/v1/scorecard/checks`
