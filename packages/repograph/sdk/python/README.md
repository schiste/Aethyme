# RepoGraph Python SDK

Official Python SDK for RepoGraph API - Graph-based code intelligence system.

## Installation

```bash
pip install repograph-sdk
```

## Quick Start

```python
from repograph_sdk import RepoGraphClient

# Initialize client
client = RepoGraphClient(
    api_key="your-api-key",
    org_id="your-org-id"
)

# Search for code
results = client.query.search("UserService")
for result in results:
    print(f"{result.symbol} in {result.file_path}:{result.line_number}")

# Get ego graph
ego = client.query.ego_graph("AuthController", depth=3)
print(f"Found {ego.total_nodes} related symbols")

# Run AI-readiness scorecard
scorecard = client.scorecard.scan(repo_id="abc123")
print(f"Score: {scorecard.overall_score}/100")
print(f"Violations: {len(scorecard.violations)}")

# Run autofixes
fixes = client.autofix.run(repo_id="abc123", dry_run=True)
print(f"Found {len(fixes.fixes)} potential fixes")
```

## Features

- **Code Search**: Search for symbols across your codebase
- **Ego Graphs**: Analyze symbol relationships
- **Impact Analysis**: Understand the impact of code changes
- **AI-Readiness Scorecard**: Check repository for AI agent compatibility
- **Autofixes**: Automatically fix common issues
- **Telemetry**: Query metrics and performance data
- **Guardrails**: Configure safety and efficiency controls

## Authentication

The SDK requires an API key and organization ID:

```python
client = RepoGraphClient(
    api_key="rg_live_...",
    org_id="org_..."
)
```

You can also set environment variables:

```bash
export REPOGRAPH_API_KEY="rg_live_..."
export REPOGRAPH_ORG_ID="org_..."
```

## API Reference

### Query API

```python
# Search
results = client.query.search(
    term="UserService",
    kind="class",
    lang="python",
    limit=20
)

# Ego graph
ego = client.query.ego_graph(
    symbol="UserService",
    depth=3
)

# Impact analysis
impact = client.query.impact_analysis(
    symbol="UserService",
    max_depth=10
)
```

### Scorecard API

```python
# Run scan
scorecard = client.scorecard.scan(
    repo_id="abc123",
    checks=["data_ui", "docs", "links"]
)

# Get history
history = client.scorecard.get_history(repo_id="abc123")

# List available checks
checks = client.scorecard.list_checks()
```

### Autofix API

```python
# Run autofixes (dry-run)
result = client.autofix.run(
    repo_id="abc123",
    fix_types=["links", "selectors"],
    dry_run=True
)

# Apply fixes
apply_result = client.autofix.apply(
    repo_id="abc123",
    fix_ids=["fix-1", "fix-2"],
    create_pr=True
)
```

### Telemetry API

```python
# List metrics
metrics = client.telemetry.list_metrics()

# Query data
data = client.telemetry.query(
    metrics=["query_latency_ms"],
    aggregation="avg",
    interval="1h"
)

# Get KPIs
kpis = client.telemetry.get_kpis(period="7d")
```

### Guardrails API

```python
# List guardrails
guardrails = client.guardrails.list_guardrails()

# Get configuration
config = client.guardrails.get_config()

# Check drift
drift = client.guardrails.check_drift(repo_id="abc123")

# Get violations
violations = client.guardrails.get_violations(hours=24)
```

## Error Handling

```python
from repograph_sdk import RepoGraphClient, APIError, AuthenticationError

try:
    client = RepoGraphClient(api_key="...", org_id="...")
    results = client.query.search("UserService")
except AuthenticationError as e:
    print(f"Authentication failed: {e}")
except APIError as e:
    print(f"API error ({e.status_code}): {e}")
```

## Context Manager

The client can be used as a context manager to ensure proper cleanup:

```python
with RepoGraphClient(api_key="...", org_id="...") as client:
    results = client.query.search("UserService")
    # Client is automatically closed when exiting the context
```

## Development

```bash
# Install dev dependencies
pip install -e ".[dev]"

# Run tests
pytest

# Type checking
mypy repograph_sdk

# Format code
black repograph_sdk
```

## License

MIT License - see LICENSE file for details.

## Support

- Documentation: https://docs.repograph.ai
- Issues: https://github.com/repograph/python-sdk/issues
- Email: support@repograph.ai
