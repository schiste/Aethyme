# Python SDK Guide

Guide for the current Aethyme Python SDK.

## Scope

The SDK tracks the mounted core API only:
- search
- ego graph
- impact analysis
- scorecard

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

## API Reference

### Query API
- `client.query.search(term, kind=None, lang=None, limit=20)`
- `client.query.ego_graph(symbol, depth=2)`
- `client.query.impact_analysis(symbol, max_depth=10)`

### Scorecard API
- `client.scorecard.scan(repo_id, checks=None)`
- `client.scorecard.get_history(repo_id, limit=10)`
- `client.scorecard.list_checks()`
