# Quick Start: Indexing Reliability Features

## Get Started in 5 Minutes

This guide gets you up and running with the new indexing reliability features from Sprint S1-T2.

## Prerequisites

```bash
# Ensure you have the dependencies
pip install prometheus-client psutil structlog
```

## 1. Test the Validator (2 minutes)

```python
from pathlib import Path
from src.indexing.validator import IndexerValidator

# Initialize validator
validator = IndexerValidator()

# Validate a repository
metrics = validator.validate_repository(
    repo_path=Path("/path/to/your/repo"),
    repo_name="my-repo",
    language="python",
    try_scip=True,
)

# View results
print(f"Indexer: {metrics.indexer_type.value}")
print(f"Duration: {metrics.duration_seconds:.2f}s")
print(f"Symbols: {metrics.symbol_count}")
print(f"Success: {metrics.result.value}")

# Generate report
print(validator.generate_report())
```

## 2. Check Index Status via API (1 minute)

```bash
# Start the API server
cd packages/aethyme
python -m uvicorn src.api.main:app --reload

# Query status (in another terminal)
curl http://localhost:8000/api/index/freshness

# Or use the Swagger UI
open http://localhost:8000/docs
```

## 3. Run Benchmarks (2 minutes)

```bash
# Clone a test repository
mkdir -p /tmp/test-repos
cd /tmp/test-repos
git clone https://github.com/pallets/flask

# Run benchmark
python packages/aethyme/benchmarks/indexing_benchmark.py \
  --repos-dir /tmp/test-repos \
  --output-dir benchmarks/results

# View report
cat benchmarks/results/index_perf_report.md
```

## 4. Monitor with Prometheus (Optional)

```bash
# Create minimal prometheus.yml
cat > prometheus.yml <<EOF
global:
  scrape_interval: 15s

scrape_configs:
  - job_name: 'aethyme'
    static_configs:
      - targets: ['localhost:8000']
    metrics_path: '/metrics'
EOF

# Start Prometheus
prometheus --config.file=prometheus.yml

# View metrics
open http://localhost:9090
```

## 5. Run Tests

```bash
# Run all tests
pytest packages/aethyme/tests/indexing/ -v

# Run with coverage
pytest packages/aethyme/tests/indexing/ \
  --cov=src/indexing \
  --cov-report=term-missing
```

## Key Features to Try

### Language Detection

```python
from pathlib import Path
from src.indexing.language_support import LanguageDetector

detector = LanguageDetector()

# Detect language
lang = detector.detect_language(Path("main.py"))
print(f"Language: {lang}")

# Check if SCIP available
if detector.should_use_scip(lang):
    print("Use SCIP indexer")
else:
    print("Use fallback indexer")

# Get all files by language
files_by_lang = detector.get_files_by_language(
    Path("/path/to/repo"),
    exclude_dirs=["node_modules", "__pycache__"],
)
print(f"Languages found: {list(files_by_lang.keys())}")
```

### Retry Logic

```python
from src.indexing.retry import RetryManager, RetryConfig

config = RetryConfig(
    max_attempts=3,
    initial_delay_seconds=1.0,
)
manager = RetryManager(config)

# Wrap risky operation with retry
def risky_operation():
    # Your code here
    return "success"

result = manager.execute_with_retry(
    risky_operation,
    operation_name="my_operation",
)
```

### Freshness Monitoring

```python
from src.indexing.freshness import FreshnessMonitor
from src.graph.connection_pool import db_pool

monitor = FreshnessMonitor(db_pool)

# Get freshness for a repo
metrics = monitor.get_repository_freshness(repo_id, tenant_id)
print(f"Status: {metrics.status.value}")
print(f"Last indexed: {metrics.staleness_hours:.1f}h ago")

# Get all stale repos
stale_repos = monitor.get_stale_repositories(
    tenant_id,
    threshold_hours=24.0,
)
print(f"Found {len(stale_repos)} stale repositories")
```

### Metrics Collection

```python
from src.indexing.metrics import metrics_collector

# Track duration
with metrics_collector.track_indexing_duration(
    repository="my-repo",
    language="python",
    indexer_type="scip",
):
    # Your indexing code
    pass

# Export metrics
from src.indexing.metrics import get_metrics_text
print(get_metrics_text())
```

## API Endpoints

### Get Index Status
```bash
curl http://localhost:8000/api/index/status/{repo_id}
```

### Get Freshness Summary
```bash
curl http://localhost:8000/api/index/freshness
```

### Trigger Re-index
```bash
curl -X POST http://localhost:8000/api/index/trigger/{repo_id}
```

## Documentation

- **Main Guide:** `docs/indexing-reliability.md`
- **Implementation Summary:** `docs/s1-tLS1-T2-IMPLEMENTATION-SUMMARY.md`
- **Dashboard Setup:** `docs/FRESHNESS-DASHBOARD-SETUP.md`
- **Deliverables Report:** `SPRINT_S1-T2_DELIVERABLES.md`

## Need Help?

1. **Check logs:** Structured JSON logs include correlation IDs
2. **View metrics:** http://localhost:8000/metrics
3. **API docs:** http://localhost:8000/docs
4. **Run tests:** `pytest tests/indexing/ -v`

## What's Next?

After trying these features:
1. Read the full guide: `docs/indexing-reliability.md`
2. Set up monitoring: `docs/FRESHNESS-DASHBOARD-SETUP.md`
3. Review benchmarks: `benchmarks/results/index_perf_report.md`
4. Deploy to staging and validate

---

**Version:** Sprint S1-T2
**Status:** Production Ready
**Last Updated:** 2025-11-22
