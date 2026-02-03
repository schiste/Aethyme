# Aethyme Telemetry & Observability Guide

**Version:** 1.0
**Feature Flag:** `TELEM_EVAL_V1`
**Last Updated:** 2025-01-22

## Overview

Aethyme provides comprehensive telemetry and observability infrastructure for monitoring quality, performance, and cost metrics across all operations. This guide covers how to use, configure, and extend the telemetry system.

## Architecture

The telemetry system consists of four main components:

1. **Enhanced Metrics** - Prometheus metrics for tokens, cost, violations, fixes
2. **Distributed Tracing** - OpenTelemetry spans with Jaeger export
3. **Correlation** - Request ID propagation across services
4. **KPI Dashboard** - Aggregated metrics and reporting

## Quick Start

### Enable Telemetry

```bash
export TELEM_EVAL_V1=true
export JAEGER_HOST=localhost
export JAEGER_PORT=6831
```

### Basic Usage

```python
from src.telemetry import (
    enhanced_metrics_collector,
    tracing_manager,
    correlation_manager,
    kpi_calculator,
)

# Set request context
correlation_manager.set_request_id("req-123")
correlation_manager.set_tenant_id("tenant-456")

# Create a trace
with tracing_manager.create_span("index_repository", {"repo": "myrepo"}):
    # Your code here

    # Record metrics
    enhanced_metrics_collector.record_token_usage(
        operation_type="index",
        model="gpt-4",
        input_tokens=5000,
        output_tokens=2000,
        trace_id=tracing_manager.get_current_trace_id(),
    )

    # Record violations
    enhanced_metrics_collector.record_violation(
        severity="warning",
        violation_type="missing_docstring",
    )
```

## Enhanced Metrics

### Available Metrics

#### Token & Cost Metrics

- `aethyme_tokens_consumed_total` - Total tokens consumed by operation/model
- `aethyme_tokens_per_operation` - Histogram of tokens per operation
- `aethyme_cost_dollars_total` - Total cost in dollars
- `aethyme_cost_per_operation_dollars` - Cost histogram

#### Quality Metrics

- `aethyme_violations_detected_total` - Violations detected by scorecard
- `aethyme_violations_prevented_total` - Violations prevented by guardrails
- `aethyme_fixes_applied_total` - Successfully applied fixes
- `aethyme_fixes_failed_total` - Failed fix attempts

#### Cache Metrics

- `aethyme_cache_operations_total` - Cache operations by result (hit/miss)
- `aethyme_cache_hit_rate` - Current cache hit rate percentage

#### Model Routing Metrics

- `aethyme_model_routing_decisions_total` - Model routing decisions
- `aethyme_model_escalations_total` - Model escalations due to failures

#### Context Management

- `aethyme_context_compaction_savings_bytes` - Bytes saved by compaction
- `aethyme_context_slots_utilized` - Context slots used

#### Safety Metrics

- `aethyme_safety_checks_total` - Safety checks performed
- `aethyme_dry_run_executions_total` - Dry-run executions

### Recording Metrics

```python
from src.telemetry import enhanced_metrics_collector

# Token usage with cost calculation
enhanced_metrics_collector.record_token_usage(
    operation_type="query",
    model="gpt-4",
    input_tokens=1000,
    output_tokens=500,
    trace_id="trace-123",
)

# Violations
enhanced_metrics_collector.record_violation(
    severity="blocker",
    violation_type="generated_file_edit",
    trace_id="trace-123",
)

# Fixes
enhanced_metrics_collector.record_fix_applied(
    fix_type="docs",
    safety_mode="dry_run",
    trace_id="trace-123",
)

# Cache operations
enhanced_metrics_collector.record_cache_operation(
    operation_type="query",
    cache_result="hit",
    trace_id="trace-123",
)

# Model routing
enhanced_metrics_collector.record_model_routing(
    from_model="gpt-3.5-turbo",
    to_model="gpt-4",
    reason="performance",
    trace_id="trace-123",
)
```

### Exporting Metrics

Metrics are exposed in Prometheus format:

```python
from src.telemetry import get_enhanced_metrics_text

metrics = get_enhanced_metrics_text()
print(metrics)
```

## Distributed Tracing

### OpenTelemetry Integration

Aethyme uses OpenTelemetry for distributed tracing with Jaeger as the backend.

#### Configuration

```python
from src.telemetry.tracing import TracingManager

manager = TracingManager(
    service_name="aethyme",
    jaeger_host="localhost",
    jaeger_port=6831,
    enabled=True,
)
```

#### Creating Spans

```python
from src.telemetry import tracing_manager

# Context manager
with tracing_manager.create_span("operation_name", {"key": "value"}):
    # Your code here
    pass

# Decorator
from src.telemetry.tracing import trace_operation

@trace_operation("my_function", {"param": "value"})
def my_function():
    # Your code
    pass
```

#### Adding Attributes and Events

```python
from src.telemetry.tracing import add_trace_attribute, add_trace_event

# Add attribute to current span
add_trace_attribute("user_id", "user-123")

# Add event to current span
add_trace_event("cache_miss", {"key": "query-456"})
```

#### Getting Trace ID

```python
from src.telemetry.tracing import get_current_trace_id

trace_id = get_current_trace_id()
# Use for correlation with metrics and logs
```

## Request Correlation

### Context Propagation

```python
from src.telemetry import correlation_manager

# Set correlation IDs
correlation_manager.set_request_id("req-123")
correlation_manager.set_session_id("sess-456")
correlation_manager.set_user_id("user-789")
correlation_manager.set_tenant_id("tenant-abc")

# Get all context
context = correlation_manager.get_correlation_context()
# {"request_id": "req-123", "session_id": "sess-456", ...}
```

### HTTP Header Propagation

```python
# Inject into outgoing requests
headers = correlation_manager.inject_http_headers()
# {"X-Request-ID": "req-123", "X-Session-ID": "sess-456", ...}

# Extract from incoming requests
incoming_headers = request.headers
correlation_manager.extract_http_headers(incoming_headers)
```

### Structured Logging Integration

```python
import structlog

logger = structlog.get_logger(__name__)

# Bind correlation context
log_context = correlation_manager.get_structured_log_context()
logger = logger.bind(**log_context)

logger.info("Operation completed", result="success")
# Output includes request_id, session_id, etc.
```

## KPI Dashboard

### Recording Operation Metrics

```python
from src.telemetry import kpi_calculator

kpi_calculator.record_operation_metrics(
    operation_type="query",
    duration=0.150,
    tokens=1000,
    cost=0.05,
    success=True,
    cache_hit=False,
)
```

### Calculating KPIs

```python
from datetime import timedelta

# Calculate for all time
snapshot = kpi_calculator.calculate_kpis()

# Calculate for last 24 hours
snapshot = kpi_calculator.calculate_kpis(
    time_window=timedelta(hours=24)
)

# Access metrics
print(f"Query p95: {snapshot.query_latency_p95:.3f}s")
print(f"Avg cost: ${snapshot.avg_cost_per_task:.4f}")
print(f"Cache hit rate: {snapshot.query_cache_hit_rate:.1f}%")
```

### Exporting KPI Reports

```python
# Export to CSV
kpi_calculator.export_to_csv("kpi_report.csv")

# Export to JSON
kpi_calculator.export_to_json("kpi_report.json")

# Print summary
kpi_calculator.print_summary()
```

### CLI Integration

```bash
# Export KPI report
aethyme kpi --output kpi_report.csv --format csv

# Export for specific time window
aethyme kpi --output kpi_report.json --format json --window 24h
```

## Grafana Dashboards

### Available Dashboards

1. **Stage 1 Overview** (`monitoring/grafana/dashboards/stage1_overview.json`)
   - System health and uptime
   - Index and query latency percentiles
   - Cache hit rates
   - Token consumption and costs
   - Fixes and violations
   - Model escalations

2. **Evaluation Results** (`monitoring/grafana/dashboards/evaluation_results.json`)
   - Retrieval precision/recall/MRR/nDCG
   - Autofix success rates
   - Scorecard precision and false positive rates
   - Benchmark trends
   - Evaluation run history

### Setting Up Grafana

```bash
# Using Docker Compose
cd monitoring/
docker-compose up -d

# Access Grafana at http://localhost:3000
# Default credentials: admin/admin
```

### Importing Dashboards

1. Open Grafana UI
2. Go to Dashboards → Import
3. Upload JSON files from `monitoring/grafana/dashboards/`
4. Select Prometheus data source
5. Click Import

## Alerts

### Prometheus Alerts

Alerts are configured in `monitoring/prometheus/alerts.yml`:

```yaml
groups:
  - name: aethyme_quality
    rules:
      - alert: RetrievalPrecisionLow
        expr: retrieval_eval_precision < 85
        for: 5m
        annotations:
          summary: "Retrieval precision below threshold"

      - alert: QueryLatencyHigh
        expr: histogram_quantile(0.95, rate(aethyme_query_duration_seconds_bucket[5m])) > 2.0
        for: 5m
        annotations:
          summary: "Query p95 latency above 2s"
```

## Best Practices

### 1. Always Use Trace IDs

Correlate metrics with traces:

```python
trace_id = tracing_manager.get_current_trace_id()

enhanced_metrics_collector.record_token_usage(
    # ... other params
    trace_id=trace_id,
)
```

### 2. Set Request Context Early

Set correlation IDs at the start of each request:

```python
@app.middleware("http")
async def correlation_middleware(request, call_next):
    correlation_manager.extract_http_headers(request.headers)

    if not correlation_manager.get_request_id():
        correlation_manager.set_request_id(
            correlation_manager.generate_request_id()
        )

    response = await call_next(request)
    return response
```

### 3. Track Costs Proactively

Record token usage for all LLM calls:

```python
response = llm.generate(prompt)

enhanced_metrics_collector.record_token_usage(
    operation_type="query",
    model="gpt-4",
    input_tokens=response.usage.prompt_tokens,
    output_tokens=response.usage.completion_tokens,
)
```

### 4. Use Structured Logging

Always bind correlation context:

```python
logger = correlation_manager.bind_logger(structlog.get_logger())
logger.info("Operation started", operation="index")
```

### 5. Monitor KPIs Regularly

Export and review KPIs:

```bash
# Daily cron job
0 2 * * * cd /app && python -m src.telemetry.kpi export --output /reports/kpi_$(date +\%Y\%m\%d).csv
```

## Troubleshooting

### Metrics Not Appearing

1. Check feature flag: `echo $TELEM_EVAL_V1`
2. Verify Prometheus scrape config
3. Check metrics endpoint: `curl http://localhost:8000/metrics`

### Traces Not in Jaeger

1. Verify Jaeger is running: `docker ps | grep jaeger`
2. Check Jaeger host/port configuration
3. Verify OpenTelemetry is installed: `pip list | grep opentelemetry`

### KPI Export Fails

1. Check file permissions
2. Verify output directory exists
3. Ensure metrics have been recorded

## Advanced Topics

### Custom Metrics

Add custom metrics to `enhanced_metrics.py`:

```python
from prometheus_client import Counter

custom_metric = Counter(
    "aethyme_custom_total",
    "Custom metric description",
    labelnames=["label1", "trace_id"],
    registry=enhanced_registry,
)

def record_custom_event(label1: str, trace_id: str):
    custom_metric.labels(label1=label1, trace_id=trace_id).inc()
```

### Sampling Configuration

Configure trace sampling:

```python
from opentelemetry.sdk.trace.sampling import TraceIdRatioBased

manager = TracingManager(
    enabled=True,
    sampler=TraceIdRatioBased(0.1),  # Sample 10% of traces
)
```

### Multi-Tenant Metrics

Use tenant labels for isolation:

```python
enhanced_metrics_collector.record_token_usage(
    operation_type="query",
    model="gpt-4",
    input_tokens=1000,
    output_tokens=500,
    # Use tenant from correlation context
)
```

## See Also

- [Evaluation Guide](./evaluation-guide.md)
- [Performance Budgets](./architecture/performance-budgets.md)
- [Monitoring & Observability](./architecture/stage1-architecture.md#observability)

## Support

For issues or questions:
1. Check Grafana dashboards for current metrics
2. Review Jaeger for trace details
3. Examine Prometheus alerts
4. Consult the team via #aethyme-observability
