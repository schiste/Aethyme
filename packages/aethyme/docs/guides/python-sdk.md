# Python SDK Guide

Guide for using the Aethyme Python SDK.

## Installation

```bash
pip install aethyme-sdk
```

## Quick Start

```python
from aethyme_sdk import AethymeClient

# Initialize client
client = AethymeClient(
    api_key="your-api-key",
    org_id="your-org-id"
)

# Search for code
results = client.query.search("UserService")
for result in results:
    print(f"{result.symbol} in {result.file_path}")

# Run scorecard
scorecard = client.scorecard.scan(repo_id="abc123")
print(f"Score: {scorecard.overall_score}/100")
```

## API Reference

### Query API
- `client.query.search(term, kind=None, lang=None, limit=20)`
- `client.query.ego_graph(symbol, depth=2)`
- `client.query.impact_analysis(symbol, max_depth=10)`

### Scorecard API
- `client.scorecard.scan(repo_id, checks=None)`
- `client.scorecard.get_history(repo_id, limit=10)`
- `client.scorecard.list_checks()`

### Autofix API
- `client.autofix.run(repo_id, fix_types=None, dry_run=True)`
- `client.autofix.apply(repo_id, fix_ids, create_pr=False)`
- `client.autofix.list_types()`

### Telemetry API
- `client.telemetry.list_metrics()`
- `client.telemetry.query(metrics, start_time=None, end_time=None)`
- `client.telemetry.get_kpis(period="7d")`

### Guardrails API
- `client.guardrails.list_guardrails()`
- `client.guardrails.get_config()`
- `client.guardrails.check_drift(repo_id)`

## Context Manager

```python
with AethymeClient(api_key="...", org_id="...") as client:
    results = client.query.search("UserService")
```

For complete SDK documentation, see sdk/python/README.md
