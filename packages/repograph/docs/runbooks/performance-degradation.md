# Runbook: Performance Degradation

**Audience:** SRE, Performance Engineers
**Severity:** HIGH
**Last Updated:** 2025-11-22

---

## Overview

Procedures for diagnosing and resolving performance issues in RepoGraph.

**SLO Targets:**
- Search/Ego/Impact P95: < 2s
- Indexing (medium repo): < 2 minutes
- API availability: > 99.5%

---

## Symptoms

- P95 latency > 2s for queries
- Indexing taking > 5 minutes
- High CPU/memory usage
- Database connection pool exhausted
- Slow page loads

**Alerts:**
```
ALERT: QueryLatencyP95 > 2000ms
ALERT: IndexingDurationP95 > 300s
ALERT: DatabaseConnectionsHigh > 80%
```

---

## Diagnostic Steps

### 1. Check Current Performance

```bash
# Query metrics
curl 'http://prometheus:9090/api/v1/query?query=histogram_quantile(0.95,repograph_request_duration_seconds)' | jq .

# Database performance
psql -U repograph -d repograph -c "SELECT * FROM pg_stat_statements ORDER BY total_exec_time DESC LIMIT 10;"

# System resources
docker stats --no-stream
```

### 2. Identify Slow Queries

```sql
-- Top 10 slowest queries
SELECT
  query,
  calls,
  total_exec_time,
  mean_exec_time,
  max_exec_time
FROM pg_stat_statements
ORDER BY mean_exec_time DESC
LIMIT 10;
```

### 3. Check Indexes

```sql
-- Missing indexes
SELECT
  schemaname,
  tablename,
  attname,
  n_distinct,
  correlation
FROM pg_stats
WHERE schemaname = 'public'
  AND n_distinct > 100
  AND correlation < 0.1;

-- Unused indexes
SELECT
  schemaname,
  tablename,
  indexname,
  idx_scan
FROM pg_stat_user_indexes
WHERE idx_scan = 0
  AND indexrelname NOT LIKE 'pg_toast%';
```

---

## Common Issues & Solutions

### Issue 1: Slow Search Queries

**Solution: Add database indexes**

```sql
-- Add index on symbol name
CREATE INDEX CONCURRENTLY idx_nodes_symbol ON nodes(symbol);

-- Add index for language filter
CREATE INDEX CONCURRENTLY idx_nodes_language ON nodes(language);

-- Composite index for common queries
CREATE INDEX CONCURRENTLY idx_nodes_repo_kind ON nodes(repository_id, kind);
```

### Issue 2: Cache Miss Rate High

**Solution: Warm cache and increase TTL**

```bash
# Warm cache with common queries
bash scripts/warm-cache.sh

# Increase Redis TTL
redis-cli CONFIG SET maxmemory-policy allkeys-lru
REDIS_CACHE_TTL=600  # 10 minutes instead of 5
```

### Issue 3: Database Connection Pool Exhausted

**Solution: Increase pool size**

```bash
# In .env
DB_POOL_MAX_SIZE=50  # Increase from 20
DB_POOL_MIN_SIZE=10

# Restart API
docker-compose restart api
```

### Issue 4: Large Result Sets

**Solution: Implement pagination**

```python
# Limit result size
@app.post("/api/search/")
async def search(query: str, limit: int = 20, offset: int = 0):
    if limit > 100:
        raise HTTPException(400, "Max limit is 100")
    # ... query with LIMIT and OFFSET
```

### Issue 5: N+1 Query Problem

**Solution: Use eager loading**

```python
# Bad: N+1 queries
for node in nodes:
    edges = await db.query(Edge).filter(Edge.source_id == node.id).all()

# Good: Single query with join
nodes_with_edges = await db.query(Node).options(
    selectinload(Node.edges)
).all()
```

---

## Performance Tuning

### Database Tuning

```sql
-- Increase shared buffers
ALTER SYSTEM SET shared_buffers = '4GB';

-- Increase work memory
ALTER SYSTEM SET work_mem = '256MB';

-- Increase effective cache size
ALTER SYSTEM SET effective_cache_size = '12GB';

-- Reload configuration
SELECT pg_reload_conf();
```

### Query Optimization

```bash
# Analyze query plans
psql -U repograph -d repograph <<EOF
EXPLAIN ANALYZE
SELECT * FROM nodes
WHERE repository_id = '...'
  AND kind = 'class'
ORDER BY symbol;
EOF
```

### API Optimization

```python
# Add response compression
from fastapi.middleware.gzip import GZipMiddleware
app.add_middleware(GZipMiddleware, minimum_size=1000)

# Add caching headers
@app.get("/api/search/")
async def search(response: Response):
    response.headers["Cache-Control"] = "public, max-age=300"
    # ...
```

---

## Scaling Procedures

### Horizontal Scaling (Add Replicas)

```bash
# Kubernetes
kubectl scale deployment repograph-api --replicas=6 -n repograph

# Docker Compose
docker-compose -f ops/docker-compose.yml up -d --scale api=4
```

### Vertical Scaling (Increase Resources)

```yaml
# k8s/deployment.yaml
resources:
  limits:
    cpu: "4"
    memory: "8Gi"
  requests:
    cpu: "2"
    memory: "4Gi"
```

---

## Related Runbooks

- [Indexing Failure](index-failure.md)
- [Rollback](rollback.md)

---

**Runbook Owner:** Performance Engineering Team
