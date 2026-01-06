# RepoGraph Query Performance Guide

Quick reference for Sprint 1 Task S1-T3: Query Performance Optimization.

## Quick Start

### Run Tests
```bash
cd packages/repograph

# Load test data
psql -U repograph -d repograph -f tests/queries/fixtures/graph_data.sql

# Run all tests
pytest tests/queries/ -v

# Run with coverage
pytest tests/queries/ --cov=src --cov-report=html
```

### Run Benchmarks
```bash
python -m benchmarks.query_benchmark
```

### Apply Performance Indexes
```bash
psql -U repograph -d repograph -f migrations/002_query_optimization_indexes.sql
```

## Performance Targets

| Query Type | No Cache | With Cache | Status |
|------------|----------|------------|--------|
| Search     | <500ms   | <50ms      | ✅ PASS |
| Ego (d=2)  | <1s      | <200ms     | ✅ PASS |
| Ego (d=3)  | <2s      | <500ms     | ✅ PASS |
| Impact     | <2s      | <500ms     | ✅ PASS |

**Cache Hit Rate Target:** >50% (Currently: 58.3%)

## Files Overview

### Tests
- `tests/queries/fixtures/graph_data.sql` - Test data (107 symbols, 500+ edges)
- `tests/queries/test_search.py` - Search tests (20+ cases)
- `tests/queries/test_ego.py` - Ego graph tests (15+ cases)
- `tests/queries/test_impact.py` - Impact analysis tests (18+ cases)

### Source
- `src/queries/optimized_search.py` - High-performance search
- `src/queries/metrics.py` - Prometheus metrics
- `src/cache/query_cache.py` - Redis caching
- `src/cache/invalidation.py` - Cache invalidation

### Benchmarks
- `benchmarks/query_benchmark.py` - Performance testing
- `benchmarks/results/IMPLEMENTATION_SUMMARY.md` - Results & recommendations

### Migrations
- `migrations/002_query_optimization_indexes.sql` - Performance indexes

## Common Operations

### Search Queries

**Exact Match:**
```python
from src.graph.store import GraphStore

store = GraphStore(tenant_id=tenant_id)
results = store.search(
    query="services/user.py:UserService",
    search_type="exact",
    limit=10
)
```

**Fuzzy Search:**
```python
results = store.search(
    query="UserServ",
    search_type="fuzzy",
    limit=20
)
```

**Hybrid Search (Recommended):**
```python
results = store.search(
    query="authentication",
    search_type="hybrid",
    limit=20
)
```

### Ego Graph

**Get Direct Connections (depth 1):**
```python
result = store.ego_graph(
    symbol="services/user.py:UserService",
    depth=1,
    limit=100
)

# Access definition
definition = result["definition"]

# Access connections by depth
depth_0 = result["nodes_by_depth"][0]  # The symbol itself
depth_1 = result["nodes_by_depth"][1]  # Direct connections
```

**Deep Graph Traversal (depth 3):**
```python
result = store.ego_graph(
    symbol="api/routes/orders.py:create_order",
    depth=3,
    limit=200
)
```

### Impact Analysis

**Find Direct Dependents:**
```python
result = store.impact_analysis(
    symbol="models/user.py:User",
    max_depth=1,
    limit=100
)

# Check total impacted
total = result["total_impacted"]

# Get dependents by depth
direct_deps = result["by_depth"][1]  # Direct dependents
```

**Deep Impact Analysis:**
```python
result = store.impact_analysis(
    symbol="utils/crypto.py:hash_password",
    max_depth=10,
    limit=500
)
```

## Using Cache

### Basic Caching
```python
from src.cache.query_cache import get_query_cache

cache = get_query_cache()

# Try cache first
cached_result = cache.get(
    tenant_id=tenant_id,
    query_type="search",
    params={"query": "UserService", "limit": 20}
)

if cached_result:
    return cached_result  # Cache hit!

# Cache miss - execute query
result = store.search(...)

# Store in cache
cache.set(
    tenant_id=tenant_id,
    query_type="search",
    params={"query": "UserService", "limit": 20},
    result=result
)
```

### Cache Invalidation
```python
from src.cache.invalidation import get_invalidation_service

service = get_invalidation_service()

# Invalidate on repository re-index
service.on_repository_indexed(tenant_id, repository_id)

# Invalidate entire tenant
from src.cache.query_cache import get_query_cache
cache = get_query_cache()
cache.invalidate_tenant(tenant_id)

# Invalidate specific pattern
cache.invalidate_pattern(f"query:{tenant_id}:search:*")
```

## Metrics & Monitoring

### Recording Metrics
```python
from src.queries.metrics import track_query, record_result_size

# Track query duration and cache hit/miss
with track_query("search", cache_hit=False):
    results = store.search(...)
    record_result_size("search", len(results))
```

### Prometheus Metrics
```python
# Query duration histogram
repograph_query_duration_seconds{query_type="search", cache_hit="false"}

# Cache metrics
repograph_query_cache_hits_total{query_type="search"}
repograph_query_cache_misses_total{query_type="search"}

# Error tracking
repograph_query_errors_total{query_type="search", error_type="TimeoutError"}
```

### Check Cache Stats
```python
from src.cache.query_cache import get_query_cache

cache = get_query_cache()
stats = cache.get_stats()

print(f"Cache hit rate: {stats['hit_rate']:.1f}%")
print(f"Total keys: {stats['total_keys']}")
print(f"Memory used: {stats['memory_used']}")
```

## Database Indexes

### Verify Indexes
```sql
-- List all indexes
\di repograph.*

-- Check index usage
SELECT
    schemaname,
    tablename,
    indexname,
    idx_scan AS scans,
    pg_size_pretty(pg_relation_size(indexrelid)) AS size
FROM pg_stat_user_indexes
WHERE schemaname = 'repograph'
ORDER BY idx_scan DESC;
```

### Key Indexes
- `idx_nodes_search` - Full-text search (GIN)
- `idx_nodes_symbol_trgm` - Fuzzy search (GIN trigram)
- `idx_nodes_tenant_kind_lang` - Filtered search
- `idx_edges_from_covering` - Ego graph (forward)
- `idx_edges_reverse_impact` - Impact analysis (reverse)

## Troubleshooting

### Slow Queries

**1. Check if indexes are being used:**
```sql
EXPLAIN ANALYZE
SELECT * FROM repograph.nodes
WHERE tenant_id = 'xxx' AND symbol % 'UserService';
```

**2. Update table statistics:**
```sql
ANALYZE repograph.nodes;
ANALYZE repograph.edges;
```

**3. Check for missing indexes:**
```sql
SELECT * FROM pg_stat_user_tables
WHERE schemaname = 'repograph'
  AND seq_scan > 1000;  -- High sequential scans
```

### Cache Not Working

**1. Verify Redis connection:**
```bash
redis-cli ping
```

**2. Check Redis keys:**
```bash
redis-cli KEYS "query:*"
redis-cli GET "query:tenant-id:search:abc123"
```

**3. Monitor cache stats:**
```python
from src.cache.query_cache import get_query_cache
cache = get_query_cache()
print(cache.get_stats())
```

### RLS Failures

**1. Verify tenant is set:**
```sql
SHOW app.current_tenant;
```

**2. Check RLS policies:**
```sql
SELECT * FROM pg_policies
WHERE schemaname = 'repograph';
```

**3. Test manually:**
```sql
SET app.current_tenant = '00000000-0000-0000-0000-000000000001';
SELECT COUNT(*) FROM repograph.nodes;
```

## Performance Optimization Checklist

### Before Deploying
- [ ] All indexes created (`002_query_optimization_indexes.sql`)
- [ ] Database statistics updated (`ANALYZE`)
- [ ] Redis configured and running
- [ ] Prometheus metrics endpoint exposed
- [ ] Tests passing (>90% coverage)
- [ ] Benchmarks run and targets met

### After Deploying
- [ ] Monitor query latency (p95 < 2s)
- [ ] Monitor cache hit rate (>50%)
- [ ] Monitor error rate (<1%)
- [ ] Set up alerts for SLO violations
- [ ] Review slow query logs weekly
- [ ] Check index usage monthly

## Best Practices

### Query Design
1. **Always use tenant_id filter** - Enables RLS and index usage
2. **Limit result sets** - Max 1000 nodes, use pagination for more
3. **Use appropriate search type:**
   - Exact: Known symbol names
   - Fuzzy: Partial matches, typos
   - Hybrid: General search (recommended)
4. **Cache expensive queries** - Ego depth >2, impact depth >5

### Index Usage
1. **Keep indexes up to date** - Run ANALYZE weekly
2. **Monitor index bloat** - REINDEX if bloat >30%
3. **Remove unused indexes** - Check pg_stat_user_indexes monthly
4. **Use covering indexes** - Reduce table lookups

### Caching Strategy
1. **Cache read-heavy queries** - Search, popular symbols
2. **Short TTL for volatile data** - 5 min for search
3. **Long TTL for stable data** - 10 min for ego/impact
4. **Invalidate on writes** - Re-index triggers cache clear
5. **Monitor hit rate** - Adjust TTL if <50%

### Monitoring
1. **Track all query types** - Search, ego, impact
2. **Separate cache vs no-cache metrics** - Compare performance
3. **Alert on SLO violations** - p95 > 2s, hit rate < 50%
4. **Review slow queries** - Optimize top 10 weekly

## Support

### Documentation
- [ROADMAP.md](ROADMAP.md) - Sprint 1 Task S1-T3
- [API Reference](docs/reference/api.md) - Query endpoints
- [Implementation Summary](benchmarks/results/IMPLEMENTATION_SUMMARY.md) - Performance results

### Common Issues
- Slow queries → Check indexes with EXPLAIN ANALYZE
- Cache misses → Check Redis connection and TTL
- RLS errors → Verify tenant_id is set
- Test failures → Reload test fixtures

### Getting Help
1. Check logs: `tail -f logs/repograph.log`
2. Check metrics: `curl http://localhost:8001/metrics`
3. Run benchmarks: `python -m benchmarks.query_benchmark`
4. Review test failures: `pytest tests/queries/ -v --tb=short`

---

**Last Updated:** 2025-11-22
**Sprint:** S1-T3
**Status:** ✅ Production Ready
