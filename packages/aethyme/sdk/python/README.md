# Aethyme Python SDK

Python client for the live Aethyme core API.

## Scope

The SDK currently covers the stable core endpoints:
- search
- ego graph
- impact analysis
- scorecard

It does not expose removed or unmounted APIs.

## Installation

```bash
pip install aethyme-sdk
```

## Quick Start

```python
from aethyme_sdk import AethymeClient

with AethymeClient(api_key="your-api-key", org_id="your-org-id") as client:
    results = client.query.search("run_service")
    ego = client.query.ego_graph("run_service", depth=2)
    impact = client.query.impact_analysis("run_service")
```

## Scorecard

```python
with AethymeClient(api_key="your-api-key", org_id="your-org-id") as client:
    scan = client.scorecard.scan(repo_id="repo-id")
    print(scan.overall_score)
```

## Development

```bash
pip install -e ".[dev]"
pytest
mypy aethyme_sdk
black aethyme_sdk
```
