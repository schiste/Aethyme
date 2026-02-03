# Aethyme Performance Budgets

**Version:** 1.0
**Date:** 2025-11-22
**Stage:** Stage 1 (CLI/Service Backend)

---

## Overview

Performance budgets define **non-negotiable targets** for Aethyme Stage 1. These budgets ensure the system meets user expectations and scales efficiently.

---

## 1. API Latency (p95)

**Measurement:** 95th percentile response time (5% of requests may exceed this)

| Endpoint Category | Target (p95) | Acceptable (p99) | Critical Threshold |
|-------------------|--------------|------------------|---------------------|
| **Authentication** | <100ms | <200ms | 500ms |
| **Health checks** | <50ms | <100ms | 200ms |
| **Repository list** | <200ms | <500ms | 1s |
| **Symbol search (cached)** | <50ms | <100ms | 200ms |
| **Symbol search (cold)** | <500ms | <1s | 2s |
| **Ego graph (depth 2)** | <2s | <3s | 5s |
| **Impact analysis** | <2s | <3s | 5s |
| **AI-readiness scorecard** | Async (job) | Async (job) | N/A |
| **Autofix** | Async (job) | Async (job) | N/A |

**How to Measure:**

```python
# Prometheus histogram
from prometheus_client import Histogram

api_latency = Histogram(
    'aethyme_api_latency_seconds',
    'API request latency',
    ['method', 'endpoint'],
    buckets=[0.01, 0.05, 0.1, 0.2, 0.5, 1, 2, 5, 10]
)

# Instrumentation
@app.middleware("http")
async def track_latency(request: Request, call_next):
    start = time.time()
    response = await call_next(request)
    duration = time.time() - start

    api_latency.labels(
        method=request.method,
        endpoint=request.url.path
    ).observe(duration)

    return response
```

**Alert Rule:**

```yaml
# Prometheus alert
- alert: HighAPILatency
  expr: histogram_quantile(0.95, aethyme_api_latency_seconds{endpoint="/query/ego"}) > 2
  for: 5m
  labels:
    severity: warning
  annotations:
    summary: "Ego graph queries p95 > 2s"
```

---

## 2. Indexing Performance

**Measurement:** Time to index a repository (full indexing cycle)

| Repository Size | Target | Acceptable | Critical |
|-----------------|--------|------------|----------|
| **Small** (<100 files) | <30s | <60s | 2min |
| **Medium** (100-1000 files) | <2min | <5min | 10min |
| **Large** (1000-10000 files) | <10min | <20min | 30min |
| **Very Large** (>10000 files) | <30min | <60min | 2 hours |

**Factors:**
- SCIP indexing time
- Git clone time
- Tree-sitter fallback time
- Database bulk insert time
- Network latency

**Optimization Strategies:**

1. **Parallel processing** - Index multiple files concurrently
2. **Incremental indexing** - Only re-index changed files
3. **Shallow clones** - `git clone --depth 1` for faster cloning
4. **Bulk inserts** - PostgreSQL COPY instead of individual INSERTs

**Test Benchmark:**

```python
import time
from aethyme.indexer import IndexingService

async def benchmark_indexing():
    """Benchmark indexing on standard repositories."""
    repos = [
        ("small", "https://github.com/pallets/click", 50),      # ~50 files
        ("medium", "https://github.com/django/django", 500),    # ~500 files
        ("large", "https://github.com/kubernetes/kubernetes", 5000)  # ~5000 files
    ]

    for size, url, file_count in repos:
        start = time.time()
        await indexer.index_repository(url)
        duration = time.time() - start

        print(f"{size}: {duration:.1f}s ({file_count} files) = {file_count/duration:.1f} files/sec")

        # Assert meets budget
        budgets = {"small": 30, "medium": 120, "large": 600}
        assert duration < budgets[size], f"{size} repo exceeded budget"
```

---

## 3. Query Throughput

**Measurement:** Requests per second per API instance

| Query Type | Target (req/s) | Acceptable | Notes |
|------------|----------------|------------|-------|
| **Search (cached)** | 1000 | 500 | Redis cache hit |
| **Search (cold)** | 100 | 50 | Database query |
| **Ego graph** | 50 | 25 | Recursive CTE |
| **Impact analysis** | 50 | 25 | Recursive CTE |

**Load Testing:**

```bash
# Using Locust
pip install locust

# locustfile.py
from locust import HttpUser, task, between

class AethymeUser(HttpUser):
    wait_time = between(1, 3)
    host = "https://api.aethyme.com"

    @task(3)
    def search(self):
        self.client.get("/api/v1/query/search?q=MyClass",
                       headers={"Authorization": f"Bearer {self.token}"})

    @task(1)
    def ego_graph(self):
        self.client.get("/api/v1/query/ego?symbol=MyClass&depth=2",
                       headers={"Authorization": f"Bearer {self.token}"})

# Run load test
locust --users 100 --spawn-rate 10 --run-time 5m
```

**Expected Results:**
- 100 concurrent users
- ~1000 req/s total (90% search, 10% ego)
- p95 latency < 500ms

---

## 4. Resource Usage (Per Instance)

**API Pod:**

| Resource | Target | Limit | Notes |
|----------|--------|-------|-------|
| **CPU** | 500m (request) | 2000m (limit) | 0.5-2 cores |
| **Memory** | 1Gi (request) | 4Gi (limit) | OOM kill at 4Gi |
| **Startup time** | <10s | <30s | Readiness probe |
| **Shutdown time** | <5s | <10s | Graceful shutdown |

**Worker Pod:**

| Resource | Target | Limit | Notes |
|----------|--------|-------|-------|
| **CPU** | 1000m (request) | 4000m (limit) | 1-4 cores |
| **Memory** | 2Gi (request) | 8Gi (limit) | For large repos |

**PostgreSQL:**

| Metric | Target | Notes |
|--------|--------|-------|
| **Connection pool** | 20 per pod | Max 200 total |
| **Query execution time** | <100ms (p95) | Slow query log at 1s |
| **Cache hit ratio** | >95% | Shared buffers + OS cache |
| **Replication lag** | <5s | Alert if >10s |

**Redis:**

| Metric | Target | Notes |
|--------|--------|-------|
| **Memory usage** | <10GB | 13GB instance size |
| **Cache hit rate** | >90% | Query result caching |
| **Eviction rate** | <1% | LRU eviction policy |

---

## 5. Availability Targets

| Metric | Target | Downtime/Year | Notes |
|--------|--------|---------------|-------|
| **Uptime (SLA)** | 99.9% | 8.76 hours | "Three nines" |
| **API availability** | 99.95% | 4.38 hours | Health check success rate |
| **Database availability** | 99.99% | 52 minutes | Multi-AZ RDS |
| **Redis availability** | 99.9% | 8.76 hours | Sentinel failover |

**Monitoring:**

```yaml
# Prometheus alert
- alert: APIDowntime
  expr: up{job="aethyme-api"} == 0
  for: 1m
  labels:
    severity: critical
  annotations:
    summary: "Aethyme API is down"

- alert: LowAvailability
  expr: avg_over_time(up{job="aethyme-api"}[1h]) < 0.999
  labels:
    severity: warning
  annotations:
    summary: "API availability < 99.9% over last hour"
```

---

## 6. Recovery Time Objectives (RTO/RPO)

| Scenario | RTO (Recovery Time) | RPO (Data Loss) | Mitigation |
|----------|---------------------|-----------------|------------|
| **API pod crash** | <1 min | 0 | Auto-restart (K8s) |
| **Database failure** | <15 min | <1 min | Multi-AZ failover |
| **Redis failure** | <5 min | Acceptable | Sentinel failover, cache rebuild |
| **Region outage** | <1 hour | <15 min | Cross-region backup restore |
| **Data corruption** | <4 hours | <1 hour | PITR (Point-in-Time Recovery) |

---

## 7. Scalability Limits

**Maximum Supported Load:**

| Dimension | Target Capacity | Max Capacity | Notes |
|-----------|----------------|--------------|-------|
| **Organizations** | 1,000 | 10,000 | RLS isolation |
| **Repositories** | 10,000 | 100,000 | Per org limit |
| **Symbols** | 10M | 100M | PostgreSQL table size |
| **Edges** | 50M | 500M | Graph relationships |
| **Concurrent API requests** | 1,000/s | 10,000/s | Horizontal scaling |
| **Concurrent indexing jobs** | 10 | 50 | Worker pool size |

**Horizontal Scaling:**

```yaml
# Auto-scale based on load
apiVersion: autoscaling/v2
kind: HorizontalPodAutoscaler
metadata:
  name: aethyme-api-hpa
spec:
  minReplicas: 3
  maxReplicas: 20  # Up to 20,000 req/s (1000 req/s per pod)
  metrics:
  - type: Resource
    resource:
      name: cpu
      target:
        type: Utilization
        averageUtilization: 70
```

---

## 8. Cost Budgets

**Monthly Infrastructure Cost (Production):**

| Component | Cost Target | Cost Limit | Notes |
|-----------|-------------|------------|-------|
| **Kubernetes (EKS)** | $73 | $150 | Control plane |
| **EC2 Instances** | $150 | $500 | Worker nodes (auto-scale) |
| **RDS PostgreSQL** | $600 | $1,200 | Multi-AZ, r6g.xlarge |
| **ElastiCache Redis** | $150 | $300 | cache.r6g.large |
| **Data Transfer** | $50 | $200 | Outbound traffic |
| **Monitoring (Grafana Cloud)** | $50 | $100 | Optional |
| **Total** | **$1,073** | **$2,450** | Monthly budget |

**Cost per Request:**

```
Assumptions:
- 10M requests/month
- $1,073/month infrastructure cost
- Cost per request = $1,073 / 10M = $0.0001073 (~$0.11 per 1,000 requests)

With 100M requests/month (10x growth):
- Infrastructure scales to ~$1,500/month (auto-scaling)
- Cost per request = $0.000015 (~$0.015 per 1,000 requests)
```

---

## 9. Testing Requirements

**Performance Test Matrix:**

| Test Type | Frequency | Duration | Success Criteria |
|-----------|-----------|----------|------------------|
| **Unit tests** | Every commit | <5 min | 100% pass |
| **Integration tests** | Every PR | <15 min | 100% pass |
| **Load tests** | Weekly | 30 min | Meet latency budgets |
| **Stress tests** | Monthly | 2 hours | Graceful degradation |
| **Chaos tests** | Quarterly | 4 hours | Auto-recovery |

**Example Load Test:**

```bash
# Load test: 1000 concurrent users, 5 minutes
locust --users 1000 --spawn-rate 50 --run-time 5m \
       --host https://api.aethyme.com \
       --headless \
       --csv results/load-test

# Expected results:
# - Total requests: ~100,000
# - Failure rate: <1%
# - p95 latency: <500ms
# - p99 latency: <2s
```

---

## 10. Monitoring Dashboards

**Grafana Dashboard Panels:**

1. **API Performance**
   - Request rate (req/s)
   - Latency (p50, p95, p99)
   - Error rate (%)
   - Cache hit rate (%)

2. **Indexing Performance**
   - Active indexing jobs
   - Avg indexing duration (by repo size)
   - Failed indexing jobs (%)
   - Queue depth

3. **Resource Usage**
   - CPU usage (%)
   - Memory usage (%)
   - Database connections
   - Redis memory

4. **Business Metrics**
   - Total repositories indexed
   - Total symbols indexed
   - Active organizations
   - API calls per org

**Sample PromQL Queries:**

```promql
# API latency p95
histogram_quantile(0.95, rate(aethyme_api_latency_seconds_bucket[5m]))

# Cache hit rate
rate(aethyme_cache_hits_total[5m]) /
  (rate(aethyme_cache_hits_total[5m]) + rate(aethyme_cache_misses_total[5m]))

# Error rate
rate(aethyme_api_requests_total{status=~"5.."}[5m]) /
  rate(aethyme_api_requests_total[5m])
```

---

## Summary: Performance Budget Checklist

- [ ] API latency p95 < 500ms (search), < 2s (graph queries)
- [ ] Indexing: Small repos < 30s, Medium < 2min, Large < 10min
- [ ] Throughput: 1000 req/s per API pod
- [ ] Uptime: 99.9% availability
- [ ] RTO: <1 hour, RPO: <15 minutes
- [ ] Cost: <$1,100/month baseline, <$2,500/month max
- [ ] Scalability: Support 1,000 orgs, 10,000 repos, 10M symbols
- [ ] Load tests pass weekly (1000 users, <1% errors)

---

**Document Status:** ✅ Complete - Performance Targets Defined
**Next Steps:** Set up monitoring, run baseline tests, establish alerts
