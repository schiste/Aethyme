#!/bin/bash
# Documentation generation script
# Generates API docs, CLI docs, and architecture diagrams

set -e

echo "=== Aethyme Documentation Generator ==="
echo ""

# Colors
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
NC='\033[0m'

# Generate OpenAPI docs
echo "Generating OpenAPI documentation..."
if [ -f "src/api/main.py" ]; then
    python3 << 'EOF'
import json
from src.api.main import app

# Generate OpenAPI schema
schema = app.openapi()

# Write to file
with open("docs/openapi.json", "w") as f:
    json.dump(schema, f, indent=2)

print("✓ OpenAPI schema written to docs/openapi.json")
EOF
else
    echo -e "${YELLOW}Warning: src/api/main.py not found, skipping OpenAPI generation${NC}"
fi

# Generate CLI reference
echo ""
echo "Generating CLI reference..."
if [ -f "src/cli.py" ]; then
    python3 << 'EOF'
import click
from src import cli

# Get all commands
ctx = click.Context(cli.cli)
commands = cli.cli.list_commands(ctx)

output = "# CLI Reference (Auto-Generated)\n\n"
output += "Generated from source code.\n\n"

for cmd_name in commands:
    cmd = cli.cli.get_command(ctx, cmd_name)
    output += f"## {cmd_name}\n\n"
    output += f"{cmd.help or 'No description'}\n\n"

    # Parameters
    if cmd.params:
        output += "**Parameters:**\n\n"
        for param in cmd.params:
            output += f"- `{param.name}`: {param.help or 'No description'}\n"
        output += "\n"

with open("docs/reference/cli-generated.md", "w") as f:
    f.write(output)

print("✓ CLI reference written to docs/reference/cli-generated.md")
EOF
else
    echo -e "${YELLOW}Warning: src/cli.py not found, skipping CLI generation${NC}"
fi

# Generate metrics documentation
echo ""
echo "Generating metrics documentation..."
cat > docs/reference/metrics.md << 'EOF'
# Metrics Reference

## Application Metrics

### Request Metrics

- `aethyme_requests_total` - Total API requests (counter)
- `aethyme_request_duration_seconds` - Request duration (histogram)
- `aethyme_errors_total` - Total errors (counter)

### Indexing Metrics

- `aethyme_indexing_total` - Total indexing operations (counter)
- `aethyme_indexing_duration_seconds` - Indexing duration (histogram)
- `aethyme_indexing_failures_total` - Failed indexing operations (counter)
- `aethyme_index_age_seconds` - Age of repository indexes (gauge)

### Query Metrics

- `aethyme_query_total` - Total queries by type (counter)
- `aethyme_query_duration_seconds` - Query duration (histogram)
- `aethyme_cache_hits_total` - Cache hits (counter)
- `aethyme_cache_misses_total` - Cache misses (counter)

### Database Metrics

- `aethyme_db_connections_active` - Active DB connections (gauge)
- `aethyme_db_connections_idle` - Idle DB connections (gauge)
- `aethyme_db_query_duration_seconds` - DB query duration (histogram)

## Usage

```bash
# Scrape metrics
curl http://localhost:9090/metrics

# Query with Prometheus
curl 'http://prometheus:9090/api/v1/query?query=rate(aethyme_requests_total[5m])'
```

**Last Updated:** $(date +%Y-%m-%d)
EOF

echo -e "${GREEN}✓${NC} Metrics reference written to docs/reference/metrics.md"

# Generate documentation index
echo ""
echo "Generating documentation index..."
cat > docs/INDEX.md << 'EOF'
# Documentation Index

Auto-generated index of all documentation files.

## Getting Started

- [Quickstart](getting-started/quickstart.md)
- [Onboarding Guide](getting-started/onboarding.md)

## Reference

- [API Reference](reference/api.md)
- [CLI Reference](reference/cli.md)

## Guides

- [Testing](guides/testing.md)
- [Troubleshooting](guides/troubleshooting.md)
- [Python SDK](guides/python-sdk.md)
- [Scorecard](../scorecard-guide.md)
- [Autofixers](../autofixers-guide.md)

## Authentication

- [Auth Setup](../auth-setup.md)
- [Auth Boundary](architecture/auth-boundary.md)

## Architecture

- [Core Architecture](architecture/core-architecture.md)
- [API Contract](architecture/api-contract.yaml)

## Security

- [Security Overview](security/security-overview.md)

## Runbooks

- [Index Failure](runbooks/index-failure.md)
- [Rollback Procedures](runbooks/rollback.md)
EOF

echo -e "${GREEN}✓${NC} Documentation index written to docs/INDEX.md"

# Generate doc statistics
echo ""
echo "=== Documentation Statistics ==="
MD_COUNT=$(find docs -name "*.md" | wc -l)
LINE_COUNT=$(find docs -name "*.md" -exec wc -l {} + | tail -1 | awk '{print $1}')
WORD_COUNT=$(find docs -name "*.md" -exec wc -w {} + | tail -1 | awk '{print $1}')

echo "Markdown files: $MD_COUNT"
echo "Total lines: $LINE_COUNT"
echo "Total words: $WORD_COUNT"

echo ""
echo -e "${GREEN}✓${NC} Documentation generation complete!"
