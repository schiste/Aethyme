# RepoGraph - Quick Testing Guide

This guide provides step-by-step instructions to verify the RepoGraph deployment.

## Prerequisites

All services should be running. Check with:
```bash
docker ps | grep repograph
```

You should see 5 containers running:
- `repograph-postgres` (port 5433)
- `repograph-redis` (port 6380)
- `repograph-api` (port 8001)
- `repograph-prometheus` (port 9090)
- `repograph-grafana` (port 3001)

---

## Step 1: Check Service Health

### Basic Health Check
```bash
curl http://localhost:8001/health/
```

**Expected output:**
```json
{
  "status": "healthy",
  "timestamp": "2025-10-02T..."
}
```

### Detailed Health Check
```bash
curl http://localhost:8001/health/detailed | jq .
```

**Expected output:**
```json
{
  "status": "healthy",
  "timestamp": "...",
  "services": {
    "database": "healthy",
    "redis": "healthy"
  },
  "system": {
    "cpu_percent": ...,
    "memory_percent": ...
  }
}
```

---

## Step 2: View API Documentation

Open in your browser:
```
http://localhost:8001/docs
```

This shows the interactive Swagger UI with all available endpoints.

---

## Step 3: Authenticate

### Get a JWT Token
```bash
curl -X POST http://localhost:8001/api/auth/login \
  -H "Content-Type: application/json" \
  -d '{
    "email": "test@example.com",
    "password": "test1234"
  }' | jq .
```

**Expected output:**
```json
{
  "access_token": "eyJhbGci...",
  "token_type": "bearer",
  "expires_in": 86400,
  "user": {
    "user_id": "...",
    "email": "test@example.com",
    "tenant_id": "...",
    "tenant_name": "aeptus"
  }
}
```

**Save the token for next steps:**
```bash
export TOKEN="<paste-access_token-here>"
```

### Verify Authentication
```bash
curl http://localhost:8001/api/auth/me \
  -H "Authorization: Bearer $TOKEN" | jq .
```

**Expected output:**
```json
{
  "user_id": "...",
  "tenant_id": "...",
  "email": "test@example.com",
  "permissions": ["read", "write"]
}
```

---

## Step 4: Test Search API

### Search for indexed code symbols
```bash
curl -X POST http://localhost:8001/api/search/ \
  -H "Authorization: Bearer $TOKEN" \
  -H "Content-Type: application/json" \
  -d '{
    "query": "GraphStore",
    "limit": 5,
    "search_type": "hybrid"
  }' | jq .
```

**Expected output:**
```json
{
  "query": "GraphStore",
  "results": [
    {
      "id": "...",
      "symbol": "graph/store.py:GraphStore",
      "file_path": "graph/store.py",
      "line_number": 17,
      "kind": "class",
      "language": "python",
      "score": 0.64,
      "documentation": null
    },
    ...
  ],
  "total_results": 5,
  "search_type": "hybrid"
}
```

### Try different search types

**Exact match:**
```bash
curl -X POST http://localhost:8001/api/search/ \
  -H "Authorization: Bearer $TOKEN" \
  -H "Content-Type: application/json" \
  -d '{
    "query": "graph/store.py:GraphStore",
    "limit": 1,
    "search_type": "exact"
  }' | jq .
```

**Fuzzy match:**
```bash
curl -X POST http://localhost:8001/api/search/ \
  -H "Authorization: Bearer $TOKEN" \
  -H "Content-Type: application/json" \
  -d '{
    "query": "store",
    "limit": 10,
    "search_type": "fuzzy"
  }' | jq .
```

---

## Step 5: Test Ego Graph (Code Relationships)

### Get connected nodes for a symbol
```bash
curl -X POST http://localhost:8001/api/ego/ \
  -H "Authorization: Bearer $TOKEN" \
  -H "Content-Type: application/json" \
  -d '{
    "symbol": "graph/store.py:GraphStore",
    "depth": 2
  }' | jq . | head -50
```

**Expected output:**
```json
{
  "symbol": "graph/store.py:GraphStore",
  "definition": {
    "id": "...",
    "symbol": "graph/store.py:GraphStore",
    "file_path": "graph/store.py",
    "line_number": 17,
    "kind": "class",
    "language": "python",
    ...
  },
  "nodes_by_depth": {
    "0": [...],
    "1": [...],
    "2": [...]
  },
  "total_nodes": ...,
  "max_depth_reached": 2,
  "cached": false
}
```

---

## Step 6: Check Database Content

### View indexed repositories
```bash
docker exec repograph-api python -m src.indexer.cli list-repos
```

**Expected output:**
```
Found 1 repositories:

ID: e63693dd-...
Name: repograph-api
Path: /app/src
Created: 2025-10-02 ...
Updated: 2025-10-02 ...
```

### View repository statistics
```bash
docker exec repograph-api python -m src.indexer.cli stats e63693dd-fb82-4dcb-9df8-fef2950b2aad
```

**Expected output:**
```
Repository Statistics:

Nodes by kind:
  function: 95
  class: 25
  method: 45
  import: 25

Edges by type:
  contains: 40
  imports: 11

Total nodes: 190
Total edges: 51
```

### Query database directly
```bash
docker exec repograph-postgres psql -U repograph -d repograph -c \
  "SELECT kind, COUNT(*) FROM repograph.nodes GROUP BY kind;"
```

**Expected output:**
```
   kind   | count
----------+-------
 class    |    25
 function |    95
 import   |    25
 method   |    45
```

---

## Step 7: Check Monitoring

### Prometheus Metrics
Open in browser:
```
http://localhost:9090
```

Try this query in Prometheus:
```
up{job="repograph"}
```

### Grafana Dashboards
Open in browser:
```
http://localhost:3001
```

Login with:
- Username: `admin`
- Password: `admin`

---

## Step 8: Test Advanced Features

### Search Suggestions
```bash
curl -X POST http://localhost:8001/api/search/suggest \
  -H "Authorization: Bearer $TOKEN" \
  -H "Content-Type: application/json" \
  -d '{
    "query": "gra",
    "limit": 5
  }' | jq .
```

### Advanced Search (with filters)
```bash
curl -X POST http://localhost:8001/api/search/advanced \
  -H "Authorization: Bearer $TOKEN" \
  -H "Content-Type: application/json" \
  -d '{
    "query": "store",
    "filters": {
      "kind": ["class", "function"],
      "language": ["python"]
    },
    "limit": 10
  }' | jq .
```

---

## Step 9: Index Additional Code

### Index a different directory
```bash
docker exec repograph-api python -m src.indexer.cli index /app \
  -l python \
  --repo-name repograph-full
```

**Expected output:**
```
Indexing repository: repograph-full
Path: /app
Tenant ID: ...
Languages: python

Indexing python files...
  Using fallback indexer for python

Indexing complete!
Files processed: 50+
Nodes created: 400+
Edges created: 100+
Errors: 0
```

### Verify new index
```bash
curl -X POST http://localhost:8001/api/search/ \
  -H "Authorization: Bearer $TOKEN" \
  -H "Content-Type: application/json" \
  -d '{
    "query": "cli",
    "limit": 10
  }' | jq .
```

---

## Troubleshooting

### Check API Logs
```bash
docker logs repograph-api --tail 50
```

### Check Database Connection
```bash
docker exec repograph-postgres psql -U repograph -d repograph -c "SELECT version();"
```

### Restart Services
```bash
docker compose -f ops/docker-compose.yml restart api
```

### View All Container Status
```bash
docker compose -f ops/docker-compose.yml ps
```

---

## Quick Summary

**What's been indexed:**
- 190 Python nodes (classes, functions, methods, imports)
- 51 edges (relationships between code elements)
- Source: `/app/src` (RepoGraph's own source code)

**Available endpoints:**
- `GET /health/` - Health check
- `POST /api/auth/login` - Authentication
- `POST /api/search/` - Code search
- `POST /api/ego/` - Code relationships
- `GET /docs` - Interactive API documentation

**Services:**
- API: http://localhost:8001
- Prometheus: http://localhost:9090
- Grafana: http://localhost:3001
- PostgreSQL: localhost:5433
- Redis: localhost:6380
